# AGENTS.md — 给所有 AI 智能体的第一条指令

> 这个文件是本项目的 **AI 智能体强制入口**。
> 你（任何模型：Claude / GPT / Cursor / Codex / 本地模型）进入这个项目，
> **第一个动作就是读 `产品手册.md`**（本软件唯一产品与操作权威），然后再读本文件。

---

## ⛔ 三条硬红线（违反即失职）

### 红线 1：本软件不接任何大模型 API

**用户原话**：

> 「这是直播监控，短视频监控，然后把文字的分别转出来，这些都放在自己该放的地方。
>  这个软件有 MCP，智能体通过 MCP 控制这个软件，转文字通过 MCP 知道在哪，
>  智能体找到就上传自己的了。**谁和你说接模型 API 了。**」

所以：

- ❌ **不要**把 `[知识提炼] api地址/api密钥` 为空当成"缺陷"去修
- ❌ **不要**要求用户提供大模型 api 地址 / 密钥
- ❌ **不要**把"复盘笔记没产出"当成 P0 bug 报上去
- ✅ 软件负责：**录下来 → 转成文字 → 放好 → 用 MCP 告诉智能体文件在哪**
- ✅ 之后的处理，由**智能体用它自己的模型**完成

### 红线 2：判定优先级不许颠倒

```
1. 用户口述                      ← 最高
2. 产品手册.md（实测佐证过的）
3. 现场实测（命令输出/文件内容/数据库/日志）
4. 代码本身
5. 其它历史文档                  ← 最低，且必须标明"可能过期"
```

**历史文档（`.trash-作废文档/` 存档）只能当线索，不能当依据。** 想引用它，先实测一遍。

本项目历史上就是因为 AI 拿一堆自相矛盾的历史文档当依据，
把不该做的功能当 P0 缺陷、把错误设计写成"验收标准"。

### 红线 3：发现 ≠ 授权

**用户原话**：

> 「为什么每次你都不找我确认，就开始自己瞎鸡巴干？」

- 看到问题 → **只报告 + 给选项**，等用户点头才动手
- 用户可以只是"想知道"，完全不打算改
- **决策类改动**（改配置策略、改删除行为、改打包流程、改产品功能）**一律先问**
- 用户问「X 在干什么」→ **只回答 X**，不要扩成「顺手帮你改好 X」

---

## ⛔ 禁止事项（操作层，**本项目唯一出处**）

> 其它文档（`CONTRACT.md` 等）**不再重复这张表**。
> 修改禁止事项 → **只改这里**。

**A 组 · 不可逆的操作**

1. 未经明确授权，**不停止、不迁移**生产 daemon 与生产数据库，**不重载 launchd 服务**。
2. **不删除、不覆盖**现有数据库、日志、产物目录。
3. 不使用 `git reset --hard`、`git checkout --`，**不撤回其他会话的修改**。

**B 组 · 不许假装成功**

4. **不把"有代码""能调用""日志有输出""按钮存在"当成端到端成功。**
5. **不伪造** Cookie、授权文件、平台响应、转写产物来通过验收。
6. **每个结论必须有能证伪它的实测**：命令输出、文件内容、或日志。没有证据的结论不许出口。
7. **不做静默失败**：出错必须把原因写进可读字段（日志 / `last_error` / 返回结构），不许只写一行字符串了事。

**C 组 · 本项目的特有红线**

8. **不许把"未配置大模型接口"当成故障上报或修复**（见红线 1）。
9. **不许引用历史文档当依据**（必须先实测，见红线 2）。

---

## 一、项目是什么

个人自用的 **直播监控 · 短视频监控 · 转写落盘 · MCP 智能体接入** 工具。
基于开源 DouyinLiveRecorder（MIT）二次开发。

三层架构：

```
Electron 桌面端  →  Rust daemon（zunzhe-core，主控）  →  Python 工具层（fupan/，被调度）
```

---

## 二、必读顺序

1. **`产品手册.md`** ← 唯一权威文档：产品定义 + 实测事实 + 操作 + 修复计划（含进度）
2. `CONTRACT.md` ← 技术层契约：验证门槛、防回归、架构契约
3. 本文件（AGENTS.md）← 红线与禁止事项

**不要读 `.trash-作废文档/` 里的文件当依据**（旧报告与旧施工日志，
只用于查"当初为什么这么写"，引用前必须先实测）。

---

## 三、关键路径（实测）

