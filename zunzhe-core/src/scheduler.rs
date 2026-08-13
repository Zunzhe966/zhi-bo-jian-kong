use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use sqlx::sqlite::SqlitePool;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::daemon_lock::DaemonLock;
use crate::db;
use crate::download;
use crate::live_monitor::{LiveCheckStats, LiveMonitor};
use crate::notify::Notifier;
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
    // 单实例锁必须贯穿整个 daemon 生命周期：重复 run-daemon 直接退出，
    // 防止两套调度同时检查/通知/下载同一批主播与视频源。
    let lock = DaemonLock::try_acquire(project_dir)
        .with_context(|| format!("获取 daemon 单实例锁失败: {}", project_dir.display()))?
        .ok_or_else(|| anyhow!("已有 Rust daemon 在本项目运行，本实例退出"))?;
    // pid 文件只有锁持有者写入/清理：第二实例拿不到锁时不会误删第一实例的 pid。
    crate::service::write_rust_daemon_pid(project_dir, std::process::id());
    let _pid_guard = DaemonPidGuard(project_dir);
    let _lock = lock;
    // 启动即写一条 uptime 日志记录：外部 doctor/桌面端据此计算连续运行时长。
    let uptime_log = project_dir.join("logs").join("zunzhe-core.uptime.log");
    if let Some(parent) = uptime_log.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let now = chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S %z")
        .to_string();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&uptime_log)
    {
        use std::io::Write;
        let _ = writeln!(f, "{now} started pid={}", std::process::id());
    }
    let fupan_dir = project_dir.join("fupan_output");
    // daemon 启动时合并旧 Python 独立视频库，确保 `已下载` 标记与历史视频
    // 元数据不会因切换主 DB 而丢失；迁移幂等，只补缺失 id，不覆盖主库字段。
    if let Err(e) = crate::video_library::migrate_legacy_video_db(&pool, &fupan_dir).await {
        warn!("旧视频库迁移失败，继续调度: {e:#}");
    }
    let watch = Watchlist::new(project_dir.to_path_buf());
    let monitor = LiveMonitor::new(&pool, project_dir, config, &watch);
    let mut stop = tokio::sync::watch::channel(false);
    // 独立下载队列 worker：不参与 70 秒 tick 护栏，每个任务自身有 600 秒超时。
    // 下载耗时不会阻塞直播/视频检查，也不会被 tick abort 后永远卡 processing。
    let mut download_worker = tokio::spawn(download::run_download_worker(
        pool.clone(),
        project_dir.to_path_buf(),
        stop.1.clone(),
        std::time::Duration::from_secs(600),
        std::time::Duration::from_secs(5),
    ));
    // 独立复盘队列 worker：下载/录制完成后自动转写、提炼、切片。
    let mut pipeline_worker = tokio::spawn(crate::pipeline_bridge::run_pipeline_worker(
        pool.clone(),
        project_dir.to_path_buf(),
        stop.1.clone(),
        std::time::Duration::from_secs(3600),
        std::time::Duration::from_secs(5),
    ));
    // 独立心跳记录器：每 60 秒把“daemon 进程仍在运行”写入 uptime 日志，
    // 不依赖 launchd/桌面端 watchdog；launchd 被 macOS TCC 挡在 ~/Desktop 外时
    // 也能持续留下 24×30 验收记录。
    let uptime_tx = stop.0.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            let now = chrono::Local::now()
                .format("%Y-%m-%d %H:%M:%S %z")
                .to_string();
            let line = format!(
                "{now} pid={} rust_daemon=true heartbeat_ok=true\n",
                std::process::id()
            );
            if let Some(parent) = uptime_log.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&uptime_log)
            {
                use std::io::Write;
                let _ = f.write_all(line.as_bytes());
            }
            if *uptime_tx.borrow() {
                break;
            }
        }
    });
    if let Some(limit) = duration_secs {
        let tx = stop.0.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(limit)).await;
            let _ = tx.send(true);
        });
    }
    loop {
        // Rust 直播监控独立于 Python 兼容层运行：检测开播、自动录制、状态持久化。
        // 每轮 tick 前刷新一次心跳，避免上一轮网络卡顿后健康判定陈旧。
        crate::service::touch_heartbeat(project_dir);
        let tick = run_tick_with_timeout(&pool, project_dir, &fupan_dir, config, &watch);
        tokio::pin!(tick);
        let mut exited = false;
        tokio::select! {
            _ = &mut tick => {}
            _ = stop.1.changed() => {
                info!("daemon duration reached, stopping current tick");
                exited = true;
            }
            _sig = shutdown_signal() => {
                info!("收到中断信号，daemon 优雅退出");
                exited = true;
            }
        }
        // tick 结束后立即刷新心跳：即使本轮被 70 秒护栏截断，也证明调度器仍活跃。
        crate::service::touch_heartbeat(project_dir);
        if once || exited || *stop.1.borrow() {
            let _ = monitor.stop_all_recordings().await;
            stop_download_worker(&pool, &stop.0, &mut download_worker).await;
            stop_pipeline_worker(&pool, &stop.0, &mut pipeline_worker).await;
            return Ok(());
        }
        // duration 未到点且 tick 提前结束后，等待下一轮；期间继续响应退出。
        let mut wait_exit = false;
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(interval_secs)) => {}
            _ = stop.1.changed() => {
                wait_exit = true;
            }
            _sig = shutdown_signal() => {
                wait_exit = true;
            }
        }
        if wait_exit || *stop.1.borrow() {
            let _ = monitor.stop_all_recordings().await;
            stop_download_worker(&pool, &stop.0, &mut download_worker).await;
            stop_pipeline_worker(&pool, &stop.0, &mut pipeline_worker).await;
            return Ok(());
        }
    }
}

