use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tokio::process::Command;

use crate::process::{ChildError, ManagedChild};

/// Electron 隐藏浏览器内核执行脚本后的结果文件。
#[derive(Debug)]
pub struct ElectronResult {
    pub value: serde_json::Value,
    pub status: String,
}

/// 用项目内置 Electron 执行 B站页面脚本，绕开常规 HTTP 请求容易触发的 412。
///
/// 所有状态都通过独立 JSON 文件传递，不依赖 Electron 的 stdout，因为 Electron
/// 主进程会把大量 GPU/Chromium 日志写到 stderr，stdout 也不是稳定的协议。
pub async fn run_bilibili_script(
    project_dir: &Path,
    script: &str,
    env: &[(&str, String)],
    timeout: Duration,
) -> Result<ElectronResult> {
    let electron = find_electron(project_dir)
        .ok_or_else(|| anyhow!("未找到项目内置 Electron，请先启动桌面程序安装依赖"))?;
    let script = script.trim();
    if script.is_empty() {
        anyhow::bail!("B站 Electron 脚本为空");
    }
    let run_dir = project_dir.join("fupan_output").join("run");
    std::fs::create_dir_all(&run_dir)
        .with_context(|| format!("创建运行目录失败: {}", run_dir.display()))?;

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let payload_path = run_dir.join(format!("bili_script_payload_{nonce}.js"));
    let status_path = run_dir.join(format!("bili_script_status_{nonce}.json"));
    crate::config::atomic_write(&payload_path, script)
        .with_context(|| format!("写入 B站脚本失败: {}", payload_path.display()))?;

    let runner = project_dir
        .join("desktop")
        .join("electron")
        .join("bili-video-list.cjs");
    if !runner.exists() {
        anyhow::bail!("缺少 B站 Electron 脚本执行器: {}", runner.display());
    }
    let mut cmd = Command::new(&electron);
    cmd.kill_on_drop(true);
    cmd.arg(&runner)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.env("FUPAN_SCRIPT_FILE", &payload_path)
        .env("FUPAN_STATUS_FILE", &status_path);

    let child = ManagedChild::spawn(&mut cmd)
        .with_context(|| format!("启动 Electron 脚本失败: {}", electron.display()))?;
    match child.wait(timeout).await {
        Ok(status) if status.success() => {}
        Ok(status) => {
            let _ = std::fs::remove_file(&payload_path);
            let _ = std::fs::remove_file(&status_path);
            anyhow::bail!("Electron 脚本退出码 {}", status);
        }
        Err(ChildError::Timeout) => {
            let _ = std::fs::remove_file(&payload_path);
            let _ = std::fs::remove_file(&status_path);
            anyhow::bail!("Electron 脚本超时（{} 秒）", timeout.as_secs());
        }
        Err(ChildError::Io(e)) => {
            let _ = std::fs::remove_file(&payload_path);
            let _ = std::fs::remove_file(&status_path);
            return Err(e).context("等待 Electron 脚本失败");
        }
    };

    let status = std::fs::read_to_string(&status_path).unwrap_or_default();
    let value = std::fs::read_to_string(&status_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(serde_json::Value::Null);
    let _ = std::fs::remove_file(&payload_path);
    let _ = std::fs::remove_file(&status_path);
    Ok(ElectronResult { value, status })
}

fn find_electron(project_dir: &Path) -> Option<PathBuf> {
    let base = project_dir
        .join("desktop")
        .join("node_modules")
        .join("electron")
        .join("dist");
    let candidates = [
        base.join("Electron.app")
            .join("Contents")
            .join("MacOS")
            .join("Electron"),
        base.join("electron"),
        base.join("electron.exe"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_validation_rejects_empty_payload() {
        let dir = std::env::temp_dir().join(format!("zunzhe-electron-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(run_bilibili_script(
            &dir,
            "   ",
            &[],
            Duration::from_secs(1),
        ));
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
