use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use serde_json::{Value, json};
use sqlx::sqlite::SqlitePool;

use crate::config::Config;
use crate::{db, pipeline_bridge, service, video_library, watchlist};

/// 组装桌面端 `status` 接口需要的一份完整状态快照。
///
/// 该函数刻意保持“只读、可独立测试”：不启动服务，也不写任何状态文件。
/// CLI、MCP 和桌面兼容层都可以复用同一个输出结构，避免字段在多个入口之间漂移。
pub async fn build(project_dir: &Path, pool: &SqlitePool, cfg: &Config) -> Result<Value> {
    let downloads = project_dir.join("downloads");
    let fupan_dir = project_dir.join("fupan_output");
    let logs_dir = project_dir.join("logs");

    let anchors = db::list_anchors(pool).await?;
    let watch = watchlist::Watchlist::new(project_dir.to_path_buf());
    let live = watch.list_live();
    let video_sources = watch.list_video();
    let library = video_library::VideoLibrary::new(pool.clone(), fupan_dir.clone());
    let videos = library.list("", "", false).await?;
    let recent_recordings = recent_recordings(&downloads, project_dir);
    let notes = pipeline_bridge::list_notes(&fupan_dir, "")?;
    let rust_running = service::rust_daemon_alive(project_dir);

    Ok(json!({
        "ok": true,
        "anchors": anchors,
        "live_watchlist": live,
        "video_watchlist": video_sources,
        "video_count": videos.len(),
        "email": cfg.email_status(),
        "rust_daemon": rust_running,
        "services": {
            "live_monitor": {"running": rust_running},
            "video_monitor": {"running": rust_running},
        },
        "recent_recordings": recent_recordings,
        "notes": notes,
        "paths": {
            "recordings": downloads.display().to_string(),
            "fupan_output": fupan_dir.display().to_string(),
            "logs": logs_dir.display().to_string(),
        },
        "version": env!("CARGO_PKG_VERSION"),
    }))
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
mod tests {
    use super::*;

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
        assert!(status["notes"].as_array().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
