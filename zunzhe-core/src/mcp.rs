use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use serde_json::{Value, json};
use sqlx::sqlite::SqlitePool;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::error;

use crate::app_prefs;
use crate::config::Config;
use crate::config_api::ConfigApi;
use crate::db;
use crate::platform;
use crate::scheduler;
use crate::training_data;
use crate::video_library::VideoLibrary;
use crate::watchlist::Watchlist;

/// 极简 MCP stdio server：支持 initialize / tools/list / tools/call。
/// 工具名与 Python 版对齐；未实现的返回明确错误，不静默失败。
pub async fn run_mcp(pool: SqlitePool, project_dir: &Path, cfg: Arc<Config>) -> Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin).lines();
    let mut writer = stdout;
    // MCP 请求可能非常频繁；daemon 存活探测要读 pid/心跳文件，不能每条请求
    // 都做。至少 60 秒才重新探测一次，维护由 6 小时哨兵自身节流。
    let mut daemon_probe_at = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(61))
        .unwrap_or_else(std::time::Instant::now);
    let mut daemon_alive = crate::service::rust_daemon_alive(project_dir);
    let library = Arc::new(VideoLibrary::new(
        pool.clone(),
        project_dir.join("fupan_output"),
    ));
    let fupan_dir = project_dir.join("fupan_output");
    let watch = Arc::new(Watchlist::new(project_dir.to_path_buf()));
    let api = Arc::new(ConfigApi::new(project_dir.to_path_buf()));
    if let Err(e) = crate::video_library::migrate_legacy_video_db(&pool, &fupan_dir).await {
        error!("视频库旧库合并失败（MCP 继续运行）: {e:#}");
    }
    if let Err(e) = crate::video_library::migrate_legacy_core_db(&pool, project_dir).await {
        error!("旧核心 DB 合并失败（MCP 继续运行）: {e:#}");
    }
    if let Ok(rewritten) = db::rewrite_legacy_task_paths(&pool, project_dir).await
        && rewritten > 0
    {
        tracing::info!("MCP 启动时改写 {rewritten} 条指向旧桌面根的任务路径");
    }
    while let Ok(Some(line)) = reader.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        // MCP server 可能长驻而 daemon 未运行：独立入口也要低频收敛数据库。
        // 每次请求前检查哨兵，daemon 已接管时由主循环维护，未接管时这里
        // 补位；哨兵与 CLI/daemon 共用，避免多个进程重复 checkpoint。
        // 失败只记日志，不中断当前请求。
        if daemon_probe_at.elapsed() >= std::time::Duration::from_secs(60) {
            daemon_alive = crate::service::rust_daemon_alive(project_dir);
            daemon_probe_at = std::time::Instant::now();
        }
        if !daemon_alive && let Err(e) = db::maintenance_if_due(project_dir, &pool).await {
            error!("MCP 数据库维护失败（继续运行）: {e:#}");
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = write_response(&mut writer, json!({"jsonrpc":"2.0","error":{"code":-32700,"message":e.to_string()},"id":null})).await;
                continue;
            }
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        let result = match method {
            "initialize" => json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "zunzhe-core", "version": env!("CARGO_PKG_VERSION")},
            }),
            "tools/list" => json!({"tools": tools_list()}),
            "tools/call" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                // 配置可能在 MCP 进程存活期间被 CLI/桌面端/其它智能体热更新
                // （邮箱、Cookie、复盘开关、自动下载等）。每轮调用前重载，
                // 失败沿用上一轮配置，保证“保存即生效”且不中断 MCP。
                let current = reload_config_for_mcp(project_dir, &cfg);
                let ctx = ToolContext {
                    library: &library,
                    fupan_dir: &fupan_dir,
                    cfg: &current,
                    pool: &pool,
                    watch: &watch,
                    api: &api,
                    project_dir,
                };
                match call_tool(name, &args, &ctx).await {
                    Ok(out) => json!({"content": [{"type":"text","text":out}], "isError": false}),
                    Err(e) => {
                        // 错误正文必须是合法 JSON，且不能手拼转义。客户端通常会按
                        // JSON 解析 `text`，如果错误信息带引号/换行，手拼 `\"` 容易
                        // 产生非法转义，导致“工具失败但客户端拿不到原因”。
                        let body = serde_json::json!({
                            "ok": false,
                            "error": e.to_string(),
                        });
                        json!({
                            "content": [{"type":"text","text": serde_json::to_string(&body).unwrap_or_else(|_| "{\"ok\":false,\"error\":\"MCP 错误序列化失败\"}".to_string())}],
                            "isError": true
                        })
                    }
                }
            }
            "notifications/initialized" => continue,
            _ => {
                json!({"error": {"code": -32601, "message": format!("method not found: {method}")}})
            }
        };
        let response = json!({"jsonrpc":"2.0","id":id,"result":result});
        if let Err(e) = write_response(&mut writer, response).await {
            error!("MCP 写响应失败: {e}");
            break;
        }
    }
    Ok(())
}

