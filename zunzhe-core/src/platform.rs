use anyhow::Context;
use serde::Serialize;
use std::future::Future;
use std::pin::Pin;

/// 统一视频元数据。各平台适配器把自有字段映射到这里，
/// 界面/下载队列/训练数据导出只认这一份结构。
#[derive(Debug, Clone, Serialize)]
pub struct VideoMeta {
    pub id: String,
    pub platform: String,
    pub url: String,
    pub title: String,
    pub cover: String,
    pub published_at: String,
    pub duration: i64,
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

/// B站适配器骨架：先解析 space.bilibili.com/<mid>，后续实现官方 API + WBI 签名。
#[derive(Debug)]
pub struct BilibiliAdapter {
    client: reqwest::Client,
}

impl Default for BilibiliAdapter {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
                .build()
                .unwrap_or_default(),
        }
    }
}

impl BilibiliAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    fn mid_from_url(url: &str) -> Option<String> {
        let re = regex::Regex::new(r"space\.bilibili\.com/(\d+)").ok()?;
        re.captures(url)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
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
        let mid = Self::mid_from_url(url)
            .with_context(|| format!("不是 B站空间地址: {url}"))?;
        // 骨架阶段先返回空列表；官方 API + WBI 签名在适配器阶段实现。
        let _ = (&self.client, mid, count);
        Ok(Vec::new())
        })
    }
}

/// 平台注册表：后续从配置/链接自动选择适配器。
pub fn resolve(url: &str) -> Box<dyn Platform> {
    if url.contains("bilibili.com") || url.contains("b23.tv") {
        Box::new(BilibiliAdapter::new())
    } else {
        Box::new(BilibiliAdapter::new())
    }
}
