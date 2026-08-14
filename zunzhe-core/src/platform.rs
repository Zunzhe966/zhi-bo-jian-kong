use anyhow::{Context, Result, anyhow};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tracing::warn;

use crate::bilibili_auth::{
    browser_cookie_path, cookie_header_from_netscape_public, ensure_anon_cookie,
    ensure_user_cookie_file, read_browser_cookie_file,
};
use crate::model::VideoMeta;
use crate::process::ManagedChild;

/// B 站 WBI 固定置换表（公开算法，非密钥）。
const MIXIN_KEY_TAB: [usize; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

fn mixin_key(img_key: &str, sub_key: &str) -> String {
    let raw = format!("{img_key}{sub_key}");
    let chars: Vec<char> = raw.chars().collect();
    MIXIN_KEY_TAB
        .iter()
        .map(|&i| chars.get(i).copied().unwrap_or('a'))
        .collect()
}

fn md5_hex(text: &str) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(text.as_bytes());
    let bytes = hasher.finalize();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 汇总三个兜底源的空结果错误，生成给用户的顶层错误说明。
///
/// 24×30 审计发现：B站源网络/DNS 故障时，旧实现把底层原因吞成“空列表”，
/// 顶层永远归因为“412 风控，请配置 cookie”，误导用户去配 Cookie 而不是
/// 检查网络。这里把每级原因带上来，风控与网络故障分别给出可操作提示。
fn upload_list_failure_message(official: &str, rsshub: &str, ytdlp: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if !official.is_empty() {
        parts.push(official);
    }
    if !rsshub.is_empty() {
        parts.push(rsshub);
    }
    if !ytdlp.is_empty() {
        parts.push(ytdlp);
    }
    let detail = if parts.is_empty() {
        "三个兜底均未返回数据".to_string()
    } else {
        parts.join("；")
    };
    // 网络故障优先判断：即使 detail 里同时带“签名失败/请求失败”和
    // “解析失败/超时”，根本原因也是网络，不能误导成配置 cookie。
    let hint = if detail.contains("解析失败")
        || detail.contains("请求失败")
        || detail.contains("DNS")
        || detail.contains("timed out")
        || detail.contains("Timeout")
        || detail.contains("超时")
    {
        "若为网络/DNS 故障请检查网络后重试".to_string()
    } else if detail.contains("412")
        || detail.contains("风控")
        || detail.contains("403")
        || detail.contains("wbi")
        || detail.contains("签名")
    {
        "若为 412 风控请配置 B站 cookie".to_string()
    } else {
        "可稍后重试或检查 B站 cookie".to_string()
    };
    format!("B站投稿列表拉取失败: {detail}（{hint}）")
}

/// 用 B 站 WBI 公钥和投稿列表参数生成签名查询串。
///
/// 纯函数便于单测；排序、拼参、`w_rid` 均按官方 WBI 规则执行。
fn build_wbi_query(img_key: &str, sub_key: &str, mid: &str, count: u32, wts: u64) -> String {
    let mut params = [
        ("mid".to_string(), mid.to_string()),
        ("ps".to_string(), count.to_string()),
        ("pn".to_string(), "1".to_string()),
        ("order".to_string(), "pubdate".to_string()),
        ("platform".to_string(), "web".to_string()),
        ("web_location".to_string(), "1550101".to_string()),
        ("wts".to_string(), wts.to_string()),
    ];
    params.sort();
    let query = params
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    let mixin = mixin_key(img_key, sub_key);
    let digest = md5_hex(&format!("{query}{mixin}"));
    format!("{query}&w_rid={digest}")
}

pub trait Platform: Send + Sync {
    fn name(&self) -> &'static str;

    /// 从博主主页/订阅地址列出已发布视频。
    fn list_videos<'a>(
        &'a self,
        url: &'a str,
        count: u32,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Vec<VideoMeta>>> + Send + 'a>>;
}

/// B站适配器：先解析 space.bilibili.com/<mid>，用 view 接口补全
/// 标题/封面/日期/时长；投稿 ID 走官方 WBI 接口，RSSHub/yt-dlp 兜底。
#[derive(Debug)]
pub struct BilibiliAdapter {
    client: reqwest::Client,
    cookie: String,
    project_dir: PathBuf,
}

impl Default for BilibiliAdapter {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .unwrap_or_default(),
            cookie: String::new(),
            project_dir: PathBuf::from("."),
        }
    }
}

impl BilibiliAdapter {
    /// 请求一次 B站官方投稿接口并解析 BV 列表；返回空表示需要上层换下一组兜底。
    async fn request_upload_ids(
        &self,
        signed_query: &str,
        headers: &reqwest::header::HeaderMap,
        count: u32,
    ) -> Vec<String> {
        let signed = format!("https://api.bilibili.com/x/space/wbi/arc/search?{signed_query}");
        let Ok(resp) = self
            .client
            .get(&signed)
            .headers(headers.clone())
            .send()
            .await
        else {
            return Vec::new();
        };
        // HTTP 层必须是成功状态，B站 412/5xx 时继续走 RSSHub/yt-dlp，
        // 不能让 HTML 或错误 JSON 被当成“空列表”。
        if !resp.status().is_success() {
            return Vec::new();
        }
        let Ok(data) = resp.json::<serde_json::Value>().await else {
            return Vec::new();
        };
        parse_official_upload_response(&data, count)
    }

