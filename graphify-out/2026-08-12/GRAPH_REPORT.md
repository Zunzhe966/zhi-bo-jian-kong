# Graph Report - 尊者监控直播短视频  (2026-08-12)

## Corpus Check
- 74 files · ~113,484 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 808 nodes · 1750 edges · 40 communities (36 shown, 4 thin omitted)
- Extraction: 98% EXTRACTED · 2% INFERRED · 0% AMBIGUOUS · INFERRED: 27 edges (avg confidence: 0.6)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `6758917e`
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
- utils.py
- App.jsx
- crypto-js.min.js
- build
- ab_sign.py
- ffmpeg_install.py
- liveme.js
- ProxyDetector
- x-bogus.js
- make-icons.cjs
- sync_req
- laixiu.js
- setup-macos.sh
- DouyinLiveRecorder
- .list_videos
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

## God Nodes (most connected - your core abstractions)
1. `async_req()` - 77 edges
2. `start_record()` - 76 edges
3. `trace_error_decorator()` - 75 edges
4. `main()` - 32 edges
5. `_json()` - 22 edges
6. `get()` - 20 edges
7. `fupan_config()` - 17 edges
8. `run_pipeline()` - 16 edges
9. `check_once()` - 16 edges
10. `is_yes()` - 15 edges

## Surprising Connections (you probably didn't know these)
- `check_ffmpeg_existence()` --calls--> `check_ffmpeg()`  [EXTRACTED]
  main.py → ffmpeg_install.py
- `push_message()` --calls--> `get_notify()`  [EXTRACTED]
  main.py → fupan/monitor_prefs.py
- `start_record()` --calls--> `get_file_paths()`  [EXTRACTED]
  main.py → src/utils.py
- `start_record()` --calls--> `get_query_params()`  [EXTRACTED]
  main.py → src/utils.py
- `start_record()` --calls--> `update_config()`  [EXTRACTED]
  main.py → src/utils.py

## Import Cycles
- None detected.

## Communities (40 total, 4 thin omitted)

### Community 0 - "spider.py"
Cohesion: 0.08
Nodes (102): callable, Exception, start_record(), async_req(), get_response_status(), OptionalDict, OptionalStr, _fetch_web_stream_data_global() (+94 more)

### Community 1 - "video_monitor.py"
Cohesion: 0.06
Nodes (69): ConfigParser, download(), _electron_binary(), extract_streams(), _ffmpeg(), is_bilibili(), Path, 返回 Electron 可执行文件（跨 Windows/macOS/Linux）。 (+61 more)

### Community 2 - "main.py"
Cohesion: 0.06
Nodes (57): _channels_for_pref(), _clean(), _push_channels(), _queue_local(), 按指定通道集合发送，channels 元素：微信/邮箱/PushPlus/Bark/ntfy, 返回要使用的通道集合；None 表示不通知。, 按该监控项的「通知=」偏好发送（可在监控偏好.json 按 URL 配置）。, 测试/无 URL 时：走全局（所有已配通道）。 (+49 more)

### Community 3 - "cli.py"
Cohesion: 0.07
Nodes (67): Connection, get_prefs(), _load(), mark_wizard_done(), _save(), set_prefs(), add_live(), add_video() (+59 more)

### Community 4 - "main.cjs"
Cohesion: 0.05
Nodes (45): DEFAULT_OUT, extractBilibiliStreams(), fs, harvestBilibiliCookies(), path, runStandalone(), { app, BrowserWindow, session }, fs (+37 more)

### Community 5 - "mcp_server.py"
Cohesion: 0.09
Nodes (48): config_schema(), _file_key(), get_config(), _is_secret(), _load(), _match_existing_key(), Path, RawConfigParser (+40 more)

### Community 6 - "devDependencies"
Cohesion: 0.12
Nodes (17): concurrently, devDependencies, concurrently, electron, electron-builder, sharp, to-ico, vite (+9 more)

### Community 7 - "utils.py"
Cohesion: 0.08
Nodes (29): clean_name(), check_node(), check_nodejs_installed(), get_package_manager(), install_nodejs(), install_nodejs_centos(), install_nodejs_mac(), install_nodejs_ubuntu() (+21 more)

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
Cohesion: 0.25
Nodes (16): _add_bundled_to_path(), check_ffmpeg(), check_ffmpeg_installed(), _download(), _download_with_sha256(), _download_with_size(), _http_get(), install_ffmpeg() (+8 more)

### Community 13 - "liveme.js"
Cohesion: 0.20
Nodes (10): Cm(), createRandom(), createSignature(), lC(), r(), requestSign(), Rm, s (+2 more)

### Community 14 - "ProxyDetector"
Cohesion: 0.26
Nodes (4): Enum, ProxyDetector, ProxyInfo, ProxyType

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

### Community 25 - ".list_videos"
Cohesion: 0.14
Nodes (17): Box, Client, Default, Future, Output, Pin, Self, Send (+9 more)

### Community 26 - "三、功能需求清单"
Cohesion: 0.09
Nodes (21): 3.1 直播监控（核心）, 3.2 复盘流水线（开关可选）, 3.3 通知（简单版）, 3.4 博主已发布视频浏览 + 选择性下载（新增重点）, 3.5 训练数据沉淀（新增）, 3.6 智能体接入（保留）, 4.1 数据模型（SQLite 表 video_library）, 4.2 列表获取 (+13 more)

### Community 27 - "db.rs"
Cohesion: 0.18
Nodes (19): PathBuf, SqlitePool, Cli, Command, Option, Result, String, run() (+11 more)

### Community 28 - "直播复盘助手 / LiveReview"
Cohesion: 0.11
Nodes (17): 1. 添加要监控的直播间, 2. 配置邮箱通知（谁开播了马上发邮件给你）, 3. 配置大模型（用于知识提炼，可选但推荐）, 4. 启动直播监控, 5. 启动发布视频监控（可选，另开一个终端）, CLI（脚本 / Shell 调用）, MCP（推荐）, Rust 核心骨架（阶段E） (+9 more)

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
Cohesion: 0.25
Nodes (7): 一、项目是什么, 三、核心需求（用户澄清后）, 二、最终目标, 五、关键约定, 六、上下文丢失恢复步骤, 四、阶段与验收（当前进度）, 尊者直播监控 — 项目目标（上下文恢复锚点）

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

## Knowledge Gaps
- **140 isolated node(s):** `{ app, BrowserWindow, session }`, `fs`, `path`, `OUT`, `hardStop` (+135 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **4 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `get_notify()` connect `cli.py` to `main.py`?**
  _High betweenness centrality (0.069) - this node is a cross-community bridge._
- **Why does `start_record()` connect `spider.py` to `main.py`, `utils.py`?**
  _High betweenness centrality (0.038) - this node is a cross-community bridge._
- **Why does `send_notification()` connect `main.py` to `cli.py`?**
  _High betweenness centrality (0.016) - this node is a cross-community bridge._
- **Are the 3 inferred relationships involving `start_record()` (e.g. with `converts_mp4()` and `generate_subtitles()`) actually correct?**
  _`start_record()` has 3 INFERRED edges - model-reasoned connections that need verification._
- **What connects `{ app, BrowserWindow, session }`, `fs`, `path` to the rest of the system?**
  _140 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `spider.py` be split into smaller, more focused modules?**
  _Cohesion score 0.08205128205128205 - nodes in this community are weakly interconnected._
- **Should `video_monitor.py` be split into smaller, more focused modules?**
  _Cohesion score 0.057124310288867254 - nodes in this community are weakly interconnected._