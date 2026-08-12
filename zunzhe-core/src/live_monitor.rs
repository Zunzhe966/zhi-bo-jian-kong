use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use sqlx::sqlite::SqlitePool;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tracing::{error, info, warn};

use crate::config::Config;
use crate::db;
use crate::notify::{Channel, Notifier};
use crate::service::write_rust_live_pid;
use crate::watchlist::{LiveItem, Watchlist};

/// 平台无关的直播状态。适配器只输出这一种结构。
#[derive(Debug, Clone, Serialize)]
pub struct LiveStatus {
    pub is_live: bool,
    pub title: String,
    pub anchor_name: String,
    pub stream_url: String,
}

/// 可插拔直播检测适配器。Rust 原生支持 B站/自定义流地址，
/// 其余平台后续可通过 Python 桥接或 yt-dlp 探测。
pub trait LiveAdapter: Send + Sync {
    #[allow(dead_code)] // 供状态/日志/调试展示适配器名称
    fn name(&self) -> &'static str;
    fn check<'a>(
        &'a self,
        url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<LiveStatus>> + Send + 'a>>;
}

pub struct BilibiliLiveAdapter {
    client: reqwest::Client,
    cookie: String,
}

impl Default for BilibiliLiveAdapter {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
                .timeout(Duration::from_secs(20))
                .build()
                .unwrap_or_default(),
            cookie: String::new(),
        }
    }
}

impl BilibiliLiveAdapter {
    #[allow(dead_code)] // 便捷构造入口
    pub fn new() -> Self {
        Self::default()
    }

    #[allow(dead_code)] // 供真实直播检测带 Cookie 使用，避免 B站 412 风控
    pub fn with_cookie(cookie: impl Into<String>) -> Self {
        Self {
            cookie: cookie.into(),
            ..Self::default()
        }
    }

    fn room_id(url: &str) -> Option<String> {
        let re = regex::Regex::new(r"live\.bilibili\.com/(\d+)").ok()?;
        re.captures(url)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
    }
}

impl LiveAdapter for BilibiliLiveAdapter {
    fn name(&self) -> &'static str {
        "bilibili"
    }

    fn check<'a>(
        &'a self,
        url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<LiveStatus>> + Send + 'a>> {
        Box::pin(async move {
            let room_id = Self::room_id(url).ok_or_else(|| anyhow!("不是 B站直播间地址: {url}"))?;
            let api = format!("https://api.live.bilibili.com/room/v1/Room/room_init?id={room_id}");
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                reqwest::header::REFERER,
                reqwest::header::HeaderValue::from_str(url).unwrap_or_else(|_| {
                    reqwest::header::HeaderValue::from_static("https://live.bilibili.com")
                }),
            );
            if !self.cookie.is_empty()
                && let Ok(v) = reqwest::header::HeaderValue::from_str(&self.cookie)
            {
                headers.insert(reqwest::header::COOKIE, v);
            }
            let resp = self.client.get(&api).headers(headers).send().await?;
            let data: serde_json::Value = resp.error_for_status()?.json().await?;
            let room = &data["data"];
            if room.get("live_status").and_then(|v| v.as_i64()) != Some(1) {
                return Ok(LiveStatus {
                    is_live: false,
                    title: String::new(),
                    anchor_name: String::new(),
                    stream_url: String::new(),
                });
            }
            let uid = room["uid"].as_i64().unwrap_or(0);
            let anchor_name = if uid > 0 {
                fetch_bilibili_anchor_name(&self.client, uid, &self.cookie)
                    .await
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let title = fetch_bilibili_room_title(&self.client, &room_id, &self.cookie)
                .await
                .unwrap_or_default();
            let stream_url =
                fetch_bilibili_stream_url(&self.client, &room_id, &self.cookie).await?;
            Ok(LiveStatus {
                is_live: true,
                title,
                anchor_name,
                stream_url,
            })
        })
    }
}

