use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::json;

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
    Ok(json!({"ok": true, "rust_daemon": rust, "services": {}}))
}

/// 启动 Rust 核心 daemon 并写入 pid 文件（供桌面端/兼容层调用）。
pub fn start_rust_daemon(project_dir: &Path, interval_secs: u64) -> Result<serde_json::Value> {
    if rust_daemon_alive(project_dir) {
        return Ok(json!({"ok": true, "rust_daemon": "已在运行"}));
    }
    let exe = project_dir
        .join("zunzhe-core")
        .join("target")
        .join("release")
        .join("zunzhe-core");
    let debug_exe = project_dir
        .join("zunzhe-core")
        .join("target")
        .join("debug")
        .join("zunzhe-core");
    let exe = if exe.exists() { exe } else { debug_exe };
    if !exe.exists() {
        bail!("找不到 Rust 核心二进制: {}", exe.display());
    }
    let log_dir = project_dir.join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("run-daemon")
        .arg("--interval")
        .arg(interval_secs.to_string())
        .current_dir(project_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd.spawn().with_context(|| "启动 Rust daemon 失败")?;
    write_rust_daemon_pid(project_dir, child.id());
    Ok(json!({"ok": true, "rust_daemon": format!("已启动 pid={}", child.id())}))
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
    Ok(json!({"ok": true, "services": services}))
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
    service_pid_by_raw(pid)
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
    let _ = std::fs::write(path, pid.to_string());
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
    let _ = std::fs::write(dir.join(format!("{name}.pid")), pid.to_string());
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
    let pid: u32 = text.trim().parse().ok()?;
    service_pid_by_raw(pid).then_some(pid)
}

fn service_pid_by_raw(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists() || libc_kill(pid as i32, 0).is_ok()
    }
    #[cfg(not(unix))]
    {
        // Windows 无 /proc：用 tasklist 查询该 PID 是否存在。
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}")])
            .output()
            .map(|o| {
                let text = String::from_utf8_lossy(&o.stdout);
                text.contains(&pid.to_string()) && !text.contains("INFO: No tasks")
            })
            .unwrap_or(false)
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
        let file = std::fs::read_dir(dir.join("fupan_output").join("run").join("rust_live"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read_to_string(file.path()).unwrap(), "12345");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
