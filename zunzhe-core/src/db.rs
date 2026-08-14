use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::Context;
use sqlx::Row;
use sqlx::sqlite::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};

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
    attempts   INTEGER NOT NULL DEFAULT 0,
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
    Ok(())
}

/// 打开生产 SQLite 连接：WAL 允许 daemon 内多个并发任务边写边读，
/// busy_timeout 让短锁冲突等待而不是立刻报错，保证 24×30 长时间运行稳定。
pub fn open_options(path: &Path) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(std::time::Duration::from_secs(15))
        .foreign_keys(true)
}

/// 供测试/调用方直接建立 SQLite 池。
pub async fn open_pool(path: &Path) -> anyhow::Result<SqlitePool> {
    let pool = SqlitePool::connect_with(open_options(path))
        .await
        .context("连接 SQLite 数据库")?;
    init_db(&pool).await?;
    Ok(pool)
}

/// 独立 CLI/MCP/看门狗入口的数据库维护节流。
///
/// 单靠 daemon 主循环每 6 小时维护会在以下场景漏掉：MCP server 长驻且
/// daemon 未运行、CLI 反复单独调用、桌面端看门狗只调 `doctor`。本函数在
/// 每次调用时检查 `fupan_output/run/zunzhe-core.maintenance` 的修改时间，
/// 超过 6 小时才执行一次，避免每个 CLI 进程都重复跑 checkpoint/清理。
pub async fn maintenance_if_due(project_dir: &Path, pool: &SqlitePool) -> anyhow::Result<bool> {
    let run_dir = project_dir.join("fupan_output").join("run");
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
            let tmp = run_dir.join("zunzhe-core.maintenance.tmp");
            std::fs::write(&tmp, chrono::Local::now().to_rfc3339())?;
            std::fs::rename(&tmp, &marker)?;
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
    Ok(())
}

#[cfg(test)]
mod maintenance_tests {
    use super::*;

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
    use std::time::SystemTime;

    fn temp_project(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let project = std::env::temp_dir().join(format!(
            "zunzhe-maint-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&project);
        std::fs::create_dir_all(project.join("fupan_output").join("run")).unwrap();
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
        let marker = project
            .join("fupan_output")
            .join("run")
            .join("zunzhe-core.maintenance");
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
        sqlx::query("INSERT INTO tasks (kind, url, name, status) VALUES (?, ?, ?, 'pending')")
            .bind(kind)
            .bind(url)
            .bind(name)
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
        sqlx::query("UPDATE tasks SET status='failed', updated_at=datetime('now','localtime') WHERE status='pending' AND attempts >= 3")
            .execute(pool)
            .await
            .context("落定超出重试上限的任务")?;
    } else {
        sqlx::query("UPDATE tasks SET status='failed', updated_at=datetime('now','localtime') WHERE status='pending' AND kind=? AND attempts >= 3")
            .bind(kind)
            .execute(pool)
            .await
            .context("落定超出重试上限的任务")?;
    }
    let mut sql = String::from(
        "SELECT id, kind, url, name, status, attempts, payload, created_at, updated_at
         FROM tasks
         WHERE status = 'pending'",
    );
    if !kind.is_empty() {
        sql.push_str(" AND kind = ?");
    }
    sql.push_str(" ORDER BY id LIMIT ?");
    let mut query = sqlx::query(&sql);
    if !kind.is_empty() {
        query = query.bind(kind);
    }
    query = query.bind(limit);
    let row = query.fetch_optional(pool).await.context("查询待处理任务")?;
    let Some(row) = row else {
        return Ok(None);
    };
    let id: i64 = row.try_get("id")?;
    let updated = sqlx::query(
        "UPDATE tasks SET status='processing', attempts=attempts+1, updated_at=datetime('now','localtime') WHERE id=? AND status = 'pending'",
    )
    .bind(id)
    .execute(pool)
    .await
    .context("认领下载任务")?;
    if updated.rows_affected() == 0 {
        return Ok(None);
    }
    let attempts: i64 = row.try_get("attempts")?;
    Ok(Some(Task {
        id,
        kind: row.try_get("kind")?,
        url: row.try_get("url")?,
        name: row.try_get("name")?,
        status: "processing".to_string(),
        // 认领时 DB 里已经 `attempts+1`，这里返回最新值，调用方据此决定是否落定 failed。
        attempts: attempts + 1,
        payload: row.try_get("payload")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    }))
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
        let pending = list_notify(&pool, "pending", 10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].attempts, 0);
        mark_notify_done(&pool, nid).await.unwrap();
        assert_eq!(list_notify(&pool, "pending", 10).await.unwrap().len(), 0);
        assert_eq!(list_notify(&pool, "sent", 10).await.unwrap()[0].attempts, 1);
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
        assert!(
            rows.iter().any(|t| t.id == id),
            "超上限的 pending 任务应自愈为 failed: {rows:?}"
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