async fn fetch_bilibili_anchor_name(
    client: &reqwest::Client,
    uid: i64,
    cookie: &str,
) -> Result<String> {
    let api = format!("https://api.live.bilibili.com/live_user/v1/Master/info?uid={uid}");
    let mut headers = reqwest::header::HeaderMap::new();
    if !cookie.is_empty()
        && let Ok(v) = reqwest::header::HeaderValue::from_str(cookie)
    {
        headers.insert(reqwest::header::COOKIE, v);
    }
    let resp = client.get(&api).headers(headers).send().await?;
    let data: serde_json::Value = resp.error_for_status()?.json().await?;
    Ok(data["data"]["info"]["uname"]
        .as_str()
        .unwrap_or("")
        .to_string())
}

async fn fetch_bilibili_room_title(
    client: &reqwest::Client,
    room_id: &str,
    cookie: &str,
) -> Result<String> {
    let api = format!(
        "https://api.live.bilibili.com/xlive/web-room/v1/index/getH5InfoByRoom?room_id={room_id}"
    );
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ORIGIN,
        reqwest::header::HeaderValue::from_static("https://live.bilibili.com"),
    );
    headers.insert(
        reqwest::header::REFERER,
        reqwest::header::HeaderValue::from_static("https://live.bilibili.com"),
    );
    if !cookie.is_empty()
        && let Ok(v) = reqwest::header::HeaderValue::from_str(cookie)
    {
        headers.insert(reqwest::header::COOKIE, v);
    }
    let resp = client.get(&api).headers(headers).send().await?;
    let data: serde_json::Value = resp.error_for_status()?.json().await?;
    Ok(data["data"]["room_info"]["title"]
        .as_str()
        .unwrap_or("")
        .to_string())
}

async fn fetch_bilibili_stream_url(
    client: &reqwest::Client,
    room_id: &str,
    cookie: &str,
) -> Result<String> {
    let api = format!(
        "https://api.live.bilibili.com/room/v1/Room/playUrl?cid={room_id}&qn=10000&platform=web"
    );
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::REFERER,
        reqwest::header::HeaderValue::from_static("https://live.bilibili.com"),
    );
    if !cookie.is_empty()
        && let Ok(v) = reqwest::header::HeaderValue::from_str(cookie)
    {
        headers.insert(reqwest::header::COOKIE, v);
    }
    let resp = client.get(&api).headers(headers.clone()).send().await?;
    let data: serde_json::Value = resp.error_for_status()?.json().await?;
    let durl = data["data"]["durl"].as_array();
    if let Some(urls) = durl
        && let Some(first) = urls.first()
        && let Some(url) = first["url"].as_str()
        && !url.is_empty()
    {
        return Ok(url.to_string());
    }
    // 新版接口兜底：getRoomPlayInfo 返回 HLS/FLV。
    let api2 = format!(
        "https://api.live.bilibili.com/xlive/web-room/v2/index/getRoomPlayInfo?room_id={room_id}&protocol=0,1&format=0,1,2&codec=0,1,2&qn=10000&platform=web&ptype=8&dolby=5&panorama=1"
    );
    let resp2 = client.get(&api2).headers(headers).send().await?;
    let data2: serde_json::Value = resp2.error_for_status()?.json().await?;
    let stream = &data2["data"]["playurl_info"]["playurl"]["stream"];
    for stream_item in stream.as_array().unwrap_or(&vec![]) {
        for format_item in stream_item["format"].as_array().unwrap_or(&vec![]) {
            for codec in format_item["codec"].as_array().unwrap_or(&vec![]) {
                if let Some(base) = codec["base_url"].as_str()
                    && let Some(hosts) = codec["url_info"].as_array()
                    && let Some(host) = hosts.first()
                    && let Some(host_str) = host["host"].as_str()
                {
                    let extra = host["extra"].as_str().unwrap_or("");
                    return Ok(format!("{host_str}{base}{extra}"));
                }
            }
        }
    }
    bail!("B站直播流地址获取失败，可能被风控或需要登录 Cookie")
}

/// 自定义/通用直播流地址适配器：直接按 m3u8/flv 判断是否可访问。
pub struct DirectLiveAdapter {
    client: reqwest::Client,
}

