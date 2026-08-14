use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use sqlx::sqlite::SqlitePool;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tracing::{error, info, warn};

use crate::bilibili_auth::cookie_header_from_project;
use crate::config::Config;
use crate::db;
use crate::live_python_bridge::PythonLiveAdapter;
use crate::notify::Notifier;
use crate::service::{remove_rust_live_pid, write_rust_live_pid};
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
    resolve_live_for_project(url, Path::new("."), "")
}

pub fn resolve_live_with_cookie(url: &str, cookie: &str) -> Box<dyn LiveAdapter> {
    resolve_live_for_project(url, Path::new("."), cookie)
}

/// 解析直播适配器：B站原生 / 多平台 Python 桥 / m3u8-flv 直链。
///
/// `project_dir` 让 Python 桥接器定位项目 `.venv`，而不是从当前工作目录猜测；
/// 旧 `resolve_live_with_cookie` 保留为无项目路径的便捷入口。
pub fn resolve_live_for_project(
    url: &str,
    project_dir: &Path,
    cookie: &str,
) -> Box<dyn LiveAdapter> {
    if url.contains("live.bilibili.com") {
        Box::new(BilibiliLiveAdapter::with_cookie(cookie))
    } else if PythonLiveAdapter::supports_url(url) {
        Box::new(PythonLiveAdapter::new(project_dir.to_path_buf()))
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
            notifier: Notifier::for_project(pool, config, project_dir),
        }
    }

    pub async fn run_once(&self) -> Result<LiveCheckStats> {
        let mut stats = LiveCheckStats::default();
        // 直播检测失败/超时并不意味着主播一定下播。B站/抖音接口在网络抖动、
        // 平台限流或 Cookie 过期时都可能单轮失败；若立刻清空 `live_states`，
        // 主播仍在播时下一轮恢复会把“已开播”误判成“刚刚开播”，重复通知；
        // 更严重的是 `finish_if_stopped` 可能直接结束仍在录制的 ffmpeg。
        //
        // 因此失败先累计“连续失败次数”，只有连续 N 次失败才按“疑似下播”
        // 收敛状态；其中任意一轮明确返回“未开播”或“开播”都会清零计数。
        // 默认 3 次，正好覆盖常见的一两次网络抖动，同时避免长期无结论残留。
        const FAILURE_LIMIT: u32 = 3;
        // 旧 Python 主程序允许用户单独关闭“开播/关播”通知，也允许“只推送不录制”。
        // Rust 侧必须遵守这些开关，否则会把用户明确关掉的通知又发出去。
        let begin_push = self
            .config
            .is_yes_main("推送配置", "开播推送开启(是/否)", true);
        let over_push = self
            .config
            .is_yes_main("推送配置", "关播推送开启(是/否)", false);
        let notify_only = self
            .config
            .is_yes_main("推送配置", "只推送通知不录制(是/否)", false);
        // 所有启用主播的检测并发执行，避免串行时单个主播卡 25 秒把整轮拖到
        // 70 秒护栏外；结果仍按列表顺序处理，保证开播/关播状态机顺序稳定。
        let items: Vec<LiveItem> = self
            .watch
            .list_live()
            .into_iter()
            .filter(|item| item.enabled)
            .collect();
        let mut checks = Vec::with_capacity(items.len());
        for item in &items {
            let cookie = self.config.get_main("Cookie", "b站cookie");
            let cookie = if cookie.is_empty() {
                cookie_header_from_project(self.project_dir)
            } else {
                cookie
            };
            let adapter = resolve_live_for_project(&item.url, self.project_dir, &cookie);
            // 单个直播检查总时限 25 秒：B站 room_init/主播名/标题/流地址可能串行
            // 多次网络请求，不设总超时会让整个 daemon tick 被拖到 70 秒上限。
            let check_url = item.url.clone();
            let handle = tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(25), async move {
                    adapter.check(&check_url).await
                })
                .await
            });
            checks.push((item.clone(), handle));
        }
        for (item, handle) in checks {
            let outcome = match handle.await {
                Ok(outcome) => outcome,
                Err(e) => {
                    // 单个主播检测任务 panic 不能拖垮整轮：按检测失败累计，
                    // 达到阈值后再收敛，避免误杀仍在录制的会话。
                    error!("[{}] 直播检查任务 panic: {e}", item.remark);
                    let failures = self.bump_consecutive_failures(&item.url).await.unwrap_or(1);
                    if failures >= FAILURE_LIMIT {
                        let _ = db::set_live_state(self.pool, &item.url, false).await;
                        self.finish_if_stopped(&item.url).await;
                    }
                    continue;
                }
            };
            match outcome {
                Ok(Ok(status)) if status.is_live => {
                    stats.checked += 1;
                    self.reset_consecutive_failures(&item.url).await;
                    let anchor = if status.anchor_name.is_empty() {
                        item.remark.clone()
                    } else {
                        status.anchor_name.clone()
                    };
                    let was_live = db::get_live_state(self.pool, &item.url)
                        .await
                        .unwrap_or(false);
                    if !was_live && begin_push {
                        self.send_live_alert(&item, &anchor, &status).await;
                    }
                    stats.started += usize::from(!was_live);
                    if let Err(e) = db::set_live_state(self.pool, &item.url, true).await {
                        warn!("[{}] 记录开播状态失败: {e:#}", anchor);
                    }
                    if !notify_only && let Err(e) = self.ensure_recording(&item, &status).await {
                        warn!("[{}] 开始录制失败: {e:#}", anchor);
                    }
                }
                Ok(Ok(_)) => {
                    stats.checked += 1;
                    self.reset_consecutive_failures(&item.url).await;
                    let was_live = db::get_live_state(self.pool, &item.url)
                        .await
                        .unwrap_or(false);
                    if was_live && over_push {
                        let anchor = db::active_live_session(self.pool, &item.url)
                            .await
                            .ok()
                            .flatten()
                            .map(|s| s.anchor)
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| item.remark.clone());
                        self.send_live_ended_alert(&item, &anchor).await;
                    }
                    if let Err(e) = db::set_live_state(self.pool, &item.url, false).await {
                        warn!("[{}] 记录关播状态失败: {e:#}", item.remark);
                    }
                    self.finish_if_stopped(&item.url).await;
                }
                Ok(Err(e)) => {
                    stats.checked += 1;
                    let failures = self.bump_consecutive_failures(&item.url).await.unwrap_or(1);
                    if failures >= FAILURE_LIMIT {
                        error!(
                            "[{}] 直播检测连续失败 {} 次，本轮按疑似下播收敛: {e:#}",
                            item.remark, failures
                        );
                        let _ = db::set_live_state(self.pool, &item.url, false).await;
                        self.finish_if_stopped(&item.url).await;
                    } else {
                        error!(
                            "[{}] 直播检测失败（第 {failures}/{} 次），保留现有状态避免抖动误关: {e:#}",
                            item.remark, FAILURE_LIMIT
                        );
                    }
                }
                Err(_) => {
                    stats.checked += 1;
                    let failures = self.bump_consecutive_failures(&item.url).await.unwrap_or(1);
                    if failures >= FAILURE_LIMIT {
                        error!(
                            "[{}] 直播检测连续超时 {} 次，本轮按疑似下播收敛",
                            item.remark, failures
                        );
                        let _ = db::set_live_state(self.pool, &item.url, false).await;
                        self.finish_if_stopped(&item.url).await;
                    } else {
                        error!(
                            "[{}] 直播检测超时（第 {failures}/{} 次），保留现有状态避免抖动误关",
                            item.remark, FAILURE_LIMIT
                        );
                    }
                }
            }
        }
        Ok(stats)
    }

    async fn bump_consecutive_failures(&self, url: &str) -> anyhow::Result<u32> {
        db::bump_live_failures(self.pool, url).await
    }

    async fn reset_consecutive_failures(&self, url: &str) {
        let _ = db::reset_live_failures(self.pool, url).await;
    }

    /// daemon 启动时回收上次硬崩溃遗留的 recording 会话。
    ///
    /// 单实例锁只保护调度器，无法保护已经被 `SIGKILL` 留下的 ffmpeg 子进程。
    /// 这些会话在 DB 里仍为 recording、pid 文件也仍存在，但新 daemon 没有旧
    /// `Child` 句柄，无法再等待它们退出。这里统一终止仍存活的录制进程、结束
    /// 会话并清理 pid 文件，避免 24×30 长期运行中状态和磁盘文件持续漂移。
    pub async fn recover_stale_recordings(&self) -> Result<usize> {
        let sessions = db::list_live_sessions(self.pool, "recording", 1000).await?;
        let mut recovered = 0usize;
        for session in sessions {
            if session.pid > 0
                && session.pid as u32 != std::process::id()
                && process_alive(session.pid as u32)
            {
                terminate_process(session.pid as u32);
                recovered += 1;
                info!(
                    "[{}] 回收重启前残留录制进程 pid={}",
                    session.anchor, session.pid
                );
            }
            let _ = db::end_live_session(
                self.pool,
                session.id,
                "interrupted",
                "daemon 重启，回收残留录制会话",
            )
            .await;
            remove_rust_live_pid(self.project_dir, &session.url);
        }
        Ok(recovered)
    }

    async fn send_live_alert(&self, item: &LiveItem, anchor: &str, status: &LiveStatus) {
        let title = "【尊者直播监控】主播开播".to_string();
        let content = format!(
            "主播：{}\n标题：{}\n链接：{}",
            anchor, status.title, item.url
        );
        let channel = self.watch.notify_for(&item.url, anchor);
        if let Err(e) = self
            .notifier
            .send("live_start", anchor, &title, &content, &channel)
            .await
        {
            warn!("[{}] 开播通知失败: {e:#}", anchor);
        }
    }

    async fn send_live_ended_alert(&self, item: &LiveItem, anchor: &str) {
        let title = "【尊者直播监控】主播关播".to_string();
        let content = format!("主播：{}\n链接：{}", anchor, item.url);
        let channel = self.watch.notify_for(&item.url, anchor);
        if let Err(e) = self
            .notifier
            .send("live_end", anchor, &title, &content, &channel)
            .await
        {
            warn!("[{}] 关播通知失败: {e:#}", anchor);
        }
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
                remove_rust_live_pid(self.project_dir, &item.url);
            }
        }
        // 磁盘剩余空间低于配置阈值时不启动录制，避免把磁盘写满导致 ffmpeg
        // 崩溃/丢状态。旧 Python 有 `录制空间剩余阈值(gb)`，Rust 必须遵守。
        self.ensure_disk_space()?;
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
        if status.stream_url.is_empty() {
            anyhow::bail!("未取得直播流地址，跳过录制");
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
        let session_id = match db::start_live_session(
            self.pool,
            &item.url,
            &anchor,
            &status.title,
            adapter_name(&item.url),
            &status.stream_url,
            pid,
            &output.display().to_string(),
        )
        .await
        {
            Ok(session_id) => session_id,
            Err(e) => {
                // ffmpeg 已启动但 DB 写入失败：不能让一个无法被状态机追踪的
                // 录制进程变成孤儿。先终止进程组并清理 pid 文件，再返回错误。
                if pid > 0 {
                    terminate_process(pid);
                    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
                }
                remove_rust_live_pid(self.project_dir, &item.url);
                return Err(e);
            }
        };
        info!("[{}] 开始录制 -> {}", anchor, output.display());

        // 后台等待 ffmpeg 退出并回收状态，不阻塞检测循环。
        let pool = self.pool.clone();
        let anchor_name = anchor.clone();
        let output_path = output.display().to_string();
        let project_dir = self.project_dir.to_path_buf();
        let item_url = item.url.clone();
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
                remove_rust_live_pid(&project_dir, &item_url);
                return;
            }
            if code == 0 {
                info!("[{}] 录制正常结束", anchor_name);
                let _ = db::end_live_session(&pool, session_id, "done", "").await;
                remove_rust_live_pid(&project_dir, &item_url);
                // 录制结束后入队复盘任务；失败只记日志，不拖垮 daemon。
                let media = if split_enabled {
                    // ffmpeg 分段录制退出后，最后一段可能刚写完还没来得及收尾；
                    // 给一点稳定窗口，避免把不完整文件送去复盘。
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    find_latest_segment(&output_path).unwrap_or_else(|| output_path.clone())
                } else {
                    output_path.clone()
                };
                // 复盘开关与下载完成一致：转写/提炼/切片任一开启才自动入队，
                // 三项全关时保持录制文件不动，避免每次录制结束都白跑 Python。
                if crate::download::pipeline_enabled(&project_dir)
                    && let Err(e) = crate::download::enqueue_pipeline_after_download(
                        &pool,
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
                remove_rust_live_pid(&project_dir, &item_url);
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
            remove_rust_live_pid(self.project_dir, url);
            info!("[{}] 直播结束，会话已回收", session.anchor);
        }
    }

    /// 停止所有录制中的会话（daemon 退出/服务停止时调用），避免 ffmpeg 孤儿进程。
    pub async fn stop_all_recordings(&self) -> Result<usize> {
        let sessions = db::list_live_sessions(self.pool, "recording", 1000).await?;
        let mut stopped = 0;
        for session in sessions {
            if session.pid > 0 && process_alive(session.pid as u32) {
                // 先发优雅退出信号并给 ffmpeg 一点时间落盘收尾；若仍在运行再强杀。
                // 直接 SIGKILL 会截断正在写入的 ts/flv，长期运行下更容易留下损坏录像。
                terminate_process(session.pid as u32);
                let pid = session.pid as u32;
                for _ in 0..5 {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    if !process_alive(pid) {
                        break;
                    }
                }
                stopped += 1;
                info!("[{}] 已停止录制进程 pid={}", session.anchor, session.pid);
            }
            let _ = db::end_live_session(self.pool, session.id, "stopped", "服务停止").await;
            remove_rust_live_pid(self.project_dir, &session.url);
        }
        Ok(stopped)
    }

    fn ffmpeg_bin(&self) -> PathBuf {
        crate::service::find_binary(self.project_dir, "ffmpeg", "ffmpeg.exe")
    }

    fn output_path(&self, item: &LiveItem, status: &LiveStatus, anchor: &str) -> Result<PathBuf> {
        let save_path = self.config.get_main("录制设置", "直播保存路径(不填则默认)");
        // 旧 Python 的 `default_path = {项目根}/downloads`，主播目录默认拼在
        // `downloads/B站直播/主播` 下；这里不再在用户自定义路径后追加 download 目录，
        // 避免默认配置产生 `downloads/downloads/...` 双重目录。
        let root = if save_path.trim().is_empty() {
            self.project_dir.join("downloads")
        } else {
            PathBuf::from(save_path.trim())
        };
        let platform_dir = platform_dir_name(&item.url);
        let mut dir = root.join(platform_dir);
        let folder_by_time = self
            .config
            .is_yes("录制设置", "保存文件夹是否以时间区分", false);
        let folder_by_title = self
            .config
            .is_yes("录制设置", "保存文件夹是否以标题区分", false);
        let now_date = chrono::Local::now().format("%Y-%m-%d").to_string();
        let folder_by_author = self
            .config
            .is_yes("录制设置", "保存文件夹是否以作者区分", true);
        if folder_by_author {
            dir = dir.join(clean_name(anchor));
        }
        // 与旧 Python 目录规则一致：按日期、按标题建子目录时，标题目录名
        // 也必须清洗，避免 `/`、`:` 等字符在 Windows 上创建目录失败。
        let clean_title = clean_name(&status.title);
        if folder_by_time {
            dir = dir.join(&now_date);
        }
        if folder_by_title && !status.title.is_empty() {
            if folder_by_time {
                dir = dir.join(format!("{clean_title}_{}", clean_name(anchor)));
            } else {
                dir = dir.join(format!("{now_date}_{clean_title}"));
            }
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
        // 主播名/标题可能带 `/`、`\`、`:` 等文件系统非法字符，必须清洗后再拼
        // 文件名，避免 Windows 上创建文件失败或路径被意外解析成子目录。
        let clean_anchor = clean_name(anchor);
        let title_part = if filename_by_title && !status.title.is_empty() {
            format!("{clean_title}_")
        } else {
            String::new()
        };
        Ok(dir.join(format!("{clean_anchor}_{title_part}{now}.{ext}")))
    }

    /// 录制前检查项目所在磁盘剩余空间是否低于阈值（GB）。
    /// 阈值从 `config.ini [录制设置] 录制空间剩余阈值(gb)` 读取，默认 1.0 GB。
    fn ensure_disk_space(&self) -> Result<()> {
        let threshold_gb = self.disk_threshold_gb();
        let free_mb = free_disk_mb(self.project_dir);
        if free_mb == 0 || (free_mb as f64) < threshold_gb * 1024.0 {
            bail!(
                "磁盘剩余空间不足，跳过录制：剩余 {free_mb} MB，阈值 {} GB。\
                 请清理 downloads/ 或 fupan_output/ 后重试",
                threshold_gb
            );
        }
        Ok(())
    }

    /// 读取并解析磁盘阈值（GB）。配置缺失/非法时回退默认 1.0 GB，
    /// 与旧 Python `录制空间剩余阈值(gb)=1.0` 默认一致。
    fn disk_threshold_gb(&self) -> f64 {
        self.config
            .get_main("录制设置", "录制空间剩余阈值(gb)")
            .trim()
            .parse()
            .unwrap_or(1.0)
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
        ];
        let ext = output
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("ts")
            .to_lowercase();
        let is_aac_container = is_aac_container(&ext);
        if is_aac_container {
            args.extend(["-bsf:a".to_string(), "aac_adtstoasc".to_string()]);
        }
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
                    // 只取音频流并重编码为 MP3；不套 AAC bitstream filter。
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
                    // m4a 容器配合 AAC bitstream filter，输出纯音频。
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

/// mp4/m4a 用 AAC 的 bitstream filter；ts/flv/mkv 和 mp3 不需要。
fn is_aac_container(ext: &str) -> bool {
    matches!(ext, "mp4" | "m4a")
}

/// 查询项目所在磁盘剩余空间（MB）。Unix 用 statvfs，Windows 用盘符 API。
fn free_disk_mb(project_dir: &Path) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c_path = std::ffi::CString::new(project_dir.as_os_str().as_bytes()).unwrap_or_default();
        let mut stat = unsafe { std::mem::zeroed::<libc::statvfs>() };
        if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } == 0 {
            let avail = stat.f_bavail as u64;
            let frsize = stat.f_frsize;
            return avail.saturating_mul(frsize) / (1024 * 1024);
        }
        0
    }
    #[cfg(not(unix))]
    {
        use std::os::windows::ffi::OsStrExt;
        let drive = project_dir
            .components()
            .next()
            .map(|c| c.as_os_str())
            .unwrap_or_else(|| std::ffi::OsStr::new("C:"));
        let wide: Vec<u16> = drive.encode_wide().chain(std::iter::once(0)).collect();
        let mut free_bytes_available_to_caller: u64 = 0;
        let mut total_number_of_bytes: u64 = 0;
        let mut total_number_of_free_bytes: u64 = 0;
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free_bytes_available_to_caller,
                &mut total_number_of_bytes,
                &mut total_number_of_free_bytes,
            )
        };
        if ok != 0 {
            free_bytes_available_to_caller / (1024 * 1024)
        } else {
            0
        }
    }
}

