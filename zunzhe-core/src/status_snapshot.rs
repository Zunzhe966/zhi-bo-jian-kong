use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use serde_json::{Value, json};
use sqlx::sqlite::SqlitePool;

use crate::config::Config;
use crate::{db, pipeline_bridge, service, watchlist};

/// `file_scan_cache` 的进程内缓存项；字段名即缓存键/载荷，避免用元组表达复杂类型。
struct FileScanCache {
    downloads: PathBuf,
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
    let downloads = project_dir.join("downloads");
    let fupan_dir = project_dir.join("fupan_output");
    let logs_dir = project_dir.join("logs");

    let watch = watchlist::Watchlist::new(project_dir.to_path_buf());
    let anchors = watch.live_anchor_view();
    let live = watch.list_live();
    let video_sources = watch.list_video();
    let (recent_recordings, notes) = file_scan_cache(&downloads, &fupan_dir, project_dir)?;
    let recording_sessions = db::list_live_sessions(pool, "recording", 100).await?;
    let rust_running = service::rust_daemon_alive(project_dir);
    let heartbeat_ok = service::heartbeat_fresh_public(project_dir);
    let (uptime_secs, uptime_human) = service::daemon_uptime_public(project_dir);
    let video_count = db::count_videos(pool).await?;
    let switches = crate::download::PipelineSwitches::load(project_dir);

    Ok(json!({
        "ok": true,
        "anchors": anchors,
        "live_watchlist": live,
        "video_watchlist": video_sources,
        "video_count": video_count,
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
        "services": {
            "live_monitor": {"running": rust_running},
            "video_monitor": {"running": rust_running},
        },
        "recent_recordings": recent_recordings,
        "recording_sessions": recording_sessions,
        "notes": notes,
        "paths": {
            "recordings": downloads.display().to_string(),
            "fupan_output": fupan_dir.display().to_string(),
            "logs": logs_dir.display().to_string(),
        },
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
    downloads: &Path,
    fupan_dir: &Path,
    project_dir: &Path,
) -> Result<(Vec<Value>, Vec<Value>)> {
    let key_downloads = downloads.to_path_buf();
    let key_fupan = fupan_dir.to_path_buf();
    let key_project = project_dir.to_path_buf();
    {
        let guard = FILE_SCAN_CACHE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(cached) = guard.as_ref()
            && cached.downloads == key_downloads
            && cached.fupan_dir == key_fupan
            && cached.project_dir == key_project
            && cached.scanned_at.elapsed() < Duration::from_secs(30)
        {
            return Ok((cached.recordings.clone(), cached.notes.clone()));
        }
    }
    let recent = recent_recordings(&key_downloads, &key_project);
    let notes = pipeline_bridge::list_notes(&key_fupan, "")?;
    let mut guard = FILE_SCAN_CACHE.lock().unwrap_or_else(|p| p.into_inner());
    *guard = Some(FileScanCache {
        downloads: key_downloads,
        fupan_dir: key_fupan,
        project_dir: key_project,
        recordings: recent.clone(),
        notes: notes.clone(),
        scanned_at: Instant::now(),
    });
    Ok((recent, notes))
}

/// 递归扫描 `downloads` 最近媒体文件，格式与旧 Python `fupan/cli.py::status()`
/// 对齐。旧实现使用 `downloads.rglob('*.*')`，会覆盖 `B站直播/抖音直播/短视频/主播/...`
/// 下的录像；只扫顶层会导致桌面端“最近录制”长期为空。
fn recent_recordings(downloads: &Path, project_dir: &Path) -> Vec<Value> {
    let mut files: Vec<(PathBuf, SystemTime, u64)> = walk_media_files(downloads)
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
            json!({
                "file": path
                    .strip_prefix(project_dir)
                    .unwrap_or(&path)
                    .to_string_lossy(),
                "size_mb": size_mb,
                "minutes_ago": age_secs / 60,
                "probably_recording": age_secs < 120,
            })
        })
        .collect()
}

/// 递归收集目录下的媒体文件，避免路径顺序影响结果，便于排序和测试。
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
                out.push(p);
            } else if p.is_dir() {
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
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-status-rec-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let downloads = dir.join("downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        std::fs::write(downloads.join("old.mp4"), b"12345").unwrap();
        std::fs::write(downloads.join("new.mkv"), b"1234567890").unwrap();
        std::fs::create_dir_all(downloads.join("B站直播").join("主播甲")).unwrap();
        std::fs::write(
            downloads.join("B站直播").join("主播甲").join("deep.ts"),
            b"deep",
        )
        .unwrap();

        let rows = recent_recordings(&downloads, &dir);
        assert_eq!(rows.len(), 3, "应递归扫描 downloads 下的媒体文件");
        assert!(
            rows.iter().any(|row| row["file"]
                .as_str()
                .unwrap()
                .contains("B站直播/主播甲/deep.ts")),
            "桌面端最近录制应包含子目录录像: {rows:?}"
        );
        assert_eq!(rows[0]["size_mb"].as_f64().unwrap(), 0.0);
        assert!(rows[0]["probably_recording"].as_bool().unwrap());
        assert!(rows[0]["file"].as_str().unwrap().starts_with("downloads/"));
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
        std::fs::write(dir.join("config").join("config.ini"), "[录制设置]\n").unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "[视频监控]\n").unwrap();
        let cfg = crate::config::Config::load(&dir).unwrap();
        let db_path = dir.join("test.db");
        let pool = crate::db::open_pool(&db_path).await.unwrap();

        let status = build(&dir, &pool, &cfg).await.unwrap();
        assert_eq!(status["ok"], serde_json::json!(true));
        assert_eq!(
            status["paths"]["recordings"],
            dir.join("downloads").display().to_string()
        );
        assert_eq!(
            status["paths"]["fupan_output"],
            dir.join("fupan_output").display().to_string()
        );
        assert_eq!(
            status["paths"]["logs"],
            dir.join("logs").display().to_string()
        );
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
        let downloads = dir.join("downloads");
        let fupan = dir.join("fupan_output");
        std::fs::create_dir_all(&downloads).unwrap();
        std::fs::create_dir_all(&fupan).unwrap();
        std::fs::write(downloads.join("one.mp4"), b"1").unwrap();

        let (first_rec, first_notes) = file_scan_cache(&downloads, &fupan, &dir).unwrap();
        assert_eq!(first_rec.len(), 1);
        assert!(first_notes.is_empty());

        // TTL 内新增文件不应触发全量扫描；30 秒窗口与桌面端轮询间隔一致。
        std::fs::write(downloads.join("two.mkv"), b"2").unwrap();
        std::fs::create_dir_all(fupan.join("主播甲").join("场次1")).unwrap();
        let (second_rec, second_notes) = file_scan_cache(&downloads, &fupan, &dir).unwrap();
        assert_eq!(second_rec.len(), 1, "TTL 内应复用上一次扫描结果");
        assert_eq!(second_notes.len(), 0);

        clear_file_scan_cache_for_tests();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