/// 退出 daemon 时停止下载 worker：先通知停止，再 abort 当前下载并归还
/// processing 队列，避免优雅退出被 10 分钟下载拖住，也避免任务永久卡死。
async fn stop_download_worker(
    pool: &SqlitePool,
    stop: &tokio::sync::watch::Sender<bool>,
    worker: &mut tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let _ = stop.send(true);
    if !worker.is_finished() {
        worker.abort();
        let _ = worker.await;
    }
    let _ = db::recover_processing_by_kind(pool, "download").await;
}

/// 退出 daemon 时停止复盘 worker：abort 当前任务并归还 process 队列。
async fn stop_pipeline_worker(
    pool: &SqlitePool,
    stop: &tokio::sync::watch::Sender<bool>,
    worker: &mut tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let _ = stop.send(true);
    if !worker.is_finished() {
        worker.abort();
        let _ = worker.await;
    }
    let _ = db::recover_processing_by_kind(pool, "process").await;
}

/// 锁持有者的 pid 清理器：无论正常返回、错误返回还是 panic，都会移除本实例 pid。
struct DaemonPidGuard<'a>(&'a Path);

impl Drop for DaemonPidGuard<'_> {
    fn drop(&mut self) {
        crate::service::remove_rust_daemon_pid(self.0);
    }
}

/// 监听 SIGINT/SIGTERM；Windows 只处理 Ctrl-C。
async fn shutdown_signal() -> bool {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("注册 SIGTERM 失败");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => true,
            _ = term.recv() => true,
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.is_ok()
    }
}

