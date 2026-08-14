# Graph Report - .  (2026-08-14)

## Corpus Check
- cluster-only mode — file stats not available

## Summary
- 1768 nodes · 4058 edges · 92 communities (73 shown, 19 thin omitted)
- Extraction: 98% EXTRACTED · 2% INFERRED · 0% AMBIGUOUS · INFERRED: 64 edges (avg confidence: 0.65)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `1c939ac4`
- Run `git rev-parse HEAD` and compare to check if the graph is stale.
- Run `graphify update .` after code changes (no API cost).

## Community Hubs (Navigation)
- spider.py
- platform.rs
- live_monitor.rs
- db.rs
- 尊者直播监控 Rust 重构 — 目标模式（北极星）
- process.rs
- notify.py
- cli.py
- model.rs
- service.rs
- notify.rs
- Watchlist
- mcp.rs
- download.rs
- mcp_server.py
- scheduler.rs
- video_monitor.py
- config.rs
- Config
- main.cjs
- main.py
- App.jsx
- pipeline_bridge.rs
- 三、功能需求清单
- crypto-js.min.js
- video_library.py
- ab_sign.py
- bilibili_electron.rs
- 直播复盘助手 / LiveReview
- extract-run.cjs
- local-notify.test.cjs
- ffmpeg_install.py
- push_message
- status_snapshot.rs
- config_api.py
- devDependencies
- .try_acquire
- bilibili-cookie-loader.test.cjs
- build
- liveme.js
- ProxyDetector
- initializer.py
- _bili_test.cjs
- README.md
- x-bogus.js
- app_prefs.rs
- healthcheck.test.cjs
- 尊者直播监控 — 项目目标（上下文恢复锚点）
- dependencies
- bili-video-list.cjs
- bili_browser.py
- scripts
- make-icons.cjs
- 尊者直播监控 — 项目图谱梳理
- local-notify.cjs
- createWindow
- export_training_data
- package.json
- mac
- PULL_REQUEST_TEMPLATE.md
- 安全审查报告 —— DouyinLiveRecorder（直播复盘助手 基础项目）
- _resolve_media
- import_legacy_account
- start-rust-daemon.sh
- sync_req
- laixiu.js
- 六、当前进度
- read_config_value
- setup-macos.sh
- PushChannels
- main
- check-rust-daemon.sh script
- DouyinLiveRecorder
- Box
- Client
- Default
- Future
- Into
- Output
- Pin
- Self
- Send
- String
- Sync
- Value
- Drop

## God Nodes (most connected - your core abstractions)
1. `尊者直播监控 Rust 重构 — 目标模式（北极星）` - 79 edges
2. `async_req()` - 77 edges
3. `start_record()` - 76 edges
4. `trace_error_decorator()` - 75 edges
5. `main()` - 35 edges
6. `Config` - 30 edges
7. `BilibiliAdapter` - 25 edges
8. `Watchlist` - 23 edges
9. `_json()` - 22 edges
10. `get()` - 20 edges

## Surprising Connections (you probably didn't know these)
- `consumeNotifyQueue()` --indirect_call--> `e()`  [INFERRED]
  desktop/electron/local-notify.cjs → src/javascript/crypto-js.min.js
- `check_ffmpeg_existence()` --calls--> `check_ffmpeg()`  [EXTRACTED]
  main.py → ffmpeg_install.py
- `_resolve_youtube()` --calls--> `get_youtube_stream_url()`  [EXTRACTED]
  fupan/live_resolve.py → src/spider.py
- `_resolve_twitch()` --calls--> `get_twitchtv_stream_data()`  [EXTRACTED]
  fupan/live_resolve.py → src/spider.py
- `push_message()` --calls--> `get_notify()`  [EXTRACTED]
  main.py → fupan/monitor_prefs.py

## Import Cycles
- None detected.

## Communities (92 total, 19 thin omitted)

