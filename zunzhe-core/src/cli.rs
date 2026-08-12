use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};

use crate::{
    config, db, download, notify, pipeline_bridge, platform, scheduler, training_data,
    video_library,
};

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
    /// 添加关注的主播/直播间（旧名 add-anchor 继续保留）
    AddLive {
        url: String,
        #[arg(default_value = "")]
        name: String,
    },
    /// 添加关注的主播/直播间（Python 版兼容别名）
    AddAnchor {
        url: String,
        #[arg(default_value = "")]
        name: String,
    },
    /// 删除关注的主播/直播间（旧名 remove-anchor 继续保留）
    RemoveLive { url: String },
    /// 删除关注的主播/直播间（Python 版兼容别名）
    RemoveAnchor { url: String },
    /// 列出关注的主播/直播间（旧名 list-anchors 继续保留）
    ListLive,
    /// 列出关注的主播/直播间（Python 版兼容别名）
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
    /// 查看邮箱配置状态
    EmailStatus,
    /// 写入 QQ 邮箱配置
    EmailSetup {
        email: String,
        auth: String,
        #[arg(default_value = "")]
        to: String,
    },
    /// 导出训练数据 JSONL
    ExportTrainingData {
        #[arg(default_value = "")]
        anchor: String,
        #[arg(default_value = "")]
        output: String,
    },
    /// 发送测试邮件
    TestEmail,
    /// 整体状态（Rust 版当前能力）
    Status,
    /// 运行守护调度：--once 只跑一轮；--duration 秒后退出
    RunDaemon {
        #[arg(long, default_value_t = false)]
        once: bool,
        #[arg(long)]
        duration: Option<u64>,
        #[arg(long, default_value_t = 900)]
        interval: u64,
    },
    /// 启动 MCP stdio server
    Mcp,
    /// 对任意音视频文件跑 Python 复盘流水线（转写/提炼/切片）
    Process {
        file_path: String,
        #[arg(default_value = "")]
        anchor_name: String,
    },
}

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    let start_dir = std::env::current_dir()?;
    let cfg = config::Config::load(&start_dir)?;
    // 所有路径以项目根为基准，避免从 zunzhe-core/ 运行时把 DB/下载写错位置。
    let project_dir = cfg.project_dir.clone();
    let db_path = cli.db.unwrap_or_else(|| project_dir.join("zunzhe-core.db"));
    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true);
    let pool = SqlitePool::connect_with(options).await?;
    db::init_db(&pool).await?;
    let fupan_dir = project_dir.join("fupan_output");

    match cli.command {
        Command::AddLive { url, name } | Command::AddAnchor { url, name } => {
            db::add_anchor(&pool, &url, &name).await?;
            println!(
                "{}",
                json!({"ok": true, "url": url, "name": name, "action": "add-anchor"})
            );
        }
        Command::RemoveLive { url } | Command::RemoveAnchor { url } => {
            let removed = db::remove_anchor(&pool, &url).await?;
            println!(
                "{}",
                json!({"ok": removed, "url": url, "action": "remove-anchor"})
            );
        }
        Command::ListLive | Command::ListAnchors => {
            for anchor in db::list_anchors(&pool).await? {
                println!("{}", serde_json::to_string(&anchor)?);
            }
        }
        Command::BloggerVideos { url, count } => {
            let cookie = cfg.get_fupan("视频监控", "b站cookie");
            let adapter: Box<dyn platform::Platform> = if url.contains("bilibili.com") {
                Box::new(platform::BilibiliAdapter::with_cookie(cookie))
            } else {
                Box::new(platform::RssAdapter::new())
            };
            let videos = adapter.list_videos(&url, count).await?;
            let library = video_library::VideoLibrary::new(pool.clone(), fupan_dir.clone());
            for v in &videos {
                let _ = library.upsert(v, "", &url).await;
            }
            let result = json!({
                "ok": true,
                "platform": adapter.name(),
                "url": url,
                "count": videos.len(),
                "videos": videos,
            });
            println!("{}", result);
        }
        Command::DownloadVideo { url, name } => {
            let downloader = download::Downloader::new(project_dir.clone());
            match downloader.download(&url, &name, &pool).await {
                Ok(result) => println!("{}", serde_json::to_string(&result)?),
                Err(e) => println!("{}", json!({"ok": false, "error": e.to_string()})),
            }
        }
        Command::EmailStatus => {
            let status = cfg.email_status();
            println!("{}", serde_json::to_string(&status)?);
        }
        Command::EmailSetup { email, auth, to } => {
            config::write_qq_email(&project_dir, &email, &auth, &to)?;
            println!(
                "{}",
                json!({"ok": true, "email": email, "to": if to.is_empty() { email } else { to }})
            );
        }
        Command::ExportTrainingData { anchor, output } => {
            let summary = training_data::export(&fupan_dir, &anchor, &output)?;
            println!("{}", serde_json::to_string(&summary)?);
        }
        Command::VideoLibrary {
            source_url,
            anchor,
            downloaded_only,
        } => {
            let library = video_library::VideoLibrary::new(pool.clone(), fupan_dir.clone());
            let videos = library
                .list(
                    &source_url,
                    &anchor,
                    matches!(downloaded_only.as_str(), "1" | "是" | "true" | "yes"),
                )
                .await?;
            println!(
                "{}",
                json!({"ok": true, "videos": videos, "count": videos.len()})
            );
        }
        Command::TestEmail => {
            let status = cfg.email_status();
            if !status.configured {
                println!(
                    "{}",
                    json!({"ok": false, "sent": false, "error": status.hint})
                );
            } else {
                match notify::send_email(
                    cfg.email_settings(),
                    "【尊者直播监控】通知测试",
                    "收到这条消息说明通知通道配置成功。",
                ) {
                    Ok(_) => {
                        let notifier = notify::Notifier::new(&pool, &cfg);
                        let _ = notifier
                            .record_sent(
                                "test",
                                "",
                                "【尊者直播监控】通知测试",
                                "收到这条消息说明通知通道配置成功。",
                                notify::Channel::Email,
                            )
                            .await;
                        println!("{}", json!({"ok": true, "sent": true}));
                    }
                    Err(e) => println!(
                        "{}",
                        json!({"ok": false, "sent": false, "error": e.to_string()})
                    ),
                }
            }
        }
        Command::Status => {
            let anchors = db::list_anchors(&pool).await?;
            let library = video_library::VideoLibrary::new(pool.clone(), fupan_dir.clone());
            let videos = library.list("", "", false).await?;
            let email = cfg.email_status();
            println!(
                "{}",
                json!({
                    "ok": true,
                    "anchors": anchors,
                    "video_count": videos.len(),
                    "email": email,
                    "version": env!("CARGO_PKG_VERSION"),
                })
            );
        }
        Command::RunDaemon {
            once,
            duration,
            interval,
        } => {
            scheduler::run_daemon(pool.clone(), &project_dir, &cfg, interval, once, duration)
                .await?;
        }
        Command::Mcp => {
            use std::sync::Arc;
            let cfg = Arc::new(cfg);
            crate::mcp::run_mcp(pool.clone(), &project_dir, cfg).await?;
        }
        Command::Process {
            file_path,
            anchor_name,
        } => {
            let summary = pipeline_bridge::run_pipeline(&cfg, &file_path, &anchor_name).await?;
            println!("{}", serde_json::to_string(&summary)?);
        }
    }
    Ok(())
}
