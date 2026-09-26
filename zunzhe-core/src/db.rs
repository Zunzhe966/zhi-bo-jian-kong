use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::Context;
use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqliteSynchronous};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};

use crate::model::{Anchor, LiveSession, NotifyEvent, Task, Video};
use crate::watchlist::Watchlist;

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
    anchor_id     TEXT NOT NULL DEFAULT '',
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
    attempts   INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now', 'localtime'))
);

CREATE TABLE IF NOT EXISTS tasks (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    kind       TEXT NOT NULL DEFAULT '',
    url        TEXT NOT NULL DEFAULT '',
    name       TEXT NOT NULL DEFAULT '',
    anchor_id  TEXT NOT NULL DEFAULT '',
    category   TEXT NOT NULL DEFAULT '短视频',
    status     TEXT NOT NULL DEFAULT 'pending',
    attempts      INTEGER NOT NULL DEFAULT 0,
    payload       TEXT NOT NULL DEFAULT '',
    last_error    TEXT NOT NULL DEFAULT '',
    next_retry_at TEXT NOT NULL DEFAULT '',
    created_at    TEXT NOT NULL DEFAULT (datetime('now', 'localtime')),
    updated_at    TEXT NOT NULL DEFAULT (datetime('now', 'localtime'))
);

CREATE TABLE IF NOT EXISTS monitor_baseline (
    source_url       TEXT PRIMARY KEY,
    baseline_at      TEXT NOT NULL DEFAULT (datetime('now', 'localtime')),
    updated_at       TEXT NOT NULL DEFAULT (datetime('now', 'localtime')),
    consecutive_fails INTEGER NOT NULL DEFAULT 0,
    last_failed_at    TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS live_sessions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    url        TEXT NOT NULL,
    anchor     TEXT NOT NULL DEFAULT '',
    anchor_id  TEXT NOT NULL DEFAULT '',
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

CREATE TABLE IF NOT EXISTS live_states (
    url               TEXT PRIMARY KEY,
    is_live           INTEGER NOT NULL DEFAULT 0,
    consecutive_fails INTEGER NOT NULL DEFAULT 0,
    updated_at        TEXT NOT NULL DEFAULT (datetime('now', 'localtime'))
);

CREATE TABLE IF NOT EXISTS mcp_calls (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    tool              TEXT NOT NULL DEFAULT '',
    risk              TEXT NOT NULL DEFAULT 'unknown',
    arguments_summary TEXT NOT NULL DEFAULT '',
    success           INTEGER NOT NULL DEFAULT 0,
    error             TEXT NOT NULL DEFAULT '',
    duration_ms       INTEGER NOT NULL DEFAULT 0,
    created_at        TEXT NOT NULL DEFAULT (datetime('now', 'localtime'))
);
CREATE INDEX IF NOT EXISTS idx_mcp_calls_created ON mcp_calls(id DESC);
"#;

pub async fn init_db(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(SCHEMA)
        .execute(pool)
        .await
        .context("初始化 SQLite schema")?;
    // 旧版本 live_states 没有 consecutive_fails 列：CREATE TABLE IF NOT EXISTS
    // 不会给已存在的表补列，必须先迁移，否则失败计数读写会直接报错。
    let has_failures = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('live_states') WHERE name='consecutive_fails'",
    )
    .fetch_one(pool)
    .await
    .context("检查 live_states 失败计数字段")?;
    if has_failures == 0 {
        sqlx::query(
            "ALTER TABLE live_states ADD COLUMN consecutive_fails INTEGER NOT NULL DEFAULT 0",
        )
        .execute(pool)
        .await
        .context("迁移 live_states 失败计数字段")?;
    }
    // 旧版本 notify 没有 attempts 列：通知失败重试需要计数上限，缺列时补上。
    let has_notify_attempts = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('notify') WHERE name='attempts'",
    )
    .fetch_one(pool)
    .await
    .context("检查 notify 重试计数字段")?;
    if has_notify_attempts == 0 {
        sqlx::query("ALTER TABLE notify ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0")
            .execute(pool)
            .await
            .context("迁移 notify 重试计数字段")?;
    }
    // 旧版本 tasks 没有结构化失败字段。只有 tracing 文本时，界面和 MCP 无法
    // 解释“点过了为什么没结果”，也不能安全地做重试/清理；启动时补齐列。
    for (column, ddl) in [
        (
            "last_error",
            "ALTER TABLE tasks ADD COLUMN last_error TEXT NOT NULL DEFAULT ''",
        ),
        (
            "next_retry_at",
            "ALTER TABLE tasks ADD COLUMN next_retry_at TEXT NOT NULL DEFAULT ''",
        ),
    ] {
        let exists = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name=?",
        )
        .bind(column)
        .fetch_one(pool)
        .await
        .with_context(|| format!("检查 tasks.{column} 字段"))?;
        if exists == 0 {
            sqlx::query(ddl)
                .execute(pool)
                .await
                .with_context(|| format!("迁移 tasks.{column} 字段"))?;
        }
    }

    // 旧版本 tasks 没有 category 列：下载产物按 <主播>/<分类>（分类 ∈
    // 短视频/批量下载）分目录，任务需携带 category 才能落到正确目录。
    let has_category = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name='category'",
    )
    .fetch_one(pool)
    .await
    .context("检查 tasks.category 字段")?;
    if has_category == 0 {
        sqlx::query("ALTER TABLE tasks ADD COLUMN category TEXT NOT NULL DEFAULT '短视频'")
            .execute(pool)
            .await
            .context("迁移 tasks.category 字段")?;
    }

    // 旧版本 monitor_baseline 没有失败计数/冷却时间：CREATE TABLE IF NOT EXISTS
    // 不会给已存在的表补列，必须先迁移，否则视频源冷却读写会直接报错。
    let has_source_fails = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('monitor_baseline') WHERE name='consecutive_fails'",
    )
    .fetch_one(pool)
    .await
    .context("检查监控源失败计数字段")?;
    if has_source_fails == 0 {
        sqlx::query(
            "ALTER TABLE monitor_baseline ADD COLUMN consecutive_fails INTEGER NOT NULL DEFAULT 0",
        )
        .execute(pool)
        .await
        .context("迁移监控源失败计数字段")?;
    }
    let has_source_failed_at = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('monitor_baseline') WHERE name='last_failed_at'",
    )
    .fetch_one(pool)
    .await
    .context("检查监控源失败时间字段")?;
    if has_source_failed_at == 0 {
        sqlx::query(
            "ALTER TABLE monitor_baseline ADD COLUMN last_failed_at TEXT NOT NULL DEFAULT ''",
        )
        .execute(pool)
        .await
        .context("迁移监控源失败时间字段")?;
    }
    // 选项 A：Anchor 一等公民 —— 给 videos / tasks / live_sessions 增加 anchor_id 列，
    // 作为贯穿采集→转写→产物树的稳定主播身份（当前稳定身份 = 主播名）。
    for (table, ddl) in [
        (
            "videos",
            "ALTER TABLE videos ADD COLUMN anchor_id TEXT NOT NULL DEFAULT ''",
        ),
        (
            "tasks",
            "ALTER TABLE tasks ADD COLUMN anchor_id TEXT NOT NULL DEFAULT ''",
        ),
        (
            "live_sessions",
            "ALTER TABLE live_sessions ADD COLUMN anchor_id TEXT NOT NULL DEFAULT ''",
        ),
    ] {
        let exists = sqlx::query_scalar::<_, i64>(
            &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name='anchor_id'"),
        )
        .fetch_one(pool)
        .await
        .with_context(|| format!("检查 {table}.anchor_id 字段"))?;
        if exists == 0 {
            sqlx::query(ddl)
                .execute(pool)
                .await
                .with_context(|| format!("迁移 {table}.anchor_id 字段"))?;
        }
    }
    Ok(())
}

/// 打开生产 SQLite 连接：WAL 允许 daemon 内多个并发任务边写边读，
/// busy_timeout 让短锁冲突等待而不是立刻报错，保证 24×30 长时间运行稳定。
pub fn open_options(path: &Path) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(std::time::Duration::from_secs(15))
        .foreign_keys(true)
}

/// 生产池显式连接数上限与等待上限。
///
/// 不依赖 sqlx 默认值：显式写死避免依赖升级时默认连接数悄悄变化，也让
/// daemon 与 CLI/MCP 同时打开时不会无限放大 SQLite 文件句柄。SQLite 单写者
/// 模型下并发上限收益有限，8 个连接足够覆盖 tick/直播/下载/通知等并发任务。
pub const POOL_MAX_CONNECTIONS: u32 = 8;
pub const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(15);

/// 供测试/调用方直接建立 SQLite 池。
pub async fn open_pool(path: &Path) -> anyhow::Result<SqlitePool> {
    let pool = SqlitePoolOptions::new()
        .max_connections(POOL_MAX_CONNECTIONS)
        .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
        .connect_with(open_options(path))
        .await
        .context("连接 SQLite 数据库")?;
    ensure_wal_mode(&pool).await?;
    init_db(&pool).await?;
    Ok(pool)
}

/// 让已有库保持 WAL，但避免每个物理连接都重复执行会争抢排他锁的
/// `PRAGMA journal_mode = WAL`。journal_mode 是数据库持久属性：首次打开时
/// 检查一次即可，之后的连接会直接继承 WAL。
async fn ensure_wal_mode(pool: &SqlitePool) -> anyhow::Result<()> {
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(pool)
        .await
        .context("读取 SQLite journal mode")?;
    if !mode.eq_ignore_ascii_case("wal") {
        sqlx::query("PRAGMA journal_mode = WAL")
            .execute(pool)
            .await
            .context("启用 SQLite WAL")?;
    }
    Ok(())
}

/// 独立 CLI/MCP/看门狗入口的数据库维护节流。
///
/// 单靠 daemon 主循环每 6 小时维护会在以下场景漏掉：MCP server 长驻且
/// daemon 未运行、CLI 反复单独调用、桌面端看门狗只调 `doctor`。本函数在
/// 每次调用时检查 `fupan_output/run/zunzhe-core.maintenance` 的修改时间，
/// 超过 6 小时才执行一次，避免每个 CLI 进程都重复跑 checkpoint/清理。
pub async fn maintenance_if_due(project_dir: &Path, pool: &SqlitePool) -> anyhow::Result<bool> {
    let run_dir = crate::paths::run_dir(project_dir);
    let marker = run_dir.join("zunzhe-core.maintenance");
    if maintenance_marker_age(&marker).is_some_and(|age| age < MAINTENANCE_INTERVAL) {
        return Ok(false);
    }
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 先执行维护，成功后刷新哨兵：失败时下次调用可再次尝试，不会因为一次
    // checkpoint/清理失败就静默跳过 6 小时。
    let result = maintenance(pool).await;
    match &result {
        Ok(()) => {
            // daemon 不在场时也要清理运行目录里的协议/原子写残留，否则
            // 长年只跑 CLI/MCP 的机器会让 fupan_output/run 无限增长。
            crate::bilibili_electron::maintain_run_dir(&run_dir);
            // 旧核心 DB 迁移曾把视频空间地址写入 anchors；只有 URL_config.ini
            // 是直播源权威，其他历史记录会让 list-live 展示假主播，逐次清理。
            if let Ok(removed) = prune_orphan_anchors(pool, project_dir).await
                && removed > 0
            {
                tracing::info!("清理 {removed} 条不在直播源里的历史 anchors 记录");
            }
            if let Ok(rewritten) = rewrite_legacy_task_paths(pool, project_dir).await
                && rewritten > 0
            {
                tracing::info!("改写 {rewritten} 条指向旧桌面根的任务路径");
            }
            touch_maintenance_marker(&run_dir)?;
            tracing::debug!("独立入口数据库维护完成");
        }
        Err(e) => tracing::warn!("独立入口数据库维护失败: {e:#}"),
    }
    result.map(|_| true)
}

/// 哨兵文件内容是 RFC3339 时间戳；缺失/损坏视为“已到期”，让维护能自动恢复。
fn maintenance_marker_age(marker: &Path) -> Option<Duration> {
    let text = std::fs::read_to_string(marker).ok()?;
    let ts = chrono::DateTime::parse_from_rfc3339(text.trim()).ok()?;
    let secs = ts.timestamp();
    let secs = u64::try_from(secs).ok()?;
    Some(Duration::from_secs(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()?
            .as_secs()
            .saturating_sub(secs),
    ))
}

/// 读取维护哨兵的时间戳（Unix 秒）；缺失/损坏返回 `None`。
///
/// daemon 启动时用真实哨兵恢复维护计时，避免“重启即重置”让 6 小时周期
/// 永远推迟；CLI/MCP 的 `maintenance_if_due` 与 daemon 主循环共用同一哨兵，
/// 不能各记各的。
pub fn maintenance_marker_timestamp(marker: &Path) -> Option<i64> {
    let text = std::fs::read_to_string(marker).ok()?;
    let ts = chrono::DateTime::parse_from_rfc3339(text.trim()).ok()?;
    Some(ts.timestamp())
}

/// 原子写入维护哨兵，供 daemon 主循环与 CLI/MCP 共用。
pub fn touch_maintenance_marker(run_dir: &Path) -> anyhow::Result<()> {
    let marker = run_dir.join("zunzhe-core.maintenance");
    let tmp = run_dir.join("zunzhe-core.maintenance.tmp");
    std::fs::write(&tmp, chrono::Local::now().to_rfc3339())?;
    std::fs::rename(&tmp, &marker)?;
    Ok(())
}

/// daemon 主循环与独立入口共用的维护周期（6 小时）。
pub const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(6 * 3600);

