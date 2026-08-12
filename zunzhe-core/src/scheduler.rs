use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use sqlx::sqlite::SqlitePool;
use tokio::time::Instant;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::db;
use crate::download;
use crate::model::VideoMeta;
use crate::notify::{Channel, Notifier};
use crate::platform;
use crate::video_library::VideoLibrary;

/// 24×30 守护调度器：固定间隔跑“检查视频源 + 处理下载队列”。
/// 任何单个任务失败只记日志，不拖垮主循环。
pub async fn run_daemon(
    pool: SqlitePool,
    project_dir: &Path,
    config: &Config,
    interval_secs: u64,
    once: bool,
    duration_secs: Option<u64>,
) -> Result<()> {
    let started = Instant::now();
    let fupan_dir = project_dir.join("fupan_output");
    loop {
        match tick(&pool, project_dir, &fupan_dir, config).await {
            Ok(stats) => info!("daemon tick ok: {:?}", stats),
            Err(e) => error!("daemon tick failed: {e:#}"),
        }
        if once {
            return Ok(());
        }
        if let Some(limit) = duration_secs
            && started.elapsed() >= Duration::from_secs(limit)
        {
            info!("daemon duration reached, exiting cleanly");
            return Ok(());
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(interval_secs)) => {}
        }
    }
}

async fn tick(
    pool: &SqlitePool,
    project_dir: &Path,
    fupan_dir: &Path,
    config: &Config,
) -> Result<TickStats> {
    // 1. 处理待下载任务
    let downloads = download::process_pending(pool, project_dir)
        .await
        .unwrap_or(0);

    // 2. 对每个视频监控源拉一次最新视频并写入库；新出现的不重复通知。
    let anchors = db::list_anchors(pool).await?;
    let library = VideoLibrary::new(pool.clone(), fupan_dir.to_path_buf());
    let notifier = Notifier::new(pool, config);
    let mut checked = 0usize;
    let mut notified = 0usize;
    for anchor in anchors {
        let adapter = platform::resolve(&anchor.url);
        match adapter.list_videos(&anchor.url, 10).await {
            Ok(videos) => {
                for v in videos {
                    if is_new_video(pool, &v).await {
                        match library.upsert(&v, &anchor.name, &anchor.url).await {
                            Ok(_) => {
                                let title = "【尊者直播监控】博主发布新视频".to_string();
                                let content = format!(
                                    "博主：{}\n标题：{}\n链接：{}",
                                    anchor.name, v.title, v.url
                                );
                                match notifier
                                    .send(
                                        "video",
                                        &anchor.name,
                                        &title,
                                        &content,
                                        Channel::parse(&anchor.notify_channel),
                                    )
                                    .await
                                {
                                    Ok(_) => notified += 1,
                                    Err(e) => warn!("通知新视频失败: {e:#}"),
                                }
                            }
                            Err(e) => warn!("写入视频库失败: {e:#}"),
                        }
                    } else {
                        let _ = library.upsert(&v, &anchor.name, &anchor.url).await;
                    }
                }
                checked += 1;
            }
            Err(e) => error!("anchor {} check failed: {e:#}", anchor.url),
        }
    }

    Ok(TickStats {
        downloads,
        anchors_checked: checked,
        videos_notified: notified,
    })
}

async fn is_new_video(pool: &SqlitePool, meta: &VideoMeta) -> bool {
    db::get_videos(pool, "", "", false)
        .await
        .map(|videos| !videos.iter().any(|v| v.id == meta.id))
        .unwrap_or(false)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TickStats {
    pub downloads: usize,
    pub anchors_checked: usize,
    pub videos_notified: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};

    #[tokio::test]
    async fn daemon_once_without_anchors() {
        let file = std::env::temp_dir().join(format!("zunzhe-daemon-{}", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let options = SqliteConnectOptions::new()
            .filename(&file)
            .create_if_missing(true);
        let pool = SqlitePool::connect_with(options).await.unwrap();
        db::init_db(&pool).await.unwrap();
        let cfg = crate::config::Config::load(Path::new(env!("CARGO_MANIFEST_DIR")))
            .unwrap_or_else(|_| crate::config::Config::defaults_for_test());
        let result = run_daemon(pool, Path::new("."), &cfg, 1, true, None).await;
        assert!(result.is_ok(), "{result:?}");
        let _ = std::fs::remove_file(&file);
    }
}
