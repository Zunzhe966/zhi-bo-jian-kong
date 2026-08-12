use std::path::PathBuf;

use clap::{Parser, Subcommand};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};

use crate::db;
use crate::platform;

#[derive(Debug, Parser)]
#[command(name = "zunzhe-core", version, about = "尊者直播监控 Rust 核心 daemon")]
pub struct Cli {
    /// SQLite 数据库路径，默认 ./zunzhe-core.db
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 添加关注的主播/直播间
    AddAnchor {
        url: String,
        #[arg(default_value = "")]
        name: String,
    },
    /// 删除关注的主播/直播间
    RemoveAnchor { url: String },
    /// 列出关注的主播/直播间
    ListAnchors,
    /// 查询视频库
    VideoLibrary {
        #[arg(default_value = "")]
        source_url: String,
        #[arg(default_value = "")]
        anchor: String,
        #[arg(default_value = "")]
        downloaded_only: String,
    },
    /// 列出博主历史视频（调用平台适配器，骨架阶段为空）
    BloggerVideos {
        url: String,
        #[arg(default_value = "30")]
        count: u32,
    },
    /// 加入下载任务（骨架阶段只入队）
    DownloadVideo {
        url: String,
        #[arg(default_value = "手动下载")]
        name: String,
    },
}

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    let db_path = cli.db.unwrap_or_else(|| PathBuf::from("zunzhe-core.db"));
    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true);
    let pool = SqlitePool::connect_with(options).await?;
    db::init_db(&pool).await?;

    match cli.command {
        Command::AddAnchor { url, name } => {
            db::add_anchor(&pool, &url, &name).await?;
            println!("added {} {}", url, name);
        }
        Command::RemoveAnchor { url } => {
            let removed = db::remove_anchor(&pool, &url).await?;
            println!("removed={}", removed);
        }
        Command::ListAnchors => {
            for anchor in db::list_anchors(&pool).await? {
                println!("{}", serde_json::to_string(&anchor)?);
            }
        }
        Command::VideoLibrary {
            source_url,
            anchor,
            downloaded_only,
        } => {
            let videos = db::list_videos(
                &pool,
                &source_url,
                &anchor,
                matches!(downloaded_only.as_str(), "1" | "是" | "true" | "yes"),
            )
            .await?;
            println!("{}", serde_json::to_string(&videos)?);
        }
        Command::BloggerVideos { url, count } => {
            let adapter = platform::resolve(&url);
            let videos = adapter.list_videos(&url, count).await?;
            let result = serde_json::json!({
                "ok": true,
                "platform": adapter.name(),
                "url": url,
                "count": videos.len(),
                "videos": videos,
            });
            println!("{}", result);
        }
        Command::DownloadVideo { url, name } => {
            let id = db::enqueue_task(&pool, "download", &url, &name).await?;
            println!("enqueued id={} url={} name={}", id, url, name);
        }
    }
    Ok(())
}
