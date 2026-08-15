use std::path::PathBuf;

use crate::{
    config, config_api, db, download, live_monitor, notify, pipeline_bridge, platform, scheduler,
    service, training_data, video_library, watchlist,
};
use clap::{Parser, Subcommand};
use serde_json::json;

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
    /// 彻底删除关注的主播/直播间（Python 版兼容）
    DeleteLive { url: String },
    /// 删除关注的主播/直播间（Python 版兼容别名）
    RemoveAnchor { url: String },
    /// 列出关注的主播/直播间（旧名 list-anchors 继续保留）
    ListLive,
    /// 列出关注的主播/直播间（Python 版兼容别名）
    ListAnchors,
    /// 检查单个直播间监控状态
    CheckLive { url: String },
    /// 设置某监控项通知通道（跟随全局/邮箱/微信/不通知）
    SetNotify {
        url: String,
        #[arg(default_value = "跟随全局")]
        channel: String,
        #[arg(default_value = "")]
        name: String,
    },
    /// 手动录制一次直播（秒数，0=一直录到结束）
    RecordLive {
        url: String,
        #[arg(default_value_t = 0)]
        duration_secs: u64,
    },
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
    /// 列出博主历史视频；count=0 表示全部，默认全部
    BloggerVideos {
        url: String,
        #[arg(default_value = "0")]
        count: u32,
    },
    /// 下载指定视频到 downloads/短视频/
    DownloadVideo {
        url: String,
        #[arg(default_value = "手动下载")]
        name: String,
    },
    /// 批量下载多个视频到 downloads/短视频/（JSON 数组：[{"url":"...","name":"..."}]）
    DownloadVideos { items_json: String },
    /// 查看邮箱配置状态
    EmailStatus,
    /// 写入 QQ 邮箱配置
    EmailSetup {
        email: String,
        auth: String,
        #[arg(default_value = "")]
        to: String,
    },
    /// 从旧配置导入邮箱（兼容 Python 命令，当前直接返回提示）
    EmailImportLegacy {
        #[arg(default_value = "")]
        legacy_file: String,
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
    /// 发送测试通知（Python 兼容别名）
    TestNotify,
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
    /// 打印 MCP 接入说明（Python 版兼容命令）
    McpInfo,
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
    /// 启动 Rust 核心 daemon（统一接管直播与视频监控）
    Start,
    /// 确保服务运行（桌面端/兼容层入口）
    EnsureServices,
    /// 停止 Rust 核心 daemon（兼容层）
    Stop,
    /// 读取后台服务日志末尾
    TailLogs {
        #[arg(default_value = "live_monitor")]
        name: String,
        #[arg(default_value_t = 200)]
        lines: u64,
    },
    /// 读取桌面应用偏好
    GetAppPrefs,
    /// 写入桌面应用偏好（JSON）
    SetAppPrefs { updates: String },
    /// 标记向导完成
    WizardDone,
    /// 查看后台服务与最近录制状态
    ServiceStatus,
    /// 运行环境诊断（日志/磁盘/ffmpeg/守护心跳）
    Doctor,
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
    let pool = db::open_pool(&db_path).await?;
    let fupan_dir = project_dir.join("fupan_output");
    // CLI 是独立入口之一：daemon 已接管时由 daemon 主循环负责维护，未接管时
    // 这里低频收敛 WAL/通知/任务/录制历史，避免长期只跑 CLI/MCP 无限膨胀。
    // 哨兵节流保证每个 CLI 进程不会重复 checkpoint；失败只记日志。
    if !service::rust_daemon_alive(&project_dir)
        && let Err(e) = db::maintenance_if_due(&project_dir, &pool).await
    {
        eprintln!("数据库维护失败（不影响本次命令）: {e:#}");
    }
    // 旧 Python 把视频库单独存在 fupan_output/video_library.db；启动命令前先
    // 无副作用合并到 Rust 主 DB，避免已下载标记、历史视频元数据被另一套 DB 遗忘。
    // 迁移接口本身幂等且跳过相同 id，失败不应阻塞只读查询，只记录诊断信息。
    if let Err(e) = video_library::migrate_legacy_video_db(&pool, &fupan_dir).await {
        eprintln!("视频库旧库合并失败（不影响本次命令）: {e:#}");
    }
    // 旧核心残留 DB zunzhe-core/zunzhe-core.db 同样以主库为权威安全合并；
    // 合并成功后归档旧文件，防止后续入口再次打开它继续累积另一套状态。
    if let Err(e) = video_library::migrate_legacy_core_db(&pool, &project_dir).await {
        eprintln!("旧核心 DB 合并失败（不影响本次命令）: {e:#}");
    }
    // 项目可能从旧桌面副本迁到当前根；每次 CLI 入口幂等改写旧任务路径，
    // 避免历史 process 任务引用过期位置。
    if let Ok(rewritten) = db::rewrite_legacy_task_paths(&pool, &project_dir).await
        && rewritten > 0
    {
        eprintln!("已改写 {rewritten} 条指向旧桌面根的任务路径");
    }
    let watch = watchlist::Watchlist::new(project_dir.clone());
    let api = config_api::ConfigApi::new(project_dir.clone());

    match cli.command {
        Command::AddLive { url, name, quality } | Command::AddAnchor { url, name, quality } => {
            let _ = watch.add_live(&url, &name, &quality);
            println!(
                "{}",
                json!({"ok": true, "url": url, "name": name, "quality": quality, "action": "add-anchor"})
            );
        }
        Command::RemoveLive { url } | Command::RemoveAnchor { url } => {
            // `remove-live` 与 Python/旧桌面端一致是“暂停监控”，只注释 URL 行，
            // 不删除监控源本身；ini 才是权威，删除只发生在 delete-live。
            let removed = watch.remove_live(&url)?;
            println!(
                "{}",
                json!({"ok": removed, "url": url, "action": "remove-live", "disabled": removed})
            );
        }
        Command::DeleteLive { url } => {
            let deleted = watch.delete_live(&url)?;
            println!(
                "{}",
                json!({"ok": deleted, "url": url, "action": "delete-live"})
            );
        }
        Command::ListLive | Command::ListAnchors => {
            let items = watch.list_live();
            let anchors = watch.live_anchor_view();
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
            let status = if let Some(r) = &room {
                let adapter = live_monitor::resolve_live_for_project(
                    &r.url,
                    &cfg.project_dir,
                    &cfg.get_main("Cookie", "b站cookie"),
                );
                match adapter.check(&r.url).await {
                    Ok(s) => serde_json::to_value(&s).ok(),
                    Err(e) => Some(json!({"error": e.to_string()})),
                }
            } else {
                None
            };
            println!(
                "{}",
                json!({
                    "url": url,
                    "found": found,
                    "room": room,
                    "status": status,
                    "hint": "Rust daemon 运行中会自动检测并录制开播"
                })
            );
        }
        Command::SetNotify { url, channel, name } => {
            let result = watch.set_notify(&url, &channel, &name);
            match result {
                Ok(_) => println!(
                    "{}",
                    json!({"ok": true, "url": url, "channel": channel, "name": name})
                ),
                Err(e) => println!(
                    "{}",
                    json!({"ok": false, "url": url, "error": e.to_string()})
                ),
            }
        }
        Command::RecordLive { url, duration_secs } => {
            match live_monitor::record_live_once(&pool, &project_dir, &cfg, &url, duration_secs)
                .await
            {
                Ok(file) => println!(
                    "{}",
                    json!({"ok": true, "url": url, "file": file, "duration_secs": duration_secs})
                ),
                Err(e) => println!(
                    "{}",
                    json!({"ok": false, "url": url, "error": e.to_string()})
                ),
            }
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
            println!(
                "{}",
                json!({"ok": true, "url": url, "name": if name.is_empty() { url.clone() } else { name.clone() },
                       "download": match download_opt { Some(true) => "是", Some(false) => "否", None => "跟随全局" },
                       "notify": if notify.is_empty() { watch.notify_for(&url, &name) } else { notify }})
            );
        }
        Command::RemoveVideo { url } => {
            let removed = watch.remove_video(&url)?;
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
                           "downloads": stats.downloads,
                           "skipped_cooldown": stats.skipped_cooldown})
                ),
                Err(e) => println!("{}", json!({"ok": false, "error": e.to_string()})),
            }
        }
        Command::BloggerVideos { url, count } => {
            let cookie = cfg.get_fupan("视频监控", "b站cookie");
            let adapter: Box<dyn platform::Platform> = if url.contains("bilibili.com") {
                Box::new(platform::BilibiliAdapter::with_cookie_and_project_dir(
                    cookie,
                    project_dir.clone(),
                ))
            } else {
                Box::new(platform::RssAdapter::new())
            };
            let videos = adapter.list_videos(&url, count).await?;
            let library = video_library::VideoLibrary::new(pool.clone(), fupan_dir.clone());
            // 批量浏览统一走有界并发入库：封面下载不串行，单张失败不阻塞
            // 其余视频，历史列表几十上百条时也不会被慢封面拖住。
            let _ = library.upsert_browse_many(&videos, &url).await;
            // 返回已入库的视频，让前端拿到封面缓存、日期、时长与已下载标记，
            // 而不是只有平台适配器返回的原始 VideoMeta。
            let persisted = library.list(&url, "", false).await?;
            let result = json!({
                "ok": true,
                "platform": adapter.name(),
                "url": url,
                "count": persisted.len(),
                "videos": video_library::videos_compat_json(&persisted),
            });
            println!("{}", result);
        }
        Command::DownloadVideo { url, name } => {
            // 手动下载与 daemon 下载 worker 必须共用任务队列：直接调用
            // Downloader::download 会绕过队列，可能与 worker 并发下载同一 URL。
            // daemon 未运行时先拉起，保证“点了就会下载”而不是只在队列里躺着。
            if !crate::service::rust_daemon_alive(&project_dir) {
                let started = crate::service::ensure_core(&project_dir);
                if let Err(e) = started {
                    println!(
                        "{}",
                        json!({
                            "ok": false,
                            "queued": false,
                            "error": format!("下载任务未入队，且 daemon 启动失败: {e:#}"),
                        })
                    );
                    return Ok(());
                }
            }
            match download::enqueue_download(&pool, &url, &name).await {
                Ok(task_id) => println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "ok": true,
                        "queued": true,
                        "task_id": task_id,
                        "url": url,
                        "name": name,
                        "hint": "已加入 daemon 下载队列，下载完成后会自动标记已下载",
                    }))?
                ),
                Err(e) => println!(
                    "{}",
                    json!({"ok": false, "queued": false, "error": e.to_string()})
                ),
            }
        }
        Command::DownloadVideos { items_json } => {
            if !crate::service::rust_daemon_alive(&project_dir) {
                let started = crate::service::ensure_core(&project_dir);
                if let Err(e) = started {
                    println!(
                        "{}",
                        json!({
                            "ok": false,
                            "queued": false,
                            "error": format!("批量下载任务未入队，且 daemon 启动失败: {e:#}"),
                        })
                    );
                    return Ok(());
                }
            }
            let parsed: Vec<serde_json::Value> = match serde_json::from_str(&items_json) {
                Ok(v) => v,
                Err(e) => {
                    println!(
                        "{}",
                        json!({"ok": false, "queued": false, "error": format!("items_json 不是 JSON 数组: {e:#}")})
                    );
                    return Ok(());
                }
            };
            let items: Vec<(String, String)> = parsed
                .iter()
                .map(|v| {
                    let url = v.get("url").and_then(|u| u.as_str()).unwrap_or_default();
                    let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("手动下载");
                    (url.to_string(), name.to_string())
                })
                .collect();
            match download::enqueue_downloads(&pool, &items).await {
                Ok(summary) => println!("{}", serde_json::to_string(&summary)?),
                Err(e) => println!(
                    "{}",
                    json!({"ok": false, "queued": false, "error": e.to_string()})
                ),
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
        Command::EmailImportLegacy { legacy_file } => {
            match config::import_legacy_email(&project_dir, &legacy_file) {
                Ok(result) => println!("{}", serde_json::to_string(&result)?),
                Err(e) => println!(
                    "{}",
                    json!({"ok": false, "error": e.to_string(), "legacy_file": legacy_file})
                ),
            }
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
                json!({"ok": true, "videos": video_library::videos_compat_json(&videos), "count": videos.len()})
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
                        let notifier = notify::Notifier::for_project(&pool, &cfg, &project_dir);
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
        Command::TestNotify => {
            let notifier = notify::Notifier::for_project(&pool, &cfg, &project_dir);
            match notifier
                .send_test(
                    "【尊者直播监控】通知测试",
                    "收到这条消息说明通知通道配置成功。",
                )
                .await
            {
                Ok(test) => {
                    println!(
                        "{}",
                        json!({
                            "ok": true,
                            "sent": test.sent,
                            "channels": test.channels,
                        })
                    );
                }
                Err(e) => println!(
                    "{}",
                    json!({"ok": false, "sent": false, "error": e.to_string()})
                ),
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
        Command::McpInfo => {
            println!(
                "{}",
                json!({
                    "mcp_command": "zunzhe-core mcp",
                    "cwd": project_dir,
                    "config_example": project_dir.join("mcp.json.example"),
                    "hint": "在 Cursor / OpenCode 等智能体的 MCP 设置中添加 stdio 服务器",
                })
            );
        }
        Command::Notes { name } => {
            let notes = pipeline_bridge::list_notes(&fupan_dir, &name)?;
            println!("{}", json!({"ok": true, "notes": notes}));
        }
        Command::Status => {
            println!(
                "{}",
                crate::status_snapshot::build(&project_dir, &pool, &cfg).await?
            );
        }
        Command::RunDaemon {
            once,
            duration,
            interval,
        } => {
            // --once 语义：先跑一轮完整 daemon tick，便于验证直播+视频+下载全链路。
            // pid 文件由 run_daemon 在拿到单实例锁后写入并随退出清理。
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
            // 与 daemon 的 process 队列走同一条路径：只入队，不在这里直接跑 Python。
            // 这样手动 process、下载后自动复盘、录制结束自动复盘都不会重复执行，
            // 且失败/重试/超时状态由复盘 worker 统一维护。
            // daemon 未运行时先拉起，否则入队后任务会一直躺着，用户以为在
            // 转写实际没有执行；与 download-video 的自动拉起语义保持一致。
            if !service::rust_daemon_alive(&project_dir)
                && let Err(e) = service::ensure_core(&project_dir)
            {
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "ok": false,
                        "queued": false,
                        "error": format!("复盘任务未入队，且 daemon 启动失败: {e:#}"),
                    }))?
                );
                return Ok(());
            }
            let task_id =
                download::enqueue_process(&pool, &project_dir, &file_path, &anchor_name).await?;
            println!(
                "{}",
                serde_json::to_string(&json!({
                    "ok": true,
                    "queued": true,
                    "task_id": task_id,
                    "file": file_path,
                    "anchor": anchor_name,
                    "hint": "已加入 daemon 复盘队列，转写/提炼/切片由后台 worker 处理"
                }))?
            );
        }
        Command::Start => {
            let result = service::ensure_core(&project_dir)?;
            println!("{}", serde_json::to_string(&result)?);
        }
        Command::EnsureServices => {
            let result = service::ensure_core(&project_dir)?;
            println!("{}", serde_json::to_string(&result)?);
        }
        Command::Stop => {
            let result = service::stop_rust_daemon(&project_dir)?;
            println!("{}", serde_json::to_string(&result)?);
        }
        Command::TailLogs { name, lines } => {
            println!(
                "{}",
                serde_json::to_string(&service::tail_logs(&project_dir, &name, lines))?
            );
        }
        Command::GetAppPrefs => {
            let prefs = crate::app_prefs::read_app_prefs(&project_dir);
            println!("{}", json!({"ok": true, "prefs": prefs}));
        }
        Command::SetAppPrefs { updates } => {
            let value: serde_json::Value = serde_json::from_str(&updates)
                .map_err(|e| anyhow::anyhow!("JSON 解析失败: {e}"))?;
            let prefs = crate::app_prefs::write_app_prefs(&project_dir, &value)?;
            println!("{}", json!({"ok": true, "prefs": prefs}));
        }
        Command::WizardDone => {
            let mut value = serde_json::Map::new();
            value.insert("wizardDone".into(), serde_json::Value::Bool(true));
            let prefs =
                crate::app_prefs::write_app_prefs(&project_dir, &serde_json::Value::Object(value))?;
            println!("{}", json!({"ok": true, "prefs": prefs}));
        }
        Command::ServiceStatus => {
            let mut result = service::status(&project_dir)?;
            let sessions = db::list_live_sessions(&pool, "recording", 100).await?;
            result
                .as_object_mut()
                .unwrap()
                .insert("live_sessions".to_string(), serde_json::to_value(sessions)?);
            let rust_running = crate::service::rust_daemon_alive(&project_dir);
            result
                .as_object_mut()
                .unwrap()
                .insert("rust_daemon".to_string(), json!(rust_running));
            println!("{}", serde_json::to_string(&result)?);
        }
        Command::Doctor => {
            let mut diag = crate::service::diagnostics(&project_dir)?;
            // 24×30 验收不仅要看进程/心跳，还要看监控源是否持续失败。
            // 源进入冷却后，该博主的新视频会静默不通知/不下载；把源健康
            // 摘要放进 doctor，桌面端/审计脚本才能一眼看到平台风控造成的
            // “假健康”。读取失败只记字段为未知，不让 doctor 本身失败。
            if let Ok((total, failing, cooling)) = db::source_health_summary(&pool).await
                && let Some(obj) = diag.as_object_mut()
            {
                obj.insert(
                    "source_health".to_string(),
                    json!({
                        "total": total,
                        "failing": failing,
                        "cooling": cooling,
                        "ok": cooling == 0,
                    }),
                );
            }
            println!("{}", serde_json::to_string(&diag)?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Python 版 CLI 命令全集必须都能被 Rust 解析（参数以示例形式覆盖）。
    /// 这里只验证命令解析，不执行真实文件操作。
    #[test]
    fn all_python_cli_commands_parse() {
        let cases: &[&[&str]] = &[
            &["zunzhe-core", "status"],
            &[
                "zunzhe-core",
                "add-live",
                "https://live.bilibili.com/1",
                "主播甲",
                "原画",
            ],
            &["zunzhe-core", "remove-live", "https://live.bilibili.com/1"],
            &["zunzhe-core", "delete-live", "https://live.bilibili.com/1"],
            &[
                "zunzhe-core",
                "set-notify",
                "https://live.bilibili.com/1",
                "邮箱",
                "主播甲",
            ],
            &["zunzhe-core", "list-live"],
            &["zunzhe-core", "check-live", "https://live.bilibili.com/1"],
            &[
                "zunzhe-core",
                "add-video",
                "https://space.bilibili.com/1",
                "博主",
                "是",
            ],
            &[
                "zunzhe-core",
                "download-video",
                "https://www.bilibili.com/video/BV1xx411c7mD",
                "手动下载",
            ],
            &[
                "zunzhe-core",
                "download-videos",
                r#"[{"url":"https://www.bilibili.com/video/BV1xx411c7mD","name":"手动下载"}]"#,
            ],
            &[
                "zunzhe-core",
                "blogger-videos",
                "https://space.bilibili.com/1",
                "10",
            ],
            &["zunzhe-core", "video-library", "src", "anchor", "1"],
            &[
                "zunzhe-core",
                "export-training-data",
                "主播甲",
                "/tmp/out.jsonl",
            ],
            &["zunzhe-core", "email-status"],
            &[
                "zunzhe-core",
                "email-setup",
                "me@qq.com",
                "auth",
                "me@qq.com",
            ],
            &["zunzhe-core", "email-import-legacy", "legacy.ini"],
            &[
                "zunzhe-core",
                "remove-video",
                "https://space.bilibili.com/1",
            ],
            &["zunzhe-core", "list-video"],
            &["zunzhe-core", "start"],
            &["zunzhe-core", "ensure-services"],
            &["zunzhe-core", "stop"],
            &["zunzhe-core", "tail-logs", "live_monitor", "20"],
            &["zunzhe-core", "get-app-prefs"],
            &["zunzhe-core", "set-app-prefs", "{}"],
            &["zunzhe-core", "wizard-done"],
            &["zunzhe-core", "doctor"],
            &["zunzhe-core", "check-videos"],
            &["zunzhe-core", "process", "/tmp/a.mp4", "主播甲"],
            &["zunzhe-core", "notes"],
            &["zunzhe-core", "test-email"],
            &["zunzhe-core", "test-notify"],
            &["zunzhe-core", "get-config", "all", "", ""],
            &[
                "zunzhe-core",
                "set-config",
                "fupan_config",
                "视频监控",
                "检查间隔秒",
                "60",
            ],
            &["zunzhe-core", "set-config-bulk", "{}"],
            &["zunzhe-core", "schema"],
            &["zunzhe-core", "mcp-info"],
            &["zunzhe-core", "service-status"],
            &[
                "zunzhe-core",
                "record-live",
                "https://live.bilibili.com/1",
                "0",
            ],
            &["zunzhe-core", "run-daemon"],
        ];
        for case in cases {
            let result = Cli::try_parse_from(*case);
            assert!(
                result.is_ok(),
                "Python 命令未能解析为 Rust CLI: {:?} -> {:?}",
                case,
                result.err()
            );
        }
    }

    #[test]
    fn mcp_info_requires_no_extra_args() {
        let cli = Cli::try_parse_from(["zunzhe-core", "mcp-info"]).unwrap();
        assert!(matches!(cli.command, Command::McpInfo));
    }

    #[test]
    fn blogger_videos_default_and_zero_mean_all() {
        let defaulted = Cli::try_parse_from([
            "zunzhe-core",
            "blogger-videos",
            "https://space.bilibili.com/1",
        ])
        .unwrap();
        match defaulted.command {
            Command::BloggerVideos { count, .. } => assert_eq!(count, 0, "默认应表示全部"),
            other => panic!("应解析为 BloggerVideos: {other:?}"),
        }
        let explicit = Cli::try_parse_from([
            "zunzhe-core",
            "blogger-videos",
            "https://space.bilibili.com/1",
            "0",
        ])
        .unwrap();
        match explicit.command {
            Command::BloggerVideos { count, .. } => assert_eq!(count, 0),
            other => panic!("count=0 应解析为全部: {other:?}"),
        }
    }
}
