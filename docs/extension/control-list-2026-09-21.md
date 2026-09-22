# MCP 控制面实录（2026-09-21）

本文件记录尊者直播监控 Rust extension 的真实 MCP 控制面。数据来源不是源码猜测，而是当前 release 的 `initialize + tools/list` 实测结果；结构化原始清单见同目录 `control-list-2026-09-21.json`。

## 结论

- 真实工具数：43。
- 风险分布：`read_only=16`、`config_write=11`、`queue_write=6`、`network=5`、`service_lifecycle=3`、`record_delete=2`。
- 只读工具已实际调用并返回真实数据，包括 `live_status`、`video_library`、`status`、`email_status`、`config_schema_tool`、`notes_list`、`notify_list`、`audit_list`、`control_list`、`doctor`、`tail_logs`。
- `blogger_videos` 实测可联网并返回 37 条 B站历史视频，耗时约 38.7 秒。
- 危险工具本轮没有执行：`record_live`、`service_start`、`service_stop`、`ensure_services`、`download_video`、`download_videos`、`video_download`、`email_setup`、`test_email`、`test_notify`、配置写入和删除类工具。

## 实测发现并修复

### tail_logs 双后缀

旧实现直接执行 `logs_dir.join(format!("{name}.log"))`。调用方传 `zunzhe-core.log` 时实际查找 `zunzhe-core.log.log`，因此返回“暂无日志”。这不仅影响 MCP，也影响桌面端日志入口。

修复后：

- 文件名已经以 `.log` 结尾时不再重复追加。
- 当前配置日志目录不存在该文件时，回退旧标准目录 `~/Library/Logs/zunzhe/`，只读历史日志，不改变写入路径。
- 已用新构建的真实 stdio 调用验证，返回生产日志内容和真实路径 `/Users/zhangxuetao/Library/Logs/zunzhe/zunzhe-core.log`。

回归测试：`tail_logs_accepts_full_file_name_without_double_extension`。

## 工具清单

### 只读（16）

`live_status`、`live_list`、`service_status`、`video_list`、`video_library`、`status`、`email_status`、`app_prefs_get`、`config_get`、`config_schema_tool`、`notes_list`、`notify_list`、`audit_list`、`control_list`、`doctor`、`tail_logs`

### 配置写入（11）

`live_add`、`live_remove`、`live_delete`、`live_set_notify`、`video_add`、`video_remove`、`email_setup`、`app_prefs_set`、`config_set`、`config_set_bulk`、`wizard_done`

### 入队 / 触发任务（6）

`live_check`、`video_check_now`、`download_video`、`video_download`、`download_videos`、`process_media`

### 联网读取（5）

`blogger_videos`、`export_training_data`、`test_email`、`test_notify`、`notify_retry`

### 服务生命周期（3）

`service_start`、`service_stop`、`ensure_services`

### 录制 / 删除（2）

`record_live`、`notify_prune`

## 参数边界

- `process_media.file_path` 必须是可信媒体目录下的真实文件；不存在、目录、任意路径都会在入队前拒绝，不会产生假任务。
- `process_media` 对 pending / processing / done 去重；failed 可按用户动作重新入队。
- `tail_logs.name` 可传 `zunzhe-core.log`，也可传 `live_monitor`；`lines` 被限制在 1..2000。
- `config_set` 必须同时提供 `file`、`section`、`key`、`value`。
- `download_videos.items_json` 是 JSON 字符串，不是数组对象。

## 未完成验证

- 需要真实平台风控恢复、Cookie 和邮箱凭据，才能端到端验证 B站/抖音/快手源、邮件通知。
- 需要用户显式授权，才能执行 `record_live`、`service_stop`、`ensure_services` 等会改变运行现场的工具。
- `audit_list` 当前会记录每次 extension 调用；本轮只读调用已经进入审计表，危险工具保持未调用。
