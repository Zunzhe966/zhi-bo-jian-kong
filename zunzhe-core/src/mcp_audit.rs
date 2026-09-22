//! MCP tool-call audit log.
//!
//! The extension server is an external control plane: agents can mutate
//! configuration, monitors, queues, and services.  Tracing alone is not an
//! audit trail because it is unstructured and does not survive log rotation in
//! a queryable form.  This module keeps a small, redacted record in SQLite.

use std::time::Instant;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use sqlx::sqlite::SqlitePool;

const ERROR_LIMIT: usize = 2000;
const SUMMARY_LIMIT: usize = 4000;

/// Persisted MCP call record.  `arguments_summary` is always redacted before
/// it reaches SQLite; callers must not bypass [`record_call`] with raw args.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpCall {
    pub id: i64,
    pub tool: String,
    pub risk: String,
    pub arguments_summary: String,
    pub success: bool,
    pub error: String,
    pub duration_ms: i64,
    pub created_at: String,
}

/// Return the stable risk class for a tool.
///
/// Keep this classification in one place.  Protocol discovery and audit
/// display both consume it, so a new tool cannot silently appear as "read only"
/// merely because its caller forgot to annotate it.
pub fn risk_for(tool: &str) -> &'static str {
    match tool {
        "live_status" | "live_list" | "service_status" | "status" | "tail_logs" | "video_list"
        | "video_library" | "email_status" | "app_prefs_get" | "config_get"
        | "config_schema_tool" | "notes_list" | "doctor" | "notify_list" | "audit_list"
        | "control_list" => "read_only",
        "live_check" | "video_check_now" | "blogger_videos" | "test_email" | "test_notify" => {
            "network"
        }
        "live_add" | "live_remove" | "live_delete" | "live_set_notify" | "video_add"
        | "video_remove" | "email_setup" | "app_prefs_set" | "config_set" | "config_set_bulk"
        | "wizard_done" => "config_write",
        "record_live"
        | "download_video"
        | "video_download"
        | "download_videos"
        | "process_media"
        | "export_training_data" => "queue_write",
        "service_start" | "ensure_services" | "service_stop" => "service_lifecycle",
        "notify_retry" | "notify_prune" => "record_delete",
        _ => "unknown",
    }
}

/// Redact credentials before persisting or returning an argument summary.
///
/// This intentionally errs on the side of redaction: an unknown nested key that
/// looks like a secret is still sensitive.  JSON shape is preserved so audit
/// readers can see which argument carried the value without seeing the value.
pub fn redact_arguments(value: &Value) -> Value {
    redact_arguments_with_context(value)
}

fn redact_arguments_with_context(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let secret_target = ["key", "name", "字段", "键名"].iter().find_map(|lookup| {
                map.get(*lookup)
                    .and_then(Value::as_str)
                    .filter(|text| is_secret_key(text))
            });
            Value::Object(
                map.iter()
                    .map(|(key, value)| {
                        let redacted = if is_secret_key(key)
                            || secret_target.is_some()
                                && matches!(key.to_ascii_lowercase().as_str(), "value" | "值")
                        {
                            Value::String("***".to_string())
                        } else {
                            redact_arguments_with_context(value)
                        };
                        (key.clone(), redacted)
                    })
                    .collect(),
            )
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(redact_arguments_with_context).collect())
        }
        other => other.clone(),
    }
}

