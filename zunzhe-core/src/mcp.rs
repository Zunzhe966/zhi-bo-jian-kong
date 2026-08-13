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
use crate::download::Downloader;
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
    let library = Arc::new(VideoLibrary::new(
        pool.clone(),
        project_dir.join("fupan_output"),
    ));
    let downloader = Arc::new(Downloader::new(project_dir.to_path_buf()));
    let fupan_dir = project_dir.join("fupan_output");
    let watch = Arc::new(Watchlist::new(project_dir.to_path_buf()));
    let api = Arc::new(ConfigApi::new(project_dir.to_path_buf()));
    if let Err(e) = crate::video_library::migrate_legacy_video_db(&pool, &fupan_dir).await {
        error!("视频库旧库合并失败（MCP 继续运行）: {e:#}");
    }

    while let Ok(Some(line)) = reader.next_line().await {
        if line.trim().is_empty() {
            continue;
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
                let ctx = ToolContext {
                    library: &library,
                    downloader: &downloader,
                    fupan_dir: &fupan_dir,
                    cfg: &cfg,
                    pool: &pool,
                    watch: &watch,
                    api: &api,
                    project_dir,
                };
                match call_tool(name, &args, &ctx).await {
                    Ok(out) => json!({"content": [{"type":"text","text":out}], "isError": false}),
                    Err(e) => {
                        json!({"content": [{"type":"text","text":format!("{{\"ok\":false,\"error\":\"{}\"}}", e.to_string().replace('"', "\\\""))}], "isError": true})
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

struct ToolContext<'a> {
    library: &'a VideoLibrary,
    downloader: &'a Downloader,
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
        json!({"name":"blogger_videos","description":"列出博主历史视频","inputSchema":{"type":"object","properties":{"url":{"type":"string"},"count":{"type":"number"}},"required":["url"]}}),
        json!({"name":"download_video","description":"下载视频到 downloads/短视频/","inputSchema":{"type":"object","properties":{"url":{"type":"string"},"name":{"type":"string"}},"required":["url"]}}),
        json!({"name":"video_download","description":"下载视频到 downloads/短视频/（Python 兼容别名）","inputSchema":{"type":"object","properties":{"url":{"type":"string"},"name":{"type":"string"}},"required":["url"]}}),
        json!({"name":"video_library","description":"查询视频库","inputSchema":{"type":"object","properties":{"source_url":{"type":"string"},"downloaded_only":{"type":"boolean"}}}}),
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
        json!({"name":"doctor","description":"运行环境诊断（日志/磁盘/ffmpeg/守护心跳）","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"wizard_done","description":"标记桌面向导完成","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"test_email","description":"发送测试邮件","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"test_notify","description":"发送测试通知（Python 兼容别名）","inputSchema":{"type":"object","properties":{}}}),
    ]
}

async fn call_tool(name: &str, args: &Value, ctx: &ToolContext<'_>) -> Result<String> {
    let ToolContext {
        library,
        downloader,
        fupan_dir,
        cfg,
        pool,
        watch,
        api,
        project_dir,
    } = ctx;
    match name {
        "live_status" => {
            let videos = library.list("", "", false).await?;
            Ok(serde_json::to_string(
                &json!({"ok":true,"video_count":videos.len(),"email":cfg.email_status(),
                        "live_watchlist":watch.list_live(),"video_watchlist":watch.list_video()}),
            )?)
        }
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
            db::add_anchor(pool, url, name).await?;
            Ok(serde_json::to_string(
                &json!({"ok":true,"url":url,"name":name}),
            )?)
        }
        "live_remove" => {
            let url = args.get("url").and_then(|v| v.as_str()).unwrap_or_default();
            let removed = watch.remove_live(url)?;
            let _ = db::remove_anchor(pool, url).await;
            Ok(serde_json::to_string(&json!({"ok":removed,"url":url}))?)
        }
        "live_delete" => {
            let url = args.get("url").and_then(|v| v.as_str()).unwrap_or_default();
            let removed = db::remove_anchor(pool, url).await?;
            let deleted = watch.delete_live(url)?;
            Ok(serde_json::to_string(
                &json!({"ok": removed || deleted, "url": url, "action": "delete-live"}),
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
            db::add_anchor(pool, url, name).await?;
            Ok(serde_json::to_string(
                &json!({"ok":true,"url":url,"name":name,"download":download}),
            )?)
        }
        "video_remove" => {
            let url = args.get("url").and_then(|v| v.as_str()).unwrap_or_default();
            let removed = watch.remove_video(url)?;
            let _ = db::remove_anchor(pool, url).await;
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
            let count = args.get("count").and_then(|v| v.as_u64()).unwrap_or(30) as u32;
            let cookie = cfg.get_fupan("视频监控", "b站cookie");
            let adapter: Box<dyn platform::Platform> = if url.contains("bilibili.com") {
                Box::new(platform::BilibiliAdapter::with_cookie_and_project_dir(
                    cookie,
                    project_dir.to_path_buf(),
                ))
            } else {
                Box::new(platform::RssAdapter::new())
            };
            let videos = adapter.list_videos(&url, count).await?;
            for v in &videos {
                let _ = library.upsert(v, "", &url).await;
            }
            let persisted = library.list(&url, "", false).await?;
            Ok(serde_json::to_string(
                &json!({"ok":true,"platform":adapter.name(),"count":persisted.len(),"videos":crate::video_library::videos_compat_json(&persisted)}),
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
            let result = downloader.download(&url, &name, pool).await?;
            Ok(serde_json::to_string(&result)?)
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
            Ok(serde_json::to_string(
                &crate::pipeline_bridge::run_pipeline(cfg, file_path, anchor_name).await?,
            )?)
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
        "doctor" => Ok(serde_json::to_string(&crate::service::diagnostics(
            project_dir,
        )?)?),
        "wizard_done" => {
            let mut updates = serde_json::Map::new();
            updates.insert("wizardDone".to_string(), Value::Bool(true));
            let prefs =
                app_prefs::write_app_prefs(project_dir, &serde_json::Value::Object(updates))?;
            Ok(serde_json::to_string(&json!({"ok":true,"prefs":prefs}))?)
        }
        "service_status" => {
            let rust_running = crate::service::rust_daemon_alive(project_dir);
            let mut result = crate::service::status(project_dir)?;
            let sessions = db::list_live_sessions(pool, "recording", 100).await?;
            result
                .as_object_mut()
                .unwrap()
                .insert("live_sessions".to_string(), serde_json::to_value(sessions)?);
            result
                .as_object_mut()
                .unwrap()
                .insert("rust_daemon".to_string(), json!(rust_running));
            result.as_object_mut().unwrap().insert(
                "services".to_string(),
                json!({
                    "live_monitor": {"running": rust_running},
                    "video_monitor": {"running": rust_running},
                }),
            );
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
            let notifier = crate::notify::Notifier::new(pool, cfg);
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
        ] {
            assert!(names.contains(expected), "MCP 缺工具: {expected}");
        }
    }
}
