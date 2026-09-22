# 尊者监控直播短视频

**直播监控 · 短视频监控 · 转写落盘 · MCP 智能体接入**

本项目是基于开源项目 [DouyinLiveRecorder](https://github.com/ihmily/DouyinLiveRecorder)（MIT 协议）
二次开发的个人工具。基于**录制能力**（40+ 平台）扩展出监控调度、转写与智能体控制面。

> 📌 **权威文档：[`项目宪法.md`](./项目宪法.md)。** 本 README 只是入口，产品定义以宪法为准。

---

## 一、这个软件做什么

| # | 职责 | 说明 |
|---|---|---|
| 1 | **直播监控** | 检测关注的主播开播 → 自动录制 → 存到产物目录 |
| 2 | **短视频监控** | 检测博主发新视频 → 通知 → 按需下载 |
| 3 | **把文字转出来** | 用本地 whisper 转写，落盘成 `.转写.txt` + `.转写.json` |
| 4 | **MCP 控制面** | 智能体通过 MCP（43 个工具）控制本软件 |
| 5 | **交还控制权** | 智能体通过 `notes_list` 拿到转写文件绝对路径，用它自己的模型处理 |

### 明确不做

**本软件不接任何大模型 API。** 模型由连接它的智能体自带
（Cursor / OpenCode / 腾讯元宝 / 本机 oMLX 等）。

软件的职责边界是：**录下来 → 转成文字 → 放好 → 告诉智能体文件在哪。**
之后的"提炼知识点、写复盘笔记"是**智能体用它自己的模型**做的事，不是本软件的功能。

---

## 二、三层架构

```
界面层    Electron 桌面端（desktop/）
            │  走 IPC 调 Rust 二进制
核心层    Rust daemon  zunzhe-core（zunzhe-core/）
            │  直播监控 / 视频监控 / 下载队列 / 调度 / 通知 / MCP / SQLite
            │  spawn 子进程
工具层    Python  fupan/（faster-whisper 转写 / ffmpeg 切片 / 邮件）
```

| 层 | 位置 | 二进制/入口 | 职责 |
|---|---|---|---|
| 界面 | `desktop/` | Electron | 图形界面、托盘、检查更新 |
| 核心 | `zunzhe-core/src/` | `target/release/zunzhe-core` | 监控调度、任务队列、MCP、SQLite |
| 工具 | `fupan/` | `.venv/bin/python -m fupan.pipeline` | 转写、切片、邮件 |

**daemon 是主控。** Python 层是被 daemon 调度出来的工具，不是日常主入口。

---

## 三、目录结构（真实的）

```
尊者监控直播短视频/            ← 程序与配置（真身在外置盘 /Volumes/A区/）
├── config/                    配置文件
│   ├── config.ini             主配置（[路径设置] 决定产物落哪）
│   ├── fupan_config.ini       复盘配置
│   ├── URL_config.ini         直播监控源
│   └── 视频监控列表.ini        短视频监控源
├── desktop/                   Electron 界面 + 打包脚本
├── fupan/                     Python 工具层
│   ├── pipeline.py            流水线入口（转写/提炼/切片/邮件）
│   ├── transcribe.py          本地 whisper 转写
│   ├── clip.py                切片
│   ├── notify.py              邮件通知
│   └── ...
├── zunzhe-core/               Rust 核心
│   ├── src/                   源码（extension.rs = MCP 控制面）
│   └── target/release/        二进制
├── scripts/                   构建、发布、安装、看门狗脚本
├── 项目宪法.md                ★ 唯一权威文档
├── 历史/                      历史存档（只读，不得作为依据）
└── docs/                      专题文档

/Volumes/A区/尊者产物/          ← 所有产物（程序与数据物理分离）
├── fupan_output/              复盘产物（转写文本等）
├── downloads/                 录像与下载
├── database/zunzhe-core.db    数据库
└── logs/                      日志
```

---

## 四、快速开始

### 4.1 MCP（推荐，给智能体用）

把 `mcp.json.example` 里的路径复制到智能体的 MCP 配置。主服务：

```
<项目绝对路径>/zunzhe-core/target/release/zunzhe-core mcp
```

可用工具 **43 个**，按职责分组：

| 组 | 工具 |
|---|---|
| 直播监控 | `live_status` `live_list` `live_add` `live_remove` `live_delete` `live_check` `live_set_notify` `record_live` |
| 短视频监控 | `video_add` `video_remove` `video_list` `video_check_now` `blogger_videos` |
| 下载 | `download_video` `video_download` `download_videos` `video_library` |
| **复盘产物** | **`notes_list`**（列产物+绝对路径）、**`process_media`**（跑流水线）、`export_training_data` |
| 服务控制 | `service_status` `service_start` `service_stop` `ensure_services` `tail_logs` `doctor` |
| 配置 | `config_get` `config_set` `config_set_bulk` `config_schema_tool` `app_prefs_get` `app_prefs_set` `wizard_done` |
| 邮箱通知 | `email_status` `email_setup` `test_email` `test_notify` `notify_list` `notify_retry` `notify_prune` |
| 状态 | `status` |

### 4.2 CLI

```bash
cd /Volumes/A区/尊者监控直播短视频
BIN=zunzhe-core/target/release/zunzhe-core

$BIN doctor        # 环境诊断
$BIN status        # 状态快照
$BIN notes         # 列出复盘产物（含绝对路径）
$BIN paths         # 路径解析（只读，不建目录）
$BIN start         # 启动 daemon
```

所有 CLI 命令输出一行 JSON。

### 4.3 桌面端

```bash
cd desktop
npm install
npm run build
npm start
```

打包发布见 `docs/打包发布操作手册.md`。

---

## 五、产物怎么找（给智能体看的）

**唯一入口：MCP 工具 `notes_list`。** 它返回每个场次的转写文件**绝对路径**：

```json
{"notes":[{
  "anchor": "从“封存”聊到公知的变化",
  "session": "[B站] 从“封存”聊到公知的变化",
  "transcript_file": "/Volumes/A区/尊者产物/fupan_output/.../[B站] ....转写.txt"
}]}
```

实测（2026-09-21）：**216 个场次，201 个转写文件真实存在。**

目录规则：

```
<产物根>/fupan_output/{视频标题}/{[平台] 场次名}/
    {[平台] 场次名}.转写.txt      ← 转写正文（带时间戳）
    {[平台] 场次名}.转写.json     ← 结构化分段
    流水线结果.json               ← 本次摘要
```

智能体拿到 `transcript_file` 后，用它**自己的模型**处理即可。

---

## 六、平台支持

录制能力继承自上游 DouyinLiveRecorder，支持 **40+ 平台**（抖音、B站、快手、虎牙、
斗鱼、小红书、YouTube、TikTok、Twitch 等，完整列表见上游仓库）。

本项目的平台适配现状（`zunzhe-core`）：

| 平台 | 直播 | 短视频/投稿 | 备注 |
|---|---|---|---|
| B站 | ✅ Rust 原生（免登录） | ✅ 官方 WBI 接口 + RSSHub + yt-dlp 三级兜底 | 投稿接口有风控，建议配 cookie |
| 抖音 | ✅ Python 链路（免登录） | ⚠️ 需登录态 cookie | 详见 `docs/抖音历史视频回捞操作手册.md` |
| 快手 | ✅ Python 链路（免登录） | — | |
| 其它 | 按上游支持 | — | |

> **实测分界线（2026-09-17）**：**直播全平台免授权可用；投稿/作品列表全平台反爬。**
> 这是平台策略差异，不是实现问题 —— 直播是平台引流资产，投稿是核心内容资产。

---

## 七、文档导航

| 文档 | 内容 | 状态 |
|---|---|---|
| **`项目宪法.md`** | ★ 产品定义 + 实测事实 + 禁止事项 | **权威** |
| `CONTRACT.md` | 技术层工程契约（路径/验证门槛/历史证据） | 有效 |
| `使用说明.md` | 面向使用者的详细说明 | 有效 |
| `PROJECT_GOAL.md` | 当前阶段与已交付能力清单 | 有效 |
| `安全审查报告.md` | 上游项目安全审查 | 有效 |
| `docs/打包发布操作手册.md` | 打包与发布流程 | 有效 |
| `docs/安全审计与加固报告.md` | 本项目安全审计 | 有效 |
| `docs/抖音历史视频回捞操作手册.md` | 抖音回捞操作 | 有效 |
| `docs/备份软件抖音监控破解分析.md` | 二手软件解剖记录 | 有效 |
| `docs/调研结论-2026-09-21.md` | 本轮实测调研 | 有效 |
| `docs/全项目调研报告-2026-09-21.md` | 全项目体检（代码/入口/逻辑/文档） | 有效 |
| `docs/项目调研方法论.md` | 可复用的调研清单 | 有效 |
| `docs/历史/Rust重构记录.md` | Rust 重构施工日志（97 节） | **只读，不得作为依据** |
| `历史/` | 历史存档（08~09 月旧报告） | **只读，不得作为依据** |

---

## 八、合规提醒

录制内容仅保存在本地，供个人学习复盘使用。请遵守各平台服务条款，尊重主播的内容版权，
不要传播录制的视频和切片。

---

## 九、许可

本项目基于 [DouyinLiveRecorder](https://github.com/ihmily/DouyinLiveRecorder)（MIT 协议）二次开发。
上游作者与贡献者名单见上游仓库。二次开发部分同样以 MIT 协议发布，详见 `LICENSE`。
