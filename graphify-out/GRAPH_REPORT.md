# Graph Report - 尊者监控直播短视频  (2026-08-12)

## Corpus Check
- 87 files · ~121,200 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 1015 nodes · 2239 edges · 57 communities (53 shown, 4 thin omitted)
- Extraction: 99% EXTRACTED · 1% INFERRED · 0% AMBIGUOUS · INFERRED: 28 edges (avg confidence: 0.6)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `a6622e8b`
- Run `git rev-parse HEAD` and compare to check if the graph is stale.
- Run `graphify update .` after code changes (no API cost).

## Community Hubs (Navigation)
- spider.py
- video_monitor.py
- main.py
- cli.py
- main.cjs
- mcp_server.py
- devDependencies
- initializer.py
- App.jsx
- crypto-js.min.js
- build
- ab_sign.py
- ffmpeg_install.py
- liveme.js
- utils.py
- x-bogus.js
- make-icons.cjs
- sync_req
- laixiu.js
- setup-macos.sh
- DouyinLiveRecorder
- platform.rs
- 三、功能需求清单
- db.rs
- 直播复盘助手 / LiveReview
- README.md
- dependencies
- scripts
- 尊者直播监控 — 项目目标（上下文恢复锚点）
- 尊者直播监控 — 项目图谱梳理
- export_training_data
- package.json
- mac
- PULL_REQUEST_TEMPLATE.md
- 安全审查报告 —— DouyinLiveRecorder（直播复盘助手 基础项目）
- main
- Config
- download.rs
- settings.py
- pipeline_bridge.rs
- notify.py
- email_setup.py
- config_api.py
- push_message
- app_prefs.py
- _download_and_process
- 尊者直播监控 Rust 重构 — 目标模式（北极星）
- monitor_prefs.py
- _load
- write_email_config
- _resolve_media
- import_legacy_account
- start-rust-daemon.sh

## God Nodes (most connected - your core abstractions)
1. `async_req()` - 77 edges
2. `start_record()` - 76 edges
3. `trace_error_decorator()` - 75 edges
4. `main()` - 35 edges
5. `_json()` - 22 edges
6. `Config` - 21 edges
7. `get()` - 20 edges
8. `is_yes()` - 18 edges
9. `fupan_config()` - 17 edges
10. `run_pipeline()` - 16 edges

## Surprising Connections (you probably didn't know these)
- `check_ffmpeg_existence()` --calls--> `check_ffmpeg()`  [EXTRACTED]
  main.py → ffmpeg_install.py
- `send_email_smtp()` --calls--> `send_email()`  [EXTRACTED]
  fupan/email_setup.py → msg_push.py
- `push_message()` --calls--> `get_notify()`  [EXTRACTED]
  main.py → fupan/monitor_prefs.py
- `_send_email()` --calls--> `send_email()`  [EXTRACTED]
  fupan/notify.py → msg_push.py
- `_send_channels()` --calls--> `bark()`  [EXTRACTED]
  fupan/notify.py → msg_push.py

## Import Cycles
- None detected.

## Communities (57 total, 4 thin omitted)

### Community 0 - "spider.py"
Cohesion: 0.07
Nodes (110): callable, Exception, start_record(), async_req(), get_response_status(), OptionalDict, OptionalStr, get_live_room_id() (+102 more)

### Community 1 - "video_monitor.py"
Cohesion: 0.06
Nodes (54): Connection, download(), _electron_binary(), extract_streams(), _ffmpeg(), is_bilibili(), Path, 返回 Electron 可执行文件（跨 Windows/macOS/Linux）。 (+46 more)

### Community 2 - "main.py"
Cohesion: 0.10
Nodes (20): translated_print(), backup_file(), backup_file_start(), check_subprocess(), clear_record_info(), converts_m4a(), converts_mp4(), direct_download_stream() (+12 more)

### Community 3 - "cli.py"
Cohesion: 0.13
Nodes (37): add_live(), add_video(), check_live(), _contains_url(), delete_live(), email_status(), ensure_services(), export_training_data() (+29 more)

### Community 4 - "main.cjs"
Cohesion: 0.05
Nodes (45): DEFAULT_OUT, extractBilibiliStreams(), fs, harvestBilibiliCookies(), path, runStandalone(), { app, BrowserWindow, session }, fs (+37 more)

### Community 5 - "mcp_server.py"
Cohesion: 0.10
Nodes (38): config_schema(), blogger_videos(), config_get(), config_schema_tool(), config_set(), config_set_bulk(), _json(), live_add() (+30 more)