### Community 0 - "spider.py"
Cohesion: 0.05
Nodes (137): callable, Exception, _empty(), _ensure_src_on_path(), main(), _project_root(), Path, resolve() (+129 more)

### Community 1 - "platform.rs"
Cohesion: 0.06
Nodes (67): Box, Client, Default, Future, HeaderMap, Into, Node, Output (+59 more)

### Community 2 - "live_monitor.rs"
Cohesion: 0.07
Nodes (53): adapter_name(), BilibiliLiveAdapter, clean_name(), DirectLiveAdapter, disk_threshold_parses_or_defaults_to_one_gb(), failed_live_check_safely_converges_stale_state(), fetch_bilibili_anchor_name(), fetch_bilibili_room_title() (+45 more)

### Community 3 - "db.rs"
Cohesion: 0.08
Nodes (78): Anchor, LiveSession, NotifyEvent, SqliteConnection, SqliteConnectOptions, Task, Video, active_live_session() (+70 more)

### Community 4 - "尊者直播监控 Rust 重构 — 目标模式（北极星）"
Cohesion: 0.03
Nodes (78): 一、最终目标（验收标准）, 七、B站视频监控 WBI 签名修复（最新）, 七十、任务自愈按 kind 隔离（最新）, 七十一、Python 直播桥接错误不再伪装成“下播”（最新）, 七十七、Windows tasklist CSV 解析修正与本地通知并发加固（最新）, 七十三、录制收尾并发排空 ffmpeg 管道（最新）, 七十二、Windows 桌面看门狗跨平台修复（最新）, 七十五、Windows tasklist 解析统一为 CSV 机器输出（最新） (+70 more)

### Community 5 - "process.rs"
Cohesion: 0.06
Nodes (41): Display, Error, ExitStatus, Formatter, Cli, Command, Option, PathBuf (+33 more)

### Community 6 - "notify.py"
Cohesion: 0.09
Nodes (48): ConfigParser, cut_clips(), _safe_name(), _chat(), _client(), distill_transcript(), _parse_json(), 输入 transcribe 的结果，输出 知识点.json 和 复盘笔记.md。 (+40 more)

### Community 7 - "cli.py"
Cohesion: 0.09
Nodes (54): get_prefs(), _load(), mark_wizard_done(), _save(), set_prefs(), add_live(), add_video(), check_live() (+46 more)

### Community 8 - "model.rs"
Cohesion: 0.08
Nodes (43): Anchor, KnowledgePoint, LiveSession, NotifyEvent, String, Vec, Task, TrainingRecord (+35 more)

### Community 9 - "service.rs"
Cohesion: 0.12
Nodes (49): cleanup_rust_recordings(), core_binary_name(), core_binary_name_is_platform_specific(), daemon_uptime(), daemon_uptime_parses_started_line(), daemon_uptime_public(), diagnostics(), diagnostics_detects_windows_bundled_ffmpeg() (+41 more)

### Community 10 - "notify.rs"
Cohesion: 0.10
Nodes (31): Mailbox, blocking_smtp_call_is_cancelled_by_timeout(), Channel, channel_name_is_serialized_in_record_sent(), flush_pending_retries_oldest_first(), flush_pending_updates_existing_record_without_duplicate(), local_notify_can_be_disabled_in_prefs(), local_notify_ids_are_unique_within_same_millisecond() (+23 more)

### Community 11 - "Watchlist"
Cohesion: 0.14
Nodes (31): add_video_replaces_by_url_when_name_contains_comma(), contains_url(), format_live_line(), format_video_line(), is_quality(), live_and_video_roundtrip(), LiveItem, normalize_url() (+23 more)

### Community 12 - "mcp.rs"
Cohesion: 0.09
Nodes (26): Arc, Stdout, ConfigApi, patch_matches_existing_key_case_insensitively(), patch_preserves_comments_and_order(), PathBuf, Result, Self (+18 more)

