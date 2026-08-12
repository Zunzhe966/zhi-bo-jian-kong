use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};

use crate::{
    config, config_api, db, download, notify, pipeline_bridge, platform, scheduler, service,
    training_data, video_library, watchlist,
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
        #[arg(default_value = "")]
        quality: String,
    },
    /// 添加关注的主播/直播间（Python 版兼容别名）
    AddAnchor {
        url: String,
        #[arg(default_value = "")]
        name: String,
        #[arg(default_value = "")]
        quality: String,
    },
    /// 删除关注的主播/直播间（旧名 remove-anchor 继续保留）
    RemoveLive { url: String },
    /// 删除关注的主播/直播间（Python 版兼容别名）
    RemoveAnchor { url: String },
    /// 列出关注的主播/直播间（旧名 list-anchors 继续保留）
    ListLive,
    /// 列出关注的主播/直播间（Python 版兼容别名）
    ListAnchors,
    /// 检查单个直播间监控状态
    CheckLive { url: String },
    /// 添加博主发视频监控
    AddVideo {
        url: String,
        #[arg(default_value = "")]
        name: String,
        /// ''=跟随全局 / 是 / 否
        #[arg(default_value = "")]
        download: String,
        #[arg(default_value = "")]
        notify: String,
    },
    /// 移除博主发视频监控
    RemoveVideo { url: String },
    /// 列出博主发视频监控
    ListVideo,
    /// 立即检查一轮所有视频监控源
    CheckVideos,
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
    /// 读取配置（all/config/fupan_config，密钥脱敏）
    GetConfig {
        #[arg(default_value = "all")]
        file: String,
        #[arg(default_value = "")]
        section: String,
        #[arg(default_value = "")]
        key: String,
    },
    /// 写入单个配置项
    SetConfig {
        file: String,
        section: String,
        key: String,
        value: String,
    },
    /// 批量写入配置（JSON）
    SetConfigBulk { updates: String },
    /// 返回配置结构说明
    Schema,
    /// 列出复盘产物
    Notes {
        #[arg(default_value = "")]
        name: String,
    },
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
    /// 启动 Python 直播监控 + 视频监控后台服务（兼容层）
    Start,
    /// 停止全部 Python 后台服务（兼容层）
    Stop,
    /// 查看后台服务与最近录制状态
    ServiceStatus,
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
    let watch = watchlist::Watchlist::new(project_dir.clone());
    let api = config_api::ConfigApi::new(project_dir.clone());

    match cli.command {
        Command::AddLive { url, name, quality } | Command::AddAnchor { url, name, quality } => {
            db::add_anchor(&pool, &url, &name).await?;
            let _ = watch.add_live(&url, &name, &quality);
            println!(
                "{}",
                json!({"ok": true, "url": url, "name": name, "quality": quality, "action": "add-anchor"})
            );
        }
        Command::RemoveLive { url } | Command::RemoveAnchor { url } => {
            let removed = db::remove_anchor(&pool, &url).await?;
            let _ = watch.remove_live(&url);
            println!(
                "{}",
                json!({"ok": removed, "url": url, "action": "remove-anchor"})
            );
        }
        Command::ListLive | Command::ListAnchors => {
            let anchors = db::list_anchors(&pool).await?;
            let items = watch.list_live();
            println!(
                "{}",
                json!({"ok": true, "rooms": items, "anchors": anchors})
            );
        }
        Command::CheckLive { url } => {
            let rooms = watch.list_live();
            let found = rooms
                .iter()
                .any(|r| r.url == url || r.url.trim_end_matches('/') == url.trim_end_matches('/'));
            let room = rooms
                .into_iter()
                .find(|r| r.url == url || r.url.trim_end_matches('/') == url.trim_end_matches('/'));
            println!(
                "{}",
                json!({
                    "url": url,
                    "found": found,
                    "room": room,
                    "hint": "Rust daemon 运行中会自动检测开播"
                })
            );
        }
        Command::AddVideo {
            url,
            name,
            download,
            notify,
        } => {
            let url_for_opt = url.clone();
            let name_for_opt = name.clone();
            let download_opt = match download.as_str() {
                "" => None,
                "是" | "yes" | "true" | "1" => Some(true),
                "否" | "no" | "false" | "0" => Some(false),
                other => {
                    println!(
                        "{}",
                        json!({"ok": false, "error": format!("下载参数不识别: {other}")})
                    );
                    return Ok(());
                }
            };
            watch.add_video(
                &url_for_opt,
                &name_for_opt,
                download_opt,
                if notify.is_empty() {
                    None
                } else {
                    Some(&notify)
                },
            )?;
            db::add_anchor(&pool, &url_for_opt, &name_for_opt).await?;
            println!(
                "{}",
                json!({"ok": true, "url": url, "name": if name.is_empty() { url.clone() } else { name.clone() },
                       "download": match download_opt { Some(true) => "是", Some(false) => "否", None => "跟随全局" },
                       "notify": if notify.is_empty() { watch.notify_for(&url, &name) } else { notify }})
            );
        }
        Command::RemoveVideo { url } => {
            let removed = watch.remove_video(&url)?;
            let _ = db::remove_anchor(&pool, &url).await;
            println!(
                "{}",
                json!({"ok": removed, "url": url, "action": "remove-video"})
            );
        }
        Command::ListVideo => {
            let items = watch.list_video();
            println!("{}", json!({"ok": true, "sources": items}));
        }
        Command::CheckVideos => {
            let result = scheduler::check_video_sources(&pool, &project_dir, &cfg).await;
            match result {
                Ok(stats) => println!(
                    "{}",
                    json!({"ok": true, "new_videos": stats.videos_notified + stats.downloads,
                           "checked": stats.anchors_checked, "notified": stats.videos_notified,
                           "downloads": stats.downloads})
                ),
                Err(e) => println!("{}", json!({"ok": false, "error": e.to_string()})),
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
        Command::GetConfig { file, section, key } => {
            println!("{}", api.get(&file, &section, &key)?);
        }
        Command::SetConfig {
            file,
            section,
            key,
            value,
        } => {
            println!("{}", api.set(&file, &section, &key, &value)?);
        }
        Command::SetConfigBulk { updates } => {
            let value: serde_json::Value = serde_json::from_str(&updates)
                .map_err(|e| anyhow::anyhow!("JSON 解析失败: {e}"))?;
            println!("{}", api.set_bulk(&value)?);
        }
        Command::Schema => {
            println!("{}", api.schema());
        }
        Command::Notes { name } => {
            let notes = pipeline_bridge::list_notes(&fupan_dir, &name)?;
            println!("{}", json!({"ok": true, "notes": notes}));
        }
        Command::Status => {
            let anchors = db::list_anchors(&pool).await?;
            let library = video_library::VideoLibrary::new(pool.clone(), fupan_dir.clone());
            let videos = library.list("", "", false).await?;
            let email = cfg.email_status();
            let live = watch.list_live();
            let video_sources = watch.list_video();
            println!(
                "{}",
                json!({
                    "ok": true,
                    "anchors": anchors,
                    "live_watchlist": live,
                    "video_watchlist": video_sources,
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
        Command::Start => {
            let result = service::ensure_started(&project_dir)?;
            println!("{}", serde_json::to_string(&result)?);
        }
        Command::Stop => {
            let result = service::stop_all(&project_dir)?;
            println!("{}", serde_json::to_string(&result)?);
        }
        Command::ServiceStatus => {
            let result = service::status(&project_dir)?;
            println!("{}", serde_json::to_string(&result)?);
        }
    }
    Ok(())
}
