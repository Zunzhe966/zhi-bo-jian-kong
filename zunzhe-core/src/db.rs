use anyhow::Context;
use sqlx::Row;
use sqlx::sqlite::SqlitePool;

use crate::model::{Anchor, LiveSession, NotifyEvent, Task, Video};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS anchors (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    url            TEXT NOT NULL UNIQUE,
    name           TEXT NOT NULL DEFAULT '',
    platform       TEXT NOT NULL DEFAULT '',
    enabled        INTEGER NOT NULL DEFAULT 1,
    live           INTEGER NOT NULL DEFAULT 0,
    notify_channel TEXT NOT NULL DEFAULT '邮箱',
    created_at     TEXT NOT NULL DEFAULT (datetime('now', 'localtime')),
    updated_at     TEXT NOT NULL DEFAULT (datetime('now', 'localtime'))
);

CREATE TABLE IF NOT EXISTS videos (
    id            TEXT PRIMARY KEY,
    platform      TEXT NOT NULL DEFAULT '',
    url           TEXT NOT NULL DEFAULT '',
    title         TEXT NOT NULL DEFAULT '',
    cover         TEXT NOT NULL DEFAULT '',
    cover_cache   TEXT NOT NULL DEFAULT '',
    published_at  TEXT NOT NULL DEFAULT '',
    duration      INTEGER NOT NULL DEFAULT 0,
    anchor        TEXT NOT NULL DEFAULT '',
    source_url    TEXT NOT NULL DEFAULT '',
    downloaded    INTEGER NOT NULL DEFAULT 0,
    downloaded_at TEXT NOT NULL DEFAULT '',
    first_seen    TEXT NOT NULL DEFAULT '',
    updated_at    TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_videos_source ON videos(source_url);

CREATE TABLE IF NOT EXISTS notify (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    event      TEXT NOT NULL DEFAULT '',
    anchor     TEXT NOT NULL DEFAULT '',
    title      TEXT NOT NULL DEFAULT '',
    content    TEXT NOT NULL DEFAULT '',
    channel    TEXT NOT NULL DEFAULT '邮箱',
    status     TEXT NOT NULL DEFAULT 'pending',
    created_at TEXT NOT NULL DEFAULT (datetime('now', 'localtime'))
);

CREATE TABLE IF NOT EXISTS tasks (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    kind       TEXT NOT NULL DEFAULT '',
    url        TEXT NOT NULL DEFAULT '',
    name       TEXT NOT NULL DEFAULT '',
    status     TEXT NOT NULL DEFAULT 'pending',
    attempts   INTEGER NOT NULL DEFAULT 0,
    payload    TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now', 'localtime')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now', 'localtime'))
);

CREATE TABLE IF NOT EXISTS monitor_baseline (
    source_url TEXT PRIMARY KEY,
    baseline_at TEXT NOT NULL DEFAULT (datetime('now', 'localtime')),
    updated_at  TEXT NOT NULL DEFAULT (datetime('now', 'localtime'))
);

CREATE TABLE IF NOT EXISTS live_sessions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    url        TEXT NOT NULL,
    anchor     TEXT NOT NULL DEFAULT '',
    title      TEXT NOT NULL DEFAULT '',
    platform   TEXT NOT NULL DEFAULT '',
    stream_url TEXT NOT NULL DEFAULT '',
    status     TEXT NOT NULL DEFAULT 'recording',
    pid        INTEGER NOT NULL DEFAULT 0,
    file_path  TEXT NOT NULL DEFAULT '',
    started_at TEXT NOT NULL DEFAULT (datetime('now', 'localtime')),
    ended_at   TEXT NOT NULL DEFAULT '',
    error      TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT (datetime('now', 'localtime'))
);
CREATE INDEX IF NOT EXISTS idx_live_sessions_url ON live_sessions(url);
"#;

pub async fn init_db(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(SCHEMA)
        .execute(pool)
        .await
        .context("初始化 SQLite schema")?;
    Ok(())
}

pub async fn add_anchor(pool: &SqlitePool, url: &str, name: &str) -> anyhow::Result<()> {
    sqlx::query(
        r#"INSERT INTO anchors (url, name, platform)
           VALUES (?, ?, '') 
           ON CONFLICT(url) DO UPDATE SET name=excluded.name, updated_at=datetime('now','localtime')"#,
    )
    .bind(url)
    .bind(name)
    .execute(pool)
    .await
    .context("写入 anchors")?;
    Ok(())
}