### Community 13 - "download.rs"
Cohesion: 0.13
Nodes (33): bilibili_browser_download(), clean_name(), download_worker_marks_stuck_task_failed(), Downloader, DownloadResult, drain_pending(), enqueue_pipeline_after_download(), enqueue_process() (+25 more)

### Community 14 - "mcp_server.py"
Cohesion: 0.11
Nodes (36): config_schema(), blogger_videos(), config_get(), config_schema_tool(), config_set(), config_set_bulk(), _json(), live_add() (+28 more)

### Community 15 - "scheduler.rs"
Cohesion: 0.14
Nodes (32): Config, Drop, JoinHandle, LiveCheckStats, LiveMonitor, Sender, T, U (+24 more)

### Community 16 - "video_monitor.py"
Cohesion: 0.10
Nodes (30): is_bilibili(), _bilibili_anon_cookies_file(), _bilibili_browser_cookies_file(), _bilibili_title(), _bilibili_upload_ids(), _bilibili_upload_ids_rsshub(), _bilibili_user_cookies_file(), check_once() (+22 more)

### Community 17 - "config.rs"
Cohesion: 0.15
Nodes (23): AsRef, atomic_write(), atomic_write_replaces_existing_file_without_leftover_tmp(), atomic_write_survives_stale_tmp_from_other_process(), email_status_lists_all_missing_fields(), find_project_root(), fupan_is_yes_reads_fupan_file_only(), import_legacy_email() (+15 more)

### Community 18 - "Config"
Cohesion: 0.16
Nodes (12): clean_push_value(), Config, email_status_missing_hint(), EmailSettings, EmailStatus, IniFile, mask(), HashMap (+4 more)

### Community 19 - "main.cjs"
Cohesion: 0.09
Nodes (25): { app, BrowserWindow, Tray, Menu, globalShortcut, ipcMain, shell, nativeImage, Notification }, applyStartupPrefs(), ASSETS, { consumeNotifyQueue }, DIST_HTML, fs, gotLock, { healthcheckInvocation } (+17 more)

### Community 20 - "main.py"
Cohesion: 0.12
Nodes (16): translated_print(), backup_file(), backup_file_start(), check_ffmpeg_existence(), check_subprocess(), clear_record_info(), converts_m4a(), converts_mp4() (+8 more)

### Community 21 - "App.jsx"
Cohesion: 0.12
Nodes (17): App(), buildRows(), detectPlatform(), normalizeUrl(), notifyOptions, platformColors, platformLabels, platformOptions (+9 more)

### Community 22 - "pipeline_bridge.rs"
Cohesion: 0.19
Nodes (20): drain_process_tasks(), list_notes(), pipeline_worker_marks_stuck_task_failed(), PipelineSummary, python_tool_paths_are_relative_to_project(), PythonTool, Duration, Path (+12 more)

### Community 23 - "三、功能需求清单"
Cohesion: 0.09
Nodes (21): 3.1 直播监控（核心）, 3.2 复盘流水线（开关可选）, 3.3 通知（简单版）, 3.4 博主已发布视频浏览 + 选择性下载（新增重点）, 3.5 训练数据沉淀（新增）, 3.6 智能体接入（保留）, 4.1 数据模型（SQLite 表 video_library）, 4.2 列表获取 (+13 more)

### Community 25 - "video_library.py"
Cohesion: 0.19
Nodes (19): Connection, blogger_videos(), 列出某博主已发布的历史视频，供界面批量勾选下载。, attach_downloaded(), cache_cover(), _connect(), get_videos(), init_db() (+11 more)

### Community 26 - "ab_sign.py"
Cohesion: 0.19
Nodes (13): ab_sign(), ff_j(), gener_random(), generate_random_str(), generate_rc4_bb_str(), get_long_int(), get_t_j(), gg_j() (+5 more)

