# Graph Report - .  (2026-08-12)

## Corpus Check
- 85 files · ~109,256 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 648 nodes · 1518 edges · 25 communities (22 shown, 3 thin omitted)
- Extraction: 98% EXTRACTED · 2% INFERRED · 0% AMBIGUOUS · INFERRED: 27 edges (avg confidence: 0.6)
- Token cost: 0 input · 0 output

## Community Hubs (Navigation)
- Community 0
- Community 1
- Community 2
- Community 3
- Community 4
- Community 5
- Community 6
- Community 7
- Community 8
- Community 9
- Community 10
- Community 11
- Community 12
- Community 13
- Community 14
- Community 15
- Community 16
- Community 17
- Community 19
- Community 20
- Community 22

## God Nodes (most connected - your core abstractions)
1. `async_req()` - 77 edges
2. `start_record()` - 76 edges
3. `trace_error_decorator()` - 75 edges
4. `main()` - 30 edges
5. `_json()` - 22 edges
6. `get()` - 19 edges
7. `run_pipeline()` - 16 edges
8. `fupan_config()` - 16 edges
9. `is_yes()` - 15 edges
10. `check_once()` - 15 edges

## Surprising Connections (you probably didn't know these)
- `check_ffmpeg_existence()` --calls--> `check_ffmpeg()`  [EXTRACTED]
  main.py → ffmpeg_install.py
- `push_message()` --calls--> `get_notify()`  [EXTRACTED]
  main.py → fupan/monitor_prefs.py
- `select_source_url()` --calls--> `get_query_params()`  [EXTRACTED]
  main.py → src/utils.py
- `start_record()` --calls--> `get_file_paths()`  [EXTRACTED]
  main.py → src/utils.py
- `start_record()` --calls--> `get_query_params()`  [EXTRACTED]
  main.py → src/utils.py

## Import Cycles
- None detected.

## Communities (25 total, 3 thin omitted)

### Community 0 - "Community 0"
Cohesion: 0.08
Nodes (102): callable, Exception, start_record(), async_req(), get_response_status(), OptionalDict, OptionalStr, _fetch_web_stream_data_global() (+94 more)

### Community 1 - "Community 1"
Cohesion: 0.06
Nodes (63): ConfigParser, download(), _electron_binary(), extract_streams(), _ffmpeg(), is_bilibili(), Path, 返回 Electron 可执行文件（跨 Windows/macOS/Linux）。 (+55 more)

### Community 2 - "Community 2"
Cohesion: 0.07
Nodes (49): _channels_for_pref(), _clean(), _push_channels(), 按指定通道集合发送，channels 元素：微信/邮箱/PushPlus/Bark/ntfy, 返回要使用的通道集合；None 表示不通知。, 按该监控项的「通知=」偏好发送（可在监控偏好.json 按 URL 配置）。, 过滤基础项目里遗留的占位符，但保留真实的 Server酱/PushPlus 密钥。, 从 [推送配置] 读取各 HTTPS 推送通道的凭据。 (+41 more)

### Community 3 - "Community 3"
Cohesion: 0.09
Nodes (52): get_prefs(), _load(), mark_wizard_done(), _save(), set_prefs(), add_live(), add_video(), blogger_videos() (+44 more)

### Community 4 - "Community 4"
Cohesion: 0.05
Nodes (45): DEFAULT_OUT, extractBilibiliStreams(), fs, harvestBilibiliCookies(), path, runStandalone(), { app, BrowserWindow, session }, fs (+37 more)

### Community 5 - "Community 5"
Cohesion: 0.08
Nodes (49): _file_key(), get_config(), _is_secret(), _load(), _match_existing_key(), Path, RawConfigParser, 大小写不敏感地找已有键，避免同名不同大小写的重复键弄崩 main.py 的解析。 (+41 more)

### Community 6 - "Community 6"
Cohesion: 0.05
Nodes (39): @ant-design/icons, antd, concurrently, author, dependencies, @ant-design/icons, antd, react (+31 more)

### Community 7 - "Community 7"
Cohesion: 0.08
Nodes (31): clean_name(), check_node(), check_nodejs_installed(), get_package_manager(), install_nodejs(), install_nodejs_centos(), install_nodejs_mac(), install_nodejs_ubuntu() (+23 more)

### Community 8 - "Community 8"
Cohesion: 0.12
Nodes (17): App(), buildRows(), detectPlatform(), normalizeUrl(), notifyOptions, platformColors, platformLabels, platformOptions (+9 more)

### Community 10 - "Community 10"
Cohesion: 0.10
Nodes (19): build, appId, directories, files, mac, portable, productName, win (+11 more)

### Community 11 - "Community 11"
Cohesion: 0.19
Nodes (13): ab_sign(), ff_j(), gener_random(), generate_random_str(), generate_rc4_bb_str(), get_long_int(), get_t_j(), gg_j() (+5 more)

### Community 12 - "Community 12"
Cohesion: 0.23
Nodes (17): _add_bundled_to_path(), check_ffmpeg(), check_ffmpeg_installed(), _download(), _download_with_sha256(), _download_with_size(), _http_get(), install_ffmpeg() (+9 more)

### Community 13 - "Community 13"
Cohesion: 0.20
Nodes (10): Cm(), createRandom(), createSignature(), lC(), r(), requestSign(), Rm, s (+2 more)

### Community 14 - "Community 14"
Cohesion: 0.26
Nodes (4): Enum, ProxyDetector, ProxyInfo, ProxyType

### Community 15 - "Community 15"
Cohesion: 0.25
Nodes (5): getDdCalcu(), c(), f(), M(), P()

### Community 16 - "Community 16"
Cohesion: 0.32
Nodes (7): assets, fs, main(), path, sharp, src, toIco

### Community 17 - "Community 17"
Cohesion: 0.50
Nodes (3): OptionalDict, OptionalStr, sync_req()

### Community 19 - "Community 19"
Cohesion: 0.83
Nodes (3): calculateSign(), generateUUID(), sign()

## Knowledge Gaps
- **79 isolated node(s):** `{ app, BrowserWindow, session }`, `fs`, `path`, `OUT`, `hardStop` (+74 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **3 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `get_notify()` connect `Community 3` to `Community 2`?**
  _High betweenness centrality (0.087) - this node is a cross-community bridge._
- **Why does `start_record()` connect `Community 0` to `Community 2`, `Community 7`?**
  _High betweenness centrality (0.053) - this node is a cross-community bridge._
- **Why does `send_notification()` connect `Community 2` to `Community 3`?**
  _High betweenness centrality (0.023) - this node is a cross-community bridge._
- **Are the 3 inferred relationships involving `start_record()` (e.g. with `converts_mp4()` and `generate_subtitles()`) actually correct?**
  _`start_record()` has 3 INFERRED edges - model-reasoned connections that need verification._
- **What connects `{ app, BrowserWindow, session }`, `fs`, `path` to the rest of the system?**
  _79 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Community 0` be split into smaller, more focused modules?**
  _Cohesion score 0.08205128205128205 - nodes in this community are weakly interconnected._
- **Should `Community 1` be split into smaller, more focused modules?**
  _Cohesion score 0.06354642313546423 - nodes in this community are weakly interconnected._