    /// 取 B 站 wbi 图片并生成查询签名（w_rid/wts）。失败返回空串，调用方走无签名兜底。
    async fn wbi_query(&self, mid: &str, count: u32) -> anyhow::Result<String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        // nav 是 WBI 图片 key 的来源；有配置/浏览器 Cookie 时也必须一并带上，
        // 否则即使用户已授权，这一步仍可能被 B站当成匿名出口并 412，导致
        // 官方投稿接口永远回退到 RSSHub/yt-dlp。
        let cookie = self.effective_cookie().await;
        let mut req = client
            .get("https://api.bilibili.com/x/web-interface/nav")
            .header(
                "User-Agent",
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36",
            )
            .header("Referer", "https://www.bilibili.com");
        if !cookie.is_empty()
            && let Ok(v) = reqwest::header::HeaderValue::from_str(&cookie)
        {
            req = req.header(reqwest::header::COOKIE, v);
        }
        let nav = req.send().await?.json::<serde_json::Value>().await?;
        let img_url = nav["data"]["wbi_img"]["img_url"].as_str().unwrap_or("");
        let sub_url = nav["data"]["wbi_img"]["sub_url"].as_str().unwrap_or("");
        if img_url.is_empty() || sub_url.is_empty() {
            anyhow::bail!("wbi nav 缺少 img_url");
        }
        let img_key = img_url.rsplit('/').next().unwrap_or("").replace(".png", "");
        let sub_key = sub_url.rsplit('/').next().unwrap_or("").replace(".png", "");
        if img_key.is_empty() || sub_key.is_empty() {
            anyhow::bail!("wbi key 为空");
        }
        let wts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Ok(build_wbi_query(&img_key, &sub_key, mid, count, wts))
    }

    pub fn with_project_dir(project_dir: PathBuf) -> Self {
        Self {
            project_dir,
            ..Self::default()
        }
    }

    /// 从桌面端「B站授权」导出的 Netscape cookie 文件读取 Cookie 字符串。
    ///
    /// 读取失败（如文件存在但无权限或损坏）时返回错误，而不是静默假装没有 Cookie，
    /// 让真实授权状态和程序缺陷不会混在一起。
    fn browser_cookie_from_project(&self) -> String {
        match read_browser_cookie_file(&self.project_dir) {
            Ok(Some(text)) => cookie_header_from_netscape_public(&text),
            Ok(None) => String::new(),
            Err(e) => {
                tracing::warn!("读取 B站授权 Cookie 失败: {e:#}");
                String::new()
            }
        }
    }

    /// 用户配置 Cookie 写入旧 Python 同路径的 Netscape 文件，并转成请求头。
    ///
    /// 返回空串表示没有可用的用户 Cookie；调用方应继续使用浏览器导出或匿名兜底。
    fn configured_cookie_header(&self) -> String {
        let Some(cookie) = crate::config::Config::load(&self.project_dir)
            .ok()
            .map(|c| c.get_fupan("视频监控", "b站cookie"))
        else {
            return String::new();
        };
        let cookie = cookie.trim();
        if cookie.is_empty() {
            return String::new();
        }
        let Some(path) = ensure_user_cookie_file(&self.project_dir, cookie) else {
            return cookie.to_string();
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return cookie.to_string();
        };
        let header = cookie_header_from_netscape_public(&text);
        if header.is_empty() {
            cookie.to_string()
        } else {
            header
        }
    }

    /// 用户配置 Cookie 的 Netscape 文件（优先返回旧 Python 同路径文件）。
    ///
    /// 只有文件不存在时才现场生成；文件已存在时优先复用，避免写覆盖旧 Python 产物。
    fn user_cookie_file(&self) -> Option<PathBuf> {
        let cookie = crate::config::Config::load(&self.project_dir)
            .ok()
            .map(|c| c.get_fupan("视频监控", "b站cookie"))
            .unwrap_or_default();
        let cookie = cookie.trim();
        if cookie.is_empty() {
            return None;
        }
        let path = crate::bilibili_auth::user_cookie_path(&self.project_dir);
        if path.exists() {
            return Some(path);
        }
        ensure_user_cookie_file(&self.project_dir, cookie)
    }

    /// 投稿列表专用 Cookie：旧 Python 明确不使用匿名 buvid，只允许账号 Cookie，
    /// 否则会更容易触发 412。这里遵循同一优先级。
    fn upload_list_cookie(&self) -> String {
        let configured = self.configured_cookie_header();
        if !configured.is_empty() {
            return configured;
        }
        let browser = self.browser_cookie_from_project();
        if !browser.is_empty() {
            return browser;
        }
        // 没有账号 Cookie 时，桌面端导出的浏览器 Cookie 里通常至少会有
        // bili_ticket/buvid 等风控校验字段；这里补一次同步读取，避免上面
        // `browser_cookie_from_project` 的日志路径把可恢复文件当成空状态。
        match read_browser_cookie_file(&self.project_dir) {
            Ok(Some(text)) => {
                let header = cookie_header_from_netscape_public(&text);
                if header.is_empty() {
                    tracing::warn!("B站授权 Cookie 文件存在但没有可导出字段");
                }
                header
            }
            Ok(None) => String::new(),
            Err(e) => {
                tracing::warn!("读取 B站授权 Cookie 失败: {e:#}");
                String::new()
            }
        }
    }

    pub fn with_cookie_and_project_dir(cookie: impl Into<String>, project_dir: PathBuf) -> Self {
        Self {
            cookie: cookie.into(),
            project_dir,
            ..Self::default()
        }
    }

    async fn effective_cookie(&self) -> String {
        if !self.cookie.is_empty() {
            self.cookie.clone()
        } else if let Some(cookie) = self.user_cookie_file() {
            let Ok(text) = std::fs::read_to_string(cookie) else {
                return self.browser_cookie_from_project();
            };
            cookie_header_from_netscape_public(&text)
        } else if let Some(anon) = ensure_anon_cookie(&self.project_dir).await {
            let Ok(text) = std::fs::read_to_string(anon) else {
                return self.browser_cookie_from_project();
            };
            cookie_header_from_netscape_public(&text)
        } else {
            self.browser_cookie_from_project()
        }
    }

    fn mid_from_url(url: &str) -> Option<String> {
        let re = regex::Regex::new(r"space\.bilibili\.com/(\d+)").ok()?;
        re.captures(url)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
    }

    async fn view(&self, bvid: &str) -> Option<VideoMeta> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::REFERER,
            reqwest::header::HeaderValue::from_static("https://www.bilibili.com"),
        );
        let cookie = self.effective_cookie().await;
        if !cookie.is_empty()
            && let Ok(v) = reqwest::header::HeaderValue::from_str(&cookie)
        {
            headers.insert(reqwest::header::COOKIE, v);
        }
        let url = format!("https://api.bilibili.com/x/web-interface/view?bvid={bvid}");
        let resp = match self.client.get(&url).headers(headers).send().await {
            Ok(r) => r,
            Err(_) => return None,
        };
        let resp = match resp.error_for_status() {
            Ok(r) => r,
            Err(_) => return None,
        };
        let data: serde_json::Value = match resp.json().await {
            Ok(d) => d,
            Err(_) => return None,
        };
        if data.get("code").and_then(|c| c.as_i64()) != Some(0) {
            return None;
        }
        let d = &data["data"];
        if d.get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .is_empty()
        {
            return None;
        }
        let ts = d["pubdate"].as_i64().unwrap_or(0);
        Some(VideoMeta {
            id: bvid.to_string(),
            platform: "bilibili".to_string(),
            url: format!("https://www.bilibili.com/video/{bvid}"),
            title: d["title"].as_str().unwrap_or(bvid).to_string(),
            cover: d["pic"]
                .as_str()
                .unwrap_or("")
                .replace("http://", "https://"),
            published_at: if ts > 0 {
                unix_to_date(ts)
            } else {
                String::new()
            },
            duration: d["duration"].as_i64().unwrap_or(0),
        })
    }
}