/// 每轮 tick 限制 70 秒：单源最多 50 秒 + 下载/封面等余量。
/// 单次网络卡顿只记日志，不让 daemon 永久阻塞。
async fn run_tick_with_timeout(
    pool: &SqlitePool,
    project_dir: &Path,
    fupan_dir: &Path,
    config: &Config,
    watch: &Watchlist,
) {
    // 用 owned 快照 + tokio::task::spawn 隔离：单个 tick panic 不会拖垮主循环。
    let pool = pool.clone();
    let project_dir = project_dir.to_path_buf();
    let fupan_dir = fupan_dir.to_path_buf();
    let config = config.clone();
    let watch = Watchlist::new(
        watch
            .url_config()
            .parent()
            .unwrap_or(project_dir.as_path())
            .to_path_buf(),
    );
    let mut live_task = tokio::task::spawn(run_live_tick(
        pool.clone(),
        project_dir.clone(),
        config.clone(),
        watch,
    ));
    let mut videos_task = tokio::task::spawn(tick_owned(pool, project_dir, fupan_dir, config));

    let outcome = tokio::time::timeout(Duration::from_secs(70), async {
        // 借用句柄而不是移动：超时分支发生后仍然能 abort 两个子任务。
        let (live, videos) = tokio::join!(&mut live_task, &mut videos_task);
        let live = match live {
            Ok(v) => v,
            Err(e) => Err(anyhow::anyhow!("直播检查任务 panic: {e}")),
        };
        let videos = match videos {
            Ok(v) => v,
            Err(e) => Err(anyhow::anyhow!("视频检查任务 panic: {e}")),
        };
        (live, videos)
    })
    .await;

    if let Ok((live, videos)) = outcome {
        match live {
            Ok(stats) => info!("live monitor tick ok: {:?}", stats),
            Err(e) => error!("live monitor tick failed: {e:#}"),
        }
        match videos {
            Ok(stats) => info!("daemon tick ok: {:?}", stats),
            Err(e) => error!("daemon tick failed: {e:#}"),
        }
    } else {
        warn!("daemon tick 超过 70 秒，终止本轮子任务并跳过");
        abort_timed_out_tasks(live_task, videos_task).await;
    }
}

/// 超时收尾：只 abort 尚未完成的任务并等待收尾。
/// 已完成的 JoinHandle 在上层 `join!` 中已被 poll 过，不能再 await，
/// 否则 tokio 会 panic（真实线上曾因“JoinHandle polled after completion”崩溃）。
async fn abort_timed_out_tasks<T, U>(a: tokio::task::JoinHandle<T>, b: tokio::task::JoinHandle<U>) {
    if !a.is_finished() {
        a.abort();
        let _ = a.await;
    }
    if !b.is_finished() {
        b.abort();
        let _ = b.await;
    }
}

async fn run_live_tick(
    pool: SqlitePool,
    project_dir: PathBuf,
    config: Config,
    watch: Watchlist,
) -> Result<LiveCheckStats> {
    let monitor = LiveMonitor::new(&pool, &project_dir, &config, &watch);
    monitor.run_once().await
}

async fn tick_owned(
    pool: SqlitePool,
    project_dir: PathBuf,
    fupan_dir: PathBuf,
    config: Config,
) -> Result<TickStats> {
    tick(&pool, &project_dir, &fupan_dir, &config).await
}

async fn tick(
    pool: &SqlitePool,
    project_dir: &Path,
    fupan_dir: &Path,
    config: &Config,
) -> Result<TickStats> {
    // 0. 重试上次发送失败/超时的邮件通知；失败保留 pending 下一轮再试。
    let notifier = Notifier::new(pool, config);
    let notified_retry = notifier.flush_pending(50).await.unwrap_or(0);

    // 1. 下载队列由独立后台 worker 处理，不在 70 秒 tick 里同步执行。

    // 2. 对每个视频监控源拉一次最新视频并写入库；新出现的不重复通知。
    // 只检查“视频监控列表.ini”里的源；anchors 里的直播主播不能当成视频源，
    // 否则会把直播间地址误当成 RSS/空间源去拉视频。
    let watch = Watchlist::new(project_dir.to_path_buf());
    let sources = watch.list_video();
    let library = VideoLibrary::new(pool.clone(), fupan_dir.to_path_buf());
    let mut checked = 0usize;
    let mut notified = 0usize;
    let mut enqueued = 0usize;
    let items: Vec<(String, String, Option<bool>)> = sources
        .into_iter()
        .map(|s| (s.url, s.name, s.download))
        .collect();
    let auto_download = config.is_yes_fupan("视频监控", "自动下载新视频(是/否)", true);
    let notify_enabled = config.is_yes_fupan("复盘邮件", "发现新视频时发送通知(是/否)", true);
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
                                        &watch.notify_for(&source_url, &name),
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
        downloads: enqueued,
        anchors_checked: checked,
        videos_notified: notified,
        notify_retried: notified_retry,
    })
}

