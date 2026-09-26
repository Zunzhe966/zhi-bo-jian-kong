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
use crate::model::LiveSession;
use crate::service::{remove_rust_live_pid, remove_rust_live_pid_if, write_rust_live_pid};
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
            client: crate::net::client_builder()
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
            client: crate::net::client_builder()
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
                terminate_process(session.pid as u32).await;
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
                // 进程还活着 ≠ 还在录。断流后 ffmpeg 可能永久挂着：不回收它，
                // 这个主播之后每一场直播都会被「已在录制」挡在外面。
                match self.zombie_reason(&session) {
                    Some(reason) => self.recycle_session(&session, &reason).await,
                    None => {
                        info!("[{}] 已在录制 (session {})", item.remark, session.id);
                        return Ok(());
                    }
                }
            } else if session.pid > 0 {
                warn!("[{}] 检测到录制进程已退出，结束旧会话", item.remark);
                db::end_live_session(self.pool, session.id, "interrupted", "录制进程已退出")
                    .await?;
                remove_rust_live_pid(self.project_dir, &item.url);
            }
        }
        // 磁盘剩余空间低于配置阈值时不启动录制，避免把磁盘写满导致 ffmpeg
        // 崩溃/丢状态。旧 Python 有 `录制空间剩余阈值(gb)`，Rust 必须遵守。
        self.ensure_disk_space()?;
        // 目录主播名必须以监控源配置的名字为准（手册 R2：一级目录=主播名）。
        // 房间昵称是营销文案且主播随时会改（如「关立新（早上7:00直播）」），
        // 拿它当目录名会把一个主播的产物随改名裂成多棵树。
        let remark = item
            .remark
            .trim()
            .trim_start_matches("主播:")
            .trim_start_matches("主播：")
            .trim();
        let anchor = if !remark.is_empty() {
            remark.to_string()
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
                    terminate_process(pid).await;
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
                // 只删属于自己的 pid 文件：若这场已被回收、同一主播紧接着开了
                // 新录制，pid 文件里已是新 pid，不能抹掉。
                remove_rust_live_pid_if(&project_dir, &item_url, pid);
                return;
            }
            if code == 0 {
                info!("[{}] 录制正常结束", anchor_name);
                let _ = db::end_live_session(&pool, session_id, "done", "").await;
                remove_rust_live_pid_if(&project_dir, &item_url, pid);
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
                remove_rust_live_pid_if(&project_dir, &item_url, pid);
            }
        });
        Ok(())
    }

    /// 直播已结束但 DB 还有活动会话时，结束它。
    async fn finish_if_stopped(&self, url: &str) {
        if let Some(session) = db::active_live_session(self.pool, url).await.ok().flatten() {
            if session.pid > 0 && process_alive(session.pid as u32) {
                // 主播已下播但 ffmpeg 没退出：正常情况它自己会在几秒内收尾退出，
                // 这里不打断它。只有当它已经变成僵尸（超限 / 文件不再增长）时
                // 才强制回收，否则这个会话会一直挂在 recording 挡住下一场。
                if let Some(reason) = self.zombie_reason(&session) {
                    self.recycle_session(&session, &reason).await;
                }
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
                terminate_process(session.pid as u32).await;
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

    /// 录制输出路径：`{直播保存路径|产物根}/{主播}/直播/{日期} {场次}/{场次}.{ext}`。
    ///
    /// 新布局（产品手册第 3 章）：产物根下一级永远是主播名，不再按平台分目录；
    /// 转写产物随后落在同一目录（pipeline 的 out_dir = 录像所在目录），满足
    /// 「文字和视频在同一个文件夹」。旧 Python 的「保存文件夹是否以作者/时间/标题
    /// 区分」「保存文件名是否包含标题」四个开关随之作废——目录形态由手册固定，
    /// 不再可配。
    fn output_path(&self, _item: &LiveItem, status: &LiveStatus, anchor: &str) -> Result<PathBuf> {
        let root = crate::paths::storage(self.project_dir).recordings();
        let now = chrono::Local::now();
        let date = now.format("%Y-%m-%d").to_string();
        // 主播名/标题可能带 `/`、`:` 等文件系统非法字符，必须清洗后再拼路径。
        // 场次名取直播标题，标题为空时退到主播名。
        let title = status.title.trim();
        let base_session = if title.is_empty() {
            clean_name(anchor)
        } else {
            clean_name(title)
        };
        let anchor_dir = root.join(clean_name(anchor)).join("直播");
        // 同一天同标题复播（或前一场失败留了空目录）时给场次名加时刻后缀，
        // 保证场次目录互不覆盖——ffmpeg -y 会写穿已存在的同名文件。
        let mut session = base_session.clone();
        let mut dir = anchor_dir.join(format!("{date} {session}"));
        if dir.exists() {
            session = format!("{base_session} {}", now.format("%H-%M-%S"));
            dir = anchor_dir.join(format!("{date} {session}"));
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
        Ok(dir.join(format!("{session}.{ext}")))
    }

    /// 录制前检查项目所在磁盘剩余空间是否低于阈值（GB）。
    /// 阈值从 `config.ini [录制设置] 录制空间剩余阈值(gb)` 读取，默认 1.0 GB。
    fn ensure_disk_space(&self) -> Result<()> {
        let threshold_gb = self.disk_threshold_gb();
        let free_mb = free_disk_mb(self.project_dir);
        if free_mb == 0 || (free_mb as f64) < threshold_gb * 1024.0 {
            bail!(
                "磁盘剩余空间不足，跳过录制：剩余 {free_mb} MB，阈值 {} GB。\
                 请清理产物目录后重试",
                threshold_gb
            );
        }
        Ok(())
    }

    /// 单场录制最长时长（小时）。`0` / 空 / 非法 = 不限制。
    ///
    /// 护栏意义：ffmpeg 在直播流断流后会一直挂着不退出，状态机只要看到
    /// "进程还活着" 就认定「已在录制」，从此再也不会为这个主播开新录制——
    /// 一场卡死会永久堵死该主播后续所有直播。默认 12 小时足够覆盖超长直播，
    /// 超过后强制结束并立刻重开一场，录像不中断只是换文件。
    fn max_record_hours(&self) -> f64 {
        let raw = self
            .config
            .get_main("录制设置", "单场最长录制时长(小时)")
            .trim()
            .to_string();
        if raw.is_empty() {
            return 12.0;
        }
        raw.parse::<f64>().unwrap_or(12.0).max(0.0)
    }

    /// 录制文件停止增长多久后判定为假死（分钟）。`0` = 不做增长检查。
    fn stall_minutes(&self) -> u64 {
        let raw = self
            .config
            .get_main("录制设置", "录制无增长回收(分钟)")
            .trim()
            .to_string();
        if raw.is_empty() {
            return 30;
        }
        raw.parse::<u64>().unwrap_or(30)
    }

    /// 判断一个"进程仍在"的会话是否已经变成僵尸，返回 Some(原因) 表示要回收。
    ///
    /// 两条独立判据：
    /// 1. 时长超限：`started_at` 距今超过 `单场最长录制时长(小时)`；
    /// 2. 文件假死：输出文件（分段录制取最新一段）超过 `录制无增长回收(分钟)`
    ///    没有再变大。ffmpeg 卡在断流/重试时会一直挂着但一个字节都不写，
    ///    只靠"进程活着"永远发现不了。
    fn zombie_reason(&self, session: &LiveSession) -> Option<String> {
        let max_hours = self.max_record_hours();
        if max_hours > 0.0 {
            if let Some(started) = parse_local_datetime(&session.started_at) {
                let elapsed_min = (chrono::Local::now() - started).num_minutes();
                if elapsed_min >= (max_hours * 60.0) as i64 {
                    return Some(format!(
                        "单场录制已 {elapsed_min} 分钟，超过上限 {} 小时",
                        max_hours
                    ));
                }
            }
        }
        let stall = self.stall_minutes();
        if stall > 0 && !session.file_path.is_empty() {
            let candidate = find_latest_segment(&session.file_path)
                .unwrap_or_else(|| session.file_path.clone());
            match std::fs::metadata(&candidate) {
                Ok(meta) => match meta.modified() {
                    Ok(mtime) => {
                        let age_min = chrono::DateTime::<chrono::Local>::from(mtime);
                        let idle = (chrono::Local::now() - age_min).num_minutes();
                        if idle >= stall as i64 {
                            return Some(format!(
                                "录制文件已 {idle} 分钟没有增长，判定为假死（{}）",
                                candidate
                            ));
                        }
                    }
                    Err(_) => {}
                },
                Err(_) => {
                    // 输出文件根本不存在：ffmpeg 还挂着却什么都没写出来，
                    // 同样按假死处理，否则这个主播会被永久堵住。
                    if let Some(started) = parse_local_datetime(&session.started_at) {
                        let idle = (chrono::Local::now() - started).num_minutes();
                        if idle >= stall as i64 {
                            return Some(format!(
                                "录制已启动 {idle} 分钟但输出文件不存在，判定为假死（{}）",
                                candidate
                            ));
                        }
                    }
                }
            }
        }
        None
    }

    /// 强制回收一个会话：终止进程组、结束会话、清理 pid 文件。
    async fn recycle_session(&self, session: &LiveSession, reason: &str) {
        warn!("[{}] 回收僵尸录制会话: {reason}", session.anchor);
        if session.pid > 0 && process_alive(session.pid as u32) {
            terminate_process(session.pid as u32).await;
        }
        let _ = db::end_live_session(self.pool, session.id, "interrupted", reason).await;
        remove_rust_live_pid_if(self.project_dir, &session.url, session.pid as u32);
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
        let ext = output
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("ts")
            .to_lowercase();
        // 纯音频格式不能先写通用的 `-map 0`：ffmpeg 的 -map 是累加，视频轨会被
        // 一起选进 MP3/M4A 容器，写头即失败（实测 exit=234「Invalid audio stream」
        // + 0 字节产物；去掉 -map 0 后正常写出 82693 字节）。
        let audio_only = matches!(ext.as_str(), "mp3" | "m4a");
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
            // 布尔 AVOption 必须显式给值：裸写 -reconnect_streamed 会让 ffmpeg 把
            // 下一个参数（-reconnect_at_eof）当成它的值，解析失败报
            // 「Error opening input files: Invalid argument」——2026-09-26 实测
            // 复现，此 bug 导致本机全部直播录制从未真正写入一个字节。
            "-reconnect_streamed".to_string(),
            "1".to_string(),
            "-reconnect_at_eof".to_string(),
            "1".to_string(),
            "-i".to_string(),
            stream_url.to_string(),
        ];
        if audio_only {
            args.extend([
                "-map".to_string(),
                "0:a".to_string(),
                "-vn".to_string(),
                "-c:a".to_string(),
                if ext == "mp3" { "libmp3lame".to_string() } else { "aac".to_string() },
                "-ab".to_string(),
                "320k".to_string(),
            ]);
        } else {
            args.extend([
                "-map".to_string(),
                "0".to_string(),
                "-c:v".to_string(),
                "copy".to_string(),
                "-c:a".to_string(),
                "copy".to_string(),
            ]);
        }
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
                // mp3/m4a 的轨道选择与编码器已在上面按 audio_only 处理完，
                // 这里不再追加 -map（否则又把视频轨拉回来，写头失败）。
                "mp3" | "m4a" => {}
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
    // 修改时间可能因文件系统精度相同；用 mtime 主键、文件名序号稳定排序，
    // 避免同批次分段在 Linux 等平台上随机选到旧文件。
    candidates.sort_by(|a, b| {
        let time = a
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .cmp(&b.metadata().and_then(|m| m.modified()).ok());
        time.then_with(|| a.file_name().cmp(&b.file_name()))
    });
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

    #[tokio::test]
    async fn wait_child_keeps_consuming_after_capture_limit() {
        // 回归隐患：旧实现只读到最后 512KB 就停止读取，子进程继续写 stderr
        // 时管道会写满并阻塞，录制永远不会退出，状态也无法回收。修复后必须
        // 持续消费管道、只保留最近 512KB，不能因为日志量大而卡住等待。
        let mut child = Command::new("sh")
            .args(["-c", "for i in $(seq 1 200000); do printf 'x'; done"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (code, log) = tokio::time::timeout(Duration::from_secs(10), wait_child(&mut child))
            .await
            .expect("日志超过 512KB 后 wait_child 不应卡死");
        assert_eq!(code, 0);
        assert!(
            log.chars().count() <= 512 * 1024 + 64,
            "内存中只应保留最近日志: {}",
            log.chars().count()
        );
    }
}

/// 单路子进程输出流的“持续消费 + 环形保留”读取器。
///
/// 记录最近 `limit` 字节（保留到行边界），但一直把管道读到 EOF，防止日志
/// 超过上限后写满管道、把 ffmpeg 卡住；长时间录制也不会无限占内存。
async fn drain_capture_limited(reader: impl tokio::io::AsyncRead + Unpin, limit: usize) -> String {
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    let mut captured: Vec<u8> = Vec::new();
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        captured.extend_from_slice(&line);
        if captured.len() > limit {
            let excess = captured.len() - limit;
            captured.drain(0..excess);
            // 尽量从行首开始保留，避免诊断日志中间被截断成半行。
            if let Some(pos) = captured.iter().position(|b| *b == b'\n') {
                captured.drain(0..pos + 1);
            }
        }
    }
    String::from_utf8_lossy(&captured).into_owned()
}

async fn wait_child(child: &mut Child) -> (i32, String) {
    // ffmpeg 的日志几乎全部写在 stderr。串行“先读完 stdout 再读 stderr”
    // 会在 stderr 管道写满后互相等待：ffmpeg 阻塞在 stderr 写、stdout 又
    // 迟迟不到 EOF，录制收尾任务就会一直卡住。这里并发读取两路管道，
    // 并各自保留最后 512KB，避免长时间录制把内存无限放大。
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let drain_stdout = async move {
        match stdout_pipe {
            Some(out) => drain_capture_limited(out, 512 * 1024).await,
            None => String::new(),
        }
    };
    let drain_stderr = async move {
        match stderr_pipe {
            Some(err) => drain_capture_limited(err, 512 * 1024).await,
            None => String::new(),
        }
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

/// 先发 SIGTERM 让 ffmpeg 优雅收尾，等 500ms 若仍存活再 SIGKILL。
///
/// 注意：本函数从 async 上下文调用，必须用 `tokio::time::sleep` 而非
/// `std::thread::sleep` —— 后者会阻塞整个 tokio worker 线程，在回收多个
/// 残留会话时每个阻塞 500ms，直接吃掉直播检测的 25s 预算。这是实测
/// 「直播检测超时」的加重因素。
async fn terminate_process(pid: u32) {
    #[cfg(unix)]
    {
        if pid == 0 || pid > i32::MAX as u32 {
            return;
        }
        unsafe {
            let _ = libc::kill(-(pid as i32), libc::SIGTERM);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
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

/// 解析 SQLite `datetime('now','localtime')` 写出的 `"YYYY-MM-DD HH:MM:SS"`。
///
/// 本机时区下的会话时间没有时区后缀，不能用 RFC3339 解析；解析失败返回 None，
/// 调用方按"时间未知"处理（不做时长判定），绝不因为时间解析问题误杀录制。
fn parse_local_datetime(text: &str) -> Option<chrono::DateTime<chrono::Local>> {
    use chrono::TimeZone;
    let text = text.trim();
    let (date, time) = text.split_once(' ')?;
    let mut d = date.split('-');
    let (y, m, day) = (
        d.next()?.parse::<i32>().ok()?,
        d.next()?.parse::<u32>().ok()?,
        d.next()?.parse::<u32>().ok()?,
    );
    let mut t = time.split(':');
    let (h, mi, s) = (
        t.next()?.parse::<u32>().ok()?,
        t.next()?.parse::<u32>().ok()?,
        t.next()?.parse::<u32>().ok()?,
    );
    chrono::Local
        .with_ymd_and_hms(y, m, day, h, mi, s)
        .single()
        .map(|dt| dt.with_timezone(&chrono::Local))
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
        // -t 必须插在 -i 之前（输入侧限时）：追加在输出文件之后对直播流
        // 不生效，会一路录到关播（2026-09-26 实测 12 秒参数录了 10 分钟未停）。
        if let Some(i) = args.iter().position(|a| a == "-i") {
            args.splice(i..i, ["-t".into(), duration_secs.to_string()]);
        } else {
            args.extend(["-t".into(), duration_secs.to_string()]);
        }
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
            terminate_process(pid).await;
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

    /// 造一个带自定义 [录制设置] 的临时项目目录，返回 (目录, monitor)。
    async fn monitor_with_config(name: &str, ini: &str) -> (std::path::PathBuf, Config, Watchlist) {
        let dir = std::env::temp_dir().join(format!("zunzhe-zombie-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(dir.join("config").join("config.ini"), ini).unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "").unwrap();
        let cfg = Config::load(&dir).unwrap();
        let watch = Watchlist::new(dir.clone());
        (dir, cfg, watch)
    }

    fn session_at(started_minutes_ago: i64, file_path: &str) -> LiveSession {
        let started = chrono::Local::now() - chrono::Duration::minutes(started_minutes_ago);
        LiveSession {
            id: 1,
            url: "https://live.example.test/1".into(),
            anchor: "测试主播".into(),
            title: "标题".into(),
            platform: "test".into(),
            stream_url: String::new(),
            status: "recording".into(),
            pid: 4242,
            file_path: file_path.into(),
            started_at: started.format("%Y-%m-%d %H:%M:%S").to_string(),
            ended_at: String::new(),
            error: String::new(),
            updated_at: String::new(),
        }
    }

    /// 把文件 mtime 改成 N 分钟前（模拟 ffmpeg 卡住不再写数据）。
    fn age_file(path: &Path, minutes: i64) {
        use std::fs::FileTimes;
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs((minutes * 60) as u64);
        f.set_times(FileTimes::new().set_modified(old)).unwrap();
    }

    #[test]
    fn parse_local_datetime_reads_sqlite_local_format() {
        let dt = parse_local_datetime("2026-09-24 09:29:04").expect("应解析 SQLite localtime 文本");
        assert_eq!(dt.format("%Y-%m-%d %H:%M:%S").to_string(), "2026-09-24 09:29:04");
        assert!(parse_local_datetime("").is_none());
        assert!(parse_local_datetime("not-a-time").is_none());
        // RFC3339（带时区后缀）不该被误解析成本地时间文本
        assert!(parse_local_datetime("2026-09-24T09:29:04Z").is_none());
    }

    #[tokio::test]
    async fn zombie_reason_flags_overlong_session() {
        // 回归锁：ffmpeg 断流后永久挂着，状态机只认「进程还活着」→
        // 这个主播之后每一场直播都被「已在录制」挡住，永久录不上。
        let (dir, cfg, watch) = monitor_with_config("overlong", "[录制设置]\n单场最长录制时长(小时) = 2\n").await;
        let pool = crate::db::open_pool(&dir.join("z.db")).await.unwrap();
        let monitor = LiveMonitor::new(&pool, &dir, &cfg, &watch);
        let file = dir.join("live.ts");
        std::fs::write(&file, "x").unwrap();
        let reason = monitor.zombie_reason(&session_at(300, &file.display().to_string()));
        assert!(reason.is_some(), "5 小时的会话应超过 2 小时上限");
        assert!(reason.unwrap().contains("超过上限"));

        let reason = monitor.zombie_reason(&session_at(10, &file.display().to_string()));
        assert!(reason.is_none(), "10 分钟、文件刚写的会话不该被判僵尸");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn zombie_reason_flags_stalled_file() {
        let (dir, cfg, watch) =
            monitor_with_config("stall", "[录制设置]\n单场最长录制时长(小时) = 12\n录制无增长回收(分钟) = 30\n").await;
        let pool = crate::db::open_pool(&dir.join("z.db")).await.unwrap();
        let monitor = LiveMonitor::new(&pool, &dir, &cfg, &watch);
        let file = dir.join("live.ts");
        std::fs::write(&file, "x").unwrap();
        age_file(&file, 45);
        // 只跑了 5 分钟（没超时），但文件 45 分钟没动静 → 假死
        let reason = monitor.zombie_reason(&session_at(5, &file.display().to_string()));
        assert!(reason.is_some(), "文件 45 分钟没增长应判假死");
        assert!(reason.unwrap().contains("没有增长"));

        let missing = dir.join("不存在.ts");
        let reason = monitor.zombie_reason(&session_at(45, &missing.display().to_string()));
        assert!(reason.is_some(), "启动 45 分钟却没产出文件应判假死");

        // 时长上限设为 0 = 不限制，只剩增长判据
        let (dir2, cfg2, watch2) =
            monitor_with_config("nolimit", "[录制设置]\n单场最长录制时长(小时) = 0\n录制无增长回收(分钟) = 30\n").await;
        let pool2 = crate::db::open_pool(&dir2.join("z.db")).await.unwrap();
        let monitor2 = LiveMonitor::new(&pool2, &dir2, &cfg2, &watch2);
        let file2 = dir2.join("live.ts");
        std::fs::write(&file2, "x").unwrap();
        assert!(
            monitor2
                .zombie_reason(&session_at(600, &file2.display().to_string()))
                .is_none(),
            "上限为 0 时不按时长杀录制"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn remove_pid_file_only_when_it_belongs_to_that_pid() {
        let dir = std::env::temp_dir().join(format!("zunzhe-pidkeep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        // 必须显式把产物根指到临时目录：留空时 paths::resolve 会退回
        // `~/Documents/尊者产物`，单测就会往用户真实数据目录里写 pid 文件。
        std::fs::write(
            dir.join("config").join("config.ini"),
            format!("[路径设置]\n产物根目录 = {}", dir.display()),
        )
        .unwrap();
        let url = "https://live.example.test/9";
        crate::service::write_rust_live_pid(&dir, url, 111);
        // 新一场录制已经写了自己的 pid；旧会话的回收任务晚到一步也不能删它
        crate::service::write_rust_live_pid(&dir, url, 222);
        crate::service::remove_rust_live_pid_if(&dir, url, 111);
        let path = crate::paths::run_dir(&dir)
            .join("rust_live")
            .join(format!("{}.pid", crate::service::rust_live_pid_name(url)));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            "222",
            "旧会话不得抹掉新会话的 pid 文件"
        );
        crate::service::remove_rust_live_pid_if(&dir, url, 222);
        assert!(!path.exists(), "属于自己的 pid 文件应当被清理");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn ffmpeg_audio_only_formats_drop_video_stream() {
        // 回归锁：mp3/m4a 出现 `-map 0`（全轨道）会把视频轨塞进音频容器，
        // 实测写头失败 exit=234、0 字节产物。
        let dir = std::env::temp_dir().join(format!("zunzhe-ffmpeg-audio-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(dir.join("config").join("config.ini"), "").unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "").unwrap();
        let pool = crate::db::open_pool(&dir.join("audio.db")).await.unwrap();
        let cfg = crate::config::Config::load(&dir).unwrap();
        let watch = Watchlist::new(dir.clone());
        let monitor = LiveMonitor::new(&pool, &dir, &cfg, &watch);
        for ext in ["mp3", "m4a"] {
            let args = monitor.ffmpeg_args("http://e.com/s.m3u8", Path::new(&format!("/tmp/o.{ext}")));
            assert!(!args.windows(2).any(|w| w[0] == "-map" && w[1] == "0"),
                "{ext} 不得出现 -map 0（会把视频轨选进音频容器）: {args:?}");
            assert!(args.iter().any(|a| a == "-vn"), "{ext} 必须有 -vn: {args:?}");
            assert!(args.iter().any(|a| a == "0:a"), "{ext} 必须选 0:a: {args:?}");
        }
        // 视频格式保持 -map 0（不能被上面的改动误伤）
        let ts = monitor.ffmpeg_args("http://e.com/s.m3u8", Path::new("/tmp/o.ts"));
        assert!(ts.windows(2).any(|w| w[0] == "-map" && w[1] == "0"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn ffmpeg_boolean_options_carry_explicit_values() {
        // 回归锁：-reconnect_streamed / -reconnect_at_eof 裸写会被 ffmpeg 把下一个
        // 参数当值解析，报「Invalid argument」导致全部直播录制从未成功（2026-09-26 实测）。
        let dir = std::env::temp_dir().join(format!("zunzhe-ffmpeg-args-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(dir.join("config").join("config.ini"), "").unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "").unwrap();
        let pool = crate::db::open_pool(&dir.join("args.db")).await.unwrap();
        let cfg = crate::config::Config::load(&dir).unwrap();
        let watch = Watchlist::new(dir.clone());
        let monitor = LiveMonitor::new(&pool, &dir, &cfg, &watch);
        let args = monitor.ffmpeg_args("http://example.com/s.m3u8", Path::new("/tmp/o.ts"));
        for opt in ["-reconnect_streamed", "-reconnect_at_eof", "-reconnect"] {
            if let Some(i) = args.iter().position(|a| a == opt) {
                let next = args.get(i + 1).map(|s| s.as_str()).unwrap_or("");
                assert!(
                    next == "1" || next == "0",
                    "{opt} 必须跟显式布尔值，实际后跟: {next:?}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
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
    async fn output_path_follows_manual_layout_and_cleans_names() {
        // 新布局（产品手册第 3 章）：{产物根}/{主播}/直播/{日期} {场次}/{场次}.ts，
        // 不按平台分目录，主播名/标题清洗非法字符。
        let dir = std::env::temp_dir().join(format!("zunzhe-live-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        let media = dir.join("产物");
        std::fs::write(
            dir.join("config").join("config.ini"),
            format!("[路径设置]\n产物根目录 = {}\n", media.display()),
        )
        .unwrap();
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
        let date = chrono::Local::now().format("%Y-%m-%d").to_string();
        let expect_dir = media
            .join("主播_甲")
            .join("直播")
            .join(format!("{date} 测试_标题_一"));
        assert_eq!(
            out,
            expect_dir.join("测试_标题_一.ts"),
            "录制路径必须按手册布局落盘"
        );
        let filename = out.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            !filename.contains(['/', '\\', ':']),
            "文件名包含非法字符: {filename}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn output_path_suffixes_session_when_folder_exists() {
        // 同一天同标题复播：场次目录已存在时加时刻后缀，绝不覆盖已有录制。
        let dir = std::env::temp_dir().join(format!("zunzhe-live-path2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        let media = dir.join("产物");
        std::fs::write(
            dir.join("config").join("config.ini"),
            format!("[路径设置]\n产物根目录 = {}\n", media.display()),
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
            title: "晚间闲聊".to_string(),
            anchor_name: "主播甲".to_string(),
            stream_url: "https://example.test/live.m3u8".to_string(),
        };
        let date = chrono::Local::now().format("%Y-%m-%d").to_string();
        std::fs::create_dir_all(
            media
                .join("主播甲")
                .join("直播")
                .join(format!("{date} 晚间闲聊")),
        )
        .unwrap();
        let out = monitor.output_path(&item, &status, "主播甲").unwrap();
        let text = out.display().to_string();
        assert!(
            text.contains(&format!("{date} 晚间闲聊 ")),
            "场次目录已存在时应追加时刻后缀: {text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn last_lines_keeps_tail() {
        let log = "a\nb\nc\nd";
        assert!(last_log_lines(log, 3).contains('d'));
    }

    #[tokio::test]
    async fn terminate_process_does_not_panic() {
        // 不存在的 pid 调用不应 panic；真实清理按 DB 会话驱动。
        terminate_process(u32::MAX).await;
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

    #[test]
    fn resolve_live_for_project_routes_each_platform() {
        // 回归保障：URL→适配器的分发是直播监控的地基，一旦被误改，
        // B站会落到 Python 桥、或抖音/快手被当成直链，导致整链解析失效。
        // 这里用适配器 name() 固化路由结果，无需联网。
        let cases = [
            // 抖音：主站 + 短链都走 Python 桥
            ("https://live.douyin.com/Glx6768", "python-bridge"),
            ("https://v.douyin.com/abc123/", "python-bridge"),
            // 快手
            ("https://live.kuaishou.com/u/Bd20210915", "python-bridge"),
            // 虎牙 / 斗鱼
            ("https://www.huya.com/10086", "python-bridge"),
            ("https://www.douyu.com/999", "python-bridge"),
            // YouTube 主站 + 短链
            ("https://www.youtube.com/watch?v=abc", "python-bridge"),
            ("https://youtu.be/abc", "python-bridge"),
            // Twitch
            ("https://www.twitch.tv/someone", "python-bridge"),
            // B站：原生适配器，不走 Python 桥
            ("https://live.bilibili.com/24931109", "bilibili"),
            // 其它未知域名/直链 m3u8：直链适配器
            ("https://example.test/live.m3u8", "direct"),
            ("https://cdn.example.com/hls/stream/index.m3u8", "direct"),
        ];
        for (url, expect) in cases {
            let adapter = resolve_live_for_project(url, Path::new("."), "");
            assert_eq!(adapter.name(), expect, "URL {url} 应路由到 {expect} 适配器");
        }
    }
}