impl Platform for BilibiliAdapter {
    fn name(&self) -> &'static str {
        "bilibili"
    }

    fn list_videos<'a>(
        &'a self,
        url: &'a str,
        count: u32,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Vec<VideoMeta>>> + Send + 'a>> {
        Box::pin(async move {
            let mid =
                Self::mid_from_url(url).with_context(|| format!("不是 B站空间地址: {url}"))?;
            // 官方 wbi/arc 接口优先（带 cookie），再 RSSHub，最后 yt-dlp；
            // Electron 页面提取从调度轮次移除，因为独立 Electron 冷启动
            // 需要 20-40 秒，会被 50 秒单源护栏截断；手动 blogger-videos
            // 场景仍可使用（由调用方决定是否补充）。
            // 三个兜底都只返回“空列表”，但空的原因可能是 412 风控，也可能是
            // 网络/DNS 故障。把每级的真实原因带上来，顶层报错才不至于把
            // “api.bilibili.com 解析失败”误导成“请配置 cookie”。
            let (mut bvids, official_err) = self.upload_ids_official(&mid, count).await;
            let mut rsshub_err = String::new();
            let mut ytdlp_err = String::new();
            tracing::warn!("B站官方投稿接口返回 {} 条 ({official_err})", bvids.len());
            if bvids.is_empty() {
                let (next, err) = self.upload_ids_rsshub(&mid, count).await;
                bvids = next;
                rsshub_err = err.clone();
                tracing::warn!("B站 RSSHub 投稿列表返回 {} 条 ({err})", bvids.len());
            }
            if bvids.is_empty() {
                let (next, err) = self.upload_ids_ytdlp(&mid, count).await;
                bvids = next;
                ytdlp_err = err.clone();
                tracing::warn!("B站 yt-dlp 投稿列表返回 {} 条 ({err})", bvids.len());
            }
            if bvids.is_empty() {
                return Err(anyhow!(upload_list_failure_message(
                    &official_err,
                    &rsshub_err,
                    &ytdlp_err
                )));
            }
            // view 补全并发执行：B站接口单次 1-3 秒，10 条串行会拖慢监控轮次。
            let results =
                futures_util::future::join_all(bvids.iter().map(|bvid| self.view(bvid))).await;
            // 占位/补全失败的结果不能进入历史视频库，否则会伪装成“成功但空”。
            let out: Vec<VideoMeta> = results.into_iter().flatten().collect();
            if out.is_empty() {
                return Err(anyhow!("B站视频信息补全失败，请配置 B站 cookie 或稍后重试"));
            }
            Ok(out)
        })
    }
}

