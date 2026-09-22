# 前端动作矩阵（2026-09-21）

这份矩阵把“前端按钮 → IPC/CLI → extension 工具 → 副作用 → 存储位置 → 实跑状态”放在一张表里。实跑状态只引用可追溯证据，不把“代码已接线”写成“端到端已验证”。

## 基线

- Rust extension `tools/list`：43 个工具。
- 前端唯一 CLI 命令：22 个。
- Electron CLI 白名单：28 个。
- 控制面来源：`.神经/控制面.json`。

## 矩阵

| 按钮/区域 | 前端实现 | IPC / CLI | extension 工具 | 真实副作用 | 存储位置 | 实跑状态 |
|---|---|---|---|---|---|---|
| 添加主播 / 主播设置保存 | desktop/src/App.jsx: onAddLive / saveItemSettings | cli('add-live')<br>cli('remove-live') 仅关闭监控 | live_add | 写直播监控列表<br>可能启动 daemon | config/URL_config.ini<br>status.paths.logs<br>status.paths.database | 部分实跑：加主播和移除监控已有真实使用；本轮未改动生产列表 |
| 添加博主 / 博主设置保存 | desktop/src/App.jsx: onAddVideo / saveItemSettings | cli('add-video') | video_add | 写视频监控列表 | config/视频监控列表.ini<br>config/监控偏好.json | 部分实跑：添加/开关自动下载已有真实使用；B站新视频端到端仍受平台风控影响 |
| 下载选中 / 下载全部 | desktop/src/App.jsx: downloadSelected / downloadAll | cli('download-videos', items_json) | download_videos | 写下载任务队列<br>联网下载<br>写视频文件 | status.paths.database<br>status.paths.downloads/短视频/<博主>/ | 已做前端隔离测试；下载任务真实落点显示已改为 status.paths.downloads，未在本轮下载生产视频 |
| 转写这个视频 / 重新转写 | desktop/src/App.jsx: queueTranscription | cli('process', file_path, anchor_name) | process_media | 写 process 任务队列<br>启动 Python 复盘流水线<br>写转写产物 | status.paths.database<br>status.paths.fupan_output | 真实历史证据：任务 513 已生成约 206,705 字节 .转写.txt 与 449,603 字节 .转写.json；项目根旧库最终为 pending、attempts=3，日志证实 2026-09-20 22:18:04 已超时结束本轮；当前 daemon 已切到外置库 |
| 启动服务 / 停止服务 | desktop/src/App.jsx: toggleService | cli('start')<br>cli('stop') | service_start<br>service_stop | 启动/停止 Rust daemon | status.paths.run<br>status.paths.logs | 未在本轮执行；停止/启动生产 daemon 需要明确授权 |
| 运行日志 / 打开日志目录 | desktop/src/LogsPanel.jsx | cli('tail-logs', name, lines)<br>openPath(logs_dir) | tail_logs | 只读日志 | status.paths.logs | 只读路径已完成隔离测试；生产日志双份问题仍按 CONTRACT.md 记录，未在本轮清理 |
| 保存配置 / 测试通知 | desktop/src/App.jsx: saveEmailSettings / testEmail | cli('set-config')<br>cli('set-app-prefs')<br>cli('test-email')<br>cli('email-status') | config_set<br>app_prefs_set<br>test_email<br>email_status | 写配置<br>发送测试邮件 | config/config.ini<br>config/fupan_config.ini<br>config/桌面偏好.json | 测试邮件未端到端成功；当前仍是占位邮箱凭据 |
| 选择存放位置 / 换个目录 / 打开产物目录 | desktop/src/App.jsx: pickStashDir / pickDatabaseFile / changeFupanDir / openPath | chooseDir<br>chooseFile<br>cli('set-config')<br>openPath<br>listDir | config_get<br>config_set<br>status | 保存路径配置<br>只读打开目录 | config/config.ini [路径设置]<br>status.paths.* | 路径解析有 Rust/desktop 隔离测试；当前 daemon PID 67494 实际打开的是外置库 /Volumes/A区/尊者产物/database/zunzhe-core.db；项目根旧库仅保留迁移前任务 512/513 |
| B站授权 / 抖音授权 | desktop/src/App.jsx: biliLogin / douyinLogin | biliLogin<br>douyinLogin | 无直接 extension 工具 | 打开登录窗口<br>写 cookie 凭据 | status.paths.run（cookie/凭据目录，具体以 path-utils 为准） | 未在本轮重新登录；B站/抖音凭据仍是端到端前置条件 |
| 通知治理 / 重试 / 清理 | desktop/src/App.jsx: openNotifyPanel / retryFailedNotify / pruneFailedNotify / pruneSentNotify | cli('notify-list')<br>cli('notify-retry')<br>cli('notify-prune') | notify_list<br>notify_retry<br>notify_prune | 读通知记录<br>重新入队<br>删除通知记录 | status.paths.database | 只读列表已有真实数据；重试/清理未在本轮执行 |

## 说明

- “已实跑”只指已有日志、数据库、文件或测试证据支持的动作。
- 生产 daemon 启停、数据库合并、launchd 重载、真实邮件发送和重新登录授权都没有在本轮执行。
- 下载完成提示和设置页文案已改为使用 `status.paths.downloads`，避免产物根换到外置盘后仍显示相对路径。
- 当前外置库的 B站 `monitor_baseline` 已实测跨轮 `0 -> 1 -> 2`：`2026-09-21 06:19:03` 两条源均写 1，`06:35:04` 均累加到 2，日志同步记录“失败冷却计数: 2”。这证明计数不再永远停在 1；第 3 次进入冷却和实际跳过仍需下一轮验证。
