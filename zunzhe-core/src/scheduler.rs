use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use sqlx::sqlite::SqlitePool;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::db;
use crate::download;
use crate::live_monitor::LiveMonitor;
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
    let fupan_dir = project_dir.join("fupan_output");
    let watch = Watchlist::new(project_dir.to_path_buf());
    let monitor = LiveMonitor::new(&pool, project_dir, config, &watch);
    let mut stop = tokio::sync::watch::channel(false);
    if let Some(limit) = duration_secs {
        let tx = stop.0.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(limit)).await;
            let _ = tx.send(true);
        });
    }
    loop {
        // Rust 直播监控独立于 Python 兼容层运行：检测开播、自动录制、状态持久化。
        run_tick_with_timeout(&monitor, &pool, project_dir, &fupan_dir, config).await;
        if once {
            let _ = monitor.stop_all_recordings().await;
            return Ok(());
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(interval_secs)) => {}
            stopped = stop.1.changed() => {
                if stopped.is_ok() && *stop.1.borrow() {
                    info!("daemon duration reached, exiting cleanly");
                    let _ = monitor.stop_all_recordings().await;
                    return Ok(());
                }
            }
        }
    }
}

/// 每轮 tick 限制 70 秒：单源最多 50 秒 + 下载/封面等余量。
/// 单次网络卡顿只记日志，不让 daemon 永久阻塞。
async fn run_tick_with_timeout(
    monitor: &LiveMonitor<'_>,
    pool: &SqlitePool,
    project_dir: &Path,
    fupan_dir: &Path,
    config: &Config,
) {
    let result = tokio::time::timeout(Duration::from_secs(70), async {
        let live = monitor.run_once().await;
        let videos = tick(pool, project_dir, fupan_dir, config).await;
        (live, videos)
    })
    .await;
    match result {
        Ok((live, videos)) => {
            match live {
                Ok(stats) => info!("live monitor tick ok: {:?}", stats),
                Err(e) => error!("live monitor tick failed: {e:#}"),
            }
            match videos {
                Ok(stats) => info!("daemon tick ok: {:?}", stats),
                Err(e) => error!("daemon tick failed: {e:#}"),
            }
        }
        Err(_) => warn!("daemon tick 超过 70 秒，本轮已跳过，继续下一轮"),
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
    let item_count = items.len();
    for (source_url, name, item_download) in items {
        let adapter = platform::resolve_with_project_dir(&source_url, project_dir);
        // 首次基线未建立时仍然检查；B站空间接口 403 等失败要明确返回错误，
        // 不能把“0 条视频”当成成功，避免用户误以为监控已正常工作。
        // 每个视频源最多 50 秒：B站官方/RSSHub/yt-dlp 多重兜底可能叠加较久，
        // 单源卡住不能拖垮整个 daemon 轮次。
        match tokio::time::timeout(
            Duration::from_secs(50),
            adapter.list_videos(&source_url, 10),
        )
        .await
        {
            Ok(Err(e)) => {
                error!("source {source_url} check failed: {e:#}");
                if item_count == 1 {
                    // 单源且是用户手动 check-videos：把失败直接暴露，便于排查。
                    return Err(anyhow::anyhow!("视频监控源检查失败 {source_url}: {e:#}"));
                }
            }
            Ok(Ok(videos)) => {
                let known_ids: Vec<String> = db::get_videos(pool, "", "", false)
                    .await?
                    .iter()
                    .map(|v| v.id.clone())
                    .collect();
                let first_run = !db::is_source_baselined(pool, &source_url).await?;
                for v in &videos {
                    // view 接口失败时只返回占位标题，跳过不写库，避免污染视频库。
                    if v.title == v.id {
                        continue;
                    }
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
            Err(_) => {
                error!("source {source_url} check timeout (50s)");
                if item_count == 1 {
                    return Err(anyhow::anyhow!("视频监控源检查超时（50 秒）: {source_url}"));
                }
            }
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