impl Default for DirectLiveAdapter {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
        }
    }
}

impl DirectLiveAdapter {
    pub fn new() -> Self {
        Self::default()
    }
}

impl LiveAdapter for DirectLiveAdapter {
    fn name(&self) -> &'static str {
        "direct"
    }

    fn check<'a>(
        &'a self,
        url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<LiveStatus>> + Send + 'a>> {
        Box::pin(async move {
            if !url.contains(".m3u8") && !url.contains(".flv") {
                bail!("不支持的直播地址: {url}（Rust 原生支持 B站直播间和 m3u8/flv 流地址）");
            }
            let resp = self.client.get(url).send().await?;
            if resp.status().is_success() {
                Ok(LiveStatus {
                    is_live: true,
                    title: String::new(),
                    anchor_name: String::new(),
                    stream_url: url.to_string(),
                })
            } else {
                Ok(LiveStatus {
                    is_live: false,
                    title: String::new(),
                    anchor_name: String::new(),
                    stream_url: String::new(),
                })
            }
        })
    }
}

#[allow(dead_code)] // 便捷无 Cookie 构造入口
pub fn resolve_live(url: &str) -> Box<dyn LiveAdapter> {
    resolve_live_with_cookie(url, "")
}

pub fn resolve_live_with_cookie(url: &str, cookie: &str) -> Box<dyn LiveAdapter> {
    if url.contains("live.bilibili.com") {
        Box::new(BilibiliLiveAdapter::with_cookie(cookie))
    } else {
        Box::new(DirectLiveAdapter::new())
    }
}

/// 录制会话状态机：负责把主播标记为直播中、启动/停止 ffmpeg、持久化会话。
pub struct LiveMonitor<'a> {
    pool: &'a SqlitePool,
    project_dir: &'a Path,
    config: &'a Config,
    watch: &'a Watchlist,
    notifier: Notifier<'a>,
}

impl<'a> LiveMonitor<'a> {
    pub fn new(
        pool: &'a SqlitePool,
        project_dir: &'a Path,
        config: &'a Config,
        watch: &'a Watchlist,
    ) -> Self {
        Self {
            pool,
            project_dir,
            config,
            watch,
            notifier: Notifier::new(pool, config),
        }
    }

    pub async fn run_once(&self) -> Result<LiveCheckStats> {
        let mut stats = LiveCheckStats::default();
        for item in self.watch.list_live() {
            if !item.enabled {
                continue;
            }
            let cookie = self.config.get_main("Cookie", "b站cookie");
            let adapter = resolve_live_with_cookie(&item.url, &cookie);
            // 单个直播检查总时限 25 秒：B站 room_init/主播名/标题/流地址可能串行
            // 多次网络请求，不设总超时会让整个 daemon tick 被拖到 70 秒上限。
            match tokio::time::timeout(Duration::from_secs(25), adapter.check(&item.url)).await {
                Ok(Ok(status)) if status.is_live => {
                    stats.checked += 1;
                    if let Err(e) = self.ensure_recording(&item, &status).await {
                        warn!("[{}] 开始录制失败: {e:#}", item.remark);
                    }
                }
                Ok(Ok(_)) => {
                    stats.checked += 1;
                    self.finish_if_stopped(&item.url).await;
                }
                Ok(Err(e)) => error!("[{}] 直播检测失败: {e:#}", item.remark),
                Err(_) => error!("[{}] 直播检测超时（25 秒），跳过本轮", item.remark),
            }
        }
        Ok(stats)
    }

