use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tokio::process::Command;

use crate::live_monitor::{LiveAdapter, LiveStatus};
use crate::process::{ChildError, ManagedChild};

/// 可交给旧 Python `src/spider.py` 解析的直播平台路由表。
const PLATFORM_ROUTES: &[(&str, &str)] = &[
    ("live.douyin.com", "douyin"),
    ("v.douyin.com", "douyin"),
    ("live.kuaishou.com", "kuaishou"),
    ("www.huya.com", "huya"),
    ("www.douyu.com", "douyu"),
    ("www.youtube.com", "youtube"),
    ("youtu.be", "youtube"),
    ("www.twitch.tv", "twitch"),
];

/// 多平台直播 Python 桥接器。Rust 负责超时、状态机、录制与调度；平台网页/签名
/// 解析仍复用旧 Python 能力。桥接输出固定 JSON，避免在 Rust 中重写 40+ 个平台的
/// 私有协议。
pub struct PythonLiveAdapter {
    python: PathBuf,
    project_dir: PathBuf,
    timeout: Duration,
}

impl PythonLiveAdapter {
    pub fn new(project_dir: PathBuf) -> Self {
        let python = crate::service::python_bin(&project_dir);
        Self {
            python,
            project_dir,
            timeout: Duration::from_secs(25),
        }
    }

    /// 平台名与 Python 解析器参数名一致；未知平台返回 None。
    pub fn platform_for_url(url: &str) -> Option<&'static str> {
        let lower = url.to_lowercase();
        PLATFORM_ROUTES
            .iter()
            .find(|(host, _)| lower.contains(host))
            .map(|(_, platform)| *platform)
    }

    pub fn supports_url(url: &str) -> bool {
        Self::platform_for_url(url).is_some()
    }

    fn command(&self, url: &str) -> Result<Command> {
        let platform = Self::platform_for_url(url)
            .ok_or_else(|| anyhow!("不支持的直播平台，暂不能通过 Python 桥接解析: {url}"))?;
        if !crate::service::python_binary_available(&self.python) {
            bail!(
                "找不到可用的 Python 环境: {}（需 .venv 或系统 PATH 中的 python3）",
                self.python.display()
            );
        }
        let mut cmd = Command::new(&self.python);
        cmd.kill_on_drop(true)
            .arg("-m")
            .arg("fupan.live_resolve")
            .arg("--url")
            .arg(url)
            .arg("--platform")
            .arg(platform)
            .current_dir(&self.project_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Ok(cmd)
    }

    fn run_command(
        &self,
        mut cmd: Command,
    ) -> Pin<Box<dyn Future<Output = Result<LiveStatus>> + Send + '_>> {
        Box::pin(async move {
            let child = ManagedChild::spawn(&mut cmd).context("启动多平台直播桥接失败")?;
            let output = match child.wait_with_output(self.timeout).await {
                Ok(output) => output,
                Err(ChildError::Io(e)) => return Err(e).context("读取多平台直播桥接输出失败"),
                Err(ChildError::Timeout) => anyhow::bail!("多平台直播桥接超时（25 秒）"),
            };
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!(
                    "多平台直播解析失败，退出码 {}: {}",
                    output.status,
                    stderr.chars().take(500).collect::<String>()
                );
            }
            let stdout = String::from_utf8_lossy(&output.stdout);
            parse_live_json(stdout.trim()).with_context(|| {
                format!(
                    "Python 直播桥接输出不是有效 JSON: {}",
                    stdout.chars().take(200).collect::<String>()
                )
            })
        })
    }
}

impl LiveAdapter for PythonLiveAdapter {
    fn name(&self) -> &'static str {
        "python-bridge"
    }

    fn check<'a>(
        &'a self,
        url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<LiveStatus>> + Send + 'a>> {
        match self.command(url) {
            Ok(cmd) => self.run_command(cmd),
            Err(e) => Box::pin(async move { Err(e) }),
        }
    }
}

/// 解析桥接脚本输出：未开播时也必须返回成功，只有进程失败或 JSON 非法才是错误。
pub fn parse_live_json(text: &str) -> Result<LiveStatus> {
    let value: serde_json::Value = serde_json::from_str(text)?;
    // 旧 Python 桥接把“未开播”和“解析失败/风控失败”都输出 is_live=false，
    // 但失败时会带 error 字段。Rust 必须把它当成明确检测失败，而不是当成
    // 一次真实的“已下播”，否则平台接口抖动/风控会让状态机误关正在录制的
    // ffmpeg，并制造重复的开播/关播通知。
    if let Some(err) = value.get("error").and_then(|v| v.as_str())
        && !err.trim().is_empty()
    {
        bail!("Python 直播桥接解析失败: {err}");
    }
    let is_live = value
        .get("is_live")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let stream_url = value
        .get("stream_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if is_live && stream_url.is_empty() {
        bail!("Python 桥接返回开播状态但缺少 stream_url");
    }
    Ok(LiveStatus {
        is_live,
        title: value
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        anchor_name: value
            .get("anchor_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        stream_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_known_platforms() {
        assert_eq!(
            PythonLiveAdapter::platform_for_url("https://live.douyin.com/123"),
            Some("douyin")
        );
        assert_eq!(
            PythonLiveAdapter::platform_for_url("https://live.kuaishou.com/u/abc"),
            Some("kuaishou")
        );
        assert_eq!(
            PythonLiveAdapter::platform_for_url("https://www.youtube.com/watch?v=x"),
            Some("youtube")
        );
        assert_eq!(
            PythonLiveAdapter::platform_for_url("https://x.test/1"),
            None
        );
    }

    #[test]
    fn parses_live_json_contract() {
        let status =
            parse_live_json(r#"{"is_live":true,"title":"t","anchor_name":"a","stream_url":"u"}"#)
                .unwrap();
        assert!(status.is_live);
        assert_eq!(status.title, "t");
        assert_eq!(status.stream_url, "u");

        let offline = parse_live_json(r#"{"is_live":false,"anchor_name":"a"}"#).unwrap();
        assert!(!offline.is_live);
        assert!(offline.stream_url.is_empty());

        let failed = parse_live_json(r#"{"is_live":false,"error":"Douyin web data fetch error"}"#)
            .unwrap_err();
        assert!(
            failed.to_string().contains("解析失败"),
            "带 error 的桥接输出必须被识别为检测失败: {failed}"
        );
    }

    #[test]
    fn live_status_without_stream_is_rejected() {
        let err = parse_live_json(r#"{"is_live":true,"anchor_name":"a"}"#).unwrap_err();
        assert!(err.to_string().contains("stream_url"));
    }
}
