use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use fs2::FileExt;
use tokio::process::Command;

use crate::process::{ChildError, ManagedChild};

/// Electron 进程内互斥：避免 daemon 并发触发多个 Electron 实例。
///
/// 实测 daemon 的直播/视频检查并行时会同时拉起多个 Electron，固定临时 userData
/// 或 Chromium profile 竞争会导致页面脚本未执行就退出；单线程 CLI 则稳定成功。
static ELECTRON_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Electron 隐藏浏览器内核执行脚本后的结果文件。
#[derive(Debug)]
pub struct ElectronResult {
    pub value: serde_json::Value,
    pub status: String,
}

/// 用项目内置 Electron 执行 B站页面脚本，绕开常规 HTTP 请求容易触发的 412。
///
/// 所有状态都通过独立 JSON 文件传递，不依赖 Electron 的 stdout，因为 Electron
/// 主进程会把大量 GPU/Chromium 日志写到 stderr，stdout 也不是稳定的协议。
pub async fn run_bilibili_script(
    project_dir: &Path,
    script: &str,
    env: &[(&str, String)],
    timeout: Duration,
) -> Result<ElectronResult> {
    // 整个 Electron 生命周期（含重试）持锁，避免并发实例互相干扰。
    let _guard = ELECTRON_LOCK.lock().await;
    // 进程内锁只能挡住同一 daemon 的并发轮次；CLI、桌面端和 daemon 是不同进程，
    // 必须再用跨进程文件锁串行化，否则多个 Chromium 实例会互相挤掉 userData/profile。
    let _cross_process_lock = acquire_cross_process_lock(project_dir, timeout).await?;
    let run_dir = project_dir.join("fupan_output").join("run");
    std::fs::create_dir_all(&run_dir)
        .with_context(|| format!("创建运行目录失败: {}", run_dir.display()))?;
    // 旧版本 Electron 脚本失败/中断时会留下 payload/stderr 文件；长年 24×30
    // 运行只累积不回收。这里在每次执行前清理超过 7 天的协议临时文件，既保留
    // 最近失败的诊断现场，也避免运行目录无限增长。
    cleanup_stale_script_files(&run_dir);
    let electron = find_electron(project_dir)
        .ok_or_else(|| anyhow!("未找到项目内置 Electron，请先启动桌面程序安装依赖"))?;
    let script = script.trim();
    if script.is_empty() {
        anyhow::bail!("B站 Electron 脚本为空");
    }

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let runner = project_dir
        .join("desktop")
        .join("electron")
        .join("bili-video-list.cjs");
    if !runner.exists() {
        anyhow::bail!("缺少 B站 Electron 脚本执行器: {}", runner.display());
    }
    // 独立 Electron 进程默认使用全新的临时 userData，必须显式注入已授权 Cookie，
    // 否则桌面端“B站授权”只在主窗口会话生效，列表/直链提取仍然匿名 412。
    let cookie_files = crate::bilibili_auth::electron_cookie_files(project_dir);
    let cookie_env: Vec<(&str, &std::path::Path)> = cookie_files
        .iter()
        .enumerate()
        .map(|(i, p)| {
            if i == 0 {
                ("FUPAN_COOKIE_FILE", p.as_path())
            } else {
                ("FUPAN_USER_COOKIE_FILE", p.as_path())
            }
        })
        .collect();

    // Electron 页面脚本偶尔会因页面加载/GPU 上下文失败而退出；失败时重试一次，
    // 避免真实监控轮次被偶发页面异常打断。重试仍失败则返回带状态详情的错误。
    let mut last_detail = String::new();
    let mut success_status_path: Option<PathBuf> = None;
    let mut success_payload_path: Option<PathBuf> = None;
    for attempt in 0..2 {
        let attempt_nonce = nonce + attempt as u128;
        let payload_path = run_dir.join(format!("bili_script_payload_{attempt_nonce}.js"));
        let status_path = run_dir.join(format!("bili_script_status_{attempt_nonce}.json"));
        crate::config::atomic_write(&payload_path, script)
            .with_context(|| format!("写入 B站脚本失败: {}", payload_path.display()))?;
        let mut cmd = Command::new(&electron);
        cmd.kill_on_drop(true);
        let stderr_path = run_dir.join(format!("bili_script_stderr_{attempt_nonce}.log"));
        let stderr_file = std::fs::File::create(&stderr_path)?;
        let stderr_handle = std::process::Stdio::from(stderr_file);
        cmd.arg(&runner)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(stderr_handle);
        for (key, value) in env {
            cmd.env(key, value);
        }
        cmd.env("FUPAN_SCRIPT_FILE", &payload_path)
            .env("FUPAN_STATUS_FILE", &status_path);
        for (key, value) in &cookie_env {
            cmd.env(key, value);
        }
        let child = ManagedChild::spawn(&mut cmd)
            .with_context(|| format!("启动 Electron 脚本失败: {}", electron.display()))?;
        match child.wait(timeout).await {
            Ok(status) if status.success() => {
                success_status_path = Some(status_path);
                success_payload_path = Some(payload_path);
                let _ = std::fs::remove_file(&stderr_path);
                break;
            }
            Ok(status) => {
                let detail = std::fs::read_to_string(&status_path).unwrap_or_default();
                last_detail = detail.chars().take(500).collect::<String>();
                tracing::warn!("Electron 脚本状态文件内容: {last_detail:?}");
                let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
                tracing::warn!(
                    "Electron 脚本 stderr 尾部: {}",
                    stderr
                        .chars()
                        .rev()
                        .take(500)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect::<String>()
                );
                let _ = std::fs::remove_file(&payload_path);
                let _ = std::fs::remove_file(&status_path);
                // stderr 保留在 run 目录，供下一次失败时定位页面/渲染器错误；
                // 成功后才会删除。
                if attempt == 1 {
                    if last_detail.trim().is_empty() {
                        anyhow::bail!("Electron 脚本退出码 {}", status);
                    } else {
                        anyhow::bail!("Electron 脚本退出码 {}: {}", status, last_detail);
                    }
                }
            }
            Err(ChildError::Timeout) => {
                let _ = std::fs::remove_file(&payload_path);
                let _ = std::fs::remove_file(&status_path);
                if attempt == 1 {
                    anyhow::bail!("Electron 脚本超时（{} 秒）", timeout.as_secs());
                }
            }
            Err(ChildError::Io(e)) => {
                let _ = std::fs::remove_file(&payload_path);
                let _ = std::fs::remove_file(&status_path);
                return Err(e).context("等待 Electron 脚本失败");
            }
        }
        tracing::warn!(
            "Electron 脚本第 {} 次尝试失败，重试一次: {last_detail}",
            attempt + 1
        );
    }

    let Some(status_path) = success_status_path else {
        anyhow::bail!("Electron 脚本未产生成功状态文件: {last_detail}");
    };
    if let Some(payload_path) = success_payload_path {
        let _ = std::fs::remove_file(&payload_path);
    }
    let status = std::fs::read_to_string(&status_path).unwrap_or_default();
    let value = std::fs::read_to_string(&status_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(serde_json::Value::Null);
    let _ = std::fs::remove_file(&status_path);
    Ok(ElectronResult { value, status })
}

/// 跨进程独占锁文件，固定放在运行目录，供 daemon/CLI/桌面端共享。
fn electron_lock_file(project_dir: &Path) -> PathBuf {
    project_dir
        .join("fupan_output")
        .join("run")
        .join("bilibili_electron.lock")
}

/// 等待获取 B站 Electron 跨进程独占锁。
///
/// 锁文件只负责互斥，不写入内容，因此永远不会被误当成状态文件；
/// 进程退出或句柄 drop 时内核自动释放锁。
async fn acquire_cross_process_lock(
    project_dir: &Path,
    timeout: Duration,
) -> Result<std::fs::File> {
    let lock_path = electron_lock_file(project_dir);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建运行目录失败: {}", parent.display()))?;
    }
    let wait_timeout = timeout + Duration::from_secs(30);
    let lock_path_clone = lock_path.clone();
    let handle = tokio::task::spawn_blocking(move || -> Result<std::fs::File> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path_clone)
            .with_context(|| format!("打开 Electron 锁文件失败: {}", lock_path_clone.display()))?;
        file.lock_exclusive()?;
        Ok(file)
    });
    match tokio::time::timeout(wait_timeout, handle).await {
        Ok(Ok(Ok(file))) => Ok(file),
        Ok(Ok(Err(e))) => {
            Err(e).with_context(|| format!("打开 Electron 锁文件失败: {}", lock_path.display()))
        }
        Ok(Err(e)) => {
            Err(e).with_context(|| format!("获取 Electron 锁失败: {}", lock_path.display()))
        }
        Err(_) => Err(anyhow!(
            "等待 B站 Electron 跨进程锁超时（{} 秒）: {}",
            wait_timeout.as_secs(),
            lock_path.display()
        )),
    }
}