#[allow(dead_code)] // 直播监控注册接口
pub async fn upsert_anchor(
    pool: &SqlitePool,
    url: &str,
    name: &str,
    platform: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"INSERT INTO anchors (url, name, platform)
           VALUES (?, ?, ?)
           ON CONFLICT(url) DO UPDATE SET name=excluded.name, platform=excluded.platform,
             updated_at=datetime('now','localtime')"#,
    )
    .bind(url)
    .bind(name)
    .bind(platform)
    .execute(pool)
    .await
    .context("写入 anchors")?;
    Ok(())
}

pub async fn remove_anchor(pool: &SqlitePool, url: &str) -> anyhow::Result<bool> {
    let result = sqlx::query("DELETE FROM anchors WHERE url = ?")
        .bind(url)
        .execute(pool)
        .await
        .context("删除 anchors")?;
    Ok(result.rows_affected() > 0)
}

pub async fn list_anchors(pool: &SqlitePool) -> anyhow::Result<Vec<Anchor>> {
    let rows = sqlx::query(
        "SELECT id, url, name, platform, enabled, live, notify_channel FROM anchors ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .context("查询 anchors")?;
    let mut out = Vec::new();
    for row in rows {
        out.push(Anchor {
            id: row.try_get("id")?,
            url: row.try_get("url")?,
            name: row.try_get("name")?,
            platform: row.try_get("platform")?,
            enabled: row.try_get::<i64, _>("enabled")? != 0,
            live: row.try_get::<i64, _>("live")? != 0,
            notify_channel: row.try_get("notify_channel")?,
        });
    }
    Ok(out)
}

pub async fn is_source_baselined(pool: &SqlitePool, source_url: &str) -> anyhow::Result<bool> {
    let row = sqlx::query("SELECT 1 FROM monitor_baseline WHERE source_url = ?")
        .bind(source_url)
        .fetch_optional(pool)
        .await
        .context("查询监控源基线")?;
    Ok(row.is_some())
}

pub async fn mark_source_baselined(pool: &SqlitePool, source_url: &str) -> anyhow::Result<()> {
    sqlx::query(
        r#"INSERT INTO monitor_baseline (source_url) VALUES (?)
           ON CONFLICT(source_url) DO UPDATE SET
             updated_at=datetime('now','localtime')"#,
    )
    .bind(source_url)
    .execute(pool)
    .await
    .context("记录监控源基线")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)] // 会话字段均为 SQLite 持久化必需
pub async fn start_live_session(
    pool: &SqlitePool,
    url: &str,
    anchor: &str,
    title: &str,
    platform: &str,
    stream_url: &str,
    pid: u32,
    file_path: &str,
) -> anyhow::Result<i64> {
    let result = sqlx::query(
        r#"INSERT INTO live_sessions
           (url, anchor, title, platform, stream_url, status, pid, file_path)
           VALUES (?,?,?,?,?, 'recording', ?, ?)"#,
    )
    .bind(url)
    .bind(anchor)
    .bind(title)
    .bind(platform)
    .bind(stream_url)
    .bind(pid)
    .bind(file_path)
    .execute(pool)
    .await
    .context("写入直播录制会话")?;
    Ok(result.last_insert_rowid())
}

pub async fn end_live_session(
    pool: &SqlitePool,
    id: i64,
    status: &str,
    error: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"UPDATE live_sessions SET status=?, error=?, pid=0,
           ended_at=datetime('now','localtime'), updated_at=datetime('now','localtime') WHERE id=?"#,
    )
    .bind(status)
    .bind(error)
    .bind(id)
    .execute(pool)
    .await
    .context("结束直播录制会话")?;
    Ok(())
}

pub async fn list_live_sessions(
    pool: &SqlitePool,
    status: &str,
    limit: i64,
) -> anyhow::Result<Vec<LiveSession>> {
    let mut sql = String::from(
        "SELECT id, url, anchor, title, platform, stream_url, status, pid, file_path,
                started_at, ended_at, error, updated_at
         FROM live_sessions WHERE 1=1",
    );
    if !status.is_empty() {
        sql.push_str(" AND status = ?");
    }
    sql.push_str(" ORDER BY id DESC LIMIT ?");
    let mut query = sqlx::query(&sql);
    if !status.is_empty() {
        query = query.bind(status);
    }
    query = query.bind(limit);
    let rows = query.fetch_all(pool).await.context("查询直播录制会话")?;
    let mut out = Vec::new();
    for row in rows {
        out.push(LiveSession {
            id: row.try_get("id")?,
            url: row.try_get("url")?,
            anchor: row.try_get("anchor")?,
            title: row.try_get("title")?,
            platform: row.try_get("platform")?,
            stream_url: row.try_get("stream_url")?,
            status: row.try_get("status")?,
            pid: row.try_get("pid")?,
            file_path: row.try_get("file_path")?,
            started_at: row.try_get("started_at")?,
            ended_at: row.try_get("ended_at")?,
            error: row.try_get("error")?,
            updated_at: row.try_get("updated_at")?,
        });
    }
    Ok(out)
}