/// 测试专用：把维护哨兵设为刚写入，使 `maintenance_if_due` 在测试里可重复触发。
#[cfg(test)]
pub fn force_maintenance_due_for_test(project_dir: &Path) {
    let marker = project_dir
        .join("fupan_output")
        .join("run")
        .join("zunzhe-core.maintenance");
    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&marker, "2000-01-01T00:00:00+00:00").unwrap();
}

/// 周期性 SQLite 维护：WAL checkpoint 把 wal 落回主库并截断，`PRAGMA optimize`
/// 让 SQLite 根据查询使用情况维护统计信息。24×30 运行中每 6 小时调用一次，
/// 防止 WAL/查询计划长期膨胀；失败只记日志，不阻塞主循环。
pub async fn maintenance(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(pool)
        .await
        .context("WAL checkpoint 失败")?;
    sqlx::query("PRAGMA optimize")
        .execute(pool)
        .await
        .context("SQLite optimize 失败")?;
    // 24×30 长跑审计：通知表/任务表只保留最近 200 条。旧 Python 桌面端本地
    // 队列只保留 30 条，SQLite 审计行也应有限额；失败/历史通知不再需要时
    // 不应让数据库和界面列表无限膨胀。保留值取大一些，避免用户刚好需要
    // 查更早失败原因时被过早清理。
    // 注意：pending/processing 是还没执行完的工作，无论 id 多旧都不能删，
    // 否则维护后队列会静默丢任务，用户以为会下载/复盘却永远不做。
    sqlx::query(
        "DELETE FROM notify WHERE status IN ('sent','failed') AND id NOT IN
         (SELECT id FROM notify WHERE status IN ('sent','failed') ORDER BY id DESC LIMIT 200)",
    )
    .execute(pool)
    .await
    .context("清理通知表")?;
    sqlx::query(
        "DELETE FROM tasks WHERE status IN ('done','failed') AND id NOT IN
         (SELECT id FROM tasks WHERE status IN ('done','failed') ORDER BY id DESC LIMIT 200)",
    )
    .execute(pool)
    .await
    .context("清理任务表")?;
    // 直播录制历史表：终态（done/failed/stopped/interrupted）只保留最近
    // 500 条；recording 是活动中的会话，无论 id 多旧都不能删，否则维护后
    // 正在录制的会话会从状态机里消失，崩溃恢复/停止录制都会失效。
    sqlx::query(
        "DELETE FROM live_sessions WHERE status != 'recording' AND id NOT IN
         (SELECT id FROM live_sessions WHERE status != 'recording' ORDER BY id DESC LIMIT 500)",
    )
    .execute(pool)
    .await
    .context("清理直播录制历史表")?;
    // MCP 调用审计表：每次工具调用落一行，MCP 被高频使用时同样会无上限增长。
    // 这里与 notify/tasks 一样限额清理（保留最近 500 条，比工作队列多留一些
    // 以便回溯）；mcp_calls 是纯审计、没有「未完成」状态，删旧行是安全的。
    sqlx::query(
        "DELETE FROM mcp_calls WHERE id NOT IN
         (SELECT id FROM mcp_calls ORDER BY id DESC LIMIT 500)",
    )
    .execute(pool)
    .await
    .context("清理 MCP 调用审计表")?;
    Ok(())
}

/// 把任务表里指向旧桌面项目根的历史路径改写到当前产物路径。
///
/// 项目从 `~/Desktop/尊者监控直播短视频` 迁到 `~/Projects/尊者监控直播短视频`
/// 后，旧的 `process` 任务 URL 仍指向桌面副本；文件其实已经随项目一起移动，
/// 继续引用旧根会让“已复盘/已下载”记录与实际位置漂移，手动重试也会打开
/// 不存在或过期的路径。只改前缀，不删任务、不改状态。
///
/// `downloads/` 与 `fupan_output/` 这两段前缀必须按**当前配置**重定位，而不是
/// 一律拼到项目根：产物根被改到别的目录（`[路径设置] 产物根目录`）之后，
/// 历史任务里的 `.../downloads/x.mp4` 应该落到新的下载目录，否则改写出来的
/// 路径是一个不存在的旧位置，比不改还糟。
pub async fn rewrite_legacy_task_paths(
    pool: &SqlitePool,
    project_dir: &Path,
) -> anyhow::Result<usize> {
    let dirs = crate::paths::storage(project_dir);
    let home = std::env::var("HOME").unwrap_or_default();
    let legacy_prefixes = [
        std::path::Path::new(&home).join("Desktop/尊者监控直播短视频"),
        std::path::Path::new(&home).join("Projects/尊者监控直播短视频"),
    ];
    let mut rows = Vec::new();
    for legacy_root in &legacy_prefixes {
        let Some(prefix) = legacy_root.to_str() else {
            continue;
        };
        rows.extend(
            sqlx::query("SELECT id, url FROM tasks WHERE url LIKE ?")
                .bind(format!("{prefix}%"))
                .fetch_all(pool)
                .await
                .context("读取旧路径任务")?,
        );
    }
    let mut changed = 0usize;
    for row in rows {
        let id: i64 = row.try_get("id")?;
        let url: String = row.try_get("url")?;
        let Some(rest) = legacy_prefixes
            .iter()
            .find_map(|prefix| url.strip_prefix(prefix.to_str()?))
        else {
            continue;
        };
        let rest = rest.trim_start_matches('/');
        // 目标目录可能尚不存在（自定义产物根、下载目录还没建），所以用宽松规范化：
        // 直接 canonicalize 会失败并退回原样，写出未解析的 `/var` 而非 `/private/var`。
        let new_url = if let Some(tail) = rest.strip_prefix("downloads/") {
            crate::paths::canonical_lenient(&dirs.downloads).join(tail)
        } else if let Some(tail) = rest.strip_prefix("fupan_output/") {
            crate::paths::canonical_lenient(&dirs.fupan).join(tail)
        } else {
            crate::paths::canonical_lenient(&dirs.project_dir).join(rest)
        };
        let new_url = new_url.to_string_lossy().to_string();
        if new_url == url {
            continue;
        }
        sqlx::query("UPDATE tasks SET url=?, updated_at=datetime('now','localtime') WHERE id=?")
            .bind(&new_url)
            .bind(id)
            .execute(pool)
            .await
            .context("改写任务旧路径")?;
        changed += 1;
    }
    Ok(changed)
}

