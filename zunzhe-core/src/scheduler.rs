use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use sqlx::sqlite::SqlitePool;
use tokio::time::Instant;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::db;
use crate::download;
use crate::notify::{Channel, Notifier};
use crate::platform;
use crate::video_library::VideoLibrary;
use crate::watchlist::Watchlist;

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
    let watch = Watchlist::new(project_dir.to_path_buf());
    let sources = watch.list_video();
    let library = VideoLibrary::new(pool.clone(), fupan_dir.to_path_buf());
    let notifier = Notifier::new(pool, config);
    let mut checked = 0usize;
    let mut notified = 0usize;
    let mut enqueued = 0usize;
    let mut items: Vec<(String, String, Option<bool>)> = sources
        .into_iter()
        .map(|s| (s.url, s.name, s.download))
        .collect();
    for anchor in anchors {
        if !items.iter().any(|(u, _, _)| *u == anchor.url) {
            items.push((anchor.url, anchor.name, None));
        }
    }
    let auto_download = config.is_yes("视频监控", "自动下载新视频(是/否)", true);
    let notify_enabled = config.is_yes("复盘邮件", "发现新视频时发送通知(是/否)", true);
    for (source_url, name, item_download) in items {
        let adapter = platform::resolve(&source_url);
        match adapter.list_videos(&source_url, 10).await {
            Ok(videos) => {
                let known_ids: Vec<String> = db::get_videos(pool, "", "", false)
                    .await?
                    .iter()
                    .map(|v| v.id.clone())
                    .collect();
                let first_run = !db::is_source_baselined(pool, &source_url).await?;
                for v in &videos {
                    let is_new = !known_ids.contains(&v.id);
                    match library.upsert(v, &name, &source_url).await {
                        // 首次监控：只把现有视频写入库作为基线，不通知、不下载。
                        Ok(_) if is_new && first_run => {}
                        Ok(_) if is_new => {
                            if notify_enabled {
                                let title = "【尊者直播监控】博主发布新视频".to_string();
                                let content =
                                    format!("博主：{}\n标题：{}\n链接：{}", name, v.title, v.url);
                                match notifier
                                    .send(
                                        "video",
                                        &name,
                                        &title,
                                        &content,
                                        Channel::parse(&watch.notify_for(&source_url, &name)),
                                    )
                                    .await
                                {
                                    Ok(_) => notified += 1,
                                    Err(e) => warn!("通知新视频失败: {e:#}"),
                                }
                            }
                            let should_download = item_download.unwrap_or(auto_download);
                            if should_download {
                                match db::enqueue_task(pool, "download", &v.url, &name).await {
                                    Ok(_) => enqueued += 1,
                                    Err(e) => warn!("下载任务入队失败: {e:#}"),
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(e) => warn!("写入视频库失败: {e:#}"),
                    }
                }
                if first_run {
                    db::mark_source_baselined(pool, &source_url).await?;
                    info!(
                        "source {source_url} first baseline recorded ({})",
                        videos.len()
                    );
                }
                checked += 1;
            }
            Err(e) => error!("source {source_url} check failed: {e:#}"),
        }
    }

    Ok(TickStats {
        downloads: downloads + enqueued,
        anchors_checked: checked,
        videos_notified: notified,
    })
}

/// 供 CLI check-videos 复用的一轮检查。
pub async fn check_video_sources(
    pool: &SqlitePool,
    project_dir: &Path,
    config: &Config,
) -> Result<TickStats> {
    let fupan_dir = project_dir.join("fupan_output");
    tick(pool, project_dir, &fupan_dir, config).await
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