    async fn ensure_recording(&self, item: &LiveItem, status: &LiveStatus) -> Result<()> {
        if let Some(session) = db::active_live_session(self.pool, &item.url).await? {
            if session.pid > 0 && process_alive(session.pid as u32) {
                info!("[{}] 已在录制 (session {})", item.remark, session.id);
                return Ok(());
            }
            if session.pid > 0 {
                warn!("[{}] 检测到录制进程已退出，结束旧会话", item.remark);
                db::end_live_session(self.pool, session.id, "interrupted", "录制进程已退出")
                    .await?;
            }
        }
        let anchor = if status.anchor_name.is_empty() {
            item.remark.clone()
        } else {
            status.anchor_name.clone()
        };
        let output = self
            .output_path(item, status, &anchor)
            .context("构造录制输出路径")?;
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建录制目录失败: {}", parent.display()))?;
        }
        let args = self.ffmpeg_args(&status.stream_url, &output);
        let ffmpeg = self.ffmpeg_bin();
        let mut cmd = Command::new(&ffmpeg);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            // ffmpeg 放进独立进程组，停止时按组结束，避免孤儿子进程。
            unsafe {
                cmd.pre_exec(|| {
                    libc::setpgid(0, 0);
                    Ok(())
                });
            }
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("启动 ffmpeg 失败，请检查 ffmpeg: {}", ffmpeg.display()))?;
        let pid = child.id().unwrap_or(0);
        write_rust_live_pid(self.project_dir, &item.url, pid);
        let session_id = db::start_live_session(
            self.pool,
            &item.url,
            &anchor,
            &status.title,
            adapter_name(&item.url),
            &status.stream_url,
            pid,
            &output.display().to_string(),
        )
        .await?;
        info!("[{}] 开始录制 -> {}", anchor, output.display());

        // 通知开播；失败只记日志，不阻断录制。
        let title = "【尊者直播监控】主播开播".to_string();
        let content = format!(
            "主播：{}\n标题：{}\n链接：{}",
            anchor, status.title, item.url
        );
        let channel = Channel::parse(&self.watch.notify_for(&item.url, &anchor));
        if let Err(e) = self
            .notifier
            .send("live_start", &anchor, &title, &content, channel)
            .await
        {
            warn!("[{}] 开播通知失败: {e:#}", anchor);
        }

        // 后台等待 ffmpeg 退出并回收状态，不阻塞检测循环。
        let pool = self.pool.clone();
        let anchor_name = anchor.clone();
        let project_dir = self.project_dir.to_path_buf();
        let output_path = output.display().to_string();
        let split_enabled = self.config.is_yes("录制设置", "分段录制是否开启", false);
        tokio::spawn(async move {
            // 等待 ffmpeg 退出后回收状态。若会话已被 stop_all_recordings 结束
            // （status 不再是 recording），只清理文件进程记录，不覆盖停止状态，
            // 也不在主动停止后跑复盘流水线。
            let (code, log) = wait_child(&mut child).await;
            let still_recording = db::live_session_recording(&pool, session_id)
                .await
                .unwrap_or(false);
            if !still_recording {
                info!("[{}] 会话已被外部停止，跳过状态回收", anchor_name);
                return;
            }
            if code == 0 {
                info!("[{}] 录制正常结束", anchor_name);
                let _ = db::end_live_session(&pool, session_id, "done", "").await;
                // 录制结束后接复盘流水线；失败只记日志，不拖垮 daemon。
                let media = if split_enabled {
                    find_latest_segment(&output_path).unwrap_or_else(|| output_path.clone())
                } else {
                    output_path.clone()
                };
                if let Err(e) = crate::pipeline_bridge::run_pipeline_by_project(
                    &project_dir,
                    &media,
                    &anchor_name,
                )
                .await
                {
                    warn!("[{}] 复盘流水线失败: {e:#}", anchor_name);
                }
            } else {
                let msg = format!("录制退出 code={code}: {}", last_log_lines(&log, 200));
                error!("[{}] {msg}", anchor_name);
                let _ = db::end_live_session(&pool, session_id, "failed", &msg).await;
            }
        });
        Ok(())
    }

    /// 直播已结束但 DB 还有活动会话时，结束它。
    async fn finish_if_stopped(&self, url: &str) {
        if let Some(session) = db::active_live_session(self.pool, url).await.ok().flatten() {
            if session.pid > 0 && process_alive(session.pid as u32) {
                return;
            }
            let _ = db::end_live_session(self.pool, session.id, "interrupted", "直播已结束").await;
            info!("[{}] 直播结束，会话已回收", session.anchor);
        }
    }

    /// 停止所有录制中的会话（daemon 退出/服务停止时调用），避免 ffmpeg 孤儿进程。
    pub async fn stop_all_recordings(&self) -> Result<usize> {
        let sessions = db::list_live_sessions(self.pool, "recording", 1000).await?;
        let mut stopped = 0;
        for session in sessions {
            if session.pid > 0 && process_alive(session.pid as u32) {
                terminate_process(session.pid as u32);
                stopped += 1;
                info!("[{}] 已停止录制进程 pid={}", session.anchor, session.pid);
            }
            let _ = db::end_live_session(self.pool, session.id, "stopped", "服务停止").await;
        }
        Ok(stopped)
    }

    fn ffmpeg_bin(&self) -> PathBuf {
        let bundled = self.project_dir.join("ffmpeg").join("ffmpeg");
        if bundled.exists() {
            bundled
        } else {
            PathBuf::from("ffmpeg")
        }
    }

    fn output_path(&self, item: &LiveItem, status: &LiveStatus, anchor: &str) -> Result<PathBuf> {
        let save_path = self.config.get_main("录制设置", "直播保存路径(不填则默认)");
        let root = if save_path.trim().is_empty() {
            self.project_dir.join("downloads")
        } else {
            PathBuf::from(save_path.trim())
        };
        let platform_dir = platform_dir_name(&item.url);
        let mut dir = root.join(platform_dir);
        let folder_by_author = self
            .config
            .is_yes("录制设置", "保存文件夹是否以作者区分", true);
        if folder_by_author {
            dir = dir.join(clean_name(anchor));
        }
        let save_type = self
            .config
            .get_main("录制设置", "视频保存格式ts|mkv|flv|mp4|mp3音频|m4a音频");
        let ext = match save_type.trim().to_lowercase().as_str() {
            "mkv" => "mkv",
            "flv" => "flv",
            "mp4" => "mp4",
            "mp3" | "mp3音频" => "mp3",
            "m4a" | "m4a音频" => "m4a",
            _ => "ts",
        };
        let now = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
        let filename_by_title = self
            .config
            .is_yes("录制设置", "保存文件名是否包含标题", false);
        let title_part = if filename_by_title && !status.title.is_empty() {
            format!("{}_", clean_name(&status.title))
        } else {
            String::new()
        };
        Ok(dir.join(format!("{anchor}_{title_part}{now}.{ext}")))
    }

    fn ffmpeg_args(&self, stream_url: &str, output: &Path) -> Vec<String> {
        let split_enabled = self.config.is_yes("录制设置", "分段录制是否开启", false);
        let mut args = vec![
            "-y".to_string(),
            "-hide_banner".to_string(),
            "-loglevel".to_string(),
            "error".to_string(),
            "-rw_timeout".to_string(),
            "15000000".to_string(),
            "-user_agent".to_string(),
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36".to_string(),
            "-reconnect_delay_max".to_string(),
            "60".to_string(),
            "-reconnect_streamed".to_string(),
            "-reconnect_at_eof".to_string(),
            "-i".to_string(),
            stream_url.to_string(),
            "-map".to_string(),
            "0".to_string(),
            "-c:v".to_string(),
            "copy".to_string(),
            "-c:a".to_string(),
            "copy".to_string(),
            "-bsf:a".to_string(),
            "aac_adtstoasc".to_string(),
        ];
        let ext = output
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("ts")
            .to_lowercase();
        if split_enabled {
            let split_time = self
                .config
                .get_main("录制设置", "视频分段时间(秒)")
                .parse::<u64>()
                .unwrap_or(1800)
                .to_string();
            args.extend([
                "-f".into(),
                "segment".into(),
                "-segment_time".into(),
                split_time,
                "-reset_timestamps".into(),
                "1".into(),
            ]);
            let templ = output.with_file_name(format!(
                "{}_%03d.{}",
                output.file_stem().and_then(|s| s.to_str()).unwrap_or("rec"),
                ext
            ));
            args.push(templ.display().to_string());
        } else {
            match ext.as_str() {
                "mp4" => {
                    args.extend(["-movflags".into(), "+faststart".into()]);
                }
                "flv" => {
                    args.extend(["-f".into(), "flv".into()]);
                }
                "mkv" => {
                    args.extend(["-f".into(), "matroska".into()]);
                }
                "mp3" => {
                    args.extend([
                        "-map".into(),
                        "0:a".into(),
                        "-c:a".into(),
                        "libmp3lame".into(),
                        "-ab".into(),
                        "320k".into(),
                    ]);
                }
                "m4a" => {
                    args.extend([
                        "-map".into(),
                        "0:a".into(),
                        "-c:a".into(),
                        "aac".into(),
                        "-ab".into(),
                        "320k".into(),
                    ]);
                }
                _ => {
                    args.extend(["-f".into(), "mpegts".into()]);
                }
            }
            args.push(output.display().to_string());
        }
        args
    }
}

