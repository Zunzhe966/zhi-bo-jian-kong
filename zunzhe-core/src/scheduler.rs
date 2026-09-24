use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use futures_util::FutureExt;
use sqlx::sqlite::SqlitePool;
use tracing::{error, info, warn};

use crate::config::Config;

/// 持有 B站 WS 监听任务句柄：run_daemon 退出（无论正常/异常路径）时自动
/// abort，避免监听任务在调度器停止后残留。
struct WsMonitorGuard(tokio::task::JoinHandle<()>);
impl Drop for WsMonitorGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}
use crate::daemon_lock::DaemonLock;
use crate::db;
use crate::download;
use crate::live_monitor::{LiveCheckStats, LiveMonitor};
use crate::notify::Notifier;
use crate::platform;
use crate::video_library::VideoLibrary;
use crate::watchlist::{self, Watchlist};

/// 每轮从平台列表窗口拉取的视频数量。
///
/// 旧实现写死 10，博主同一天连发超过 10 条时，最新视频可能落在第 11+ 位，
/// 导致明明发布了新内容却一直不通知、不入下载队列。窗口放大到 30 条后，
/// 与 CLI 浏览历史视频默认 30 条一致，覆盖绝大多数单轮发布量，同时单源
/// 50 秒护栏仍能兜底网络卡顿。
const SCHEDULER_VIDEO_WINDOW: u32 = 30;

/// 短视频「按时间窗口下载」：只下载发布时间距今 N 天以内的新视频。
///
/// 用户需求「以 3 天为周期下载 3 天以内的视频」——平台列表接口会返回博主
/// 全部历史作品，如果见一个下一个，会把老视频也全量补下。这里在通知/下载前
/// 按 `published_at` 过滤：超过 N 天的旧视频只写入视频库作为基线，不通知不下载。
const SCHEDULER_VIDEO_MAX_AGE_DAYS: i64 = 3;

/// 解析平台返回的 `published_at`（`YYYY-MM-DD HH:MM:SS` 或 `YYYY-MM-DD`），
/// 判断是否在 `max_age_days` 天内。解析失败视为「旧视频」，只写库不下载，
/// 避免把时间缺失的占位记录误当新片下载。
fn published_within_days(published_at: &str, max_age_days: i64) -> bool {
    let published_at = published_at.trim();
    if published_at.is_empty() {
        return false;
    }
    let parsed =
        chrono::NaiveDateTime::parse_from_str(published_at, "%Y-%m-%d %H:%M:%S").or_else(|_| {
            chrono::NaiveDate::parse_from_str(published_at, "%Y-%m-%d")
                .map(|d| d.and_hms_opt(0, 0, 0).unwrap_or_default())
        });
    let Ok(published) = parsed else {
        return false;
    };
    let now = chrono::Local::now().naive_local();
    let age = now.signed_duration_since(published);
    age.num_days() >= 0 && age.num_days() <= max_age_days
}

/// 视频源并发检查上限。
///
/// 直播检查已经并发，但视频源此前是串行遍历：一个源被 B站 412/RSSHub 403
/// 或网络卡住 50 秒，后面所有源都会被拖住，多源时整轮很容易撞上 70 秒
/// tick 护栏。这里给视频源同样加上有界并发（默认 4 路），平台抖动只影响
/// 自己，不阻塞其它博主；上限取小值避免几十个源同时打平台把风控打得更严。
const SCHEDULER_VIDEO_CONCURRENCY: usize = 4;

/// 一个视频源检查结果：网络/平台/时区等问题在单源内消化，不中断整轮。
struct SourceCheckOutcome {
    source_url: String,
    name: String,
    item_download: Option<bool>,
    videos: Result<Vec<crate::model::VideoMeta>>,
    timed_out: bool,
}

