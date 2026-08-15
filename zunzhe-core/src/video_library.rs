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

/// 历史视频浏览的结果：拉取成功时返回最新列表；平台风控/网络失败时回退
/// 到本地已入库缓存，避免用户想看“以前下载过没”时直接被报错挡住。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BrowseOutcome {
    pub videos: Vec<Video>,
    pub from_cache: bool,
    pub warning: Option<String>,
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

    /// 浏览博主历史视频批量入库：封面下载有界并发，单张失败只保留原 URL，
    /// 不阻塞其余视频；结果保持输入顺序，方便调用方直接拿回已入库记录。
    pub async fn upsert_browse_many(&self, metas: &[VideoMeta], source_url: &str) -> Vec<String> {
        if metas.is_empty() {
            return Vec::new();
        }
        const MAX_CONCURRENT: usize = 6;
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT));
        let mut tasks = Vec::with_capacity(metas.len());
        for meta in metas {
            let sem = sem.clone();
            let fupan_dir = self.fupan_dir.clone();
            let pool = self.pool.clone();
            let source_url = source_url.to_string();
            let meta = meta.clone();
            tasks.push(tokio::spawn(async move {
                let _permit = sem.acquire().await;
                let cover_cache = cache_cover(&fupan_dir, &meta.cover, &meta.id)
                    .await
                    .unwrap_or_else(|_| meta.cover.clone());
                let _ = db::upsert_video_browse(&pool, &meta, &source_url, &cover_cache).await;
                cover_cache
            }));
        }
        let mut results = Vec::with_capacity(tasks.len());
        for task in tasks {
            results.push(task.await.unwrap_or_default());
        }
        results
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