impl BilibiliAdapter {
    /// 返回 `(bv_ids, 失败原因)`。失败原因用于顶层区分风控与网络故障，
    /// 空串表示“无具体原因可描述”。
    async fn upload_ids_official(&self, mid: &str, count: u32) -> (Vec<String>, String) {
        // 旧 Python 的投稿列表接口明确不使用匿名 buvid：那会更容易触发 412。
        // 这里只使用用户配置或浏览器授权 Cookie，不补匿名设备 Cookie。
        let cookie = self.upload_list_cookie();
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::REFERER,
            reqwest::header::HeaderValue::from_str(&format!(
                "https://space.bilibili.com/{mid}/video"
            ))
            .unwrap_or_else(|_| {
                reqwest::header::HeaderValue::from_static("https://www.bilibili.com")
            }),
        );
        if let Ok(v) = reqwest::header::HeaderValue::from_str(&cookie) {
            headers.insert(reqwest::header::COOKIE, v);
        }

        // 官方接口优先使用 WBI 签名。签名（nav）本身也可能被 B站 412/限流，
        // 一次失败不代表接口不可用：重试一次再回退 RSSHub/yt-dlp，避免
        // 偶发网络抖动让官方接口被静默跳过。
        let mut reason = String::new();
        for attempt in 0..2 {
            match self.wbi_query(mid, count).await {
                Ok(signed_query) => {
                    let ids = self
                        .request_upload_ids(&signed_query, &headers, count)
                        .await;
                    if !ids.is_empty() {
                        return (ids, reason);
                    }
                    if reason.is_empty() {
                        reason = "官方 wbi 接口未返回 BV（可能是 412 风控或接口变更）".to_string();
                    }
                    tracing::warn!(
                        "B站官方投稿接口第 {} 次尝试未返回 BV，重试一次",
                        attempt + 1
                    );
                }
                Err(e) => {
                    if reason.is_empty() {
                        reason = format!("官方接口签名失败: {e:#}");
                    }
                    tracing::warn!("B站官方 wbi 签名第 {} 次失败: {e:#}", attempt + 1);
                }
            }
        }
        (Vec::new(), reason)
    }

    async fn upload_ids_rsshub(&self, mid: &str, count: u32) -> (Vec<String>, String) {
        let rss = format!("https://rsshub.app/bilibili/user/video/{mid}");
        match self.client.get(&rss).send().await {
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    return (Vec::new(), format!("RSSHub 返回 HTTP {status}"));
                }
                match resp.text().await {
                    Ok(text) if text.starts_with('<') => {
                        let ids = extract_bvids(&text);
                        if ids.is_empty() {
                            return (Vec::new(), "RSSHub 返回 HTML 但未解析出 BV 号".to_string());
                        }
                        (
                            ids.into_iter().take(count as usize).collect(),
                            String::new(),
                        )
                    }
                    Ok(_) => (Vec::new(), "RSSHub 返回非 HTML 内容".to_string()),
                    Err(e) => (Vec::new(), format!("RSSHub 响应读取失败: {e:#}")),
                }
            }
            Err(e) => (Vec::new(), format!("RSSHub 请求失败: {e:#}")),
        }
    }

    async fn upload_ids_ytdlp(&self, mid: &str, count: u32) -> (Vec<String>, String) {
        // 与旧 Python 一致：优先用户配置 Cookie，其次桌面端浏览器授权 Cookie，
        // 再补匿名 buvid 文件；全部没有时才裸请求一次。Cookie 一律转成
        // `yt-dlp --cookies` 文件，不用易被编码/换行截断的 `--add-header`。
        // B站会间歇性 412：同一 Cookie 文件失败后立即重试一次，再换下一组。
        let candidate = self
            .project_venv_ytdlp()
            .unwrap_or_else(|| PathBuf::from("yt-dlp"));
        let user_cookies = self.user_cookie_file();
        let browser_cookies = browser_cookie_path(&self.project_dir);
        let anon = crate::bilibili_auth::ensure_anon_cookie(&self.project_dir).await;
        // 顺序：用户配置 -> 浏览器授权 -> 匿名 buvid；None 表示“该组不存在”。
        let mut cookie_options: Vec<Option<PathBuf>> = vec![
            user_cookies,
            Some(browser_cookies).filter(|p| p.exists()),
            anon,
        ];
        // 无任何 Cookie 文件时也试一次裸请求，避免“配置为空但网络可匿名”时
        // 明明能取到列表却提前返回失败。
        if cookie_options.is_empty() {
            cookie_options.push(None);
        }
        let mut last_error = String::new();
        // 每个 Cookie 组最多尝试 2 次；单次 yt-dlp 执行 30 秒超时，由
        // ManagedChild 统一回收进程组，不会让 412/网络卡顿无限拖住监控轮次。
        // 两次尝试之间加退避，避免风控/限流时立即连打同一出口加重 412。
        for cookie_file in cookie_options {
            let label = cookie_file
                .as_deref()
                .map(|p| {
                    p.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string()
                })
                .unwrap_or_else(|| "none".to_string());
            for attempt in 0..2 {
                let mut cmd = Command::new(&candidate);
                cmd.kill_on_drop(true);
                cmd.args(["--flat-playlist", "--playlist-end"])
                    .arg(count.to_string())
                    .args(["--print", "%(id)s"])
                    .arg(format!("https://space.bilibili.com/{mid}/video"));
                if let Some(cookie_file) = &cookie_file {
                    cmd.args(["--cookies", cookie_file.display().to_string().as_str()]);
                }
                cmd.stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                let Ok(child) = ManagedChild::spawn(&mut cmd) else {
                    break;
                };
                let Ok(out) = child.wait_with_output(Duration::from_secs(30)).await else {
                    last_error = "yt-dlp 兜底超时（30 秒）".to_string();
                    break;
                };
                let text = String::from_utf8_lossy(&out.stdout);
                let ids: Vec<String> = text
                    .lines()
                    .map(|s| s.trim().to_string())
                    .filter(|s| is_plausible_bvid(s))
                    .take(count as usize)
                    .collect();
                if !ids.is_empty() {
                    // 之前尝试可能被 412 挡掉，但只要本次 stdout 有完整 BV 列表
                    // 就按成功处理；必须清空旧错误，否则顶层日志会把成功误报成失败。
                    return (ids, String::new());
                }
                last_error = String::from_utf8_lossy(&out.stderr)
                    .chars()
                    .take(300)
                    .collect::<String>();
                warn!(
                    "yt-dlp 未返回 BV 号 (cookie={label}, attempt={}): {}",
                    attempt + 1,
                    last_error
                );
                if attempt == 0 {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
        warn!("yt-dlp 兜底全部失败: {last_error}");
        (Vec::new(), last_error)
    }

    /// 用 Electron 浏览器内核从 B站空间页提取投稿 BV 号。
    ///
    /// B站 `arc/search` 接口、RSSHub 和 yt-dlp 都可能因为匿名出口/公共实例
    /// 限制而失败；真实 Chromium 页面会跑完前端风控逻辑，是最接近桌面浏览器
    /// 的取数方式。这里只用于“取列表”，不触发下载，因此不会占用带宽写文件。
    /// 目前调度轮次不调用（冷启动 20-40 秒会撞 50 秒护栏），保留给未来
    /// 后台任务/手动诊断使用。
    #[allow(dead_code)]
    async fn upload_ids_electron(&self, mid: &str, count: u32) -> Vec<String> {
        let cookie_path = crate::bilibili_auth::browser_cookie_path(&self.project_dir);
        let cookie = match std::fs::read_to_string(&cookie_path) {
            Ok(cookie) => cookie,
            Err(e) => {
                tracing::warn!(
                    "跳过 B站 Electron 列表提取：读取 Cookie 失败 path={} err={e:#}",
                    cookie_path.display()
                );
                return Vec::new();
            }
        };
        if cookie.trim().is_empty() {
            tracing::warn!(
                "跳过 B站 Electron 列表提取：Cookie 文件为空 path={}",
                cookie_path.display()
            );
            return Vec::new();
        }
        let url = format!("https://space.bilibili.com/{mid}/video");
        let script = r#"(function(){{
  var out={{ok:false,bvids:[]}};
  try{{
    var text=document.body ? document.body.innerText : '';
    var html=document.documentElement ? document.documentElement.innerHTML : '';
    var seen={{}};
    var re=/BV[0-9A-Za-z]{{10}}/g;
    (text+'\n'+html).replace(re, function(v){{ if(!seen[v]){{seen[v]=1; out.bvids.push(v);}} return v; }});
    out.ok=out.bvids.length>0;
    if(!out.ok && window.__INITIAL_STATE__){{
      var st=window.__INITIAL_STATE__;
      var list=(st.videoList||st.space||{{}}).list;
      if(Array.isArray(list)){{
        for(var i=0;i<list.length;i++){{ if(list[i].bvid && !seen[list[i].bvid]){{ seen[list[i].bvid]=1; out.bvids.push(list[i].bvid); }} }}
      }}
    }}
    out.ok=out.bvids.length>0;
  }}catch(e){{ out.error=String(e && e.message ? e.message : e); }}
  return JSON.stringify(out);
}})()"#
            .to_string();
        let result = match crate::bilibili_electron::run_bilibili_script(
            &self.project_dir,
            &script,
            &[("FUPAN_TARGET_URL", url)],
            std::time::Duration::from_secs(45),
        )
        .await
        {
            Ok(result) => result,
            Err(e) => {
                tracing::warn!("B站 Electron 投稿列表提取失败: {e:#}");
                return Vec::new();
            }
        };
        let value = result.value;
        let Some(bvids) = value.get("bvids").and_then(|v| v.as_array()) else {
            return Vec::new();
        };
        bvids
            .iter()
            .filter_map(|v| v.as_str())
            .filter(|s| is_plausible_bvid(s))
            .take(count as usize)
            .map(ToOwned::to_owned)
            .collect()
    }

    fn project_venv_ytdlp(&self) -> Option<PathBuf> {
        let candidates = [
            self.project_dir.join(".venv").join("bin").join("yt-dlp"),
            self.project_dir
                .join(".venv")
                .join("Scripts")
                .join("yt-dlp.exe"),
        ];
        candidates.into_iter().find(|p| p.exists())
    }
}