### Community 6 - "devDependencies"
Cohesion: 0.12
Nodes (17): concurrently, devDependencies, concurrently, electron, electron-builder, sharp, to-ico, vite (+9 more)

### Community 7 - "initializer.py"
Cohesion: 0.21
Nodes (10): check_node(), check_nodejs_installed(), get_package_manager(), install_nodejs(), install_nodejs_centos(), install_nodejs_mac(), install_nodejs_ubuntu(), install_nodejs_windows() (+2 more)

### Community 8 - "App.jsx"
Cohesion: 0.12
Nodes (17): App(), buildRows(), detectPlatform(), normalizeUrl(), notifyOptions, platformColors, platformLabels, platformOptions (+9 more)

### Community 10 - "build"
Cohesion: 0.14
Nodes (13): build, appId, directories, files, portable, productName, win, output (+5 more)

### Community 11 - "ab_sign.py"
Cohesion: 0.19
Nodes (13): ab_sign(), ff_j(), gener_random(), generate_random_str(), generate_rc4_bb_str(), get_long_int(), get_t_j(), gg_j() (+5 more)

### Community 12 - "ffmpeg_install.py"
Cohesion: 0.23
Nodes (17): _add_bundled_to_path(), check_ffmpeg(), check_ffmpeg_installed(), _download(), _download_with_sha256(), _download_with_size(), _http_get(), install_ffmpeg() (+9 more)

### Community 13 - "liveme.js"
Cohesion: 0.20
Nodes (10): Cm(), createRandom(), createSignature(), lC(), r(), requestSign(), Rm, s (+2 more)

### Community 14 - "utils.py"
Cohesion: 0.11
Nodes (17): Enum, clean_name(), ProxyDetector, ProxyInfo, ProxyType, check_disk_capacity(), check_md5(), Color (+9 more)

### Community 15 - "x-bogus.js"
Cohesion: 0.25
Nodes (5): getDdCalcu(), c(), f(), M(), P()

### Community 16 - "make-icons.cjs"
Cohesion: 0.32
Nodes (7): assets, fs, main(), path, sharp, src, toIco

### Community 17 - "sync_req"
Cohesion: 0.50
Nodes (3): OptionalDict, OptionalStr, sync_req()

### Community 19 - "laixiu.js"
Cohesion: 0.83
Nodes (3): calculateSign(), generateUUID(), sign()

### Community 25 - "platform.rs"
Cohesion: 0.12
Nodes (26): Box, Client, Default, Future, Into, Output, Pin, Send (+18 more)

### Community 26 - "三、功能需求清单"
Cohesion: 0.09
Nodes (21): 3.1 直播监控（核心）, 3.2 复盘流水线（开关可选）, 3.3 通知（简单版）, 3.4 博主已发布视频浏览 + 选择性下载（新增重点）, 3.5 训练数据沉淀（新增）, 3.6 智能体接入（保留）, 4.1 数据模型（SQLite 表 video_library）, 4.2 列表获取 (+13 more)

### Community 27 - "db.rs"
Cohesion: 0.11
Nodes (45): add_anchor(), add_notify(), enqueue_task(), get_videos(), init_db(), is_downloaded(), list_anchors(), list_notify() (+37 more)

### Community 28 - "直播复盘助手 / LiveReview"
Cohesion: 0.11
Nodes (18): 1. 添加要监控的直播间, 2.1 老项目 QQ 邮箱迁移说明, 2. 配置邮箱通知（谁开播了马上发邮件给你）, 3. 配置大模型（用于知识提炼，可选但推荐）, 4. 启动直播监控, 5. 启动发布视频监控（可选，另开一个终端）, CLI（脚本 / Shell 调用）, MCP（推荐） (+10 more)

### Community 29 - "README.md"
Cohesion: 0.18
Nodes (10): 🌱使用说明, 🐋容器运行, 😺已支持平台, ⏳提交日志, 有问题可以提issue, 我会在这里持续添加更多直播平台的录制 欢迎Star, 🎃源码运行, 🤖相关项目, 💡简介 (+2 more)

### Community 30 - "dependencies"
Cohesion: 0.22
Nodes (9): @ant-design/icons, antd, dependencies, @ant-design/icons, antd, react, react-dom, react (+1 more)

### Community 31 - "scripts"
Cohesion: 0.25
Nodes (8): scripts, build, dev, electron:dev, icons, pack, pack:mac, start