/// 并发检查队列里的一个待检视频源：`(URL, 博主名, 下载偏好)`。
type PendingVideoSource = (String, String, Option<bool>);

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
    let uptime_log = crate::paths::logs(project_dir).join("zunzhe-core.uptime.log");
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
    let fupan_dir = crate::paths::fupan(project_dir);
    // daemon 启动时合并旧 Python 独立视频库，确保 `已下载` 标记与历史视频
    // 元数据不会因切换主 DB 而丢失；迁移幂等，只补缺失 id，不覆盖主库字段。
    if let Err(e) = crate::video_library::migrate_legacy_video_db(&pool, &fupan_dir).await {
        warn!("旧视频库迁移失败，继续调度: {e:#}");
    }
    // 旧核心残留 DB 合并：daemon 启动时统一收敛到主库，避免两个库并存。
    if let Err(e) = crate::video_library::migrate_legacy_core_db(&pool, project_dir).await {
        warn!("旧核心 DB 迁移失败，继续调度: {e:#}");
    }
    // 项目从旧桌面副本迁到当前根后，任务表可能仍记录旧绝对路径；启动时
    // 立即改写一次，避免“已复盘/已下载”状态继续指向过期位置。幂等且失败
    // 不阻断 daemon。
    if let Ok(rewritten) = db::rewrite_legacy_task_paths(&pool, project_dir).await
        && rewritten > 0
    {
        info!("启动时改写 {rewritten} 条指向旧桌面根的任务路径");
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
    // B站弹幕 WS 实时开播监听（录播姬同款）：只负责“开播事件 → 提前唤醒
    // 下一轮 tick”，检测/录制/通知/防重仍由 LiveMonitor 状态机完成；
    // 掉线或漏报由 300 秒轮询兜底。任务随 run_daemon 退出自动清理。
    let (ws_tx, mut ws_rx) = tokio::sync::mpsc::channel::<String>(16);
    let _ws_guard = WsMonitorGuard(crate::bili_live_ws::spawn_bili_ws_monitor(project_dir.to_path_buf(), ws_tx));
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
    // 视频源检查节流：`config/fupan_config.ini [视频监控] 检查间隔秒` 才是视频
    // 轮次的真实节拍。直播检查不受它约束——开播要尽快发现，仍按 CLI
    // `--interval`（默认 60 秒）跑；视频检查是打外部接口的重活，必须尊重配置。
    // 0 表示尚未检查过，首轮立即查一次，避免刚启动就干等 15 分钟。
    let mut last_video_check: i64 = 0;
    // 独立下载队列 worker：不参与 70 秒 tick 护栏，每个任务自身有 600 秒超时。
    // 下载耗时不会阻塞直播/视频检查，也不会被 tick abort 后永远卡 processing。
    let download_pool = pool.clone();
    let download_project = project_dir.to_path_buf();
    let mut download_worker = spawn_supervised_worker(
        "下载",
        stop.0.clone(),
        stop.1.clone(),
        Duration::from_secs(5),
        move |rx| {
            download::run_download_worker(
                download_pool.clone(),
                download_project.clone(),
                rx,
                std::time::Duration::from_secs(600),
                std::time::Duration::from_secs(5),
            )
        },
    );
    // 独立复盘队列 worker：下载/录制完成后自动转写、提炼、切片。
    let pipeline_pool = pool.clone();
    let pipeline_project = project_dir.to_path_buf();
    let mut pipeline_worker = spawn_supervised_worker(
        "复盘",
        stop.0.clone(),
        stop.1.clone(),
        Duration::from_secs(5),
        move |rx| {
            crate::pipeline_bridge::run_pipeline_worker(
                pipeline_pool.clone(),
                pipeline_project.clone(),
                rx,
                std::time::Duration::from_secs(6 * 3600),
                std::time::Duration::from_secs(5),
            )
        },
    );
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
        // tokio interval 第一次 tick() 会立即返回。若不丢弃这一次，daemon 刚
        // 启动、心跳还没来得及刷新（尤其是上次崩溃残留的旧心跳文件）时，
        // 这里会误判“调度僵死”并触发优雅退出；而退出后外部 watchdog 又会
        // 重新拉起，形成“拉起→误判自杀→再拉起”的死循环（已实测复现）。
        // 丢弃首次立即返回，让第一次检查真正等到 watchdog_interval 之后。
        interval.tick().await;
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
                    if let Ok(rewritten) = db::rewrite_legacy_task_paths(&pool, project_dir).await
                        && rewritten > 0
                    {
                        info!("改写 {rewritten} 条指向旧桌面根的任务路径");
                    }
                    // B站 Electron 协议临时文件：调度轮次不跑 Electron，但旧版本
                    // 或手动诊断可能留下 payload/stderr 残留；一并按“超 7 天删 +
                    // 每类保留 10 个”清理，避免 run 目录无限增长。
                    maintain_run_dir_cleanup(&fupan_dir);
                    // 封面缓存孤儿文件：历史浏览/接口失败可能留下 DB 不再引用
                    // 的图片，维护时一并清理，避免 24×30 长跑磁盘无限膨胀。
                    if let Ok(removed) =
                        crate::video_library::maintain_orphan_covers(&pool, &fupan_dir).await
                        && removed > 0
                    {
                        info!("清理 {removed} 个孤儿封面缓存文件");
                    }
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
        // 视频轮次是否到点：`检查间隔秒` 是唯一权威（下限 60 秒、上限 24 小时）。
        // 未到点就只跑直播检测，把外部接口压力降下来。
        let video_interval = current_config.video_check_interval_secs();
        let run_videos = last_video_check == 0
            || now_ts.saturating_sub(last_video_check) >= video_interval as i64;
        if !run_videos {
            info!(
                "视频源检查未到间隔（{}s，上次 {}），本轮只跑直播检测",
                video_interval, last_video_check
            );
        }
        let tick =
            run_tick_with_timeout(&pool, project_dir, &fupan_dir, &current_config, run_videos);
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
        // 只有真的跑了视频检查才推进计时。放在 tick 结束后而不是发起前：
        // 本轮失败/超时也算「查过了」，否则会退化成每 60 秒重试一次，
        // 反而比配置的间隔更密（单源冷却机制挡不住整轮重试）。
        if run_videos {
            last_video_check = now_ts;
        }
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
                live = ws_rx.recv() => {
                    if let Some(live) = live {
                        info!("[B站WS] {live} 开播事件触发提前检测");
                        break;
                    }
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

/// 启动一个带自愈监督的队列 worker。
///
/// worker 自身返回 `Err` 或 panic 时，单个 worker 不应拖垮整个 daemon：
/// 监督任务记录诊断并重新拉起 worker（最多 3 次连续失败，避免坏配置无限
/// 快速重启刷日志）。停止信号优先于重启；外层停机时会 abort 内层任务并
/// 归还 processing 队列，避免任务永久卡死。
fn spawn_supervised_worker<F, Fut>(
    name: &'static str,
    stop_tx: tokio::sync::watch::Sender<bool>,
    mut stop_rx: tokio::sync::watch::Receiver<bool>,
    restart_delay: Duration,
    factory: F,
) -> tokio::task::JoinHandle<anyhow::Result<()>>
where
    F: Fn(tokio::sync::watch::Receiver<bool>) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    tokio::spawn(async move {
        let mut consecutive_failures = 0u64;
        let mut task = tokio::spawn(factory(stop_rx.clone()));
        loop {
            let result = tokio::select! {
                _ = stop_rx.changed() => {
                    if *stop_rx.borrow() {
                        task.abort();
                        let _ = task.await;
                        return Ok(());
                    }
                    continue;
                }
                r = &mut task => r,
            };
            if *stop_rx.borrow() {
                return Ok(());
            }
            consecutive_failures += 1;
            let (normal, reason) = match &result {
                Ok(Ok(())) => (true, "正常退出".to_string()),
                Ok(Err(e)) => (false, format!("失败: {e:#}")),
                Err(join) if join.is_panic() => (false, "panic".to_string()),
                Err(_) => (false, "已取消".to_string()),
            };
            if normal {
                info!("{} worker 正常退出，重启后重新接管队列", name);
                consecutive_failures = 0;
            }
            warn!(
                "{} worker 退出（{reason}，连续失败 {consecutive_failures} 次），5 秒后自动重启",
                name
            );
            if consecutive_failures >= 3 {
                error!(
                    "{} worker 连续失败 3 次，停止自动重启并触发 daemon 停机",
                    name
                );
                let _ = stop_tx.send(true);
                return Err(anyhow!("{} worker 连续失败 3 次", name));
            }
            let mut waiting = false;
            tokio::select! {
                _ = stop_rx.changed() => {
                    waiting = *stop_rx.borrow();
                }
                _ = tokio::time::sleep(restart_delay) => {}
            }
            if waiting {
                return Ok(());
            }
            task = tokio::spawn(factory(stop_rx.clone()));
        }
    })
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
    // 监督器收到 stop 后会先 abort 内层 worker，再返回；直接等它收尾，
    // 不能只 abort 监督任务本身，否则内层下载任务可能仍在后台跑。
    let _ = worker.await;
    let _ = db::recover_processing_by_kind(pool, "download").await;
}

/// 退出 daemon 时停止复盘 worker：abort 当前任务并归还 process 队列。
async fn stop_pipeline_worker(
    pool: &SqlitePool,
    stop: &tokio::sync::watch::Sender<bool>,
    worker: &mut tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let _ = stop.send(true);
    let _ = worker.await;
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
    run_videos: bool,
) {
    // 用 owned 快照 + tokio::task::spawn 隔离：单个 tick panic 不会拖垮主循环。
    let pool = pool.clone();
    let project_dir = project_dir.to_path_buf();
    let fupan_dir = fupan_dir.to_path_buf();
    let config = config.clone();
    // 直播检测必须用项目根构造 Watchlist，才能读到 config/URL_config.ini。
    // 旧实现误用 url_config().parent()（= config/ 目录），导致 Watchlist
    // 去读不存在的 config/config/URL_config.ini，list_live() 永远返回空、
    // live tick 每轮 checked=0，直播监控彻底失效。用 project_dir 后
    // list_live() 正确读取 config/URL_config.ini。
    let watch = Watchlist::new(project_dir.clone());
    let mut live_task = tokio::task::spawn(run_live_tick(
        pool.clone(),
        project_dir.clone(),
        config.clone(),
        watch,
    ));
    let mut videos_task = tokio::task::spawn(async move {
        // 未到「检查间隔秒」就不打外部接口：直接返回空统计，交由上层决定
        // 是否记日志。直播任务照跑，不受影响。
        if !run_videos {
            return Ok(TickStats::default());
        }
        tick_owned(pool, project_dir, fupan_dir, config).await
    });

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
        // 跳过的轮次不记统计日志，否则每 60 秒刷一条空 tick 噪声。
        if run_videos {
            match videos {
                Ok(stats) => info!("daemon tick ok: {:?}", stats),
                Err(e) => error!("daemon tick failed: {e:#}"),
            }
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

/// 取「本轮真正要检查」的视频源。
///
/// `Watchlist::list_video()` 刻意返回全部条目（含以 `#` 开头的暂停行），
/// 因为状态快照要靠它把暂停源也显示给界面；因此启用与否必须由调用方决定。
/// 视频 tick 曾经忘了这层过滤，后果是：配置文件里注释掉的源照样每轮被拉取、
/// 照样通知和下载，「以 # 开头 = 暂停监控」对视频源形同虚设（注释掉一个
/// 没登录的抖音源，只会换来每轮一条「尚未登录抖音」的 ERROR 和持续冷却计数）。
fn enabled_video_sources(watch: &Watchlist) -> Vec<watchlist::VideoSource> {
    watch
        .list_video()
        .into_iter()
        .filter(|s| s.enabled)
        .collect()
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
    let sources = enabled_video_sources(&watch);
    let library = VideoLibrary::new(pool.clone(), fupan_dir.to_path_buf());
    let mut checked = 0usize;
    let mut notified = 0usize;
    let mut enqueued = 0usize;
    let mut skipped_cooldown = 0usize;
    let mut skipped_missing_credential = 0usize;
    let items: Vec<(String, String, Option<bool>)> = sources
        .into_iter()
        .map(|s| (s.url, s.name, s.download))
        .collect();
    let auto_download = config.is_yes_fupan("视频监控", "自动下载新视频(是/否)", true);
    let notify_enabled = config.is_yes_fupan("复盘邮件", "发现新视频时发送通知(是/否)", true);
    let (outcomes, skipped_in_concurrent, skipped_proxy_off) = check_video_sources_concurrent(
        pool,
        project_dir,
        items,
        SCHEDULER_VIDEO_CONCURRENCY,
        platform::resolve_with_project_dir,
    )
    .await?;
    skipped_cooldown += skipped_in_concurrent;
    // 所有源共用同一个“已知视频”集合，只在第一个成功源前查询一次；
    // 后面的源复用这份快照，避免 10 个源每轮全量扫描 10 次 videos 表。
    let mut known_ids: Option<std::collections::HashSet<String>> = None;
    let only_source = outcomes.len() == 1;
    for outcome in outcomes {
        apply_source_outcome(
            pool,
            &watch,
            &library,
            &notifier,
            outcome,
            only_source,
            auto_download,
            notify_enabled,
            &mut known_ids,
            &mut checked,
            &mut notified,
            &mut enqueued,
            &mut skipped_missing_credential,
        )
        .await?;
    }

    Ok(TickStats {
        downloads: enqueued,
        anchors_checked: checked,
        videos_notified: notified,
        notify_retried: notified_retry,
        skipped_cooldown,
        skipped_proxy_off,
        skipped_missing_credential,
    })
}

#[allow(clippy::too_many_arguments)]
async fn apply_source_outcome(
    pool: &SqlitePool,
    watch: &Watchlist,
    library: &VideoLibrary,
    notifier: &Notifier<'_>,
    outcome: SourceCheckOutcome,
    only_source: bool,
    auto_download: bool,
    notify_enabled: bool,
    known_ids: &mut Option<std::collections::HashSet<String>>,
    checked: &mut usize,
    notified: &mut usize,
    enqueued: &mut usize,
    skipped_missing_credential: &mut usize,
) -> Result<()> {
    let source_url = outcome.source_url;
    let name = outcome.name;
    let item_download = outcome.item_download;
    match outcome.videos {
        Ok(videos) => {
            // 只有拿到明确内容才算“恢复”。平台在风控/接口异常时可能返回
            // 空列表；若已有连续失败计数，空成功不能把计数清零，否则
            // B站间歇性 412 会让失败永远停在 1。真实恢复（有内容成功）
            // 或无历史失败时仍立即清零。
            let (source_fails, _) = db::source_failure_state(pool, &source_url).await?;
            if should_reset_source_failures(source_fails, videos.is_empty()) {
                let _ = db::reset_source_failures(pool, &source_url).await;
            }
            if known_ids.is_none() {
                let all = db::get_videos(pool, "", "", false).await?;
                *known_ids = Some(all.iter().map(|v| v.id.clone()).collect());
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
                        // 时间窗口过滤：只下载/通知 3 天以内的新视频；更早的
                        // 视频只写库作为基线，避免把博主历史作品全量补下。
                        if !published_within_days(&v.published_at, SCHEDULER_VIDEO_MAX_AGE_DAYS) {
                            continue;
                        }
                        if notify_enabled {
                            let title = new_video_notify_title(&name, &v.title);
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
                                Ok(_) => *notified += 1,
                                Err(e) => warn!("通知新视频失败: {e:#}"),
                            }
                        }
                        let should_download = item_download.unwrap_or(auto_download);
                        if should_download {
                            match db::enqueue_task(pool, "download", &v.url, &name).await {
                                Ok(_) => *enqueued += 1,
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
            *checked += 1;
        }
        Err(e) => {
            let msg = format!("{e:#}");
            // 缺凭据（未登录/缺 cookie）是确定性错误：重试不会自愈，
            // 累计失败冷却没有意义，只会每轮刷 ERROR 并占用并发槽位。
            // 这里直接跳过、不计失败，提示用户去授权；授权后下一轮自动恢复。
            if is_missing_credential_error(&msg) {
                warn!(
                    "source {source_url} 未完成授权，本轮跳过（不计失败冷却，授权后自动恢复）: {msg}"
                );
                *skipped_missing_credential += 1;
                return Ok(());
            }
            if outcome.timed_out {
                error!("source {source_url} check timeout (50s)");
            } else {
                error!("source {source_url} check failed: {msg}");
            }
            let fails = db::bump_source_failures(pool, &source_url)
                .await
                .unwrap_or(0);
            info!("source {source_url} 失败冷却计数: {fails}");
            if only_source {
                // 单源且是用户手动 check-videos：把失败直接暴露，便于排查。
                if outcome.timed_out {
                    return Err(anyhow!("视频监控源检查超时（50 秒）: {source_url}"));
                }
                return Err(anyhow!("视频监控源检查失败 {source_url}: {msg}"));
            }
        }
    }
    Ok(())
}

/// 有界并发检查视频源，结果按列表顺序返回。
///
/// 每个源使用自己的适配器与 50 秒总超时；并发上限防止几十个源同时打平台。
/// 冷却逻辑仍然保留：失败 >=3 次的源在并发检查前跳过，避免冷却源继续占用
/// 并发槽位。任何单源错误/超时只进入 Outcome，由调用方统一记失败次数。
async fn check_video_sources_concurrent<F>(
    pool: &SqlitePool,
    project_dir: &Path,
    items: Vec<(String, String, Option<bool>)>,
    concurrency: usize,
    resolve: F,
) -> Result<(Vec<SourceCheckOutcome>, usize, usize)>
where
    F: Fn(&str, &Path) -> Box<dyn platform::Platform> + Clone + Send + Sync + 'static,
{
    let concurrency = concurrency.max(1);
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency));
    let mut pending: Vec<(usize, PendingVideoSource)> = Vec::with_capacity(items.len());
    let mut skipped_cooldown = 0usize;
    let mut skipped_proxy_off = 0usize;
    // 代理健康预检：代理软件没开时，本机代理端口 Connection refused，依赖
    // 代理的国外源（rsshub.app 等）每轮都会“失败”——这不是源故障，不应
    // 累计失败冷却，也不该刷错误日志。这里探测一次代理端口，不通则明确
    // 跳过、不计失败；代理恢复后下一轮自动继续检查这些源。
    let mut proxy_state: Option<bool> = None;
    for (idx, item) in items.into_iter().enumerate() {
        let (source_url, name, item_download) = item;
        if crate::net::policy().needs_proxy(&source_url) {
            // 每轮最多探测一次：所有需要代理的源共用同一个结果。
            if proxy_state.is_none() {
                proxy_state = Some(crate::net::policy().proxy_alive().await.unwrap_or(false));
            }
            if proxy_state == Some(false) {
                info!(
                    "source {source_url} 出站代理未运行，本轮跳过（不计失败冷却，开代理后自动恢复）"
                );
                skipped_proxy_off += 1;
                continue;
            }
        }
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
        pending.push((idx, (source_url, name, item_download)));
    }

    // JoinSet 负责托管所有源任务：外层 tick 被 70 秒护栏取消时，JoinSet
    // Drop 会 abort 尚未完成的子任务，避免网络请求脱离本轮继续占用资源。
    let mut tasks = tokio::task::JoinSet::new();
    for (idx_ref, (source_url, name, item_download)) in pending.iter() {
        let idx = *idx_ref;
        let sem = semaphore.clone();
        let project_dir = project_dir.to_path_buf();
        let resolve = resolve.clone();
        let source_url = source_url.clone();
        let name = name.clone();
        let item_download = *item_download;
        tasks.spawn(async move {
            let fallback = || SourceCheckOutcome {
                source_url: source_url.clone(),
                name: name.clone(),
                item_download,
                videos: Err(anyhow!("视频监控源检查任务 panic: {source_url}")),
                timed_out: false,
            };
            // 单个适配器 panic 也只影响它自己；JoinSet 仍可继续收集其它源。
            let result = std::panic::AssertUnwindSafe(async {
                let _permit = sem.acquire_owned().await.expect("信号量不会关闭");
                let adapter = resolve(&source_url, &project_dir);
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
                    Ok(videos) => SourceCheckOutcome {
                        source_url: source_url.clone(),
                        name: name.clone(),
                        item_download,
                        videos,
                        timed_out: false,
                    },
                    Err(_) => {
                        let err = anyhow!("视频监控源检查超时（50 秒）: {source_url}");
                        SourceCheckOutcome {
                            source_url: source_url.clone(),
                            name: name.clone(),
                            item_download,
                            videos: Err(err),
                            timed_out: true,
                        }
                    }
                }
            })
            .catch_unwind()
            .await;
            (idx, result.unwrap_or_else(|_| fallback()))
        });
    }

    let mut outcomes = Vec::with_capacity(pending.len());
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(item) => outcomes.push(item),
            Err(e) => warn!("视频监控源任务被取消: {e}"),
        }
    }
    outcomes.sort_by_key(|(idx, _)| *idx);
    Ok((
        outcomes.into_iter().map(|(_, item)| item).collect(),
        skipped_cooldown,
        skipped_proxy_off,
    ))
}

