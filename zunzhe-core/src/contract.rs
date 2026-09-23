//! 单一命令契约（Single Command Contract）。
//!
//! 这是 CLI / MCP / 前端三套方言的**唯一事实源**。所有 MCP 工具的
//! `name` / `risk` / `description` / `inputSchema` 都从 [`TOOLS`] 派生。
//!
//! 设计约束（见 `产品手册.md` 第 8 章 / `CONTRACT.md` 防回归规则）：
//! - 禁止在 `extension.rs` 或 `mcp_audit.rs` 里再手写一份工具定义；
//!   它们只能调用本模块的 `tools_list()` / `risk_for()`。
//! - 工具数量（18）由 `control_plane_contract.rs` 锁定，增减工具必须同步改这里。
//! - 2026-09-23：按 `产品手册.md` 第 6 章完成 P3 收敛（43 → 18），
//!   旧工具（live_* / video_* / 别名 / 审计 / 通知治理）已删除。

/// 单个输入参数定义。
#[derive(Debug, Clone, Copy)]
pub struct ParamSpec {
    pub name: &'static str,
    pub ty: &'static str,
    pub required: bool,
}

/// 单个 MCP 工具的契约定义。
#[derive(Debug, Clone, Copy)]
pub struct ToolSpec {
    pub name: &'static str,
    pub risk: &'static str,
    pub description: &'static str,
    pub params: &'static [ParamSpec],
}

macro_rules! t {
    ($name:literal, $risk:literal, $desc:literal) => {
        ToolSpec {
            name: $name,
            risk: $risk,
            description: $desc,
            params: &[],
        }
    };
    ($name:literal, $risk:literal, $desc:literal, $(($pn:literal, $pt:literal, $pr:literal)),+ $(,)?) => {
        ToolSpec {
            name: $name,
            risk: $risk,
            description: $desc,
            params: &[ $(ParamSpec { name: $pn, ty: $pt, required: $pr }),+ ],
        }
    };
}

/// MCP 全部工具（18 个，与 `control_plane_contract.rs` 锁点一致）。
pub const TOOLS: &[ToolSpec] = &[
    t!("anchor_search", "network", "按名字搜索主播，返回候选（平台/账号URL/昵称/头像），智能体拿 URL 去 monitor_add", ("name", "string", true)),
    t!("monitor_add", "config_write", "添加监控（直播/短视频按 URL 自动识别；主播主页同时挂两种）", ("url", "string", true), ("name", "string", false)),
    t!("monitor_remove", "config_write", "移除监控", ("url", "string", true)),
    t!("monitor_list", "read_only", "列出全部监控源（直播+短视频一张表）"),
    t!("monitor_set", "config_write", "改单个源的开关/通知", ("url", "string", true), ("key", "string", true), ("value", "string", true)),
    t!("check_now", "network", "立刻全量检查一轮（直播+短视频）"),
    t!("download_video", "queue_write", "下载视频到主播目录", ("url", "string", true), ("name", "string", false)),
    t!("process_media", "queue_write", "对音视频跑转写流水线（必须传主播名）", ("file_path", "string", true), ("anchor_name", "string", false)),
    t!("notes_list", "read_only", "列出转写产物（含绝对路径），智能体找文件就靠它", ("name", "string", false)),
    t!("status", "read_only", "整体状态快照（监控/视频库/邮箱/守护）"),
    t!("paths", "read_only", "输出全部路径（只读，不建目录）"),
    t!("doctor", "read_only", "环境诊断（日志/磁盘/ffmpeg/守护心跳/监控源健康）"),
    t!("config_get", "read_only", "读取配置", ("file", "string", false), ("section", "string", false), ("key", "string", false)),
    t!("config_set", "config_write", "写入配置", ("file", "string", true), ("section", "string", true), ("key", "string", true), ("value", "string", true)),
    t!("service_start", "service_lifecycle", "启动 Rust 核心 daemon（统一直播+视频监控）"),
    t!("service_stop", "service_lifecycle", "停止 Rust 核心 daemon"),
    t!("tail_logs", "read_only", "读取后台日志末尾", ("name", "string", false), ("lines", "number", false)),
    t!("test_email", "network", "发送测试邮件"),
];

/// 从契约派生 MCP `tools/list` 的返回体（与旧手写实现逐字段一致）。
pub fn tools_list() -> Vec<serde_json::Value> {
    use serde_json::json;
    TOOLS
        .iter()
        .map(|tool| {
            let properties: serde_json::Map<String, serde_json::Value> = tool
                .params
                .iter()
                .map(|p| (p.name.to_string(), json!({ "type": p.ty })))
                .collect();
            let required: Vec<&str> = tool.params.iter().filter(|p| p.required).map(|p| p.name).collect();
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": {
                    "type": "object",
                    "properties": properties,
                    "required": required,
                }
            })
        })
        .collect()
}

/// 从契约派生工具风险级别（与旧 `mcp_audit::risk_for` 完全一致）。
pub fn risk_for(tool: &str) -> &'static str {
    TOOLS
        .iter()
        .find(|tool_spec| tool_spec.name == tool)
        .map(|tool_spec| tool_spec.risk)
        .unwrap_or("unknown")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_has_18_tools() {
        assert_eq!(TOOLS.len(), 18, "MCP 工具数必须与契约锁点一致（产品手册第 6 章）");
    }

    #[test]
    fn contract_names_unique() {
        let mut seen = std::collections::HashSet::new();
        for tool in TOOLS {
            assert!(seen.insert(tool.name), "重复工具名: {}", tool.name);
        }
    }

    #[test]
    fn contract_risk_classes_valid() {
        for tool in TOOLS {
            assert!(
                matches!(
                    tool.risk,
                    "read_only"
                        | "network"
                        | "config_write"
                        | "queue_write"
                        | "service_lifecycle"
                        | "record_delete"
                ),
                "非法风险级: {} = {}",
                tool.name,
                tool.risk
            );
        }
    }

    #[test]
    fn contract_derives_mcp_shape() {
        let list = tools_list();
        assert_eq!(list.len(), 18);
        let monitor_add = list.iter().find(|v| v["name"] == "monitor_add").unwrap();
        assert_eq!(monitor_add["inputSchema"]["required"][0], "url");
        assert_eq!(monitor_add["inputSchema"]["properties"]["name"]["type"], "string");
        assert_eq!(risk_for("monitor_add"), "config_write");
        assert_eq!(risk_for("process_media"), "queue_write");
        assert_eq!(risk_for("doctor"), "read_only");
        assert_eq!(risk_for("anchor_search"), "network");
    }

    #[test]
    fn contract_has_no_legacy_tools() {
        for tool in TOOLS {
            assert!(
                !tool.name.starts_with("live_")
                    && !tool.name.starts_with("video_")
                    && !tool.name.starts_with("app_prefs")
                    && !tool.name.starts_with("notify_")
                    && !tool.name.starts_with("audit_")
                    && !tool.name.starts_with("control_"),
                "旧工具混入契约: {}",
                tool.name
            );
        }
    }
}
