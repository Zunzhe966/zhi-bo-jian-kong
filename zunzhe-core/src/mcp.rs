use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use serde_json::{Value, json};
use sqlx::sqlite::SqlitePool;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::error;

use crate::config::Config;
use crate::download::Downloader;
use crate::platform;
use crate::training_data;
use crate::video_library::VideoLibrary;

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
                match call_tool(name, &args, &library, &downloader, &fupan_dir, &cfg, &pool).await {
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

async fn write_response(writer: &mut tokio::io::Stdout, value: Value) -> std::io::Result<()> {
    let mut line = serde_json::to_string(&value)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await
}

fn tools_list() -> Vec<Value> {
    vec![
        json!({"name":"live_status","description":"查看整体状态：监控、视频库、邮箱","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"blogger_videos","description":"列出博主历史视频","inputSchema":{"type":"object","properties":{"url":{"type":"string"},"count":{"type":"number"}},"required":["url"]}}),
        json!({"name":"download_video","description":"下载视频到 downloads/短视频/","inputSchema":{"type":"object","properties":{"url":{"type":"string"},"name":{"type":"string"}},"required":["url"]}}),
        json!({"name":"video_library","description":"查询视频库","inputSchema":{"type":"object","properties":{"source_url":{"type":"string"},"downloaded_only":{"type":"boolean"}}}}),
        json!({"name":"export_training_data","description":"导出训练数据 JSONL","inputSchema":{"type":"object","properties":{"anchor":{"type":"string"},"output":{"type":"string"}}}}),
        json!({"name":"email_status","description":"查看邮箱配置状态","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"test_email","description":"发送测试邮件","inputSchema":{"type":"object","properties":{}}}),
    ]
}

async fn call_tool(
    name: &str,
    args: &Value,
    library: &VideoLibrary,
    downloader: &Downloader,
    fupan_dir: &Path,
    cfg: &Config,
    pool: &SqlitePool,
) -> Result<String> {
    match name {
        "live_status" => {
            let videos = library.list("", "", false).await?;
            Ok(serde_json::to_string(
                &json!({"ok":true,"video_count":videos.len(),"email":cfg.email_status()}),
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
                Box::new(platform::BilibiliAdapter::with_cookie(cookie))
            } else {
                Box::new(platform::RssAdapter::new())
            };
            let videos = adapter.list_videos(&url, count).await?;
            for v in &videos {
                let _ = library.upsert(v, "", &url).await;
            }
            Ok(serde_json::to_string(
                &json!({"ok":true,"platform":adapter.name(),"count":videos.len(),"videos":videos}),
            )?)
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
                &json!({"ok":true,"count":videos.len(),"videos":videos}),
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
        _ => anyhow::bail!("工具未实现: {name}"),
    }
}
