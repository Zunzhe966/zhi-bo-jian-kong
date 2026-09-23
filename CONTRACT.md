# 尊者直播监控工程契约（技术层）

> ⚠️ **权威文档已迁至 [`产品手册.md`](./产品手册.md)。**
> 本文件保留为**技术层工程契约**（路径契约、验证门槛、架构契约），
> **产品定义以 `产品手册.md` 第 1 条为准**。
> 两者冲突时：产品定义 → 产品手册；技术细节 → 本文件。
>
> 本文件只记录**规则**（不可漂移的技术状态、禁止事项、验证门槛、回滚方式、维护顺序）。
> 历史过程记录已归档，不作为依据。

更新：2026-09-23（文档整顿，删除会话流水账与过期接口契约）

---

## 0. 【最高优先级】产品定义不看本文件，看 `产品手册.md`

**本软件不接任何大模型 API。** 转写之后的理解、总结、提炼，由智能体用
它自己的模型完成，不是本软件的功能。任何"修复未配置 API"类改动都是错误方向。

## 1. 冻结的正确状态

- 程序与配置在项目目录；产物在可配置的 `[路径设置]` 根目录。`产物根目录`、`短视频下载目录`、`复盘输出目录`、`日志目录`、`数据库文件` 空白时，按 `zunzhe-core/src/paths.rs` 的默认规则回退。
- 生产运行状态以 `zunzhe-core/src/paths.rs`、`config/config.ini`、`launchctl print`、进程与日志现场为准。
- `zunzhe-core` 承载直播监控、视频监控、状态快照、任务队列、通知与 MCP 控制面；Python `fupan` 只承担被调度的工具层。
- 复盘产物、视频库、复盘笔记和转写队列必须使用同一个 `[路径设置]` 产物根。`extension.rs` 不得绕过统一路径解析去写 `project_dir.join("fupan_output")`。
- 复盘流水线 `pipeline_summary.status` 为 `pending/failed` 时，必须保留真实 `attempts` 与 `last_error`；进入 `done` 必须形成真实队列任务和真实产物。

## 2. 禁止事项 → 见 `AGENTS.md`

**禁止事项的唯一出处是 [`AGENTS.md`](./AGENTS.md)「禁止事项（操作层，本项目唯一出处）」。**
本文件不重复该表——同一约束写在两处，改一处忘一处就会互相矛盾。

## 3. 已知病根与事实边界

- 平台源失败清零、B 站兜底、抖音授权闭环、SQLite WAL/慢查询、日志双份与路径漂移等问题，必须用现场日志、数据库记录和可复现测试判断，不能只凭旧报告宣称已修或未修。
- 工作区允许同时存在其他会话的未提交修改。任何编辑前先核对目标文件的 `stat` 与 `sha`，不得覆盖不属于当前任务的改动。
- 行为改动先写失败测试，再写最小实现。路径类改动必须使用非默认路径的隔离项目验证。

## 4. 验证门槛

- 路径类改动必须使用 `test_support::isolated_project`、`crate::paths::{logs,run_dir}` 或等价的隔离夹具，不能把测试写到真实项目目录。
- 涉及 daemon 和调度器的行为，至少通过 `cargo check`、`cargo test -q --all-targets`；集成目标也要单独记录通过数。
- 桌面端改动运行 `npm test`；脚本改动运行 `bash -n` 和对应的真实执行回归。
- 每个修复必须留下"先失败、后通过"的证据，或者明确说明为什么该边界无法自动化验证。

## 5. 回滚方式

- 代码回滚以 Git 提交为单位；生产状态回滚必须先停止变更、保留现场，再由用户决定。
- 任何生产状态变更若发生，必须先停止变更、保留现场，再由用户决定回滚。

## 6. 维护顺序

代码改动先通过测试，再更新本契约；本契约与旧报告冲突时，以可复现的现场证据和本文件为准。

---

## 7. 技术契约要点（路径 / 媒体 / 任务 / 日志）

### 7.1 路径只读契约

`paths.rs` 的路径查询（`storage`、`media_root`、`downloads`、`fupan`、`logs`、`covers`、`run_dir`、`database`、`known_roots`）是**纯只读能力**：只解析配置并返回路径，不创建目录、不迁移文件、不修改数据库。目录创建只允许显式调用 `paths::ensure_dirs` 的启动/命令入口执行。

路径可信边界必须跟着 `[路径设置]` 与 `[录制设置] 直播保存路径` 解析结果走，兼容升级前 `<项目根>/downloads` 与 `<项目根>/fupan_output`。媒体入队使用 `paths::is_trusted_media_path`，不能在调用方另写死项目根路径。

macOS 的 `/var` 与 `/private/var` 属于同一真实路径。路径比较必须使用 `canonical_lenient`，目标尚不存在时也应解析最近存在祖先再拼回剩余路径。