/// 清理历史冗余的 anchors：`anchors` 表只用于兼容旧核心 DB 迁移展示，
/// 直播监控的唯一权威是 `URL_config.ini`。旧库曾把视频空间地址也写入
/// anchors（例如影视飓风 `space.bilibili.com/...`），这些记录不在直播源里，
/// 会让 `list-live` 把没监控直播的博主也当成直播主播。
pub async fn prune_orphan_anchors(pool: &SqlitePool, project_dir: &Path) -> anyhow::Result<usize> {
    let watch = Watchlist::new(project_dir.to_path_buf());
    let live_urls: std::collections::HashSet<String> = watch
        .list_live()
        .into_iter()
        .map(|item| item.url.trim_end_matches('/').to_string())
        .collect();
    let rows = sqlx::query("SELECT id, url FROM anchors")
        .fetch_all(pool)
        .await
        .context("读取 anchors 以清理孤儿记录")?;
    let mut removed = 0usize;
    for row in rows {
        let id: i64 = row.try_get("id")?;
        let url: String = row.try_get("url")?;
        let normalized = url.trim_end_matches('/').to_string();
        if !live_urls.contains(&normalized) {
            sqlx::query("DELETE FROM anchors WHERE id = ?")
                .bind(id)
                .execute(pool)
                .await
                .context("删除孤儿 anchors 记录")?;
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod maintenance_tests {
    use super::*;

    #[tokio::test]
    async fn prune_orphan_anchors_keeps_only_live_sources() {
        let project = std::env::temp_dir().join(format!(
            "zunzhe-prune-anchors-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&project);
        std::fs::create_dir_all(project.join("config")).unwrap();
        let db = project.join("zunzhe-test.db");
        let pool = open_pool(&db).await.unwrap();
        crate::db::add_anchor(&pool, "https://live.bilibili.com/123", "直播主播")
            .await
            .unwrap();
        crate::db::add_anchor(&pool, "https://space.bilibili.com/946974", "旧库视频源")
            .await
            .unwrap();
        std::fs::write(
            project.join("config").join("URL_config.ini"),
            "https://live.bilibili.com/123,直播主播\n",
        )
        .unwrap();

        let removed = crate::db::prune_orphan_anchors(&pool, &project)
            .await
            .unwrap();
        assert_eq!(removed, 1, "不在直播源里的历史 anchors 应被清理");
        let anchors = crate::db::list_anchors(&pool).await.unwrap();
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].url, "https://live.bilibili.com/123");
        std::mem::forget(db);
        let _ = std::fs::remove_dir_all(&project);
    }

    #[tokio::test]
    async fn rewrite_legacy_task_paths_points_to_current_project() {
        let project = crate::test_support::isolated_project("rewrite-path");
        std::fs::create_dir_all(crate::paths::run_dir(&project)).unwrap();
        let db = project.join("zunzhe-test.db");
        let pool = open_pool(&db).await.unwrap();

        let home = std::env::var("HOME").unwrap();
        let legacy = format!("{home}/Desktop/尊者监控直播短视频/downloads/短视频/主播/视频.mp4");
        crate::db::enqueue_task(&pool, "process", &legacy, "主播")
            .await
            .unwrap();

        let changed = crate::db::rewrite_legacy_task_paths(&pool, &project)
            .await
            .unwrap();
        assert_eq!(changed, 1);
        let tasks = crate::db::list_tasks(&pool, "pending", 10).await.unwrap();
        assert_eq!(tasks.len(), 1);
        let url = &tasks[0].url;
        let expected = crate::paths::canonical_lenient(&crate::paths::storage(&project).downloads)
            .join("短视频")
            .join("主播")
            .join("视频.mp4");
        assert_eq!(
            Path::new(url),
            expected,
            "任务路径应改写为配置的下载目录: {url}"
        );
        assert!(!url.contains("Desktop/尊者监控直播短视频"));
        std::mem::forget(db);
        let _ = std::fs::remove_dir_all(&project);
    }

    #[tokio::test]
    async fn rewrite_legacy_task_paths_covers_projects_symlink_root() {
        let project = crate::test_support::isolated_project("rewrite-projects-path");
        std::fs::create_dir_all(crate::paths::run_dir(&project)).unwrap();
        let db = project.join("zunzhe-test.db");
        let pool = open_pool(&db).await.unwrap();

        let home = std::env::var("HOME").unwrap();
        let legacy = format!("{home}/Projects/尊者监控直播短视频/downloads/短视频/主播/视频.mp4");
        crate::db::enqueue_task(&pool, "process", &legacy, "主播")
            .await
            .unwrap();

        let changed = crate::db::rewrite_legacy_task_paths(&pool, &project)
            .await
            .unwrap();
        assert_eq!(changed, 1, "Projects 软链根的旧任务路径也必须迁移");
        let tasks = crate::db::list_tasks(&pool, "pending", 10).await.unwrap();
        let expected = crate::paths::canonical_lenient(&crate::paths::storage(&project).downloads)
            .join("短视频")
            .join("主播")
            .join("视频.mp4");
        assert_eq!(Path::new(&tasks[0].url), expected);
        std::mem::forget(db);
        let _ = std::fs::remove_dir_all(&project);
    }

    #[tokio::test]
    async fn maintenance_checkpoints_wal_and_optimize() {
        let file = std::env::temp_dir().join(format!(
            "zunzhe-maintenance-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = open_pool(&file).await.unwrap();
        enqueue_task(&pool, "download", "https://x.test/maint", "博主")
            .await
            .unwrap();
        maintenance(&pool).await.unwrap();
        // maintenance 是幂等操作：重复执行不应报错，且不会破坏已入库数据。
        maintenance(&pool).await.unwrap();
        let tasks = list_tasks(&pool, "pending", 10).await.unwrap();
        assert_eq!(tasks.len(), 1);
        std::mem::forget(file);
    }

    #[tokio::test]
    async fn maintenance_keeps_pending_tasks_and_notify() {
        let file = std::env::temp_dir().join(format!(
            "zunzhe-maintenance-pending-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = open_pool(&file).await.unwrap();
        // 先写 3 条终态任务/通知再写 1 条 pending：清理后 pending 必须保留。
        for i in 0..3 {
            let id = enqueue_task(
                &pool,
                "download",
                &format!("https://x.test/done/{i}"),
                "博主",
            )
            .await
            .unwrap();
            set_task_status(&pool, id, "done").await.unwrap();
            let nid = add_notify(&pool, "video", "博主", "标题", "内容", "邮箱")
                .await
                .unwrap();
            fail_notify(&pool, nid).await.unwrap();
        }
        let pending = enqueue_task(&pool, "download", "https://x.test/pending", "博主")
            .await
            .unwrap();
        let pending_notify = add_notify(&pool, "live", "博主", "开播", "内容", "邮箱")
            .await
            .unwrap();

        maintenance(&pool).await.unwrap();

        assert!(
            list_tasks(&pool, "pending", 10)
                .await
                .unwrap()
                .iter()
                .any(|t| t.id == pending),
            "维护不能删除待处理任务"
        );
        assert!(
            list_notify(&pool, "pending", 10)
                .await
                .unwrap()
                .iter()
                .any(|n| n.id == pending_notify),
            "维护不能删除待发送通知"
        );
        std::mem::forget(file);
    }

    #[tokio::test]
    async fn failed_notify_can_be_retried_and_pruned() {
        let file = std::env::temp_dir().join(format!("zunzhe-notify-mgmt-{}", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let pool = crate::db::open_pool(&file).await.unwrap();
        for _ in 0..3 {
            let id = crate::db::add_notify(&pool, "video", "博主", "标题", "内容", "微信")
                .await
                .unwrap();
            crate::db::fail_notify(&pool, id).await.unwrap();
        }
        let sent = crate::db::add_notify(&pool, "video", "博主", "标题", "内容", "邮箱")
            .await
            .unwrap();
        crate::db::mark_notify_done(&pool, sent).await.unwrap();

        assert_eq!(
            crate::db::list_notify(&pool, "failed", 10)
                .await
                .unwrap()
                .len(),
            3
        );
        let retried = crate::db::retry_failed_notify(&pool, 2).await.unwrap();
        assert_eq!(retried, 2, "只应重试前两条失败通知");
        assert_eq!(
            crate::db::list_pending_notify(&pool, 10)
                .await
                .unwrap()
                .len(),
            2,
            "重试后的通知必须回到 pending 队列"
        );
        assert_eq!(
            crate::db::list_notify(&pool, "failed", 10)
                .await
                .unwrap()
                .len(),
            1
        );

        let pruned = crate::db::prune_notify(&pool, "sent", 0).await.unwrap();
        assert_eq!(pruned, 1, "应能清理已发送记录");
        let retried_all = crate::db::retry_failed_notify(&pool, 0).await.unwrap();
        assert_eq!(retried_all, 1, "limit=0 应表示不限制，重试剩余失败通知");
        assert_eq!(
            crate::db::list_pending_notify(&pool, 10)
                .await
                .unwrap()
                .len(),
            3,
            "三次失败通知都应回到 pending 队列"
        );
        let err = crate::db::prune_notify(&pool, "pending", 10)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("只允许清理"), "pending 不得清理");
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(file.with_extension("db-shm"));
        let _ = std::fs::remove_file(file.with_extension("db-wal"));
    }

    #[tokio::test]
    async fn notify_health_summary_counts_by_status() {
        let file =
            std::env::temp_dir().join(format!("zunzhe-notify-health-{}", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let pool = crate::db::open_pool(&file).await.unwrap();
        for _ in 0..2 {
            let id = crate::db::add_notify(&pool, "video", "博主", "标题", "内容", "微信")
                .await
                .unwrap();
            crate::db::fail_notify(&pool, id).await.unwrap();
        }
        for _ in 0..3 {
            let id = crate::db::add_notify(&pool, "video", "博主", "标题", "内容", "邮箱")
                .await
                .unwrap();
            crate::db::mark_notify_done(&pool, id).await.unwrap();
        }
        let pending = crate::db::add_notify(&pool, "video", "博主", "标题", "内容", "邮箱")
            .await
            .unwrap();
        assert!(pending > 0);

        let (pending_count, failed, sent) = crate::db::notify_health_summary(&pool).await.unwrap();
        assert_eq!(pending_count, 1);
        assert_eq!(failed, 2);
        assert_eq!(sent, 3);
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(file.with_extension("db-shm"));
        let _ = std::fs::remove_file(file.with_extension("db-wal"));
    }

    #[test]
    fn notify_health_hint_points_to_action() {
        let configured = false;
        let hint = crate::db::notify_health_hint(0, 2, configured, Some("微信")).unwrap();
        assert!(hint.contains("Server酱"), "未配置时应提示 Server酱: {hint}");
        assert!(hint.contains("notify-retry"), "未配置时应提示重试: {hint}");

        let hint = crate::db::notify_health_hint(0, 1, true, Some("PushPlus")).unwrap();
        assert!(hint.contains("PushPlus"), "应定位失败通道: {hint}");
        assert!(
            hint.contains("notify-prune"),
            "已配置失败应提示清理: {hint}"
        );

        let hint = crate::db::notify_health_hint(3, 0, true, None).unwrap();
        assert!(hint.contains("等待发送"), "pending 应提示等待/重试: {hint}");

        assert!(crate::db::notify_health_hint(0, 0, true, None).is_none());
        let hint = crate::db::notify_health_hint(0, 0, false, None).unwrap();
        assert!(
            hint.contains("不会发送通知"),
            "未配置空队列也应提示: {hint}"
        );
    }

    #[tokio::test]
    async fn maintenance_prunes_finished_live_sessions_keeps_recording() {
        let file = std::env::temp_dir().join(format!(
            "zunzhe-maintenance-livesess-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = open_pool(&file).await.unwrap();
        // 先写入一个活动中的 recording 会话（必须永远保留）。
        let active = start_live_session(
            &pool,
            "https://live.test/active",
            "主播甲",
            "标题",
            "bilibili",
            "",
            0,
            "",
        )
        .await
        .unwrap();
        // 再写入超过 500 条的终态会话。
        for i in 0..520 {
            let id = start_live_session(
                &pool,
                &format!("https://live.test/{i}"),
                "主播乙",
                "标题",
                "bilibili",
                "",
                0,
                "",
            )
            .await
            .unwrap();
            end_live_session(&pool, id, "done", "").await.unwrap();
        }

        maintenance(&pool).await.unwrap();

        // 活动中的 recording 必须保留。
        assert!(
            list_live_sessions(&pool, "recording", 10)
                .await
                .unwrap()
                .iter()
                .any(|s| s.id == active),
            "维护不能删除正在录制的会话"
        );
        // 终态只保留最近 500 条。
        let done = list_live_sessions(&pool, "done", 1000).await.unwrap();
        assert_eq!(done.len(), 500, "终态录制历史应只保留最近 500 条");
        std::mem::forget(file);
    }
}

#[cfg(test)]
mod opportunistic_maintenance_tests {
    use super::*;

    #[test]
    fn maintenance_marker_timestamp_recovers_after_restart() {
        let project = temp_project("marker-ts");
        let marker = crate::paths::run_dir(&project.0).join("zunzhe-core.maintenance");
        // 哨兵缺省时返回 None：daemon 启动会立即执行一次维护，而不是把计时
        // 重置成“现在”后继续推迟 6 小时。
        assert_eq!(maintenance_marker_timestamp(&marker), None);
        // 写入 3 小时前的哨兵：返回真实时间戳，重启后仍按“已运行 3 小时”恢复。
        let three_hours_ago = (chrono::Local::now() - chrono::Duration::hours(3)).to_rfc3339();
        std::fs::write(&marker, three_hours_ago).unwrap();
        let ts = maintenance_marker_timestamp(&marker).unwrap();
        let now = chrono::Local::now().timestamp();
        assert!(
            now - ts >= 3 * 3600 - 5 && now - ts <= 3 * 3600 + 5,
            "哨兵时间戳应接近写入时刻: now={now} ts={ts}"
        );
        let _ = std::fs::remove_dir_all(&project.0);
    }

    #[test]
    fn touch_maintenance_marker_persists_for_next_restart() {
        let project = temp_project("marker-touch");
        let run_dir = crate::paths::run_dir(&project.0);
        touch_maintenance_marker(&run_dir).unwrap();
        let marker = run_dir.join("zunzhe-core.maintenance");
        assert!(marker.exists(), "维护哨兵应落盘");
        let ts = maintenance_marker_timestamp(&marker).unwrap();
        let now = chrono::Local::now().timestamp();
        assert!(
            (now - ts).abs() <= 5,
            "哨兵时间戳应接近写入时刻: now={now} ts={ts}"
        );
        // 写入后不应留下原子写临时文件。
        assert!(
            !run_dir.join("zunzhe-core.maintenance.tmp").exists(),
            "哨兵写入不应留下 tmp 残留"
        );
        let _ = std::fs::remove_dir_all(&project.0);
    }

    fn temp_project(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let project = crate::test_support::isolated_project(name);
        std::fs::create_dir_all(crate::paths::run_dir(&project)).unwrap();
        let db = project.join("zunzhe-test.db");
        (project, db)
    }

    #[tokio::test]
    async fn maintenance_if_due_runs_and_updates_marker() {
        let (project, db) = temp_project("due");
        let pool = open_pool(&db).await.unwrap();
        enqueue_task(&pool, "download", "https://x.test/due", "博主")
            .await
            .unwrap();
        force_maintenance_due_for_test(&project);
        let ran = maintenance_if_due(&project, &pool).await.unwrap();
        assert!(ran, "哨兵已到期时应执行维护");
        let tasks = list_tasks(&pool, "pending", 10).await.unwrap();
        assert_eq!(tasks.len(), 1, "维护不应删除 pending 任务");
        let marker = crate::paths::run_dir(&project).join("zunzhe-core.maintenance");
        assert!(
            marker.exists(),
            "维护成功后应刷新哨兵，避免每个 CLI 进程都重复 checkpoint"
        );
        // 刚刷新过的哨兵会使下一次调用短路。
        let ran = maintenance_if_due(&project, &pool).await.unwrap();
        assert!(!ran, "6 小时内不应重复执行维护");
        std::mem::forget(db);
        let _ = std::fs::remove_dir_all(&project);
    }

    #[tokio::test]
    async fn maintenance_if_due_does_not_fail_without_daemon() {
        let (project, db) = temp_project("idle");
        let pool = open_pool(&db).await.unwrap();
        let ran = maintenance_if_due(&project, &pool).await.unwrap();
        assert!(ran, "第一次调用无哨兵时应执行维护");
        std::mem::forget(db);
        let _ = std::fs::remove_dir_all(&project);
    }

    #[tokio::test]
    async fn maintenance_if_due_cleans_run_dir_residue() {
        let (project, db) = temp_project("residue");
        let run_dir = crate::paths::run_dir(&project);
        // 模拟旧版本 Electron 中断残留 + 原子写硬崩溃残留。
        let old_payload = run_dir.join("bili_script_payload_stale.js");
        std::fs::write(&old_payload, "old").unwrap();
        set_mtime_days_ago(&old_payload, 8);
        let old_tmp = run_dir.join(".zunzhe-core.heartbeat.tmp.123.456.0");
        std::fs::write(&old_tmp, "stale").unwrap();
        set_mtime_days_ago(&old_tmp, 8);

        let pool = open_pool(&db).await.unwrap();
        let ran = maintenance_if_due(&project, &pool).await.unwrap();
        assert!(ran);
        assert!(!old_payload.exists(), "超龄 Electron payload 应被维护清理");
        assert!(!old_tmp.exists(), "超龄原子写残留应被维护清理");
        std::mem::forget(db);
        let _ = std::fs::remove_dir_all(&project);
    }

    use crate::test_support::set_mtime_days_ago;
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

/// 旧核心 DB 迁移专用：只在主库没有相同 url 时插入，绝不覆盖已有记录。
///
/// 返回 `true` 表示本次确实写入，返回 `false` 表示因主库已存在而跳过。
/// 与 `insert_legacy_video` 一样使用单条 `INSERT OR IGNORE`，幂等且并发安全。
#[allow(clippy::too_many_arguments)] // 对齐旧库 anchors 字段，逐字段复制
pub async fn insert_legacy_anchor(
    tx: &mut sqlx::SqliteConnection,
    url: &str,
    name: &str,
    platform: &str,
    enabled: bool,
    live: bool,
    notify_channel: &str,
) -> anyhow::Result<bool> {
    let result = sqlx::query(
        r#"INSERT OR IGNORE INTO anchors
           (url, name, platform, enabled, live, notify_channel)
           VALUES (?,?,?,?,?,?)"#,
    )
    .bind(url)
    .bind(name)
    .bind(platform)
    .bind(if enabled { 1i64 } else { 0i64 })
    .bind(if live { 1i64 } else { 0i64 })
    .bind(notify_channel)
    .execute(&mut *tx)
    .await
    .context("写入旧核心 DB anchors")?;
    Ok(result.rows_affected() > 0)
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
        r#"INSERT INTO monitor_baseline (source_url, consecutive_fails, last_failed_at)
           VALUES (?, 0, '')
           ON CONFLICT(source_url) DO UPDATE SET
             updated_at=datetime('now','localtime'),
             consecutive_fails=0,
             last_failed_at=''"#,
    )
    .bind(source_url)
    .execute(pool)
    .await
    .context("记录监控源基线")?;
    Ok(())
}

/// 记录一次监控源检查成功：失败计数清零并标记当前已成功时间。
///
/// 成功是风控/网络恢复的明确信号，必须清掉冷却状态，否则一次正常恢复后
/// 还要被旧失败计数再拖几轮。
pub async fn reset_source_failures(pool: &SqlitePool, source_url: &str) -> anyhow::Result<()> {
    sqlx::query(
        r#"UPDATE monitor_baseline SET consecutive_fails=0,
           last_failed_at='',
           updated_at=datetime('now','localtime')
           WHERE source_url=?"#,
    )
    .bind(source_url)
    .execute(pool)
    .await
    .context("清零监控源失败次数")?;
    Ok(())
}

/// 记录一次监控源检查失败/超时，返回当前连续失败次数。
pub async fn bump_source_failures(pool: &SqlitePool, source_url: &str) -> anyhow::Result<u32> {
    sqlx::query(
        r#"INSERT INTO monitor_baseline (source_url, consecutive_fails, last_failed_at)
           VALUES (?, 1, datetime('now','localtime'))
           ON CONFLICT(source_url) DO UPDATE SET
             consecutive_fails = consecutive_fails + 1,
             last_failed_at = datetime('now','localtime'),
             updated_at = datetime('now','localtime')"#,
    )
    .bind(source_url)
    .execute(pool)
    .await
    .context("累计监控源失败次数")?;
    let row: Option<i64> =
        sqlx::query_scalar("SELECT consecutive_fails FROM monitor_baseline WHERE source_url=?")
            .bind(source_url)
            .fetch_optional(pool)
            .await
            .context("查询监控源失败次数")?;
    Ok(row.unwrap_or(0).clamp(0, i64::from(u32::MAX)) as u32)
}

/// 返回 `(连续失败次数, 上次失败 Unix 秒)`；无记录时视为 `(0, 0)`。
pub async fn source_failure_state(
    pool: &SqlitePool,
    source_url: &str,
) -> anyhow::Result<(u32, i64)> {
    let row = sqlx::query(
        "SELECT consecutive_fails, last_failed_at FROM monitor_baseline WHERE source_url=?",
    )
    .bind(source_url)
    .fetch_optional(pool)
    .await
    .context("查询监控源失败状态")?;
    let Some(row) = row else {
        return Ok((0, 0));
    };
    let fails: i64 = row.try_get("consecutive_fails")?;
    let last: String = row.try_get("last_failed_at")?;
    let ts = parse_local_datetime(&last).unwrap_or(0);
    Ok((fails.clamp(0, i64::from(u32::MAX)) as u32, ts))
}

/// 所有视频监控源的健康摘要。
///
/// 24×30 验收不只要求 daemon 进程活着，还要求监控源本身稳定：平台风控或
/// 网络故障会造成源连续失败，进入冷却后该博主的新视频会静默不通知/不下载。
/// `doctor` 与长跑审计据此把“存在连续失败源”当成健康风险，而不是只报
/// 进程/心跳。返回 `(总数, 连续失败数, 冷却中数)`；无记录视为全部健康。
pub async fn source_health_summary(pool: &SqlitePool) -> anyhow::Result<(u64, u64, u64)> {
    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM monitor_baseline WHERE source_url <> ''")
            .fetch_one(pool)
            .await
            .context("统计监控源总数")?;
    let failing: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM monitor_baseline WHERE consecutive_fails > 0 AND source_url <> ''",
    )
    .fetch_one(pool)
    .await
    .context("统计监控源连续失败数")?;
    let cooling: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM monitor_baseline WHERE consecutive_fails >= 3 AND source_url <> ''",
    )
    .fetch_one(pool)
    .await
    .context("统计监控源冷却数")?;
    Ok((
        total.max(0) as u64,
        failing.max(0) as u64,
        cooling.max(0) as u64,
    ))
}

/// 把 SQLite `YYYY-MM-DD HH:MM:SS` 本地时间解析成 Unix 秒。
fn parse_local_datetime(text: &str) -> Option<i64> {
    chrono::NaiveDateTime::parse_from_str(text.trim(), "%Y-%m-%d %H:%M:%S")
        .ok()
        .and_then(|dt| {
            use chrono::TimeZone;
            chrono::Local
                .from_local_datetime(&dt)
                .single()
                .map(|d| d.timestamp())
        })
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
           (url, anchor, anchor_id, title, platform, stream_url, status, pid, file_path)
           VALUES (?,?,?,?,?,?, 'recording', ?, ?)"#,
    )
    .bind(url)
    .bind(anchor)
    .bind(resolve_anchor_id(anchor))
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
           ended_at=datetime('now','localtime'), updated_at=datetime('now','localtime')
           WHERE id=? AND status='recording'"#,
    )
    .bind(status)
    .bind(error)
    .bind(id)
    .execute(pool)
    .await
    .context("结束直播录制会话")?;
    Ok(())
}

/// 查询某次录制会话是否仍处于 recording（后台收尾前确认，避免覆盖已停止状态）。
pub async fn live_session_recording(pool: &SqlitePool, id: i64) -> anyhow::Result<bool> {
    let row: Option<i64> =
        sqlx::query_scalar("SELECT id FROM live_sessions WHERE id=? AND status='recording'")
            .bind(id)
            .fetch_optional(pool)
            .await
            .context("查询录制会话状态")?;
    Ok(row.is_some())
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

/// 读取某主播上一轮的直播状态，用于“开播/关播”边沿通知。
pub async fn get_live_state(pool: &SqlitePool, url: &str) -> anyhow::Result<bool> {
    let row: Option<i64> = sqlx::query_scalar("SELECT is_live FROM live_states WHERE url=?")
        .bind(url)
        .fetch_optional(pool)
        .await
        .context("查询直播状态")?;
    Ok(row == Some(1))
}

/// 记录某主播当前直播状态；即使「只推送不录制」也能正确触发关播通知。
pub async fn set_live_state(pool: &SqlitePool, url: &str, is_live: bool) -> anyhow::Result<()> {
    // 明确拿到开播/未开播结论时，连续失败计数必须一并清零；否则恢复后的
    // 正常轮次会被历史失败叠加，下一次抖动会提前触发“疑似下播”误判。
    sqlx::query(
        r#"INSERT INTO live_states (url, is_live, consecutive_fails, updated_at)
           VALUES (?, ?, 0, datetime('now','localtime'))
           ON CONFLICT(url) DO UPDATE SET
             is_live = excluded.is_live,
             consecutive_fails = 0,
             updated_at = excluded.updated_at"#,
    )
    .bind(url)
    .bind(if is_live { 1i64 } else { 0i64 })
    .execute(pool)
    .await
    .context("记录直播状态")?;
    Ok(())
}

/// 某直播源连续检测失败/超时的次数，达到阈值后才按疑似下播收敛。
pub async fn get_live_failures(pool: &SqlitePool, url: &str) -> anyhow::Result<u32> {
    let row: Option<i64> =
        sqlx::query_scalar("SELECT consecutive_fails FROM live_states WHERE url=?")
            .bind(url)
            .fetch_optional(pool)
            .await
            .context("查询直播检测失败次数")?;
    Ok(row.unwrap_or(0).clamp(0, i64::from(u32::MAX)) as u32)
}

/// 检测失败/超时时累计连续失败次数；数据库已有旧表时用迁移补齐字段。
pub async fn bump_live_failures(pool: &SqlitePool, url: &str) -> anyhow::Result<u32> {
    sqlx::query(
        r#"INSERT INTO live_states (url, is_live, consecutive_fails, updated_at)
           VALUES (?, 0, 1, datetime('now','localtime'))
           ON CONFLICT(url) DO UPDATE SET
             consecutive_fails = consecutive_fails + 1,
             updated_at = datetime('now','localtime')"#,
    )
    .bind(url)
    .execute(pool)
    .await
    .context("累计直播检测失败次数")?;
    get_live_failures(pool, url).await
}

/// 明确拿到开播/未开播结论后清零连续失败计数。
pub async fn reset_live_failures(pool: &SqlitePool, url: &str) -> anyhow::Result<()> {
    sqlx::query(
        r#"UPDATE live_states SET consecutive_fails=0, updated_at=datetime('now','localtime')
           WHERE url=?"#,
    )
    .bind(url)
    .execute(pool)
    .await
    .context("清零直播检测失败次数")?;
    Ok(())
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
           (id, platform, url, title, cover, cover_cache, published_at, duration, anchor, anchor_id, source_url, updated_at)
           VALUES (?,?,?,?,?,?,?,?,?,?,?,?)
           ON CONFLICT(id) DO UPDATE SET
             platform=excluded.platform, url=excluded.url, title=excluded.title,
             cover=excluded.cover, cover_cache=excluded.cover_cache,
             published_at=excluded.published_at, duration=excluded.duration,
             anchor=excluded.anchor, anchor_id=excluded.anchor_id, source_url=excluded.source_url, updated_at=excluded.updated_at"#,
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
    .bind(resolve_anchor_id(anchor))
    .bind(source_url)
    .bind(now)
    .execute(pool)
    .await
    .context("写入 videos")?;
    Ok(())
}

/// 浏览博主历史视频时的“只补不抢”写入：新记录按当前来源入库；已有记录只
/// 刷新平台元数据（标题/封面/时长/日期），绝不覆盖已有的 `anchor/source_url`
/// 归属，避免浏览一次就把视频从监控源名下“改嫁”到临时浏览地址。
pub async fn upsert_video_browse(
    pool: &SqlitePool,
    video: &crate::model::VideoMeta,
    source_url: &str,
    cover_cache: &str,
) -> anyhow::Result<()> {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    sqlx::query(
        r#"INSERT INTO videos
           (id, platform, url, title, cover, cover_cache, published_at, duration, source_url, updated_at)
           VALUES (?,?,?,?,?,?,?,?,?,?)
           ON CONFLICT(id) DO UPDATE SET
             platform=excluded.platform, url=excluded.url, title=excluded.title,
             cover=excluded.cover, cover_cache=excluded.cover_cache,
             published_at=excluded.published_at, duration=excluded.duration,
             updated_at=excluded.updated_at"#,
    )
    .bind(&video.id)
    .bind(&video.platform)
    .bind(&video.url)
    .bind(&video.title)
    .bind(&video.cover)
    .bind(cover_cache)
    .bind(&video.published_at)
    .bind(video.duration)
    .bind(source_url)
    .bind(now)
    .execute(pool)
    .await
    .context("浏览式写入 videos")?;
    Ok(())
}

/// 旧视频库迁移专用：只在主库没有相同 id 时插入，绝不覆盖已有记录。
///
/// 返回 `true` 表示本次确实写入，返回 `false` 表示因主库已存在而跳过。
/// 使用单条 `INSERT OR IGNORE`，比“先查再插”更不容易受并发影响，且幂等。
#[allow(clippy::too_many_arguments)] // 对齐旧库 videos 字段，逐字段复制
pub async fn insert_legacy_video(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    platform: &str,
    url: &str,
    title: &str,
    cover: &str,
    cover_cache: &str,
    published_at: &str,
    duration: i64,
    anchor: &str,
    source_url: &str,
    downloaded: bool,
    downloaded_at: &str,
    first_seen: &str,
) -> anyhow::Result<bool> {
    let result = sqlx::query(
        r#"INSERT OR IGNORE INTO videos
           (id, platform, url, title, cover, cover_cache, published_at, duration,
            anchor, source_url, downloaded, downloaded_at, first_seen)
           VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)"#,
    )
    .bind(id)
    .bind(platform)
    .bind(url)
    .bind(title)
    .bind(cover)
    .bind(cover_cache)
    .bind(published_at)
    .bind(duration)
    .bind(anchor)
    .bind(source_url)
    .bind(if downloaded { 1i64 } else { 0i64 })
    .bind(downloaded_at)
    .bind(first_seen)
    .execute(&mut *tx)
    .await
    .context("写入旧视频库记录")?;
    Ok(result.rows_affected() > 0)
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

/// 按最终下载 URL 标记视频库已下载。
///
/// B站视频 id 就是 BV 号，`mark_downloaded` 按 id 就能命中；但 RSS/YouTube/
/// 抖音等平台没有统一的 `BV` 提取规则，浏览历史入库时 id 可能是 GUID 或
/// 完整 URL，下载完成后必须按 URL 再更新一次，否则同一视频会留下
/// `downloaded=0` 的记录，桌面端会反复显示“未下载”并重复下载。
pub async fn mark_downloaded_by_url(
    pool: &SqlitePool,
    url: &str,
    downloaded: bool,
) -> anyhow::Result<u64> {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let result =
        sqlx::query("UPDATE videos SET downloaded=?, downloaded_at=?, updated_at=? WHERE url=?")
            .bind(if downloaded { 1 } else { 0 })
            .bind(if downloaded { now.as_str() } else { "" })
            .bind(if downloaded { now.as_str() } else { "" })
            .bind(url)
            .execute(pool)
            .await
            .context("按 URL 标记视频已下载")?;
    Ok(result.rows_affected())
}

/// 跨平台去重查询：同主播、规范化标题相同（忽略半角/全角空格）、
/// 但来自其他 URL 的**已下载**视频。命中说明同一内容已在别的平台下载过，
/// 调用方（下载 worker）应跳过本次下载，避免同一视频多平台各存一份。
/// 返回 (平台, 标题)。
pub async fn find_cross_platform_duplicate(
    pool: &SqlitePool,
    anchor: &str,
    title: &str,
    url: &str,
) -> anyhow::Result<Option<(String, String)>> {
    let norm_title = normalize_title(title);
    let row: Option<(String, String)> = sqlx::query_as(
        r#"SELECT COALESCE(NULLIF(platform,''),'未知'), title
           FROM videos
           WHERE anchor = ? AND downloaded = 1 AND url != ?
             AND REPLACE(REPLACE(title,' ',''),'　','') = ?
           LIMIT 1"#,
    )
    .bind(anchor)
    .bind(url)
    .bind(&norm_title)
    .fetch_optional(pool)
    .await
    .context("跨平台重复查询失败")?;
    Ok(row)
}

/// 标题规范化：去除半角/全角空格后比较，吸收多平台发布时的空格差异。
fn normalize_title(title: &str) -> String {
    title.replace([' ', '　'], "")
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
           (id, platform, url, title, anchor, anchor_id, source_url, updated_at)
           VALUES (?, '', ?, ?, ?, ?, ?, ?)"#,
    )
    .bind(video_id)
    .bind(url)
    .bind(title)
    .bind(anchor)
    .bind(resolve_anchor_id(anchor))
    .bind(url)
    .bind(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string())
    .execute(pool)
    .await
    .context("补写 videos 下载记录")?;
    Ok(())
}

/// 把 `source_url` 归一成「同一个博主」的比较键：去掉 `?query` 与 `#fragment`、
/// 去掉首尾空白与尾部斜杠、统一小写。
///
/// 用户在 B站复制的主页地址常带 `?spm_id_from=333.337.0.0` 这类跟踪参数；若按
/// 逐字节比较，同一主页会分裂成两个来源，列表和历史下载记录都对不上。
fn source_url_key(url: &str) -> String {
    let trimmed = url.trim();
    let cut = trimmed
        .find(|c| c == '?' || c == '#')
        .unwrap_or(trimmed.len());
    trimmed[..cut].trim_end_matches('/').to_ascii_lowercase()
}

/// 与 [`source_url_key`] 等价的 SQL 表达式（作用在 `videos.source_url` 列上），
/// 两边必须保持同一套规则，否则查库会漏。
const SOURCE_URL_KEY_SQL: &str = "LOWER(RTRIM(TRIM(SUBSTR(source_url, 1, MIN(\
     CASE WHEN INSTR(source_url,'?')=0 THEN LENGTH(source_url) ELSE INSTR(source_url,'?')-1 END, \
     CASE WHEN INSTR(source_url,'#')=0 THEN LENGTH(source_url) ELSE INSTR(source_url,'#')-1 END))), '/'))";

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
        // 用户输入可能带尾部斜杠、大小写差异或 `?spm_id_from=` 跟踪参数，和库里
        // 保存的 source_url 不一定逐字节相同；统一按「同一个博主」的键比较，
        // 保证「平台失败回退缓存」和「列历史取并集」不会因为 URL 写法不同而漏条。
        sql.push_str(&format!(" AND {SOURCE_URL_KEY_SQL} = ?"));
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
        query = query.bind(source_url_key(source_url));
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

/// 按一批视频 id 取回库内记录。
///
/// 浏览博主历史时，结果必须以平台返回的顺序和条数为准（平台就是权威），库里
/// 只用来补「已下载 / 封面缓存」这些状态。**不能用 `source_url` 反查**：同一个
/// 视频可能先被别的 `source_url` 写进库里（联合投稿同时挂在两个 UP 主下、换过
/// 主页地址、地址带 `?spm_id_from=` 跟踪参数），而 `videos.id` 是主键、冲突时
/// 不改写 `source_url`，按 source_url 查就会把它漏掉——表现为「官方 46 条、
/// 界面只列 45 条」，且少的那条还查不出原因。
pub async fn videos_by_ids(pool: &SqlitePool, ids: &[String]) -> anyhow::Result<Vec<Video>> {
    let mut out = Vec::new();
    if ids.is_empty() {
        return Ok(out);
    }
    // SQLite 单条语句的绑定变量有上限，分批查避免列表很长时直接报错。
    for chunk in ids.chunks(400) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT id, platform, url, title, cover, cover_cache, published_at, duration,
                    anchor, source_url, downloaded, downloaded_at
             FROM videos WHERE id IN ({placeholders})"
        );
        let mut query = sqlx::query(&sql);
        for id in chunk {
            query = query.bind(id);
        }
        let rows = query.fetch_all(pool).await.context("按 id 查询 videos")?;
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
    }
    Ok(out)
}

/// 只统计视频库总数，不加载记录内容。
///
/// `status` 快照只展示 `video_count`，此前却调用 `get_videos` 把全部视频
/// 记录载入内存再取长度。视频库长期积累后（几千上万条）会让桌面端每次轮询
/// status 都做一次全量查询，白白拖慢接口；改用 COUNT 后语义不变，负载恒定。
pub async fn count_videos(pool: &SqlitePool) -> anyhow::Result<i64> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM videos")
        .fetch_one(pool)
        .await
        .context("统计视频库数量")?;
    Ok(count)
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
        "SELECT id, platform, url, title, cover, cover_cache, published_at, duration, anchor, source_url, downloaded, downloaded_at
         FROM videos WHERE 1=1",
    );
    if !source_url.is_empty() {
        // 与 get_videos 同一套「同一个博主」比较规则，避免两条读路径口径不一致。
        sql.push_str(&format!(" AND {SOURCE_URL_KEY_SQL} = ?"));
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
        query = query.bind(source_url_key(source_url));
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
/// 选项 A：Anchor 一等公民 —— 主播身份的唯一解析点。
///
/// 当前稳定身份取主播名（与产物目录一级=主播名、R2 规则一致）。
/// 返回值是写入 `videos.anchor_id` / `tasks.anchor_id` / `live_sessions.anchor_id`
/// 的稳定主键；未来若要改成哈希 id 或查 `anchors` 表，只改这一处。
pub fn resolve_anchor_id(name: &str) -> String {
    name.to_string()
}

pub async fn enqueue_task(
    pool: &SqlitePool,
    kind: &str,
    url: &str,
    name: &str,
) -> anyhow::Result<i64> {
    // 同一 kind+url 已有未完成任务时不再重复入队，避免并发/重复执行同一资源。
    // 这里只认 pending/processing：failed 是“重试已耗尽”的终态，用户再次
    // 触发下载/复盘时应允许重新入队，否则旧失败记录会永久挡住新请求。
    let existing: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM tasks WHERE kind=? AND url=? AND status IN ('pending','processing') LIMIT 1",
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
        sqlx::query("INSERT INTO tasks (kind, url, name, anchor_id, status) VALUES (?, ?, ?, ?, 'pending')")
    .bind(kind)
    .bind(url)
    .bind(name)
    .bind(resolve_anchor_id(name))
    .execute(pool)
            .await
            .context("写入 tasks")?;
    Ok(result.last_insert_rowid())
}

/// 下载任务入队（带分类）：短视频监控与「批量下载」要落到不同的顶层目录，
/// 因此任务需携带 `category`（短视频 / 批量下载），worker 据此决定落盘位置。
/// 与 `enqueue_task` 相同的 pending/processing 去重语义，避免重复下载。
pub async fn enqueue_download_task(
    pool: &SqlitePool,
    url: &str,
    name: &str,
    category: &str,
) -> anyhow::Result<i64> {
    if url.trim().is_empty() {
        anyhow::bail!("下载地址不能为空");
    }
    let existing: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM tasks WHERE kind='download' AND url=? AND status IN ('pending','processing') LIMIT 1",
    )
    .bind(url)
    .fetch_optional(pool)
    .await
    .context("查询已有下载任务")?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let result = sqlx::query(
        "INSERT INTO tasks (kind, url, name, anchor_id, category, status) VALUES (?, ?, ?, ?, ?, 'pending')",
    )
    .bind("download")
    .bind(url)
    .bind(name)
    .bind(resolve_anchor_id(name))
    .bind(category)
    .execute(pool)
    .await
    .context("写入 tasks")?;
    Ok(result.last_insert_rowid())
}

/// 原子认领一条待处理任务：把 `pending` 置为 `processing` 并返回任务。
/// 认领后崩溃会把任务留在 processing，下次启动用 `recover_processing` 归还队列；
/// 单实例 daemon 下也防止同批任务被并发重复执行。
pub async fn claim_pending_task(pool: &SqlitePool, limit: i64) -> anyhow::Result<Option<Task>> {
    claim_pending_task_by_kind(pool, "", limit).await
}

/// 认领指定 kind 的待处理任务；kind 为空时认领任意 pending 任务。
/// 下载/复盘各自只认领自己的任务，避免并发 worker 互相抢任务。
pub async fn claim_pending_task_by_kind(
    pool: &SqlitePool,
    kind: &str,
    limit: i64,
) -> anyhow::Result<Option<Task>> {
    // 历史/异常数据自愈：attempts 已超过重试上限的 pending 任务直接落定 failed，
    // 避免旧计数（如旧版无限重试留下的 692）或反复崩溃恢复后继续无限重试。
    // 正常路径下 worker 在第三次失败/超时时就会落定 failed，这里只是兜底。
    // 必须同时限定 kind：下载 worker 只自愈自己的任务，不能把另一个 worker
    // 正在处理的 process 任务标 failed，也不能在运行中把 processing 恢复。
    if kind.is_empty() {
        sqlx::query(
            "UPDATE tasks
             SET status='failed',
                 last_error=CASE WHEN trim(last_error) = '' THEN '任务重试次数已超过上限，自动结束' ELSE last_error END,
                 updated_at=datetime('now','localtime')
             WHERE status='pending' AND attempts >= 3",
        )
            .execute(pool)
            .await
            .context("落定超出重试上限的任务")?;
    } else {
        sqlx::query(
            "UPDATE tasks
             SET status='failed',
                 last_error=CASE WHEN trim(last_error) = '' THEN '任务重试次数已超过上限，自动结束' ELSE last_error END,
                 updated_at=datetime('now','localtime')
             WHERE status='pending' AND kind=? AND attempts >= 3",
        )
            .bind(kind)
            .execute(pool)
            .await
            .context("落定超出重试上限的任务")?;
    }
    // 失败/超时后回 pending 的任务必须退避，不能立刻被 worker 再次认领。
    // B站 412、RSSHub 403、SMTP 限流等故障在几秒内不会恢复；通知队列已有
    // 退避，任务队列也应一致，否则 3 次失败会在 15 秒内连打 3 次，反而加重
    // 风控。等待时间按 attempts 递增：第 1 次失败后 1 分钟、第 2 次 5 分钟、
    // 第 3 次 15 分钟；attempts>=3 在进入本函数时已落定 failed，因此实际只会
    // 遇到 1/2 分钟档。新任务（attempts=0）不等待，坏时间戳按“已到期”处理。
    let mut sql = String::from(
        "SELECT id, kind, url, name, category, status, attempts, payload, last_error, next_retry_at, created_at, updated_at
         FROM tasks
         WHERE status = 'pending'",
    );
    let backoff_sql = if kind.is_empty() { "" } else { " AND kind = ?" };
    sql.push_str(backoff_sql);
    // 新任务（attempts=0）必须先排到前面，否则候选窗口恰好被退避任务占满
    // 时会漏掉窗口外的新任务；退避任务之间仍按 id 保持先来后到。
    sql.push_str(" ORDER BY CASE WHEN attempts = 0 THEN 0 ELSE 1 END, id LIMIT ?");
    let mut query = sqlx::query(&sql);
    if !kind.is_empty() {
        query = query.bind(kind);
    }
    // 多取一些候选，在 Rust 里按退避窗口过滤；SQLite 的 strftime/localtime
    // 组合容易踩时区坑，Rust 侧解析本地时间字符串更可靠。
    let candidate_limit = limit.saturating_mul(20).max(20);
    query = query.bind(candidate_limit);
    let candidates = query.fetch_all(pool).await.context("查询待处理任务")?;
    let now = chrono::Local::now().naive_local();
    for row in candidates {
        let attempts: i64 = row.try_get("attempts").unwrap_or(0);
        if attempts != 0 {
            let updated_at: String = row.try_get("updated_at").unwrap_or_default();
            if let Ok(updated) =
                chrono::NaiveDateTime::parse_from_str(&updated_at, "%Y-%m-%d %H:%M:%S")
            {
                let wait_secs = match attempts {
                    1 => 60,
                    2 => 300,
                    _ => 900,
                };
                if updated + chrono::Duration::seconds(wait_secs) > now {
                    continue;
                }
            }
            // 坏时间戳按“已到期”处理，避免异常数据永久卡队列。
        }
        let id: i64 = row.try_get("id")?;
        let updated = sqlx::query(
            "UPDATE tasks SET status='processing', attempts=attempts+1, updated_at=datetime('now','localtime') WHERE id=? AND status = 'pending'",
        )
        .bind(id)
        .execute(pool)
        .await
        .context("认领下载任务")?;
        if updated.rows_affected() == 0 {
            // 并发入口抢到同一任务时继续看候选里的下一条，避免整次认领返回
            // None 让后面的可认领任务多等一轮。
            continue;
        }
        let attempts: i64 = row.try_get("attempts")?;
        return Ok(Some(Task {
            id,
            kind: row.try_get("kind")?,
            url: row.try_get("url")?,
            name: row.try_get("name")?,
            category: row.try_get("category")?,
            status: "processing".to_string(),
            // 认领时 DB 里已经 `attempts+1`，这里返回最新值，调用方据此决定是否落定 failed。
            attempts: attempts + 1,
            payload: row.try_get("payload")?,
            last_error: row.try_get("last_error")?,
            next_retry_at: row.try_get("next_retry_at")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        }));
    }
    Ok(None)
}

/// 启动/每轮 tick 前把上次崩溃遗留的 processing 任务归还 pending 重试。
/// 注意：下载 worker 与复盘 worker 各自认领前只恢复自己 kind 的任务，
/// 避免把另一个 worker 正在处理的任务误判成崩溃残留。
pub async fn recover_processing(pool: &SqlitePool) -> anyhow::Result<u64> {
    let result = sqlx::query("UPDATE tasks SET status='pending', updated_at=datetime('now','localtime') WHERE status='processing'")
        .execute(pool)
        .await
        .context("恢复处理中任务")?;
    Ok(result.rows_affected())
}

/// 只恢复指定 kind 的 processing 任务（下载/复盘队列各自独立恢复）。
pub async fn recover_processing_by_kind(pool: &SqlitePool, kind: &str) -> anyhow::Result<u64> {
    let result = sqlx::query(
        "UPDATE tasks SET status='pending', updated_at=datetime('now','localtime')
         WHERE status='processing' AND kind=?",
    )
    .bind(kind)
    .execute(pool)
    .await
    .context("恢复指定类型处理中任务")?;
    Ok(result.rows_affected())
}

/// 运行中自愈：把 attempts 已超过重试上限、却仍卡在 processing 的任务直接
/// 落定 failed。
///
/// 正常路径下 worker 在第三次失败/超时时就会落定 failed；但 DB 更新失败、
/// 进程被 SIGKILL 后未重启、或看门狗误杀等异常会让任务留在 processing。
/// 只处理指定 kind，避免把另一个 worker 正在执行的任务误标 failed。
pub async fn fail_stale_processing_by_kind(
    pool: &SqlitePool,
    kind: &str,
    attempts_limit: i64,
) -> anyhow::Result<u64> {
    let result = sqlx::query(
        "UPDATE tasks
         SET status='failed',
             last_error=CASE WHEN trim(last_error) = '' THEN '任务处理中断且重试次数已超过上限，自动结束' ELSE last_error END,
             updated_at=datetime('now','localtime')
         WHERE status='processing' AND kind=? AND attempts >= ?",
    )
    .bind(kind)
    .bind(attempts_limit)
    .execute(pool)
    .await
    .context("自愈超限 processing 任务")?;
    Ok(result.rows_affected())
}

pub async fn list_tasks(pool: &SqlitePool, status: &str, limit: i64) -> anyhow::Result<Vec<Task>> {
    let mut sql = String::from(
        "SELECT id, kind, url, name, category, status, attempts, payload, last_error, next_retry_at, created_at, updated_at FROM tasks WHERE 1=1",
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
            category: row.try_get("category")?,
            status: row.try_get("status")?,
            attempts: row.try_get("attempts")?,
            payload: row.try_get("payload")?,
            last_error: row.try_get("last_error")?,
            next_retry_at: row.try_get("next_retry_at")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        });
    }
    Ok(out)
}

/// 按任务类型读取最近的任务，供状态快照/界面显示队列进度。
///
/// 与 `list_tasks` 分开：调度器需要按状态和插入顺序工作；桌面端只关心某类任务
/// 最近发生了什么，必须按 id 倒序拿到最新记录，不能把全表拉到调用方再过滤。
pub async fn list_tasks_by_kind(
    pool: &SqlitePool,
    kind: &str,
    limit: i64,
) -> anyhow::Result<Vec<Task>> {
    let rows = sqlx::query(
        "SELECT id, kind, url, name, category, status, attempts, payload, last_error, next_retry_at, created_at, updated_at
         FROM tasks WHERE kind=? ORDER BY id DESC LIMIT ?",
    )
    .bind(kind)
    .bind(limit)
    .fetch_all(pool)
    .await
    .context("查询指定类型 tasks")?;
    let mut out = Vec::new();
    for row in rows {
        out.push(Task {
            id: row.try_get("id")?,
            kind: row.try_get("kind")?,
            url: row.try_get("url")?,
            name: row.try_get("name")?,
            category: row.try_get("category")?,
            status: row.try_get("status")?,
            attempts: row.try_get("attempts")?,
            payload: row.try_get("payload")?,
            last_error: row.try_get("last_error")?,
            next_retry_at: row.try_get("next_retry_at")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        });
    }
    Ok(out)
}

/// 记录一次任务失败，并保留可展示、可诊断的原因。
///
/// `status` 由调用方给 `pending`（还会自动重试）或 `failed`（重试已耗尽）。
/// `next_retry_at` 只对 pending 有意义；终态失败必须清空，避免界面显示一条
/// 永远不会再执行的任务仍在等待。
pub async fn record_task_failure(
    pool: &SqlitePool,
    id: i64,
    error: &str,
    status: &str,
) -> anyhow::Result<()> {
    let retry_expr = if status == "pending" {
        "datetime('now','localtime','+1 minute')"
    } else {
        "''"
    };
    let sql = format!(
        "UPDATE tasks SET status=?, last_error=?, next_retry_at={retry_expr}, updated_at=datetime('now','localtime') WHERE id=?"
    );
    sqlx::query(&sql)
        .bind(status)
        .bind(error.chars().take(4000).collect::<String>())
        .bind(id)
        .execute(pool)
        .await
        .context("记录任务失败原因")?;
    Ok(())
}

pub async fn update_task_status(pool: &SqlitePool, id: i64, status: &str) -> anyhow::Result<()> {
    // 本函数只负责终态更新（done/failed/pending），attempts 在认领时递增，
    // 避免“超时”与“失败”两个分支重复计数，也让同一条任务每次真正执行都计一次。
    sqlx::query("UPDATE tasks SET status=?, updated_at=datetime('now','localtime') WHERE id=?")
        .bind(status)
        .bind(id)
        .execute(pool)
        .await
        .context("更新任务状态")?;
    Ok(())
}

/// 完成一个已经产出真实结果的任务，同时保留一段“非失败说明”。
///
/// 典型场景：转写文件已经落盘，但用户尚未配置 LLM 接口，Python 流水线以
/// “未配置大模型接口”提前结束。整条任务不能因此继续重试并覆盖真实状态；
/// 这里统一标 done、清空 next_retry_at，并把说明留在 last_error 供界面显示。
pub async fn complete_task_with_note(pool: &SqlitePool, id: i64, note: &str) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE tasks SET status='done', last_error=?, next_retry_at='', updated_at=datetime('now','localtime') WHERE id=?",
    )
    .bind(note.chars().take(4000).collect::<String>())
    .bind(id)
    .execute(pool)
    .await
    .context("完成带说明的任务")?;
    Ok(())
}

/// 更新任务状态但不增加 attempts（用于达到重试上限后落定 failed）。
pub async fn set_task_status(pool: &SqlitePool, id: i64, status: &str) -> anyhow::Result<()> {
    sqlx::query("UPDATE tasks SET status=?, updated_at=datetime('now','localtime') WHERE id=?")
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
        "INSERT INTO notify (event, anchor, title, content, channel, status, attempts) VALUES (?,?,?,?,?,'pending',0)",
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
        "SELECT id, event, anchor, title, content, channel, status, attempts, created_at FROM notify WHERE 1=1",
    );
    if !status.is_empty() {
        sql.push_str(" AND status = ?");
    }
    // 通知审计/界面默认最新在前；重试队列单独走 `list_pending_notify`。
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
            attempts: row.try_get("attempts")?,
            created_at: row.try_get("created_at")?,
        });
    }
    Ok(out)
}

