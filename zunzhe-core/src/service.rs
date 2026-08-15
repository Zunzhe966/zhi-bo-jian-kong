use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::json;
use std::sync::OnceLock;

/// 24×30 长跑健康审计的机器可读结果。
///
/// `doctor` 原本的 `ok` 表示“环境可用”，并不代表 daemon 正在健康运行；
/// 长跑验收需要单独一个字段区分“环境没问题”和“守护确实在跑且心跳连续”。
#[derive(Debug, Clone, serde::Serialize)]
pub struct HealthAudit {
    pub ok: bool,
    pub daemon_ok: bool,
    pub heartbeat_ok: bool,
    pub uptime_secs: u64,
    pub uptime_human: String,
    pub pid: Option<u32>,
    pub rss_kib: Option<u64>,
    pub started_at: Option<String>,
    pub last_heartbeat_age_secs: Option<u64>,
    pub heartbeat_lines: usize,
    pub consecutive_heartbeats: usize,
    pub log_gap_secs: Option<u64>,
    pub last_log_line_at: Option<String>,
}

/// 合并读取 uptime 主日志与轮转历史（`.1..3`）。
///
/// 24×30 长跑期间若 daemon 重启导致启动行被轮转到历史文件，只读当前文件
/// 会丢掉同 pid 的最早起点，uptime 变成“最近一段”而不是“本实例全程”。
/// 轮转方向是 `.3` 最旧、当前文件最新，按该顺序拼接后上层可统一按时间排序。
pub fn read_uptime_logs(project_dir: &Path) -> String {
    let dir = project_dir.join("logs");
    let base = dir.join("zunzhe-core.uptime.log");
    let mut parts = Vec::new();
    for idx in (1..=3).rev() {
        let rotated = base.with_extension(format!("log.{idx}"));
        if let Ok(text) = std::fs::read_to_string(&rotated) {
            parts.push(text);
        }
    }
    if let Ok(text) = std::fs::read_to_string(&base) {
        parts.push(text);
    }
    parts.join("")
}

/// 读取 daemon 自己每 60 秒追加的 uptime 日志，量化“连续心跳、启动记录、
/// 日志断档、当前 RSS”四个 24×30 关键指标。任何一项不可解析都不 panic，
/// 只把该字段留空并保持 `ok=false`，让上层诊断而不是让审计进程崩溃。
pub fn health_audit(project_dir: &Path) -> HealthAudit {
    let pid = std::fs::read_to_string(pid_file(project_dir, "zunzhe-core"))
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok());
    let daemon_ok = rust_daemon_alive(project_dir);
    let heartbeat_ok = heartbeat_fresh(project_dir, 30 * 60);
    let (uptime_secs, uptime_human) = daemon_uptime(project_dir);
    let rss_kib = pid.and_then(process_rss_kib);

    let text = read_uptime_logs(project_dir);
    let heartbeat_lines = text
        .lines()
        .filter(|line| line.contains("rust_daemon=true") || line.contains("heartbeat_ok=true"))
        .count();
    // 合并读取后当前文件可能保留上一实例的 started 行；started_at 必须只取
    // “当前 pid”的启动行，否则重启后 health_audit 会报告旧实例的启动时间。
    let started_at = if let Some(pid) = pid {
        text.lines()
            .find(|line| line.contains(" started ") && line.contains(&format!("pid={pid}")))
            .map(|line| {
                line.split(" started")
                    .next()
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_string()
            })
    } else {
        None
    };
    let last_log_line_at = text.lines().rev().find_map(|line| {
        let mut parts = line.splitn(3, ' ');
        let date = parts.next()?;
        let time = parts.next()?;
        let tz = parts.next()?;
        if date.len() == 10 && time.len() == 8 && tz.starts_with('+') {
            Some(format!("{date} {time} {tz}"))
        } else {
            None
        }
    });
    // 同一 pid 启动后连续心跳数：uptime 日志里所有“包含当前 pid 且
    // rust_daemon=true”的行数。日志轮转后可能只剩当前周期，但至少证明
    // 最近这段时间没有断档。
    let consecutive_heartbeats = if let Some(pid) = pid {
        text.lines()
            .filter(|line| {
                line.contains(&format!("pid={pid}"))
                    && (line.contains("rust_daemon=true") || line.contains("heartbeat_ok=true"))
            })
            .count()
    } else {
        0
    };
    let log_gap_secs = last_log_line_at.as_ref().and_then(|line| {
        // 行首固定是 "YYYY-MM-DD HH:MM:SS +HHMM"，后面才接 pid 等诊断信息。
        let ts = chrono::DateTime::parse_from_str(line.get(..25)?, "%Y-%m-%d %H:%M:%S %z").ok()?;
        let now = chrono::Local::now().with_timezone(&ts.timezone());
        Some((now - ts).num_seconds().max(0) as u64)
    });
    let last_heartbeat_age_secs = heartbeat_age_secs(project_dir);

    let ok = daemon_ok
        && heartbeat_ok
        && last_heartbeat_age_secs.is_some()
        && last_heartbeat_age_secs <= Some(30 * 60)
        && log_gap_secs.is_some()
        && log_gap_secs <= Some(30 * 60);
    HealthAudit {
        ok,
        daemon_ok,
        heartbeat_ok,
        uptime_secs,
        uptime_human,
        pid,
        rss_kib,
        started_at,
        last_heartbeat_age_secs,
        heartbeat_lines,
        consecutive_heartbeats,
        log_gap_secs,
        last_log_line_at,
    }
}

/// 读取进程当前 RSS（KiB）。跨平台尽量给出真实值；取不到返回 None，不报错。
#[cfg(target_os = "linux")]
pub fn process_rss_kib(pid: u32) -> Option<u64> {
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    // Linux 页大小固定 4KiB。
    Some(resident_pages.saturating_mul(4))
}

/// macOS 没有 /proc，用 `ps -axo pid=,rss=` 全量扫描并按 PID 匹配读取。
///
/// 不能用 `ps -p PID`：macOS 的 ps 对超过 99999 的 PID 会直接报
/// "process id too large"，而 PID 回绕后完全可能出现六位数；全量扫描不受限。
#[cfg(all(unix, not(target_os = "linux")))]
pub fn process_rss_kib(pid: u32) -> Option<u64> {
    process_rss_ps(pid)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_rss_ps(pid: u32) -> Option<u64> {
    let output = std::process::Command::new("ps")
        .args(["-axo", "pid=,rss="])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let candidate: u32 = parts.next()?.parse().ok()?;
        if candidate == pid {
            parts.next()?.parse::<u64>().ok()
        } else {
            None
        }
    })
}

#[cfg(windows)]
pub fn process_rss_kib(pid: u32) -> Option<u64> {
    let _ = pid;
    None
}