### Community 32 - "尊者直播监控 — 项目目标（上下文恢复锚点）"
Cohesion: 0.22
Nodes (8): Rust 重构（zunzhe-core）已交付能力, 一、项目是什么, 三、核心需求（用户澄清后）, 二、最终目标, 五、关键约定, 六、上下文丢失恢复步骤, 四、阶段与验收（当前进度）, 尊者直播监控 — 项目目标（上下文恢复锚点）

### Community 33 - "尊者直播监控 — 项目图谱梳理"
Cohesion: 0.25
Nodes (7): 一、项目是什么, 三、核心调用链（God Nodes = 系统骨架）, 二、分层结构（图谱社区即模块边界）, 五、本次已修复的问题（配合图谱定位）, 六、怎么用这份图谱, 四、关键风险点（图谱佐证）, 尊者直播监控 — 项目图谱梳理

### Community 34 - "export_training_data"
Cohesion: 0.43
Nodes (5): export_training_data(), _fmt_date(), Path, 扫描 fupan_output/<主播>/<场次>/，把转写与知识点导出为 JSONL。, _read_json()

### Community 35 - "package.json"
Cohesion: 0.33
Nodes (5): author, description, main, name, version

### Community 36 - "mac"
Cohesion: 0.33
Nodes (6): mac, category, icon, target, dmg, zip

### Community 37 - "PULL_REQUEST_TEMPLATE.md"
Cohesion: 0.33
Nodes (5): 🔍 描述（Description）, 📜 标题（Title）, 📋 检查清单（Checklist）, 🏗️ 测试（Testing）, 📝 类型（Type of Change）

### Community 38 - "安全审查报告 —— DouyinLiveRecorder（直播复盘助手 基础项目）"
Cohesion: 0.33
Nodes (5): 一、审查范围与方法, 三、发现的风险点与 bug, 二、逐项排查结论, 四、总体评价, 安全审查报告 —— DouyinLiveRecorder（直播复盘助手 基础项目）

### Community 40 - "Config"
Cohesion: 0.08
Nodes (34): HashMap, Config, email_status_missing_hint(), EmailSettings, EmailStatus, find_project_root(), IniFile, mask() (+26 more)

### Community 41 - "download.rs"
Cohesion: 0.08
Nodes (35): Arc, Stdout, Value, Downloader, DownloadResult, extract_video_id(), find_latest_video(), find_ytdlp() (+27 more)

### Community 42 - "settings.py"
Cohesion: 0.30
Nodes (16): cut_clips(), _safe_name(), _chat(), _client(), distill_transcript(), _parse_json(), 输入 transcribe 的结果，输出 知识点.json 和 复盘笔记.md。, main() (+8 more)

### Community 43 - "pipeline_bridge.rs"
Cohesion: 0.15
Nodes (16): Cli, Command, Option, PathBuf, Result, String, run(), PipelineSummary (+8 more)

### Community 44 - "notify.py"
Cohesion: 0.19
Nodes (17): get_notify(), _channels_for_pref(), _clean(), _push_channels(), 按指定通道集合发送，channels 元素：微信/邮箱/PushPlus/Bark/ntfy, 返回要使用的通道集合；None 表示不通知。, 按该监控项的「通知=」偏好发送（可在监控偏好.json 按 URL 配置）。, 过滤基础项目里遗留的占位符，但保留真实的 Server酱/PushPlus 密钥。 (+9 more)

### Community 45 - "email_setup.py"
Cohesion: 0.18
Nodes (10): main(), _decrypt_legacy(), email_status(), _mask(), 尝试用老项目的 AES 密钥解密；无密钥或失败一律返回 None。, 真正走 SMTP 发信；测试用（不落盘），成功后返回 sent=True。, 返回邮箱配置状态与可诊断的错误原因（不回显授权码）。, send_email_smtp() (+2 more)

### Community 46 - "config_api.py"
Cohesion: 0.36
Nodes (12): _file_key(), get_config(), _is_secret(), _load(), _match_existing_key(), Path, RawConfigParser, 大小写不敏感地找已有键，避免同名不同大小写的重复键弄崩 main.py 的解析。 (+4 more)

### Community 47 - "push_message"
Cohesion: 0.40
Nodes (10): push_message(), bark(), dingtalk(), ntfy(), pushplus(), Any, PushPlus推送通知 API文档: https://www.pushplus.plus/doc/, send_email() (+2 more)