pub async fn active_live_session(
    pool: &SqlitePool,
    url: &str,
) -> anyhow::Result<Option<LiveSession>> {
    let row = sqlx::query(
        r#"SELECT id, url, anchor, title, platform, stream_url, status, pid, file_path,
                  started_at, ended_at, error, updated_at
           FROM live_sessions
           WHERE url=? AND status='recording'
           ORDER BY id DESC LIMIT 1"#,
    )
    .bind(url)
    .fetch_optional(pool)
    .await
    .context("查询活动直播录制会话")?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(LiveSession {
        id: row.try_get("id")?,
        url: row.try_get("url")?,
        anchor: row.try_get("anchor")?,
        title: row.try_get("title")?,
        platform: row.try_get("platform")?,
        stream_url: row.try_get("stream_url")?,
        status: row.try_get("status")?,
        pid: row.try_get("pid")?,
        file_path: row.try_get("file_path")?,
        started_at: row.try_get("started_at")?,
        ended_at: row.try_get("ended_at")?,
        error: row.try_get("error")?,
        updated_at: row.try_get("updated_at")?,
    }))
}

#[allow(dead_code)] // 直播状态机写入接口
pub async fn set_anchor_live(pool: &SqlitePool, url: &str, live: bool) -> anyhow::Result<()> {
    sqlx::query("UPDATE anchors SET live=?, updated_at=datetime('now','localtime') WHERE url=?")
        .bind(if live { 1 } else { 0 })
        .bind(url)
        .execute(pool)
        .await
        .context("更新主播直播状态")?;
    Ok(())
}

pub async fn upsert_video(
    pool: &SqlitePool,
    video: &crate::model::VideoMeta,
    anchor: &str,
    source_url: &str,
    cover_cache: &str,
) -> anyhow::Result<()> {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    sqlx::query(
        r#"INSERT INTO videos
           (id, platform, url, title, cover, cover_cache, published_at, duration, anchor, source_url, updated_at)
           VALUES (?,?,?,?,?,?,?,?,?,?,?)
           ON CONFLICT(id) DO UPDATE SET
             platform=excluded.platform, url=excluded.url, title=excluded.title,
             cover=excluded.cover, cover_cache=excluded.cover_cache,
             published_at=excluded.published_at, duration=excluded.duration,
             anchor=excluded.anchor, source_url=excluded.source_url, updated_at=excluded.updated_at"#,
    )
    .bind(&video.id)
    .bind(&video.platform)
    .bind(&video.url)
    .bind(&video.title)
    .bind(&video.cover)
    .bind(cover_cache)
    .bind(&video.published_at)
    .bind(video.duration)
    .bind(anchor)
    .bind(source_url)
    .bind(now)
    .execute(pool)
    .await
    .context("写入 videos")?;
    Ok(())
}

pub async fn mark_downloaded(
    pool: &SqlitePool,
    video_id: &str,
    downloaded: bool,
) -> anyhow::Result<()> {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    sqlx::query("UPDATE videos SET downloaded=?, downloaded_at=?, updated_at=? WHERE id=?")
        .bind(if downloaded { 1 } else { 0 })
        .bind(if downloaded { now.as_str() } else { "" })
        .bind(if downloaded { now.as_str() } else { "" })
        .bind(video_id)
        .execute(pool)
        .await
        .context("标记视频已下载")?;
    Ok(())
}

/// 下载成功但视频库还没有该记录时，先补一行最小记录（不覆盖已有字段）。
pub async fn ensure_video_row(
    pool: &SqlitePool,
    video_id: &str,
    url: &str,
    title: &str,
    anchor: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"INSERT OR IGNORE INTO videos
           (id, platform, url, title, anchor, source_url, updated_at)
           VALUES (?, '', ?, ?, ?, ?, ?)"#,
    )
    .bind(video_id)
    .bind(url)
    .bind(title)
    .bind(anchor)
    .bind(url)
    .bind(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string())
    .execute(pool)
    .await
    .context("补写 videos 下载记录")?;
    Ok(())
}