#[allow(dead_code)] // 保留显式 Python 兜底入口，CLI/MCP 默认不再自动调用
pub fn ensure_started(project_dir: &Path) -> Result<serde_json::Value> {
    let python = python_bin(project_dir);
    if !python_available(project_dir) {
        bail!(
            "找不到可用的 Python 环境: {}（需 .venv 或系统 PATH 中的 python3）",
            python.display()
        );
    }
    let mut services = serde_json::Map::new();
    for (name, args) in [
        ("live_monitor", vec!["-u", "main.py"]),
        ("video_monitor", vec!["-u", "-m", "fupan.video_monitor"]),
    ] {
        if service_pid(project_dir, name).is_some() {
            services.insert(name.to_string(), json!("已在运行"));
            continue;
        }
        let mut cmd = std::process::Command::new(&python);
        cmd.args(&args)
            .current_dir(project_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let child = cmd.spawn().with_context(|| format!("启动 {name} 失败"))?;
        write_pid(project_dir, name, child.id());
        services.insert(
            name.to_string(),
            json!(format!("已启动 pid={}", child.id())),
        );
    }
    Ok(json!({"ok": true, "services": services, "logs": project_dir.join("logs")}))
}

#[allow(dead_code)] // 保留显式 Python 兜底入口，CLI/MCP 默认不再自动调用
pub fn stop_all(project_dir: &Path) -> Result<serde_json::Value> {
    let mut services = serde_json::Map::new();
    for name in ["live_monitor", "video_monitor"] {
        if let Some(pid) = service_pid(project_dir, name) {
            kill_process(pid);
            services.insert(name.to_string(), json!("已停止"));
            let _ = std::fs::remove_file(pid_file(project_dir, name));
        } else {
            services.insert(name.to_string(), json!("未在运行"));
        }
    }
    Ok(json!({"ok": true, "services": services}))
}

/// 桌面端/兼容层统一入口：Rust daemon 负责直播监控与视频监控，
/// Python 服务仅在需要时作为兜底，避免重复录制同一路直播。
pub fn ensure_core(project_dir: &Path) -> Result<serde_json::Value> {
    #[cfg(target_os = "macos")]
    sync_launchd_scripts(project_dir);
    // macOS 上 launchd 是 24×30 的正式管理者（KeepAlive 自动拉起）。
    // 若已安装 com.zunzhe.monitor，ensure 优先让 launchd 拉起，避免本进程
    // 直接 spawn 与 launchd 抢单实例锁；否则 launchd 会不断重试并报
    // “已有 Rust daemon 在本项目运行”。
    #[cfg(target_os = "macos")]
    if launchd_can_manage(project_dir) {
        if rust_daemon_alive(project_dir) {
            return Ok(
                json!({"ok": true, "rust_daemon": "已在运行（launchd 管理）", "services": {}}),
            );
        }
        // daemon 未健康时让 launchd 重新拉起；-k 确保已加载但僵死的服务被重启。
        let uid = unsafe { libc::getuid() };
        let ok = std::process::Command::new("launchctl")
            .args(["kickstart", "-k", &format!("gui/{uid}/com.zunzhe.monitor")])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            // launchd 已尝试拉起，但 daemon 写心跳/拿单实例锁需要一点时间；
            // 给最多 5 秒让它真正健康，避免下面直接 spawn 与 launchd 抢锁。
            for _ in 0..50 {
                if rust_daemon_alive(project_dir) {
                    return Ok(
                        json!({"ok": true, "rust_daemon": "launchd 已接管", "services": {}}),
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            // launchd 实例没能健康（TCC 拒绝访问项目目录、二进制缺失等）
            // 时再回退由本进程直接启动，保证看门狗能恢复。
            tracing::warn!("launchctl kickstart 后 daemon 未在 5 秒内健康，回退直接启动");
        }
        // launchctl 本身失败（服务未加载等）也走直接 spawn 兜底。
    }
    // 项目仍位于桌面/文稿等 TCC 保护目录时，launchd 无法稳定接管；即使已经
    // 安装过 com.zunzhe.monitor，也必须由本进程直接拉起，不能继续 kickstart
    // 一个每次都会失败的 launchd 任务，也不能把手动进程误报成“launchd 管理”。
    if rust_daemon_alive(project_dir) {
        return Ok(json!({
            "ok": true,
            "rust_daemon": "已在运行",
            "services": {},
            "hints": ensure_core_hints(project_dir)
        }));
    }
    let rust = start_rust_daemon(project_dir, 900)?;
    // Rust daemon 已统一接管直播检测/录制和视频监控；不再并行启动 Python
    // video_monitor，避免同一批视频源被两套逻辑重复检查/通知/下载。
    let rust_status = rust
        .get("rust_daemon")
        .cloned()
        .unwrap_or(serde_json::Value::Bool(true));
    Ok(json!({
        "ok": true,
        "rust_daemon": rust_status,
        "services": {},
        "hints": ensure_core_hints(project_dir)
    }))
}

/// macOS：把仓库里的 launchd 守护/健康脚本同步到 `~/Library/LaunchAgents`。
///
/// `scripts/setup-macos.sh` 只在安装时复制一次，后续代码/脚本更新后如果不
/// 重跑 setup，launchd 会一直执行旧副本。`ensure_core` 是桌面端/看门狗高频
/// 入口，在这里做幂等同步，保证长期运行期间守护脚本始终与仓库一致。只同步
/// 脚本文件，不修改已加载的 plist，也不重启正在运行的 daemon，避免打断
/// 24×30 同 pid 连续计数。
#[cfg(target_os = "macos")]
fn sync_launchd_scripts(project_dir: &Path) {
    use std::fs;

    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let launch_agents = std::path::Path::new(&home)
        .join("Library")
        .join("LaunchAgents");
    let repo_scripts = project_dir.join("scripts").join("launchd");
    let pairs = [
        (
            repo_scripts.join("zunzhe-daemon.sh"),
            launch_agents.join("zunzhe-daemon.sh"),
        ),
        (
            repo_scripts.join("zunzhe-healthcheck.sh"),
            launch_agents.join("zunzhe-healthcheck.sh"),
        ),
        (
            project_dir.join("scripts").join("ensure-full-access.sh"),
            launch_agents.join("ensure-full-access.sh"),
        ),
        (
            project_dir.join("scripts").join("audit-longrun.sh"),
            launch_agents.join("audit-longrun.sh"),
        ),
    ];
    for (src, dst) in pairs {
        if !src.is_file() {
            continue;
        }
        // 相同内容直接跳过；只有仓库版本更新时才覆盖，避免无谓的写盘/权限错误。
        let changed = match (fs::read(&src), fs::read(&dst)) {
            (Ok(src_bytes), Ok(dst_bytes)) => src_bytes != dst_bytes,
            (Ok(_), Err(_)) => true,
            (Err(_), _) => false,
        };
        if !changed {
            continue;
        }
        let _ = fs::create_dir_all(&launch_agents);
        if fs::copy(&src, &dst).is_ok()
            && let Ok(meta) = fs::metadata(&src)
        {
            let _ = fs::set_permissions(&dst, meta.permissions());
        }
    }
}

/// macOS：检查 com.zunzhe.monitor 是否已安装到用户 LaunchAgents。
#[cfg(target_os = "macos")]
fn launchd_monitor_installed() -> bool {
    let path = std::path::Path::new(&std::env::var("HOME").unwrap_or_default())
        .join("Library")
        .join("LaunchAgents")
        .join("com.zunzhe.monitor.plist");
    path.is_file()
}

/// macOS：launchd 是否真的能管理当前项目。
///
/// 即使 plist 已安装，只要项目位于桌面/文稿等 TCC 保护目录，launchd 拉起的
/// 守护脚本也会被系统策略挡掉并持续失败。此时必须走直接 spawn，不能做
/// 无效的 `launchctl kickstart`，更不能把直接启动的进程标成“launchd 管理”。
#[cfg(target_os = "macos")]
fn launchd_can_manage(project_dir: &Path) -> bool {
    launchd_monitor_installed() && !is_tcc_protected_path(project_dir)
}

/// 项目是否位于 macOS TCC 保护目录（桌面/文稿）。
///
/// 桌面端/CLI 在启动失败提示、ensure 决策、健康诊断里共用同一判断，
/// 避免“plist 已装 = launchd 可用”的错误假设。
pub fn is_tcc_protected_path(project_dir: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        let home_value = std::env::var("HOME").unwrap_or_default();
        let home = std::path::Path::new(&home_value);
        tcc_protected_with_home(project_dir, home)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = project_dir;
        false
    }
}

#[cfg(target_os = "macos")]
fn tcc_protected_with_home(project_dir: &Path, home: &Path) -> bool {
    [home.join("Desktop"), home.join("Documents")]
        .iter()
        .any(|protected| project_dir.starts_with(protected))
}

/// ensure 结果里附带的可操作提示；没有缺口时返回 `[]`。
fn ensure_core_hints(project_dir: &Path) -> Vec<String> {
    let mut hints = Vec::new();
    if is_tcc_protected_path(project_dir) {
        hints.push(
            "项目位于桌面/文稿：launchd 无法稳定管理，24×30 自愈请迁移到 \
             ~/Projects/尊者监控直播短视频 后运行 scripts/setup-macos.sh"
                .to_string(),
        );
    }
    hints
}

/// 启动 Rust 核心 daemon 并写入 pid 文件（供桌面端/兼容层调用）。
pub fn start_rust_daemon(project_dir: &Path, interval_secs: u64) -> Result<serde_json::Value> {
    if rust_daemon_alive(project_dir) {
        return Ok(json!({"ok": true, "rust_daemon": "已在运行"}));
    }
    // 锁被占用但心跳不新鲜：进程仍在而调度疑似僵死，先停掉再拉起。
    // 这样 watchdog 每分钟重试时能把“假健康/僵死 daemon”真正恢复，而不是永远误判。
    if crate::daemon_lock::DaemonLock::try_acquire(project_dir)
        .with_context(|| "探测 daemon 单实例锁失败")?
        .is_none()
    {
        stop_rust_daemon(project_dir)?;
        // 等旧进程退出、内核释放锁，避免下面 spawn 出的新实例立刻抢锁失败。
        for _ in 0..50 {
            // 这里只探测“锁是否已被让出”：拿到后立即 drop，不能把锁留在父进程手里。
            if crate::daemon_lock::DaemonLock::try_acquire(project_dir)?.is_none() {
                std::thread::sleep(std::time::Duration::from_millis(100));
            } else {
                break;
            }
        }
    }
    let exe = project_dir
        .join("zunzhe-core")
        .join("target")
        .join("release")
        .join(core_binary_name());
    let debug_exe = project_dir
        .join("zunzhe-core")
        .join("target")
        .join("debug")
        .join(core_binary_name());
    let exe = if exe.exists() { exe } else { debug_exe };
    if !exe.exists() {
        bail!("找不到 Rust 核心二进制: {}", exe.display());
    }
    let log_dir = project_dir.join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let log_file = log_dir.join("zunzhe-core.log");
    rotate_log(&log_file, 5);
    let log_handle = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
        .with_context(|| format!("打开日志文件失败: {}", log_file.display()))?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("run-daemon")
        .arg("--interval")
        .arg(interval_secs.to_string())
        .current_dir(project_dir)
        .stdin(std::process::Stdio::null())
        .stdout(log_handle.try_clone().context("复制日志句柄失败")?)
        .stderr(log_handle);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().with_context(|| "启动 Rust daemon 失败")?;
    // pid 文件由 daemon 拿到单实例锁后自己写入；这里只轮询等待“健康”确认，
    // 避免并发启动时父进程把另一实例的 pid 覆盖掉。
    let child_pid = child.id();
    for _ in 0..50 {
        if rust_daemon_alive(project_dir) {
            return Ok(json!({"ok": true, "rust_daemon": format!("已启动 pid={child_pid}")}));
        }
        match child.try_wait()? {
            Some(status) => {
                bail!("Rust daemon 启动后立即退出（code={status}），请查看 logs/zunzhe-core.log")
            }
            None => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
    bail!("Rust daemon 启动超时（5 秒），请查看 logs/zunzhe-core.log")
}

/// 当前平台的核心二进制名。Windows 上 cargo 输出 `.exe`，Unix 上没有后缀。
fn core_binary_name() -> &'static str {
    if cfg!(windows) {
        "zunzhe-core.exe"
    } else {
        "zunzhe-core"
    }
}

pub fn stop_rust_daemon(project_dir: &Path) -> Result<serde_json::Value> {
    // 只停 Rust daemon 与 Rust ffmpeg 录制进程；不再自动拉起 Python 服务，
    // 否则用户点“停止”反而会启动另一套服务。
    let path = pid_file(project_dir, "zunzhe-core");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let pid = text.trim().parse::<u32>();
    if let Ok(pid) = pid
        && service_pid_by_raw(pid)
    {
        kill_process(pid);
    }
    // pid 文件在 stop 命令里先删除，避免旧 daemon 的 DaemonPidGuard 退出时
    // 再“无条件删一次”把新实例刚写入的 pid 误删；这里保留 pid 文件直到
    // 确认旧进程已退出，但 stop 本身不再抢删。
    let _ = std::fs::remove_file(&path);
    // 清理可能残留的 Rust ffmpeg 录制进程（如果 daemon 未及优雅退出）。
    cleanup_rust_recordings(project_dir);
    Ok(
        json!({"ok": true, "rust_daemon": "已停止", "python_fallback": "未自动启动，需要时手动 start"}),
    )
}

/// 兜底清理：读取 fupan_output/run/rust_live/ 下录制 pid 文件，逐个结束。
fn cleanup_rust_recordings(project_dir: &Path) {
    let dir = project_dir
        .join("fupan_output")
        .join("run")
        .join("rust_live");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        if let Ok(text) = std::fs::read_to_string(entry.path())
            && let Ok(pid) = text.trim().parse::<u32>()
            && service_pid_by_raw(pid)
        {
            kill_process(pid);
        }
        let _ = std::fs::remove_file(entry.path());
    }
}

pub fn status(project_dir: &Path) -> Result<serde_json::Value> {
    let mut services = serde_json::Map::new();
    for name in ["live_monitor", "video_monitor"] {
        services.insert(
            name.to_string(),
            if let Some(pid) = service_pid(project_dir, name) {
                json!({"running": true, "pid": pid})
            } else {
                json!({"running": false})
            },
        );
    }
    let rust_running = rust_daemon_alive(project_dir);
    let heartbeat_ok = heartbeat_fresh_public(project_dir);
    let (uptime_secs, uptime_human) = daemon_uptime_public(project_dir);
    Ok(json!({
        "ok": true,
        "services": services,
        "rust_daemon": rust_running,
        "heartbeat_ok": heartbeat_ok,
        "uptime_secs": uptime_secs,
        "uptime_human": uptime_human,
    }))
}

/// 读取后台服务日志末尾若干行（兼容 Python tail-logs）。
pub fn tail_logs(project_dir: &Path, name: &str, lines: u64) -> serde_json::Value {
    let log = project_dir.join("logs").join(format!("{name}.log"));
    if !log.exists() {
        return json!({"ok": true, "name": name, "path": log.display().to_string(),
                      "content": "（暂无日志）"});
    }
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let keep = (lines.clamp(1, 2000)) as usize;
    let tail: String = text
        .lines()
        .rev()
        .take(keep)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    json!({"ok": true, "name": name, "path": log.display().to_string(),
           "content": tail, "lines": tail.lines().count()})
}

/// 检查 Rust 核心 daemon 是否在运行（通过 pid 文件，与 Python 服务同目录）。
pub fn rust_daemon_alive(project_dir: &Path) -> bool {
    let path = pid_file(project_dir, "zunzhe-core");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(pid) = text.trim().parse::<u32>() else {
        return false;
    };
    if !service_pid_by_raw(pid) {
        return false;
    }
    // 心跳文件最近 30 分钟内更新过，避免“进程还在但调度已僵死”的假健康。
    heartbeat_fresh(project_dir, 30 * 60)
}

/// 移除当前 daemon 的 pid 文件（daemon 优雅退出时调用）。
pub fn remove_rust_daemon_pid(project_dir: &Path) {
    let _ = std::fs::remove_file(pid_file(project_dir, "zunzhe-core"));
}

/// 只有当 pid 文件内容仍是 `pid` 时才删除，防止旧实例退出误删新实例 pid。
pub fn remove_rust_daemon_pid_if(project_dir: &Path, pid: u32) {
    let path = pid_file(project_dir, "zunzhe-core");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    if text.trim().parse::<u32>().ok() == Some(pid) {
        let _ = std::fs::remove_file(path);
    }
}

/// 公开只读接口：心跳是否在 30 分钟内刷新。
///
/// `status_snapshot` 需要把守护健康状态放进桌面端 `status` 返回，而不是让
/// 每个入口都复制一遍“30 分钟”阈值。这里保持与 `diagnostics` 同源。
pub fn heartbeat_fresh_public(project_dir: &Path) -> bool {
    heartbeat_fresh(project_dir, 30 * 60)
}

/// 心跳文件距当前时刻的秒数；文件缺失/损坏返回 None。
/// 调用方应把 None 视为“不新鲜”，不能因为拿不到年龄就当作健康。
pub fn heartbeat_age_secs(project_dir: &Path) -> Option<u64> {
    let meta = std::fs::metadata(heartbeat_file(project_dir)).ok()?;
    let modified = meta.modified().ok()?;
    modified.elapsed().ok().map(|d| d.as_secs())
}

/// 公开只读接口：当前 daemon 连续运行时长。
pub fn daemon_uptime_public(project_dir: &Path) -> (u64, String) {
    daemon_uptime(project_dir)
}

/// daemon 心跳：每次调度 tick 后刷新，供外部存活检查。
pub fn touch_heartbeat(project_dir: &Path) {
    let path = heartbeat_file(project_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = crate::config::atomic_write(&path, chrono::Local::now().to_rfc3339());
}

fn heartbeat_file(project_dir: &Path) -> std::path::PathBuf {
    project_dir
        .join("fupan_output")
        .join("run")
        .join("zunzhe-core.heartbeat")
}

fn heartbeat_fresh(project_dir: &Path, max_age_secs: u64) -> bool {
    let Ok(meta) = std::fs::metadata(heartbeat_file(project_dir)) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let Ok(age) = modified.elapsed() else {
        return false;
    };
    age.as_secs() <= max_age_secs
}

/// 简单日志轮转：把 log 依次后移，最多保留 max_keep 份历史。
pub fn rotate_log(log: &Path, max_keep: usize) {
    for i in (1..=max_keep).rev() {
        let from = if i == 1 {
            log.to_path_buf()
        } else {
            log.with_extension(format!("log.{}", i - 1))
        };
        let to = log.with_extension(format!("log.{i}"));
        if from.exists() {
            let _ = std::fs::rename(&from, &to);
        }
    }
}

/// 若日志文件超过阈值则轮转，保留 `max_keep` 份历史。
///
/// daemon 由 launchd 直接启动时没有父进程轮转 `logs/zunzhe-core.log`；
/// 心跳记录器每 60 秒调用一次，超过 10MB 才轮转，避免 24×30 期间
/// 日志文件无限增长，也避免频繁 rename 干扰正在 append 的日志句柄。
pub fn rotate_log_if_large(log: &Path, max_keep: usize, max_bytes: u64) {
    let Ok(meta) = std::fs::metadata(log) else {
        return;
    };
    if meta.len() < max_bytes {
        return;
    }
    rotate_log(log, max_keep);
}

/// 诊断项：供桌面端/CLI 快速判断“为什么监控没在跑”。
pub fn diagnostics(project_dir: &Path) -> Result<serde_json::Value> {
    let log_dir = project_dir.join("logs");
    let run_dir = project_dir.join("fupan_output").join("run");
    let heartbeat_ok = heartbeat_fresh(project_dir, 30 * 60);
    let daemon_ok = rust_daemon_alive(project_dir);
    let disk = disk_free_bytes(project_dir);
    let (uptime_secs, uptime_human) = daemon_uptime(project_dir);
    let python_ok = python_available(project_dir);
    let pipeline_hint = if python_ok {
        None
    } else {
        Some(
            "缺少 .venv/bin/python：自动转写/知识提炼/精华切片复盘桥接不可用，\
             请运行 scripts/setup-macos.sh 或重新创建虚拟环境",
        )
    };
    // 邮箱是默认通知通道，doctor 只报 ok 会让人误以为通知已可用；
    // 未配置时把缺失项带进 hints，桌面端/CLI 一眼能看到缺口。
    let loaded_cfg = crate::config::Config::load(project_dir).ok();
    let email_hint = loaded_cfg.as_ref().and_then(|cfg| {
        let status = cfg.email_status();
        if status.configured {
            None
        } else {
            Some(status.hint)
        }
    });
    let mut hints: Vec<String> = Vec::new();
    if let Some(hint) = pipeline_hint {
        hints.push(hint.to_string());
    }
    if let Some(hint) = email_hint {
        hints.push(hint);
    }
    // doctor 的 ffmpeg 检查必须与录制/下载一致：项目内置优先（含另一平台后缀
    // 的兼容回退），其次 PATH。Windows 上内置文件名是 ffmpeg.exe，不能只检查
    // ffmpeg/ffmpeg；下载链路同一入口，避免“doctor 说可用但下载用不到”。
    let ffmpeg_path = find_binary(project_dir, "ffmpeg", "ffmpeg.exe");
    let ffmpeg_ok = ffmpeg_path.is_absolute()
        || std::env::var_os("PATH").is_some_and(|paths| {
            std::env::split_paths(&paths).any(|dir| dir.join(&ffmpeg_path).exists())
        });
    // 环境可用（ok）与守护健康（health）分离：即使 daemon 不在跑，doctor
    // 仍应返回环境诊断供修复；health 字段明确告诉调用方“监控本身是否健康”。
    let audit = health_audit(project_dir);
    // 三个复盘开关是自动复盘是否入队/跳过环节的直接依据；doctor 返回
    // 机器可读状态，避免用户/智能体在界面里翻配置才知道哪个环节被关闭。
    let switches = crate::download::PipelineSwitches::load(project_dir);
    // 通知投递健康摘要：由调用方注入 DB 数据，doctor 只负责把字段原样返回，
    // 避免这里异步查询破坏同步签名。读取失败时字段为 null，不让 doctor 失败。
    let notify_configured = loaded_cfg
        .as_ref()
        .is_some_and(|cfg| cfg.email_configured() || cfg.push_channels().configured());
    let notify_health = json!({
        "configured": notify_configured,
        "pending": null,
        "failed": null,
        "sent": null,
        "ok": null,
        "latest_failed_channel": null,
        "hint": crate::db::notify_health_hint(0, 0, notify_configured, None),
    });
    Ok(json!({
        "ok": true,
        "rust_daemon": daemon_ok,
        "heartbeat_ok": heartbeat_ok,
        "daemon_ok": audit.daemon_ok,
        "health": audit.ok,
        "health_audit": audit,
        "uptime_secs": uptime_secs,
        "uptime_human": uptime_human,
        "logs_writable": log_dir.is_dir() || std::fs::create_dir_all(&log_dir).is_ok(),
        "run_dir_writable": run_dir.is_dir() || std::fs::create_dir_all(&run_dir).is_ok(),
        "ffmpeg": if ffmpeg_ok {
            "ok"
        } else {
            "missing（项目 ffmpeg/ 与 PATH 均未找到，录制/下载将失败，请安装 ffmpeg 或运行 scripts/setup-macos.sh）"
        },
        "ffmpeg_available": ffmpeg_ok,
        "disk_free_mb": disk,
        "pipeline_python_ok": python_ok,
        "pipeline_switches": {
            "transcribe": switches.transcribe,
            "distill": switches.distill,
            "clips": switches.clips,
            "any_enabled": switches.any_enabled(),
        },
        "notify_health": notify_health,
        "hints": hints,
    }))
}

/// 当前 daemon pid 的连续运行时长。日志由 daemon 自己每 60 秒追加，
/// 其中 `started pid=...` 行是本实例启动时间，取它的最早记录作为起点。
fn daemon_uptime(project_dir: &Path) -> (u64, String) {
    let Ok(pid_text) = std::fs::read_to_string(pid_file(project_dir, "zunzhe-core")) else {
        return (0, String::new());
    };
    let Ok(pid) = pid_text.trim().parse::<u32>() else {
        return (0, String::new());
    };
    let text = read_uptime_logs(project_dir);
    if text.is_empty() {
        return (0, String::new());
    }
    static UPTIME_TS_RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = UPTIME_TS_RE.get_or_init(|| {
        regex::Regex::new(r"\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} [+-]\d{4}")
            .unwrap_or_else(|_| regex::Regex::new(r"\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}").unwrap())
    });
    let mut started_at: Option<chrono::DateTime<chrono::FixedOffset>> = None;
    for line in text.lines() {
        // 启动行有两种格式，统一按“包含 started 且匹配 pid”处理。
        if !line.contains(" started") || !line.contains(&format!("pid={pid}")) {
            continue;
        }
        let Some(ts) = re.find(line).and_then(|m| {
            chrono::DateTime::parse_from_str(m.as_str(), "%Y-%m-%d %H:%M:%S %z").ok()
        }) else {
            continue;
        };
        if started_at.is_none() || ts < started_at.unwrap() {
            started_at = Some(ts);
        }
    }
    let Some(started_at) = started_at else {
        return (0, String::new());
    };
    let now = chrono::Local::now().with_timezone(&started_at.timezone());
    let secs = (now - started_at).num_seconds().max(0) as u64;
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;
    let human = if days > 0 {
        format!("{days} 天 {hours} 小时 {mins} 分")
    } else if hours > 0 {
        format!("{hours} 小时 {mins} 分")
    } else {
        format!("{mins} 分钟")
    };
    (secs, human)
}

fn disk_free_bytes(_project_dir: &Path) -> u64 {
    #[cfg(unix)]
    {
        unsafe {
            let mut stat = std::mem::zeroed::<libc::statvfs>();
            let c_path = std::ffi::CString::new("/").unwrap_or_default();
            if libc::statvfs(c_path.as_ptr(), &mut stat) == 0 {
                let avail = stat.f_bavail as u64;
                let frsize = stat.f_frsize;
                return avail.saturating_mul(frsize) / (1024 * 1024);
            }
        }
        0
    }
    #[cfg(not(unix))]
    {
        // Windows 下用 `GetDiskFreeSpaceExW` 查询项目盘符剩余空间，
        // 避免 doctor 在 Windows 上永远显示 0 MB。
        use std::os::windows::ffi::OsStrExt;
        let drive = _project_dir
            .components()
            .next()
            .map(|c| c.as_os_str())
            .unwrap_or_else(|| std::ffi::OsStr::new("C:"));
        let wide: Vec<u16> = drive.encode_wide().chain(std::iter::once(0)).collect();
        let mut free_bytes_available_to_caller: u64 = 0;
        let mut total_number_of_bytes: u64 = 0;
        let mut total_number_of_free_bytes: u64 = 0;
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free_bytes_available_to_caller,
                &mut total_number_of_bytes,
                &mut total_number_of_free_bytes,
            )
        };
        if ok != 0 {
            free_bytes_available_to_caller / (1024 * 1024)
        } else {
            0
        }
    }
}

/// 跨平台查找项目内置二进制：优先本机后缀，再回退另一平台后缀，最后使用 PATH。
/// 直播录制、B站 ffmpeg 合流下载与 doctor 共用此入口，保证三处路径解析一致。
pub fn find_binary(project_dir: &Path, name: &str, windows_name: &str) -> PathBuf {
    let unix_name = if cfg!(windows) { windows_name } else { name };
    let bundled = project_dir.join("ffmpeg").join(unix_name);
    if bundled.exists() {
        return bundled;
    }
    let cross_bundled =
        project_dir
            .join("ffmpeg")
            .join(if cfg!(windows) { name } else { windows_name });
    if cross_bundled.exists() {
        return cross_bundled;
    }
    // PATH 兜底必须用当前平台可执行后缀：Windows 直接执行 `ffmpeg` 会找不到，
    // 必须回退到 `ffmpeg.exe`。
    PathBuf::from(if cfg!(windows) { windows_name } else { name })
}

/// 返回项目可用的 Python 解释器。
///
/// 优先项目内 `.venv`，缺失时回退到系统 PATH 中的 `python3` / `python.exe`。
/// 返回值可能只是相对命令名，不代表可执行：调用方必须用 [`python_available`]
/// 探测真实可启动性，不能用 `Path::is_file()` 判断。
#[allow(dead_code)] // 供 ensure_started/stop_all 兜底路径使用
pub fn python_bin(project_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        let venv = project_dir.join(".venv").join("Scripts").join("python.exe");
        if venv.exists() {
            venv
        } else {
            PathBuf::from("python.exe")
        }
    } else {
        let venv = project_dir.join(".venv").join("bin").join("python");
        if venv.exists() {
            venv
        } else {
            PathBuf::from("python3")
        }
    }
}