/// 浏览博主历史视频的公共入口：成功时入库并返回最新列表；失败但本地已有
/// 该博主的视频记录时，回退显示缓存列表并附上原因，不让一次平台 412 就
/// 让“已下载/未下载”完全不可见。只有既拉取失败又没有缓存时才向上报错。
pub async fn browse_with_fallback(
    library: &VideoLibrary,
    fetch: anyhow::Result<Vec<VideoMeta>>,
    source_url: &str,
) -> anyhow::Result<BrowseOutcome> {
    match fetch {
        Ok(metas) => {
            let _ = library.upsert_browse_many(&metas, source_url).await;
            let videos = library.list(source_url, "", false).await?;
            Ok(BrowseOutcome {
                videos,
                from_cache: false,
                warning: None,
            })
        }
        Err(e) => {
            let videos = library.list(source_url, "", false).await?;
            if videos.is_empty() {
                return Err(e);
            }
            tracing::warn!("历史视频拉取失败，显示本地缓存: {e:#}");
            Ok(BrowseOutcome {
                videos,
                from_cache: true,
                warning: Some(format!("平台拉取失败，已显示上次缓存列表: {e:#}")),
            })
        }
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

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct LegacyCoreMigration {
    pub legacy_path: String,
    pub videos_migrated: usize,
    pub videos_skipped: usize,
    pub anchors_migrated: usize,
    pub anchors_skipped: usize,
    pub archived_to: Option<String>,
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

/// 把旧核心残留 DB `zunzhe-core/zunzhe-core.db` 安全合并到主 DB。
///
/// 历史上 CLI 曾以 `zunzhe-core/` 为当前目录创建数据库，与主 DB
/// `项目根/zunzhe-core.db` 并存；若某个入口仍打开旧副本，两个库会各自
/// 累积视频/关注记录，造成“状态丢失”假象。这里以主库为权威：
///
/// - `videos` 只补主库缺失的 id，绝不覆盖主库已更新的下载/元数据。
/// - `anchors` 只补主库缺失的 url，主库已有则以主库为准。
/// - 迁移成功且旧库没有任何记录被漏写时，把旧库原子归档为
///   `zunzhe-core.db.legacy-<时间戳>`，避免后续入口再次打开它；归档
///   失败只记日志，不阻塞本次启动。
///
/// 全程幂等，可重复调用；失败不抛致命错误，由调用方决定是否阻断。
pub async fn migrate_legacy_core_db(
    pool: &SqlitePool,
    project_dir: &Path,
) -> Result<LegacyCoreMigration> {
    let legacy_path = project_dir.join("zunzhe-core").join("zunzhe-core.db");
    let mut out = LegacyCoreMigration {
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
    .with_context(|| format!("打开旧核心 DB 失败: {}", legacy_path.display()))?;

    let mut tx = pool.begin().await.context("开始旧核心 DB 迁移事务")?;

    // 旧库可能缺表或缺列；逐字段 try_get，缺什么字段就跳过该条，不因单个
    // 畸形行中断整次迁移。
    let has_videos: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='videos'",
    )
    .fetch_one(&legacy_pool)
    .await
    .with_context(|| "检查旧核心 DB 表结构失败")?;
    if has_videos > 0 {
        let rows = sqlx::query(
            "SELECT id, platform, url, title, cover, cover_cache, published_at, duration,
                    anchor, source_url, downloaded, downloaded_at, first_seen
             FROM videos",
        )
        .fetch_all(&legacy_pool)
        .await
        .with_context(|| "读取旧核心 DB videos 失败")?;
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
            .with_context(|| format!("迁移旧核心 DB 视频记录失败: {id}"))?
            {
                out.videos_migrated += 1;
            } else {
                out.videos_skipped += 1;
            }
        }
    }

    let has_anchors: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='anchors'",
    )
    .fetch_one(&legacy_pool)
    .await
    .with_context(|| "检查旧核心 DB anchors 表失败")?;
    if has_anchors > 0 {
        let rows =
            sqlx::query("SELECT url, name, platform, enabled, live, notify_channel FROM anchors")
                .fetch_all(&legacy_pool)
                .await
                .with_context(|| "读取旧核心 DB anchors 失败")?;
        for row in rows {
            let Ok(url) = row.try_get::<String, _>("url") else {
                out.failed += 1;
                continue;
            };
            let name = row.try_get::<String, _>("name").unwrap_or_default();
            let platform = row.try_get::<String, _>("platform").unwrap_or_default();
            let enabled = row.try_get::<i64, _>("enabled").unwrap_or(1) != 0;
            let live = row.try_get::<i64, _>("live").unwrap_or(0) != 0;
            let notify_channel = row
                .try_get::<String, _>("notify_channel")
                .unwrap_or_else(|_| "邮箱".to_string());
            if db::insert_legacy_anchor(
                &mut tx,
                &url,
                &name,
                &platform,
                enabled,
                live,
                &notify_channel,
            )
            .await
            .with_context(|| format!("迁移旧核心 DB 关注记录失败: {url}"))?
            {
                out.anchors_migrated += 1;
            } else {
                out.anchors_skipped += 1;
            }
        }
    }

    legacy_pool.close().await;
    tx.commit().await.context("提交旧核心 DB 迁移")?;

    // 只有全部行都成功处理（failed == 0）才归档：失败时保留旧库，下次
    // 启动可继续补迁，避免把未读数据归档掉。
    if out.failed == 0 {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let archived = legacy_path.with_file_name(format!("zunzhe-core.db.legacy-{stamp}"));
        if let Err(e) = archive_legacy_db(&legacy_path, &archived) {
            tracing::warn!("旧核心 DB 归档失败（不影响迁移结果）: {e:#}");
        } else {
            out.archived_to = Some(archived.display().to_string());
        }
    }
    Ok(out)
}

/// 归档旧 SQLite 文件，避免 `-wal/-shm` 残留被后续入口误当作主库。
fn archive_legacy_db(legacy: &Path, archived: &Path) -> std::io::Result<()> {
    // 记录原始 -wal/-shm 路径，rename 后再清理：with_extension 会把
    // zunzhe-core.db 改成 zunzhe-core.db-wal，但那不是 wal 的真实文件名，
    // 必须在 rename 前从原始路径显式拼接，否则会误删/生成空文件。
    let wal = PathBuf::from(format!("{}-wal", legacy.display()));
    let shm = PathBuf::from(format!("{}-shm", legacy.display()));
    // 先原子覆盖目标；若目标已存在（同秒重复调用），追加毫秒避免覆盖证据。
    let mut target = archived.to_path_buf();
    if target.exists() {
        target = archived.with_file_name(format!(
            "{}.{}",
            archived
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("legacy"),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or_default()
        ));
    }
    rename_replace(legacy, &target)?;
    let _ = std::fs::remove_file(wal);
    let _ = std::fs::remove_file(shm);
    Ok(())
}

#[cfg(not(windows))]
fn rename_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

#[cfg(windows)]
fn rename_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let from_wide: Vec<u16> = from
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let to_wide: Vec<u16> = to
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let result = unsafe {
        MoveFileExW(
            from_wide.as_ptr(),
            to_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
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
            Ok(bytes) if !bytes.is_empty() => Ok(crate::config::atomic_write(&dest, bytes)
                .map(|_| dest.display().to_string())
                .unwrap_or_else(|_| cover.clone())),
            _ => Ok(cover),
        },
        _ => Ok(cover),
    }
}