/// 供 CLI check-videos 复用的一轮检查。
pub async fn check_video_sources(
    pool: &SqlitePool,
    project_dir: &Path,
    config: &Config,
) -> Result<TickStats> {
    let fupan_dir = crate::paths::fupan(project_dir);
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

/// 新视频通知标题：与旧 Python `send_new_video_alert` 对齐，带上博主名和
/// 视频标题，避免多条不同视频在通知列表里看起来都像同一条。
fn new_video_notify_title(anchor: &str, video_title: &str) -> String {
    format!("【新视频】{anchor} 发布了新内容：{video_title}")
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct TickStats {
    pub downloads: usize,
    pub anchors_checked: usize,
    pub videos_notified: usize,
    pub notify_retried: usize,
    pub skipped_cooldown: usize,
    /// 因出站代理未运行而跳过的源数（不计失败冷却，开代理自动恢复）。
    pub skipped_proxy_off: usize,
    /// 因未完成授权（缺 cookie 凭据）而跳过的源数（确定性错误，不计失败冷却，
    /// 授权后下一轮自动恢复）。
    pub skipped_missing_credential: usize,
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

fn should_reset_source_failures(previous_fails: u32, empty_success: bool) -> bool {
    previous_fails == 0 || !empty_success
}

/// 判断源检查失败是否属于「确定性缺失凭据/配置」——即不论重试多少次都不会
/// 自行恢复的用户侧问题（未登录、缺 cookie 文件等）。
///
/// 这类错误和 B站 412 风控/网络抖动有本质区别：
/// - 风控/网络是**暂时性**的，值得累计失败并指数冷却，等条件恢复；
/// - 缺凭据是**确定性**的，重试一万次也一样失败，累计冷却毫无意义，
///   只会每轮刷一条 ERROR 并让该源长期占据并发槽位。
///
/// 因此调用方应对这类错误直接跳过、**不计失败冷却**，只提示一次让用户去授权；
/// 用户完成授权后，下一轮 `cookie_path.exists()` 即为真，自动恢复正常检查，
/// 不需要任何手动重置。这与「代理未运行则跳过源」是同一种设计。
fn is_missing_credential_error(message: &str) -> bool {
    // 与平台适配器的错误文案约定（见 platform.rs 各适配器 missing-cookie 分支）。
    // 用「尚未登录」/「未登录」+「缺少/缺」等关键词覆盖 B站/抖音/快手等适配器，
    // 避免各平台文案不一致时漏判。
    let has_login = message.contains("尚未登录") || message.contains("未登录");
    let has_missing = message.contains("缺少") || message.contains("缺");
    has_login && has_missing
}

/// 冷却是否仍生效：`now >= last_failed_at + cooldown` 时允许重试。
fn source_cooldown_active(now_secs: i64, last_failed_at: i64, cooldown_secs: u64) -> bool {
    cooldown_secs > 0 && now_secs.saturating_sub(last_failed_at) < cooldown_secs as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_watchlist_must_be_constructed_from_project_root_not_config_dir() {
        // 回归：run_tick_with_timeout 曾用 url_config().parent()（= config/ 目录）
        // 构造 Watchlist，导致它去读不存在的 config/config/URL_config.ini，
        // list_live() 永远返回空、live tick 每轮 checked=0、直播监控彻底失效。
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-sched-watch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(
            dir.join("config").join("URL_config.ini"),
            "原画,https://live.bilibili.com/24931109,侯翠翠\n",
        )
        .unwrap();
        // 修复后的构造方式：项目根
        let fixed = Watchlist::new(dir.clone());
        let rooms = fixed.list_live();
        assert_eq!(rooms.len(), 1, "从项目根构造必须能读到直播间");
        assert_eq!(rooms[0].url, "https://live.bilibili.com/24931109");
        assert!(rooms[0].enabled);
        // 旧实现（config/ 目录）必须读不到，防止回归
        let broken = Watchlist::new(fixed.url_config().parent().unwrap().to_path_buf());
        assert!(
            broken.list_live().is_empty(),
            "旧实现（config/ 目录）应读不到直播间"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paused_video_sources_are_not_checked() {
        // 回归：视频 tick 曾直接使用 `list_video()`，漏掉 `enabled` 过滤，
        // 导致「以 # 开头 = 暂停监控」对视频源不生效——注释掉的源照样每轮
        // 被拉取、照样通知下载。直播链路一直有这层过滤（见 live_monitor），
        // 视频链路漏了，两条路径行为不一致。
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-sched-paused-video-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(
            dir.join("config").join("视频监控列表.ini"),
            concat!(
                "# 注释说明行，不含地址，不该被当成源\n",
                "https://space.bilibili.com/1458143131,侯翠翠,下载=是\n",
                "#https://space.bilibili.com/946974,测试UP主,下载=是\n",
                "#https://www.douyin.com/user/MS4wLjABAAAAfake,关立新,下载=是\n",
            ),
        )
        .unwrap();
        let watch = Watchlist::new(dir.clone());

        // 状态快照仍要看得到全部条目（含暂停），否则界面没法把暂停源灰显出来。
        let all = watch.list_video();
        assert_eq!(all.len(), 3, "list_video 应保留暂停行供界面展示");
        assert_eq!(all.iter().filter(|s| s.enabled).count(), 1);

        // 真正要检查的只能有一个：停掉的测试源和未登录的抖音源都不许进列表。
        let checked = enabled_video_sources(&watch);
        assert_eq!(checked.len(), 1, "暂停的视频源不得进入待检查列表");
        assert_eq!(checked[0].url, "https://space.bilibili.com/1458143131");
        assert!(checked[0].enabled);

        let _ = std::fs::remove_dir_all(&dir);
    }

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
        let dir = crate::test_support::isolated_project("daemon-once");
        let file = crate::paths::database(&dir);
        let run_dir = crate::paths::run_dir(&dir);
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

    #[test]
    fn published_within_days_accepts_recent_and_rejects_old() {
        use chrono::Local;
        let now = Local::now().naive_local();
        // 刚发布的（1 天前）应判为「窗口内」
        let recent = now - chrono::Duration::days(1);
        assert!(published_within_days(
            &recent.format("%Y-%m-%d %H:%M:%S").to_string(),
            3
        ));
        // 恰好 3 天前（边界）应判为「窗口内」
        let edge = now - chrono::Duration::days(3);
        assert!(published_within_days(
            &edge.format("%Y-%m-%d %H:%M:%S").to_string(),
            3
        ));
        // 4 天前应判为「窗口外」
        let old = now - chrono::Duration::days(4);
        assert!(!published_within_days(
            &old.format("%Y-%m-%d %H:%M:%S").to_string(),
            3
        ));
        // 只有日期没有时间也能解析
        assert!(published_within_days(
            &recent.format("%Y-%m-%d").to_string(),
            3
        ));
        // 空字符串/非法日期视为「窗口外」，避免误下载
        assert!(!published_within_days("", 3));
        assert!(!published_within_days("not-a-date", 3));
    }

    #[tokio::test]
    async fn supervised_worker_restarts_after_error_and_stops_after_three() {
        let (tx, rx) = tokio::sync::watch::channel(false);
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let attempts_clone = attempts.clone();
        let supervisor = spawn_supervised_worker(
            "测试",
            tx.clone(),
            rx,
            Duration::from_millis(20),
            move |_stop| {
                let attempts = attempts_clone.clone();
                async move {
                    attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Err(anyhow::anyhow!("模拟 worker 异常"))
                }
            },
        );
        // 连续失败 3 次后监督器应主动停机并返回错误。
        let result = supervisor.await.expect("监督器任务本身不应 panic");
        assert!(result.is_err(), "连续失败 3 次监督器应返回错误: {result:?}");
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert!(*tx.borrow(), "连续失败后应触发 daemon 停机信号");
    }

    #[tokio::test]
    async fn supervised_worker_recovers_from_panic() {
        let (tx, rx) = tokio::sync::watch::channel(false);
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let attempts_clone = attempts.clone();
        let supervisor = spawn_supervised_worker(
            "测试",
            tx.clone(),
            rx,
            Duration::from_millis(20),
            move |_stop| {
                let attempts = attempts_clone.clone();
                async move {
                    let n = attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if n < 2 {
                        panic!("模拟 worker panic");
                    }
                    tokio::time::sleep(Duration::from_millis(10_000)).await;
                    Ok(())
                }
            },
        );
        // 前两次 panic 由监督器隔离并重启，第三次进入正常等待状态。
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert!(!*tx.borrow(), "panic 自愈成功不应触发停机");
        tx.send(true).unwrap();
        assert!(supervisor.await.expect("监督器任务本身不应 panic").is_ok());
    }

    #[tokio::test]
    async fn supervised_worker_stop_aborts_inner_long_task() {
        let (tx, rx) = tokio::sync::watch::channel(false);
        let running = std::sync::Arc::new(tokio::sync::Notify::new());
        let running_clone = running.clone();
        let supervisor = spawn_supervised_worker(
            "测试",
            tx.clone(),
            rx,
            Duration::from_millis(20),
            move |_stop| {
                let running = running_clone.clone();
                async move {
                    running.notify_waiters();
                    tokio::time::sleep(Duration::from_millis(10_000)).await;
                    Ok(())
                }
            },
        );
        running.notified().await;
        // 停机信号必须在监督器等待窗口内被响应，并 abort 内层 10 秒任务。
        tx.send(true).unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(2_000);
        tokio::time::timeout_at(deadline, supervisor)
            .await
            .expect("stop 后监督器应在时限内返回")
            .expect("监督器任务本身不应 panic")
            .expect("停机路径应返回 Ok");
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
        let dir = crate::test_support::isolated_project("daemon-duplicate");
        let file = crate::paths::database(&dir);
        let pool = crate::db::open_pool(&file).await.unwrap();
        db::init_db(&pool).await.unwrap();
        let cfg = crate::config::Config::load(Path::new(env!("CARGO_MANIFEST_DIR")))
            .unwrap_or_else(|_| crate::config::Config::defaults_for_test());
        // 第一个实例持有锁并写入自己的 pid（真实进程才能通过存活检查）。
        let lock = DaemonLock::try_acquire(&dir).unwrap().unwrap();
        crate::service::write_rust_daemon_pid(&dir, std::process::id());

        let second = run_daemon(pool.clone(), &dir, &cfg, 1, true, None).await;
        assert!(second.is_err(), "同一项目根下第二个 daemon 必须被拒绝");
        let pid_text =
            std::fs::read_to_string(crate::paths::run_dir(&dir).join("zunzhe-core.pid")).unwrap();
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
    fn new_video_notify_title_includes_anchor_and_video() {
        let title = new_video_notify_title("测试UP主", "你被AI内容骗过吗？");
        assert_eq!(title, "【新视频】测试UP主 发布了新内容：你被AI内容骗过吗？");
        assert!(
            title.contains("你被AI内容骗过吗？"),
            "标题必须带上视频标题，避免多条不同视频看起来像同一条: {title}"
        );
    }

    #[test]
    fn empty_success_does_not_clear_existing_failure_state() {
        assert!(!should_reset_source_failures(2, true));
        assert!(should_reset_source_failures(2, false));
        assert!(should_reset_source_failures(0, true));
    }

    #[tokio::test]
    async fn empty_success_between_failures_still_accumulates_cooldown() {
        use crate::db;

        let root = crate::test_support::isolated_project("source-failure-accumulation");
        let file = crate::paths::database(&root);
        let pool = db::open_pool(&file).await.unwrap();
        db::init_db(&pool).await.unwrap();
        let source = "https://space.bilibili.com/946974";
        let watch = Watchlist::new(root.clone());
        let library = VideoLibrary::new(pool.clone(), crate::paths::fupan(&root));
        let config = Config::defaults_for_test();
        let notifier = Notifier::for_project(&pool, &config, &root);
        let mut known_ids = None;
        let mut checked = 0usize;
        let mut notified = 0usize;
        let mut enqueued = 0usize;
        let mut skipped_missing_credential = 0usize;

        // 第一次真实失败：必须写 1，并记录失败时间。
        apply_source_outcome(
            &pool,
            &watch,
            &library,
            &notifier,
            SourceCheckOutcome {
                source_url: source.to_string(),
                name: "测试UP主".to_string(),
                item_download: None,
                videos: Err(anyhow!("官方接口返回 HTTP 412")),
                timed_out: false,
            },
            false,
            false,
            false,
            &mut known_ids,
            &mut checked,
            &mut notified,
            &mut enqueued,
            &mut skipped_missing_credential,
        )
        .await
        .unwrap();
        let (fails, last) = db::source_failure_state(&pool, source).await.unwrap();
        assert_eq!(fails, 1);
        assert!(last > 0);

        // 平台风控有时会返回“成功但空列表”。这不是恢复信号：若这里清零，
        // 下一轮失败仍会显示 1，冷却永远不会达到 3，B站会被无限重打。
        apply_source_outcome(
            &pool,
            &watch,
            &library,
            &notifier,
            SourceCheckOutcome {
                source_url: source.to_string(),
                name: "测试UP主".to_string(),
                item_download: None,
                videos: Ok(Vec::new()),
                timed_out: false,
            },
            false,
            false,
            false,
            &mut known_ids,
            &mut checked,
            &mut notified,
            &mut enqueued,
            &mut skipped_missing_credential,
        )
        .await
        .unwrap();

        // 第二次真实失败必须累加到 2。
        apply_source_outcome(
            &pool,
            &watch,
            &library,
            &notifier,
            SourceCheckOutcome {
                source_url: source.to_string(),
                name: "测试UP主".to_string(),
                item_download: None,
                videos: Err(anyhow!("官方接口返回 HTTP 412")),
                timed_out: false,
            },
            false,
            false,
            false,
            &mut known_ids,
            &mut checked,
            &mut notified,
            &mut enqueued,
            &mut skipped_missing_credential,
        )
        .await
        .unwrap();
        let (fails, last_failed_at) = db::source_failure_state(&pool, source).await.unwrap();
        assert_eq!(fails, 2, "空成功不得把连续失败计数清零");
        assert!(last_failed_at > 0, "失败时间必须保留: {last_failed_at}");

        std::mem::forget(pool);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_credential_errors_skip_without_cooldown() {
        // 真实的「未登录」文案（抖音适配器）应被识别为确定性错误。
        assert!(is_missing_credential_error(
            "尚未登录抖音：缺少 /path/douyin_browser_cookies.txt，请先在桌面端完成「抖音授权」扫码登录"
        ));
        // B站/快手风格的同类文案也应命中。
        assert!(is_missing_credential_error(
            "尚未登录：缺少 bilibili cookie 文件"
        ));
        // 只有「未登录」没有「缺少」时不算（避免把别的语义误判）。
        assert!(!is_missing_credential_error("尚未登录"));
        // 暂时性错误不能被误判成确定性错误，否则会绕过冷却。
        assert!(!is_missing_credential_error("官方接口返回 HTTP 412"));
        assert!(!is_missing_credential_error("请求超时（50 秒）"));
        assert!(!is_missing_credential_error("RSSHub 请求失败: 连接超时"));
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
    async fn concurrent_video_checks_isolate_slow_source() {
        use std::time::Duration;

        struct SlowPlatform;
        struct FastPlatform;

        impl platform::Platform for SlowPlatform {
            fn name(&self) -> &'static str {
                "slow"
            }

            fn list_videos<'a>(
                &'a self,
                _url: &'a str,
                _count: u32,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = anyhow::Result<Vec<crate::model::VideoMeta>>>
                        + Send
                        + 'a,
                >,
            > {
                Box::pin(async move {
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    Err(anyhow!("慢源超时"))
                })
            }
        }

        impl platform::Platform for FastPlatform {
            fn name(&self) -> &'static str {
                "fast"
            }

            fn list_videos<'a>(
                &'a self,
                url: &'a str,
                _count: u32,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = anyhow::Result<Vec<crate::model::VideoMeta>>>
                        + Send
                        + 'a,
                >,
            > {
                let url = url.to_string();
                Box::pin(async move {
                    Ok(vec![crate::model::VideoMeta {
                        id: format!("fast-{url}"),
                        platform: "test".into(),
                        url: url.clone(),
                        title: "fast".into(),
                        cover: String::new(),
                        published_at: "2026-08-15".into(),
                        duration: 0,
                    }])
                })
            }
        }

        let root = std::env::temp_dir().join(format!(
            "zunzhe-video-conc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let db_file = root.join("test.db");
        let pool = crate::db::open_pool(&db_file).await.unwrap();
        db::init_db(&pool).await.unwrap();

        let (outcomes, _skipped, _proxy_off) = check_video_sources_concurrent(
            &pool,
            &root,
            vec![
                ("slow-url".to_string(), "慢源".to_string(), None),
                ("fast-url".to_string(), "快源".to_string(), None),
            ],
            2,
            move |url, _| {
                if url == "slow-url" {
                    Box::new(SlowPlatform)
                } else {
                    Box::new(FastPlatform)
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(outcomes.len(), 2);
        assert!(
            outcomes[0].videos.is_err(),
            "慢源失败应作为单源错误保留，不吞掉"
        );
        assert!(
            outcomes[1].videos.is_ok(),
            "快源不应被慢源阻塞：并发上限 2 时两者同时执行"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn proxy_off_precheck_only_targets_proxied_domains() {
        // needs_proxy 语义：未配置代理恒 false；配置代理后仅“非直连域名”为 true。
        let directs = vec!["bilibili.com".to_string()];
        let no_proxy = crate::net::OutboundPolicy::for_test(None, directs.clone());
        assert!(!no_proxy.needs_proxy("https://rsshub.app/douyin/user/x"));
        assert!(!no_proxy.needs_proxy("https://space.bilibili.com/1"));

        let with_proxy =
            crate::net::OutboundPolicy::for_test(Some("http://127.0.0.1:7891"), directs);
        assert!(with_proxy.needs_proxy("https://rsshub.app/douyin/user/x"));
        assert!(!with_proxy.needs_proxy("https://space.bilibili.com/1"));

        // 未配置代理时 proxy_alive 返回 None（无需探测），调度器跳过逻辑不触发。
        // 用当前测试进程的全局策略（未 init 时默认全直连）验证 None 分支。
        let default_alive = futures_executor_block_on(crate::net::policy().proxy_alive());
        assert_eq!(default_alive, None, "全直连策略不应有代理可探测");
    }

    fn futures_executor_block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fut)
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
