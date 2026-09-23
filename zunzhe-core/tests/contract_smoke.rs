use std::path::Path;
use std::process::Command;

use zunzhe_core::config::Config;
use zunzhe_core::extension;
use zunzhe_core::service;

fn write_valid_wav(path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    const WAV: &[u8] = b"RIFF\x24\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x01\x00\x01\x00\x40\x1f\x00\x00\x80\x3e\x00\x00\x02\x00\x10\x00data\x00\x00\x00\x00";
    std::fs::write(path, WAV).unwrap();
}

/// 数据根契约：用户把数据放到全新目录时，必须先补齐受管目录，
/// 否则首次打开 SQLite 会直接报 “unable to open database file”。
#[test]
fn configured_data_root_is_created_before_sqlite_open() {
    let root = std::env::temp_dir().join(format!(
        "zunzhe-data-root-contract-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let products = root.join("new-products");
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(
        root.join("config").join("config.ini"),
        format!("[路径设置]\n产物根目录 = {}\n", products.display()),
    )
    .unwrap();
    std::fs::write(
        root.join("config").join("fupan_config.ini"),
        "[复盘]\n任务超时秒数 = 21600\n",
    )
    .unwrap();
    std::fs::write(
        root.join("config").join("fupan_config.ini.example"),
        "[复盘]\n任务超时秒数 = 21600\n",
    )
    .unwrap();

    let paths = zunzhe_core::paths::storage(&root);
    zunzhe_core::paths::ensure_dirs(&root).unwrap();
    let database = paths.database.clone();
    assert!(
        database.parent().unwrap().is_dir(),
        "数据库父目录必须预先存在"
    );
    assert!(products.join("logs").is_dir(), "日志目录必须预先存在");
    assert!(
        products.join("fupan_output").is_dir(),
        "复盘目录必须预先存在"
    );
    // 新布局下产物根自己就是短视频落点（{产物根}/{主播}/短视频/...），
    // 不再预建 downloads/短视频 空壳目录；下载/录制发生时按需创建。
    assert!(
        products.is_dir(),
        "产物根必须预先存在（短视频/录像都落在它下面）"
    );
    assert!(
        !products.join("downloads").exists(),
        "不再预建 downloads 空壳目录"
    );
}

#[tokio::test]
async fn legacy_database_is_copied_to_the_new_data_root_without_deleting_source() {
    let root = std::env::temp_dir().join(format!(
        "zunzhe-legacy-db-contract-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("config")).unwrap();
    let legacy = root.join("zunzhe-core.db");
    let legacy_pool = zunzhe_core::db::open_pool(&legacy).await.unwrap();
    zunzhe_core::db::enqueue_task(&legacy_pool, "process", "/tmp/legacy.mp4", "旧任务")
        .await
        .unwrap();
    legacy_pool.close().await;

    let products = root.join("new-products");
    std::fs::write(
        root.join("config").join("config.ini"),
        format!("[路径设置]\n产物根目录 = {}\n", products.display()),
    )
    .unwrap();
    zunzhe_core::paths::ensure_dirs(&root).unwrap();
    let paths = zunzhe_core::paths::storage(&root);
    let copied = zunzhe_core::paths::migrate_legacy_database_if_needed(&root, &paths.database)
        .await
        .unwrap();
    assert_eq!(copied.as_deref(), Some(legacy.as_path()));
    assert!(legacy.is_file(), "旧库只能保留，不能迁移时删除");
    assert!(paths.database.is_file(), "新数据根应获得旧库在线备份");

    let migrated_pool = zunzhe_core::db::open_pool(&paths.database).await.unwrap();
    let task_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
        .fetch_one(&migrated_pool)
        .await
        .unwrap();
    assert_eq!(task_count, 1, "迁移后的新库必须保留旧任务");
}

#[tokio::test]
async fn empty_legacy_database_does_not_block_new_schema() {
    let root = std::env::temp_dir().join(format!(
        "zunzhe-empty-legacy-db-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(root.join("zunzhe-core.db"), b"").unwrap();
    let products = root.join("new-products");
    std::fs::write(
        root.join("config").join("config.ini"),
        format!("[路径设置]\n产物根目录 = {}\n", products.display()),
    )
    .unwrap();
    zunzhe_core::paths::ensure_dirs(&root).unwrap();
    let paths = zunzhe_core::paths::storage(&root);
    let copied = zunzhe_core::paths::migrate_legacy_database_if_needed(&root, &paths.database)
        .await
        .unwrap();
    assert!(copied.is_some(), "空旧库也应完成一次迁移");
    let pool = zunzhe_core::db::open_pool(&paths.database).await.unwrap();
    let table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='tasks'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(table_count, 1, "迁移后的库必须由 open_pool 建立完整 schema");
}

/// 接口契约冒烟：daemon 心跳/pid 生命周期、doctor 输出、日志轮转文件。
#[test]
fn daemon_heartbeat_and_doctor_contract() {
    let dir = std::env::temp_dir().join(format!("zunzhe-contract-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("config")).unwrap();
    let products = dir.join("products");
    std::fs::create_dir_all(&products).unwrap();
    std::fs::write(
        dir.join("config").join("config.ini"),
        format!("[路径设置]\n产物根目录 = {}\n", products.display()),
    )
    .unwrap();
    let logs = zunzhe_core::paths::logs(&dir);
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join("zunzhe-core.log"), "stdout\n").unwrap();
    std::fs::write(logs.join("zunzhe-core.err.log"), "stderr\n").unwrap();

    // doctor 必须能返回机器可读 JSON，且不依赖真实项目配置文件。
    let diag = service::diagnostics(&dir).unwrap();
    assert_eq!(diag["ok"], true);
    assert!(diag.get("disk_free_mb").is_some());
    assert!(diag.get("logs_writable").is_some());
    // 日志可能因为旧 launchd 尚未 reload 而仍在历史目录写入；doctor 必须
    // 明确报告“界面应打开的配置目录”和“实际读到的文件”，不能只报 writable
    // 让排障人员误以为当前 daemon 日志就在配置目录里。
    assert_eq!(
        diag["log_paths"]["configured_dir"],
        zunzhe_core::paths::logs(&dir).display().to_string()
    );
    assert_eq!(diag["log_paths"]["primary"]["source"], "configured");
    assert_eq!(diag["log_paths"]["error"]["source"], "configured");
    assert_eq!(diag["log_paths"]["consistent"], true);
    // doctor 必须暴露三个复盘开关，界面/智能体无需翻配置就能确认自动复盘
    // 为何入队/跳过。
    assert_eq!(
        diag["pipeline_switches"]["transcribe"],
        serde_json::json!(true)
    );
    assert_eq!(
        diag["pipeline_switches"]["distill"],
        serde_json::json!(true)
    );
    assert_eq!(diag["pipeline_switches"]["clips"], serde_json::json!(true));
    assert!(diag["pipeline_switches"]["any_enabled"].as_bool().is_some());

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

/// 权威文档集必须存在（文档整顿后的唯一导航：产品手册 + CONTRACT + AGENTS + README）。
/// 旧 PROJECT_GOAL.md 已按产品手册第 9 章移入 .trash-作废文档/，不再是锚点。
#[test]
fn goal_docs_exist() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    for doc in ["README.md", "CONTRACT.md", "AGENTS.md", "产品手册.md"] {
        assert!(root.join(doc).exists(), "权威文档缺失: {doc}");
    }
}

/// 单一权威 DB 路径契约：默认数据库由 `[路径设置]` 解析到产物根下；
/// 项目根旧库只在升级兼容期且新库尚未建立时沿用，不能成为另一套长期状态。
#[test]
fn single_authoritative_db_path_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let default_database = zunzhe_core::paths::default_media_root()
        .join("database")
        .join("zunzhe-core.db");
    assert!(default_database.parent().is_some());
    let legacy = root.join("zunzhe-core").join("zunzhe-core.db");
    assert_ne!(default_database, legacy);
    assert_eq!(
        default_database.file_name().and_then(|s| s.to_str()),
        Some("zunzhe-core.db")
    );
}

/// CLI/MCP 对齐契约：MCP 工具清单必须覆盖 CLI 暴露的核心能力。
#[test]
fn paths_command_is_read_only_and_reports_configured_roots() {
    let root = std::env::temp_dir().join(format!(
        "zunzhe-paths-readonly-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let products = root.join("products-not-created-by-paths");
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(
        root.join("config").join("config.ini"),
        format!("[路径设置]\n产物根目录 = {}\n", products.display()),
    )
    .unwrap();
    std::fs::write(
        root.join("config").join("fupan_config.ini"),
        "[复盘]\n任务超时秒数 = 21600\n",
    )
    .unwrap();
    std::fs::write(
        root.join("config").join("fupan_config.ini.example"),
        "[复盘]\n任务超时秒数 = 21600\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_zunzhe-core"))
        .arg("paths")
        .current_dir(&root)
        .output()
        .expect("paths command should start");
    assert!(
        output.status.success(),
        "paths should exit successfully: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("paths must output JSON");
    assert_eq!(value["media_root"], products.to_string_lossy().as_ref());
    assert_eq!(
        value["downloads"],
        products.join("downloads").to_string_lossy().as_ref()
    );
    assert!(
        !products.exists(),
        "read-only paths must not create the product root: {}",
        products.display()
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn mcp_tools_cover_core_cli_commands() {
    let tools = extension::tools_list();
    let names: std::collections::HashSet<String> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect();
    for expected in [
        "anchor_search",
        "monitor_add",
        "monitor_remove",
        "monitor_list",
        "monitor_set",
        "check_now",
        "download_video",
        "process_media",
        "notes_list",
        "status",
        "paths",
        "doctor",
        "config_get",
        "config_set",
        "service_start",
        "service_stop",
        "tail_logs",
        "test_email",
    ] {
        assert!(names.contains(expected), "MCP 缺少工具: {expected}");
    }
}

/// 批量下载契约：CLI 输出必须带桌面端“下载全部/下载选中”依赖的汇总字段。
/// 桌面端不再逐条循环调用 download-video，而是解析这里的 queued/errors/
/// duplicated/skipped_downloaded，字段缺失会让批量按钮的提示和失败统计失效。
#[test]
fn batch_download_summary_contract() {
    let summary = zunzhe_core::download::BatchDownloadSummary {
        requested: 0,
        queued: 0,
        duplicated: 0,
        skipped_downloaded: 0,
        errors: 0,
        task_ids: vec![],
        error_messages: vec![],
    };
    let json = serde_json::to_value(&summary).unwrap();
    for field in [
        "requested",
        "queued",
        "duplicated",
        "skipped_downloaded",
        "errors",
        "task_ids",
        "error_messages",
    ] {
        assert!(
            json.get(field).is_some(),
            "批量下载汇总缺少桌面端依赖字段: {field}"
        );
    }
}

/// 复盘入队结果必须区分首次入队、已在队列和已经完成。
///
/// 桌面端与 MCP 客户端都依赖这组字段决定是否提示“已加入队列”；
/// 只有 task_id 时，已完成视频再次点击会被谎报成刚入队。
#[tokio::test]
async fn process_enqueue_result_distinguishes_queued_and_existing_tasks() {
    use std::path::PathBuf;
    use zunzhe_core::{db, download};

    let root = std::env::temp_dir().join(format!(
        "zunzhe-contract-process-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let media_dir = root.join("downloads").join("短视频").join("测试博主");
    std::fs::create_dir_all(&media_dir).unwrap();
    let media = media_dir.join("x.mp4");
    write_valid_wav(&media);
    let db_path: PathBuf = root.join("test.db");
    let pool = db::open_pool(&db_path).await.unwrap();
    db::init_db(&pool).await.unwrap();

    let queued = download::enqueue_process(&pool, &root, media.to_str().unwrap(), "测试博主")
        .await
        .unwrap();
    assert!(queued.queued);
    assert_eq!(queued.status, "pending");
    assert_eq!(queued.disposition, "queued");

    let active = download::enqueue_process(&pool, &root, media.to_str().unwrap(), "测试博主")
        .await
        .unwrap();
    assert!(!active.queued);
    assert_eq!(active.status, "pending");
    assert_eq!(active.disposition, "already_active");
    assert_eq!(active.task_id, queued.task_id);

    db::update_task_status(&pool, queued.task_id, "done")
        .await
        .unwrap();
    let completed = download::enqueue_process(&pool, &root, media.to_str().unwrap(), "测试博主")
        .await
        .unwrap();
    assert!(!completed.queued);
    assert_eq!(completed.status, "done");
    assert_eq!(completed.disposition, "already_done");
    assert_eq!(completed.task_id, queued.task_id);

    let _ = std::fs::remove_dir_all(&root);
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

/// 真实 stdio 冷启动契约：协议发现不能被 daemon 启动探测或数据库维护拖住。
///
/// launchd/TCC 路径上 `ensure_core` 的同步进程探测曾让每个 `tools/call`
/// 卡住数十秒；`initialize`/`tools/list` 不经过该路径，所以必须在子进程中
/// 覆盖第一个工具调用，而不是只测内存里的 `call_tool`。
#[cfg(unix)]
#[test]
fn extension_stdio_cold_start_first_tool_call_returns_promptly() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let root = std::env::temp_dir().join(format!(
        "zunzhe-contract-extension-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let media_dir = root.join("downloads").join("短视频").join("契约博主");
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::create_dir_all(&media_dir).unwrap();
    std::fs::write(
        root.join("config").join("config.ini"),
        format!(
            "[路径设置]\n产物根目录 = {}\n",
            root.join("products").display()
        ),
    )
    .unwrap();
    std::fs::write(root.join("config").join("fupan_config.ini"), "[视频监控]\n").unwrap();
    std::fs::write(
        root.join("config").join("config.ini.example"),
        "[路径设置]\n",
    )
    .unwrap();
    std::fs::write(
        root.join("config").join("fupan_config.ini.example"),
        "[视频监控]\n",
    )
    .unwrap();
    let media = media_dir.join("x.mp4");
    write_valid_wav(&media);

    let binary = env!("CARGO_BIN_EXE_zunzhe-core");
    let mut child = Command::new(binary)
        .args([
            "--db",
            root.join("contract.db").to_str().unwrap(),
            "extension",
        ])
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let requests = [
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"process_media","arguments":{"file_path":media.display().to_string(),"anchor_name":"契约博主"}}}),
    ];
    for request in requests {
        let id = request["id"].as_i64().unwrap();
        let started = Instant::now();
        writeln!(stdin, "{}", serde_json::to_string(&request).unwrap()).unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        assert!(!line.is_empty(), "extension 在响应 id={id} 前退出");
        // 这个用例检查“有没有协议响应、有没有误触重活”，不是 CPU 性能基准。
        // 本机可能同时跑 Whisper 转写，子进程在 8 秒内甚至未必获得调度。
        // 因此统一给 20 秒有界阈值；空响应、id 错配、工具报错仍然立即失败。
        let deadline = Duration::from_secs(20);
        assert!(
            started.elapsed() < deadline,
            "冷启动工具调用必须在 {deadline:?} 内返回，id={id} 实际 {:?}",
            started.elapsed()
        );
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], id);
        if id == 3 {
            let body: serde_json::Value =
                serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                    .unwrap();
            assert_eq!(body["ok"], true);
            assert_eq!(body["queued"], true);
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&root);
}