/// B站 BV 号是 10 位字母数字且以 BV1 开头；用于过滤 RSS/yt-dlp 输出里的脏文本。
fn is_plausible_bvid(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 12
        && text.starts_with("BV1")
        && bytes[3..].iter().all(|b| b.is_ascii_alphanumeric())
}

/// 从 B站官方 `arc/search` 成功 JSON 中提取 BV 列表。
///
/// `code != 0`、缺 `data.list.vlist` 或数组中无 BV 都视为“没有可用结果”，
/// 由上层继续 RSSHub/yt-dlp 兜底，不把风控/空响应伪装成成功。
fn parse_official_upload_response(data: &serde_json::Value, count: u32) -> Vec<String> {
    if data.get("code").and_then(|c| c.as_i64()) != Some(0) {
        return Vec::new();
    }
    let Some(rows) = data["data"]["list"]["vlist"].as_array() else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|r| {
            r.get("bvid")
                .and_then(|b| b.as_str())
                .map(|s| s.to_string())
        })
        .take(count as usize)
        .collect()
}

/// 从 RSS/HTML 文本提取 BV 号（保持顺序、去重）。
pub fn extract_bvids(text: &str) -> Vec<String> {
    let re = regex::Regex::new(r"BV[0-9A-Za-z]{10}").unwrap();
    let mut out = Vec::new();
    for cap in re.find_iter(text) {
        let s = cap.as_str().to_string();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

fn unix_to_date(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(0, 0).unwrap_or_default())
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d")
        .to_string()
}

/// RSS/Atom 适配器：解析标题/链接/封面/日期，返回统一 VideoMeta。
#[derive(Debug)]
pub struct RssAdapter {
    client: reqwest::Client,
}

impl Default for RssAdapter {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .unwrap_or_default(),
        }
    }
}