/// 列出待重试通知，最老优先。
///
/// 单轮 `flush_pending` 只处理前 N 条，若用最新优先，队列超过单轮上限时
/// 老通知会被持续产生的新通知永远挤出重试窗口；最老优先保证每条通知
/// 都能在后续轮次被处理并收敛到 sent/failed。
pub async fn list_pending_notify(
    pool: &SqlitePool,
    limit: i64,
) -> anyhow::Result<Vec<NotifyEvent>> {
    let rows = sqlx::query(
        "SELECT id, event, anchor, title, content, channel, status, attempts, created_at
         FROM notify WHERE status = 'pending' ORDER BY id ASC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .context("查询待重试通知")?;
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
            attempts: row.try_get("attempts")?,
            created_at: row.try_get("created_at")?,
        });
    }
    Ok(out)
}

pub async fn mark_notify_done(pool: &SqlitePool, id: i64) -> anyhow::Result<()> {
    sqlx::query("UPDATE notify SET status='sent', attempts=attempts+1 WHERE id=?")
        .bind(id)
        .execute(pool)
        .await
        .context("更新通知状态")?;
    Ok(())
}

/// 通知明确失败（通道未接入/配置缺失）时标记，避免无限 pending。
pub async fn fail_notify(pool: &SqlitePool, id: i64) -> anyhow::Result<()> {
    sqlx::query("UPDATE notify SET status='failed' WHERE id=?")
        .bind(id)
        .execute(pool)
        .await
        .context("更新通知状态")?;
    Ok(())
}