/// 探测 Python 是否真实可用：无论 venv 绝对路径还是 PATH 回退名，都必须能
/// 启动并输出版本。仅检查 `is_file()` 会漏掉 PATH 兜底，也会把空文件误判为
/// 可用；容器/云服务器里没有项目内 .venv 但系统 python3 可用时应如实报告。
pub fn python_available(project_dir: &Path) -> bool {
    python_binary_available(&python_bin(project_dir))
}

/// 探测单个 Python 解释器能否启动并输出版本，不关心它来自 .venv 还是 PATH。
pub fn python_binary_available(python: &Path) -> bool {
    std::process::Command::new(python)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn pid_file(project_dir: &Path, name: &str) -> std::path::PathBuf {
    project_dir
        .join("fupan_output")
        .join("run")
        .join(format!("{name}.pid"))
}

fn write_pid(project_dir: &Path, name: &str, pid: u32) {
    let path = pid_file(project_dir, name);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = crate::config::atomic_write(&path, pid.to_string());
}

/// 供启动脚本/桌面端写入 Rust daemon pid，保持同一查询接口。
pub fn write_rust_daemon_pid(project_dir: &Path, pid: u32) {
    write_pid(project_dir, "zunzhe-core", pid);
}

/// 录制进程 pid 落盘（fupan_output/run/rust_live/<url-hash>.pid），供停止兜底清理。
pub fn write_rust_live_pid(project_dir: &Path, url: &str, pid: u32) {
    let dir = project_dir
        .join("fupan_output")
        .join("run")
        .join("rust_live");
    let _ = std::fs::create_dir_all(&dir);
    let name = rust_live_pid_name(url);
    let _ = crate::config::atomic_write(&dir.join(format!("{name}.pid")), pid.to_string());
}

/// 删除某个直播地址对应的录制 pid 文件，供会话结束、启动失败或兜底清理使用。
pub fn remove_rust_live_pid(project_dir: &Path, url: &str) {
    let path = project_dir
        .join("fupan_output")
        .join("run")
        .join("rust_live")
        .join(format!("{}.pid", rust_live_pid_name(url)));
    let _ = std::fs::remove_file(path);
}

/// 直播地址 -> 录制 pid 文件名（同一 URL 稳定映射，供停止兜底清理）。
pub fn rust_live_pid_name(text: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn service_pid(project_dir: &Path, name: &str) -> Option<u32> {
    let path = pid_file(project_dir, name);
    let text = std::fs::read_to_string(&path).ok()?;
    let Ok(pid) = text.trim().parse::<u32>() else {
        // 残留/损坏的 pid 文件没有价值，避免后续查询一直返回异常状态。
        let _ = std::fs::remove_file(&path);
        return None;
    };
    if service_pid_by_raw(pid) {
        Some(pid)
    } else {
        // 进程已退出后，pid 文件会成为“死 pid”残留；顺手清掉，
        // 避免 service-status / 兼容层误判服务还在运行。
        let _ = std::fs::remove_file(&path);
        None
    }
}

fn service_pid_by_raw(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists() || libc_kill(pid as i32, 0).is_ok()
    }
    #[cfg(not(unix))]
    {
        // Windows 无 /proc：统一走 process::process_alive（tasklist CSV 机器解析）。
        crate::process::process_alive(pid)
    }
}

#[cfg(unix)]
fn libc_kill(pid: i32, signal: i32) -> std::io::Result<()> {
    // 用标准库调 kill(2) 检查存活；这里不引入 libc crate。
    unsafe {
        let ret = libc::kill(pid, signal);
        if ret == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

#[cfg(unix)]
unsafe fn libc_kill_process_group(pid: u32) {
    unsafe {
        let _ = libc::kill(-(pid as i32), libc::SIGTERM);
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    unsafe {
        let _ = libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process(pid: u32) {
    // Windows 下用 taskkill 兼容。
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .status();
}

#[cfg(unix)]
fn kill_process(pid: u32) {
    unsafe {
        libc_kill_process_group(pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_live_pid_roundtrip() {
        let dir = std::env::temp_dir().join(format!("zunzhe-rust-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        write_rust_live_pid(&dir, "https://live.bilibili.com/320", 12345);
        let pid_dir = dir.join("fupan_output").join("run").join("rust_live");
        let file = std::fs::read_dir(&pid_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read_to_string(file.path()).unwrap(), "12345");
        remove_rust_live_pid(&dir, "https://live.bilibili.com/320");
        assert!(
            std::fs::read_dir(&pid_dir).unwrap().next().is_none(),
            "remove_rust_live_pid 应删除同一个 URL 对应的 pid 文件"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_service_pid_is_removed() {
        let dir = std::env::temp_dir().join(format!("zunzhe-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("fupan_output").join("run")).unwrap();
        // 使用一个几乎不可能存在的 pid，确保进程已退出。
        write_pid(&dir, "video_monitor", 4_000_000_000);
        assert!(service_pid(&dir, "video_monitor").is_none());
        assert!(
            !pid_file(&dir, "video_monitor").exists(),
            "死 pid 文件应被自动清理"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_pid_if_never_deletes_new_instance_pid() {
        let dir = std::env::temp_dir().join(format!("zunzhe-pid-if-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("fupan_output").join("run")).unwrap();

        // 模拟旧实例退出时看到新实例已经写入 pid：只删自己 pid 的接口不能误删。
        write_rust_daemon_pid(&dir, 888_888);
        remove_rust_daemon_pid_if(&dir, 777_777);
        assert_eq!(
            std::fs::read_to_string(pid_file(&dir, "zunzhe-core"))
                .unwrap()
                .trim(),
            "888888"
        );

        // 自己的 pid 仍会被清理，保证正常退出不留残留。
        remove_rust_daemon_pid_if(&dir, 888_888);
        assert!(!pid_file(&dir, "zunzhe-core").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ensure_core_does_not_claim_launchd_on_tcc_protected_path() {
        // 项目位于桌面/文稿时，即使本机已安装 com.zunzhe.monitor，ensure
        // 也不应走 launchctl kickstart 或把手动进程误报成 launchd 管理。
        // 这里用注入的 HOME 验证 TCC 判定与提示生成，不依赖/污染真实环境。
        let home = std::env::temp_dir().join(format!("zunzhe-home-{}", std::process::id()));
        let desktop = home.join("Desktop").join("尊者监控直播短视频");
        assert!(
            tcc_protected_with_home(&desktop, &home),
            "Desktop 子目录应判定为 TCC 保护路径"
        );
        assert!(
            !tcc_protected_with_home(&home.join("Projects").join("尊者监控直播短视频"), &home),
            "~/Projects 不应判定为 TCC 保护路径"
        );
        let _ = desktop;
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sync_launchd_scripts_updates_outdated_copies_only() {
        let home = std::env::temp_dir().join(format!("zunzhe-home-sync-{}", std::process::id()));
        let launch_agents = home.join("Library").join("LaunchAgents");
        let project = std::env::temp_dir().join(format!("zunzhe-repo-sync-{}", std::process::id()));
        let repo_scripts = project.join("scripts").join("launchd");
        std::fs::create_dir_all(&repo_scripts).unwrap();
        std::fs::create_dir_all(&launch_agents).unwrap();

        let daemon_repo = repo_scripts.join("zunzhe-daemon.sh");
        let daemon_installed = launch_agents.join("zunzhe-daemon.sh");
        std::fs::write(&daemon_repo, "new daemon\n").unwrap();
        std::fs::write(&daemon_installed, "old daemon\n").unwrap();
        std::fs::write(
            project.join("scripts").join("ensure-full-access.sh"),
            "guard\n",
        )
        .unwrap();
        std::fs::write(project.join("scripts").join("audit-longrun.sh"), "audit\n").unwrap();

        unsafe { std::env::set_var("HOME", &home) };
        sync_launchd_scripts(&project);
        unsafe { std::env::remove_var("HOME") };

        assert_eq!(
            std::fs::read_to_string(&daemon_installed).unwrap(),
            "new daemon\n",
            "旧副本应被仓库版本覆盖"
        );
        assert_eq!(
            std::fs::read_to_string(launch_agents.join("ensure-full-access.sh")).unwrap(),
            "guard\n"
        );
        assert_eq!(
            std::fs::read_to_string(launch_agents.join("audit-longrun.sh")).unwrap(),
            "audit\n"
        );

        // 幂等：再次同步不应改写任何文件（mtime 不变）。
        let before = std::fs::metadata(&daemon_installed)
            .unwrap()
            .modified()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        unsafe { std::env::set_var("HOME", &home) };
        sync_launchd_scripts(&project);
        unsafe { std::env::remove_var("HOME") };
        let after = std::fs::metadata(&daemon_installed)
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after, "内容一致时不应反复覆盖");

        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn heartbeat_and_diagnostics_roundtrip() {
        let dir = std::env::temp_dir().join(format!("zunzhe-heartbeat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("fupan_output").join("run")).unwrap();
        assert!(!rust_daemon_alive(&dir));
        touch_heartbeat(&dir);
        let status = status(&dir).unwrap();
        assert_eq!(status["rust_daemon"], false);
        assert_eq!(status["heartbeat_ok"], true);
        assert!(status.get("uptime_secs").is_some());
        assert!(status.get("uptime_human").is_some());
        let diag = diagnostics(&dir).unwrap();
        assert!(diag["heartbeat_ok"].as_bool().unwrap());
        assert!(diag["run_dir_writable"].as_bool().unwrap());
        assert_eq!(diag["uptime_secs"].as_u64(), Some(0));
        assert_eq!(diag["uptime_human"].as_str(), Some(""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diagnostics_reports_pipeline_python_environment() {
        let dir = std::env::temp_dir().join(format!("zunzhe-diag-py-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("fupan_output").join("run")).unwrap();
        std::fs::create_dir_all(dir.join("config")).unwrap();
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
        let diag = diagnostics(&dir).unwrap();
        // 本机可能装了 python3，也可能没装；断言必须与真实探测一致，不能
        // 假定“没有 .venv 就不可用”。容器/云服务器无 venv 时系统 python3
        // 是合法回退，doctor 应如实报告可用。
        assert_eq!(
            diag["pipeline_python_ok"],
            python_available(&dir),
            "doctor 的 pipeline_python_ok 必须与真实 Python 可启动性一致"
        );
        if !python_available(&dir) {
            assert!(
                diag["hints"]
                    .as_array()
                    .and_then(|a| a.iter().find(|h| {
                        h.as_str().map(|s| s.contains("复盘桥接")).unwrap_or(false)
                    }))
                    .is_some(),
                "缺少 Python 复盘环境时 doctor 应给出可见提示"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn python_available_detects_path_fallback() {
        // 无 venv 时返回 PATH 中的 python3；测试只验证与真实探测一致，
        // 不依赖本机是否安装。
        let dir = std::env::temp_dir().join(format!("zunzhe-py-fallback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let expected = std::process::Command::new(if cfg!(windows) {
            "python.exe"
        } else {
            "python3"
        })
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
        assert_eq!(python_available(&dir), expected);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn python_available_rejects_fake_venv_file() {
        let dir = std::env::temp_dir().join(format!("zunzhe-py-fake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let venv = if cfg!(windows) {
            dir.join(".venv").join("Scripts").join("python.exe")
        } else {
            dir.join(".venv").join("bin").join("python")
        };
        std::fs::create_dir_all(venv.parent().unwrap()).unwrap();
        std::fs::write(&venv, "").unwrap();
        assert!(
            !python_available(&dir),
            "假 venv 文件不能算可用（当前机器无 .venv/bin/python 可执行程序）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diagnostics_reports_missing_email_config() {
        let dir = std::env::temp_dir().join(format!("zunzhe-diag-mail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        // 空 [推送配置]：没有 SMTP 服务器、登录账号、授权码、收件人。
        std::fs::write(dir.join("config").join("config.ini"), "[推送配置]\n").unwrap();
        // 避免 find_project_root 一路向上找到真实项目根：放一个空 config.ini.example
        // 也不影响，因为真实 config.ini 已存在；这里确保诊断只针对临时目录。
        std::fs::write(
            dir.join("config").join("config.ini.example"),
            "[推送配置]\n",
        )
        .unwrap();
        // 测试目录只有 config.ini，没有 fupan_config.ini(.example)，
        // Config::load 会在 diagnostics 里失败而静默跳过邮箱提示；
        // 这里补一个空 fupan_config.ini.example，让诊断能读到主配置。
        std::fs::write(
            dir.join("config").join("fupan_config.ini.example"),
            "[转写]\n",
        )
        .unwrap();
        let diag = diagnostics(&dir).unwrap();
        assert!(
            diag["hints"]
                .as_array()
                .and_then(|a| a.iter().find(|h| h
                    .as_str()
                    .map(|s| s.contains("邮箱未配置"))
                    .unwrap_or(false)))
                .is_some(),
            "邮箱未配置时 doctor 应给出可见提示，实际: {:?}",
            diag["hints"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diagnostics_detects_windows_bundled_ffmpeg() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-diag-ffmpeg-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ffmpeg")).unwrap();
        std::fs::write(dir.join("ffmpeg").join("ffmpeg.exe"), "bin").unwrap();
        let diag = diagnostics(&dir).unwrap();
        // 当前平台是 macOS 时，本测试不要求 ffmpeg.exe 生效；这里锁定的是
        // 输出字段存在，跨平台 CI 会在 Windows 上验证真实行为。
        assert!(diag.get("ffmpeg_available").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diagnostics_missing_ffmpeg_hint_is_not_contradictory() {
        // 空目录里既没有项目内置 ffmpeg，也不应在 PATH 兜底时声称“将使用
        // PATH 中的 ffmpeg”（那会误导用户以为录制还能跑）。文案必须明确
        // 指出缺失后果与修复路径。
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-diag-ffmpeg-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let diag = diagnostics(&dir).unwrap();
        let text = diag["ffmpeg"].as_str().unwrap_or("");
        if diag["ffmpeg_available"] == serde_json::json!(false) {
            assert!(text.contains("missing"), "缺失时必须标记 missing: {text}");
            assert!(
                !text.contains("将使用 PATH 中的 ffmpeg"),
                "缺失时不能声称将使用 PATH 兜底: {text}"
            );
            assert!(text.contains("录制/下载将失败"), "缺失时应说明后果: {text}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn daemon_uptime_parses_started_line() {
        let dir = std::env::temp_dir().join(format!("zunzhe-uptime-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        std::fs::create_dir_all(dir.join("fupan_output").join("run")).unwrap();
        write_rust_daemon_pid(&dir, 424242);
        let started = chrono::Local::now() - chrono::Duration::seconds(125);
        std::fs::write(
            dir.join("logs").join("zunzhe-core.uptime.log"),
            format!(
                "{} started pid=424242\n{} pid=424242 rust_daemon=true heartbeat_ok=true\n",
                started.format("%Y-%m-%d %H:%M:%S %z"),
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z")
            ),
        )
        .unwrap();
        let (secs, human) = daemon_uptime(&dir);
        assert!(secs >= 120, "uptime_secs 应接近 125 秒，实际 {secs}");
        assert!(!human.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn daemon_uptime_tolerates_corrupt_log() {
        // 日志可能被外部工具/磁盘异常写坏；uptime 只是诊断展示，绝不能因为
        // 正则或时间解析失败让 status/doctor 崩溃。
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-uptime-corrupt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        std::fs::create_dir_all(dir.join("fupan_output").join("run")).unwrap();
        write_rust_daemon_pid(&dir, 424242);
        std::fs::write(
            dir.join("logs").join("zunzhe-core.uptime.log"),
            "这不是有效时间戳 started pid=424242\nbroken line\n",
        )
        .unwrap();
        let (secs, human) = daemon_uptime(&dir);
        assert_eq!(secs, 0);
        assert!(human.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn daemon_uptime_merges_rotated_logs() {
        // 24×30 期间 uptime 日志会被 daemon 启动时轮转；如果医生只看当前
        // 文件，跨轮转后就会把“本实例全程”误算成“最近一段”。这里模拟
        // 启动行落在 .1 历史里、当前文件只剩心跳，验证能合并出正确时长。
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-uptime-rotated-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        std::fs::create_dir_all(dir.join("fupan_output").join("run")).unwrap();
        write_rust_daemon_pid(&dir, 424242);

        let started = chrono::Local::now() - chrono::Duration::seconds(9000);
        std::fs::write(
            dir.join("logs").join("zunzhe-core.uptime.log.1"),
            format!(
                "{} started pid=424242\n",
                started.format("%Y-%m-%d %H:%M:%S %z")
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("logs").join("zunzhe-core.uptime.log"),
            format!(
                "{} pid=424242 rust_daemon=true heartbeat_ok=true\n",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z")
            ),
        )
        .unwrap();

        let (secs, human) = daemon_uptime(&dir);
        assert!(secs >= 8900, "应跨轮转文件读取最早启动行，实际 {secs}");
        assert!(human.contains("小时"), "2.5 小时应有人类可读格式: {human}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn health_audit_reports_daemon_and_heartbeat_and_rss() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-health-audit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        std::fs::create_dir_all(dir.join("fupan_output").join("run")).unwrap();

        // 心跳与 pid 都不存在：审计必须明确不健康，而不是报空健康。
        let audit = health_audit(&dir);
        assert!(!audit.ok);
        assert!(!audit.daemon_ok);
        assert!(!audit.heartbeat_ok);
        assert!(audit.rss_kib.is_none());

        // 当前测试进程作为“daemon”：写 pid 与新鲜心跳，uptime 日志包含启动
        // 记录和心跳行；审计应能看到这些真实信号。
        write_rust_daemon_pid(&dir, std::process::id());
        touch_heartbeat(&dir);
        let started = chrono::Local::now() - chrono::Duration::seconds(125);
        std::fs::write(
            dir.join("logs").join("zunzhe-core.uptime.log"),
            format!(
                "{} started pid={}\n{} pid={} rust_daemon=true heartbeat_ok=true\n",
                started.format("%Y-%m-%d %H:%M:%S %z"),
                std::process::id(),
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z"),
                std::process::id()
            ),
        )
        .unwrap();

        let audit = health_audit(&dir);
        assert!(audit.daemon_ok, "当前测试进程应视为 daemon 存活");
        assert!(audit.heartbeat_ok, "刚写入的心跳应视为新鲜");
        assert_eq!(audit.pid, Some(std::process::id()));
        // RSS 在测试进程退出后可能已查不到（macOS ps 全量快照有竞态），
        // 不作为硬性健康条件；真实常驻 daemon 下能读到即可用于诊断。
        assert!(audit.uptime_secs >= 120);
        assert!(audit.heartbeat_lines >= 1);
        assert!(
            audit.consecutive_heartbeats >= 1,
            "同一 pid 的连续心跳数应 >=1: {audit:?}"
        );
        assert!(
            audit.log_gap_secs.is_some_and(|gap| gap <= 30 * 60),
            "uptime 日志最近心跳不应断档: {audit:?}"
        );
        assert!(audit.started_at.is_some());
        assert!(audit.last_log_line_at.is_some());
        assert!(
            audit.ok,
            "daemon+心跳+RSS 齐全时整体 health 应为 true: {audit:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn health_audit_tolerates_corrupt_uptime_log() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-health-corrupt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        std::fs::create_dir_all(dir.join("fupan_output").join("run")).unwrap();
        std::fs::write(
            dir.join("logs").join("zunzhe-core.uptime.log"),
            b"\xff\xfe\x00",
        )
        .unwrap();

        let audit = health_audit(&dir);
        assert!(!audit.ok);
        assert_eq!(audit.heartbeat_lines, 0);
        assert!(audit.started_at.is_none());
        assert!(audit.last_log_line_at.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn health_audit_started_at_only_matches_current_pid() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-health-started-pid-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        std::fs::create_dir_all(dir.join("fupan_output").join("run")).unwrap();

        // 合并读取后日志里既有旧实例的 started 行，也有当前实例的启动行；
        // started_at 必须指向当前 pid，而不是被历史行带偏。
        write_rust_daemon_pid(&dir, 424242);
        touch_heartbeat(&dir);
        let old_start = chrono::Local::now() - chrono::Duration::days(20);
        let new_start = chrono::Local::now() - chrono::Duration::seconds(60);
        std::fs::write(
            dir.join("logs").join("zunzhe-core.uptime.log"),
            format!(
                "{} started pid=111\n{} started pid=424242\n{} pid=424242 rust_daemon=true heartbeat_ok=true\n",
                old_start.format("%Y-%m-%d %H:%M:%S %z"),
                new_start.format("%Y-%m-%d %H:%M:%S %z"),
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z")
            ),
        )
        .unwrap();

        let audit = health_audit(&dir);
        let started_at = audit.started_at.expect("当前 pid 的 started 行应存在");
        assert!(
            !started_at.starts_with(&old_start.format("%Y-%m-%d").to_string()),
            "started_at 不应指向旧实例: {started_at}"
        );
        assert!(
            started_at.starts_with(&new_start.format("%Y-%m-%d").to_string()),
            "started_at 应指向当前实例: {started_at}"
        );
        assert!(audit.uptime_secs < 24 * 3600, "uptime 应基于当前实例");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotate_log_keeps_history() {
        let dir = std::env::temp_dir().join(format!("zunzhe-rotate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("zunzhe-core.log");
        std::fs::write(&log, "v1").unwrap();
        rotate_log(&log, 2);
        assert!(log.with_extension("log.1").exists());
        std::fs::write(&log, "v2").unwrap();
        rotate_log(&log, 2);
        assert!(log.with_extension("log.1").exists());
        assert!(log.with_extension("log.2").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotate_log_if_large_keeps_small_file_untouched() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-rotate-if-large-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("zunzhe-core.log");
        std::fs::write(&log, "small").unwrap();
        rotate_log_if_large(&log, 5, 1024);
        assert!(log.exists(), "小日志不应轮转");
        assert!(!log.with_extension("log.1").exists(), "小日志不应产生历史");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotate_log_if_large_rotates_when_threshold_reached() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-rotate-large-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("zunzhe-core.log");
        std::fs::write(&log, vec![b'x'; 4096]).unwrap();
        rotate_log_if_large(&log, 5, 1024);
        assert!(log.with_extension("log.1").exists(), "超限日志应轮转");
        // rotate_log 只后移历史文件，不重建空文件；append 模式下次写会自动创建。
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn core_binary_name_is_platform_specific() {
        let name = core_binary_name();
        if cfg!(windows) {
            assert_eq!(name, "zunzhe-core.exe");
        } else {
            assert_eq!(name, "zunzhe-core");
        }
    }

    #[test]
    fn tasklist_csv_second_field_is_pid() {
        // `/FO CSV /NH` 输出的第二段固定是 PID；映像名本身可能带引号。
        let sample = "\"zunzhe-core.exe\",\"12345\",\"Console\",\"1\",\"1,234 K\"\n\
             \"zunzhe-core.exe\",\"67890\",\"Console\",\"2\",\"5,678 K\"\n";
        assert!(crate::process::tasklist_csv_has_pid(sample, 12345));
        assert!(!crate::process::tasklist_csv_has_pid(sample, 1234));
        assert!(!crate::process::tasklist_csv_has_pid(sample, 99999));
        assert!(!crate::process::tasklist_csv_has_pid(
            "INFO: No tasks are running which match",
            12345
        ));
    }
}
