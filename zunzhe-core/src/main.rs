use clap::Parser;
use tracing_subscriber::EnvFilter;
use zunzhe_core::cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 日志写入 stderr，不影响 CLI 的 stdout JSON 契约；RUST_LOG=debug 可在排查时打开。
    {
        // panic 也要留下可诊断日志，而不是静默退出（配合 launchd KeepAlive 自动拉起）。
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            eprintln!("[zunzhe-core] panic: {info}");
            // 桌面端读 stdout 的 JSON 契约；panic 时也输出中文错误，
            // 避免用户看到 Rust 默认的英文崩溃信息。
            println!(
                "{}",
                serde_json::json!({"ok": false, "error": "操作失败：后台程序处理内容时出错，请重试"})
            );
            default_hook(info);
        }));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
            // extension 的 stdout 是 JSON-RPC 协议通道；任何普通日志混进去都会
            // 让桌面端把日志当协议行解析并直接报错。日志必须显式走 stderr。
            .with_writer(std::io::stderr)
            .try_init();
    }

    let cli = cli::Cli::parse();
    cli::run(cli).await
}
