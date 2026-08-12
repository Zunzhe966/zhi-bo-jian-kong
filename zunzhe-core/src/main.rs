mod cli;
mod config;
mod db;
mod download;
mod mcp;
mod model;
mod notify;
mod pipeline_bridge;
mod platform;
mod scheduler;
mod training_data;
mod video_library;

use clap::Parser;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let cli = cli::Cli::parse();
    cli::run(cli).await
}
