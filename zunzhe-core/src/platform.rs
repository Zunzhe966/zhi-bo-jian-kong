use anyhow::{Context, Result, anyhow};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tracing::{debug, warn};

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
/// 标题/封面/日期/时长；投稿 ID 走 RSSHub 或 yt-dlp 兜底（WBI 签名后续可插拔）。
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
        let nav = client
            .get("https://api.bilibili.com/x/web-interface/nav")
            .header("User-Agent", "Mozilla/5.0")
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;
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
        // 标准 WBI：参数按 key 排序后拼接，再附 mixin key，整体 md5。
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
        let mixin = mixin_key(&img_key, &sub_key);
        let digest = md5_hex(&format!("{query}{mixin}"));
        Ok(format!("{query}&w_rid={digest}"))
    }

    pub fn with_project_dir(project_dir: PathBuf) -> Self {
        Self {
            project_dir,
            ..Self::default()
        }
    }

    pub fn with_cookie_and_project_dir(cookie: impl Into<String>, project_dir: PathBuf) -> Self {
        Self {
            cookie: cookie.into(),
            project_dir,
            ..Self::default()
        }
    }

    fn mid_from_url(url: &str) -> Option<String> {
        let re = regex::Regex::new(r"space\.bilibili\.com/(\d+)").ok()?;
        re.captures(url)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
    }

    async fn view(&self, bvid: &str) -> VideoMeta {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::REFERER,
            reqwest::header::HeaderValue::from_static("https://www.bilibili.com"),
        );
        if !self.cookie.is_empty()
            && let Ok(v) = reqwest::header::HeaderValue::from_str(&self.cookie)
        {
            headers.insert(reqwest::header::COOKIE, v);
        }
        let url = format!("https://api.bilibili.com/x/web-interface/view?bvid={bvid}");
        let fallback = VideoMeta {
            id: bvid.to_string(),
            platform: "bilibili".to_string(),
            url: format!("https://www.bilibili.com/video/{bvid}"),
            title: bvid.to_string(),
            cover: String::new(),
            published_at: String::new(),
            duration: 0,
        };
        let resp = match self.client.get(&url).headers(headers).send().await {
            Ok(r) => r,
            Err(_) => return fallback,
        };
        let resp = match resp.error_for_status() {
            Ok(r) => r,
            Err(_) => return fallback,
        };
        let data: serde_json::Value = match resp.json().await {
            Ok(d) => d,
            Err(_) => return fallback,
        };
        if data.get("code").and_then(|c| c.as_i64()) != Some(0) {
            return fallback;
        }
        let d = &data["data"];
        let ts = d["pubdate"].as_i64().unwrap_or(0);
        VideoMeta {
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
        }
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
                out.push(task.await);
            }
            Ok(out)
        })
    }
}

impl BilibiliAdapter {
    async fn upload_ids_official(&self, mid: &str, count: u32) -> Vec<String> {
        if self.cookie.is_empty() {
            return Vec::new();
        }
        let url = format!(
            "https://api.bilibili.com/x/space/wbi/arc/search?mid={mid}&ps={count}&pn=1&order=pubdate"
        );
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
        if let Ok(v) = reqwest::header::HeaderValue::from_str(&self.cookie) {
            headers.insert(reqwest::header::COOKIE, v);
        }
        // 部分风控环境下需要 WBI 签名；这里先做“取 wbi 图片 + 重试”的轻量兜底。
        if let Ok(signed_query) = self.wbi_query(mid, count).await {
            let signed = format!("https://api.bilibili.com/x/space/wbi/arc/search?{signed_query}");
            if let Ok(resp) = self
                .client
                .get(&signed)
                .headers(headers.clone())
                .send()
                .await
                && let Ok(data) = resp.json::<serde_json::Value>().await
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
        let Ok(resp) = self.client.get(&url).headers(headers).send().await else {
            return Vec::new();
        };
        let Ok(data) = resp.json::<serde_json::Value>().await else {
            return Vec::new();
        };
        if data.get("code").and_then(|c| c.as_i64()) != Some(0) {
            return Vec::new();
        }
        data["data"]["list"]["vlist"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| {
                        r.get("bvid")
                            .and_then(|b| b.as_str())
                            .map(|s| s.to_string())
                    })
                    .take(count as usize)
                    .collect()
            })
            .unwrap_or_default()
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
        // 先找项目 .venv 里的 yt-dlp，再回退到系统 PATH，避免依赖当前工作目录。
        let candidate = self
            .project_venv_ytdlp()
            .unwrap_or_else(|| PathBuf::from("yt-dlp"));
        debug!(
            "yt-dlp fallback: {} (cookie={})",
            candidate.display(),
            !self.cookie.is_empty()
        );
        let mut cmd = Command::new(&candidate);
        cmd.kill_on_drop(true);
        cmd.args(["--flat-playlist", "--playlist-end"])
            .arg(count.to_string())
            .args(["--print", "%(id)s"])
            .arg(format!("https://space.bilibili.com/{mid}/video"));
        // 带用户 Cookie 可显著降低 B站风控；匿名请求容易被 352/412 拒绝。
        if !self.cookie.is_empty() {
            cmd.args(["--add-header", &format!("Cookie: {}", self.cookie)]);
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
    let days = ts / 86400;
    let base = chrono::DateTime::from_timestamp(0, 0)
        .map(|d| d + chrono::Duration::days(days))
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(ts, 0).unwrap_or_default());
    base.format("%Y-%m-%d").to_string()
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
            let text = resp.text().await?;
            Ok(parse_rss(&text).into_iter().take(count as usize).collect())
        })
    }
}