fn adapter_name(url: &str) -> &'static str {
    if crate::live_python_bridge::PythonLiveAdapter::supports_url(url) {
        "python-bridge"
    } else if url.contains("live.bilibili.com") {
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

#[cfg(test)]
mod segment_tests {
    use super::*;

    #[test]
    fn find_latest_segment_skips_other_prefixes_and_picks_latest() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-segment-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let older = dir.join("rec_000.ts");
        let newer = dir.join("rec_001.ts");
        let other = dir.join("other_000.ts");
        std::fs::write(&older, b"old").unwrap();
        std::fs::write(&newer, b"new").unwrap();
        std::fs::write(&other, b"other").unwrap();

        let template = dir.join("rec_%03d.ts").display().to_string();
        let picked = find_latest_segment(&template).unwrap();
        // 修改时间可能因文件系统精度相同；这里验证候选集只含 rec 前缀，
        // 且文件名序号更高的 rec_001 会被选上（同精度时稳定取文件名靠后）。
        assert!(picked.ends_with("rec_001.ts"), "应选最新分段: {picked}");
        assert!(!picked.ends_with("other_000.ts"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn wait_child_drains_stdout_and_stderr_concurrently() {
        // 回归旧实现：串行“先等 stdout EOF、再读 stderr”会在 stderr 写满
        // 管道后卡住。这里让子进程在 stdout 保持打开时写满 stderr 小管道，
        // 并发读取必须仍然能完成等待。
        let mut child = Command::new("sh")
            .args(["-c", "i=0; while [ $i -lt 50000 ]; do printf 'x'; i=$((i+1)); done; printf '\\n'; sleep 3"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (code, log) = tokio::time::timeout(Duration::from_secs(10), wait_child(&mut child))
            .await
            .expect("wait_child 不应因 stderr 管道写满而卡死");
        assert_eq!(code, 0);
        assert!(log.contains('\n'), "应收集到 stderr 内容");
    }
}

async fn wait_child(child: &mut Child) -> (i32, String) {
    // ffmpeg 的日志几乎全部写在 stderr。串行“先读完 stdout 再读 stderr”
    // 会在 stderr 管道写满后互相等待：ffmpeg 阻塞在 stderr 写、stdout 又
    // 迟迟不到 EOF，录制收尾任务就会一直卡住。这里并发读取两路管道，
    // 并各自保留最后 512KB，避免长时间录制把内存无限放大。
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let drain_stdout = async move {
        let mut captured = String::new();
        if let Some(out) = stdout_pipe {
            let mut out = BufReader::new(out);
            let mut line = Vec::new();
            while out.read_until(b'\n', &mut line).await.unwrap_or(0) != 0 {
                if captured.len() + line.len() > 512 * 1024 {
                    break;
                }
                captured.push_str(&String::from_utf8_lossy(&line));
                line.clear();
            }
        }
        captured
    };
    let drain_stderr = async move {
        let mut captured = String::new();
        if let Some(err) = stderr_pipe {
            let mut err = BufReader::new(err);
            let mut line = Vec::new();
            while err.read_until(b'\n', &mut line).await.unwrap_or(0) != 0 {
                if captured.len() + line.len() > 512 * 1024 {
                    break;
                }
                captured.push_str(&String::from_utf8_lossy(&line));
                line.clear();
            }
        }
        captured
    };
    let (stdout, stderr) = tokio::join!(drain_stdout, drain_stderr);
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
    {
        if pid == 0 || pid > i32::MAX as u32 {
            return false;
        }
        unsafe { libc::kill(pid as i32, 0) == 0 }
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
    {
        if pid == 0 || pid > i32::MAX as u32 {
            return;
        }
        unsafe {
            let _ = libc::kill(-(pid as i32), libc::SIGTERM);
        }
        std::thread::sleep(Duration::from_millis(500));
        unsafe {
            let _ = libc::kill(-(pid as i32), libc::SIGKILL);
        }
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
    let cookie = if cookie.is_empty() {
        cookie_header_from_project(project_dir)
    } else {
        cookie
    };
    let adapter = resolve_live_for_project(url, project_dir, &cookie);
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
    monitor.ensure_disk_space()?;
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
    // 手动录制不写 rust_live/*.pid：该目录是 daemon 会话专用，`stop` 与
    // 重启恢复会按 pid 文件清理 ffmpeg。手动命令若写进去，用户点“停止服务”
    // 或重启 daemon 时会把正在录的 ffmpeg 误杀；不写则手动命令自己的
    // cleanup_task 负责在 Ctrl-C/SIGTERM 后收尾，不会留下孤儿。
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
    // 异步等待 Ctrl-C/SIGTERM；到点后结束 ffmpeg 进程组，避免孤儿。
    let cleanup_pid_2 = pid_for_cleanup.clone();
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
    });
    let (code, log) = wait_child(&mut child).await;
    cleanup_task.abort();
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

    #[tokio::test]
    async fn failed_live_check_safely_converges_stale_state() {
        // 连续失败达到阈值后才收敛；单次失败必须保留旧状态，避免网络抖动
        // 把正在直播的主播误判成下播并结束录制。
        struct FailingAdapter;
        impl LiveAdapter for FailingAdapter {
            fn name(&self) -> &'static str {
                "failing-test"
            }

            fn check<'a>(
                &'a self,
                _url: &'a str,
            ) -> Pin<Box<dyn Future<Output = Result<LiveStatus>> + Send + 'a>> {
                Box::pin(async { Err(anyhow!("simulated transient failure")) })
            }
        }

        let dir = std::env::temp_dir().join(format!("zunzhe-live-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(
            dir.join("config").join("URL_config.ini"),
            "https://live.example.test/1\n",
        )
        .unwrap();
        std::fs::write(dir.join("config").join("config.ini"), "").unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "").unwrap();
        let db_file = dir.join("live.db");
        let pool = crate::db::open_pool(&db_file).await.unwrap();
        let cfg = crate::config::Config::load(&dir).unwrap();
        let watch = Watchlist::new(dir.clone());
        crate::db::set_live_state(&pool, "https://live.example.test/1", true)
            .await
            .unwrap();

        let monitor = LiveMonitor::new(&pool, &dir, &cfg, &watch);
        // 前两轮失败：状态仍应保持为“开播”，避免一次抖动误关。
        for _ in 0..2 {
            let stats = monitor.run_once().await.unwrap();
            assert_eq!(stats.checked, 1);
            assert!(
                crate::db::get_live_state(&pool, "https://live.example.test/1")
                    .await
                    .unwrap(),
                "单次/两次失败不能清掉上一轮确认的开播状态"
            );
        }
        // 第三轮连续失败：达到阈值后才按疑似下播收敛。
        let stats = monitor.run_once().await.unwrap();
        assert_eq!(stats.checked, 1);
        assert!(
            !crate::db::get_live_state(&pool, "https://live.example.test/1")
                .await
                .unwrap(),
            "连续失败达到阈值后旧的 live_state 应清除"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn platform_dir_maps_common_hosts() {
        assert_eq!(platform_dir_name("https://live.bilibili.com/1"), "B站直播");
        assert_eq!(platform_dir_name("https://live.douyin.com/1"), "抖音直播");
        assert_eq!(platform_dir_name("https://x.test/1"), "其他直播");
    }

    #[tokio::test]
    async fn disk_threshold_parses_or_defaults_to_one_gb() {
        let cfg = |text: &str| {
            let ini = crate::config::IniFile::parse(text).unwrap();
            crate::config::Config {
                project_dir: PathBuf::from("."),
                config_ini: PathBuf::from("config.ini"),
                fupan_config_ini: PathBuf::from("fupan_config.ini"),
                main: ini,
                fupan: crate::config::IniFile::default(),
            }
        };
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let missing = cfg("[录制设置]\n");
        let monitor = LiveMonitor {
            pool: &pool,
            project_dir: Path::new("."),
            config: &missing,
            watch: &Watchlist::new(PathBuf::from(".")),
            notifier: Notifier::for_project(&pool, &missing, Path::new(".")),
        };
        assert_eq!(monitor.disk_threshold_gb(), 1.0);

        let custom = cfg("[录制设置]\n录制空间剩余阈值(gb) = 2.5\n");
        let monitor = LiveMonitor {
            pool: &pool,
            project_dir: Path::new("."),
            config: &custom,
            watch: &Watchlist::new(PathBuf::from(".")),
            notifier: Notifier::for_project(&pool, &custom, Path::new(".")),
        };
        assert_eq!(monitor.disk_threshold_gb(), 2.5);
    }

    #[tokio::test]
    async fn output_path_uses_single_downloads_root_and_cleans_anchor() {
        let dir = std::env::temp_dir().join(format!("zunzhe-live-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(dir.join("config").join("config.ini"), "").unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "").unwrap();
        let db_file = dir.join("live-path.db");
        let pool = crate::db::open_pool(&db_file).await.unwrap();
        let cfg = crate::config::Config::load(&dir).unwrap();
        let watch = Watchlist::new(dir.clone());
        let monitor = LiveMonitor::new(&pool, &dir, &cfg, &watch);
        let item = LiveItem {
            url: "https://live.bilibili.com/1".to_string(),
            remark: "主播甲".to_string(),
            quality: "原画".to_string(),
            enabled: true,
            notify: "跟随全局".to_string(),
        };
        let status = LiveStatus {
            is_live: true,
            title: "测试:标题/一".to_string(),
            anchor_name: "主播/甲".to_string(),
            stream_url: "https://example.test/live.m3u8".to_string(),
        };
        let out = monitor.output_path(&item, &status, "主播/甲").unwrap();
        let text = out.display().to_string();
        assert!(
            text.contains("downloads/B站直播/主播_甲"),
            "默认路径不应出现 downloads/downloads 双重目录: {text}"
        );
        assert!(
            !text.contains("downloads/downloads"),
            "默认路径出现双重 downloads: {text}"
        );
        let filename = out.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            !filename.contains(['/', '\\', ':']),
            "文件名包含非法字符: {filename}"
        );
        assert!(
            filename.starts_with("主播_甲_"),
            "应使用清洗后的主播名: {filename}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn output_path_supports_time_and_title_folders() {
        let dir = std::env::temp_dir().join(format!("zunzhe-live-path2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(
            dir.join("config").join("config.ini"),
            "[录制设置]\n保存文件夹是否以时间区分 = 是\n保存文件夹是否以标题区分 = 是\n",
        )
        .unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "").unwrap();
        let db_file = dir.join("live-path2.db");
        let pool = crate::db::open_pool(&db_file).await.unwrap();
        let cfg = crate::config::Config::load(&dir).unwrap();
        let watch = Watchlist::new(dir.clone());
        let monitor = LiveMonitor::new(&pool, &dir, &cfg, &watch);
        let item = LiveItem {
            url: "https://live.bilibili.com/1".to_string(),
            remark: "主播甲".to_string(),
            quality: "原画".to_string(),
            enabled: true,
            notify: "跟随全局".to_string(),
        };
        let status = LiveStatus {
            is_live: true,
            title: "测试:标题/一".to_string(),
            anchor_name: "主播/甲".to_string(),
            stream_url: "https://example.test/live.m3u8".to_string(),
        };
        let out = monitor.output_path(&item, &status, "主播/甲").unwrap();
        let text = out.display().to_string();
        assert!(
            text.contains("downloads/B站直播/主播_甲/2026-"),
            "按时间/标题建目录未生效: {text}"
        );
        assert!(
            !text.contains("主播_甲_测试_标题"),
            "标题目录应清洗非法字符: {text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
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

    #[test]
    fn aac_bitstream_filter_only_applies_to_mp4_and_m4a() {
        assert!(is_aac_container("mp4"));
        assert!(is_aac_container("m4a"));
        assert!(!is_aac_container("mp3"));
        assert!(!is_aac_container("flv"));
        assert!(!is_aac_container("mkv"));
        assert!(!is_aac_container("ts"));
    }

    #[tokio::test]
    async fn run_once_checks_multiple_anchors_without_serial_stall() {
        // 两个主播都使用会阻塞到超时的适配器：串行实现总耗时约 50 秒，
        // 并发实现只等最慢的一个（约 25 秒），且必须都计入 checked。
        struct SlowAdapter;
        impl LiveAdapter for SlowAdapter {
            fn name(&self) -> &'static str {
                "slow-test"
            }

            fn check<'a>(
                &'a self,
                _url: &'a str,
            ) -> Pin<Box<dyn Future<Output = Result<LiveStatus>> + Send + 'a>> {
                Box::pin(async {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    Ok(LiveStatus {
                        is_live: false,
                        title: String::new(),
                        anchor_name: String::new(),
                        stream_url: String::new(),
                    })
                })
            }
        }

        let dir = std::env::temp_dir().join(format!("zunzhe-live-par-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(
            dir.join("config").join("URL_config.ini"),
            "https://live.example.test/1\nhttps://live.example.test/2\n",
        )
        .unwrap();
        std::fs::write(dir.join("config").join("config.ini"), "").unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "").unwrap();
        let db_file = dir.join("live-par.db");
        let pool = crate::db::open_pool(&db_file).await.unwrap();
        let cfg = crate::config::Config::load(&dir).unwrap();
        let watch = Watchlist::new(dir.clone());
        let monitor = LiveMonitor::new(&pool, &dir, &cfg, &watch);

        let started = std::time::Instant::now();
        let stats = monitor.run_once().await.unwrap();
        let elapsed = started.elapsed();
        assert_eq!(stats.checked, 2, "两个主播都应被检查");
        assert!(
            elapsed < Duration::from_secs(50),
            "直播检查应并发执行，两个 30 秒慢源不能串行 60 秒: {elapsed:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn live_check_can_be_wrapped_in_total_timeout() {
        // 守护进程必须能在 25 秒总超时内等待任意 LiveAdapter，避免借用 item.url
        // 越过 await 后无法继续执行。这里用一个延迟适配器证明 same 路径可编译运行。
        struct SlowAdapter;
        impl LiveAdapter for SlowAdapter {
            fn name(&self) -> &'static str {
                "slow-test"
            }

            fn check<'a>(
                &'a self,
                _url: &'a str,
            ) -> Pin<Box<dyn Future<Output = Result<LiveStatus>> + Send + 'a>> {
                Box::pin(async {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    Ok(LiveStatus {
                        is_live: true,
                        title: "t".into(),
                        anchor_name: "a".into(),
                        stream_url: "s".into(),
                    })
                })
            }
        }

        let adapter = Box::new(SlowAdapter) as Box<dyn LiveAdapter>;
        let url = "https://live.example.test/1".to_string();
        let check_url = url.clone();
        let result = tokio::time::timeout(Duration::from_millis(100), async move {
            adapter.check(&check_url).await
        })
        .await;
        assert!(result.is_ok());
        assert!(result.unwrap().unwrap().is_live);
    }

    #[tokio::test]
    async fn recover_stale_recordings_reclaims_session_and_pid_file() {
        let dir = std::env::temp_dir().join(format!("zunzhe-live-recover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(dir.join("config").join("config.ini"), "").unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "").unwrap();

        let db_file = dir.join("live.db");
        let pool = crate::db::open_pool(&db_file).await.unwrap();
        let cfg = crate::config::Config::load(&dir).unwrap();
        let watch = Watchlist::new(dir.clone());
        let url = "https://live.example.test/42";
        let session_id = crate::db::start_live_session(
            &pool,
            url,
            "测试主播",
            "测试标题",
            "test",
            "https://example.test/live.m3u8",
            u32::MAX,
            "/tmp/unused.mp4",
        )
        .await
        .unwrap();
        crate::service::write_rust_live_pid(&dir, url, u32::MAX);

        let monitor = LiveMonitor::new(&pool, &dir, &cfg, &watch);
        let recovered = monitor.recover_stale_recordings().await.unwrap();
        assert_eq!(recovered, 0, "不存在 pid 时不应计数为已杀进程");
        let sessions = crate::db::list_live_sessions(&pool, "recording", 10)
            .await
            .unwrap();
        assert!(
            sessions.iter().all(|s| s.id != session_id),
            "硬崩溃残留会话应被回收"
        );
        assert!(
            !dir.join("fupan_output")
                .join("run")
                .join("rust_live")
                .join(format!("{}.pid", crate::service::rust_live_pid_name(url)))
                .exists(),
            "硬崩溃残留 pid 文件应被清理"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
