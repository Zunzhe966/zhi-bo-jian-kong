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

/// 每轮从平台列表窗口拉取的视频数量。
///
/// 旧实现写死 10，博主同一天连发超过 10 条时，最新视频可能落在第 11+ 位，
/// 导致明明发布了新内容却一直不通知、不入下载队列。窗口放大到 30 条后，
/// 与 CLI 浏览历史视频默认 30 条一致，覆盖绝大多数单轮发布量，同时单源
/// 50 秒护栏仍能兜底网络卡顿。
const SCHEDULER_VIDEO_WINDOW: u32 = 30;

/// 周期维护时清理 B站 Electron 协议残留文件（payload/stderr/status）。
///
/// 调度轮次不跑 Electron，但旧版本/手动诊断可能留下临时文件；daemon 每 6 小时
/// 维护时调用，避免 `fupan_output/run` 目录无限增长。抽成纯函数便于单测接线。
fn maintain_run_dir_cleanup(fupan_dir: &Path) {
    let run_dir = fupan_dir.join("run");
    crate::bilibili_electron::maintain_run_dir(&run_dir);
}

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
    // 清理顺序必须固定为：先删 pid，再释放单实例锁。反向声明保证 drop 顺序，
    // 否则旧进程释放锁后、删除 pid 前会留出窗口，让新实例写入的 pid 被误删。
    let _lock = lock;
    let _pid_guard = DaemonPidGuard(project_dir);
    // 启动即写一条 uptime 日志记录：外部 doctor/桌面端据此计算连续运行时长。
    let uptime_log = project_dir.join("logs").join("zunzhe-core.uptime.log");
    if let Some(parent) = uptime_log.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // uptime 日志只追加不轮转会随 24×30 无限增长；保留最近 3 份滚动历史。
    crate::service::rotate_log(&uptime_log, 3);
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
    // 旧核心残留 DB 合并：daemon 启动时统一收敛到主库，避免两个库并存。
    if let Err(e) = crate::video_library::migrate_legacy_core_db(&pool, project_dir).await {
        warn!("旧核心 DB 迁移失败，继续调度: {e:#}");
    }
    let watch = Watchlist::new(project_dir.to_path_buf());
    let monitor = LiveMonitor::new(&pool, project_dir, config, &watch);
    // 硬崩溃/断电后，SQLite 仍会留着上一实例的 recording 会话。新实例没有旧
    // Child 句柄，必须在首轮 tick 前统一回收，否则僵尸录制状态会长期残留。
    match monitor.recover_stale_recordings().await {
        Ok(0) => {}
        Ok(n) => info!("已回收 {n} 个重启前残留录制会话"),
        Err(e) => warn!("回收重启前残留录制会话失败，继续调度: {e:#}"),
    }
    let mut stop = tokio::sync::watch::channel(false);
    // SQLite 维护计数器：每 6 小时 checkpoint/optimize 一次，防长期 WAL 膨胀。
    // 必须从持久化哨兵恢复，而不是每次启动都重置为当前时间，否则频繁重启
    // 会让维护永远推迟，run 目录残留/数据库 WAL 长期得不到周期收敛。
    let maintenance_interval = 6 * 3600u64;
    let maintenance_marker = fupan_dir.join("run").join("zunzhe-core.maintenance");
    let mut last_maintenance = match crate::db::maintenance_marker_timestamp(&maintenance_marker) {
        Some(ts) if ts > 0 => ts,
        // 哨兵缺失/损坏：立即执行一次维护，让新目录或异常状态尽快收敛。
        _ => 0,
    };
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
    // 也能持续留下 24×30 验收记录。主日志轮转交给外层启动器（start_rust_daemon
    // 启动前 rotate）和 shell 看门狗，不在 daemon 内 rename：tracing 持有打开句柄，
    // 进程内轮转会把新日志写到旧 inode，主日志看起来停更，反而破坏诊断。
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
    // 独立看门监督器：主循环若在 tick/等待阶段异常卡住超过 2 个间隔，
    // 立即记录诊断并触发优雅退出。旧版心跳由主循环维护，卡住时外部看门狗
    // 仍看到“进程活着”，uptime 日志继续写，形成假健康；本监督器保证
    // 24×30 下主循环僵死会自动恢复（外部 watchdog/start_rust_daemon 拉起）。
    let watchdog_interval = interval_secs.max(15);
    let watchdog_limit = interval_secs.saturating_mul(2).max(60);
    let watchdog_project_dir = project_dir.to_path_buf();
    let watchdog_pid = std::process::id();
    let watchdog_stop_tx = stop.0.clone();
    let watchdog_stop_rx = stop.1.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(watchdog_interval));
        loop {
            interval.tick().await;
            if *watchdog_stop_rx.borrow() {
                break;
            }
            // 心跳缺失/损坏按“不新鲜”处理：主循环启动前会先写一次，因此
            // 文件不存在只能说明调度没有真正跑起来，必须让 watchdog 恢复。
            let heartbeat_ok = crate::service::heartbeat_age_secs(&watchdog_project_dir)
                .is_some_and(|age| age <= watchdog_limit);
            if !heartbeat_ok {
                warn!(
                    "心跳超过 {watchdog_limit}s 未更新（pid={watchdog_pid}），调度疑似僵死，触发优雅退出以便 watchdog 拉起"
                );
                let _ = watchdog_stop_tx.send(true);
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
        let now_ts = chrono::Local::now().timestamp();
        if now_ts.saturating_sub(last_maintenance) >= maintenance_interval as i64 {
            match db::maintenance(&pool).await {
                Ok(()) => {
                    if let Ok(removed) = db::prune_orphan_anchors(&pool, project_dir).await
                        && removed > 0
                    {
                        info!("清理 {removed} 条不在直播源里的历史 anchors 记录");
                    }
                    // B站 Electron 协议临时文件：调度轮次不跑 Electron，但旧版本
                    // 或手动诊断可能留下 payload/stderr 残留；一并按“超 7 天删 +
                    // 每类保留 10 个”清理，避免 run 目录无限增长。
                    maintain_run_dir_cleanup(&fupan_dir);
                    // 持久化维护哨兵：CLI/MCP 的 maintenance_if_due 与 daemon 主
                    // 循环共用同一哨兵；只更新内存计时的话，重启后又读到旧哨兵，
                    // 会重复执行维护，也无法证明“维护已完成”。
                    if let Err(e) = crate::db::touch_maintenance_marker(&fupan_dir.join("run")) {
                        warn!("维护哨兵持久化失败（不影响本轮维护）: {e:#}");
                    }
                    info!("SQLite 周期维护完成（WAL checkpoint + optimize + run 残留清理）");
                    last_maintenance = now_ts;
                }
                Err(e) => warn!("SQLite 周期维护失败，下一轮重试: {e:#}"),
            }
        }
        // 配置在 24×30 运行期间可能被桌面端/CLI 热更新（邮箱、转写/提炼/切片、
        // 自动下载、通知开关等）。每轮 tick 重新加载配置，避免用户保存后必须
        // 重启服务才生效；加载失败沿用上一轮配置继续调度，不中断 daemon。
        let current_config = match Config::load(project_dir) {
            Ok(reloaded) => reloaded,
            Err(e) => {
                warn!("配置热重载失败，沿用启动时配置: {e:#}");
                config.clone()
            }
        };
        let tick = run_tick_with_timeout(&pool, project_dir, &fupan_dir, &current_config, &watch);
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
            return shutdown_daemon(
                &pool,
                &monitor,
                &stop.0,
                &mut download_worker,
                &mut pipeline_worker,
            )
            .await;
        }
        // duration 未到点且 tick 提前结束后，等待下一轮；期间每 60 秒刷新心跳，
        // 即使网络任务卡住也保留“调度器仍在运转”的可审计信号。
        let mut wait_exit = false;
        let mut waited = 0u64;
        while waited < interval_secs {
            let step = (interval_secs - waited).min(60);
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(step)) => {}
                _ = stop.1.changed() => {
                    wait_exit = true;
                    break;
                }
                _sig = shutdown_signal() => {
                    wait_exit = true;
                    break;
                }
            }
            waited = waited.saturating_add(step);
            crate::service::touch_heartbeat(project_dir);
        }
        if wait_exit || *stop.1.borrow() {
            return shutdown_daemon(
                &pool,
                &monitor,
                &stop.0,
                &mut download_worker,
                &mut pipeline_worker,
            )
            .await;
        }
    }
}

