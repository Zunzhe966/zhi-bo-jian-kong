mod app_prefs;
mod cli;
mod config;
mod config_api;
mod db;
mod download;
mod live_monitor;
mod mcp;
mod model;
mod notify;
mod pipeline_bridge;
mod platform;
mod scheduler;
mod service;
mod training_data;
mod video_library;
mod watchlist;

use clap::Parser;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 只有 run-daemon / mcp 需要日志；CLI 命令必须保持 stdout 只有一行 JSON。
    let is_daemon = std::env::args().any(|a| a == "run-daemon" || a == "mcp");
    if is_daemon {
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
            .init();
    }

    let cli = cli::Cli::parse();
    cli::run(cli).await
}
