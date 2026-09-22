# 前端动作与 MCP 覆盖审计（2026-09-21）

本文件由 `node scripts/audit-frontend-actions.js` 生成；工具清单与风险级别来自真实二进制 `initialize + tools/list + control_list`，脚本只读源码，不调用有副作用的工具。

## 摘要

- 真实工具数：43
- 前端直接调用的 CLI 工具：25
- 前端未直接发现 CLI 入口的工具：live_status、live_list、record_live、service_status、video_list、video_check_now、download_video、video_download、video_library、export_training_data、email_setup、config_set_bulk、config_schema_tool、notes_list、doctor、test_notify、audit_list、control_list
- Electron 白名单拒绝的前端命令：无
- 扫描前端文件：7 个

## MCP 与 IPC 覆盖

| 动作 | 层 | 风险 | 前端位置 | 说明 |
|---|---|---|---|---|
| live_status | MCP | read_only | 未发现前端入口 | 查看整体状态：监控、视频库、邮箱 |
| live_list | MCP | read_only | 未发现前端入口 | 列出直播监控项 |
| live_add | MCP | config_write | desktop/src/App.jsx:568 (add-live)<br>desktop/src/App.jsx:743 (add-live)<br>desktop/src/App.jsx:745 (add-live)<br>desktop/src/App.jsx:799 (add-live)<br>desktop/src/App.jsx:817 (add-live) | 添加直播监控 |
| live_remove | MCP | config_write | desktop/src/App.jsx:746 (remove-live)<br>desktop/src/App.jsx:820 (remove-live) | 移除直播监控 |
| live_delete | MCP | config_write | desktop/src/App.jsx:766 (delete-live)<br>desktop/src/App.jsx:834 (delete-live) | 彻底删除直播监控 |
| live_check | MCP | network | desktop/src/App.jsx:801 (check-live) | 检查直播监控项 |
| live_set_notify | MCP | config_write | desktop/src/App.jsx:600 (set-notify) | 设置直播监控通知通道 |
| record_live | MCP | queue_write | 未发现前端入口 | 手动录制一次直播（秒数，0=一直录到结束） |
| service_status | MCP | read_only | 未发现前端入口 | 查看 Python 服务、Rust daemon 与直播录制会话 |
| tail_logs | MCP | read_only | desktop/src/LogsPanel.jsx:34 (tail-logs) | 读取后台服务日志末尾 |
| video_add | MCP | config_write | desktop/src/App.jsx:585 (add-video)<br>desktop/src/App.jsx:749 (add-video)<br>desktop/src/App.jsx:784 (add-video) | 添加博主发视频监控 |
| video_remove | MCP | config_write | desktop/src/App.jsx:769 (remove-video)<br>desktop/src/App.jsx:847 (remove-video) | 移除发视频监控 |
| video_list | MCP | read_only | 未发现前端入口 | 列出发视频监控 |
| video_check_now | MCP | network | 未发现前端入口 | 立即检查一轮新视频 |
| blogger_videos | MCP | network | desktop/src/App.jsx:864 (blogger-videos) | 列出博主历史视频（count=0 表示全部） |
| download_video | MCP | queue_write | 未发现前端入口 | 下载视频到 downloads/短视频/ |
| video_download | MCP | queue_write | 未发现前端入口 | 下载视频到 downloads/短视频/（Python 兼容别名） |
| download_videos | MCP | queue_write | desktop/src/App.jsx:963 (download-videos)<br>desktop/src/App.jsx:982 (download-videos) | 批量下载多个视频（跳过已下载与空 URL） |
| video_library | MCP | read_only | 未发现前端入口 | 查询视频库 |
| status | MCP | read_only | desktop/src/App.jsx:420 (status)<br>desktop/src/App.jsx:489 (status) | 桌面端状态快照（含最近录制、复盘产物与目录） |
| export_training_data | MCP | queue_write | 未发现前端入口 | 导出训练数据 JSONL |
| email_status | MCP | read_only | desktop/src/App.jsx:248 (email-status) | 查看邮箱配置状态 |
| email_setup | MCP | config_write | 未发现前端入口 | 写入 QQ 邮箱配置 |
| app_prefs_get | MCP | read_only | desktop/src/App.jsx:241 (get-app-prefs)<br>desktop/src/App.jsx:486 (get-app-prefs) | 读取桌面应用偏好 |
| app_prefs_set | MCP | config_write | desktop/src/App.jsx:347 (set-app-prefs)<br>desktop/src/Wizard.jsx:16 (set-app-prefs) | 写入桌面应用偏好 |
| config_get | MCP | read_only | desktop/src/App.jsx:238 (get-config)<br>desktop/src/App.jsx:239 (get-config)<br>desktop/src/App.jsx:240 (get-config) | 读取配置 |
| config_set | MCP | config_write | desktop/src/App.jsx:307 (set-config)<br>desktop/src/App.jsx:310 (set-config)<br>desktop/src/App.jsx:313 (set-config)<br>desktop/src/App.jsx:316 (set-config)<br>desktop/src/App.jsx:318 (set-config)<br>desktop/src/App.jsx:320 (set-config)<br>desktop/src/App.jsx:323 (set-config)<br>desktop/src/App.jsx:325 (set-config)<br>desktop/src/App.jsx:327 (set-config)<br>desktop/src/App.jsx:330 (set-config)<br>desktop/src/App.jsx:333 (set-config)<br>desktop/src/App.jsx:345 (set-config)<br>desktop/src/App.jsx:1042 (set-config)<br>desktop/src/Wizard.jsx:14 (set-config) | 写入配置 |
| config_set_bulk | MCP | config_write | 未发现前端入口 | 批量写入配置 |
| config_schema_tool | MCP | read_only | 未发现前端入口 | 配置结构说明 |
| notes_list | MCP | read_only | 未发现前端入口 | 列出复盘产物 |
| process_media | MCP | queue_write | desktop/src/App.jsx:700 (process) | 对音视频跑复盘流水线 |
| service_start | MCP | service_lifecycle | desktop/src/App.jsx:556 (start)<br>desktop/src/App.jsx:997 (start) | 启动 Rust 核心 daemon（统一直播+视频监控） |
| service_stop | MCP | service_lifecycle | desktop/src/App.jsx:997 (stop) | 停止 Rust 核心 daemon |
| ensure_services | MCP | service_lifecycle | desktop/src/Wizard.jsx:26 (ensure-services) | 确保 Rust 核心 daemon 运行（桌面端/watchdog 入口） |
| doctor | MCP | read_only | 未发现前端入口 | 运行环境诊断（日志/磁盘/ffmpeg/守护心跳/监控源健康） |
| wizard_done | MCP | config_write | desktop/src/App.jsx:492 (wizard-done) | 标记桌面向导完成 |
| test_email | MCP | network | desktop/src/App.jsx:369 (test-email) | 发送测试邮件 |
| test_notify | MCP | network | 未发现前端入口 | 发送测试通知（Python 兼容别名） |
| notify_list | MCP | read_only | desktop/src/App.jsx:889 (notify-list)<br>desktop/src/App.jsx:901 (notify-list) | 列出最近通知记录（status 可填 pending/sent/failed/空） |
| notify_retry | MCP | record_delete | desktop/src/App.jsx:914 (notify-retry) | 把失败通知重新放回队列（配置补齐后手动重发） |
| notify_prune | MCP | record_delete | desktop/src/App.jsx:927 (notify-prune)<br>desktop/src/App.jsx:940 (notify-prune) | 清理已发送/已失败的通知记录（limit=0 表示全部） |
| audit_list | MCP | read_only | 未发现前端入口 | 列出最近的 MCP 工具调用审计（参数已脱敏） |
| control_list | MCP | read_only | 未发现前端入口 | 列出所有 MCP 工具、参数与风险级别 |

