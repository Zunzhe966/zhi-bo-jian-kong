use clap::Parser;
use tracing_subscriber::EnvFilter;
use zunzhe_core::cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 只有 run-daemon / mcp 需要日志；CLI 命令必须保持 stdout 只有一行 JSON。
    let is_daemon = std::env::args().any(|a| a == "run-daemon" || a == "mcp");
    if is_daemon {
        // panic 也要留下可诊断日志，而不是静默退出（配合 launchd KeepAlive 自动拉起）。
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            eprintln!("[zunzhe-core] panic: {info}");
            default_hook(info);
        }));
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
            .init();
    }

    let cli = cli::Cli::parse();
    cli::run(cli).await
}