### Community 27 - "bilibili_electron.rs"
Cohesion: 0.22
Nodes (18): acquire_cross_process_lock(), cleanup_stale_script_files(), electron_lock_file(), ElectronResult, find_electron(), old_electron_temp_files_are_cleaned_up(), payload_and_runner_paths_are_project_local(), Duration (+10 more)

### Community 28 - "直播复盘助手 / LiveReview"
Cohesion: 0.11
Nodes (18): 1. 添加要监控的直播间, 2.1 老项目 QQ 邮箱迁移说明, 2. 配置邮箱通知（谁开播了马上发邮件给你）, 3. 配置大模型（用于知识提炼，可选但推荐）, 4. 启动直播监控, 5. 启动发布视频监控（可选，另开一个终端）, CLI（脚本 / Shell 调用）, MCP（推荐） (+10 more)

### Community 29 - "extract-run.cjs"
Cohesion: 0.14
Nodes (14): DEFAULT_OUT, extractBilibiliStreams(), fs, harvestBilibiliCookies(), path, runStandalone(), { app }, cookieParser (+6 more)

### Community 30 - "local-notify.test.cjs"
Cohesion: 0.11
Nodes (17): assert, { consumeNotifyQueue }, corrupt, dir, first, fourth, fs, items (+9 more)

### Community 31 - "ffmpeg_install.py"
Cohesion: 0.25
Nodes (16): _add_bundled_to_path(), check_ffmpeg(), check_ffmpeg_installed(), _download(), _download_with_sha256(), _download_with_size(), _http_get(), install_ffmpeg() (+8 more)

### Community 32 - "push_message"
Cohesion: 0.21
Nodes (17): 真正走 SMTP 发信；测试用（不落盘），成功后返回 sent=True。, send_email_smtp(), 按指定通道集合发送，channels 元素：微信/邮箱/PushPlus/Bark/ntfy, Server酱³：推送到微信，走 443 HTTPS，代理环境下可达。, _send_channels(), _send_email(), _serverchan(), push_message() (+9 more)

### Community 33 - "status_snapshot.rs"
Cohesion: 0.27
Nodes (16): Instant, build(), clear_file_scan_cache_for_tests(), file_scan_cache(), file_scan_cache_short_ttl_reuses_previous_scan(), FileScanCache, recent_recordings(), recent_recordings_are_sorted_and_relative() (+8 more)

### Community 34 - "config_api.py"
Cohesion: 0.26
Nodes (15): _file_key(), get_config(), _is_secret(), _load(), _match_existing_key(), Path, RawConfigParser, 大小写不敏感地找已有键，避免同名不同大小写的重复键弄崩 main.py 的解析。 (+7 more)

### Community 35 - "devDependencies"
Cohesion: 0.13
Nodes (15): concurrently, devDependencies, concurrently, electron-builder, sharp, to-ico, vite, @vitejs/plugin-react (+7 more)

### Community 36 - ".try_acquire"
Cohesion: 0.25
Nodes (11): DaemonLock, lock_file(), Drop, File, Option, Path, PathBuf, Result (+3 more)

### Community 37 - "bilibili-cookie-loader.test.cjs"
Cohesion: 0.16
Nodes (12): fs, injectCookies(), parseNetscape(), path, assert, cookies, dir, file (+4 more)

### Community 38 - "build"
Cohesion: 0.14
Nodes (13): build, appId, directories, files, portable, productName, win, output (+5 more)

### Community 39 - "liveme.js"
Cohesion: 0.20
Nodes (10): Cm(), createRandom(), createSignature(), lC(), r(), requestSign(), Rm, s (+2 more)

### Community 40 - "ProxyDetector"
Cohesion: 0.26
Nodes (4): Enum, ProxyDetector, ProxyInfo, ProxyType

### Community 41 - "initializer.py"
Cohesion: 0.30
Nodes (10): check_node(), check_nodejs_installed(), get_package_manager(), install_nodejs(), install_nodejs_centos(), install_nodejs_mac(), install_nodejs_ubuntu(), install_nodejs_windows() (+2 more)