fn adapter_name(url: &str) -> &'static str {
    if url.contains("live.bilibili.com") {
        "bilibili"
    } else if url.contains(".m3u8") || url.contains(".flv") {
        "direct"
    } else {
        "unknown"
    }
}

/// 分段录制时找目录内最新一个媒体文件，作为复盘输入。
fn find_latest_segment(output_template: &str) -> Option<String> {
    let path = Path::new(output_template);
    let dir = path.parent()?;
    let prefix = path.file_stem().and_then(|s| s.to_str())?;
    let prefix = prefix.split("_%03d").next().unwrap_or(prefix);
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(prefix))
                .unwrap_or(false)
        })
        .collect();
    candidates.sort_by_key(|p| p.metadata().and_then(|m| m.modified()).ok());
    candidates.last().map(|p| p.display().to_string())
}

async fn wait_child(child: &mut Child) -> (i32, String) {
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout).await;
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr).await;
    }
    let status = child.wait().await;
    let code = status
        .as_ref()
        .map(|s| s.code().unwrap_or(-1))
        .unwrap_or(-1);
    (code, format!("{stdout}\n{stderr}"))
}

fn last_log_lines(log: &str, max_bytes: usize) -> String {
    let chars: Vec<char> = log.chars().collect();
    let start = chars.len().saturating_sub(max_bytes);
    let start = chars
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, c)| **c == '\n')
        .map(|(i, _)| i + 1)
        .unwrap_or(start);
    chars[start..].iter().collect()
}

fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, 0) == 0
    }
    #[cfg(not(unix))]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}")])
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout).lines().any(|line| {
                    let mut parts = line.split_whitespace();
                    // tasklist 输出首列是 Image Name，第二列才是 PID。
                    let _image = parts.next();
                    parts
                        .next()
                        .and_then(|p| p.parse::<u32>().ok())
                        .map(|p| p == pid)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    }
}

fn terminate_process(pid: u32) {
    #[cfg(unix)]
    unsafe {
        let _ = libc::kill(-(pid as i32), libc::SIGTERM);
    }
    #[cfg(not(unix))]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status();
    }
}

fn clean_name(input: &str) -> String {
    let mut out = String::new();
    for ch in input.trim().chars() {
        match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '&' | '#' | '.' | ',' | '，'
            | '。' | '~' | '！' | '·' | ' ' => out.push('_'),
            _ => out.push(ch),
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "空白昵称".to_string()
    } else {
        trimmed
    }
}

fn platform_dir_name(url: &str) -> &'static str {
    if url.contains("live.bilibili.com") {
        "B站直播"
    } else if url.contains("douyin.com") {
        "抖音直播"
    } else if url.contains("kuaishou.com") {
        "快手直播"
    } else if url.contains("douyu.com") {
        "斗鱼直播"
    } else if url.contains("huya.com") {
        "虎牙直播"
    } else {
        "其他直播"
    }
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct LiveCheckStats {
    pub checked: usize,
    pub started: usize,
}

