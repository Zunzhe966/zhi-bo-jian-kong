# 尊者监控直播短视频

**直播监控 · 短视频监控 · 转写落盘 · MCP 智能体接入**

本项目是基于开源项目 [DouyinLiveRecorder](https://github.com/ihmily/DouyinLiveRecorder)（MIT 协议）
二次开发的个人工具。基于**录制能力**（40+ 平台）扩展出监控调度、转写与智能体控制面。

> 📌 **权威文档：[`产品手册.md`](./产品手册.md)。** 本 README 只是入口，产品定义、功能清单、修复计划全部以手册为准。

---

## 一、这个软件做什么（就五条）

| # | 职责 | 一句话 |
|---|---|---|
| 1 | **监控短视频** | 主播发新视频 → 发现它 → 下载下来 |
| 2 | **监控直播** | 主播开播 → 发现它 → 录制下来 |
| 3 | **转文字** | 本地 whisper 转写，**文字和视频放同一个文件夹** |
| 4 | **通知** | 开播/发新视频时发一封邮件。顶天了，没有别的 |
| 5 | **MCP 控制** | 智能体通过 MCP 控制本软件全部功能、查得到每个文件的位置 |

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
│   ├── fupan_config.ini       转写/切片配置
│   ├── URL_config.ini         直播监控源
│   └── 视频监控列表.ini        短视频监控源
├── desktop/                   Electron 界面 + 打包脚本
├── fupan/                     Python 工具层（转写/切片/邮件，被 daemon 调度）
├── zunzhe-core/               Rust 核心（src/ + target/release/ 二进制）
├── scripts/                   构建、发布、安装、看门狗脚本
├── 产品手册.md                ★ 唯一权威文档
├── AGENTS.md                  AI 红线（进入本项目的强制入口）
├── CONTRACT.md                技术层工程契约
├── README.md                  本文件（门面）
└── .trash-作废文档/            已作废的旧文档存档（不物理删除，不作依据）

/Volumes/A区/尊者产物/          ← 所有产物（程序与数据物理分离）
├── {主播名}/                  一级永远是主播名
│   ├── 直播/{日期 场次}/      场次.mp4 + 场次.转写.txt/.json（文字视频同目录）
│   └── 短视频/{标题}/         标题.mp4 + 标题.转写.txt/.json
├── database/zunzhe-core.db    数据库
└── logs/                      日志
```

产物目录三条铁律（详见产品手册第 3 章）：
- **R1** 直播和短视频在同一个主播目录下分两个子目录
- **R2** 一级目录只能是主播名
- **R3** 文字和视频在同一个文件夹里

---

## 四、快速开始

### 4.1 MCP（推荐，给智能体用）

把 `mcp.json.example` 里的路径复制到智能体的 MCP 配置。主服务：

```
<项目绝对路径>/zunzhe-core/target/release/zunzhe-core mcp
```

> **工具数量**：当前 43 个（含历史别名），**规划收敛为一套与 CLI 对齐的 `monitor_*` 主命令**（`anchor_search` / `monitor_add` / `monitor_remove` / `monitor_list` / `monitor_set` / `check_now` / `download_video` / `process_media` / `notes_list` / `status` / `paths` / `doctor` / `config_get` / `config_set` / `service_start` / `service_stop` / `tail_logs` / `test_email`）。完整定义与删除清单见 `产品手册.md` 第 6 章。

智能体典型用法：

```
用户：「帮我监控XXX」
智能体：anchor_search("XXX") → 确认候选 → monitor_add(url, name) → 完事

用户：「XXX 的转写呢」
智能体：notes_list → 按主播过滤 → 拿到绝对路径 → 自己读文件、用自己的模型处理
```

### 4.2 CLI

```bash
cd /Volumes/A区/尊者监控直播短视频
BIN=zunzhe-core/target/release/zunzhe-core

$BIN doctor        # 环境诊断
$BIN status        # 状态快照
$BIN notes         # 列出转写产物（含绝对路径）
$BIN paths         # 路径解析（只读，不建目录）
$BIN start         # 启动 daemon
```

所有 CLI 命令输出一行 JSON。完整命令集见 `产品手册.md` 第 5 章。

### 4.3 桌面端

```bash
cd desktop
npm install
npm run build
npm start
```

打包发布见 `scripts/publish-release.sh --build` 与 `scripts/install-app-safe.sh`。

---

## 五、产物怎么找（给智能体看的）

**唯一入口：MCP 工具 `notes_list`。** 它返回每个场次的转写文件**绝对路径**：

```json
{"notes":[{
  "anchor": "侯翠翠",
  "kind": "短视频",
  "session": "一个人自驾阿勒泰_风都是自由的",
  "media_file": "/Volumes/A区/尊者产物/侯翠翠/短视频/一个人自驾阿勒泰_风都是自由的/一个人自驾阿勒泰_风都是自由的.mp4",
  "transcript_file": "/Volumes/A区/尊者产物/侯翠翠/短视频/一个人自驾阿勒泰_风都是自由的/一个人自驾阿勒泰_风都是自由的.转写.txt"
}]}
```

智能体拿到 `transcript_file` 后，用它**自己的模型**处理即可。

---

## 六、平台支持

| 平台 | 直播 | 短视频/投稿 | 备注 |
|---|---|---|---|
| B站 | ✅ Rust 原生（免登录） | ✅ 官方 WBI 接口 + RSSHub + yt-dlp 三级兜底 | 投稿接口有风控，建议配 cookie |
| 抖音 | ✅ Python 链路（免登录） | ⚠️ 需登录态 cookie | 短视频源需先扫码授权 |
| 快手 | ✅ Python 链路（免登录） | — | |

> **实测分界线**：直播全平台免授权可用；投稿/作品列表反爬是平台策略，不是实现问题。
> 前端只列后端真实有适配器的平台（短视频：B站/抖音；直播：B站/抖音/快手），见产品手册冲突 #6 裁决。

---

## 七、文档导航

| 文档 | 角色 | 状态 |
|---|---|---|
| **`产品手册.md`** | ★ 产品定义 + 实测事实 + 操作 + 修复计划（含进度） | **唯一权威** |
| **`AGENTS.md`** | AI 红线与禁止事项（进入项目的强制入口） | 有效 |
| **`CONTRACT.md`** | 技术层工程契约（验证门槛/防回归/架构契约） | 有效 |
| **`README.md`** | 本文件，项目门面 | 有效 |
| `.trash-作废文档/` | 旧文档存档（项目宪法/使用说明/docs/历史等） | **只读，不得作为依据** |

---

## 八、合规提醒

录制内容仅保存在本地，供个人学习复盘使用。请遵守各平台服务条款，尊重主播的内容版权，
不要传播录制的视频和切片。

---

## 九、许可

本项目基于 [DouyinLiveRecorder](https://github.com/ihmily/DouyinLiveRecorder)（MIT 协议）二次开发。
上游作者与贡献者名单见上游仓库。二次开发部分同样以 MIT 协议发布，详见 `LICENSE`。