### 7.2 损坏媒体预检

复盘入队 `download::enqueue_process` 在写库前执行两道真实性检查：文件必须存在且是普通文件；已知媒体后缀必须用 `ffmpeg -v error -i <path> -t 0 -f null -` 做零时长探测。探测启动失败、20 秒超时或非零退出都必须返回包含真实原因的错误，且不得留下 `pending` 任务。

`PROCESS_MEDIA_SUFFIXES` 必须与 `fupan/pipeline.py` 的 `_MEDIA_SUFFIXES` 对齐：
`mp4/ts/mkv/flv/mov/webm/mp3/m4a/wav`。未知后缀不做内容格式猜测。

损坏媒体错误的截断上限为 500 字符，必须让调用方读到"媒体文件损坏或无法读取"及 ffmpeg 的关键诊断；不得先谎报"已加入队列"。

自动下载成功后若复盘入队失败，`DownloadResult` 必须保留 `pipeline_error`，并由 `pipeline_warning()` 生成"下载成功，但自动复盘入队失败: ..."供展示。下载任务本身仍可标 `done`，但不得静默吞掉复盘失败原因。

### 7.3 process 入队后的后端可达性

`process` 命令先写入真实任务，再确保 daemon 会消费该任务：daemon 已运行时只探测；未运行时必须调用 `service::ensure_core` 拉起。结果必须同时返回 `queued`、`task_id`、`disposition`、`daemon_running` 和 `startup_error`，不能只报"已入队"而让任务永久停在 `pending`。

daemon 启动失败不回滚已落库的任务：任务仍可显示在队列中，但 `startup_error` 必须带回桌面端。前端 `processEnqueueFeedback` 在 `queued=true` 且 `startup_error` 非空时必须显示 warning，禁止伪装成普通 success。

### 7.4 日志目录一致性

日志只有一份权威配置源：`[路径设置] 日志目录`；留空时为 `<产物根目录>/logs`。仓库里的 launchd 模板、渲染脚本、Rust `paths` 和 Python 设置必须都指向同一目录。

`doctor` 与桌面端 `status` 必须返回机器可读的 `log_paths`，至少包含 `configured_dir`、`primary`、`error`、`consistent`、`hint`。`primary/error` 要报告配置路径、旧 `~/Library/Logs/zunzhe` 路径、实际读取路径和来源（`configured` / `legacy_fallback` / `missing`）。

`tail-logs` 可以读取旧目录中的历史日志作为迁移兼容，但只有配置目录不存在时才回退；UI 必须显示实际读取的文件路径，并在 `consistent=false` 时警告"后台服务仍在写历史日志目录"。

修复日志漂移必须同时覆盖渲染、磁盘 plist 和已加载任务，不能只改仓库模板。
只读审计入口：`bash scripts/audit-launchd-log-paths.sh`（不 reload、不重启、不修改任何文件）。
重载 launchd 会中断当前 daemon 的长跑 pid，必须在得到明确运行授权后执行。

### 7.5 双数据库权威性与合并规则

权威运行库为**外置产物库** `<产物根>/database/zunzhe-core.db`（WAL/SHM 同目录）。
项目根旧库 `<项目根>/zunzhe-core.db` 是迁移前事实，不再代表当前运行库。

合并规则：
- 禁止用项目根库整库覆盖外置库（会回退已修正路径）；也禁止用外置库整库覆盖项目根库（会丢项目根库独有任务）。
- 合并前必须先停 daemon 并备份两库；任务运行期间不得执行合并或 daemon 切换。
- 合并动作只包含：把项目根库独有的任务插入外置库；对共同任务保留外置库中的已改写 `url`，再按项目根库中确实更新的运行字段做显式白名单更新。不得批量按 `updated_at` 取整行。

---

## 8. 已验证基线（历史证据摘要，非规则）

- Rust：`cargo test --all-targets` 基线 **289 passed / 0 failed / 1 ignored**；`cargo clippy -D warnings` 零告警；`cargo fmt --check` 通过。
- 桌面端：`npm test` 基线 **8 passed / 0 failed**。
- 控制面：真实 stdio `tools/list` = **43 个工具**（源码契约 `tests/control_plane_contract.rs` 断言同数）。
- 转写链路：58 分钟 B站视频在纯 CPU + `small` 模型下约 26 分钟完成转写，产出 `.转写.txt` + `.转写.json`，证明转写链路可产出真实文件。
- 监控源失败冷却：已实盘证实 `consecutive_fails` 跨轮累加（不是永远停 1）；进入冷却阈值后的跳过行为以现场数据为准。

---

## 9. 架构契约（Rust 重构验收标准与模块清单）

### 9.1 重构的最终目标（验收标准）

