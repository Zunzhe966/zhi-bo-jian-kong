# 项目目标与现状（上下文恢复锚点）

> 📌 **产品定义以 [`项目宪法.md`](./项目宪法.md) 为准。**
> 本文件是**进度与已交付能力清单**，不承担产品定义职责。

---

## 一、项目是什么

个人自用的多平台**直播监控 · 短视频监控 · 转写落盘 · MCP 智能体接入**工具。
macOS 优先，兼容 Windows。内容仅本地保存。

- GitHub 私有仓库：`Zunzhe966/zun-zhe-zhi-bo-jian-kong`
- **正式路径（唯一真身）**：`/Volumes/A区/尊者监控直播短视频`（外置盘）
- `~/Projects/尊者监控直播短视频` = **符号链接** → 上面那个（2026-09-17 建立）
- `~/Desktop/尊者监控直播短视频` = **已不存在**

---

## 二、目标架构

```
界面层    Electron 桌面端（desktop/）
                │ IPC（调 Rust 二进制）
核心层    Rust daemon  zunzhe-core
  ├─ 调度器 + 主播生命周期状态机
  ├─ SQLite：videos / tasks / notify / live_states / monitor_baseline ...
  ├─ Platform trait（bilibili / douyin / kuaishou ... 每平台一适配器）
  ├─ 下载队列（单/批量/全部，失败重试，成功标记 downloaded）
  ├─ 直播监控（Rust 原生 B站 / Python 链路 抖音快手）
  ├─ 通知总线（邮箱默认 + 每主播覆盖 + 本地兜底）
  ├─ 复盘流水线桥接（spawn Python 子进程）
  ├─ 训练数据导出（JSONL）
  └─ MCP Server + JSON CLI（43 工具）
                │ spawn 子进程
工具层    Python  fupan/
  └─ faster-whisper 转写 / ffmpeg 切片 / 邮件 / 平台解析
```

**核心设计**：daemon 是主控，Python 是被调度的工具层。
**边界**：软件不接大模型 API（见宪法第 1 条）。

---

## 三、核心需求

1. 个人自用，非工业级，但底层按可靠设计
2. 平台：**macOS 优先**，兼容 Windows，未来 Linux/云服务器
3. **直播监控**：多平台开播检测 + 自动录制
4. **短视频监控**：博主发布视频监控 + 选择性下载（标题/封面/日期/时长/已下载标记）
5. **转写**：本地 whisper，带时间戳，落盘到固定目录
6. **MCP 控制面**：智能体通过 MCP 控制本软件，并通过 `notes_list` 找到转写文件
7. **通知**：开播→邮箱；发视频→邮箱；每主播可覆盖；本地兜底
8. 训练数据沉淀（转写文本结构化保存 + 导出 JSONL）

---

## 四、已交付能力（实测）

### 4.1 Rust 核心

- 全量测试通过；`cargo clippy -D warnings` 零告警
- SQLite 持久化、崩溃可恢复、任务失败有上限有退避
- 单一数据库权威：`<产物根>/database/zunzhe-core.db`
- 路径全部可配置：`zunzhe-core/src/paths.rs`（唯一事实来源）
  - 改造前产物写死在项目目录；现在由 `[路径设置] 产物根目录` 决定
  - `paths` 命令是**纯只读**：只解析配置返回路径，不建目录、不改数据库

### 4.2 平台适配

- B站：官方 WBI 接口 + RSSHub + yt-dlp 三级兜底
- B站下载：优先 Electron 浏览器内核提直链，失败回退 yt-dlp
- 抖音 / 快手：Python 链路 `fupan/live_resolve.py`（直播免登录）
- 投稿监控带失败冷却：连续失败 3 次起冷却 15 分钟、指数增长上限 2 小时

### 4.3 Cookie / 授权链路

- B站 cookie 三级按序使用：用户配置 → 桌面端浏览器授权 → 匿名 buvid 设备 cookie
- 桌面端「B站授权」导出 `fupan_output/run/bilibili_browser_cookies.txt`
- **cookie 落点约定**：禁止用 `__dirname` 推导（打包态会写进 app.asar 污染签名）。
  统一走 `ZUNZHE_RUN_DIR` → `ZUNZHE_PROJECT_ROOT` → `__dirname`（仅开发态）

