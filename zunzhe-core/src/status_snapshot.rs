use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use serde_json::{Value, json};
use sqlx::sqlite::SqlitePool;

use crate::config::Config;
use crate::{db, pipeline_bridge, service, watchlist};

/// `file_scan_cache` 的进程内缓存项；字段名即缓存键/载荷，避免用元组表达复杂类型。
struct FileScanCache {
    media_root: PathBuf,
    fupan_dir: PathBuf,
    project_dir: PathBuf,
    recordings: Vec<Value>,
    notes: Vec<Value>,
    scanned_at: Instant,
}

static FILE_SCAN_CACHE: std::sync::Mutex<Option<FileScanCache>> = std::sync::Mutex::new(None);

/// 组装桌面端 `status` 接口需要的一份完整状态快照。
///
/// 该函数刻意保持“只读、可独立测试”：不启动服务，也不写任何状态文件。
/// CLI、MCP 和桌面兼容层都可以复用同一个输出结构，避免字段在多个入口之间漂移。
pub async fn build(project_dir: &Path, pool: &SqlitePool, cfg: &Config) -> Result<Value> {
    let dirs = crate::paths::storage(project_dir);
    let media_root = dirs.media_root.clone();
    let fupan_dir = dirs.fupan.clone();
    let logs_dir = dirs.logs();
    // 直播录像落点：`[录制设置] 直播保存路径` 填了就用它（与 live_monitor 同口径），
    // 否则落在产物根（布局 {产物根}/{主播}/直播/...）。界面「打开录制目录」
    // 必须开真正的录像目录。
    let recordings_dir = dirs.recordings();

    let watch = watchlist::Watchlist::new(project_dir.to_path_buf());
    let anchors = watch.live_anchor_view();
    let live = watch.list_live();
    let video_sources = watch.list_video();
    let (recent_recordings, notes) = file_scan_cache(
        &media_root,
        &fupan_dir,
        project_dir,
        &recordings_dir,
        &dirs.video_root,
    )?;
    let recording_sessions = db::list_live_sessions(pool, "recording", 100).await?;
    let rust_running = service::rust_daemon_alive(project_dir);
    let heartbeat_ok = service::heartbeat_fresh_public(project_dir);
    let (uptime_secs, uptime_human) = service::daemon_uptime_public(project_dir);
    let video_count = db::count_videos(pool).await?;
    // 转写按钮写入的是 kind=process 队列。桌面端必须看到队列生命周期，
    // 否则只能显示“已加入队列”，用户无法判断它是否真的在跑、失败原因是什么。
    let process_tasks = db::list_tasks_by_kind(pool, "process", 500)
        .await?
        .into_iter()
        .map(|task| {
            json!({
                "id": task.id,
                "media_path": task.url,
                "name": task.name,
                "status": task.status,
                "attempts": task.attempts,
                "last_error": task.last_error,
                "next_retry_at": task.next_retry_at,
                "created_at": task.created_at,
                "updated_at": task.updated_at,
            })
        })
        .collect::<Vec<_>>();
    let switches = crate::download::PipelineSwitches::load(project_dir);
    // 平台授权状态：抖音需要浏览器 Cookie（douyin_browser_cookies.txt），
    // 没有它时后端每轮都会跳过该源。桌面端据此显示「未授权」而非伪装可用。
    let douyin_authorized = crate::paths::run_dir(project_dir)
        .join("douyin_browser_cookies.txt")
        .exists();
    let bili_authorized = crate::paths::run_dir(project_dir)
        .join("bilibili_browser_cookies.txt")
        .exists();
    // 与 CLI/MCP doctor 同口径：源进入冷却后会静默不通知/不下载，
    // 桌面端 status 必须能暴露该健康风险，不能只显示“服务运行中”。
    // 读取失败按“未知”处理，不让状态快照整体失败。
    let source_health = match db::source_health_summary(pool).await {
        Ok((total, failing, cooling)) => {
            // 连续失败（哪怕还没到冷却阈值）也说明源近期不稳定，不能显示“健康”；
            // 与 doctor/长跑审计的 failing 语义保持一致。
            json!({"total": total, "failing": failing, "cooling": cooling, "ok": failing == 0 && cooling == 0})
        }
        Err(_) => {
            json!({"total": null, "failing": null, "cooling": null, "ok": null})
        }
    };
    // 通知投递健康：邮箱/微信等通道连续失败或配置缺失时，通知表会堆积
    // failed/pending；桌面端 status 必须能看到，不能只看“daemon 在跑”。
    let notify_health = match db::notify_health_summary(pool).await {
        Ok((pending, failed, sent)) => {
            let configured = cfg.email_configured() || cfg.push_channels().configured();
            let latest_failed_channel = db::latest_failed_notify_channel(pool).await.ok().flatten();
            json!({
                "pending": pending,
                "failed": failed,
                "sent": sent,
                "configured": configured,
                "ok": failed == 0 && pending == 0 && configured,
                "latest_failed_channel": latest_failed_channel,
                "hint": db::notify_health_hint(pending, failed, configured, latest_failed_channel.as_deref()),
            })
        }
        Err(_) => {
            json!({
                "pending": null,
                "failed": null,
                "sent": null,
                "configured": null,
                "ok": null,
                "latest_failed_channel": null,
                "hint": "通知健康查询失败，请查看 daemon 日志",
            })
        }
    };

    // 桌面端「打开目录」按钮的路径来源。以前只有三个字段，且白名单在 Electron
    // 侧写死成项目目录，导致产物搬到别处后按钮静默失效；现在把解析出来的全部
    // 目录一并上报（含 media_root / covers / run / database / customized），
    // 界面按需取用，不必猜路径。
    let mut paths = dirs.as_json();
    if let Some(obj) = paths.as_object_mut() {
        obj.insert(
            "recordings".into(),
            json!(recordings_dir.display().to_string()),
        );
        obj.insert("logs".into(), json!(logs_dir.display().to_string()));
    }

    Ok(json!({
        "ok": true,
        "anchors": anchors,
        "live_watchlist": live,
        "video_watchlist": video_sources,
        "platform_credentials": {"douyin": douyin_authorized, "bilibili": bili_authorized},
        "video_count": video_count,
        "process_tasks": process_tasks,
        "email": cfg.email_status(),
        "pipeline_switches": {
            "transcribe": switches.transcribe,
            "distill": switches.distill,
            "clips": switches.clips,
            "any_enabled": switches.any_enabled(),
        },
        "rust_daemon": rust_running,
        "heartbeat_ok": heartbeat_ok,
        "uptime_secs": uptime_secs,
        "uptime_human": uptime_human,
        "source_health": source_health,
        "notify_health": notify_health,
        "services": {
            "live_monitor": {"running": rust_running},
            "video_monitor": {"running": rust_running},
        },
        "recent_recordings": recent_recordings,
        "recording_sessions": recording_sessions,
        "notes": notes,
        "paths": paths,
        "log_paths": service::log_paths_diagnostics(project_dir),
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// 短 TTL 的目录扫描缓存。
///
/// `status` 由桌面端界面每 30 秒轮询调用；`recent_recordings` 会递归
/// 扫描整个 downloads，`notes` 会递归扫描 fupan_output。录制/复盘产物积累后，
/// 每次调用都做全量扫描会白白拖慢接口。缓存有效期 30 秒：与桌面端 30 秒轮询
/// 对齐，界面每次刷新不会重新全量扫盘；单次录制/复盘完成耗时通常远大于 30 秒，
/// 因此“最近录制/笔记列表”不会长期失真。daemon 不共享该进程内缓存，
/// 所以不涉及跨进程一致性问题。
fn file_scan_cache(
    media_root: &Path,
    fupan_dir: &Path,
    project_dir: &Path,
    recordings_dir: &Path,
    video_root: &Path,
) -> Result<(Vec<Value>, Vec<Value>)> {
    let key_media = media_root.to_path_buf();
    let key_fupan = fupan_dir.to_path_buf();
    let key_project = project_dir.to_path_buf();
    {
        let guard = FILE_SCAN_CACHE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(cached) = guard.as_ref()
            && cached.media_root == key_media
            && cached.fupan_dir == key_fupan
            && cached.project_dir == key_project
            && cached.scanned_at.elapsed() < Duration::from_secs(30)
        {
            return Ok((cached.recordings.clone(), cached.notes.clone()));
        }
    }
    let recent = recent_recordings(&key_media, &key_project);
    let notes = pipeline_bridge::list_notes(recordings_dir, video_root, &key_fupan, "")?;
    let mut guard = FILE_SCAN_CACHE.lock().unwrap_or_else(|p| p.into_inner());
    *guard = Some(FileScanCache {
        media_root: key_media,
        fupan_dir: key_fupan,
        project_dir: key_project,
        recordings: recent.clone(),
        notes: notes.clone(),
        scanned_at: Instant::now(),
    });
    Ok((recent, notes))
}

/// 递归扫描产物根下的最近媒体文件（布局 `{产物根}/{主播}/{直播|短视频|批量下载}/...`）。
/// 产物根下的系统目录（database/logs/downloads/fupan_output/covers/run）一律跳过，
/// 只收媒体文件——日志、数据库、转写文本不该出现在「最近录制」里。
fn recent_recordings(media_root: &Path, project_dir: &Path) -> Vec<Value> {
    let mut files: Vec<(PathBuf, SystemTime, u64)> = walk_media_files(media_root)
        .into_iter()
        .filter_map(|path| {
            let meta = std::fs::metadata(&path).ok()?;
            Some((path, meta.modified().ok()?, meta.len()))
        })
        .collect();
    files.sort_by_key(|(_, modified, _)| std::cmp::Reverse(*modified));
    files.truncate(10);

    let now = SystemTime::now();
    files
        .into_iter()
        .map(|(path, modified, size)| {
            let age_secs = now
                .duration_since(modified)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let size_mb = (size as f64 / 1_048_576.0 * 10.0).round() / 10.0;
            // “正在录制”不能只看文件 mtime：投稿视频下载完成时文件也很新，
            // 会把“刚下载完的短视频”误报成“正在录制的直播”（已实测：侯翠翠
            // 投稿下载后桌面端显示正在录制）。只有直播目录（B站直播/抖音直播
            // 等）下的近期文件才可能是进行中的直播录像。
            let is_live_dir = path.to_string_lossy().contains("直播");
            json!({
                "file": path
                    .strip_prefix(project_dir)
                    .unwrap_or(&path)
                    .to_string_lossy(),
                "size_mb": size_mb,
                "minutes_ago": age_secs / 60,
                "probably_recording": is_live_dir && age_secs < 120,
            })
        })
        .collect()
}

/// 递归收集产物根下的媒体文件。跳过系统目录（见 pipeline_bridge::NON_ANCHOR_DIRS）
/// 与隐藏目录；只收已知媒体后缀的文件，避免路径顺序影响结果，便于排序和测试。
fn walk_media_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                let is_media = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| {
                        crate::pipeline_bridge::MEDIA_SUFFIXES
                            .contains(&e.to_ascii_lowercase().as_str())
                    })
                    .unwrap_or(false);
                if is_media {
                    out.push(p);
                }
            } else if p.is_dir() {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if name.starts_with('.')
                    || crate::pipeline_bridge::NON_ANCHOR_DIRS.contains(&name.as_str())
                {
                    continue;
                }
                pending.push(p);
            }
        }
    }
    out
}