/// 清理 B站 Electron 协议临时文件：超过 7 天的直接删，每类前缀只保留最近
/// 10 个（诊断现场）。文件名由 `bili_script_payload_/status_/stderr_` 前缀 +
/// 纳秒时间戳组成；清理只按文件名前缀和修改时间，不删除其它运行状态文件。
///
/// 调度轮次不跑 Electron（冷启动会撞 50 秒护栏），但旧版本/手动诊断可能留下
/// payload/stderr 残留；daemon 周期维护时也要调用，避免 run 目录无限增长。
pub(crate) fn cleanup_stale_script_files(run_dir: &Path) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let stale_prefixes = [
        "bili_script_payload_",
        "bili_script_status_",
        "bili_script_stderr_",
    ];
    let Ok(entries) = std::fs::read_dir(run_dir) else {
        return;
    };
    let files: Vec<(PathBuf, i64)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            if !stale_prefixes.iter().any(|p| name.starts_with(p)) {
                return None;
            }
            let modified = entry
                .metadata()
                .ok()?
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            Some((entry.path(), modified))
        })
        .collect();
    // 每类保留最近的 10 个：旧版本异常退出可能残留 payload/stderr，且时间戳
    // 很新，只按年龄清理无法阻止长期积累。按“前缀”分组后保留最新 N 个。
    for prefix in stale_prefixes {
        let mut group: Vec<(PathBuf, i64)> = files
            .iter()
            .filter(|(p, _)| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with(prefix))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        group.sort_by_key(|(_, modified)| *modified);
        group.reverse();
        for (idx, (path, modified)) in group.into_iter().enumerate() {
            if idx >= 10 || now.saturating_sub(modified) >= 7 * 24 * 60 * 60 {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// 运行目录统一维护：清理 Electron 协议残留和原子写崩溃残留的临时文件。
///
/// CLI/MCP 在 daemon 未接管时也会低频维护，不能只收敛数据库而让
/// `fupan_output/run` 无限积累旧版本留下的诊断文件。
pub(crate) fn maintain_run_dir(run_dir: &Path) {
    cleanup_stale_script_files(run_dir);
    // daemon 换实例/崩溃后，旧实例会把当前 pid/heartbeat 重命名成
    // `*.stale-<unix_ts>` 留给诊断；这些文件已无运行价值，长期运行会随重启
    // 次数无限积累，必须在周期维护里一并清掉。正式 pid/heartbeat 不匹配
    // `*.stale-` 模式，不会被误删。
    cleanup_daemon_state_stale_files(run_dir);
    let Ok(entries) = std::fs::read_dir(run_dir) else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name = match file_name.to_str() {
            Some(name) => name,
            None => continue,
        };
        // atomic_write 的临时文件命名：`.{target}.tmp.{pid}.{nanos}.{counter}`。
        // 正常写入会在错误路径清理自身，残留只来自进程硬崩溃；只有达到“超过
        // 7 天”才删除，避免误删另一个进程正在写入的新临时文件。
        if !name.contains(".tmp.") {
            continue;
        }
        let modified = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if now.saturating_sub(modified) >= 7 * 24 * 60 * 60 {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// 清理旧实例留下的 `zunzhe-core.*.stale-*` 状态残留。
fn cleanup_daemon_state_stale_files(run_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(run_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.starts_with("zunzhe-core.") && name.contains(".stale-") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn find_electron(project_dir: &Path) -> Option<PathBuf> {
    let base = project_dir
        .join("desktop")
        .join("node_modules")
        .join("electron")
        .join("dist");
    let candidates = [
        base.join("Electron.app")
            .join("Contents")
            .join("MacOS")
            .join("Electron"),
        base.join("electron"),
        base.join("electron.exe"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_and_runner_paths_are_project_local() {
        let dir = std::env::temp_dir().join(format!("zunzhe-electron-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(run_bilibili_script(
            &dir,
            "(function(){return 1})()",
            &[],
            Duration::from_secs(1),
        ));
        // 没有 Electron/runner 时应给出明确的项目内路径错误，而不是假装成功。
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Electron") || msg.contains("bili-video-list"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn script_validation_rejects_empty_payload() {
        let dir = std::env::temp_dir().join(format!("zunzhe-electron-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(run_bilibili_script(
            &dir,
            "   ",
            &[],
            Duration::from_secs(1),
        ));
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_electron_temp_files_are_cleaned_up() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-electron-cleanup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let run_dir = dir.join("fupan_output").join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        // 一个 8 天前的旧 payload/stderr，一个刚写入的同名临时文件。
        let old_payload = run_dir.join("bili_script_payload_old.js");
        let old_stderr = run_dir.join("bili_script_stderr_old.log");
        std::fs::write(&old_payload, "old").unwrap();
        std::fs::write(&old_stderr, "old stderr").unwrap();
        set_mtime_days_ago(&old_payload, 8);
        set_mtime_days_ago(&old_stderr, 8);
        let fresh = run_dir.join("bili_script_status_fresh.json");
        std::fs::write(&fresh, "{}").unwrap();

        cleanup_stale_script_files(&run_dir);
        assert!(!old_payload.exists(), "旧 payload 应被清理");
        assert!(!old_stderr.exists(), "旧 stderr 应被清理");
        assert!(fresh.exists(), "新临时文件不应被误删");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recent_payload_cap_keeps_only_latest_ten() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-electron-cap-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let run_dir = dir.join("fupan_output").join("run");
        std::fs::create_dir_all(&run_dir).unwrap();

        // 12 个近期残留 payload，修改时间从旧到新，外加一个超龄文件。
        let mut payloads = Vec::new();
        for i in 0..12 {
            let path = run_dir.join(format!("bili_script_payload_recent_{i}.js"));
            std::fs::write(&path, i.to_string()).unwrap();
            set_mtime_seconds_ago(&path, 120 - i);
            payloads.push(path);
        }
        let old = run_dir.join("bili_script_payload_old.js");
        std::fs::write(&old, "old").unwrap();
        set_mtime_days_ago(&old, 8);

        cleanup_stale_script_files(&run_dir);

        let remaining: Vec<PathBuf> = std::fs::read_dir(&run_dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("bili_script_payload_"))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(remaining.len(), 10, "近期 payload 应只保留最新 10 个");
        // 最新 10 个保留，最旧 2 个与超龄文件被清理。
        for path in &payloads[2..] {
            assert!(
                path.exists(),
                "较新的 payload 不应被误删: {}",
                path.display()
            );
        }
        for path in &payloads[..2] {
            assert!(
                !path.exists(),
                "最旧的 payload 应被清理: {}",
                path.display()
            );
        }
        assert!(!old.exists(), "超龄 payload 应被清理");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn maintain_run_dir_removes_stale_atomic_write_tmp_only() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-run-maintain-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let run_dir = dir.join("fupan_output").join("run");
        std::fs::create_dir_all(&run_dir).unwrap();

        let stale_tmp = run_dir.join(".zunzhe-core.heartbeat.tmp.123.456.0");
        std::fs::write(&stale_tmp, "stale").unwrap();
        set_mtime_days_ago(&stale_tmp, 8);
        let fresh_tmp = run_dir.join(".zunzhe-core.heartbeat.tmp.999.888.0");
        std::fs::write(&fresh_tmp, "fresh").unwrap();
        let live_file = run_dir.join("zunzhe-core.heartbeat");
        std::fs::write(&live_file, "now").unwrap();
        let stale_heartbeat = run_dir.join("zunzhe-core.heartbeat.stale-1786712453");
        std::fs::write(&stale_heartbeat, "old").unwrap();
        let stale_pid = run_dir.join("zunzhe-core.pid.stale-1786712453");
        std::fs::write(&stale_pid, "424242").unwrap();
        let fresh_state = run_dir.join("zunzhe-core.pid");
        std::fs::write(&fresh_state, "424242").unwrap();

        maintain_run_dir(&run_dir);

        assert!(!stale_tmp.exists(), "超龄原子写残留应被清理");
        assert!(fresh_tmp.exists(), "刚写入的临时文件不应被误删");
        assert!(live_file.exists(), "正式状态文件不应被误删");
        assert!(
            !stale_heartbeat.exists(),
            "旧实例 heartbeat stale 残留应被清理"
        );
        assert!(!stale_pid.exists(), "旧实例 pid stale 残留应被清理");
        assert!(fresh_state.exists(), "正式 pid 文件不应被误删");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    fn set_mtime_days_ago(path: &Path, days: u64) {
        set_mtime_seconds_ago(path, days * 24 * 60 * 60);
    }

    #[cfg(unix)]
    fn set_mtime_seconds_ago(path: &Path, seconds: u64) {
        use std::os::unix::ffi::OsStrExt;
        let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(seconds);
        let secs = old_time
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let times = [
            libc::timespec {
                tv_sec: secs,
                tv_nsec: 0,
            },
            libc::timespec {
                tv_sec: secs,
                tv_nsec: 0,
            },
        ];
        assert_eq!(
            unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) },
            0
        );
    }
}