pub async fn get_videos(
    pool: &SqlitePool,
    source_url: &str,
    anchor: &str,
    downloaded_only: bool,
) -> anyhow::Result<Vec<Video>> {
    let mut sql = String::from(
        "SELECT id, platform, url, title, cover, cover_cache, published_at, duration,
                anchor, source_url, downloaded, downloaded_at
         FROM videos WHERE 1=1",
    );
    if !source_url.is_empty() {
        sql.push_str(" AND source_url = ?");
    }
    if !anchor.is_empty() {
        sql.push_str(" AND anchor = ?");
    }
    if downloaded_only {
        sql.push_str(" AND downloaded = 1");
    }
    sql.push_str(" ORDER BY published_at DESC, updated_at DESC");

    let mut query = sqlx::query(&sql);
    if !source_url.is_empty() {
        query = query.bind(source_url);
    }
    if !anchor.is_empty() {
        query = query.bind(anchor);
    }
    let rows = query.fetch_all(pool).await.context("查询 videos")?;
    let mut out = Vec::new();
    for row in rows {
        out.push(Video {
            id: row.try_get("id")?,
            platform: row.try_get("platform")?,
            url: row.try_get("url")?,
            title: row.try_get("title")?,
            cover: row.try_get("cover")?,
            cover_cache: row.try_get("cover_cache")?,
            published_at: row.try_get("published_at")?,
            duration: row.try_get("duration")?,
            anchor: row.try_get("anchor")?,
            source_url: row.try_get("source_url")?,
            downloaded: row.try_get::<i64, _>("downloaded")? != 0,
            downloaded_at: row.try_get("downloaded_at")?,
        });
    }
    Ok(out)
}

#[allow(dead_code)] // 下载队列/界面查重接口
pub async fn is_downloaded(pool: &SqlitePool, video_id: &str) -> anyhow::Result<bool> {
    let row = sqlx::query("SELECT downloaded FROM videos WHERE id=?")
        .bind(video_id)
        .fetch_optional(pool)
        .await
        .context("查询已下载状态")?;
    Ok(row
        .map(|r| r.try_get::<i64, _>("downloaded").unwrap_or(0) != 0)
        .unwrap_or(false))
}

#[allow(dead_code)] // MCP/CLI video-library 查询接口
pub async fn list_videos(
    pool: &SqlitePool,
    source_url: &str,
    anchor: &str,
    downloaded_only: bool,
) -> anyhow::Result<Vec<Video>> {
    let mut sql = String::from(
        "SELECT id, platform, url, title, cover, cover_cache, published_at, duration, anchor, source_url, downloaded
         FROM videos WHERE 1=1",
    );
    if !source_url.is_empty() {
        sql.push_str(" AND source_url = ?");
    }
    if !anchor.is_empty() {
        sql.push_str(" AND anchor = ?");
    }
    if downloaded_only {
        sql.push_str(" AND downloaded = 1");
    }
    sql.push_str(" ORDER BY published_at DESC, updated_at DESC");

    let mut query = sqlx::query(&sql);
    if !source_url.is_empty() {
        query = query.bind(source_url);
    }
    if !anchor.is_empty() {
        query = query.bind(anchor);
    }
    let rows = query.fetch_all(pool).await.context("查询 videos")?;
    let mut out = Vec::new();
    for row in rows {
        out.push(Video {
            id: row.try_get("id")?,
            platform: row.try_get("platform")?,
            url: row.try_get("url")?,
            title: row.try_get("title")?,
            cover: row.try_get("cover")?,
            cover_cache: row.try_get("cover_cache")?,
            published_at: row.try_get("published_at")?,
            duration: row.try_get("duration")?,
            anchor: row.try_get("anchor")?,
            source_url: row.try_get("source_url")?,
            downloaded: row.try_get::<i64, _>("downloaded")? != 0,
            downloaded_at: row.try_get("downloaded_at")?,
        });
    }
    Ok(out)
}

#[allow(dead_code)] // daemon 下载队列入队接口
pub async fn enqueue_task(
    pool: &SqlitePool,
    kind: &str,
    url: &str,
    name: &str,
) -> anyhow::Result<i64> {
    // 同一 kind+url 已有未完成（pending/failed 也会重试）任务时不再重复入队，
    // 避免 daemon 并发/重复下载同一资源。
    let existing: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM tasks WHERE kind=? AND url=? AND status IN ('pending','failed') LIMIT 1",
    )
    .bind(kind)
    .bind(url)
    .fetch_optional(pool)
    .await
    .context("查询已有下载任务")?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let result =
        sqlx::query("INSERT INTO tasks (kind, url, name, status) VALUES (?, ?, ?, 'pending')")
            .bind(kind)
            .bind(url)
            .bind(name)
            .execute(pool)
            .await
            .context("写入 tasks")?;
    Ok(result.last_insert_rowid())
}