### Community 48 - "app_prefs.py"
Cohesion: 0.29
Nodes (9): get_prefs(), _load(), mark_wizard_done(), _save(), set_prefs(), _queue_local(), 发送测试通知。未配置邮箱时给出明确修复提示，不再只报发送失败。, 写入待推送队列，由 Electron 桌面端弹出系统通知（代理/额度失败时的兜底）。 (+1 more)

### Community 49 - "_download_and_process"
Cohesion: 0.32
Nodes (8): download_video(), 立即下载单个视频到 downloads/短视频/<名称>/，并跑复盘流水线。, _bilibili_anon_cookies_file(), _bilibili_user_cookies_file(), _download_and_process(), Path, 若用户在 [视频监控] b站cookie 里粘贴了浏览器 Cookie，转成 Netscape 格式给 yt-dlp 用。B站 412 风控时这是最可靠的解法。, B站对无 cookie 请求常返回 412 风控。领取匿名设备标识(buvid)写成 Netscape 格式 cookies 文件供 yt-dlp…

### Community 50 - "尊者直播监控 Rust 重构 — 目标模式（北极星）"
Cohesion: 0.25
Nodes (7): 一、最终目标（验收标准）, 三、接口契约（冻结）, 二、原子模块清单与接口契约, 五、稳定性验收（24×30）, 六、当前进度, 四、防回归规则（每次改动必须）, 尊者直播监控 Rust 重构 — 目标模式（北极星）

### Community 51 - "monitor_prefs.py"
Cohesion: 0.48
Nodes (5): set_item_notify(), delete_prefs(), _load(), _save(), set_notify()

### Community 52 - "_load"
Cohesion: 0.33
Nodes (6): ConfigParser, _load(), Path, RawConfigParser, 返回项目虚拟环境里的 Python 可执行文件（跨 Windows/macOS/Linux）。, venv_python()

### Community 53 - "write_email_config"
Cohesion: 0.40
Nodes (5): email_setup(), 写入 QQ 邮箱 SMTP 配置并启用邮件通知。, 写入 QQ 邮箱 SMTP 配置，并启用邮件通知渠道。, 内部写入（供迁移函数使用，忽略单键失败时抛错）。, write_email_config()

### Community 54 - "_resolve_media"
Cohesion: 0.50
Nodes (5): _find_candidates(), Path, 按路径找实际录制文件。分段录制路径是 ffmpeg 模板（如 xxx_%03d.ts）， 实际文件是 xxx_000.ts；转码又会把 ts 换成…, 录制的 ts 可能正在被异步转成 mp4（转完删原文件），等文件落定后返回实际路径。, _resolve_media()

### Community 55 - "import_legacy_account"
Cohesion: 0.50
Nodes (4): email_import_legacy(), 尝试从老项目 zunzhe-dev 迁移 QQ 邮箱账号。, import_legacy_account(), 从老项目 accounts.json 读取 QQ 邮箱账号并写入当前配置。 注意：老项目密码是 AES-256-GCM 加密的，需要同目录…

### Community 56 - "start-rust-daemon.sh"
Cohesion: 0.50
Nodes (3): PATH, RUST_LOG, start-rust-daemon.sh script

## Knowledge Gaps
- **150 isolated node(s):** `{ app, BrowserWindow, session }`, `fs`, `path`, `OUT`, `hardStop` (+145 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **4 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `get_notify()` connect `notify.py` to `monitor_prefs.py`, `main.py`, `cli.py`, `push_message`?**
  _High betweenness centrality (0.048) - this node is a cross-community bridge._
- **Why does `start_record()` connect `spider.py` to `main.py`, `utils.py`, `push_message`?**
  _High betweenness centrality (0.031) - this node is a cross-community bridge._
- **Why does `Config` connect `Config` to `download.rs`, `pipeline_bridge.rs`?**
  _High betweenness centrality (0.019) - this node is a cross-community bridge._
- **Are the 3 inferred relationships involving `start_record()` (e.g. with `converts_mp4()` and `generate_subtitles()`) actually correct?**
  _`start_record()` has 3 INFERRED edges - model-reasoned connections that need verification._
- **What connects `{ app, BrowserWindow, session }`, `fs`, `path` to the rest of the system?**
  _150 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `spider.py` be split into smaller, more focused modules?**
  _Cohesion score 0.07328054649899084 - nodes in this community are weakly interconnected._
- **Should `video_monitor.py` be split into smaller, more focused modules?**
  _Cohesion score 0.060451977401129946 - nodes in this community are weakly interconnected._