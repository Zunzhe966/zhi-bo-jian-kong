use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use serde_json::{Value, json};
use sqlx::sqlite::SqlitePool;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::error;

use crate::config::Config;
use crate::config_api::ConfigApi;
use crate::db;
use crate::mcp_audit;
use crate::scheduler;
use crate::video_library::VideoLibrary;
use crate::watchlist::Watchlist;

/// extension 的复盘输出目录必须与 CLI/daemon 使用同一份路径配置。
fn mcp_fupan_dir(project_dir: &Path) -> std::path::PathBuf {
    crate::paths::fupan(project_dir)
}

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
    let fupan_dir = mcp_fupan_dir(project_dir);
    let library = Arc::new(VideoLibrary::new(pool.clone(), fupan_dir.clone()));
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
        // 维护不能占用每条 extension 请求的关键路径：checkpoint 在锁竞争时可能等待 busy_timeout。
        if !daemon_alive {
            let maintenance_project = project_dir.to_path_buf();
            let maintenance_pool = pool.clone();
            tokio::spawn(async move {
                if let Err(e) =
                    db::maintenance_if_due(&maintenance_project, &maintenance_pool).await
                {
                    error!("extension 数据库维护失败（继续运行）: {e:#}");
                }
            });
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
                let request_started = std::time::Instant::now();
                // 配置可能在 MCP 进程存活期间被 CLI/桌面端/其它智能体热更新
                // （邮箱、Cookie、复盘开关、自动下载等）。每轮调用前重载，
                // 失败沿用上一轮配置，保证“保存即生效”且不中断 MCP。
                let current = reload_config_for_mcp(project_dir, &cfg);
                tracing::info!(
                    "tools/call {name} config_reload_ms={}",
                    request_started.elapsed().as_millis()
                );
                let ctx = ToolContext {
                    library: &library,
                    fupan_dir: &fupan_dir,
                    cfg: &current,
                    pool: &pool,
                    watch: &watch,
                    api: &api,
                    project_dir,
                };
                let started = std::time::Instant::now();
                match call_tool(name, &args, &ctx).await {
                    Ok(out) => {
                        tracing::info!(
                            "tools/call {name} tool_done_ms={} total_ms={}",
                            started.elapsed().as_millis(),
                            request_started.elapsed().as_millis()
                        );
                        if let Err(e) =
                            mcp_audit::record_call(&pool, name, &args, true, "", started.elapsed())
                                .await
                        {
                            error!("MCP 审计写入失败（工具调用已成功）: {e:#}");
                        }
                        json!({"content": [{"type":"text","text":out}], "isError": false})
                    }
                    Err(e) => {
                        if let Err(audit_err) = mcp_audit::record_call(
                            &pool,
                            name,
                            &args,
                            false,
                            &e.to_string(),
                            started.elapsed(),
                        )
                        .await
                        {
                            error!("MCP 审计写入失败（工具调用已失败）: {audit_err:#}");
                        }
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
    crate::contract::tools_list()
}

async fn call_tool(name: &str, args: &Value, ctx: &ToolContext<'_>) -> Result<String> {
    let ToolContext {
        library: _library,
        fupan_dir,
        cfg,
        pool,
        watch,
        api,
        project_dir,
    } = ctx;
    match name {
        "status" => Ok(serde_json::to_string(
            &crate::status_snapshot::build(project_dir, pool, cfg).await?,
        )?),
        "monitor_list" => Ok(serde_json::to_string(
            &json!({"ok":true,"live":watch.list_live(),"video":watch.list_video()}),
        )?),
        "monitor_add" => {
            let url = args.get("url").and_then(|v| v.as_str()).unwrap_or_default();
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if url.trim().is_empty() {
                anyhow::bail!("url 不能为空");
            }
            // 裸抖音号（如 Glx6768）：iesdouyin 免登录解析，直播+短视频一起挂。
            // 与 CLI monitor-add 同一链路（1.7.2 起）。
            if crate::platform::looks_like_douyin_id(url) {
                let (live_url, home_url, nickname) =
                    crate::platform::resolve_douyin_id_to_urls(url).await?;
                let display = if name.is_empty() { &nickname } else { name };
                watch.add_live(&live_url, &format!("主播: {display}"), "")?;
                watch.add_video(&home_url, display, None, None)?;
                return Ok(serde_json::to_string(
                    &json!({"ok":true,"url":url,"name":display,"resolved_live":live_url,
                            "resolved_video":home_url,"added_live":true,"added_video":true}),
                )?);
            }
            let lower = url.to_lowercase();
            let live_domain = lower.contains("live.bilibili")
                || lower.contains("live.kuaishou")
                || lower.contains("live.douyin")
                || lower.contains("live_");
            let video_domain = lower.contains("space.bilibili")
                || lower.contains("douyin.com/user")
                || lower.contains("v.douyin")
                || lower.contains("/user/");
            let mut added_live = false;
            let mut added_video = false;
            // 抖音主页链接的直播自动挂载结果说明：成功为空串，失败/跳过为可读原因。
            // 绝不静默——挂不上必须让调用方知道，否则用户以为直播也在监控。
            let mut douyin_live_note = String::new();
            if live_domain && !video_domain {
                let remark = if name.is_empty() { url } else { name };
                watch.add_live(url, &format!("主播: {remark}"), "")?;
                added_live = true;
            } else if video_domain && !live_domain {
                // 抖音主页链接：顺带解析抖音号，把直播监控也挂上
                // （DouyinLiveRecorder 同款：live.douyin.com/{抖音号}，免登录检测）
                if lower.contains("douyin.com/user") {
                    match crate::platform::douyin_unique_id_from_url(url, project_dir).await {
                        Ok(info) => {
                            let uid = info.get("unique_id").and_then(|v| v.as_str()).unwrap_or("");
                            if !uid.is_empty() && uid != "0" {
                                let live_url = format!("https://live.douyin.com/{uid}");
                                let lname = if name.is_empty() {
                                    live_url.clone()
                                } else {
                                    name.to_string()
                                };
                                watch.add_live(&live_url, &format!("主播: {lname}"), "")?;
                                added_live = true;
                            } else {
                                douyin_live_note =
                                    "解析到的抖音号为空，未自动挂直播监控".to_string();
                            }
                        }
                        Err(e) => {
                            douyin_live_note = format!(
                                "抖音号解析失败，未自动挂直播监控: {e}；可改用 live.douyin.com/抖音号 链接再添加一次"
                            );
                        }
                    }
                }
                let vname = if name.is_empty() { url } else { name };
                watch.add_video(url, vname, None, None)?;
                added_video = true;
            } else {
                // 主播主页或无法区分：直播+短视频一起挂，宁多勿漏。
                let remark = if name.is_empty() { url } else { name };
                watch.add_live(url, &format!("主播: {remark}"), "")?;
                added_live = true;
                let vname = if name.is_empty() { url } else { name };
                watch.add_video(url, vname, None, None)?;
                added_video = true;
            }
            Ok(serde_json::to_string(
                &json!({"ok":true,"url":url,"name":name,"added_live":added_live,"added_video":added_video,"douyin_live_note":douyin_live_note,"hint":"daemon 运行中会自动检测；短视频需 视频监控列表.ini 与平台 cookie"}),
            )?)
        }
        "monitor_remove" => {
            let url = args.get("url").and_then(|v| v.as_str()).unwrap_or_default();
            let removed_live = watch.remove_live(url)?;
            let removed_video = watch.remove_video(url)?;
            Ok(serde_json::to_string(
                &json!({"ok":removed_live||removed_video,"url":url,"removed_live":removed_live,"removed_video":removed_video}),
            )?)
        }
        "monitor_set" => {
            let url = args.get("url").and_then(|v| v.as_str()).unwrap_or_default();
            let key = args.get("key").and_then(|v| v.as_str()).unwrap_or_default();
            let value = args
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if url.trim().is_empty() || key.trim().is_empty() {
                anyhow::bail!("url 与 key 不能为空");
            }
            let is_live = watch
                .list_live()
                .iter()
                .any(|i| i.url.trim_end_matches('/') == url.trim_end_matches('/'));
            let is_video = watch
                .list_video()
                .iter()
                .any(|i| i.url.trim() == url.trim());
            if is_live {
                let changed = watch.set_live(url, key, value)?;
                Ok(serde_json::to_string(
                    &json!({"ok":true,"kind":"live","url":url,"key":key,"value":value,"changed":changed}),
                )?)
            } else if is_video {
                let changed = watch.set_video(url, key, value)?;
                Ok(serde_json::to_string(
                    &json!({"ok":true,"kind":"video","url":url,"key":key,"value":value,"changed":changed}),
                )?)
            } else {
                anyhow::bail!("监控源不存在: {url}（先 monitor_list 确认）");
            }
        }
        "check_now" => {
            // 直播逐个实况探测；短视频走 scheduler 全量检查（通知+自动下载）。
            let cookie = cfg.get_main("Cookie", "b站cookie");
            let mut rooms = Vec::new();
            for room in watch.list_live() {
                let adapter =
                    crate::live_monitor::resolve_live_for_project(&room.url, project_dir, &cookie);
                match adapter.check(&room.url).await {
                    Ok(s) => rooms.push(json!({
                        "url": room.url,
                        "remark": room.remark,
                        "ok": true,
                        "status": serde_json::to_value(&s).ok(),
                    })),
                    Err(e) => rooms.push(json!({"url": room.url, "remark": room.remark, "ok": false, "error": e.to_string()})),
                }
            }
            let stats = scheduler::check_video_sources(pool, project_dir, cfg).await?;
            Ok(serde_json::to_string(&json!({
                "ok":true,
                "live":rooms,
                "new_videos":stats.videos_notified+stats.downloads,
                "notified":stats.videos_notified,
                "downloads":stats.downloads,
                "hint":"Rust daemon 运行中会自动检测并录制开播"
            }))?)
        }
        "download_video" => {
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
        "process_media" => {
            let started = std::time::Instant::now();
            let file_path = args
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let anchor_name = args
                .get("anchor_name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            // 与 CLI `process` 一致：daemon 不在就先拉起，否则任务落库 pending
            // 却没有任何 worker 消费它——智能体看到「已加入队列」后永远等不到
            // 结果（1.7.5 修复）。拉起失败**不阻断入队**（文件已入库还能由
            // 之后的 daemon 消费），但必须把原因写进 startup_error 字段。
            let (daemon_running, startup_error) = if crate::service::rust_daemon_alive(project_dir) {
                (true, None)
            } else {
                match crate::service::ensure_core(project_dir) {
                    Ok(_) => (true, None),
                    Err(e) => (false, Some(format!("{e:#}"))),
                }
            };
            let result =
                crate::download::enqueue_process(pool, project_dir, file_path, anchor_name).await?;
            tracing::info!(
                "process_media enqueue finished task_id={} queued={} elapsed_ms={}",
                result.task_id,
                result.queued,
                started.elapsed().as_millis()
            );
            let daemon_running = daemon_running || crate::service::rust_daemon_alive(project_dir);
            tracing::info!(
                "process_media daemon probe finished daemon_running={} elapsed_ms={}",
                daemon_running,
                started.elapsed().as_millis()
            );
            Ok(serde_json::to_string(&json!({
                "ok": true,
                "queued": result.queued,
                "task_id": result.task_id,
                "status": result.status,
                "disposition": result.disposition,
                "daemon_running": daemon_running,
                "startup_error": startup_error,
                "file": file_path,
                "anchor": anchor_name,
                "hint": result.hint
            }))?)
        }
        "notes_list" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let media_root = crate::paths::media_root(project_dir);
            let notes = crate::pipeline_bridge::list_notes(&media_root, fupan_dir, name)?;
            Ok(serde_json::to_string(&json!({"ok":true,"notes":notes}))?)
        }
        "paths" => Ok(serde_json::to_string(
            &crate::paths::storage(project_dir).as_json(),
        )?),
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
                        "ok": failing == 0 && cooling == 0,
                    }),
                );
            }
            if let Ok((pending, failed, sent)) = db::notify_health_summary(pool).await
                && let Some(obj) = diag.as_object_mut()
            {
                let configured = cfg.email_configured() || cfg.push_channels().configured();
                let latest_failed_channel =
                    db::latest_failed_notify_channel(pool).await.ok().flatten();
                obj.insert(
                    "notify_health".to_string(),
                    json!({
                        "pending": pending,
                        "failed": failed,
                        "sent": sent,
                        "configured": configured,
                        "ok": failed == 0 && pending == 0 && configured,
                        "latest_failed_channel": latest_failed_channel,
                        "hint": db::notify_health_hint(
                            pending,
                            failed,
                            configured,
                            latest_failed_channel.as_deref(),
                        ),
                    }),
                );
            }
            Ok(serde_json::to_string(&diag)?)
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
        "service_start" => Ok(serde_json::to_string(&crate::service::ensure_core(
            project_dir,
        )?)?),
        "service_stop" => Ok(serde_json::to_string(&crate::service::stop_rust_daemon(
            project_dir,
        )?)?),
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
        "anchor_search" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if name.trim().is_empty() {
                anyhow::bail!("name 不能为空");
            }
            let result = crate::platform::search_anchor(name, project_dir, cfg).await?;
            Ok(serde_json::to_string(&result)?)
        }
        "check_update" => Ok(serde_json::to_string(&crate::updater::check_update(
            project_dir,
        ))?),
        "apply_update" => Ok(serde_json::to_string(&crate::updater::request_update(
            project_dir,
        )?)?),
        _ => anyhow::bail!("工具未实现: {name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_list_has_unique_names_and_required_args() {
        let tools = tools_list();
        assert_eq!(
            tools.len(),
            20,
            "源码控制面必须暴露 20 个工具（产品手册第 6 章）"
        );
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
            "anchor_search",
            "monitor_add",
            "monitor_remove",
            "monitor_list",
            "monitor_set",
            "check_now",
            "download_video",
            "process_media",
            "notes_list",
            "status",
            "paths",
            "doctor",
            "config_get",
            "config_set",
            "service_start",
            "service_stop",
            "tail_logs",
            "test_email",
            "check_update",
            "apply_update",
        ] {
            assert!(names.contains(expected), "MCP 缺工具: {expected}");
        }
    }

    #[test]
    fn mcp_fupan_dir_honours_configured_output_directory() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-extension-fupan-path-{}-{}",
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
            "[路径设置]\n复盘输出目录 = /tmp/zunzhe-extension-fupan-自定义\n",
        )
        .unwrap();

        assert_eq!(
            mcp_fupan_dir(&dir),
            std::path::PathBuf::from("/tmp/zunzhe-extension-fupan-自定义")
        );

        let _ = std::fs::remove_dir_all(&dir);
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

    #[tokio::test]
    async fn process_media_reports_existing_queue_and_completed_tasks() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-extension-process-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::create_dir_all(dir.join("downloads").join("短视频").join("测试博主")).unwrap();
        std::fs::write(dir.join("config").join("config.ini"), "[录制设置]\n").unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "[视频监控]\n").unwrap();
        std::fs::write(
            dir.join("config").join("config.ini.example"),
            "[录制设置]\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("config").join("fupan_config.ini.example"),
            "[视频监控]\n",
        )
        .unwrap();
        let media = dir
            .join("downloads")
            .join("短视频")
            .join("测试博主")
            .join("x.mp4");
        crate::test_support::write_valid_wav(&media);

        let db_file = std::env::temp_dir().join(format!(
            "zunzhe-extension-process-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = crate::db::open_pool(&db_file).await.unwrap();
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
        let args = json!({
            "file_path": media.display().to_string(),
            "anchor_name": "测试博主",
        });

        let first: Value =
            serde_json::from_str(&call_tool("process_media", &args, &ctx).await.unwrap()).unwrap();
        assert_eq!(first["queued"], true);
        assert_eq!(first["status"], "pending");
        assert_eq!(first["disposition"], "queued");
        let task_id = first["task_id"].as_i64().unwrap();

        let duplicate: Value =
            serde_json::from_str(&call_tool("process_media", &args, &ctx).await.unwrap()).unwrap();
        assert_eq!(duplicate["queued"], false);
        assert_eq!(duplicate["status"], "pending");
        assert_eq!(duplicate["disposition"], "already_active");
        assert_eq!(duplicate["task_id"].as_i64(), Some(task_id));

        crate::db::update_task_status(&pool, task_id, "done")
            .await
            .unwrap();
        let completed: Value =
            serde_json::from_str(&call_tool("process_media", &args, &ctx).await.unwrap()).unwrap();
        assert_eq!(completed["queued"], false);
        assert_eq!(completed["status"], "done");
        assert_eq!(completed["disposition"], "already_done");
        assert_eq!(completed["task_id"].as_i64(), Some(task_id));

        std::mem::forget(db_file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tail_logs_contract_clamps_and_counts() {
        let dir = std::env::temp_dir().join(format!("zunzhe-tail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let logs = crate::paths::logs(&dir);
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("live_monitor.log"), "a\nb\nc\nd\ne\n").unwrap();
        let out = crate::service::tail_logs(&dir, "live_monitor", 3);
        assert_eq!(out["ok"], true);
        assert_eq!(out["content"], "c\nd\ne");
        assert_eq!(out["lines"], 3);
        let out = crate::service::tail_logs(&dir, "live_monitor", 0);
        assert_eq!(out["lines"], 1, "0 行应按至少 1 行处理，不能返回空内容");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tail_logs_accepts_full_file_name_without_double_extension() {
        let dir = std::env::temp_dir().join(format!("zunzhe-tail-full-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let logs = crate::paths::logs(&dir);
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("zunzhe-core.log"), "first\nsecond\n").unwrap();

        let out = crate::service::tail_logs(&dir, "zunzhe-core.log", 1);

        assert_eq!(out["ok"], true);
        assert_eq!(out["content"], "second");
        assert!(
            out["path"]
                .as_str()
                .unwrap_or_default()
                .ends_with("zunzhe-core.log"),
            "完整文件名不能再次追加 .log: {out}"
        );
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