impl RssAdapter {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Platform for RssAdapter {
    fn name(&self) -> &'static str {
        "rss"
    }

    fn list_videos<'a>(
        &'a self,
        url: &'a str,
        count: u32,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<VideoMeta>>> + Send + 'a>> {
        Box::pin(async move {
            let resp = self.client.get(url).send().await?;
            // 非成功状态或非 XML/HTML 正文都是“没拿到有效订阅”，必须明确失败。
            // 若把空列表当成正常基线，用户会永远收不到该博主的新视频通知。
            if !resp.status().is_success() {
                return Err(anyhow!(
                    "RSS 源请求失败（HTTP {}）: {url}",
                    resp.status().as_u16()
                ));
            }
            let text = resp.text().await?;
            let trimmed = text.trim_start();
            if !trimmed.starts_with('<') {
                return Err(anyhow!("RSS 源未返回 XML/HTML 内容: {url}"));
            }
            Ok(parse_rss(&text)?.into_iter().take(count as usize).collect())
        })
    }
}

const ATOM_NS: &str = "http://www.w3.org/2005/Atom";
const MRSS_NS: &str = "http://search.yahoo.com/mrss/";

/// RSS 2.0 / Atom 解析。
///
/// 使用只读 XML DOM 而不是字符串切分，才能正确识别 XML 命名空间（如
/// `<atom:link>`、`<media:thumbnail>`）和 CDATA 文本；这两类在真实订阅源里很常见，
/// 旧 Python 依赖 `xml.etree.ElementTree`，Rust 侧也必须对齐同样的解析语义。
pub fn parse_rss(text: &str) -> Result<Vec<VideoMeta>> {
    let doc = roxmltree::Document::parse(text).context("RSS/Atom XML 解析失败")?;

    let mut rss = Vec::new();
    for item in doc
        .descendants()
        .filter(|n| matches_local_name(*n, "item", None))
    {
        let title = child_text(item, "title");
        let link = child_text(item, "link");
        if link.is_empty() {
            continue;
        }
        let guid = child_text(item, "guid");
        let cover = media_cover(item)
            .or_else(|| element_attr(item, "enclosure", "url"))
            .unwrap_or_default();
        let published_at = normalize_rss_date(Some(child_text(item, "pubdate")));
        rss.push(VideoMeta {
            id: if guid.is_empty() { link.clone() } else { guid },
            platform: "rss".to_string(),
            url: link,
            title,
            cover,
            published_at,
            duration: 0,
        });
    }
    if !rss.is_empty() {
        return Ok(rss);
    }

    let mut atom = Vec::new();
    for entry in doc
        .descendants()
        .filter(|n| matches_local_name(*n, "entry", Some(ATOM_NS)))
    {
        let title = namespaced_child_text(entry, ATOM_NS, "title");
        let link = atom_link(entry);
        if link.is_empty() {
            continue;
        }
        let guid = namespaced_child_text(entry, ATOM_NS, "id");
        let cover = media_cover(entry).unwrap_or_default();
        let published_at = {
            let published = namespaced_child_text(entry, ATOM_NS, "published");
            if published.is_empty() {
                namespaced_child_text(entry, ATOM_NS, "updated")
            } else {
                published
            }
        };
        atom.push(VideoMeta {
            id: if guid.is_empty() { link.clone() } else { guid },
            platform: "rss".to_string(),
            url: link,
            title,
            cover,
            published_at: normalize_rss_date(Some(published_at)),
            duration: 0,
        });
    }
    Ok(atom)
}

