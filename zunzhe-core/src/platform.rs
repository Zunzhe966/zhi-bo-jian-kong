use anyhow::{Context, Result, anyhow};
use std::future::Future;
use std::pin::Pin;

use crate::model::VideoMeta;

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
}

impl Default for BilibiliAdapter {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
                .build()
                .unwrap_or_default(),
            cookie: String::new(),
        }
    }
}

impl BilibiliAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_cookie(cookie: impl Into<String>) -> Self {
        Self {
            cookie: cookie.into(),
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
            // 先试 RSSHub 投稿列表，失败则 yt-dlp 兜底。
            let bvids = self.upload_ids(&mid, count).await;
            if bvids.is_empty() {
                return Err(anyhow!(
                    "B站投稿列表拉取失败，可能被 412 风控，请配置 B站 cookie 后重试"
                ));
            }
            let mut out = Vec::new();
            for bvid in bvids {
                out.push(self.view(&bvid).await);
            }
            Ok(out)
        })
    }
}

impl BilibiliAdapter {
    async fn upload_ids(&self, mid: &str, count: u32) -> Vec<String> {
        let rss = format!("https://rsshub.app/bilibili/user/video/{mid}");
        if let Ok(resp) = self.client.get(&rss).send().await
            && let Ok(text) = resp.text().await
        {
            let ids = extract_bvids(&text);
            if !ids.is_empty() {
                return ids.into_iter().take(count as usize).collect();
            }
        }
        // yt-dlp 兜底
        let mut cmd = std::process::Command::new("yt-dlp");
        cmd.args(["--flat-playlist", "--playlist-end"])
            .arg(count.to_string())
            .args(["--print", "%(id)s"])
            .arg(format!("https://space.bilibili.com/{mid}/video"));
        if let Ok(out) = cmd.output() {
            let text = String::from_utf8_lossy(&out.stdout);
            return text
                .lines()
                .map(|s| s.trim().to_string())
                .filter(|s| s.starts_with("BV"))
                .take(count as usize)
                .collect();
        }
        Vec::new()
    }
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
    let lower = text.to_lowercase();
    let items: Vec<&str> = split_tags(&lower, "item");
    for item in items {
        let title = tag_text(item, "title").unwrap_or_default();
        let link = tag_text(item, "link").unwrap_or_default();
        if link.is_empty() {
            continue;
        }
        let guid = tag_text(item, "guid").unwrap_or_else(|| link.clone());
        let cover = tag_attr(item, "url")
            .or_else(|| tag_text(item, "enclosure"))
            .unwrap_or_default();
        let published_at = tag_text(item, "pubdate").unwrap_or_default();
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
    out
}

fn split_tags<'a>(text: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        let body_start = start + open.len();
        let after = &rest[body_start..];
        if let Some(end) = after.find(&close) {
            out.push(&after[..end]);
            rest = &after[end + close.len()..];
        } else {
            break;
        }
    }
    out
}

fn tag_text(item: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = item.find(&open)? + open.len();
    let end = item[start..].find(&close)? + start;
    let value = item[start..end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn tag_attr(item: &str, attr: &str) -> Option<String> {
    let pat = format!("{attr}=\"");
    let start = item.find(&pat)? + pat.len();
    let end = item[start..].find('"')? + start;
    let value = item[start..end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// 平台注册表：后续从配置/链接自动选择适配器。
pub fn resolve(url: &str) -> Box<dyn Platform> {
    if url.contains("bilibili.com") || url.contains("b23.tv") {
        Box::new(BilibiliAdapter::new())
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
