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
            default_hook(info);
        }));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
            .try_init();
    }

    let cli = cli::Cli::parse();
    cli::run(cli).await
}