/// MCP 每次工具调用前重新读取配置；读取失败沿用启动/上一轮配置。
fn reload_config_for_mcp(project_dir: &Path, fallback: &Config) -> Config {
    Config::load(project_dir).unwrap_or_else(|e| {
        tracing::warn!("MCP 配置热重载失败，沿用上一轮: {e:#}");
        fallback.clone()
    })
}

struct ToolContext<'a> {
    library: &'a VideoLibrary,
    fupan_dir: &'a Path,
    cfg: &'a Config,
    pool: &'a SqlitePool,
    watch: &'a Watchlist,
    api: &'a ConfigApi,
    project_dir: &'a Path,
}

async fn write_response(writer: &mut tokio::io::Stdout, value: Value) -> std::io::Result<()> {
    let mut line = serde_json::to_string(&value)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await
}

pub fn tools_list() -> Vec<Value> {
    vec![
        json!({"name":"live_status","description":"查看整体状态：监控、视频库、邮箱","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"live_list","description":"列出直播监控项","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"live_add","description":"添加直播监控","inputSchema":{"type":"object","properties":{"url":{"type":"string"},"name":{"type":"string"},"quality":{"type":"string"}},"required":["url"]}}),
        json!({"name":"live_remove","description":"移除直播监控","inputSchema":{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}}),
        json!({"name":"live_delete","description":"彻底删除直播监控","inputSchema":{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}}),
        json!({"name":"live_check","description":"检查直播监控项","inputSchema":{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}}),
        json!({"name":"live_set_notify","description":"设置直播监控通知通道","inputSchema":{"type":"object","properties":{"url":{"type":"string"},"channel":{"type":"string"},"name":{"type":"string"}},"required":["url"]}}),
        json!({"name":"record_live","description":"手动录制一次直播（秒数，0=一直录到结束）","inputSchema":{"type":"object","properties":{"url":{"type":"string"},"duration_secs":{"type":"number"}},"required":["url"]}}),
        json!({"name":"service_status","description":"查看 Python 服务、Rust daemon 与直播录制会话","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"tail_logs","description":"读取后台服务日志末尾","inputSchema":{"type":"object","properties":{"name":{"type":"string"},"lines":{"type":"number"}}}}),
        json!({"name":"video_add","description":"添加博主发视频监控","inputSchema":{"type":"object","properties":{"url":{"type":"string"},"name":{"type":"string"},"download":{"type":"string"}},"required":["url"]}}),
        json!({"name":"video_remove","description":"移除发视频监控","inputSchema":{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}}),
        json!({"name":"video_list","description":"列出发视频监控","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"video_check_now","description":"立即检查一轮新视频","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"blogger_videos","description":"列出博主历史视频（count=0 表示全部）","inputSchema":{"type":"object","properties":{"url":{"type":"string"},"count":{"type":"number"}},"required":["url"]}}),
        json!({"name":"download_video","description":"下载视频到 downloads/短视频/","inputSchema":{"type":"object","properties":{"url":{"type":"string"},"name":{"type":"string"}},"required":["url"]}}),
        json!({"name":"video_download","description":"下载视频到 downloads/短视频/（Python 兼容别名）","inputSchema":{"type":"object","properties":{"url":{"type":"string"},"name":{"type":"string"}},"required":["url"]}}),
        json!({"name":"download_videos","description":"批量下载多个视频（跳过已下载与空 URL）","inputSchema":{"type":"object","properties":{"items_json":{"type":"string"}},"required":["items_json"]}}),
        json!({"name":"video_library","description":"查询视频库","inputSchema":{"type":"object","properties":{"source_url":{"type":"string"},"downloaded_only":{"type":"boolean"}}}}),
        json!({"name":"status","description":"桌面端状态快照（含最近录制、复盘产物与目录）","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"export_training_data","description":"导出训练数据 JSONL","inputSchema":{"type":"object","properties":{"anchor":{"type":"string"},"output":{"type":"string"}}}}),
        json!({"name":"email_status","description":"查看邮箱配置状态","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"email_setup","description":"写入 QQ 邮箱配置","inputSchema":{"type":"object","properties":{"email":{"type":"string"},"auth":{"type":"string"},"to":{"type":"string"}},"required":["email","auth"]}}),
        json!({"name":"app_prefs_get","description":"读取桌面应用偏好","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"app_prefs_set","description":"写入桌面应用偏好","inputSchema":{"type":"object","properties":{"updates":{"type":"object"}}}}),
        json!({"name":"config_get","description":"读取配置","inputSchema":{"type":"object","properties":{"file":{"type":"string"},"section":{"type":"string"},"key":{"type":"string"}}}}),
        json!({"name":"config_set","description":"写入配置","inputSchema":{"type":"object","properties":{"file":{"type":"string"},"section":{"type":"string"},"key":{"type":"string"},"value":{"type":"string"}},"required":["file","section","key","value"]}}),
        json!({"name":"config_set_bulk","description":"批量写入配置","inputSchema":{"type":"object","properties":{"updates_json":{"type":"string"}}}}),
        json!({"name":"config_schema_tool","description":"配置结构说明","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"notes_list","description":"列出复盘产物","inputSchema":{"type":"object","properties":{"name":{"type":"string"}}}}),
        json!({"name":"process_media","description":"对音视频跑复盘流水线","inputSchema":{"type":"object","properties":{"file_path":{"type":"string"},"anchor_name":{"type":"string"}},"required":["file_path"]}}),
        json!({"name":"service_start","description":"启动 Rust 核心 daemon（统一直播+视频监控）","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"service_stop","description":"停止 Rust 核心 daemon","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"ensure_services","description":"确保 Rust 核心 daemon 运行（桌面端/watchdog 入口）","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"doctor","description":"运行环境诊断（日志/磁盘/ffmpeg/守护心跳/监控源健康）","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"wizard_done","description":"标记桌面向导完成","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"test_email","description":"发送测试邮件","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"test_notify","description":"发送测试通知（Python 兼容别名）","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"notify_list","description":"列出最近通知记录（status 可填 pending/sent/failed/空）","inputSchema":{"type":"object","properties":{"status":{"type":"string"},"limit":{"type":"number"}}}}),
        json!({"name":"notify_retry","description":"把失败通知重新放回队列（配置补齐后手动重发）","inputSchema":{"type":"object","properties":{"limit":{"type":"number"}}}}),
        json!({"name":"notify_prune","description":"清理已发送/已失败的通知记录（limit=0 表示全部）","inputSchema":{"type":"object","properties":{"status":{"type":"string"},"limit":{"type":"number"}}}}),
    ]
}

async fn call_tool(name: &str, args: &Value, ctx: &ToolContext<'_>) -> Result<String> {
    let ToolContext {
        library,
        fupan_dir,
        cfg,
        pool,
        watch,
        api,
        project_dir,
    } = ctx;
    match name {
        "live_status" => Ok(serde_json::to_string(
            &crate::status_snapshot::build(project_dir, pool, cfg).await?,
        )?),
        "status" => Ok(serde_json::to_string(
            &crate::status_snapshot::build(project_dir, pool, cfg).await?,
        )?),
        "live_list" => Ok(serde_json::to_string(
            &json!({"ok":true,"rooms":watch.list_live()}),
        )?),
        "live_add" => {
            let url = args.get("url").and_then(|v| v.as_str()).unwrap_or_default();
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let quality = args
                .get("quality")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            watch.add_live(url, &format!("主播: {name}"), quality)?;
            Ok(serde_json::to_string(
                &json!({"ok":true,"url":url,"name":name}),
            )?)
        }
        "live_remove" => {
            let url = args.get("url").and_then(|v| v.as_str()).unwrap_or_default();
            let removed = watch.remove_live(url)?;
            Ok(serde_json::to_string(&json!({"ok":removed,"url":url}))?)
        }
        "live_delete" => {
            let url = args.get("url").and_then(|v| v.as_str()).unwrap_or_default();
            let deleted = watch.delete_live(url)?;
            Ok(serde_json::to_string(
                &json!({"ok": deleted, "url": url, "action": "delete-live"}),
            )?)
        }
        "live_set_notify" => {
            let url = args.get("url").and_then(|v| v.as_str()).unwrap_or_default();
            let channel = args
                .get("channel")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            match watch.set_notify(url, channel, name) {
                Ok(_) => Ok(serde_json::to_string(
                    &json!({"ok": true, "url": url, "channel": channel, "name": name}),
                )?),
                Err(e) => Ok(serde_json::to_string(
                    &json!({"ok": false, "url": url, "error": e.to_string()}),
                )?),
            }
        }
        "record_live" => {
            let url = args.get("url").and_then(|v| v.as_str()).unwrap_or_default();
            let duration_secs = args
                .get("duration_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let file =
                crate::live_monitor::record_live_once(pool, project_dir, cfg, url, duration_secs)
                    .await?;
            Ok(serde_json::to_string(
                &json!({"ok":true,"url":url,"file":file,"duration_secs":duration_secs}),
            )?)
        }
        "tail_logs" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("live_monitor");
            let lines = args.get("lines").and_then(|v| v.as_u64()).unwrap_or(200);
            Ok(serde_json::to_string(&crate::service::tail_logs(
                project_dir,
                name,
                lines,
            ))?)
        }
        "app_prefs_get" => Ok(serde_json::to_string(
            &json!({"ok": true, "prefs": app_prefs::read_app_prefs(project_dir)}),
        )?),
        "app_prefs_set" => {
            let updates = args.get("updates").cloned().unwrap_or_else(|| json!({}));
            let prefs = app_prefs::write_app_prefs(project_dir, &updates)?;
            Ok(serde_json::to_string(&json!({"ok": true, "prefs": prefs}))?)
        }
        "live_check" => {
            let url = args.get("url").and_then(|v| v.as_str()).unwrap_or_default();
            let room = watch.list_live().into_iter().find(|r| r.url == url);
            let status = if let Some(r) = &room {
                let adapter = crate::live_monitor::resolve_live_for_project(
                    &r.url,
                    project_dir,
                    &cfg.get_main("Cookie", "b站cookie"),
                );
                match adapter.check(&r.url).await {
                    Ok(s) => serde_json::to_value(&s).ok(),
                    Err(e) => Some(json!({"error": e.to_string()})),
                }
            } else {
                None
            };
            Ok(serde_json::to_string(
                &json!({"ok":true,"url":url,"found":room.is_some(),"room":room,
                        "status":status,"hint":"Rust daemon 运行中会自动检测并录制开播"}),
            )?)
        }
        "video_add" => {
            let url = args.get("url").and_then(|v| v.as_str()).unwrap_or_default();
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let download = args
                .get("download")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let download_opt = match download {
                "" => None,
                "是" | "yes" | "true" | "1" => Some(true),
                "否" | "no" | "false" | "0" => Some(false),
                _ => None,
            };
            watch.add_video(url, name, download_opt, None)?;
            Ok(serde_json::to_string(
                &json!({"ok":true,"url":url,"name":name,"download":download}),
            )?)
        }
        "video_remove" => {
            let url = args.get("url").and_then(|v| v.as_str()).unwrap_or_default();
            let removed = watch.remove_video(url)?;
            Ok(serde_json::to_string(&json!({"ok":removed,"url":url}))?)
        }
        "video_list" => Ok(serde_json::to_string(
            &json!({"ok":true,"sources":watch.list_video()}),
        )?),
        "video_check_now" => {
            let stats = scheduler::check_video_sources(pool, project_dir, cfg).await?;
            Ok(serde_json::to_string(
                &json!({"ok":true,"new_videos":stats.videos_notified+stats.downloads,
                        "notified":stats.videos_notified,"downloads":stats.downloads}),
            )?)
        }
        "blogger_videos" => {
            let url = args
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let count = args.get("count").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let cookie = cfg.get_fupan("视频监控", "b站cookie");
            let adapter: Box<dyn platform::Platform> = if url.contains("bilibili.com") {
                Box::new(platform::BilibiliAdapter::with_cookie_and_project_dir(
                    cookie,
                    project_dir.to_path_buf(),
                ))
            } else {
                Box::new(platform::RssAdapter::new())
            };
            // 与 CLI blogger-videos 共用同一浏览入口：成功入库，失败回退缓存。
            let outcome = crate::video_library::browse_with_fallback(
                library,
                adapter.list_videos(&url, count).await,
                &url,
            )
            .await?;
            Ok(serde_json::to_string(
                &json!({"ok":true,"platform":adapter.name(),"count":outcome.videos.len(),"from_cache":outcome.from_cache,"warning":outcome.warning,"videos":crate::video_library::videos_compat_json(&outcome.videos)}),
            )?)
        }
        "download_video" | "video_download" => {
            let url = args
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("手动下载")
                .to_string();
            // 与 CLI 一样统一走下载任务队列，避免与 daemon worker 并发下载。
            // daemon 未运行时先拉起，确保入队任务真的会被消费。
            if !crate::service::rust_daemon_alive(project_dir) {
                crate::service::ensure_core(project_dir)?;
            }
            let task_id = crate::download::enqueue_download(pool, &url, &name).await?;
            Ok(serde_json::to_string(&json!({
                "ok": true,
                "queued": true,
                "task_id": task_id,
                "url": url,
                "name": name,
                "hint": "已加入 daemon 下载队列，下载完成后会自动标记已下载",
            }))?)
        }
        "download_videos" => {
            let items_json = args
                .get("items_json")
                .and_then(|v| v.as_str())
                .unwrap_or("[]");
            let parsed: Vec<serde_json::Value> = serde_json::from_str(items_json)?;
            let items: Vec<(String, String)> = parsed
                .iter()
                .map(|v| {
                    let url = v.get("url").and_then(|u| u.as_str()).unwrap_or_default();
                    let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("手动下载");
                    (url.to_string(), name.to_string())
                })
                .collect();
            if !crate::service::rust_daemon_alive(project_dir) {
                crate::service::ensure_core(project_dir)?;
            }
            let summary = crate::download::enqueue_downloads(pool, &items).await?;
            Ok(serde_json::to_string(&summary)?)
        }
        "video_library" => {
            let source = args
                .get("source_url")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let done = args
                .get("downloaded_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let videos = library.list(source, "", done).await?;
            Ok(serde_json::to_string(
                &json!({"ok":true,"count":videos.len(),"videos":crate::video_library::videos_compat_json(&videos)}),
            )?)
        }
        "export_training_data" => {
            let anchor = args
                .get("anchor")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let output = args
                .get("output")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            Ok(serde_json::to_string(&training_data::export(
                fupan_dir, anchor, output,
            )?)?)
        }
        "email_status" => Ok(serde_json::to_string(&cfg.email_status())?),
        "email_setup" => {
            let email = args
                .get("email")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let auth = args
                .get("auth")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let to = args.get("to").and_then(|v| v.as_str()).unwrap_or_default();
            crate::config::write_qq_email(project_dir, email, auth, to)?;
            Ok(serde_json::to_string(
                &json!({"ok":true,"email":email,"to":if to.is_empty(){email}else{to}}),
            )?)
        }
        "config_get" => {
            let file = args.get("file").and_then(|v| v.as_str()).unwrap_or("all");
            let section = args.get("section").and_then(|v| v.as_str()).unwrap_or("");
            let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");
            Ok(serde_json::to_string(&api.get(file, section, key)?)?)
        }
        "config_set" => {
            let file = args
                .get("file")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let section = args
                .get("section")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let key = args.get("key").and_then(|v| v.as_str()).unwrap_or_default();
            let value = args
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            Ok(serde_json::to_string(&api.set(file, section, key, value)?)?)
        }
        "config_set_bulk" => {
            let updates = args
                .get("updates_json")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let parsed = if updates.is_string() {
                serde_json::from_str(updates.as_str().unwrap_or("{}"))?
            } else {
                updates
            };
            Ok(serde_json::to_string(&api.set_bulk(&parsed)?)?)
        }
        "config_schema_tool" => Ok(serde_json::to_string(&api.schema())?),
        "notes_list" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let notes = crate::pipeline_bridge::list_notes(fupan_dir, name)?;
            Ok(serde_json::to_string(&json!({"ok":true,"notes":notes}))?)
        }
        "process_media" => {
            let file_path = args
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let anchor_name = args
                .get("anchor_name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            // 与 CLI 一致：daemon 未运行时先拉起，确保入队后复盘 worker 会消费。
            if !crate::service::rust_daemon_alive(project_dir) {
                crate::service::ensure_core(project_dir)?;
            }
            let task_id =
                crate::download::enqueue_process(pool, project_dir, file_path, anchor_name).await?;
            Ok(serde_json::to_string(&json!({
                "ok": true,
                "queued": true,
                "task_id": task_id,
                "file": file_path,
                "anchor": anchor_name,
                "hint": "已加入 daemon 复盘队列，转写/提炼/切片由后台 worker 处理"
            }))?)
        }
        "service_start" => Ok(serde_json::to_string(&crate::service::ensure_core(
            project_dir,
        )?)?),
        "ensure_services" => Ok(serde_json::to_string(&crate::service::ensure_core(
            project_dir,
        )?)?),
        "service_stop" => Ok(serde_json::to_string(&crate::service::stop_rust_daemon(
            project_dir,
        )?)?),
        "doctor" => {
            // 与 CLI doctor 共用同一口径：源进入冷却后，该博主的新视频会
            // 静默不通知/不下载，必须让 MCP 调用方也能一眼看到平台风控。
            // 读取失败只记字段为未知，不让 doctor 本身失败。
            let mut diag = crate::service::diagnostics(project_dir)?;
            if let Ok((total, failing, cooling)) = db::source_health_summary(pool).await
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
            if let Ok((pending, failed, sent)) = db::notify_health_summary(pool).await
                && let Some(obj) = diag.as_object_mut()
            {
                let configured = cfg.email_configured() || cfg.push_channels().configured();
                obj.insert(
                    "notify_health".to_string(),
                    json!({
                        "pending": pending,
                        "failed": failed,
                        "sent": sent,
                        "configured": configured,
                        "ok": failed == 0 && pending == 0 && configured,
                    }),
                );
            }
            Ok(serde_json::to_string(&diag)?)
        }
        "wizard_done" => {
            let mut updates = serde_json::Map::new();
            updates.insert("wizardDone".to_string(), Value::Bool(true));
            let prefs =
                app_prefs::write_app_prefs(project_dir, &serde_json::Value::Object(updates))?;
            Ok(serde_json::to_string(&json!({"ok":true,"prefs":prefs}))?)
        }
        "service_status" => {
            // 与 CLI `service-status` 共用同一个轻量入口：只读 pid/心跳/uptime，
            // 不递归扫描 downloads 和 fupan_output。调用方要完整桌面快照应使用 `status`。
            let mut result = crate::service::status(project_dir)?;
            let sessions = db::list_live_sessions(pool, "recording", 100).await?;
            result
                .as_object_mut()
                .unwrap()
                .insert("live_sessions".to_string(), serde_json::to_value(sessions)?);
            Ok(serde_json::to_string(&result)?)
        }
        "test_email" => {
            let status = cfg.email_status();
            if !status.configured {
                return Ok(serde_json::to_string(
                    &json!({"ok":false,"sent":false,"error":status.hint}),
                )?);
            }
            crate::notify::send_email(
                cfg.email_settings(),
                "【尊者直播监控】通知测试",
                "收到这条消息说明通知通道配置成功。",
            )?;
            Ok(serde_json::to_string(&json!({"ok":true,"sent":true}))?)
        }
        "test_notify" => {
            let notifier = crate::notify::Notifier::for_project(pool, cfg, project_dir);
            match notifier
                .send_test(
                    "【尊者直播监控】通知测试",
                    "收到这条消息说明通知通道配置成功。",
                )
                .await
            {
                Ok(test) => Ok(serde_json::to_string(&json!({
                    "ok": true,
                    "sent": test.sent,
                    "channels": test.channels,
                }))?),
                Err(e) => Ok(serde_json::to_string(
                    &json!({"ok": false, "sent": false, "error": e.to_string()}),
                )?),
            }
        }
        "notify_list" => {
            let status = args
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(50);
            let limit = if limit <= 0 { 200 } else { limit };
            let events = db::list_notify(pool, status, limit).await?;
            Ok(serde_json::to_string(
                &json!({"ok":true,"count":events.len(),"notify":events}),
            )?)
        }
        "notify_retry" => {
            let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(100);
            let limit = if limit <= 0 { 100 } else { limit };
            let retried = db::retry_failed_notify(pool, limit).await?;
            let pending = db::list_pending_notify(pool, limit).await?;
            Ok(serde_json::to_string(&json!({
                "ok":true,
                "retried":retried,
                "pending_count":pending.len(),
                "hint":"已把失败通知放回队列，daemon 下一轮按退避重试；也可再执行 notify-retry 立即重发",
            }))?)
        }
        "notify_prune" => {
            let status = args
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("failed");
            let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(0);
            let limit = if limit <= 0 { i64::MAX } else { limit };
            let removed = db::prune_notify(pool, status, limit).await?;
            Ok(serde_json::to_string(
                &json!({"ok":true,"removed":removed,"status":status}),
            )?)
        }
        _ => anyhow::bail!("工具未实现: {name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_list_has_unique_names_and_required_args() {
        let tools = tools_list();
        let mut names = std::collections::HashSet::new();
        for tool in &tools {
            let name = tool["name"].as_str().unwrap_or_default();
            assert!(!name.is_empty(), "工具缺 name: {tool}");
            assert!(names.insert(name.to_string()), "工具名重复: {name}");
            let schema = tool
                .get("inputSchema")
                .and_then(|s| s.as_object())
                .unwrap_or_else(|| panic!("工具 {name} 缺 inputSchema"));
            if let Some(props) = schema.get("properties") {
                assert!(props.is_object(), "工具 {name} properties 必须是对象");
            }
        }
        assert_eq!(names.len(), tools.len());
    }

    #[test]
    fn tools_list_covers_python_mcp_surface() {
        let tools = tools_list();
        let names: std::collections::HashSet<String> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect();
        for expected in [
            "live_status",
            "live_add",
            "live_remove",
            "live_list",
            "live_check",
            "video_add",
            "video_download",
            "download_videos",
            "blogger_videos",
            "video_remove",
            "video_list",
            "video_check_now",
            "service_start",
            "service_stop",
            "config_get",
            "config_set",
            "config_set_bulk",
            "config_schema_tool",
            "notes_list",
            "process_media",
            "test_email",
            "notify_list",
            "notify_retry",
            "notify_prune",
        ] {
            assert!(names.contains(expected), "MCP 缺工具: {expected}");
        }
    }

    #[test]
    fn mcp_error_text_is_valid_json_with_special_chars() {
        // 工具失败时错误正文必须是可解析 JSON，且引号/换行不能破坏转义。
        let mut response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "content": [],
                "isError": true
            }
        });
        let err = anyhow::anyhow!("模拟错误：包含 \"引号\" 与\n换行");
        let body = serde_json::json!({"ok": false, "error": err.to_string()});
        let text = serde_json::to_string(&body).unwrap();
        response["result"]["content"] = serde_json::json!([{"type": "text", "text": text}]);
        let serialized = serde_json::to_string(&response).unwrap();
        let parsed: Value = serde_json::from_str(&serialized).unwrap();
        let text = parsed["result"]["content"][0]["text"].as_str().unwrap();
        let inner: Value = serde_json::from_str(text).unwrap();
        assert_eq!(inner["ok"], false);
        assert!(inner["error"].as_str().unwrap().contains("引号"));
    }

    #[tokio::test]
    async fn mcp_doctor_contract_includes_source_health() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-mcp-doctor-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        std::fs::create_dir_all(dir.join("fupan_output").join("run")).unwrap();
        std::fs::write(
            dir.join("config").join("config.ini"),
            "[推送配置]\nSMTP邮件服务器 = smtp.qq.com\n",
        )
        .unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "[视频监控]\n").unwrap();
        std::fs::write(
            dir.join("config").join("config.ini.example"),
            "[推送配置]\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("config").join("fupan_config.ini.example"),
            "[视频监控]\n",
        )
        .unwrap();

        let file = std::env::temp_dir().join(format!(
            "zunzhe-mcp-doctor-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = crate::db::open_pool(&file).await.unwrap();
        crate::db::init_db(&pool).await.unwrap();

        let fupan_dir = dir.join("fupan_output");
        let library = VideoLibrary::new(pool.clone(), fupan_dir.clone());
        let watch = Watchlist::new(dir.to_path_buf());
        let api = ConfigApi::new(dir.to_path_buf());
        let cfg = Config::load(&dir).unwrap();
        let ctx = ToolContext {
            library: &library,
            fupan_dir: &fupan_dir,
            cfg: &cfg,
            pool: &pool,
            watch: &watch,
            api: &api,
            project_dir: &dir,
        };

        let out = call_tool("doctor", &json!({}), &ctx).await.unwrap();
        let diag: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(diag["ok"], true);
        assert_eq!(
            diag["source_health"],
            json!({"total": 0, "failing": 0, "cooling": 0, "ok": true})
        );

        std::mem::forget(file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tail_logs_contract_clamps_and_counts() {
        let dir = std::env::temp_dir().join(format!("zunzhe-tail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        std::fs::write(dir.join("logs").join("live_monitor.log"), "a\nb\nc\nd\ne\n").unwrap();
        let out = crate::service::tail_logs(&dir, "live_monitor", 3);
        assert_eq!(out["ok"], true);
        assert_eq!(out["content"], "c\nd\ne");
        assert_eq!(out["lines"], 3);
        let out = crate::service::tail_logs(&dir, "live_monitor", 0);
        assert_eq!(out["lines"], 1, "0 行应按至少 1 行处理，不能返回空内容");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn mcp_reloads_config_between_tool_calls() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-mcp-reload-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(
            dir.join("config").join("config.ini"),
            "[推送配置]\nSMTP邮件服务器 = smtp.qq.com\n",
        )
        .unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "[视频监控]\n").unwrap();
        std::fs::write(
            dir.join("config").join("config.ini.example"),
            "[推送配置]\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("config").join("fupan_config.ini.example"),
            "[视频监控]\n",
        )
        .unwrap();

        let file = std::env::temp_dir().join(format!(
            "zunzhe-mcp-reload-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = crate::db::open_pool(&file).await.unwrap();
        crate::db::init_db(&pool).await.unwrap();
        let fallback = Config::load(&dir).unwrap();

        // 第一次重载：配置里没有邮箱，email_status 未配置。
        let first = reload_config_for_mcp(&dir, &fallback);
        assert!(!first.email_status().configured);

        // 模拟配置被外部修改：写入完整邮箱，然后下一次重载应读到新配置。
        crate::config::write_qq_email(&dir, "me@qq.com", "authcode", "you@qq.com").unwrap();
        let second = reload_config_for_mcp(&dir, &fallback);
        assert!(
            second.email_status().configured,
            "MCP 应读到热更新后的邮箱配置"
        );
        assert_eq!(second.email_status().host, "smtp.qq.com");

        std::mem::forget(file);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