/// 增加通知重试计数并返回最新值；由调用方决定是否达到上限后落定 failed。
pub async fn bump_notify_attempts(pool: &SqlitePool, id: i64) -> anyhow::Result<i64> {
    sqlx::query("UPDATE notify SET attempts=attempts+1 WHERE id=?")
        .bind(id)
        .execute(pool)
        .await
        .context("更新通知重试计数")?;
    let attempts: i64 = sqlx::query_scalar("SELECT attempts FROM notify WHERE id=?")
        .bind(id)
        .fetch_one(pool)
        .await
        .context("查询通知重试计数")?;
    Ok(attempts)
}

/// 把已收敛为 failed 的通知重新放回 pending 队列，供配置补齐后手动重试。
///
/// 返回被重新入队的记录数。只有终态 failed 才允许重置，避免把正在发送的
/// pending 记录重置成另一轮重复投递；`attempts` 同时清零，并把创建时间拨
/// 到当前，让退避从手动重试那一刻重新起算，而不是沿用旧的失败时间。
pub async fn retry_failed_notify(pool: &SqlitePool, limit: i64) -> anyhow::Result<u64> {
    let effective = if limit <= 0 { i64::MAX } else { limit };
    let result = sqlx::query(
        "UPDATE notify SET status='pending', attempts=0, created_at=datetime('now','localtime')
         WHERE id IN (
             SELECT id FROM notify WHERE status='failed' ORDER BY id ASC LIMIT ?
         )",
    )
    .bind(effective)
    .execute(pool)
    .await
    .context("重新入队失败通知")?;
    Ok(result.rows_affected())
}