1. **长期稳定可用**：`zunzhe-core` 可 **24 小时 × 30 天**持续运行，不崩溃、不丢状态、崩溃后重启自恢复（SQLite 持久化 + 任务队列重放 + 日志）。
2. **原子模块化**：每个最小功能是一个独立模块，模块间只通过公开 trait + 结构体契约连接。
3. **改动不破坏**：任何模块改动都必须先跑全量回归（`cargo test` + 集成测试），已验收功能回归失败 = **不允许提交**。
4. **能力对齐**：Rust 逐个重写旧 Python 能力 —— 直播监控/录制调度、发布视频监控、历史视频浏览与选择性下载、转写/切片复盘桥接、邮箱通知、视频库、CLI/MCP 命令集。
5. **平台**：macOS 优先可用，Windows/Linux 可编译运行。
6. **简单可用**：个人用户不用懂内部实现，桌面端/CLI 一键操作。

### 9.2 原子模块清单

| 模块 | 职责 | 关键公开接口 |
|---|---|---|
| M1 `config` | 读写 INI 配置、QQ 邮箱模板 | `Config::load()`, `Config::email_status()` |
| M2 `model` | 统一类型 | `VideoMeta`, `Anchor`, `Task`, `NotifyEvent`, `Transcript`, `KnowledgePoint` |
| M3 `db` | SQLite schema + CRUD | `init_db`, `upsert_video`, `mark_downloaded`, `list_videos`, `enqueue_task` |
| M4 `platform` | 平台适配器 | `Platform::list_videos(url, count)`, `BilibiliAdapter`, `RssAdapter` |
| M5 `video_library` | 视频库/封面缓存/已下载 | `upsert`, `list`, `mark_downloaded`, `cache_cover` |
| M6 `training_data` | 训练数据 JSONL | `export(anchor, output)` |
| M7 `download` | 下载队列 | `Downloader::download(url, name)`, `process_pending` |
| M8 `notify` | 通知总线 | `Notifier::send(event)` |
| M9 `pipeline_bridge` | 桥接 Python 工具层 | `run_pipeline(media, anchor)` |
| M10 `scheduler` | 24×30 守护调度 | `run_daemon()`，状态机/心跳/崩溃恢复 |
| M11 `cli` | JSON CLI | 子命令与 `fupan.cli` 对齐，每行 JSON |
| M12 `extension` | MCP stdio server | 工具与 CLI 对齐 |
| M13 `assembly` | daemon 组装/打包 | `zunzhe-core` 二进制 + 打包脚本 |
| M14 `watchlist` | 监控列表读写、监控偏好 | `Watchlist::list_live/list_video/add/remove` |
| M15 `config_api` | 配置文件 JSON API（逐行保留注释） | `ConfigApi::get/set/set_bulk/schema` |
| M16 `service` | 服务兼容层 | `ensure_started/stop_all/status` |
| M17 `live_monitor` | 直播检测/录制状态机 | `LiveAdapter`, `BilibiliLiveAdapter`, `LiveMonitor` |

> **接口命名以 `产品手册.md` 第 5/6 章为准**（CLI `动词-名词`，MCP `名词_动词`，同一套底层能力）。
> 历史 kebab-case 别名与 snake_case 旧工具名属待收敛项，不在本契约重复定义。

---

## 10. 防回归规则（每次改 Rust 必须）

1. `cd zunzhe-core && cargo fmt --check && cargo clippy --all-targets && cargo test`
2. 新增模块必须带 `#[cfg(test)]` 单测；涉及跨模块的加集成测试。
3. 全量回归通过后才 `git add` / commit；commit 信息注明模块（如 `feat(zunzhe-core): M1 config`）。
4. 不允许把已验收模块"顺手改坏"：若修改共用接口，**先更新本契约再实现**，并跑全量测试。

---

## 11. 稳定性验收（24×30）

- **daemon 循环**用 `tokio::select!`，任何单个任务 panic 不拖垮主循环；顶层安装 panic hook + 崩溃日志；直播检查与视频检查各自独立任务，`JoinError` 只记日志，主循环继续下一轮。
- **SQLite** 每次写操作事务化；崩溃后重启读取 DB 恢复主播/视频/任务/通知状态。
- **服务守护**：发现进程不在则拉起（launchd KeepAlive / 看门狗）；日志轮转（保留 5 份）；心跳文件 **30 分钟内**必须刷新，否则 `status` 判为不健康；磁盘满/ffmpeg 缺失等通过 `doctor` 可诊断。
- **冒烟测试**：`run-daemon --once` 与 `run-daemon --duration 30s` 不崩；`SIGTERM/SIGINT` 优雅退出并清理 pid 文件；长期稳定性由真实机器持续运行验收。

---

## 12. 历史说明

Rust 重构（M0–M17）已完成并通过全量回归（施工过程见 git 历史与 `.trash-作废文档/` 存档，不作为依据）。
**当前状态以 `产品手册.md` 为准。**