### 4.4 复盘流水线

| 步 | 动作 | 状态 |
|---|---|---|
| 1/4 | 转写（本地 whisper） | ✅ **正常工作，已产出 201 个 `.转写.txt`** |
| 2/4 | 知识提炼（外部大模型 API） | ⚠️ **设计偏差，见宪法第 3 条** |
| 3/4 | 精华切片 | 卡在第2步之后 |
| 4/4 | 邮件通知 | 卡在第2步之后 |

实测：58 分钟 B站视频在纯 CPU + `small` 模型下约 26 分钟完成转写，
生成 98,097 字节 `.转写.txt` + 179,736 字节 `.转写.json`（1551 分段，时长 3517.24 秒）。

### 4.5 MCP 控制面

- 43 个工具，源码契约 `zunzhe-core/tests/control_plane_contract.rs` 断言
- 控制面清单：`.神经总账/控制面.json`
- 实测 `notes_list` 返回 216 场次、201 个转写文件绝对路径

### 4.6 服务与看门狗

- launchd `com.zunzhe.monitor`（`KeepAlive`）+ `com.zunzhe.healthcheck`（每分钟）
- 重启职责只归一条链路：launchd `KeepAlive` + `scripts/check-rust-daemon.sh`
  - Electron `serviceWatchdog` **不得自己再调 `ensure-services`**
    （历史上三套互踩，一天 57 次重启）
- `service.rs::ensure_core()` 会调 `ensure_healthcheck_loaded()`（幂等），
  防止"唯一会自愈的组件自己没人守护"

### 4.7 更新机制

- 更新源 = `<产物根>/releases/`，全离线无服务器
- 设计铁律：**只换程序，永不碰数据目录**
- 实现：`desktop/electron/update-run.cjs`；界面侧栏「检查更新」按钮
- 发版：`bash scripts/publish-release.sh --build`
- 安装：`bash scripts/install-app-safe.sh`（顺序钉死为 停→锁处理→校验→备份→换→起→验）

### 4.8 安全

- Electron 沙箱 + IPC 白名单 + 预加载隔离
- 本地文件走受限制的 `local-file://` 协议，渲染进程不直连 `file://`
- CLI 命令注入：`Command::arg` 列表化传参，无拼 shell
- 供应链：ffmpeg 从官方源下载 + SHA-256 校验

---

## 五、当前待处理

### 5.1 产品定义偏差（唯一实质性代码问题）

`fupan/pipeline.py:123-126` 把"智能体该干的第2步"设成流水线硬门槛。
见 `项目宪法.md` 第 3 条。**待用户点头，未批准不许动。**

### 5.2 打包落后于源码

| 层 | 时间 | 内容 |
|---|---|---|
| app 内前端 | — | `index-Bajs-e4L.js`（旧） |
| `dist/` 前端 | 2026-09-21 09:03 | `index-CaocZset.js`（**未打进 app**） |
| `app.asar` | 2026-09-20 16:07 | 落后 |

⇒ 使用中的 app 不含最新源码。需重新打包安装。

### 5.3 转写覆盖率

216 场次中 201 有转写文件，**15 个缺失**。

---

## 六、关键约定

- 所有改动先在 macOS 实测通过再提交
- 改动即提交（git），保持工作区清晰
- 图谱重生成：`graphify . --update`（输出 `graphify-out/`）
- 训练数据格式：JSONL，一行一条（含时间戳/主播/日期/元数据）
- **文档纪律见宪法第 6 条**：权威文档只有 `项目宪法.md`，历史文档只读

---

## 七、上下文丢失恢复步骤

1. **读 `项目宪法.md`**（产品定义 + 实测事实 + 禁止事项）
2. 读本文件（进度与已交付能力）
3. `git status` 看未提交改动
4. `pgrep -fl "run-daemon"` 看 daemon
5. `zunzhe-core/target/release/zunzhe-core doctor` 看环境
6. 从「当前待处理」继续

**不要读 `历史/` 目录里的文件当依据。**