pub async fn list_tasks(pool: &SqlitePool, status: &str, limit: i64) -> anyhow::Result<Vec<Task>> {
    let mut sql = String::from(
        "SELECT id, kind, url, name, status, attempts, payload, created_at, updated_at FROM tasks WHERE 1=1",
    );
    if !status.is_empty() {
        sql.push_str(" AND status = ?");
    }
    sql.push_str(" ORDER BY id LIMIT ?");
    let mut query = sqlx::query(&sql);
    if !status.is_empty() {
        query = query.bind(status);
    }
    query = query.bind(limit);
    let rows = query.fetch_all(pool).await.context("查询 tasks")?;
    let mut out = Vec::new();
    for row in rows {
        out.push(Task {
            id: row.try_get("id")?,
            kind: row.try_get("kind")?,
            url: row.try_get("url")?,
            name: row.try_get("name")?,
            status: row.try_get("status")?,
            attempts: row.try_get("attempts")?,
            payload: row.try_get("payload")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        });
    }
    Ok(out)
}

pub async fn update_task_status(pool: &SqlitePool, id: i64, status: &str) -> anyhow::Result<()> {
    sqlx::query("UPDATE tasks SET status=?, attempts=attempts+1, updated_at=datetime('now','localtime') WHERE id=?")
        .bind(status)
        .bind(id)
        .execute(pool)
        .await
        .context("更新任务状态")?;
    Ok(())
}

pub async fn add_notify(
    pool: &SqlitePool,
    event: &str,
    anchor: &str,
    title: &str,
    content: &str,
    channel: &str,
) -> anyhow::Result<i64> {
    let result = sqlx::query(
        "INSERT INTO notify (event, anchor, title, content, channel, status) VALUES (?,?,?,?,?,'pending')",
    )
    .bind(event)
    .bind(anchor)
    .bind(title)
    .bind(content)
    .bind(channel)
    .execute(pool)
    .await
    .context("写入 notify")?;
    Ok(result.last_insert_rowid())
}

#[allow(dead_code)] // 通知审计/重发接口
pub async fn list_notify(
    pool: &SqlitePool,
    status: &str,
    limit: i64,
) -> anyhow::Result<Vec<NotifyEvent>> {
    let mut sql = String::from(
        "SELECT id, event, anchor, title, content, channel, status, created_at FROM notify WHERE 1=1",
    );
    if !status.is_empty() {
        sql.push_str(" AND status = ?");
    }
    sql.push_str(" ORDER BY id DESC LIMIT ?");
    let mut query = sqlx::query(&sql);
    if !status.is_empty() {
        query = query.bind(status);
    }
    query = query.bind(limit);
    let rows = query.fetch_all(pool).await.context("查询 notify")?;
    let mut out = Vec::new();
    for row in rows {
        out.push(NotifyEvent {
            id: row.try_get("id")?,
            event: row.try_get("event")?,
            anchor: row.try_get("anchor")?,
            title: row.try_get("title")?,
            content: row.try_get("content")?,
            channel: row.try_get("channel")?,
            status: row.try_get("status")?,
            created_at: row.try_get("created_at")?,
        });
    }
    Ok(out)
}