fn child_text<'a, 'input>(node: roxmltree::Node<'a, 'input>, name: &str) -> String {
    node.children()
        .filter(|n| n.is_element() && matches_local_name(*n, name, None))
        .find_map(|n| {
            let text = n.text().unwrap_or("").trim();
            (!text.is_empty()).then(|| text.to_string())
        })
        .unwrap_or_default()
}

fn matches_local_name<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    name: &str,
    allowed_namespace: Option<&str>,
) -> bool {
    if !node.is_element() || node.tag_name().name() != name {
        return false;
    }
    match allowed_namespace {
        Some(ns) => {
            node.tag_name().namespace() == Some(ns) || node.tag_name().namespace().is_none()
        }
        None => true,
    }
}

fn namespaced_child_text<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    namespace: &str,
    name: &str,
) -> String {
    node.children()
        .filter(|n| {
            n.is_element()
                && (n.tag_name().namespace() == Some(namespace)
                    || n.tag_name().namespace().is_none())
                && n.has_tag_name(name)
        })
        .find_map(|n| {
            let text = n.text().unwrap_or("").trim();
            (!text.is_empty()).then(|| text.to_string())
        })
        .unwrap_or_default()
}

fn element_attr<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    name: &str,
    attr: &str,
) -> Option<String> {
    node.children()
        .filter(|n| n.is_element() && n.has_tag_name(name))
        .find_map(|n| {
            n.attribute(attr)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
        })
}

fn media_cover<'a, 'input>(node: roxmltree::Node<'a, 'input>) -> Option<String> {
    ["thumbnail", "content"].into_iter().find_map(|name| {
        node.children()
            .filter(|n| {
                n.is_element() && n.tag_name().namespace() == Some(MRSS_NS) && n.has_tag_name(name)
            })
            .find_map(|n| {
                n.attribute("url")
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned)
            })
    })
}

fn atom_link<'a, 'input>(node: roxmltree::Node<'a, 'input>) -> String {
    node.children()
        .find(|n| {
            n.is_element() && n.tag_name().namespace() == Some(ATOM_NS) && n.has_tag_name("link")
        })
        .and_then(|n| n.attribute("href"))
        .unwrap_or("")
        .trim()
        .to_string()
}

/// 把 RSS 2.0 / Atom 常见日期规范成 `YYYY-MM-DD`，失败时回退 `YYYY-MM-DD` 前缀。
/// 与旧 Python `video_monitor.py` 的 `_rss_date` 语义保持一致。
fn normalize_rss_date(raw: Option<String>) -> String {
    let raw = raw.unwrap_or_default();
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    // RFC 2822 / RFC 1123 日期（RSS 2.0 常用）。
    let rfc = chrono::DateTime::parse_from_rfc2822(raw);
    if let Ok(dt) = rfc {
        return dt.format("%Y-%m-%d").to_string();
    }
    // ISO 8601（Atom 常用），含 `Z` 或时区偏移。
    let iso = chrono::DateTime::parse_from_rfc3339(raw);
    if let Ok(dt) = iso {
        return dt.format("%Y-%m-%d").to_string();
    }
    // 兼容无时区的基础日期时间。
    let plain = chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S")
        .map(|dt| dt.format("%Y-%m-%d").to_string());
    if let Ok(date) = plain {
        return date;
    }
    // 已无法识别时，保留前 10 位；连日期前缀都没有则返回空串。
    let date = raw.chars().take(10).collect::<String>();
    if date.len() == 10 && date.chars().nth(4) == Some('-') && date.chars().nth(7) == Some('-') {
        date
    } else {
        String::new()
    }
}

