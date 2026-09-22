use std::process::Stdio;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

fn isolated_project(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "zunzhe-control-plane-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(
        root.join("config").join("config.ini"),
        "[路径设置]\n\
         产物根目录 = products\n\
         数据库文件 = products/database/zunzhe-core.db\n",
    )
    .unwrap();
    std::fs::write(root.join("config").join("fupan_config.ini"), "[视频监控]\n").unwrap();
    root
}

async fn call(
    stdin: &mut tokio::process::ChildStdin,
    stdout: &mut BufReader<tokio::process::ChildStdout>,
    request: Value,
) -> Value {
    stdin
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    stdin.flush().await.unwrap();
    let mut line = String::new();
    stdout.read_line(&mut line).await.unwrap();
    assert!(!line.is_empty(), "extension 在响应前退出：{request}");
    serde_json::from_str(&line).unwrap()
}

#[tokio::test]
async fn stdio_control_plane_exposes_all_tools_and_central_risk_classes() {
    let root = isolated_project("surface");
    let binary = env!("CARGO_BIN_EXE_zunzhe-core");
    let mut child = Command::new(binary)
        .args([
            "--db",
            root.join("products/database/zunzhe-core.db")
                .to_str()
                .unwrap(),
            "extension",
        ])
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let initialized = call(
        &mut stdin,
        &mut stdout,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    )
    .await;
    assert_eq!(initialized["result"]["serverInfo"]["name"], "zunzhe-core");

    let listed = call(
        &mut stdin,
        &mut stdout,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    )
    .await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    let names: std::collections::HashSet<&str> = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(tools.len(), 43, "源码控制面必须暴露 43 个工具");
    assert!(names.contains("control_list"));
    assert!(names.contains("audit_list"));

    let controlled = call(
        &mut stdin,
        &mut stdout,
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"control_list","arguments":{}}}),
    )
    .await;
    assert_eq!(controlled["result"]["isError"], false);
    let body: Value =
        serde_json::from_str(controlled["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["count"], 43);
    let controls = body["controls"].as_array().unwrap();
    for control in controls {
        let name = control["name"].as_str().unwrap();
        assert_ne!(control["risk"], "unknown", "工具 {name} 未登记风险级别");
    }

    drop(stdin);
    let _ = child.start_kill();
    let _ = child.wait().await;
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn process_command_starts_core_when_daemon_is_offline() {
    let source = include_str!("../src/cli.rs");
    let process_branch = source
        .split("Command::Process {")
        .nth(1)
        .and_then(|rest| rest.split("Command::Start =>").next())
        .expect("必须能定位 Command::Process 分支");

    assert!(
        process_branch.contains("ensure_core(&project_dir)")
            || process_branch.contains("ensure_core(project_dir)"),
        "process 入队后若 daemon 未运行，必须主动拉起；只返回 daemon_running=false 会让转写按钮像假按钮"
    );
}
