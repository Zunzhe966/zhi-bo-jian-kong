use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::json;

/// Python 后台服务兼容层：Rust 版本优先使用自身 daemon，
/// 但仍提供 start/stop/status 来管理旧 Python 服务（录制引擎目前仍在 Python 层）。
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

fn service_pid(project_dir: &Path, name: &str) -> Option<u32> {
    let path = pid_file(project_dir, name);
    let text = std::fs::read_to_string(&path).ok()?;
    let pid: u32 = text.trim().parse().ok()?;
    let alive =
        std::path::Path::new(&format!("/proc/{pid}")).exists() || libc_kill(pid as i32, 0).is_ok();
    alive.then_some(pid)
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
