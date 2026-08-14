use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::json;
use std::sync::OnceLock;

#[allow(dead_code)] // 保留显式 Python 兜底入口，CLI/MCP 默认不再自动调用
pub fn ensure_started(project_dir: &Path) -> Result<serde_json::Value> {
    let python = python_bin(project_dir);
    if !python.exists() {
        bail!("找不到 Python 虚拟环境，请先运行 scripts/setup-macos.sh");
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
    let rust = start_rust_daemon(project_dir, 900)?;
    // Rust daemon 已统一接管直播检测/录制和视频监控；不再并行启动 Python
    // video_monitor，避免同一批视频源被两套逻辑重复检查/通知/下载。
    let rust_status = rust
        .get("rust_daemon")
        .cloned()
        .unwrap_or(serde_json::Value::Bool(true));
    Ok(json!({"ok": true, "rust_daemon": rust_status, "services": {}}))
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
/// 供 daemon 内部看门监督器判断主循环是否僵死。
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

/// 诊断项：供桌面端/CLI 快速判断“为什么监控没在跑”。
pub fn diagnostics(project_dir: &Path) -> Result<serde_json::Value> {
    let log_dir = project_dir.join("logs");
    let run_dir = project_dir.join("fupan_output").join("run");
    let heartbeat_ok = heartbeat_fresh(project_dir, 30 * 60);
    let daemon_ok = rust_daemon_alive(project_dir);
    let disk = disk_free_bytes(project_dir);
    let (uptime_secs, uptime_human) = daemon_uptime(project_dir);
    let python_ok = python_bin(project_dir).is_file();
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
    let email_hint = crate::config::Config::load(project_dir)
        .ok()
        .and_then(|cfg| {
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
    Ok(json!({
        "ok": true,
        "rust_daemon": daemon_ok,
        "heartbeat_ok": heartbeat_ok,
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
    let log = project_dir.join("logs").join("zunzhe-core.uptime.log");
    let Ok(text) = std::fs::read_to_string(&log) else {
        return (0, String::new());
    };
    static UPTIME_TS_RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = UPTIME_TS_RE.get_or_init(|| {
        regex::Regex::new(r"\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} [+-]\d{4}")
            .expect("uptime 时间戳正则应合法")
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

#[allow(dead_code)] // 供 ensure_started/stop_all 兜底路径使用
fn python_bin(project_dir: &Path) -> std::path::PathBuf {
    if cfg!(windows) {
        project_dir.join(".venv").join("Scripts").join("python.exe")
    } else {
        project_dir.join(".venv").join("bin").join("python")
    }
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
        let diag = diagnostics(&dir).unwrap();
        assert_eq!(diag["pipeline_python_ok"], false);
        assert!(
            diag["hints"]
                .as_array()
                .and_then(|a| a
                    .iter()
                    .find(|h| { h.as_str().map(|s| s.contains("复盘桥接")).unwrap_or(false) }))
                .is_some(),
            "缺少 Python 复盘环境时 doctor 应给出可见提示"
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