fn is_secret_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    [
        "auth",
        "authorization",
        "cookie",
        "credential",
        "password",
        "passwd",
        "secret",
        "token",
        "apikey",
        "api_key",
        "密钥",
        "密码",
        "授权码",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn summarize(arguments: &Value) -> String {
    let redacted = redact_arguments(arguments);
    let text = serde_json::to_string(&redacted).unwrap_or_else(|_| "{}".to_string());
    text.chars().take(SUMMARY_LIMIT).collect()
}

/// Record one completed MCP tool call.
///
/// The caller supplies elapsed time rather than this function reading the
/// clock, which keeps the persistence boundary testable and avoids timing the
/// audit write itself.
pub async fn record_call(
    pool: &SqlitePool,
    tool: &str,
    arguments: &Value,
    success: bool,
    error: &str,
    elapsed: std::time::Duration,
) -> anyhow::Result<i64> {
    let duration_ms = i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX);
    let result = sqlx::query(
        "INSERT INTO mcp_calls
         (tool, risk, arguments_summary, success, error, duration_ms)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(tool)
    .bind(risk_for(tool))
    .bind(summarize(arguments))
    .bind(if success { 1 } else { 0 })
    .bind(error.chars().take(ERROR_LIMIT).collect::<String>())
    .bind(duration_ms)
    .execute(pool)
    .await
    .context("写入 MCP 调用审计")?;
    Ok(result.last_insert_rowid())
}

/// Record a completed call without ever turning audit failure into tool failure.
///
/// The extension server must preserve the original tool result even if the
/// audit database is temporarily locked or unavailable.  Logging the audit
/// failure is the caller's responsibility; this helper centralizes the
/// "best effort" rule so every call site behaves the same.
pub async fn record_call_best_effort(
    pool: &SqlitePool,
    tool: &str,
    arguments: &Value,
    success: bool,
    error: &str,
    elapsed: std::time::Duration,
) {
    if let Err(audit_error) = record_call(pool, tool, arguments, success, error, elapsed).await {
        tracing::error!("extension 审计写入失败（保留原工具结果）: {audit_error:#}");
    }
}

/// List recent audit records, newest first.
pub async fn list_calls(pool: &SqlitePool, limit: i64) -> anyhow::Result<Vec<McpCall>> {
    let limit = limit.clamp(1, 1000);
    let rows = sqlx::query(
        "SELECT id, tool, risk, arguments_summary, success, error, duration_ms, created_at
         FROM mcp_calls ORDER BY id DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .context("查询 MCP 调用审计")?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(McpCall {
            id: row.try_get("id")?,
            tool: row.try_get("tool")?,
            risk: row.try_get("risk")?,
            arguments_summary: row.try_get("arguments_summary")?,
            success: row.try_get::<i64, _>("success")? != 0,
            error: row.try_get("error")?,
            duration_ms: row.try_get("duration_ms")?,
            created_at: row.try_get("created_at")?,
        });
    }
    Ok(out)
}

/// Build the JSON payload for the `audit_list` tool.
///
/// Keeping this in the stable audit module means the extension entry only has to
/// dispatch to it; redaction and query semantics are covered by the module's
/// own tests even if the large extension source is being read under load.
pub async fn list_calls_json(pool: &SqlitePool, limit: i64) -> anyhow::Result<Value> {
    let calls = list_calls(pool, limit).await?;
    Ok(serde_json::json!({
        "ok": true,
        "count": calls.len(),
        "calls": calls,
    }))
}

/// 根据已经暴露的工具定义生成控制面清单。
///
/// `control_list` 必须复用 `tools_list()` 和 `risk_for()`，不能在另一个地方
/// 手写一份工具数组；否则新增工具时会出现“能调用但控制面看不见”或风险
/// 级别漂移。这里只做投影，不执行任何工具。
pub fn controls_json(tools: &[Value]) -> Value {
    let controls: Vec<Value> = tools
        .iter()
        .filter_map(|tool| {
            let name = tool.get("name").and_then(Value::as_str)?;
            Some(serde_json::json!({
                "name": name,
                "description": tool.get("description").and_then(Value::as_str).unwrap_or(""),
                "inputSchema": tool.get("inputSchema").cloned().unwrap_or_else(|| serde_json::json!({
                    "type": "object",
                    "properties": {}
                })),
                "risk": risk_for(name),
            }))
        })
        .collect();
    let count = controls.len();
    serde_json::json!({
        "ok": true,
        "count": count,
        "controls": controls,
    })
}

/// Measure a synchronous section and return both its result and duration.
pub fn timed<T>(f: impl FnOnce() -> T) -> (T, std::time::Duration) {
    let start = Instant::now();
    let result = f();
    (result, start.elapsed())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::db::init_db(&pool).await.unwrap();
        pool
    }

    #[test]
    fn redacts_secret_keys_recursively_but_keeps_shape() {
        let value = json!({
            "url": "https://example.test/video",
            "email": {
                "auth": "smtp-secret",
                "password": "pw",
                "to": "user@example.test"
            },
            "items": [{"token": "abc"}, {"name": "safe"}]
        });
        let redacted = redact_arguments(&value);
        assert_eq!(redacted["url"], value["url"]);
        assert_eq!(redacted["email"]["auth"], "***");
        assert_eq!(redacted["email"]["password"], "***");
        assert_eq!(redacted["email"]["to"], value["email"]["to"]);
        assert_eq!(redacted["items"][0]["token"], "***");
        assert_eq!(redacted["items"][1]["name"], "safe");
    }

    #[tokio::test]
    async fn records_redacted_call_and_lists_newest_first() {
        let pool = test_pool().await;
        let first = record_call(
            &pool,
            "config_set",
            &json!({"key": "auth", "value": "top-secret"}),
            true,
            "",
            std::time::Duration::from_millis(12),
        )
        .await
        .unwrap();
        let second = record_call(
            &pool,
            "doctor",
            &json!({}),
            false,
            "boom",
            std::time::Duration::from_millis(34),
        )
        .await
        .unwrap();
        let calls = list_calls(&pool, 10).await.unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, second);
        assert!(!calls[0].success);
        assert_eq!(calls[0].error, "boom");
        assert_eq!(calls[1].id, first);
        assert_eq!(calls[1].risk, "config_write");
        assert!(calls[1].arguments_summary.contains("\"value\":\"***\""));
        assert!(!calls[1].arguments_summary.contains("top-secret"));
    }

    #[tokio::test]
    async fn json_payload_contains_no_secret_argument_values() {
        let pool = test_pool().await;
        record_call(
            &pool,
            "email_setup",
            &json!({"email": "me@example.test", "auth": "mail-secret"}),
            true,
            "",
            std::time::Duration::from_millis(3),
        )
        .await
        .unwrap();
        let payload = list_calls_json(&pool, 10).await.unwrap();
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["count"], 1);
        let text = payload.to_string();
        assert!(text.contains("me@example.test"));
        assert!(!text.contains("mail-secret"));
    }

    #[test]
    fn controls_json_projects_tool_metadata_and_risk() {
        let tools = vec![
            json!({
                "name": "control_list",
                "description": "list controls",
                "inputSchema": {"type": "object", "properties": {}}
            }),
            json!({
                "name": "doctor",
                "description": "diagnose",
                "inputSchema": {"type": "object", "properties": {}}
            }),
            json!({"name": "future_tool"}),
        ];

        let payload = controls_json(&tools);

        assert_eq!(payload["ok"], true);
        assert_eq!(payload["count"], 3);
        assert_eq!(payload["controls"][0]["name"], "control_list");
        assert_eq!(payload["controls"][0]["risk"], "read_only");
        assert_eq!(payload["controls"][1]["risk"], "read_only");
        assert_eq!(payload["controls"][2]["risk"], "unknown");
        assert_eq!(payload["controls"][2]["description"], "");
        assert_eq!(payload["controls"][2]["inputSchema"]["type"], "object");
    }
}
