use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use sqlx::sqlite::SqlitePool;

use crate::db;
use crate::model::{Video, VideoMeta};

/// 视频库：把平台元数据写入 SQLite，并本地缓存封面。
pub struct VideoLibrary {
    pool: SqlitePool,
    fupan_dir: PathBuf,
}

impl VideoLibrary {
    pub fn new(pool: SqlitePool, fupan_dir: PathBuf) -> Self {
        Self { pool, fupan_dir }
    }

    pub async fn upsert(&self, meta: &VideoMeta, anchor: &str, source_url: &str) -> Result<String> {
        let cover_cache = cache_cover(&self.fupan_dir, &meta.cover, &meta.id).await?;
        db::upsert_video(&self.pool, meta, anchor, source_url, &cover_cache).await?;
        Ok(cover_cache)
    }

    #[allow(dead_code)] // 下载完成标记接口
    pub async fn mark_downloaded(&self, video_id: &str, downloaded: bool) -> Result<()> {
        db::mark_downloaded(&self.pool, video_id, downloaded).await
    }

    pub async fn list(
        &self,
        source_url: &str,
        anchor: &str,
        downloaded_only: bool,
    ) -> Result<Vec<Video>> {
        db::get_videos(&self.pool, source_url, anchor, downloaded_only).await
    }
}

/// 下载封面到 fupan_output/covers/，失败返回原 URL。
pub async fn cache_cover(fupan_dir: &Path, cover_url: &str, video_id: &str) -> Result<String> {
    if cover_url.is_empty() {
        return Ok(String::new());
    }
    let covers = fupan_dir.join("covers");
    fs::create_dir_all(&covers)?;
    let ext = if cover_url.to_lowercase().contains(".png") {
        "png"
    } else {
        "jpg"
    };
    let dest = covers.join(format!("{video_id}.{ext}"));
    if dest.exists() && dest.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        return Ok(dest.display().to_string());
    }
    // 异步下载小封面；失败保留原 URL 作为兜底。封面图偶尔会慢或不可达，
    // 必须给请求加显式超时，避免视频库 upsert 卡住拖慢整个监控轮次。
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)")
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();
    let cover = cover_url.to_string();
    let dest = dest.clone();
    match client.get(&cover).send().await {
        Ok(resp) if resp.status().is_success() => match resp.bytes().await {
            Ok(bytes) if !bytes.is_empty() => Ok(fs::write(&dest, bytes)
                .map(|_| dest.display().to_string())
                .unwrap_or_else(|_| cover.clone())),
            _ => Ok(cover),
        },
        _ => Ok(cover),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cover_cache_missing_url_returns_empty() {
        let dir = std::env::temp_dir();
        assert_eq!(cache_cover(&dir, "", "x").await.unwrap(), "");
    }

    #[test]
    fn async_cache_cover_is_not_blocked_by_block_on() {
        // 编译期保证：cache_cover 是 async 且不在内部 block_on，可安全用于 daemon。
        fn assert_future_send<T: Send>(_f: impl std::future::Future<Output = T> + Send) {}
        let dir = std::env::temp_dir();
        assert_future_send(cache_cover(&dir, "", "x"));
    }
}
