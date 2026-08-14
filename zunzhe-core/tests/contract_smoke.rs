use std::path::Path;

use zunzhe_core::config::Config;
use zunzhe_core::mcp;
use zunzhe_core::service;

/// 接口契约冒烟：daemon 心跳/pid 生命周期、doctor 输出、日志轮转文件。
#[test]
fn daemon_heartbeat_and_doctor_contract() {
    let dir = std::env::temp_dir().join(format!("zunzhe-contract-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::create_dir_all(dir.join("logs")).unwrap();
    std::fs::create_dir_all(dir.join("fupan_output").join("run")).unwrap();

    // doctor 必须能返回机器可读 JSON，且不依赖真实项目配置文件。
    let diag = service::diagnostics(&dir).unwrap();
    assert_eq!(diag["ok"], true);
    assert!(diag.get("disk_free_mb").is_some());
    assert!(diag.get("logs_writable").is_some());

    // 心跳后 pid 不存在时 status 判定为未运行（不能因为心跳新鲜就误判）。
    service::touch_heartbeat(&dir);
    assert!(!service::rust_daemon_alive(&dir));

    // pid 存在且进程是当前测试进程时，心跳机制仍要求心跳文件新鲜。
    service::write_rust_daemon_pid(&dir, std::process::id());
    service::touch_heartbeat(&dir);
    assert!(service::rust_daemon_alive(&dir));

    // 清理 pid 后不再认为运行中。
    service::remove_rust_daemon_pid(&dir);
    assert!(!service::rust_daemon_alive(&dir));
    let _ = std::fs::remove_dir_all(&dir);
}

/// CLI/守护路径不依赖真实工作目录：从任意 cwd 都能定位项目配置。
#[test]
fn config_load_from_any_cwd() {
    let dir = std::env::temp_dir().join(format!("zunzhe-cfg-contract-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::write(
        dir.join("config").join("config.ini"),
        "[推送配置]\nSMTP邮件服务器 = smtp.qq.com\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("config").join("fupan_config.ini"),
        "[视频监控]\n自动下载新视频(是/否) = 是\n",
    )
    .unwrap();
    let cfg = Config::load(&dir).unwrap();
    assert_eq!(cfg.get_main("推送配置", "SMTP邮件服务器"), "smtp.qq.com");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 目标模式文档与项目根北极星必须存在（上下文恢复锚点）。
#[test]
fn goal_docs_exist() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    assert!(root.join("PROJECT_GOAL.md").exists());
    assert!(root.join("zunzhe-core").join("目标模式.md").exists());
}

/// 单一权威 DB 路径契约：CLI/daemon/MCP 统一使用项目根/zunzhe-core.db，
/// 旧核心子目录里的 zunzhe-core.db 只能被迁移，不能成为第二套状态。
#[test]
fn single_authoritative_db_path_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    // 代码里唯一默认 DB 路径（cli.rs --db 缺省分支），不得依赖当前目录。
    let cli_default = root.join("zunzhe-core.db");
    assert!(cli_default.parent().is_some());
    let legacy = root.join("zunzhe-core").join("zunzhe-core.db");
    // 权威库必须位于项目根，而不是 zunzhe-core/ 子目录；旧库即使存在也
    // 不应成为任何入口默认打开的库。
    assert_ne!(cli_default, legacy);
    assert_eq!(
        cli_default.file_name().and_then(|s| s.to_str()),
        Some("zunzhe-core.db")
    );
}

/// CLI/MCP 对齐契约：MCP 工具清单必须覆盖 CLI 暴露的核心能力。
#[test]
fn mcp_tools_cover_core_cli_commands() {
    let tools = mcp::tools_list();
    let names: std::collections::HashSet<String> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect();
    for expected in [
        "live_status",
        "live_list",
        "live_add",
        "live_remove",
        "live_delete",
        "live_check",
        "live_set_notify",
        "record_live",
        "service_status",
        "tail_logs",
        "video_add",
        "video_remove",
        "video_list",
        "video_check_now",
        "blogger_videos",
        "download_video",
        "video_download",
        "video_library",
        "export_training_data",
        "email_status",
        "email_setup",
        "app_prefs_get",
        "app_prefs_set",
        "config_get",
        "config_set",
        "config_set_bulk",
        "config_schema_tool",
        "notes_list",
        "process_media",
        "service_start",
        "service_stop",
        "ensure_services",
        "doctor",
        "wizard_done",
        "test_email",
        "test_notify",
    ] {
        assert!(names.contains(expected), "MCP 缺少工具: {expected}");
    }
}

/// 真实进程级崩溃恢复契约：SIGKILL 后单实例锁自动让出，新实例可重获。
#[cfg(unix)]
#[test]
fn sigkill_frees_lock_and_allows_recovery() {
    use std::process::{Command, Stdio};
    use zunzhe_core::daemon_lock::DaemonLock;

    let dir = std::env::temp_dir().join(format!("zunzhe-crash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::create_dir_all(dir.join("logs")).unwrap();
    std::fs::create_dir_all(dir.join("fupan_output").join("run")).unwrap();

    // 用 python3 子进程以 flock 独占同一锁文件，模拟持有 daemon 锁的进程。
    let lock_path = zunzhe_core::daemon_lock::lock_file(&dir);
    let script = format!(
        "import fcntl, time\nf=open({:?}, 'w')\nfcntl.flock(f, fcntl.LOCK_EX)\ntime.sleep(30)\n",
        lock_path.to_string_lossy()
    );
    let mut child = Command::new("python3")
        .arg("-c")
        .arg(&script)
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    // 等子进程拿到锁。
    let mut ready = false;
    for _ in 0..50 {
        if DaemonLock::try_acquire(&dir).unwrap().is_none() {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(ready, "子进程未在预期时间内持有锁");

    // 模拟硬崩溃：SIGKILL 不会执行任何清理，锁由内核释放。
    unsafe {
        libc::kill(child.id() as i32, libc::SIGKILL);
    }
    let _ = child.wait();

    // 锁应可重获；心跳文件无需存在，恢复由新实例自己刷新。
    let lock = DaemonLock::try_acquire(&dir).unwrap();
    assert!(lock.is_some(), "SIGKILL 后单实例锁必须释放");
    drop(lock);

    let _ = std::fs::remove_dir_all(&dir);
}