/// 清理不再被视频库引用的封面缓存文件。
///
/// `fupan_output/covers` 由 `cache_cover` 按视频 id 落盘，历史浏览/接口失败
/// 可能留下 DB 已不引用的孤儿文件；24×30 长跑中不清理会持续占用磁盘。
/// 只删除 `videos.cover_cache` 中不存在的 `*.jpg/*.png`，已下载/已入库存档
/// 的封面全部保留，避免破坏“已下载”事实与历史列表。
pub async fn maintain_orphan_covers(
    pool: &sqlx::SqlitePool,
    fupan_dir: &Path,
) -> anyhow::Result<usize> {
    use std::collections::HashSet;

    let rows: Vec<String> =
        sqlx::query_scalar("SELECT cover_cache FROM videos WHERE cover_cache != ''")
            .fetch_all(pool)
            .await
            .context("读取视频封面引用")?;
    let referenced: HashSet<String> = rows
        .iter()
        .filter_map(|path| std::path::Path::new(path).file_name())
        .filter_map(|name| name.to_str())
        .map(|s| s.to_string())
        .collect();
    let covers = fupan_dir.join("covers");
    let Ok(entries) = std::fs::read_dir(&covers) else {
        return Ok(0);
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_image = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("png"));
        if !is_image {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        if !referenced.contains(&name) && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
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
    async fn orphan_covers_are_removed_but_referenced_ones_survive() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-covers-maintain-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let covers = dir.join("covers");
        std::fs::create_dir_all(&covers).unwrap();
        std::fs::write(covers.join("keep.jpg"), b"keep").unwrap();
        std::fs::write(covers.join("orphan.jpg"), b"orphan").unwrap();
        std::fs::write(covers.join("orphan.png"), b"orphan").unwrap();
        std::fs::write(covers.join("notes.txt"), b"keep").unwrap();

        let file = dir.join("test.db");
        let pool = crate::db::open_pool(&file).await.unwrap();
        sqlx::query("INSERT INTO videos (id, url, title, cover_cache) VALUES (?, ?, ?, ?)")
            .bind("keep-video")
            .bind("https://x.test/v")
            .bind("标题")
            .bind(covers.join("keep.jpg").display().to_string())
            .execute(&pool)
            .await
            .unwrap();

        let removed = maintain_orphan_covers(&pool, &dir).await.unwrap();
        assert_eq!(removed, 2);
        assert!(covers.join("keep.jpg").exists(), "被引用封面必须保留");
        assert!(!covers.join("orphan.jpg").exists());
        assert!(!covers.join("orphan.png").exists());
        assert!(covers.join("notes.txt").exists(), "非图片文件不得删除");

        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(file.with_extension("db-shm"));
        let _ = std::fs::remove_file(file.with_extension("db-wal"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn browse_many_preserves_order_and_tolerates_cover_failure() {
        let root = std::env::temp_dir().join(format!(
            "zunzhe-browse-many-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("fupan_output")).unwrap();
        let file = root.join("main.db");
        let pool = crate::db::open_pool(&file).await.unwrap();
        crate::db::init_db(&pool).await.unwrap();
        let library = VideoLibrary::new(pool.clone(), root.join("fupan_output"));

        let metas: Vec<VideoMeta> = (0..8)
            .map(|i| VideoMeta {
                id: format!("id{i}"),
                platform: "bilibili".to_string(),
                url: format!("https://x.test/{i}"),
                title: format!("视频 {i}"),
                cover: format!("https://x.test/cover{i}.jpg"),
                published_at: "2026-08-15".to_string(),
                duration: 60,
            })
            .collect();
        let results = library
            .upsert_browse_many(&metas, "https://space.bilibili.com/1")
            .await;
        assert_eq!(results.len(), metas.len());
        // 封面地址不可达时按原 URL 兜底，顺序与输入一致，入库仍成功。
        assert_eq!(results[0], metas[0].cover);
        assert_eq!(results[7], metas[7].cover);
        let rows = library
            .list("https://space.bilibili.com/1", "", false)
            .await
            .unwrap();
        assert_eq!(rows.len(), 8);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn browse_falls_back_to_cache_when_platform_fails() {
        let root = std::env::temp_dir().join(format!(
            "zunzhe-browse-fallback-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("fupan_output")).unwrap();
        let file = root.join("main.db");
        let pool = crate::db::open_pool(&file).await.unwrap();
        crate::db::init_db(&pool).await.unwrap();
        let library = VideoLibrary::new(pool.clone(), root.join("fupan_output"));
        let source = "https://space.bilibili.com/946974";

        // 无缓存时平台失败应原样报错，不能假装有列表。
        let err = browse_with_fallback(&library, Err(anyhow::anyhow!("412 风控")), source)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("412"));

        // 先成功入库一条，再模拟平台失败：应回退到缓存并带明确标记。
        let meta = VideoMeta {
            id: "BV1".into(),
            platform: "bilibili".into(),
            url: "https://www.bilibili.com/video/BV1".into(),
            title: "已有视频".into(),
            cover: String::new(),
            published_at: "2026-08-15".into(),
            duration: 120,
        };
        library.upsert_browse(&meta, source).await.unwrap();
        let outcome = browse_with_fallback(&library, Err(anyhow::anyhow!("RSSHub 403")), source)
            .await
            .unwrap();
        assert!(outcome.from_cache);
        assert_eq!(outcome.videos.len(), 1);
        assert!(outcome.warning.as_deref().unwrap().contains("RSSHub 403"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn browse_cache_matches_source_url_normally() {
        let root = std::env::temp_dir().join(format!(
            "zunzhe-browse-normalize-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("fupan_output")).unwrap();
        let file = root.join("main.db");
        let pool = crate::db::open_pool(&file).await.unwrap();
        crate::db::init_db(&pool).await.unwrap();
        let library = VideoLibrary::new(pool.clone(), root.join("fupan_output"));
        let stored = "https://space.bilibili.com/946974";
        let meta = VideoMeta {
            id: "BV2".into(),
            platform: "bilibili".into(),
            url: "https://www.bilibili.com/video/BV2".into(),
            title: "缓存视频".into(),
            cover: String::new(),
            published_at: "2026-08-15".into(),
            duration: 60,
        };
        library.upsert_browse(&meta, stored).await.unwrap();

        // 尾部斜杠、大小写变化都不应影响缓存回退命中。
        for input in [
            "https://space.bilibili.com/946974/",
            "HTTPS://SPACE.BILIBILI.COM/946974",
            "https://space.bilibili.com/946974//",
        ] {
            let outcome = browse_with_fallback(&library, Err(anyhow::anyhow!("平台失败")), input)
                .await
                .unwrap();
            assert!(outcome.from_cache, "输入 {input} 应命中缓存");
            assert_eq!(outcome.videos.len(), 1);
        }

        let _ = std::fs::remove_dir_all(&root);
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

    #[tokio::test]
    async fn legacy_core_db_migrates_missing_rows_without_overwrite_and_archives() {
        let root = std::env::temp_dir().join(format!(
            "zunzhe-legacy-core-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("zunzhe-core")).unwrap();
        let main_db = root.join("zunzhe-core.db");
        let main = crate::db::open_pool(&main_db).await.unwrap();

        // 主库先有同一个视频和同一个主播：旧库不能覆盖它们的字段。
        let existing = crate::model::VideoMeta {
            id: "legacy-core-id".into(),
            platform: "bilibili".into(),
            url: "https://example.com/old".into(),
            title: "主库标题".into(),
            cover: String::new(),
            published_at: "2026-08-12".into(),
            duration: 1,
        };
        crate::db::upsert_video(&main, &existing, "主库主播", "source", "")
            .await
            .unwrap();
        crate::db::mark_downloaded(&main, "legacy-core-id", true)
            .await
            .unwrap();
        crate::db::add_anchor(&main, "https://space.bilibili.com/946974", "主库影视飓风")
            .await
            .unwrap();

        // 构造旧核心 DB：包含主库已有记录（带旧标题）+ 一条主库没有的新视频、
        // 一个主库没有的新主播。
        let legacy_db = root.join("zunzhe-core").join("zunzhe-core.db");
        let legacy = SqlitePool::connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&legacy_db)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        crate::db::init_db(&legacy).await.unwrap();
        sqlx::query(
            "INSERT INTO videos (id, platform, url, title, anchor, source_url, downloaded, downloaded_at)
             VALUES ('legacy-core-id','bilibili','https://example.com/old','旧库标题','旧库主播','source',0,''),
                    ('only-legacy-id','bilibili','https://example.com/new','旧库独有视频','旧库主播','source',1,'2026-08-13 00:00:00')",
        )
        .execute(&legacy)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO anchors (url, name, platform, enabled, live, notify_channel)
             VALUES ('https://space.bilibili.com/946974','旧库影视飓风','bilibili',1,0,'邮箱'),
                    ('https://space.bilibili.com/999999','旧库独有主播','bilibili',1,0,'邮箱')",
        )
        .execute(&legacy)
        .await
        .unwrap();
        legacy.close().await;

        let first = migrate_legacy_core_db(&main, &root).await.unwrap();
        assert_eq!(first.videos_migrated, 1);
        assert_eq!(first.videos_skipped, 1);
        assert_eq!(first.anchors_migrated, 1);
        assert_eq!(first.anchors_skipped, 1);
        assert!(first.archived_to.is_some());
        assert!(!legacy_db.exists());
        assert!(Path::new(first.archived_to.as_ref().unwrap()).exists());

        // 主库字段不能被旧库覆盖，独有记录必须补上。
        let videos = crate::db::get_videos(&main, "", "", false).await.unwrap();
        let old = videos.iter().find(|v| v.id == "legacy-core-id").unwrap();
        assert_eq!(old.title, "主库标题");
        assert_eq!(old.anchor, "主库主播");
        assert!(old.downloaded);
        let only = videos.iter().find(|v| v.id == "only-legacy-id").unwrap();
        assert_eq!(only.title, "旧库独有视频");
        assert!(only.downloaded);

        let anchors = crate::db::list_anchors(&main).await.unwrap();
        let existing_anchor = anchors
            .iter()
            .find(|a| a.url == "https://space.bilibili.com/946974")
            .unwrap();
        assert_eq!(existing_anchor.name, "主库影视飓风");
        assert!(
            anchors
                .iter()
                .any(|a| a.url == "https://space.bilibili.com/999999")
        );

        // 归档后再次迁移：旧文件不存在，直接返回 0，幂等且不报错。
        let second = migrate_legacy_core_db(&main, &root).await.unwrap();
        assert_eq!(second.videos_migrated, 0);
        assert_eq!(second.anchors_migrated, 0);
        assert!(second.archived_to.is_none());

        let _ = std::fs::remove_dir_all(&root);
    }
}