/// 所有退出路径共用同一套收尾逻辑，确保 SIGTERM/SIGINT、--once、--duration、
/// 外部 stop 命令都不会遗留 ffmpeg 或下载/复盘子任务。
async fn shutdown_daemon(
    pool: &SqlitePool,
    monitor: &LiveMonitor<'_>,
    stop: &tokio::sync::watch::Sender<bool>,
    download_worker: &mut tokio::task::JoinHandle<anyhow::Result<()>>,
    pipeline_worker: &mut tokio::task::JoinHandle<anyhow::Result<()>>,
) -> Result<()> {
    let _ = stop.send(true);
    let _ = monitor.stop_all_recordings().await;
    stop_download_worker(pool, stop, download_worker).await;
    stop_pipeline_worker(pool, stop, pipeline_worker).await;
    Ok(())
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
        // 只删自己的 pid：旧实例退出时若新实例已经写入 pid，不能把新实例删掉。
        crate::service::remove_rust_daemon_pid_if(self.0, std::process::id());
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
    let notifier = Notifier::for_project(pool, config, project_dir);
    let notified_retry = notifier.flush_pending(50).await.unwrap_or(0);

    // 1. 下载队列由独立后台 worker 处理，不在 70 秒 tick 里同步执行。

    // 2. 对每个视频监控源拉一次最新视频并写入库；新出现的不重复通知。
    // 只检查“视频监控列表.ini”里的源；anchors 里的直播主播不能当成视频源，
    // 否则会把直播间地址误当成 RSS/空间源去拉视频。
    // 每轮重新读列表：桌面端/CLI 新增或暂停视频源后下一轮立即生效，
    // 不需要重启 daemon；与直播列表（URL_config.ini）每轮热加载保持一致。
    let watch = Watchlist::new(project_dir.to_path_buf());
    let sources = watch.list_video();
    let library = VideoLibrary::new(pool.clone(), fupan_dir.to_path_buf());
    let mut checked = 0usize;
    let mut notified = 0usize;
    let mut enqueued = 0usize;
    let mut skipped_cooldown = 0usize;
    let items: Vec<(String, String, Option<bool>)> = sources
        .into_iter()
        .map(|s| (s.url, s.name, s.download))
        .collect();
    let auto_download = config.is_yes_fupan("视频监控", "自动下载新视频(是/否)", true);
    let notify_enabled = config.is_yes_fupan("复盘邮件", "发现新视频时发送通知(是/否)", true);
    let item_count = items.len();
    // 所有源共用同一个“已知视频”集合，只在第一个成功源前查询一次；
    // 后面的源复用这份快照，避免 10 个源每轮全量扫描 10 次 videos 表。
    let mut known_ids: Option<std::collections::HashSet<String>> = None;
    for (source_url, name, item_download) in items {
        // B站风控/网络故障不是一次就能恢复；连续失败后给该源冷却时间，
        // 避免每轮都走完整的官方+RSSHub+yt-dlp 兜底链，既拖慢 daemon 又
        // 更容易加重 412。连续失败 3 次起冷却 15 分钟，指数增长，上限 2 小时；
        // 任何一次成功立即清零，不会把刚恢复的源误判成“还在故障”。
        let (source_fails, last_failed_at) = db::source_failure_state(pool, &source_url).await?;
        if source_fails >= 3 {
            let now = chrono::Local::now().timestamp();
            let cooldown = source_cooldown_secs(source_fails);
            if source_cooldown_active(now, last_failed_at, cooldown) {
                info!(
                    "source {source_url} 连续失败 {source_fails} 次，冷却 {} 分钟",
                    cooldown / 60
                );
                skipped_cooldown += 1;
                continue;
            }
        }
        let adapter = platform::resolve_with_project_dir(&source_url, project_dir);
        // 首次基线未建立时仍然检查；B站空间接口 403 等失败要明确返回错误，
        // 不能把“0 条视频”当成成功，避免用户误以为监控已正常工作。
        // 每个视频源最多 50 秒：B站官方/RSSHub/yt-dlp 多重兜底可能叠加较久，
        // 单源卡住不能拖垮整个 daemon 轮次。
        match tokio::time::timeout(
            Duration::from_secs(50),
            adapter.list_videos(&source_url, SCHEDULER_VIDEO_WINDOW),
        )
        .await
        {
            Ok(Err(e)) => {
                error!("source {source_url} check failed: {e:#}");
                let fails = db::bump_source_failures(pool, &source_url)
                    .await
                    .unwrap_or(0);
                info!("source {source_url} 失败冷却计数: {fails}");
                if item_count == 1 {
                    // 单源且是用户手动 check-videos：把失败直接暴露，便于排查。
                    return Err(anyhow::anyhow!("视频监控源检查失败 {source_url}: {e:#}"));
                }
            }
            Ok(Ok(videos)) => {
                // 成功即清零冷却状态；即使本轮返回空，只要平台接口明确成功，
                // 也说明网络/风控已恢复，不应继续被旧失败计数拖累。
                let _ = db::reset_source_failures(pool, &source_url).await;
                if known_ids.is_none() {
                    let all = db::get_videos(pool, "", "", false).await?;
                    known_ids = Some(all.iter().map(|v| v.id.clone()).collect());
                }
                let first_run = !db::is_source_baselined(pool, &source_url).await?;
                // 同源一轮返回的视频里，平台兜底链偶尔会重复同一个 BV/链接。
                // 每轮每个源只处理一次，避免同轮重复写库、重复通知和重复入队。
                let mut seen_this_round = std::collections::HashSet::new();
                for v in videos
                    .iter()
                    .filter(|v| seen_this_round.insert(v.id.clone()))
                {
                    // view 接口失败时只返回占位标题，跳过不写库，避免污染视频库。
                    if v.title == v.id {
                        continue;
                    }
                    let known = known_ids.as_mut().expect("本轮已初始化视频库快照");
                    let is_new = !known.contains(&v.id);
                    match library.upsert(v, &name, &source_url).await {
                        // 首次监控：只把现有视频写入库作为基线，不通知、不下载。
                        Ok(_) if is_new && first_run => {
                            known.insert(v.id.clone());
                        }
                        Ok(_) if is_new => {
                            known.insert(v.id.clone());
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
                let fails = db::bump_source_failures(pool, &source_url)
                    .await
                    .unwrap_or(0);
                info!("source {source_url} 失败冷却计数: {fails}");
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
        skipped_cooldown,
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
    pub skipped_cooldown: usize,
}

/// 连续失败次数对应的冷却秒数：3 次起 15 分钟，之后按失败次数线性增加，上限 2 小时。
fn source_cooldown_secs(fails: u32) -> u64 {
    if fails <= 2 {
        return 0;
    }
    let base = Duration::from_secs(15 * 60);
    let level = fails.saturating_sub(2);
    (base * level)
        .min(Duration::from_secs(2 * 60 * 60))
        .as_secs()
}

/// 冷却是否仍生效：`now >= last_failed_at + cooldown` 时允许重试。
fn source_cooldown_active(now_secs: i64, last_failed_at: i64, cooldown_secs: u64) -> bool {
    cooldown_secs > 0 && now_secs.saturating_sub(last_failed_at) < cooldown_secs as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maintenance_cleans_stale_electron_files_in_run_dir() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-sched-maint-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let run_dir = dir.join("fupan_output").join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        // 一个 8 天前的旧 payload 残留，一个刚写入的其它状态文件。
        let old_payload = run_dir.join("bili_script_payload_stale.js");
        std::fs::write(&old_payload, "old").unwrap();
        set_mtime_days_ago(&old_payload, 8);
        let fresh = run_dir.join("bili_script_status_fresh.json");
        std::fs::write(&fresh, "{}").unwrap();
        let stale_tmp = run_dir.join(".zunzhe-core.heartbeat.tmp.123.456.0");
        std::fs::write(&stale_tmp, "stale").unwrap();
        set_mtime_days_ago(&stale_tmp, 8);

        maintain_run_dir_cleanup(&dir.join("fupan_output"));

        assert!(!old_payload.exists(), "超龄 payload 残留应被维护清理");
        assert!(!stale_tmp.exists(), "超龄原子写残留应被维护清理");
        assert!(fresh.exists(), "新状态文件不应被误删");
        let _ = std::fs::remove_dir_all(&dir);
    }

    use crate::test_support::set_mtime_days_ago;

    #[tokio::test]
    async fn daemon_once_without_anchors() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let file =
            std::env::temp_dir().join(format!("zunzhe-daemon-{}-{nonce}", std::process::id()));
        let dir =
            std::env::temp_dir().join(format!("zunzhe-daemon-dir-{}-{nonce}", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let run_dir = dir.join("fupan_output").join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        // 首轮 tick 应执行一次到期维护并刷新哨兵，同时清理旧版本 Electron
        // 协议残留；防止“启动后维护永远推迟”这类回归再次出现。
        let old_payload = run_dir.join("bili_script_payload_stale.js");
        std::fs::write(&old_payload, "old").unwrap();
        set_mtime_days_ago(&old_payload, 8);
        let stale_tmp = run_dir.join(".zunzhe-core.heartbeat.tmp.123.456.0");
        std::fs::write(&stale_tmp, "stale").unwrap();
        set_mtime_days_ago(&stale_tmp, 8);
        let pool = crate::db::open_pool(&file).await.unwrap();
        db::init_db(&pool).await.unwrap();
        let cfg = crate::config::Config::load(Path::new(env!("CARGO_MANIFEST_DIR")))
            .unwrap_or_else(|_| crate::config::Config::defaults_for_test());
        let result = run_daemon(pool, &dir, &cfg, 1, true, None).await;
        assert!(result.is_ok(), "{result:?}");
        let marker = run_dir.join("zunzhe-core.maintenance");
        assert!(marker.exists(), "首轮到期维护后应刷新维护哨兵");
        let marker_ts = crate::db::maintenance_marker_timestamp(&marker).unwrap();
        let now = chrono::Local::now().timestamp();
        assert!(
            (now - marker_ts).abs() <= 30,
            "维护哨兵应记录本轮执行时间: now={now} ts={marker_ts}"
        );
        assert!(!old_payload.exists(), "首轮维护应清理超龄 payload 残留");
        assert!(!stale_tmp.exists(), "首轮维护应清理超龄原子写残留");
        // --once 模式退出后必须清理 pid/锁，不能污染临时目录或真实工作区。
        assert!(
            !dir.join("fupan_output")
                .join("run")
                .join("zunzhe-core.pid")
                .exists()
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn scheduler_video_window_covers_burst_publishes() {
        // 平台列表按时间倒序返回：一次连发超过 10 条时，窗口必须覆盖第 11+
        // 条，否则新视频会被误判成“已基线”而永久漏通知。
        const _: () = assert!(SCHEDULER_VIDEO_WINDOW >= 11, "窗口过小会漏掉爆发式发布");
        assert_eq!(SCHEDULER_VIDEO_WINDOW, 30);
    }

    #[tokio::test]
    async fn daemon_duration_exits_and_cleans_pid() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let file =
            std::env::temp_dir().join(format!("zunzhe-daemon-dur-{}-{nonce}", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let pool = crate::db::open_pool(&file).await.unwrap();
        db::init_db(&pool).await.unwrap();
        let cfg = crate::config::Config::load(Path::new(env!("CARGO_MANIFEST_DIR")))
            .unwrap_or_else(|_| crate::config::Config::defaults_for_test());
        let dir =
            std::env::temp_dir().join(format!("zunzhe-daemon-dir-{}-{nonce}", std::process::id()));
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
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let file =
            std::env::temp_dir().join(format!("zunzhe-daemon-dup-{}-{nonce}", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let pool = crate::db::open_pool(&file).await.unwrap();
        db::init_db(&pool).await.unwrap();
        let cfg = crate::config::Config::load(Path::new(env!("CARGO_MANIFEST_DIR")))
            .unwrap_or_else(|_| crate::config::Config::defaults_for_test());
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-daemon-dup-dir-{}-{nonce}",
            std::process::id()
        ));
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

    #[test]
    fn heartbeat_watchdog_threshold_covers_wait_and_tick() {
        // 等待阶段每 60 秒刷心跳，因此看门监督器阈值必须 >= 2 个间隔；
        // 否则正常 tick + 等待组合（最坏 ~70s + 900s）会被误判为僵死。
        let interval = 900u64;
        let limit = interval.saturating_mul(2).max(60);
        assert!(limit >= 2 * interval);
        assert!(limit >= 70 + 60, "阈值必须覆盖 tick 超时与单次等待步长");

        // 15 分钟间隔下，60 秒等待步长不会超过阈值。
        let mut waited = 0u64;
        while waited < interval {
            let step = (interval - waited).min(60);
            assert!(waited.saturating_add(step) <= limit);
            waited = waited.saturating_add(step);
        }
    }

    #[test]
    fn source_cooldown_rules_are_exponential_and_recoverable() {
        assert_eq!(source_cooldown_secs(0), 0);
        assert_eq!(source_cooldown_secs(2), 0, "连续失败 2 次不进入冷却");
        assert_eq!(source_cooldown_secs(3), 15 * 60);
        assert_eq!(source_cooldown_secs(4), 30 * 60);
        assert_eq!(source_cooldown_secs(6), 60 * 60);
        assert_eq!(source_cooldown_secs(100), 2 * 60 * 60, "冷却上限 2 小时");

        let now = 1_000_000;
        assert!(
            !source_cooldown_active(now, now - 15 * 60, 15 * 60),
            "冷却期满应允许重试"
        );
        assert!(
            source_cooldown_active(now, now - 60, 15 * 60),
            "冷却期内应跳过"
        );
        assert!(
            !source_cooldown_active(now, now - 60, 0),
            "无冷却时永远允许"
        );
    }

    #[tokio::test]
    async fn maintenance_prunes_notify_and_tasks_to_recent_200() {
        let file =
            std::env::temp_dir().join(format!("zunzhe-maintenance-prune-{}", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let pool = crate::db::open_pool(&file).await.unwrap();
        for i in 0..250 {
            let nid = db::add_notify(&pool, "video", "博主", "标题", "内容", "邮箱")
                .await
                .unwrap();
            // 终态记录才会被清理；pending 通知无论新旧都必须保留。
            db::fail_notify(&pool, nid).await.unwrap();
            let id = db::enqueue_task(&pool, "download", &format!("https://x.test/{i}"), "博主")
                .await
                .unwrap();
            // 终态记录才会被清理；pending 任务无论新旧都必须保留。
            db::set_task_status(&pool, id, "done").await.unwrap();
        }
        crate::db::maintenance(&pool).await.unwrap();
        assert_eq!(db::list_notify(&pool, "", 1000).await.unwrap().len(), 200);
        assert_eq!(db::list_tasks(&pool, "", 1000).await.unwrap().len(), 200);
        let _ = std::fs::remove_file(&file);
    }
}