/// 极简 RSS 2.0 解析（够用且可测；完整能力交给后续 quick-xml 升级）。
pub fn parse_rss(text: &str) -> Vec<VideoMeta> {
    let mut out = Vec::new();
    // RSS 2.0 items
    let items: Vec<String> = split_original(text, "item");
    for item in items {
        let title = tag_text_case(&item, "title").unwrap_or_default();
        let link = tag_text_case(&item, "link").unwrap_or_default();
        if link.is_empty() {
            continue;
        }
        let guid = tag_text_case(&item, "guid").unwrap_or_else(|| link.clone());
        let cover = tag_attr_case(&item, "url")
            .or_else(|| tag_text_case(&item, "enclosure"))
            .unwrap_or_default();
        let published_at = tag_text_case(&item, "pubdate").unwrap_or_default();
        out.push(VideoMeta {
            id: guid,
            platform: "rss".to_string(),
            url: link,
            title,
            cover,
            published_at,
            duration: 0,
        });
    }
    // Atom entries（media:thumbnail/content 兼容）
    if out.is_empty() {
        let entries: Vec<String> = split_original(text, "entry");
        for entry in entries {
            let title = tag_text_case(&entry, "title").unwrap_or_default();
            let link = tag_attr_case(&entry, "href")
                .or_else(|| tag_text_case(&entry, "link"))
                .unwrap_or_default();
            if link.is_empty() {
                continue;
            }
            let guid = tag_text_case(&entry, "id").unwrap_or_else(|| link.clone());
            let cover = tag_attr_case(&entry, "url")
                .or_else(|| tag_text_case(&entry, "enclosure"))
                .unwrap_or_default();
            let published_at = tag_text_case(&entry, "published")
                .or_else(|| tag_text_case(&entry, "updated"))
                .unwrap_or_default();
            out.push(VideoMeta {
                id: guid,
                platform: "rss".to_string(),
                url: link,
                title,
                cover,
                published_at,
                duration: 0,
            });
        }
    }
    out
}

fn split_original(text: &str, tag: &str) -> Vec<String> {
    split_tags(&text.to_lowercase(), tag)
        .into_iter()
        .map(|range| text[range].to_string())
        .collect()
}

fn split_tags(text: &str, tag: &str) -> Vec<std::ops::Range<usize>> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        let body_start = start + open.len();
        let after = &rest[body_start..];
        if let Some(end) = after.find(&close) {
            let base = text.len() - rest.len();
            out.push((base + body_start)..(base + body_start + end));
            rest = &after[end + close.len()..];
        } else {
            break;
        }
    }
    out
}

fn tag_text_case(item: &str, tag: &str) -> Option<String> {
    let lower = item.to_lowercase();
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = lower.find(&open)? + open.len();
    let end = lower[start..].find(&close)? + start;
    let value = item[start..end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn tag_attr_case(item: &str, attr: &str) -> Option<String> {
    let lower = item.to_lowercase();
    let pat = format!("{attr}=\"");
    let start = lower.find(&pat)? + pat.len();
    let end = lower[start..].find('"')? + start;
    let value = item[start..end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
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
        let xml = r#"<rss><channel>
          <item><title>测试一</title><link>https://x.test/1</link><guid>g1</guid>
          <media:thumbnail url="https://x.test/1.jpg"/></item>
          <item><title>测试二</title><link>https://x.test/2</link><guid>g2</guid></item>
        </channel></rss>"#;
        let items = parse_rss(xml);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "测试一");
        assert_eq!(items[0].cover, "https://x.test/1.jpg");
        assert_eq!(items[1].url, "https://x.test/2");
    }

    #[test]
    fn parses_atom_entries() {
        let xml = r#"<feed xmlns="http://www.w3.org/2005/Atom">
          <entry><title>Atom 视频</title><id>urn:1</id>
          <link href="https://x.test/1"/>
          <media:thumbnail url="https://x.test/1.jpg"/>
          <published>2026-08-12T10:00:00+08:00</published></entry>
        </feed>"#;
        let items = parse_rss(xml);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Atom 视频");
        assert_eq!(items[0].url, "https://x.test/1");
        assert_eq!(items[0].cover, "https://x.test/1.jpg");
        assert_eq!(items[0].published_at, "2026-08-12T10:00:00+08:00");
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
        assert_eq!(unix_to_date(1786550400), "2026-08-12");
    }
}