#[cfg(test)]
fn clear_file_scan_cache_for_tests() {
    let mut guard = FILE_SCAN_CACHE.lock().unwrap_or_else(|p| p.into_inner());
    *guard = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::VideoMeta;

    #[test]
    fn recent_recordings_are_sorted_and_relative() {
        // 布局：{产物根}/{主播}/{直播|短视频}/{场次}/。扫描必须覆盖分类目录、
        // 跳过系统目录（logs/database/fupan_output 等），且只收媒体文件。
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-status-rec-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let media = dir.join("产物");
        let video_dir = media.join("主播甲").join("短视频").join("某标题");
        std::fs::create_dir_all(&video_dir).unwrap();
        std::fs::write(video_dir.join("某标题.mp4"), b"12345").unwrap();
        std::fs::write(video_dir.join("某标题.转写.txt"), "文字").unwrap();
        let live_dir = media.join("主播甲").join("直播").join("2026-09-22 场次一");
        std::fs::create_dir_all(&live_dir).unwrap();
        std::fs::write(live_dir.join("场次一.ts"), b"deep").unwrap();
        // 系统目录里的文件绝不许进「最近录制」。
        std::fs::create_dir_all(media.join("logs")).unwrap();
        std::fs::write(media.join("logs").join("zunzhe-core.log"), "log").unwrap();
        std::fs::create_dir_all(media.join("fupan_output").join("旧")) .unwrap();
        std::fs::write(
            media.join("fupan_output").join("旧").join("旧.ts"),
            b"old",
        )
        .unwrap();

        let rows = recent_recordings(&media, &dir);
        assert_eq!(
            rows.len(),
            2,
            "只应收分类目录下的媒体文件（转写文本/系统目录除外）: {rows:?}"
        );
        assert!(
            rows.iter().any(|row| row["file"]
                .as_str()
                .unwrap()
                .contains("主播甲/直播/2026-09-22 场次一/场次一.ts")),
            "桌面端最近录制应包含直播目录下的直播录像: {rows:?}"
        );
        assert!(
            rows.iter().any(|row| row["file"]
                .as_str()
                .unwrap()
                .contains("主播甲/短视频/某标题/某标题.mp4")),
            "桌面端最近录制应包含短视频目录下的投稿: {rows:?}"
        );
        assert_eq!(rows[0]["size_mb"].as_f64().unwrap(), 0.0);
        assert!(rows[0]["probably_recording"].as_bool().unwrap());
        assert!(rows[0]["file"].as_str().unwrap().starts_with("产物/"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn snapshot_includes_desktop_paths_and_notes() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-status-snapshot-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::create_dir_all(dir.join("downloads")).unwrap();
        std::fs::create_dir_all(dir.join("fupan_output").join("主播甲").join("场次1")).unwrap();
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        std::fs::write(dir.join("logs").join("zunzhe-core.log"), "stdout\n").unwrap();
        std::fs::write(dir.join("logs").join("zunzhe-core.err.log"), "stderr\n").unwrap();
        std::fs::write(
            dir.join("config").join("config.ini"),
            format!("[录制设置]\n[路径设置]\n产物根目录 = {}\n", dir.display()),
        )
        .unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "[视频监控]\n").unwrap();
        let cfg = crate::config::Config::load(&dir).unwrap();
        let db_path = dir.join("zunzhe-core.db");
        let pool = crate::db::open_pool(&db_path).await.unwrap();
        crate::db::init_db(&pool).await.unwrap();

        let status = build(&dir, &pool, &cfg).await.unwrap();
        assert_eq!(status["ok"], serde_json::json!(true));
        assert_eq!(
            status["paths"]["recordings"],
            dir.display().to_string(),
            "新布局下录像落点默认是产物根本身（产物根/主播/直播/...）"
        );
        assert_eq!(
            status["paths"]["fupan_output"],
            dir.join("fupan_output").display().to_string()
        );
        assert_eq!(
            status["paths"]["logs"],
            dir.join("logs").display().to_string()
        );
        assert_eq!(
            status["paths"]["database"],
            dir.join("database")
                .join("zunzhe-core.db")
                .display()
                .to_string(),
            "状态页应上报统一的新数据根位置，旧库迁移由启动入口负责"
        );
        assert_eq!(
            status["log_paths"]["configured_dir"],
            dir.join("logs").display().to_string(),
            "桌面端 status 必须和 doctor 一样暴露日志落点诊断"
        );
        assert_eq!(status["log_paths"]["consistent"], true);
        assert_eq!(status["log_paths"]["primary"]["source"], "configured");
        assert!(status["heartbeat_ok"].is_boolean());
        assert!(status["uptime_secs"].is_number());
        assert!(status["uptime_human"].is_string());
        assert_eq!(
            status["pipeline_switches"]["transcribe"],
            serde_json::json!(true)
        );
        assert_eq!(
            status["pipeline_switches"]["distill"],
            serde_json::json!(true)
        );
        assert_eq!(
            status["pipeline_switches"]["clips"],
            serde_json::json!(true)
        );
        assert_eq!(
            status["pipeline_switches"]["any_enabled"],
            serde_json::json!(true)
        );
        assert!(status["notes"].as_array().is_some());
        assert_eq!(status["recording_sessions"], serde_json::json!([]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn snapshot_exposes_recent_process_queue_for_ui() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-status-process-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::create_dir_all(dir.join("downloads")).unwrap();
        std::fs::create_dir_all(dir.join("fupan_output")).unwrap();
        std::fs::write(dir.join("config").join("config.ini"), "[录制设置]\n").unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "[视频监控]\n").unwrap();
        let cfg = crate::config::Config::load(&dir).unwrap();
        let pool = crate::db::open_pool(&dir.join("test.db")).await.unwrap();

        let media = dir.join("downloads").join("真实媒体.mp4");
        crate::test_support::write_valid_wav(&media);
        let enqueue =
            crate::download::enqueue_process(&pool, &dir, media.to_str().unwrap(), "测试博主")
                .await
                .unwrap();
        let task_id = enqueue.task_id;
        let claimed = crate::db::claim_pending_task_by_kind(&pool, "process", 1)
            .await
            .unwrap()
            .expect("刚入队的 process 任务应可认领");
        assert_eq!(claimed.id, task_id);
        crate::db::record_task_failure(&pool, task_id, "缺少转写依赖：faster-whisper", "failed")
            .await
            .unwrap();

        let status = build(&dir, &pool, &cfg).await.unwrap();
        let tasks = status["process_tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1, "状态快照必须暴露 process 队列: {status}");
        assert_eq!(tasks[0]["id"].as_i64(), Some(task_id));
        assert_eq!(tasks[0]["status"], serde_json::json!("failed"));
        assert_eq!(tasks[0]["attempts"].as_i64(), Some(1));
        assert_eq!(
            tasks[0]["media_path"],
            serde_json::json!(media.display().to_string())
        );
        assert!(
            tasks[0]["last_error"]
                .as_str()
                .unwrap()
                .contains("faster-whisper")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn snapshot_keeps_old_completed_process_tasks_visible() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-status-process-history-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::create_dir_all(dir.join("downloads")).unwrap();
        std::fs::create_dir_all(dir.join("fupan_output")).unwrap();
        std::fs::write(dir.join("config").join("config.ini"), "[录制设置]\n").unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "[视频监控]\n").unwrap();
        let cfg = crate::config::Config::load(&dir).unwrap();
        let pool = crate::db::open_pool(&dir.join("test.db")).await.unwrap();

        let media = dir.join("downloads").join("旧视频.mp4");
        crate::test_support::write_valid_wav(&media);
        let old =
            crate::download::enqueue_process(&pool, &dir, media.to_str().unwrap(), "测试博主")
                .await
                .unwrap();
        crate::db::update_task_status(&pool, old.task_id, "done")
            .await
            .unwrap();
        for i in 0..25 {
            let newer = dir.join("downloads").join(format!("新视频-{i}.mp4"));
            crate::test_support::write_valid_wav(&newer);
            crate::download::enqueue_process(&pool, &dir, newer.to_str().unwrap(), "测试博主")
                .await
                .unwrap();
        }

        let status = build(&dir, &pool, &cfg).await.unwrap();
        let tasks = status["process_tasks"].as_array().unwrap();
        assert_eq!(
            tasks.len(),
            26,
            "状态快照不能只保留最近 20 条转写任务: {status}"
        );
        assert!(
            tasks
                .iter()
                .any(|task| task["id"].as_i64() == Some(old.task_id)),
            "较早完成的转写任务仍必须可见，否则视频详情会误显示成未转写"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn snapshot_video_count_does_not_load_all_rows() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-status-count-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::create_dir_all(dir.join("downloads")).unwrap();
        std::fs::create_dir_all(dir.join("fupan_output")).unwrap();
        std::fs::write(dir.join("config").join("config.ini"), "[录制设置]\n").unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "[视频监控]\n").unwrap();
        let cfg = crate::config::Config::load(&dir).unwrap();
        let pool = crate::db::open_pool(&dir.join("test.db")).await.unwrap();

        for i in 0..3 {
            crate::db::upsert_video(
                &pool,
                &VideoMeta {
                    id: format!("BVcount{i}"),
                    platform: "bilibili".into(),
                    url: format!("https://www.bilibili.com/video/BVcount{i}"),
                    title: format!("视频 {i}"),
                    cover: String::new(),
                    published_at: String::new(),
                    duration: 0,
                },
                "博主",
                "https://space.bilibili.com/1",
                "",
            )
            .await
            .unwrap();
        }

        let status = build(&dir, &pool, &cfg).await.unwrap();
        assert_eq!(status["video_count"].as_i64(), Some(3));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn snapshot_includes_recording_sessions_from_db() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-status-recording-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::create_dir_all(dir.join("downloads")).unwrap();
        std::fs::write(dir.join("config").join("config.ini"), "[录制设置]\n").unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "[视频监控]\n").unwrap();
        let cfg = crate::config::Config::load(&dir).unwrap();
        let db_path = dir.join("test.db");
        let pool = crate::db::open_pool(&db_path).await.unwrap();
        crate::db::init_db(&pool).await.unwrap();
        crate::db::start_live_session(
            &pool,
            "https://live.bilibili.com/1",
            "主播甲",
            "测试直播",
            "bilibili",
            "https://example.com/stream",
            1234,
            "downloads/B站直播/主播甲/test.ts",
        )
        .await
        .unwrap();

        let status = build(&dir, &pool, &cfg).await.unwrap();
        let sessions = status["recording_sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["url"], "https://live.bilibili.com/1");
        assert_eq!(sessions[0]["status"], "recording");
        assert_eq!(sessions[0]["anchor"], "主播甲");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_scan_cache_short_ttl_reuses_previous_scan() {
        clear_file_scan_cache_for_tests();
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-status-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let media = dir.join("产物");
        let fupan = dir.join("fupan_output");
        // 布局：{主播}/{短视频}/{标题}/，且带复盘产物才会进 notes 列表。
        let video_dir = media.join("主播甲").join("短视频").join("一");
        std::fs::create_dir_all(&video_dir).unwrap();
        std::fs::create_dir_all(&fupan).unwrap();
        std::fs::write(video_dir.join("一.复盘笔记.md"), "# 笔记").unwrap();
        std::fs::write(video_dir.join("一.mp4"), b"1").unwrap();

        let (first_rec, first_notes) = file_scan_cache(&media, &fupan, &dir, &media, &media).unwrap();
        assert_eq!(first_rec.len(), 1);
        assert_eq!(first_notes.len(), 1, "短视频目录下的复盘产物应被列出");

        // TTL 内新增文件不应触发全量扫描；30 秒窗口与桌面端轮询间隔一致。
        std::fs::write(video_dir.join("二.mkv"), b"2").unwrap();
        std::fs::create_dir_all(fupan.join("主播甲").join("场次1")).unwrap();
        let (second_rec, second_notes) = file_scan_cache(&media, &fupan, &dir, &media, &media).unwrap();
        assert_eq!(second_rec.len(), 1, "TTL 内应复用上一次扫描结果");
        assert_eq!(second_notes.len(), 1, "TTL 内应复用上一次笔记扫描结果");

        clear_file_scan_cache_for_tests();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn snapshot_includes_source_health() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-status-source-health-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::create_dir_all(dir.join("fupan_output").join("run")).unwrap();
        std::fs::write(dir.join("config").join("config.ini"), "[推送配置]\n").unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "[视频监控]\n").unwrap();
        std::fs::write(
            dir.join("config").join("config.ini.example"),
            "[推送配置]\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("config").join("fupan_config.ini.example"),
            "[视频监控]\n",
        )
        .unwrap();

        let file = std::env::temp_dir().join(format!(
            "zunzhe-status-source-health-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = crate::db::open_pool(&file).await.unwrap();
        crate::db::init_db(&pool).await.unwrap();
        let cfg = Config::load(&dir).unwrap();

        let snapshot = build(&dir, &pool, &cfg).await.unwrap();
        assert_eq!(
            snapshot["source_health"],
            json!({"total": 0, "failing": 0, "cooling": 0, "ok": true})
        );

        // 连续失败但未到冷却阈值（1-2 次）也必须判不健康；与 doctor/审计
        // 的 failing 语义一致，避免桌面端把正在抖动的源显示成“健康”。
        let failing_url = "https://rss.example.com/failing";
        crate::db::mark_source_baselined(&pool, failing_url)
            .await
            .unwrap();
        crate::db::bump_source_failures(&pool, failing_url)
            .await
            .unwrap();
        crate::db::bump_source_failures(&pool, failing_url)
            .await
            .unwrap();
        let snapshot = build(&dir, &pool, &cfg).await.unwrap();
        assert_eq!(
            snapshot["source_health"],
            json!({"total": 1, "failing": 1, "cooling": 0, "ok": false})
        );

        std::mem::forget(file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn snapshot_includes_notify_health() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-snapshot-notify-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(dir.join("config").join("config.ini"), "[推送配置]\n").unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "[视频监控]\n").unwrap();
        let db_path = dir.join("test.db");
        let pool = crate::db::open_pool(&db_path).await.unwrap();
        let cfg = crate::config::Config::load(&dir).unwrap();
        let status = build(&dir, &pool, &cfg).await.unwrap();
        let nh = &status["notify_health"];
        assert_eq!(nh["pending"], json!(0));
        assert_eq!(nh["failed"], json!(0));
        assert_eq!(nh["configured"], json!(false));
        assert_eq!(nh["ok"], json!(false));

        let id = crate::db::add_notify(&pool, "video", "博主", "标题", "内容", "微信")
            .await
            .unwrap();
        crate::db::fail_notify(&pool, id).await.unwrap();
        let status = build(&dir, &pool, &cfg).await.unwrap();
        assert_eq!(status["notify_health"]["failed"], json!(1));
        assert_eq!(status["notify_health"]["ok"], json!(false));
        assert_eq!(
            status["notify_health"]["latest_failed_channel"],
            json!("微信")
        );
        let hint = status["notify_health"]["hint"].as_str().unwrap();
        assert!(
            hint.contains("Server酱"),
            "未配置时应给出可操作提示: {hint}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