/// 删除指定状态的终态通知记录，返回删除条数。
///
/// 只允许清理 sent/failed 等已完成投递的记录；pending 是还没送完的通知，
/// 删除会静默丢提醒，因此明确拒绝。
pub async fn prune_notify(pool: &SqlitePool, status: &str, limit: i64) -> anyhow::Result<u64> {
    if !matches!(status, "sent" | "failed") {
        anyhow::bail!("只允许清理 sent 或 failed 状态的通知记录: {status}");
    }
    let effective = if limit <= 0 { i64::MAX } else { limit };
    let result = sqlx::query(
        "DELETE FROM notify WHERE status=? AND id IN (
             SELECT id FROM notify WHERE status=? ORDER BY id ASC LIMIT ?
         )",
    )
    .bind(status)
    .bind(status)
    .bind(effective)
    .execute(pool)
    .await
    .context("清理通知记录")?;
    Ok(result.rows_affected())
}

/// 通知投递健康摘要：`(pending, failed, sent)`，只统计当前表内记录。
///
/// 24×30 验收里“通知链路可用”不能只看进程健康：配置缺失或通道连续失败时，
/// 通知表会堆积 failed/pending 记录，`doctor/status/audit` 需要一眼看到。
pub async fn notify_health_summary(pool: &SqlitePool) -> anyhow::Result<(u64, u64, u64)> {
    let counts: Vec<(String, i64)> =
        sqlx::query_as("SELECT status, COUNT(*) FROM notify GROUP BY status")
            .fetch_all(pool)
            .await
            .context("统计通知状态")?;
    let mut pending = 0i64;
    let mut failed = 0i64;
    let mut sent = 0i64;
    for (status, count) in counts {
        match status.as_str() {
            "pending" => pending = count,
            "failed" => failed = count,
            "sent" => sent = count,
            _ => {}
        }
    }
    Ok((
        pending.max(0) as u64,
        failed.max(0) as u64,
        sent.max(0) as u64,
    ))
}

/// 生成可操作的通知健康提示，供 status/CLI doctor/MCP doctor 共用。
///
/// 返回值中的 `hint` 给用户/智能体下一步动作，`latest_failed_channel` 用于
/// 精确定位是哪条通道未配置或投递失败。规则：
/// - 未配置任何通道且存在失败记录：优先提示配置邮箱/Server酱。
/// - 有失败记录：提示清理或重试，并给出最近一条失败通道。
/// - 有 pending 堆积：提示等待重试或检查网络。
/// - 全部健康：返回 None。
pub fn notify_health_hint(
    pending: u64,
    failed: u64,
    configured: bool,
    latest_failed_channel: Option<&str>,
) -> Option<String> {
    if failed > 0 {
        let channel = latest_failed_channel
            .filter(|s| !s.is_empty())
            .unwrap_or("未知");
        if !configured {
            return Some(format!(
                "通知失败 {failed} 条：未配置可用通知通道，请在 config.ini 补齐邮箱(SMTP)或 Server酱 SendKey 后执行 notify-retry 0"
            ));
        }
        return Some(format!(
            "通知失败 {failed} 条（最近通道：{channel}）：请检查该通道配置/网络，或执行 notify-retry 0 重试；确认无需再发送可执行 notify-prune failed 0 清理"
        ));
    }
    if pending > 0 {
        return Some(format!(
            "有 {pending} 条通知等待发送：daemon 会按退避重试，也可检查网络或通知通道配置"
        ));
    }
    if !configured {
        return Some("通知通道未配置：邮箱/Server酱都为空，当前不会发送通知".to_string());
    }
    None
}

