use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use anyhow::Result;
use serde_json::{Value, json};
use sqlx::Row;
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

    /// 浏览博主历史视频专用：只补新记录，已有记录不覆盖 anchor/source_url 归属。
    pub async fn upsert_browse(&self, meta: &VideoMeta, source_url: &str) -> Result<String> {
        let cover_cache = cache_cover(&self.fupan_dir, &meta.cover, &meta.id).await?;
        db::upsert_video_browse(&self.pool, meta, source_url, &cover_cache).await?;
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

/// 给旧桌面端/脚本提供与 Python `date` 字段一致的视频 JSON。
///
/// Rust 统一内部字段名是 `published_at`，但旧 Python 的 `blogger-videos`
/// 和视频库输出还带有 `date`。输出层同时提供两者，避免已发布前端依赖
/// `v.date` 时因字段缺失而退化或异常，同时不污染持久化模型。
pub fn video_compat_json(video: &Video) -> Value {
    let mut value = serde_json::to_value(video).unwrap_or(Value::Null);
    if let Some(obj) = value.as_object_mut() {
        obj.entry("date")
            .or_insert_with(|| json!(video.published_at));
    }
    value
}

pub fn videos_compat_json(videos: &[Video]) -> Value {
    Value::Array(videos.iter().map(video_compat_json).collect())
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct LegacyVideoMigration {
    pub legacy_path: String,
    pub migrated: usize,
    pub skipped: usize,
    pub failed: usize,
}

/// 把旧 Python 独立视频库 `fupan_output/video_library.db` 合并到当前 Rust 主 DB。
///
/// 这里只做无破坏、幂等合并：主 DB 已有 id 的记录不覆盖，旧库里相同 id
/// 的 `downloaded` 状态也不会覆盖主 DB 已经更新的状态；因此安全可重复调用。
/// 当前主 DB 与旧库字段一致，但如果旧库表缺失，直接返回 0 而不把启动流程打断。
pub async fn migrate_legacy_video_db(
    pool: &SqlitePool,
    fupan_dir: &Path,
) -> Result<LegacyVideoMigration> {
    let legacy_path = fupan_dir.join("video_library.db");
    let mut out = LegacyVideoMigration {
        legacy_path: legacy_path.display().to_string(),
        ..Default::default()
    };
    if !legacy_path.exists() {
        return Ok(out);
    }

    let legacy_pool = SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&legacy_path)
            .read_only(true),
    )
    .await
    .with_context(|| format!("打开旧视频库失败: {}", legacy_path.display()))?;

    let has_videos: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='videos'",
    )
    .fetch_one(&legacy_pool)
    .await
    .with_context(|| "检查旧视频库表结构失败")?;
    if has_videos == 0 {
        return Ok(out);
    }

    let rows = sqlx::query(
        "SELECT id, platform, url, title, cover, cover_cache, published_at, duration,
                anchor, source_url, downloaded, downloaded_at, first_seen
         FROM videos",
    )
    .fetch_all(&legacy_pool)
    .await
    .with_context(|| "读取旧视频库失败")?;

    let mut tx = pool.begin().await.context("开始旧视频库迁移事务")?;
    for row in rows {
        let Ok(id) = row.try_get::<String, _>("id") else {
            out.failed += 1;
            continue;
        };
        let platform = row.try_get::<String, _>("platform").unwrap_or_default();
        let url = row.try_get::<String, _>("url").unwrap_or_default();
        let title = row.try_get::<String, _>("title").unwrap_or_default();
        let cover = row.try_get::<String, _>("cover").unwrap_or_default();
        let cover_cache = row.try_get::<String, _>("cover_cache").unwrap_or_default();
        let published_at = row.try_get::<String, _>("published_at").unwrap_or_default();
        let duration = row.try_get::<i64, _>("duration").unwrap_or(0);
        let anchor = row.try_get::<String, _>("anchor").unwrap_or_default();
        let source_url = row.try_get::<String, _>("source_url").unwrap_or_default();
        let downloaded = row.try_get::<i64, _>("downloaded").unwrap_or(0) != 0;
        let downloaded_at = row
            .try_get::<String, _>("downloaded_at")
            .unwrap_or_default();
        let first_seen = row.try_get::<String, _>("first_seen").unwrap_or_default();

        if db::insert_legacy_video(
            &mut tx,
            &id,
            &platform,
            &url,
            &title,
            &cover,
            &cover_cache,
            &published_at,
            duration,
            &anchor,
            &source_url,
            downloaded,
            &downloaded_at,
            &first_seen,
        )
        .await
        .with_context(|| format!("迁移旧视频记录失败: {id}"))?
        {
            out.migrated += 1;
        } else {
            out.skipped += 1;
        }
    }
    tx.commit().await.context("提交旧视频库迁移")?;
    Ok(out)
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
            Ok(bytes) if !bytes.is_empty() => Ok(crate::config::atomic_write(&dest, bytes)
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

    #[tokio::test]
    async fn legacy_video_migration_is_idempotent_and_does_not_overwrite() {
        let root = std::env::temp_dir().join(format!("zunzhe-legacy-video-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let legacy_dir = root.join("fupan_output");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        let main_db = root.join("main.db");
        let main = crate::db::open_pool(&main_db).await.unwrap();

        // 先用 db 层建表并写入主库已有记录。
        let existing = crate::model::VideoMeta {
            id: "legacy-id".into(),
            platform: "bilibili".into(),
            url: "https://example.com/old".into(),
            title: "主库标题".into(),
            cover: String::new(),
            published_at: "2026-08-12".into(),
            duration: 1,
        };
        crate::db::upsert_video(&main, &existing, "主播", "source", "")
            .await
            .unwrap();
        crate::db::mark_downloaded(&main, "legacy-id", true)
            .await
            .unwrap();

        // 旧库中同一 id 有不同标题、下载状态和封面缓存，迁移不能覆盖主库。
        let legacy_db = legacy_dir.join("video_library.db");
        let legacy = SqlitePool::connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&legacy_db)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        crate::db::init_db(&legacy).await.unwrap();
        sqlx::query(
            "INSERT INTO videos (id, platform, url, title, cover, cover_cache, published_at, duration, anchor, source_url, downloaded, downloaded_at, first_seen)
             VALUES ('legacy-id','bilibili','https://example.com/old','旧库标题','','cache','2026-01-01',2,'主播','source',0,'','')
             ON CONFLICT(id) DO NOTHING",
        )
        .execute(&legacy)
        .await
        .unwrap();
        legacy.close().await;

        let first = migrate_legacy_video_db(&main, &legacy_dir).await.unwrap();
        assert_eq!(first.skipped, 1);
        assert_eq!(first.migrated, 0);

        let second = migrate_legacy_video_db(&main, &legacy_dir).await.unwrap();
        assert_eq!(second.skipped, 1);
        assert_eq!(second.migrated, 0);

        let videos = crate::db::get_videos(&main, "", "", false).await.unwrap();
        let video = videos.iter().find(|v| v.id == "legacy-id").unwrap();
        assert_eq!(video.title, "主库标题");
        assert!(video.downloaded);
        assert_eq!(video.cover_cache, "");
        assert_eq!(video.duration, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn video_compat_json_includes_date_alias() {
        let video = Video {
            id: "BV1".into(),
            platform: "bilibili".into(),
            url: "https://example.com/video".into(),
            title: "标题".into(),
            cover: String::new(),
            cover_cache: String::new(),
            published_at: "2026-08-12".into(),
            duration: 0,
            anchor: "主播".into(),
            source_url: "https://example.com/space".into(),
            downloaded: false,
            downloaded_at: String::new(),
        };
        let value = video_compat_json(&video);
        assert_eq!(value["published_at"], "2026-08-12");
        assert_eq!(value["date"], "2026-08-12");
    }

    #[test]
    fn async_cache_cover_is_not_blocked_by_block_on() {
        // 编译期保证：cache_cover 是 async 且不在内部 block_on，可安全用于 daemon。
        fn assert_future_send<T: Send>(_f: impl std::future::Future<Output = T> + Send) {}
        let dir = std::env::temp_dir();
        assert_future_send(cache_cover(&dir, "", "x"));
    }
}