/// 带项目根的平台注册表：yt-dlp 兜底按项目 .venv 查找，跨目录运行也可用。
pub fn resolve_with_project_dir(url: &str, project_dir: &Path) -> Box<dyn Platform> {
    if url.contains("bilibili.com") || url.contains("b23.tv") {
        let cookie = crate::config::Config::load(project_dir)
            .ok()
            .map(|c| c.get_fupan("视频监控", "b站cookie"))
            .unwrap_or_default();
        Box::new(BilibiliAdapter::with_cookie_and_project_dir(
            cookie,
            project_dir.to_path_buf(),
        ))
    } else {
        Box::new(RssAdapter::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_bvids_ordered_unique() {
        let text = "BV1abcDEFghi BV1abcDEFghi BV1xyz123456";
        assert_eq!(extract_bvids(text), vec!["BV1abcDEFghi", "BV1xyz123456"]);
    }

    #[test]
    fn parses_rss_items() {
        let xml = r#"<rss xmlns:media="http://search.yahoo.com/mrss/"><channel>
          <item><title>测试一</title><link>https://x.test/1</link><guid>g1</guid>
          <media:thumbnail url="https://x.test/1.jpg"/></item>
          <item><title>测试二</title><link>https://x.test/2</link><guid>g2</guid></item>
        </channel></rss>"#;
        let items = parse_rss(xml).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "测试一");
        assert_eq!(items[0].cover, "https://x.test/1.jpg");
        assert_eq!(items[1].url, "https://x.test/2");
    }

    #[test]
    fn parses_atom_entries() {
        let xml = r#"<feed xmlns="http://www.w3.org/2005/Atom"
          xmlns:media="http://search.yahoo.com/mrss/">
          <entry><title>Atom 视频</title><id>urn:1</id>
          <link href="https://x.test/1"/>
          <media:thumbnail url="https://x.test/1.jpg"/>
          <published>2026-08-12T10:00:00+08:00</published></entry>
        </feed>"#;
        let items = parse_rss(xml).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Atom 视频");
        assert_eq!(items[0].url, "https://x.test/1");
        assert_eq!(items[0].cover, "https://x.test/1.jpg");
        assert_eq!(items[0].published_at, "2026-08-12");
    }

    #[test]
    fn parses_cdata_titles_and_rss_default_namespace() {
        let xml = r#"<rss version="2.0" xmlns="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
          <channel>
            <item>
              <title><![CDATA[一个 <带标签> 的标题]]></title>
              <link>https://x.test/cdata</link>
            </item>
          </channel>
        </rss>"#;
        let items = parse_rss(xml).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "一个 <带标签> 的标题");
        assert_eq!(items[0].url, "https://x.test/cdata");
    }

    #[test]
    fn malformed_rss_is_an_error_not_empty_baseline() {
        assert!(parse_rss("<rss><item><title>未闭合").is_err());
    }

    #[test]
    fn rss_link_is_preferred_over_empty_atom_link() {
        let xml = r#"<rss xmlns:atom="http://www.w3.org/2005/Atom"><channel>
          <item>
            <title>RSS 标题</title>
            <atom:link href=""/>
            <link>https://x.test/rss</link>
          </item>
        </channel></rss>"#;
        let items = parse_rss(xml).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].url, "https://x.test/rss");
    }

    #[test]
    fn normalizes_rss_and_atom_dates() {
        assert_eq!(
            normalize_rss_date(Some("Wed, 12 Aug 2026 10:00:00 +0800".to_string())),
            "2026-08-12"
        );
        assert_eq!(
            normalize_rss_date(Some("2026-08-12T10:00:00+08:00".to_string())),
            "2026-08-12"
        );
        assert_eq!(
            normalize_rss_date(Some("2026-08-12T10:00:00".to_string())),
            "2026-08-12"
        );
        assert_eq!(normalize_rss_date(Some("not a date".to_string())), "");
        assert_eq!(normalize_rss_date(None), "");
    }

    #[test]
    fn mid_parsed_from_space_url() {
        assert_eq!(
            BilibiliAdapter::mid_from_url("https://space.bilibili.com/946974/video"),
            Some("946974".to_string())
        );
    }

    #[test]
    fn unix_ts_converts_date() {
        assert_eq!(unix_to_date(1786464000), "2026-08-12");
    }

    #[test]
    fn wbi_query_sorts_params_and_appends_w_rid() {
        // 固定时间戳和图片 key，验证签名串与官方 WBI 拼接规则一致。
        let query = build_wbi_query(
            "1234567890abcdefghijklmnopqrstuvwxyz",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZ01",
            "946974",
            10,
            1786550400,
        );
        assert_eq!(
            query,
            "mid=946974&order=pubdate&platform=web&pn=1&ps=10&web_location=1550101&wts=1786550400&w_rid=1b8f0e19353b02df0a82591de339a31f"
        );
        assert!(query.contains("&w_rid="));
    }

    #[test]
    fn official_upload_response_requires_code_zero_and_bvids() {
        let ok = serde_json::json!({
            "code": 0,
            "data": {"list": {"vlist": [
                {"bvid": "BV1xx411c7mD"},
                {"bvid": "BV1yy411c7mD"},
                {"title": "无 bvid 的行应被忽略"}
            ]}}
        });
        assert_eq!(
            parse_official_upload_response(&ok, 10),
            vec!["BV1xx411c7mD".to_string(), "BV1yy411c7mD".to_string()]
        );
        assert_eq!(
            parse_official_upload_response(&serde_json::json!({"code": -412}), 10),
            Vec::<String>::new()
        );
        assert_eq!(
            parse_official_upload_response(&serde_json::json!({"code": 0, "data": {}}), 10),
            Vec::<String>::new()
        );
        assert_eq!(
            parse_official_upload_response(
                &serde_json::json!({"code": 0, "data": {"list": {"vlist": []}}}),
                10
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn upload_failure_message_distinguishes_network_from_risk_control() {
        // 网络/DNS 故障：必须提示查网络，不能误导成“请配置 cookie”。
        let net = upload_list_failure_message(
            "官方接口签名失败: reqwest error resolving api.bilibili.com: nodename nor servname provided",
            "RSSHub 请求失败: connection timed out",
            "yt-dlp 兜底超时（30 秒）",
        );
        assert!(
            net.contains("解析失败") || net.contains("请求失败"),
            "{net}"
        );
        assert!(net.contains("检查网络"), "{net}");
        assert!(!net.contains("请配置 B站 cookie"), "{net}");

        // 风控：提示配置 cookie。
        let risk = upload_list_failure_message(
            "官方 wbi 接口未返回 BV（可能是 412 风控或接口变更）",
            "RSSHub 返回 HTTP 403",
            "yt-dlp 未返回 BV 号",
        );
        assert!(risk.contains("412"), "{risk}");
        assert!(risk.contains("请配置 B站 cookie"), "{risk}");

        // 完全无原因：给出通用提示，不崩溃。
        let empty = upload_list_failure_message("", "", "");
        assert!(empty.contains("三个兜底均未返回数据"), "{empty}");
    }
}