/// 查询最近一条 failed 通知的通道，供健康提示定位配置缺口。
pub async fn latest_failed_notify_channel(pool: &SqlitePool) -> anyhow::Result<Option<String>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT channel FROM notify WHERE status='failed' ORDER BY id DESC LIMIT 1")
            .fetch_optional(pool)
            .await
            .context("查询最近失败通知通道")?;
    Ok(row.map(|r| r.0))
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
        let pool = open_pool(&file).await.unwrap();
        // 测试库文件交由 OS 临时目录回收，避免并发删除正在使用的 SQLite 文件。
        std::mem::forget(file);
        pool
    }

    #[test]
    fn source_url_key_normalizes_query_slash_and_case() {
        assert_eq!(
            source_url_key(" https://Space.Bilibili.com/601214690/?spm_id_from=1#/all "),
            "https://space.bilibili.com/601214690"
        );
        assert_eq!(source_url_key("https://x.test/a/"), "https://x.test/a");
        assert_eq!(source_url_key(""), "");
    }

    #[tokio::test]
    async fn source_url_matching_ignores_tracking_query() {
        // 用户在 B站复制的主页地址常带 ?spm_id_from=... 这类跟踪参数。若按逐字节
        // 比较，同一个博主会被当成两个来源：列历史取并集时捞不到历史条目
        // （真实事故：界面从 45 条掉到 34 条），平台失败时的缓存回退也会失效。
        let pool = test_pool().await;
        let meta = VideoMeta {
            id: "BVtrack".into(),
            platform: "bilibili".into(),
            url: "https://www.bilibili.com/video/BVtrack".into(),
            title: "带跟踪参数入库".into(),
            cover: String::new(),
            published_at: "2026-09-25".into(),
            duration: 60,
        };
        upsert_video(
            &pool,
            &meta,
            "某主播",
            "https://space.bilibili.com/601214690?spm_id_from=333.337.0.0",
            "",
        )
        .await
        .unwrap();

        // 库里存的是带参数的地址；用归一化（去掉跟踪参数）后的地址查，必须命中。
        let by_clean = get_videos(&pool, "https://space.bilibili.com/601214690", "", false)
            .await
            .unwrap();
        assert_eq!(by_clean.len(), 1, "去掉跟踪参数后必须还能查到历史条目");
        assert_eq!(by_clean[0].id, "BVtrack");

        // 反向同理：库里干净、查询带参数也要命中。
        let by_dirty = get_videos(
            &pool,
            "https://space.bilibili.com/601214690?spm_id_from=333.1007.0.0",
            "",
            false,
        )
        .await
        .unwrap();
        assert_eq!(by_dirty.len(), 1);

        // 两条读路径口径必须一致，否则界面与 MCP 会给出不同的条数。
        let via_list = list_videos(&pool, "https://space.bilibili.com/601214690/", "", false)
            .await
            .unwrap();
        assert_eq!(via_list.len(), 1, "list_videos 必须与 get_videos 同口径");

        // 不同博主绝不能被误合并。
        let other = get_videos(&pool, "https://space.bilibili.com/123456", "", false)
            .await
            .unwrap();
        assert!(other.is_empty());
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

    #[test]
    fn production_pool_limits_are_explicit() {
        // 防止 sqlx 升级时默认值悄悄变化：24×30 需要可预期的并发上限，
        // 不能因为依赖默认连接数改变而出现文件句柄膨胀或 acquire 超时。
        assert_eq!(POOL_MAX_CONNECTIONS, 8);
        assert_eq!(POOL_ACQUIRE_TIMEOUT, Duration::from_secs(15));
    }

    #[tokio::test]
    async fn open_options_does_not_reapply_wal_on_every_connection() {
        let seq = TEST_DB_SEQ.fetch_add(1, Ordering::Relaxed);
        let file = std::env::temp_dir().join(format!(
            "zunzhe-wal-reinit-{}-{}.db",
            std::process::id(),
            seq
        ));
        let _ = std::fs::remove_file(&file);

        // 第一次打开负责把库切到 WAL；此后 journal_mode 是数据库的持久属性。
        let first = open_pool(&file).await.unwrap();
        first.close().await;

        // 第二次打开必须只继承持久 WAL，而不是再次发出 `PRAGMA journal_mode=WAL`。
        // 生产日志里 6~11 秒的慢查询正是这条连接初始化语句。
        // sqlx 的 Debug 会列出所有 pragma key，但值为 `None` 的条目只是
        // “未启用”，不代表连接时会执行。这里必须只检查实际有值的 pragma，
        // 否则会把 `journal_mode: None` 误判成重复切 WAL。
        let options_debug = format!("{:?}", open_options(&file)).to_ascii_lowercase();
        let journal_mode_value = options_debug
            .split("journal_mode\":")
            .nth(1)
            .and_then(|tail| tail.split(',').next())
            .unwrap_or_default()
            .trim();
        assert!(
            journal_mode_value == "none" || journal_mode_value.is_empty(),
            "已存在的库不应在每个连接重复设置 journal_mode: {journal_mode_value}"
        );
        assert!(options_debug.contains("busy_timeout"));
        assert!(options_debug.contains("foreign_keys"));

        // 但库本身仍必须保持持久 WAL，后续连接无需再切换。
        let reopened = open_pool(&file).await.unwrap();
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&reopened)
            .await
            .unwrap();
        assert_eq!(mode.to_ascii_lowercase(), "wal");
        reopened.close().await;

        let _ = std::fs::remove_file(&file);
    }

    #[tokio::test]
    async fn pool_survives_concurrent_read_write_stress() {
        let pool = test_pool().await;
        // 36 个并发任务同时读/写同一张表，模拟 daemon tick、直播状态、下载
        // worker 与通知重试在真实运行中共享一个池；任何写事务失败或 acquire
        // 超时都会让 24×30 的“状态不丢”承诺失效。
        let mut handles = Vec::new();
        for i in 0..36u64 {
            let pool = pool.clone();
            handles.push(tokio::spawn(async move {
                let meta = VideoMeta {
                    id: format!("BVstress{i}"),
                    platform: "bilibili".into(),
                    url: format!("https://www.bilibili.com/video/BVstress{i}"),
                    title: format!("并发视频 {i}"),
                    cover: String::new(),
                    published_at: "2026-08-14".into(),
                    duration: 60,
                };
                upsert_video(&pool, &meta, "并发博主", "https://space.example.com/x", "")
                    .await
                    .unwrap();
                mark_downloaded(&pool, &meta.id, i % 2 == 0).await.unwrap();
                let seen = is_downloaded(&pool, &meta.id).await.unwrap();
                assert_eq!(seen, i % 2 == 0);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        let videos = get_videos(&pool, "", "", false).await.unwrap();
        assert_eq!(videos.len(), 36);
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
        let pending = list_notify(&pool, "pending", 10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].attempts, 0);
        mark_notify_done(&pool, nid).await.unwrap();
        assert_eq!(list_notify(&pool, "pending", 10).await.unwrap().len(), 0);
        assert_eq!(list_notify(&pool, "sent", 10).await.unwrap()[0].attempts, 1);
    }

    #[tokio::test]
    async fn completed_task_can_keep_a_note_without_waiting_for_retry() {
        let pool = test_pool().await;
        let id = enqueue_task(&pool, "process", "/tmp/有转写.mp4", "测试")
            .await
            .unwrap();
        record_task_failure(&pool, id, "旧的临时失败", "pending")
            .await
            .unwrap();
        complete_task_with_note(&pool, id, "未配置大模型接口，已保留转写文本，流水线结束")
            .await
            .unwrap();
        let task = list_tasks(&pool, "done", 10)
            .await
            .unwrap()
            .into_iter()
            .find(|task| task.id == id)
            .expect("转写成功但未提炼的任务应进入 done");
        assert!(task.last_error.contains("已保留转写文本"), "{task:?}");
        assert!(
            task.next_retry_at.is_empty(),
            "done 任务不应等待重试: {task:?}"
        );
    }

    #[tokio::test]
    async fn failed_task_records_structured_error_and_retry_time() {
        let pool = test_pool().await;
        let id = enqueue_task(&pool, "process", "/tmp/失败.mp4", "测试")
            .await
            .unwrap();
        record_task_failure(&pool, id, "联网下载模型超时: ConnectTimeout", "pending")
            .await
            .unwrap();
        let task = list_tasks(&pool, "pending", 10)
            .await
            .unwrap()
            .into_iter()
            .find(|task| task.id == id)
            .expect("失败后仍应保留可重试任务");
        assert!(
            task.last_error.contains("ConnectTimeout"),
            "应保存可读错误: {task:?}"
        );
        assert!(
            !task.next_retry_at.is_empty(),
            "失败后应写入下次重试时间: {task:?}"
        );
        record_task_failure(&pool, id, "最终失败", "failed")
            .await
            .unwrap();
        let task = list_tasks(&pool, "failed", 10)
            .await
            .unwrap()
            .into_iter()
            .find(|task| task.id == id)
            .expect("终态失败任务应可查询");
        assert_eq!(task.last_error, "最终失败");
        assert!(
            task.next_retry_at.is_empty(),
            "终态失败不应继续等待重试: {task:?}"
        );
    }

    #[tokio::test]
    async fn failed_task_can_be_re_enqueued_for_manual_retry() {
        let pool = test_pool().await;
        let id = enqueue_task(&pool, "download", "https://x.test/retry", "博主")
            .await
            .unwrap();
        set_task_status(&pool, id, "failed").await.unwrap();
        // failed 是重试耗尽后的终态：手动再次入队应得到一条新的可执行任务，
        // 而不是被旧失败记录永久挡在队列外。
        let new_id = enqueue_task(&pool, "download", "https://x.test/retry", "博主")
            .await
            .unwrap();
        assert_ne!(new_id, id, "failed 任务应允许重新入队");
        let pending = list_tasks(&pool, "pending", 10).await.unwrap();
        assert!(
            pending.iter().any(|t| t.id == new_id),
            "重新入队的任务应回到 pending 可被 worker 认领"
        );
        // pending 状态下再去重仍返回同一条，不能无限堆积。
        let dup = enqueue_task(&pool, "download", "https://x.test/retry", "博主")
            .await
            .unwrap();
        assert_eq!(dup, new_id, "pending 中再次入队应去重");
    }

    #[tokio::test]
    async fn claim_and_recover_processing_task() {
        let pool = test_pool().await;
        let id = enqueue_task(&pool, "download", "https://x.test/claim", "博主")
            .await
            .unwrap();

        let claimed = claim_pending_task(&pool, 1).await.unwrap().unwrap();
        assert_eq!(claimed.id, id);
        assert_eq!(claimed.status, "processing");
        // 认领后同一轮不能再拿到同一条任务。
        assert!(claim_pending_task(&pool, 1).await.unwrap().is_none());

        // 模拟 daemon 在 processing 时崩溃：重启后 recover 把它归还 pending。
        let recovered = recover_processing(&pool).await.unwrap();
        assert_eq!(recovered, 1);
        // 崩溃恢复的任务 attempts 可能为 1，测试拨旧时间绕过退避窗口，
        // 快速验证“恢复后能再次认领”的状态机语义。
        sqlx::query("UPDATE tasks SET updated_at=datetime('now','localtime','-1 hour') WHERE id=?")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let again = claim_pending_task(&pool, 1).await.unwrap().unwrap();
        assert_eq!(again.id, id);

        // 达到重试上限后标 failed，之后不能再被自动认领（防止无限循环）。
        set_task_status(&pool, id, "failed").await.unwrap();
        assert!(
            claim_pending_task(&pool, 1).await.unwrap().is_none(),
            "failed 任务不应被下载 worker 自动重新认领"
        );
    }

    #[tokio::test]
    async fn retry_task_waits_backoff_before_reclaim() {
        let pool = test_pool().await;
        let id = enqueue_task(&pool, "download", "https://x.test/backoff", "博主")
            .await
            .unwrap();

        // 第一次认领并模拟失败：attempts=1，回 pending 后必须等 1 分钟。
        let claimed = claim_pending_task_by_kind(&pool, "download", 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, id);
        assert_eq!(claimed.attempts, 1);
        update_task_status(&pool, id, "pending").await.unwrap();
        assert!(
            claim_pending_task_by_kind(&pool, "download", 1)
                .await
                .unwrap()
                .is_none(),
            "失败后 1 分钟内不应再次认领"
        );

        // 把 updated_at 拨到 2 分钟前，等价于等待期结束，应能再次认领。
        sqlx::query(
            "UPDATE tasks SET updated_at=datetime('now','localtime','-2 minutes') WHERE id=?",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
        let reclaimed = claim_pending_task_by_kind(&pool, "download", 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reclaimed.id, id);
        assert_eq!(reclaimed.attempts, 2);

        // 新任务（attempts=0）不受退避影响，立即认领。
        let fresh = enqueue_task(&pool, "download", "https://x.test/fresh", "博主")
            .await
            .unwrap();
        let claimed = claim_pending_task_by_kind(&pool, "download", 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, fresh, "新任务应跳过退避立即认领");
    }

    #[tokio::test]
    async fn fresh_task_outside_backoff_window_is_still_claimed() {
        let pool = test_pool().await;
        // 前 20 条都是刚失败、处于退避期的任务：若候选窗口按 id 只取前 20
        // 条，第 21 条新任务会被一起漏掉，worker 必须多等一轮才能认领。
        // 新任务应排到退避任务前面，跨过整个退避窗口立即被认领。
        let mut backoff_ids = Vec::new();
        for i in 0..20 {
            let id = enqueue_task(
                &pool,
                "download",
                &format!("https://x.test/backoff-{i}"),
                "博主",
            )
            .await
            .unwrap();
            backoff_ids.push(id);
        }
        let fresh = enqueue_task(&pool, "download", "https://x.test/fresh-outside", "博主")
            .await
            .unwrap();
        // 模拟这 20 条都刚失败回到 pending，attempts=1 且在 1 分钟退避期内。
        sqlx::query("UPDATE tasks SET attempts=1 WHERE id IN (SELECT id FROM tasks WHERE kind='download' AND url LIKE 'https://x.test/backoff-%')")
            .execute(&pool)
            .await
            .unwrap();

        let claimed = claim_pending_task_by_kind(&pool, "download", 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            claimed.id, fresh,
            "退避窗口外的第 21 条新任务应立即被认领，而不是卡住"
        );
        assert_eq!(claimed.attempts, 1);

        // 新任务被认领后，退避任务仍不能被提前认领。
        assert!(
            claim_pending_task_by_kind(&pool, "download", 1)
                .await
                .unwrap()
                .is_none(),
            "退避中的任务不应被提前认领"
        );
        // 确认 20 条退避任务仍留在 pending，没有因为候选窗口被跳过而丢失。
        let pending = list_tasks(&pool, "pending", 100).await.unwrap();
        assert!(
            pending
                .iter()
                .filter(|t| backoff_ids.contains(&t.id))
                .count()
                == backoff_ids.len(),
            "退避任务应全部保持 pending: {pending:?}"
        );
    }

    #[tokio::test]
    async fn pending_task_with_exhausted_attempts_is_self_healed() {
        let pool = test_pool().await;
        let id = enqueue_task(&pool, "download", "https://x.test/stale", "博主")
            .await
            .unwrap();
        // 模拟旧版无限重试或崩溃恢复后留下的异常计数：attempts 已超上限但仍 pending。
        sqlx::query("UPDATE tasks SET attempts=5 WHERE id=?")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let claimed = claim_pending_task_by_kind(&pool, "download", 1)
            .await
            .unwrap();
        assert!(claimed.is_none(), "超上限任务不应再被认领");
        let rows = list_tasks(&pool, "failed", 10).await.unwrap();
        let task = rows.iter().find(|t| t.id == id);
        assert!(
            task.is_some(),
            "超上限的 pending 任务应自愈为 failed: {rows:?}"
        );
        assert!(
            task.is_some_and(|t| !t.last_error.trim().is_empty()),
            "自愈失败的任务必须保留非空失败原因，不能在界面显示空白: {rows:?}"
        );
    }

    #[tokio::test]
    async fn kind_scoped_self_heal_does_not_touch_other_worker_tasks() {
        let pool = test_pool().await;
        let dl_id = enqueue_task(&pool, "download", "https://x.test/stale-dl", "博主")
            .await
            .unwrap();
        let process_id = enqueue_task(&pool, "process", "/tmp/stale-process.mp4", "博主")
            .await
            .unwrap();
        sqlx::query("UPDATE tasks SET attempts=5 WHERE id IN (?, ?)")
            .bind(dl_id)
            .bind(process_id)
            .execute(&pool)
            .await
            .unwrap();

        // 下载 worker 自愈只能把自己的 download 任务标 failed，
        // 不能把另一个 worker 的 process 任务一起标 failed。
        let claimed = claim_pending_task_by_kind(&pool, "download", 1)
            .await
            .unwrap();
        assert!(claimed.is_none(), "超上限的 download 任务不应被认领");
        let failed = list_tasks(&pool, "failed", 10).await.unwrap();
        assert!(
            failed.iter().any(|t| t.id == dl_id),
            "download 超上限任务应自愈为 failed: {failed:?}"
        );
        assert!(
            !failed.iter().any(|t| t.id == process_id),
            "下载 worker 自愈不能误伤 process 任务: {failed:?}"
        );
        let pending = list_tasks(&pool, "pending", 10).await.unwrap();
        assert!(
            pending.iter().any(|t| t.id == process_id),
            "process 任务应保持 pending 等待复盘 worker: {pending:?}"
        );
    }

    #[tokio::test]
    async fn stale_processing_with_exhausted_attempts_is_self_healed() {
        let pool = test_pool().await;
        let dl_id = enqueue_task(&pool, "download", "https://x.test/stale-proc", "博主")
            .await
            .unwrap();
        let process_id = enqueue_task(&pool, "process", "/tmp/stale-proc.mp4", "博主")
            .await
            .unwrap();
        // 模拟异常退出后留在 processing、attempts 已超上限的两类任务。
        sqlx::query("UPDATE tasks SET status='processing', attempts=5 WHERE id=?")
            .bind(dl_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE tasks SET status='processing', attempts=5 WHERE id=?")
            .bind(process_id)
            .execute(&pool)
            .await
            .unwrap();

        let healed = fail_stale_processing_by_kind(&pool, "download", 3)
            .await
            .unwrap();
        assert_eq!(healed, 1);
        let failed = list_tasks(&pool, "failed", 10).await.unwrap();
        let task = failed.iter().find(|t| t.id == dl_id);
        assert!(
            task.is_some(),
            "超限 download processing 应落定 failed: {failed:?}"
        );
        assert!(
            task.is_some_and(|t| !t.last_error.trim().is_empty()),
            "processing 自愈失败的任务也必须保留非空失败原因: {failed:?}"
        );
        assert!(
            !failed.iter().any(|t| t.id == process_id),
            "不能把 process worker 正在执行的任务标 failed: {failed:?}"
        );
        // 未超限的 processing 不能误伤；process 任务 attempts=5 由复盘 worker
        // 自己的自愈处理，这里只验证低 attempts 不被本函数清理。
        sqlx::query("UPDATE tasks SET status='processing', attempts=1 WHERE id=?")
            .bind(process_id)
            .execute(&pool)
            .await
            .unwrap();
        let healed = fail_stale_processing_by_kind(&pool, "process", 3)
            .await
            .unwrap();
        assert_eq!(healed, 0);
        let processing = list_tasks(&pool, "processing", 10).await.unwrap();
        assert!(
            processing.iter().any(|t| t.id == process_id),
            "未超限 processing 应继续保留: {processing:?}"
        );
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
    async fn browse_upsert_does_not_steal_existing_ownership() {
        let pool = test_pool().await;
        let meta = VideoMeta {
            id: "BVowner".into(),
            platform: "bilibili".into(),
            url: "https://www.bilibili.com/video/BVowner".into(),
            title: "原标题".into(),
            cover: "https://example.com/owner.jpg".into(),
            published_at: "2026-08-12".into(),
            duration: 60,
        };
        // 监控调度器先按真实博主和来源写入。
        upsert_video(
            &pool,
            &meta,
            "真实博主",
            "https://space.bilibili.com/monitored",
            "",
        )
        .await
        .unwrap();

        // 用户只是浏览另一位博主的历史视频，已有记录归属不能被抢走。
        upsert_video_browse(&pool, &meta, "https://space.bilibili.com/browsed", "")
            .await
            .unwrap();
        let rows = get_videos(&pool, "", "", false).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].anchor, "真实博主");
        assert_eq!(rows[0].source_url, "https://space.bilibili.com/monitored");

        // 全新视频浏览时补一条，source_url 指向浏览来源，anchor 保持空。
        let new_meta = VideoMeta {
            id: "BVnew-browse".into(),
            platform: "bilibili".into(),
            url: "https://www.bilibili.com/video/BVnew-browse".into(),
            title: "新视频".into(),
            cover: String::new(),
            published_at: "2026-08-14".into(),
            duration: 10,
        };
        upsert_video_browse(&pool, &new_meta, "https://space.bilibili.com/browsed", "")
            .await
            .unwrap();
        let rows = get_videos(&pool, "", "", false).await.unwrap();
        assert_eq!(rows.len(), 2);
        let new_row = rows.iter().find(|r| r.id == "BVnew-browse").unwrap();
        assert_eq!(new_row.source_url, "https://space.bilibili.com/browsed");
        assert_eq!(new_row.anchor, "");
    }

    #[tokio::test]
    async fn mark_downloaded_by_url_updates_all_platform_records() {
        let pool = test_pool().await;
        // 非 B站平台：浏览历史入库时 id 使用 GUID，与下载完成后
        // extract_video_id 推导的完整 URL 不一致。按 id 标记不会命中这条记录。
        let meta = VideoMeta {
            id: "yt-guid-abc123".into(),
            platform: "rss".into(),
            url: "https://www.youtube.com/watch?v=abc123".into(),
            title: "油管视频".into(),
            cover: String::new(),
            published_at: "2026-08-12".into(),
            duration: 120,
        };
        upsert_video_browse(&pool, &meta, "https://rss.example.com/youtube", "")
            .await
            .unwrap();

        // 模拟下载完成：Rust 侧推导出的 id 是完整 URL，与库里的 GUID 不同，
        // 按 id 标记不命中；随后按 URL 标记必须命中浏览历史记录。
        mark_downloaded(&pool, "https://www.youtube.com/watch?v=abc123", true)
            .await
            .unwrap();
        assert!(
            !is_downloaded(&pool, "yt-guid-abc123").await.unwrap(),
            "按推导 URL 作为 id 标记不应命中 GUID id 的历史记录"
        );
        let updated = mark_downloaded_by_url(&pool, "https://www.youtube.com/watch?v=abc123", true)
            .await
            .unwrap();
        assert_eq!(updated, 1, "应按 URL 命中浏览历史记录");
        assert!(is_downloaded(&pool, "yt-guid-abc123").await.unwrap());

        // 同一 URL 在 B站监控源和 RSS 浏览里各有一条时，按 URL 标记应全部命中。
        let bili = VideoMeta {
            id: "BVurl".into(),
            platform: "bilibili".into(),
            url: "https://www.bilibili.com/video/BVurl".into(),
            title: "B站同源".into(),
            cover: String::new(),
            published_at: "2026-08-13".into(),
            duration: 60,
        };
        upsert_video(&pool, &bili, "博主", "https://space.bilibili.com/1", "")
            .await
            .unwrap();
        let updated = mark_downloaded_by_url(&pool, "https://www.bilibili.com/video/BVurl", true)
            .await
            .unwrap();
        assert_eq!(updated, 1);
        assert!(is_downloaded(&pool, "BVurl").await.unwrap());
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
    async fn source_failure_cooldown_bumps_and_resets() {
        let pool = test_pool().await;
        let url = "https://space.bilibili.com/cooldown";
        assert_eq!(
            source_failure_state(&pool, url).await.unwrap(),
            (0, 0),
            "未失败的源应无冷却状态"
        );
        assert_eq!(bump_source_failures(&pool, url).await.unwrap(), 1);
        assert_eq!(bump_source_failures(&pool, url).await.unwrap(), 2);
        let (fails, last) = source_failure_state(&pool, url).await.unwrap();
        assert_eq!(fails, 2);
        assert!(last > 0, "失败时间应写入: {last}");
        reset_source_failures(&pool, url).await.unwrap();
        assert_eq!(
            source_failure_state(&pool, url).await.unwrap(),
            (0, 0),
            "成功后失败计数与时间都应清零"
        );
    }

    #[tokio::test]
    async fn source_health_summary_counts_failing_and_cooling() {
        let pool = test_pool().await;
        assert_eq!(source_health_summary(&pool).await.unwrap(), (0, 0, 0));

        let ok_url = "https://rss.example.com/ok";
        let fail_url = "https://rss.example.com/fail";
        let cooling_url = "https://rss.example.com/cooling";
        mark_source_baselined(&pool, ok_url).await.unwrap();
        for _ in 0..2 {
            bump_source_failures(&pool, fail_url).await.unwrap();
        }
        for _ in 0..4 {
            bump_source_failures(&pool, cooling_url).await.unwrap();
        }
        assert_eq!(
            source_health_summary(&pool).await.unwrap(),
            (3, 2, 1),
            "应分别统计总源数、连续失败源数、冷却中源数"
        );
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

    #[tokio::test]
    async fn ended_session_cannot_be_overwritten_by_stale_waiter() {
        let pool = test_pool().await;
        let id = start_live_session(
            &pool,
            "https://live.bilibili.com/999",
            "主播",
            "测试直播",
            "bilibili",
            "https://stream.example/live.m3u8",
            123,
            "/tmp/x.ts",
        )
        .await
        .unwrap();

        // daemon 停止时先把会话标记为 stopped；随后后台 ffmpeg 等待任务醒来。
        assert!(live_session_recording(&pool, id).await.unwrap());
        end_live_session(&pool, id, "stopped", "服务停止")
            .await
            .unwrap();
        assert!(!live_session_recording(&pool, id).await.unwrap());

        // 迟到的收尾调用不能再把它覆盖成 failed。
        end_live_session(&pool, id, "failed", "录制退出 code=1")
            .await
            .unwrap();
        let sessions = list_live_sessions(&pool, "stopped", 10).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].status, "stopped");
        assert_eq!(sessions[0].error, "服务停止");
    }

    #[tokio::test]
    async fn live_state_persists_and_updates() {
        let pool = test_pool().await;
        let url = "https://live.bilibili.com/320";
        assert!(!get_live_state(&pool, url).await.unwrap());
        set_live_state(&pool, url, true).await.unwrap();
        assert!(get_live_state(&pool, url).await.unwrap());
        set_live_state(&pool, url, false).await.unwrap();
        assert!(!get_live_state(&pool, url).await.unwrap());
    }

    #[tokio::test]
    async fn set_live_state_clears_failure_counter() {
        let pool = test_pool().await;
        let url = "https://live.example.test/state-clears-fail";
        bump_live_failures(&pool, url).await.unwrap();
        bump_live_failures(&pool, url).await.unwrap();
        assert_eq!(get_live_failures(&pool, url).await.unwrap(), 2);
        set_live_state(&pool, url, true).await.unwrap();
        assert_eq!(
            get_live_failures(&pool, url).await.unwrap(),
            0,
            "拿到明确开播结论后失败计数必须清零"
        );
        assert!(get_live_state(&pool, url).await.unwrap());
        set_live_state(&pool, url, false).await.unwrap();
        assert_eq!(
            get_live_failures(&pool, url).await.unwrap(),
            0,
            "拿到明确未开播结论后失败计数也必须清零"
        );
    }

    #[tokio::test]
    async fn live_failure_counter_bumps_and_resets() {
        let pool = test_pool().await;
        let url = "https://live.example.test/failure-counter";
        let other = "https://live.example.test/other-failure";
        assert_eq!(get_live_failures(&pool, url).await.unwrap(), 0);
        assert_eq!(bump_live_failures(&pool, url).await.unwrap(), 1);
        assert_eq!(bump_live_failures(&pool, url).await.unwrap(), 2);
        assert_eq!(get_live_failures(&pool, other).await.unwrap(), 0);
        bump_live_failures(&pool, other).await.unwrap();
        assert_eq!(
            get_live_failures(&pool, url).await.unwrap(),
            2,
            "不同直播源的失败计数必须互不影响"
        );
        assert_eq!(get_live_failures(&pool, url).await.unwrap(), 2);
        reset_live_failures(&pool, url).await.unwrap();
        assert_eq!(get_live_failures(&pool, url).await.unwrap(), 0);
        assert_eq!(get_live_failures(&pool, other).await.unwrap(), 1);
        assert!(!get_live_state(&pool, url).await.unwrap());
    }
}