pub async fn mark_notify_done(pool: &SqlitePool, id: i64) -> anyhow::Result<()> {
    sqlx::query("UPDATE notify SET status='sent' WHERE id=?")
        .bind(id)
        .execute(pool)
        .await
        .context("更新通知状态")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::VideoMeta;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DB_SEQ: AtomicU64 = AtomicU64::new(0);

    async fn test_pool() -> SqlitePool {
        let seq = TEST_DB_SEQ.fetch_add(1, Ordering::Relaxed);
        let file = std::env::temp_dir().join(format!(
            "zunzhe-test-{}-{}-{}.db",
            std::process::id(),
            seq,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&file)
            .create_if_missing(true);
        let pool = SqlitePool::connect_with(options).await.unwrap();
        init_db(&pool).await.unwrap();
        // 测试库文件交由 OS 临时目录回收，避免并发删除正在使用的 SQLite 文件。
        std::mem::forget(file);
        pool
    }

    #[tokio::test]
    async fn video_library_crud() {
        let pool = test_pool().await;
        let meta = VideoMeta {
            id: "BV1abc".into(),
            platform: "bilibili".into(),
            url: "https://www.bilibili.com/video/BV1abc".into(),
            title: "测试视频".into(),
            cover: "https://example.com/a.jpg".into(),
            published_at: "2026-08-12".into(),
            duration: 60,
        };
        upsert_video(
            &pool,
            &meta,
            "影视飓风",
            "https://space.bilibili.com/946974",
            "",
        )
        .await
        .unwrap();
        assert!(!is_downloaded(&pool, "BV1abc").await.unwrap());
        mark_downloaded(&pool, "BV1abc", true).await.unwrap();
        assert!(is_downloaded(&pool, "BV1abc").await.unwrap());
        let list = get_videos(&pool, "https://space.bilibili.com/946974", "", false)
            .await
            .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "测试视频");
        assert!(list[0].downloaded);
        let done = get_videos(&pool, "", "", true).await.unwrap();
        assert_eq!(done.len(), 1);
    }

    #[tokio::test]
    async fn tasks_and_notify_persist() {
        let pool = test_pool().await;
        let id = enqueue_task(&pool, "download", "https://x.test/v", "博主")
            .await
            .unwrap();
        let dup = enqueue_task(&pool, "download", "https://x.test/v", "博主2")
            .await
            .unwrap();
        assert_eq!(dup, id, "同 URL 未完成下载任务不应重复入队");
        let tasks = list_tasks(&pool, "pending", 10).await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, id);
        update_task_status(&pool, id, "done").await.unwrap();
        assert_eq!(list_tasks(&pool, "pending", 10).await.unwrap().len(), 0);

        let nid = add_notify(&pool, "live", "博主", "开播", "内容", "邮箱")
            .await
            .unwrap();
        assert_eq!(list_notify(&pool, "pending", 10).await.unwrap().len(), 1);
        mark_notify_done(&pool, nid).await.unwrap();
        assert_eq!(list_notify(&pool, "pending", 10).await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn ensure_video_row_keeps_existing_fields() {
        let pool = test_pool().await;
        let meta = VideoMeta {
            id: "BVkeep".into(),
            platform: "bilibili".into(),
            url: "https://www.bilibili.com/video/BVkeep".into(),
            title: "原标题".into(),
            cover: "https://example.com/keep.jpg".into(),
            published_at: "2026-08-12".into(),
            duration: 60,
        };
        upsert_video(&pool, &meta, "博主", "https://space.bilibili.com/1", "")
            .await
            .unwrap();
        ensure_video_row(&pool, "BVkeep", "https://x", "新标题", "新主播")
            .await
            .unwrap();
        let rows = get_videos(&pool, "", "", false).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "原标题");
        assert_eq!(rows[0].anchor, "博主");

        ensure_video_row(&pool, "BVnew", "https://x", "新标题", "新主播")
            .await
            .unwrap();
        let rows = get_videos(&pool, "", "", false).await.unwrap();
        assert_eq!(rows.len(), 2);
        let new_row = rows.iter().find(|r| r.id == "BVnew").unwrap();
        assert_eq!(new_row.title, "新标题");
        assert_eq!(new_row.anchor, "新主播");
    }

    #[tokio::test]
    async fn monitor_baseline_persists() {
        let pool = test_pool().await;
        let url = "https://space.bilibili.com/946974";
        assert!(!is_source_baselined(&pool, url).await.unwrap());
        mark_source_baselined(&pool, url).await.unwrap();
        assert!(is_source_baselined(&pool, url).await.unwrap());
    }

    #[tokio::test]
    async fn live_sessions_persist_and_end() {
        let pool = test_pool().await;
        let id = start_live_session(
            &pool,
            "https://live.bilibili.com/320",
            "主播",
            "测试直播",
            "bilibili",
            "https://stream.example/live.m3u8",
            123,
            "/tmp/x.ts",
        )
        .await
        .unwrap();
        let active = active_live_session(&pool, "https://live.bilibili.com/320")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(active.id, id);
        assert_eq!(active.status, "recording");
        end_live_session(&pool, id, "done", "").await.unwrap();
        assert!(
            active_live_session(&pool, "https://live.bilibili.com/320")
                .await
                .unwrap()
                .is_none()
        );
        let sessions = list_live_sessions(&pool, "done", 10).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].error, "");
    }
}