| 项 | 真值 |
|---|---|
| **项目真身** | `/Volumes/A区/尊者监控直播短视频`（外置盘） |
| `~/Projects/尊者监控直播短视频` | **符号链接** → 上面那个，进哪个都一样 |
| `~/Desktop/尊者监控直播短视频` | **已不存在** |
| **产物根** | `/Volumes/A区/尊者产物` |
| 数据库 | `/Volumes/A区/尊者产物/database/zunzhe-core.db` |
| Rust 二进制 | `<项目>/zunzhe-core/target/release/zunzhe-core` |
| MCP 工具数 | **20 个**（2026-09-24 实测 `tools/list`：新增 `check_update`/`apply_update` 更新链路；`.神经总账/控制面.json` 是**快照，会过期**） |
| MCP 控制面快照 | `.神经总账/控制面.json`（**不是** `.神经/控制面.json`，后者是旧残留） |
| **B站直播监听** | **弹幕 WS 实时监听（录播姬同款，2026-09-24 起）**：getDanmuInfo → WSS → 30s 心跳 → `LIVE` 事件 → 提前唤醒 live tick；检测/录制/通知/防重复用 LiveMonitor 状态机，**300 秒轮询降为对账兜底**；掉线自动退避重连 |
| **守护职责** | **桌面端 watchdog**（GUI 会话进程）。launchd 域（gui/501）进程**无法访问外置盘** `/Volumes/A区` → **launchd 无法托管本 daemon**；`com.zunzhe.healthcheck`（launchd 版健康检查，每 60 秒访问外置盘被 TCC 拒绝持续刷错）**已于 2026-09-24 停用**，plist 保留可恢复；`scripts/launchd/*` 仅备用 |
| **日志目录** | `~/Library/Logs/zunzhe`（config.ini `[路径设置] 日志目录` 显式配置，2026-09-23 起） |
| **打包真身（改 main 层必须改这里）** | `desktop/electron/main.cjs`（package.json `main: electron/main.cjs`、build.files `electron/**`；含 runtimeDir/数据目录指针/托盘菜单/ipcMain.handle 全量）。**`desktop/main.cjs` 是死文件，改它不进包**——1.5.3/1.5.4 曾把修复改在死文件上导致未生效，2026-09-24 已全部移植回真身 |
| **数据目录指针** | `~/Library/Application Support/尊者直播监控/数据目录.txt`（第一行为数据根路径；曾误指向 userData 导致数据根分裂、watchdog 拉不起 daemon，2026-09-24 已修正为 `/Volumes/A区/尊者监控直播短视频`；有 `ZUNZHE_PROJECT_DIR` 环境变量覆盖） |
| **更新机制** | App 内「检查更新」= 本地更新源 `releases/尊者直播监控-<version>.zip`（electron 层 `update-run.cjs`，31 项单测）；发布走 `publish-release.sh --build`，用户程序内一键更新，无需手动装 |
| **当前已装版本** | **1.6.4**（2026-09-24 15:16；抖音登录窗口伪装 Chrome UA 降低风控误判 + 自动点登录增强 + 风控可读提示；1.6.3 含抖音未授权警示/未授权源不计冷却/日志修复；更新源只留最新版 zip，旧版自动清理） |
| **前端代码结构（2026-09-24 拆分）** | `desktop/src/App.jsx`（1086 行，主逻辑与卡片区）+ 8 个独立文件：`DetailModal.jsx`（详情弹窗）/`AddModals.jsx`（添加直播+视频）/`ItemSettingsModal.jsx`（主播设置）/`SettingsModal.jsx`（全局设置）/`HistoryModals.jsx`（批量下载+历史视频）/`NotifyModal.jsx`（通知治理）/`ui-constants.js`/`platform-utils.js`。**改弹窗去对应文件，不要堆回 App.jsx**；每步 `npm run build` 验证 + AST 缺失引用扫描（黑屏根因 = 用未 import 的变量） |

> ⚠️ **`.神经总账/控制面.json` 是静态快照，会与真实工具集不一致**（曾出现快照 41 / 实测 43）。
> **要准确数字，永远实测 `tools/list`，不要读这个 json。**

---

## 四、常用命令

```bash
cd /Volumes/A区/尊者监控直播短视频
BIN=zunzhe-core/target/release/zunzhe-core

pgrep -fl "run-daemon"   # daemon 状态（run-daemon --interval 默认 300 秒）
$BIN doctor              # 环境诊断
$BIN status              # 状态快照
$BIN notes               # 列出复盘产物（含绝对路径）
$BIN paths               # 路径解析（只读）

# 直连 MCP 测试（本机没有 timeout 命令）
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"p","version":"1"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"notes_list","arguments":{}}}' \
  | $BIN mcp

cd zunzhe-core && cargo test --all-targets       # Rust 测试
npm test --prefix desktop                        # 桌面端测试
```

---

## 五、智能体与本软件的协作方式

```
1. 智能体通过 MCP 让软件去监控（`monitor_add` / `monitor_remove` / `monitor_list` / `monitor_set`，P3 已完成 43→18 收敛）
2. 软件自动录制 / 下载
3. 软件自动调本地 whisper 转写               → 落盘 .转写.txt
4. 智能体通过 MCP 调 notes_list              → 拿到转写文件绝对路径
5. 智能体用自己的模型处理转写文本             ← 这一步在软件之外
```

**第 5 步是智能体的活，不是软件的活。** 这是本项目最容易被误解的地方。

---

## 六、环境坑（实测，避免重走）

| 坑 | 真相 |
|---|---|
| `timeout` 命令 | **本机没有**（macOS 默认无 coreutils） |
| `ps` | 被 TCC 完全禁止，只能用 `pgrep -fl` / `lsof` / `lsappinfo` |
| `osascript ... Finder delete` | 在外置卷 A区上失败（TCC -10004） |
| `trash-put` / `gio` / `/usr/bin/trash` | **都不存在** |
| `launchctl bootstrap` | 在受限环境恒失败（`Input/output error`），但 `bootout` 会成功 ⇒ **不要 bootout** |
| **launchd 访问外置盘** | launchd 域进程对 `/Volumes/A区` 全部被系统策略 deny（实测 `Operation not permitted`；log show 见 `xpcproxy deny file-read-data`）——外置盘项目**不能靠 launchd 守护**，只能靠桌面端 watchdog |
| `~/.Trash` | 读不了（Operation not permitted），但 `lsof -p <pid>` 能穿透读出 |
| 打包态 `PROJECT_DIR` | 硬编码 `~/Projects/尊者监控直播短视频`（经符号链接落到外置盘） |
| Shell `$VAR` 后接中文 | 必须写 `${VAR}`，否则 bash 把全角字符当变量名 |

---

## 七、你如果发现问题

**报告，不要动手。** 格式：

```
现象：（实测命令 + 输出）
影响：
候选方案：（2-3 个，说明取舍）
我的建议：
等你确认。
```

---

*本文件由用户要求建立于 2026-09-21，目的是让任何 AI 智能体进入项目时不被历史文档干扰。*