### Community 42 - "_bili_test.cjs"
Cohesion: 0.18
Nodes (8): { app, BrowserWindow, session }, fs, hardStop, OUT, path, { contextBridge, ipcRenderer }, electron, electron

### Community 43 - "README.md"
Cohesion: 0.18
Nodes (10): 🌱使用说明, 🐋容器运行, 😺已支持平台, ⏳提交日志, 有问题可以提issue, 我会在这里持续添加更多直播平台的录制 欢迎Star, 🎃源码运行, 🤖相关项目, 💡简介 (+2 more)

### Community 44 - "x-bogus.js"
Cohesion: 0.25
Nodes (5): getDdCalcu(), c(), f(), M(), P()

### Community 45 - "app_prefs.rs"
Cohesion: 0.42
Nodes (10): app_prefs_defaults(), app_prefs_path(), defaults_are_stable_and_unknown_keys_are_dropped(), read_app_prefs(), read_missing_file_returns_defaults(), Path, PathBuf, Result (+2 more)

### Community 46 - "healthcheck.test.cjs"
Cohesion: 0.22
Nodes (8): healthcheckInvocation(), path, assert, dir, fs, { healthcheckInvocation }, os, path

### Community 47 - "尊者直播监控 — 项目目标（上下文恢复锚点）"
Cohesion: 0.20
Nodes (9): Rust 三平台 CI, Rust 重构（zunzhe-core）已交付能力, 一、项目是什么, 三、核心需求（用户澄清后）, 二、最终目标, 五、关键约定, 六、上下文丢失恢复步骤, 四、阶段与验收（当前进度） (+1 more)

### Community 48 - "dependencies"
Cohesion: 0.22
Nodes (9): @ant-design/icons, antd, dependencies, @ant-design/icons, antd, react, react-dom, react (+1 more)

### Community 49 - "bili-video-list.cjs"
Cohesion: 0.25
Nodes (8): { app, BrowserWindow, session }, cookieParser, crypto, finish(), fs, path, run(), watchdog

### Community 50 - "bili_browser.py"
Cohesion: 0.31
Nodes (8): download(), _electron_binary(), extract_streams(), _ffmpeg(), Path, 返回 Electron 可执行文件（跨 Windows/macOS/Linux）。, 驱动 Electron 提取 B站视频的 DASH 直链。返回 {ok,title,video,audio}。, 下载 B站视频到 save_dir，返回最终 mp4 路径；失败返回 None。

### Community 51 - "scripts"
Cohesion: 0.25
Nodes (8): scripts, build, dev, electron:dev, icons, pack, pack:mac, start

### Community 52 - "make-icons.cjs"
Cohesion: 0.32
Nodes (7): assets, fs, main(), path, sharp, src, toIco

### Community 53 - "尊者直播监控 — 项目图谱梳理"
Cohesion: 0.25
Nodes (7): 一、项目是什么, 三、核心调用链（God Nodes = 系统骨架）, 二、分层结构（图谱社区即模块边界）, 五、本次已修复的问题（配合图谱定位）, 六、怎么用这份图谱, 四、关键风险点（图谱佐证）, 尊者直播监控 — 项目图谱梳理

### Community 54 - "local-notify.cjs"
Cohesion: 0.38
Nodes (6): consumeNotifyQueue(), fs, loadSeenNotifyIds(), path, persistSeenNotifyIds(), e()

### Community 55 - "createWindow"
Cohesion: 0.38
Nodes (7): createTray(), createWindow(), loadAppIcon(), loadDistPage(), registerShortcut(), reloadUiWindow(), trayIcon()

### Community 56 - "export_training_data"
Cohesion: 0.43
Nodes (5): export_training_data(), _fmt_date(), Path, 扫描 fupan_output/<主播>/<场次>/，把转写与知识点导出为 JSONL。, _read_json()

### Community 57 - "package.json"
Cohesion: 0.33
Nodes (5): author, description, main, name, version