## 只读证据

- 真实工具名：live_status、live_list、live_add、live_remove、live_delete、live_check、live_set_notify、record_live、service_status、tail_logs、video_add、video_remove、video_list、video_check_now、blogger_videos、download_video、video_download、download_videos、video_library、status、export_training_data、email_status、email_setup、app_prefs_get、app_prefs_set、config_get、config_set、config_set_bulk、config_schema_tool、notes_list、process_media、service_start、service_stop、ensure_services、doctor、wizard_done、test_email、test_notify、notify_list、notify_retry、notify_prune、audit_list、control_list
- 前端 CLI：add-live、add-video、blogger-videos、check-live、delete-live、download-videos、email-status、ensure-services、get-app-prefs、get-config、notify-list、notify-prune、notify-retry、process、remove-live、remove-video、set-app-prefs、set-config、set-notify、start、status、stop、tail-logs、test-email、wizard-done
- 前端 desktop API：biliLogin、checkUpdate、chooseDir、chooseFile、cli、coverUrl、douyinLogin、listDir、openExternal、openPath、readText、reloadUi、setLoginItem、showItem
- Electron CLI 白名单：add-live、remove-live、delete-live、check-live、add-video、remove-video、blogger-videos、download-video、download-videos、check-videos、video-download、process、status、get-config、set-config、get-app-prefs、set-app-prefs、set-notify、email-status、test-email、notify-list、notify-retry、notify-prune、start、stop、wizard-done、ensure-services、tail-logs
