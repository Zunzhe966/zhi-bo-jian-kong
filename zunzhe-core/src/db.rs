use anyhow::Context;
use serde::Serialize;
use sqlx::sqlite::SqlitePool;
use sqlx::Row;

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
"#;

#[derive(Debug, Clone, Serialize)]
pub struct Anchor {
    pub id: i64,
    pub url: String,
    pub name: String,
    pub platform: String,
    pub enabled: bool,
    pub live: bool,
    pub notify_channel: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Video {
    pub id: String,
    pub platform: String,
    pub url: String,
    pub title: String,
    pub cover: String,
    pub cover_cache: String,
    pub published_at: String,
    pub duration: i64,
    pub anchor: String,
    pub source_url: String,
    pub downloaded: bool,
}

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

pub async fn remove_anchor(pool: &SqlitePool, url: &str) -> anyhow::Result<bool> {
    let result = sqlx::query("DELETE FROM anchors WHERE url = ?")
        .bind(url)
        .execute(pool)
        .await
        .context("删除 anchors")?;
    Ok(result.rows_affected() > 0)
}

pub async fn list_anchors(pool: &SqlitePool) -> anyhow::Result<Vec<Anchor>> {
    let rows = sqlx::query("SELECT id, url, name, platform, enabled, live, notify_channel FROM anchors ORDER BY id")
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
        });
    }
    Ok(out)
}

pub async fn enqueue_task(
    pool: &SqlitePool,
    kind: &str,
    url: &str,
    name: &str,
) -> anyhow::Result<i64> {
    let result = sqlx::query("INSERT INTO tasks (kind, url, name, status) VALUES (?, ?, ?, 'pending')")
        .bind(kind)
        .bind(url)
        .bind(name)
        .execute(pool)
        .await
        .context("写入 tasks")?;
    Ok(result.last_insert_rowid())
}
