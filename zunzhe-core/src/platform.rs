use anyhow::{Context, Result, anyhow};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tracing::{debug, warn};

use crate::bilibili_auth::{
    browser_cookie_path, cookie_header_from_netscape_public, cookie_header_from_project,
    ensure_user_cookie_file,
};
use crate::model::VideoMeta;

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

/// 用 B 站 WBI 公钥和投稿列表参数生成签名查询串。
///
/// 纯函数便于单测；排序、拼参、`w_rid` 均按官方 WBI 规则执行。
fn build_wbi_query(img_key: &str, sub_key: &str, mid: &str, count: u32, wts: u64) -> String {
    let mut params = [
        ("mid".to_string(), mid.to_string()),
        ("ps".to_string(), count.to_string()),
        ("pn".to_string(), "1".to_string()),
        ("order".to_string(), "pubdate".to_string()),
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
    /// 取 B 站 wbi 图片并生成查询签名（w_rid/wts）。失败返回空串，调用方走无签名兜底。
    async fn wbi_query(&self, mid: &str, count: u32) -> anyhow::Result<String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        // nav 是 WBI 图片 key 的来源；有配置/浏览器 Cookie 时也必须一并带上，
        // 否则即使用户已授权，这一步仍可能被 B站当成匿名出口并 412，导致
        // 官方投稿接口永远回退到 RSSHub/yt-dlp。
        let cookie = self.effective_cookie();
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
    fn browser_cookie_from_project(&self) -> String {
        cookie_header_from_project(&self.project_dir)
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
        String::new()
    }

    pub fn with_cookie_and_project_dir(cookie: impl Into<String>, project_dir: PathBuf) -> Self {
        Self {
            cookie: cookie.into(),
            project_dir,
            ..Self::default()
        }
    }

    fn effective_cookie(&self) -> String {
        if !self.cookie.is_empty() {
            self.cookie.clone()
        } else if let Some(cookie) = self.user_cookie_file() {
            let Ok(text) = std::fs::read_to_string(cookie) else {
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
        let cookie = self.effective_cookie();
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
            // 官方 wbi/arc 接口优先（带 cookie），再 RSSHub，最后 yt-dlp。
            let mut bvids = self.upload_ids_official(&mid, count).await;
            if bvids.is_empty() {
                bvids = self.upload_ids_rsshub(&mid, count).await;
            }
            if bvids.is_empty() {
                bvids = self.upload_ids_ytdlp(&mid, count).await;
            }
            if bvids.is_empty() {
                return Err(anyhow!(
                    "B站投稿列表拉取失败，可能被 412 风控，请配置 B站 cookie 后重试"
                ));
            }
            // view 补全并发执行：B站接口单次 1-3 秒，10 条串行会拖慢监控轮次。
            let mut tasks = Vec::with_capacity(bvids.len());
            for bvid in &bvids {
                tasks.push(self.view(bvid));
            }
            let mut out = Vec::with_capacity(tasks.len());
            for task in tasks {
                // 占位/补全失败的结果不能进入历史视频库，否则会伪装成“成功但空”。
                if let Some(meta) = task.await {
                    out.push(meta);
                }
            }
            if out.is_empty() {
                return Err(anyhow!("B站视频信息补全失败，请配置 B站 cookie 或稍后重试"));
            }
            Ok(out)
        })
    }
}

impl BilibiliAdapter {
    async fn upload_ids_official(&self, mid: &str, count: u32) -> Vec<String> {
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

        // 官方接口优先使用 WBI 签名。无 Cookie 也可尝试，失败再走匿名/下方兜底。
        if let Ok(signed_query) = self.wbi_query(mid, count).await {
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
            if let Ok(data) = resp.json::<serde_json::Value>().await
                && data.get("code").and_then(|c| c.as_i64()) == Some(0)
                && let Some(rows) = data["data"]["list"]["vlist"].as_array()
            {
                let ids: Vec<String> = rows
                    .iter()
                    .filter_map(|r| {
                        r.get("bvid")
                            .and_then(|b| b.as_str())
                            .map(|s| s.to_string())
                    })
                    .take(count as usize)
                    .collect();
                if !ids.is_empty() {
                    return ids;
                }
            }
        }
        Vec::new()
    }

    async fn upload_ids_rsshub(&self, mid: &str, count: u32) -> Vec<String> {
        let rss = format!("https://rsshub.app/bilibili/user/video/{mid}");
        if let Ok(resp) = self.client.get(&rss).send().await
            && resp.status().is_success()
            && let Ok(text) = resp.text().await
            && text.starts_with('<')
        {
            let ids = extract_bvids(&text);
            if !ids.is_empty() {
                return ids.into_iter().take(count as usize).collect();
            }
        }
        Vec::new()
    }

    async fn upload_ids_ytdlp(&self, mid: &str, count: u32) -> Vec<String> {
        let cookie = self.effective_cookie();
        // 与旧 Python 一致：用户配置 Cookie 优先转成 `--cookies`；没有账号 Cookie
        // 才补匿名 buvid 文件，且不把用户 Cookie 写成易被截断的 `--add-header`。
        let candidate = self
            .project_venv_ytdlp()
            .unwrap_or_else(|| PathBuf::from("yt-dlp"));
        debug!(
            "yt-dlp fallback: {} (cookie={})",
            candidate.display(),
            !cookie.is_empty()
        );
        let mut cmd = Command::new(&candidate);
        cmd.kill_on_drop(true);
        cmd.args(["--flat-playlist", "--playlist-end"])
            .arg(count.to_string())
            .args(["--print", "%(id)s"])
            .arg(format!("https://space.bilibili.com/{mid}/video"));
        let user_cookies = self.user_cookie_file();
        let browser_cookies = browser_cookie_path(&self.project_dir);
        if let Some(user_cookies) = user_cookies {
            cmd.args(["--cookies", user_cookies.display().to_string().as_str()]);
        } else if browser_cookies.exists() {
            cmd.args(["--cookies", browser_cookies.display().to_string().as_str()]);
        } else if cookie.is_empty()
            && let Some(anon) = crate::bilibili_auth::ensure_anon_cookie(&self.project_dir).await
        {
            cmd.args(["--cookies", anon.display().to_string().as_str()]);
        }
        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        if let Ok(child) = child {
            match tokio::time::timeout(Duration::from_secs(30), child.wait_with_output()).await {
                Ok(Ok(out)) => {
                    let text = String::from_utf8_lossy(&out.stdout);
                    let ids: Vec<String> = text
                        .lines()
                        .map(|s| s.trim().to_string())
                        .filter(|s| is_plausible_bvid(s))
                        .take(count as usize)
                        .collect();
                    if ids.is_empty() {
                        let err = String::from_utf8_lossy(&out.stderr);
                        warn!(
                            "yt-dlp 未返回 BV 号: {}",
                            err.chars().take(300).collect::<String>()
                        );
                    } else {
                        debug!("yt-dlp 返回 {} 条", ids.len());
                    }
                    return ids;
                }
                Ok(Err(e)) => {
                    warn!("yt-dlp 读取输出失败: {e}");
                    return Vec::new();
                }
                Err(_) => {
                    warn!("yt-dlp 兜底超时（30 秒）");
                    return Vec::new();
                }
            }
        }
        warn!("yt-dlp 启动失败: {candidate:?}");
        Vec::new()
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
        Box::new(BilibiliAdapter::with_project_dir(project_dir.to_path_buf()))
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
            "mid=946974&order=pubdate&pn=1&ps=10&wts=1786550400&w_rid=6ee69f49fe6f88e479e6a504b0521981"
        );
        assert!(query.contains("&w_rid="));
    }
}