/// 供 CLI check-videos 复用的一轮检查。
pub async fn check_video_sources(
    pool: &SqlitePool,
    project_dir: &Path,
    config: &Config,
) -> Result<TickStats> {
    let fupan_dir = project_dir.join("fupan_output");
    // CLI 手动检查也要有护栏：单源最多 50 秒、整体 70 秒，避免网络卡死挂住命令。
    match tokio::time::timeout(
        Duration::from_secs(70),
        tick(pool, project_dir, &fupan_dir, config),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(anyhow!("check-videos 超过 70 秒，已中止本轮检查")),
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct TickStats {
    pub downloads: usize,
    pub anchors_checked: usize,
    pub videos_notified: usize,
    pub notify_retried: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn daemon_once_without_anchors() {
        let file = std::env::temp_dir().join(format!("zunzhe-daemon-{}", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let pool = crate::db::open_pool(&file).await.unwrap();
        db::init_db(&pool).await.unwrap();
        let cfg = crate::config::Config::load(Path::new(env!("CARGO_MANIFEST_DIR")))
            .unwrap_or_else(|_| crate::config::Config::defaults_for_test());
        let result = run_daemon(pool, Path::new("."), &cfg, 1, true, None).await;
        assert!(result.is_ok(), "{result:?}");
        let _ = std::fs::remove_file(&file);
    }

    #[tokio::test]
    async fn daemon_duration_exits_and_cleans_pid() {
        let file = std::env::temp_dir().join(format!("zunzhe-daemon-dur-{}", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let pool = crate::db::open_pool(&file).await.unwrap();
        db::init_db(&pool).await.unwrap();
        let cfg = crate::config::Config::load(Path::new(env!("CARGO_MANIFEST_DIR")))
            .unwrap_or_else(|_| crate::config::Config::defaults_for_test());
        let dir = std::env::temp_dir().join(format!("zunzhe-daemon-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::service::write_rust_daemon_pid(&dir, std::process::id());
        let result = run_daemon(pool, &dir, &cfg, 1, false, Some(1)).await;
        assert!(result.is_ok(), "{result:?}");
        assert!(
            !dir.join("fupan_output")
                .join("run")
                .join("zunzhe-core.pid")
                .exists()
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&file);
    }

    #[tokio::test]
    async fn second_daemon_rejected_and_keeps_first_pid() {
        let file = std::env::temp_dir().join(format!("zunzhe-daemon-dup-{}", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let pool = crate::db::open_pool(&file).await.unwrap();
        db::init_db(&pool).await.unwrap();
        let cfg = crate::config::Config::load(Path::new(env!("CARGO_MANIFEST_DIR")))
            .unwrap_or_else(|_| crate::config::Config::defaults_for_test());
        let dir =
            std::env::temp_dir().join(format!("zunzhe-daemon-dup-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 第一个实例持有锁并写入自己的 pid（真实进程才能通过存活检查）。
        let lock = DaemonLock::try_acquire(&dir).unwrap().unwrap();
        crate::service::write_rust_daemon_pid(&dir, std::process::id());

        let second = run_daemon(pool.clone(), &dir, &cfg, 1, true, None).await;
        assert!(second.is_err(), "同一项目根下第二个 daemon 必须被拒绝");
        let pid_text =
            std::fs::read_to_string(dir.join("fupan_output").join("run").join("zunzhe-core.pid"))
                .unwrap();
        assert_eq!(
            pid_text.trim(),
            std::process::id().to_string(),
            "第二实例不能误删第一实例的 pid 文件"
        );

        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&file);
    }

    #[tokio::test]
    async fn tick_timeout_abort_does_not_panic_on_finished_handles() {
        // 复现真实 panic：两个任务都在 70 秒前完成，超时闭包已 await 过句柄；
        // 超时分支不能再 await，否则 tokio 会 "JoinHandle polled after completion"。
        let mut a = tokio::task::spawn(async { 1u32 });
        let mut b = tokio::task::spawn(async { 2u32 });
        let _ = tokio::join!(&mut a, &mut b);
        // 两个句柄已完成，超时收尾不能 panic。
        abort_timed_out_tasks(a, b).await;
    }
}