/// 手动录制一次直播：检测流地址 -> 启动 ffmpeg 录 duration 秒 -> 等待退出。
/// 用于命令行/界面一键验证。duration=0 时也会监听中断信号并清理 ffmpeg，
/// 避免 Ctrl-C 后留下孤儿录制进程；不写入 live_sessions（避免和 daemon 状态机冲突）。
pub async fn record_live_once(
    pool: &SqlitePool,
    project_dir: &Path,
    config: &Config,
    url: &str,
    duration_secs: u64,
) -> Result<String> {
    let cookie = config.get_main("Cookie", "b站cookie");
    let adapter = resolve_live_with_cookie(url, &cookie);
    let status = adapter.check(url).await.context("直播检测失败")?;
    if !status.is_live {
        bail!("当前未开播，无法录制: {url}");
    }
    let watch = Watchlist::new(project_dir.to_path_buf());
    let item = LiveItem {
        url: url.to_string(),
        remark: status.anchor_name.clone(),
        quality: String::new(),
        enabled: true,
        notify: String::new(),
    };
    let monitor = LiveMonitor::new(pool, project_dir, config, &watch);
    let output = monitor.output_path(&item, &status, &status.anchor_name)?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let ffmpeg = monitor.ffmpeg_bin();
    let mut args = monitor.ffmpeg_args(&status.stream_url, &output);
    if duration_secs > 0 {
        args.extend(["-t".into(), duration_secs.to_string()]);
    }
    let mut cmd = Command::new(&ffmpeg);
    let pid_for_cleanup = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let cleanup_path = project_dir
        .join("fupan_output")
        .join("run")
        .join("rust_live");
    let cleanup_hash = crate::service::rust_live_pid_name(url);
    let cleanup_file = cleanup_path.join(format!("{cleanup_hash}.pid"));
    let _ = std::fs::create_dir_all(&cleanup_path);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        unsafe {
            cmd.pre_exec(|| {
                libc::setpgid(0, 0);
                Ok(())
            });
        }
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("启动 ffmpeg 失败: {}", ffmpeg.display()))?;
    let pid = child.id().unwrap_or(0);
    pid_for_cleanup.store(pid, std::sync::atomic::Ordering::SeqCst);
    if pid > 0 {
        let _ = std::fs::create_dir_all(&cleanup_path);
        let _ = std::fs::write(&cleanup_file, pid.to_string());
    }
    // 异步等待 Ctrl-C/SIGTERM；到点后结束 ffmpeg 进程组，避免孤儿。
    let cleanup_pid_2 = pid_for_cleanup.clone();
    let cleanup_file_2 = cleanup_file.clone();
    let cleanup_task = tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            if let Ok(mut sigterm) = signal(SignalKind::terminate()) {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = sigterm.recv() => {}
                }
            } else {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        let pid = cleanup_pid_2.load(std::sync::atomic::Ordering::SeqCst);
        if pid > 0 {
            terminate_process(pid);
        }
        let _ = std::fs::remove_file(&cleanup_file_2);
    });
    let (code, log) = wait_child(&mut child).await;
    cleanup_task.abort();
    let _ = std::fs::remove_file(&cleanup_file);
    if code != 0 {
        if code == -1 {
            anyhow::bail!("录制被中断: {}", last_log_lines(&log, 300));
        }
        anyhow::bail!("录制失败 code={code}: {}", last_log_lines(&log, 300));
    }
    Ok(output.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_name_removes_invalid_chars() {
        assert_eq!(clean_name(" 主播/甲:测试? "), "主播_甲_测试");
        assert_eq!(clean_name("////"), "空白昵称");
    }

    #[test]
    fn platform_dir_maps_common_hosts() {
        assert_eq!(platform_dir_name("https://live.bilibili.com/1"), "B站直播");
        assert_eq!(platform_dir_name("https://live.douyin.com/1"), "抖音直播");
        assert_eq!(platform_dir_name("https://x.test/1"), "其他直播");
    }

    #[test]
    fn last_lines_keeps_tail() {
        let log = "a\nb\nc\nd";
        assert!(last_log_lines(log, 3).contains('d'));
    }

    #[test]
    fn terminate_process_does_not_panic() {
        // 不存在的 pid 调用不应 panic；真实清理按 DB 会话驱动。
        terminate_process(u32::MAX);
    }
}