### Community 58 - "mac"
Cohesion: 0.33
Nodes (6): mac, category, icon, target, dmg, zip

### Community 59 - "PULL_REQUEST_TEMPLATE.md"
Cohesion: 0.33
Nodes (5): 🔍 描述（Description）, 📜 标题（Title）, 📋 检查清单（Checklist）, 🏗️ 测试（Testing）, 📝 类型（Type of Change）

### Community 60 - "安全审查报告 —— DouyinLiveRecorder（直播复盘助手 基础项目）"
Cohesion: 0.33
Nodes (5): 一、审查范围与方法, 三、发现的风险点与 bug, 二、逐项排查结论, 四、总体评价, 安全审查报告 —— DouyinLiveRecorder（直播复盘助手 基础项目）

### Community 61 - "_resolve_media"
Cohesion: 0.50
Nodes (5): _find_candidates(), Path, 按路径找实际录制文件。分段录制路径是 ffmpeg 模板（如 xxx_%03d.ts）， 实际文件是 xxx_000.ts；转码又会把 ts 换成…, 录制的 ts 可能正在被异步转成 mp4（转完删原文件），等文件落定后返回实际路径。, _resolve_media()

### Community 62 - "import_legacy_account"
Cohesion: 0.50
Nodes (4): _decrypt_legacy(), import_legacy_account(), 尝试用老项目的 AES 密钥解密；无密钥或失败一律返回 None。, 从老项目 accounts.json 读取 QQ 邮箱账号并写入当前配置。 注意：老项目密码是 AES-256-GCM 加密的，需要同目录…

### Community 63 - "start-rust-daemon.sh"
Cohesion: 0.50
Nodes (3): PATH, RUST_LOG, start-rust-daemon.sh script

### Community 64 - "sync_req"
Cohesion: 0.50
Nodes (3): OptionalDict, OptionalStr, sync_req()

### Community 66 - "laixiu.js"
Cohesion: 0.83
Nodes (3): calculateSign(), generateUUID(), sign()

### Community 67 - "六、当前进度"
Cohesion: 0.50
Nodes (4): 供应链事实（ffmpeg 与平台下载）, 六、当前进度, 权限事实（桌面端“完全访问”）, 邮箱通知事实

### Community 68 - "read_config_value"
Cohesion: 0.67
Nodes (3): Any, RawConfigParser, read_config_value()

## Knowledge Gaps
- **275 isolated node(s):** `📜 标题（Title）`, `🔍 描述（Description）`, `📝 类型（Type of Change）`, `🏗️ 测试（Testing）`, `📋 检查清单（Checklist）` (+270 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **19 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `Config` connect `Config` to `status_snapshot.rs`, `live_monitor.rs`, `notify.rs`, `mcp.rs`, `config.rs`, `pipeline_bridge.rs`?**
  _High betweenness centrality (0.061) - this node is a cross-community bridge._
- **Why does `Watchlist` connect `Watchlist` to `live_monitor.rs`, `mcp.rs`?**
  _High betweenness centrality (0.022) - this node is a cross-community bridge._
- **Why does `get_notify()` connect `cli.py` to `push_message`, `main.py`, `notify.py`?**
  _High betweenness centrality (0.012) - this node is a cross-community bridge._
- **Are the 3 inferred relationships involving `start_record()` (e.g. with `converts_mp4()` and `generate_subtitles()`) actually correct?**
  _`start_record()` has 3 INFERRED edges - model-reasoned connections that need verification._
- **What connects `📜 标题（Title）`, `🔍 描述（Description）`, `📝 类型（Type of Change）` to the rest of the system?**
  _275 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `spider.py` be split into smaller, more focused modules?**
  _Cohesion score 0.05129619415333701 - nodes in this community are weakly interconnected._
- **Should `platform.rs` be split into smaller, more focused modules?**
  _Cohesion score 0.05772005772005772 - nodes in this community are weakly interconnected._