# 尊者直播监控工程契约（技术层）

> ⚠️ **权威文档已迁至 [`项目宪法.md`](./项目宪法.md)。**
> 本文件保留为**技术层工程契约**（路径契约、验证门槛、历史证据），
> **产品定义以 `项目宪法.md` 第 1 条为准**。
> 两者冲突时：产品定义 → 项目宪法；技术细节 → 本文件。

更新：2026-09-21

本文件是代码、文档与运行状态冲突时的技术层优先级契约。这里记录不可漂移的技术状态、禁止事项、已知病根、验证门槛、回滚方式与维护顺序。

---

## 0. 【最高优先级】产品定义不看本文件，看 `项目宪法.md`

**本文件第 9 节曾把「未配置大模型接口」记录为"已修复的状态"，这是错误的收编。**

用户 2026-09-21 明确：

> 「这个软件有 MCP，智能体通过 MCP 控制这个软件，转文字通过 MCP 知道在哪，
>  智能体找到就上传自己的了。**谁和你说接模型 API 了。**」

⇒ **软件不接任何大模型 API。知识提炼由智能体自带模型完成。**
⇒ 第 9 节的"修复"只是让任务状态不再无限重试（工程上正确），
  但**它默认了"需要软件自己配 API"这个错误前提**，产品定义以 `项目宪法.md` 为准。

## 1. 冻结的正确状态

- 程序与配置在项目目录；产物在可配置的 `[路径设置]` 根目录。`产物根目录`、`短视频下载目录`、`复盘输出目录`、`日志目录`、`数据库文件` 空白时，按 `zunzhe-core/src/paths.rs` 的默认规则回退。
- 生产运行状态以 `zunzhe-core/src/paths.rs`、`config/config.ini`、`launchctl print`、进程与日志现场为准。
- `zunzhe-core` 承载直播监控、视频监控、状态快照、任务队列、通知、状态快照与 extension 控制面；Python `fupan` 只承担 AI 工具层。
- 复盘产物、视频库、复盘笔记、训练数据导出和转写队列必须使用同一个 `[路径设置]` 产物根。`extension.rs` 不得绕过统一路径解析去写 `project_dir.join("fupan_output")`。
- 复盘流水线 `pipeline_summary.status` 为 `pending/failed` 时，必须保留真实 `attempts` 与 `last_error`；进入 `done` 必须形成真实队列任务和真实产物。前端按钮、IPC、CLI、MCP 工具与副作用必须能追溯到同一个动作目录。

## 2. 禁止事项 → 见 `AGENTS.md`

**禁止事项的唯一出处是 [`AGENTS.md`](./AGENTS.md)「禁止事项（操作层，本项目唯一出处）」。**

本文件**不再重复该表**（2026-09-22 收敛）。

> 原第 2 节那 5 条与 `项目宪法.md` 第 4 条**逐字重复**，是文档技术债的典型形态：
> 同一约束写在两处，改一处忘一处就会互相矛盾。现统一为单一出处。

## 3. 已知病根与事实边界

- `extension.rs` 曾直接使用 `project_dir.join("fupan_output")`，绕过 `[路径设置]`；已改为统一路径解析。
- 复盘桥接曾把失败任务误报为 `done`；现要求失败状态保留 `pending`、`attempts` 和 `last_error`。
- 平台源失败清零、B 站兜底、抖音授权闭环、SQLite WAL/慢查询、日志双份与路径漂移等问题，必须用现场日志、数据库记录和可复现测试判断，不能只凭旧报告宣称已修或未修。
- 工作区允许同时存在其他会话的未提交修改。任何编辑前先核对目标文件的 `stat` 与 `sha`，不得覆盖不属于当前任务的改动。
- 行为改动先写失败测试，再写最小实现。路径类改动必须使用非默认路径的隔离项目验证。

## 4. 验证门槛

- 路径类改动必须使用 `test_support::isolated_project`、`crate::paths::{logs,run_dir}` 或等价的隔离夹具，不能把测试写到真实项目目录。
- 涉及 daemon 和调度器的行为，至少通过 `cargo check`、`cargo test -q --all-targets`；集成目标也要单独记录通过数。
- 桌面端改动运行 `npm test`；脚本改动运行 `bash -n` 和对应的真实执行回归。
- 每个修复必须留下“先失败、后通过”的证据，或者明确说明为什么该边界无法自动化验证。

## 5. 回滚方式

- 代码回滚以 Git 提交为单位；生产状态回滚必须先停止变更、保留现场，再由用户决定。
- 任何生产状态变更若发生，必须先停止变更、保留现场，再由用户决定回滚。

## 6. 维护顺序

代码改动先通过测试，再更新本宪法；本宪法与旧报告冲突时，以可复现的现场证据和本文件为准。

## 7. 历史已验证证据（2026-09-19）

- Rust：`cargo test --all-targets` -> `262 passed, 0 failed, 1 ignored`；`tests/contract_smoke.rs` -> `7 passed`；`cargo check` -> 通过。
- 桌面端：`npm test` -> `8 passed / 0 failed`。
- extension：隔离项目真实 stdio 审计 -> `initialize` 成功，`tools/list=42`，`control_list=42`，12 个只读调用无错误。
- 复盘桥接：隔离真实媒体 + 缺依赖环境 -> 任务不再虚假 `done`，而是 `pending`、`attempts` 递增、`last_error=转写为空或未启用，流水线结束`。
- 尚未完成端到端：真实含语音媒体在完整 `.venv` 下的 `.转写.txt/.json` 落盘仍未复验；本轮真实媒体只验证了失败状态收敛，不宣称转写产物已修。

## 8. 本轮已验证证据（2026-09-21）

- 本轮只改测试夹具、脚本与宪法，不触碰生产 daemon、生产数据库或产物目录。
- 根因：相关测试用手拼临时目录（无 `config/config.ini`）并写 `dir/logs`，而生产路径按 `zunzhe-core/src/paths.rs` 的配置解析规则回退到默认产物根，导致读取共享/真实日志；改为 `test_support::isolated_project` 与 `crate::paths::{logs,run_dir}` 后隔离。
- TDD 证据：`daemon_uptime_parses_started_line`、`daemon_uptime_merges_rotated_logs`、`health_audit_tolerates_corrupt_uptime_log`、`health_audit_started_at_only_matches_current_pid` 先稳定 FAIL（实际 `0/0/2/None`），修夹具后逐个 PASS。
- 回归中还发现同一类隔离缺陷 `heartbeat_and_diagnostics_roundtrip`，已改为隔离项目并 PASS。
- 全库：`cargo test -q --all-targets` -> `285 passed, 0 failed, 1 ignored`；两个集成目标分别为 `12 passed` 与 `1 passed`。
- 编译：`cargo check` -> 通过。
- 看门狗路径回归：`scripts/test-check-rust-daemon.sh` 先对上一提交的 `scripts/check-rust-daemon.sh` 稳定 FAIL（未写入配置日志目录），再对当前实现 PASS；手工把 pid 路径改回 `$ROOT/fupan_output/run` 后也稳定 FAIL（读到 `111111` 而非配置目录的 `424242`）。
- 看门狗路径验证：配置 `复盘输出目录` 与 `产物根目录` 故意指向不同目录，日志必须写 `日志目录`，pid 必须取 `复盘输出目录/run/zunzhe-core.pid`。
- launchd：`bash scripts/test-launchd-daemon.sh` -> `PASS`。
- Windows 看门狗同一路径回归：`scripts/check-rust-daemon.ps1` 原来日志目录可配置、pid 仍写死 `$Root\fupan_output\run`；已补读 `[路径设置] 复盘输出目录` 并改用 `$FupanDir\run\zunzhe-core.pid`。当前主机无 `pwsh`，本轮只做了静态路径核对，未运行 PowerShell 端到端测试。
- 格式：`cargo fmt -- --check` 仍只报既有 `zunzhe-core/src/db.rs` 与 `zunzhe-core/src/lib.rs` 的换行/模块排序差异，本轮未做无关格式改动。
- 图谱：`graphify update .` -> `4618 nodes, 7733 edges, 266 communities`；本轮增量更新时代码 AST 已重算，文档/图片语义增量因当前宿主无 Agent 分发工具且无 Gemini 凭据未伪造，待具备条件后再补语义抽取。
- 未执行：生产 release 重编译、launchd plist 重载和 daemon 迁移，均等待明确授权。

## 9. 本轮转写链路修复证据（2026-09-21）

- 现场实测：58 分钟 B站视频在纯 CPU + `small` 模型下约 26 分钟完成转写，生成 `98,097` 字节的 `.转写.txt` 和 `179,736` 字节的 `.转写.json`（1551 个分段，时长 3517.24 秒），证明转写链路自身可产出真实文件。
- 根因：Python 流水线在转写完成后因“未配置大模型接口”提前结束，旧桥接层把这个状态当成整条任务失败并重新入队，导致 UI 显示失败、用户以为转写按钮是假的。
- 修复：`validate_pipeline_summary` 对“未配置大模型接口，已保留转写文本”返回完成说明；worker 用 `complete_task_with_note` 将任务标 `done` 并保留说明，不再无限重试。真正“转写为空或未启用”仍按失败处理。
- 长任务上限：新增 `config/fupan_config.ini [复盘] 任务超时秒数`，默认 `21600` 秒（6 小时），下限 60 秒、上限 24 小时；桌面端全局设置可编辑。worker 每轮重新读取该配置，Python 子进程使用同一上限，避免旧 3600 秒硬超时砍掉长视频。
- 已验证：`cargo test -q --all-targets` -> `289 passed, 0 failed, 1 ignored`，两个集成目标为 `12 passed` 与 `1 passed`；`cargo check` 通过；`bash scripts/test-launchd-daemon.sh` 与 `bash scripts/test-check-rust-daemon.sh` 均 PASS；`npm test --prefix desktop` 为 `8 passed / 0 failed`。契约冷启动测试的 8 秒硬阈值在 Whisper 满载时会抖动，已改为统一的 20 秒有界协议断言，仍保留响应、id 和工具结果硬校验。
- 生产状态收敛：任务 512 已由 `pending` 改为 `done`，保留 `last_error=未配置大模型接口，已保留转写文本，流水线结束`；变更前备份为 `/Volumes/A区/尊者产物/database/zunzhe-core.backup-before-task512-status-20260921-035713.db`。
- 未执行：生产 release 尚未用本轮最新源码重构建、launchd 未重载，任务 513 仍在真实运行中；任何 daemon 切换继续等待明确授权。

## 10. 控制面接线现场（2026-09-21）

- 真实 Rust extension stdio `initialize + tools/list` 返回 `43` 个工具；源码契约 `zunzhe-core/tests/control_plane_contract.rs` 也断言 `43`。
- 根因：神经刷新器只读取 `.神经/控制面.json`，项目实际旧清单在 `.神经总账/控制面.json`（`tool_count=41`），因此刷新后总账里只有 `214` 个 Python脚本和 `81` 个 CLI，`extension工具` 为 `0`。
- 已生成正确的薄清单 `/Volumes/A区/尊者监控直播短视频/.神经/控制面.json`，内容来自真实 Rust `tools/list`，共 `43` 项，未执行任何有副作用工具。
- 尚未完成：`neural_ledger_refresh` 全量在 900 秒内未返回、增量刷新不会重扫控制面；外置盘总账文件复制到 `/tmp` 可读，但原路径直接 SQLite 打开偶发 `unable to open database file (14)`。后续需在总账工具层修复外置盘打开与全量刷新耗时，不能重复盲跑。

## 11. 双数据库权威性与合并规则（2026-09-21）

- 当前现场（2026-09-21 06:18:06 起）：daemon PID `67494`，实际执行的 release 为 `/Volumes/A区/尊者监控直播短视频/zunzhe-core/target/release/zunzhe-core`，inode `6991398`，SHA-256 `f7197bc788b60fb0c8dc72a75edc97e104c49d078c4f085ec177a697c0d31ada`。它已改为打开外置产物库 `/Volumes/A区/尊者产物/database/zunzhe-core.db`，WAL/SHM 同目录；项目根旧库 `/Volumes/A区/尊者监控直播短视频/zunzhe-core.db` 仍保留迁移前事实，不再代表当前运行库。
- 外置库当前 `monitor_baseline`：B站 `946974` 与 `1458143131` 均已从 `consecutive_fails=1` / `last_failed_at=2026-09-21 06:19:03` 累加到 `consecutive_fails=2` / `last_failed_at=2026-09-21 06:35:04`，日志同步记录两条源“失败冷却计数: 2”。这已实盘证实 Bug 1 的跨轮累加闭环（不是永远停在 1），但尚未等到第 3 次进入冷却，因此“跳过冷却”本身仍未实盘验证；抖音源为旧库遗留 `consecutive_fails=13`，当前已缺 `douyin_browser_cookies.txt`，新 daemon 日志按“未完成授权，本轮跳过，不计失败冷却”处理，因此不会继续把旧 13 次累加。
- 项目根旧库任务最终状态只读核对：任务 `512` 为 `done`、`attempts=1`；任务 `513` 为 `pending`、`attempts=3`、`last_error=未配置大模型接口，已保留转写文本，流水线结束`、`next_retry_at=2026-09-21 04:53:28`。日志已证实任务 513 在 `2026-09-20 22:18:04` 因心跳超过 1800 秒触发优雅退出时“超时（attempts=3），已显式结束本轮”，不能再描述为仍在 `processing`。
- 只读对比（项目根旧库 vs 外置库）：两库均为 11 张表；`videos=293/293` 且 ID 集合完全一致；`monitor_baseline=3/3`；`live_states=3/3`；两库都不存在只属于外置库的任务。项目根旧库独有任务 `512`、`513`。
- 任务表差异仅有三类：
  - 项目根库独有：任务 `512`（`done`，`attempts=1`）和 `513`（`pending`，`attempts=3`，本轮已超时结束）。
  - 125 条共同任务的 `url` 不同：项目根库仍是旧项目根路径（`~/Projects/尊者监控直播短视频/downloads/...`），外置库已由 `rewrite_legacy_task_paths` 改写为当前产物路径（`/Volumes/A区/尊者产物/downloads/...`）。
  - 其余任务字段全部一致；`status/attempts/payload/last_error/next_retry_at/name/kind` 无任何冲突。
- 权威性规则：最终运行库应为外置产物库；合并时以外置库为目标，以项目根库为“最新运行事实来源”。禁止用项目根库整库覆盖外置库，否则会回退 125 条已修正路径；也禁止用外置库整库覆盖项目根库，否则会丢任务 `512/513`。
- 合并前必须先停 daemon 并备份两库；任务 513 运行期间不得执行合并或 daemon 切换。
- 合并动作应当只包含：将项目根库独有的任务 `512`、`513` 插入外置库；对共同任务保留外置库中的已改写 `url`，再按项目根库中确实更新的运行字段做显式白名单更新；不得批量按 `updated_at` 取整行，因为外置库那 125 条的 `updated_at` 是路径改写时间，不代表业务状态更新。

## 12. 路径只读契约与损坏媒体预检（2026-09-21）

- `zunzhe-core/src/paths.rs` 的路径查询（`storage`、`media_root`、`downloads`、`fupan`、`logs`、`covers`、`run_dir`、`database`、`known_roots`）是**纯只读能力**：只解析配置并返回路径，不创建目录、不迁移文件、不修改数据库。目录创建只允许显式调用 `paths::ensure_dirs` 的启动/命令入口执行；桌面端执行 `paths` 命令时不得因为查询路径而创建整套产物目录。
- 路径可信边界必须跟着 `[路径设置]` 与 `[录制设置] 直播保存路径` 解析结果走，同时兼容升级前 `<项目根>/downloads` 与 `<项目根>/fupan_output`。媒体入队使用 `paths::is_trusted_media_path`，不能在调用方另写死项目根路径。
- macOS 的 `/var` 与 `/private/var` 属于同一真实路径。路径比较必须使用 `canonical_lenient`，目标尚不存在时也应解析最近存在祖先再拼回剩余路径，避免 `/tmp` 与 `/private/tmp` 被误判成两个边界。
- 复盘入队 `download::enqueue_process` 在写库前执行两道真实性检查：文件必须存在且是普通文件；已知媒体后缀必须用 `ffmpeg -v error -i <path> -t 0 -f null -` 做零时长探测。探测启动失败、20 秒超时或非零退出都必须返回包含真实原因的错误，且不得留下 `pending` 任务。
- `PROCESS_MEDIA_SUFFIXES` 必须与 `fupan/pipeline.py` 的 `_MEDIA_SUFFIXES` 对齐：`mp4/ts/mkv/flv/mov/webm/mp3/m4a/wav`。未知后缀不做内容格式猜测，只保留原有可信路径与存在性校验。
- 损坏媒体错误的截断上限为 500 字符，必须让调用方读到“媒体文件损坏或无法读取”及 ffmpeg 的关键诊断；不得只返回内部任务失败或先谎报“已加入队列”。
- 自动下载成功后若复盘入队失败，`DownloadResult` 必须保留 `pipeline_error`，并由 `pipeline_warning()` 生成“下载成功，但自动复盘入队失败: ...”供调用方/日志展示。下载任务本身仍可标 `done`（文件确实已下载），但不得再静默吞掉复盘失败原因。
- Bug 1 的“计数永远是 1”旧结论已被当前现场数据推翻：外置库的 B站源计数已从 1 跨轮累加到 2，且有同步日志。只能表述为“已证实跨轮累加；进入冷却阈值后的跳过行为仍待实盘确认”，不能继续宣称当前代码永不累加。

## 13. process 入队后的后端可达性（2026-09-21）

- `process` 命令先写入真实任务，再确保 daemon 会消费该任务：daemon 已运行时只探测；未运行时必须调用 `service::ensure_core` 拉起。结果必须同时返回 `queued`、`task_id`、`disposition`、`daemon_running` 和 `startup_error`，不能只报“已入队”而让任务永久停在 `pending`。
- daemon 启动失败不回滚已落库的任务：任务仍可显示在队列中，但 `startup_error` 必须带回桌面端。前端 `processEnqueueFeedback` 在 `queued=true` 且 `startup_error` 非空时必须显示 warning，禁止伪装成普通 success。
- 外置库不得写死；`zunzhe-core paths` 是桌面端可信根的唯一只读入口，路径查询不得创建目录或修改数据库。
- 2026-09-21 本轮只读现场：外置库 B站两个源的 `consecutive_fails` 当前均为 `0`，抖音源为旧值 `13`；日志显示 B站每轮仍走官方/RSSHub/yt-dlp 三级并失败，但是否清零为何发生、以及进入第 3 次冷却后的跳过行为，仍需继续按日志和数据库证据核对。

## 14. 日志目录与 launchd 安装态一致性（2026-09-21）

- 日志只有一份权威配置源：`[路径设置] 日志目录`；留空时为 `<产物根目录>/logs`。仓库里的 launchd 模板、渲染脚本、Rust `paths` 和 Python 设置必须都指向同一目录。
- `doctor` 与桌面端 `status` 必须返回机器可读的 `log_paths`，至少包含 `configured_dir`、`primary`、`error`、`consistent`、`hint`。`primary/error` 要报告配置路径、旧 `~/Library/Logs/zunzhe` 路径、实际读取路径和来源（`configured` / `legacy_fallback` / `missing`）；不能只报 `logs_writable=true`。
- `tail-logs` 可以读取旧目录中的历史日志作为迁移兼容，但只有配置目录不存在时才回退；UI 必须显示实际读取的文件路径，并在 `consistent=false` 时警告“后台服务仍在写历史日志目录”，避免用户打开配置目录却看不到当前日志。
- 现场根因不是 `paths.rs` 解析错误，而是安装产物陈旧：仓库模板和渲染脚本已使用 `__LOG_DIR__`，磁盘 `~/Library/LaunchAgents/com.zunzhe.monitor.plist` 仍指项目 `logs/`，而已加载 launchd 更旧，仍指 `~/Library/Logs/zunzhe/`。修复必须同时覆盖渲染、磁盘 plist 和已加载任务，不能只改仓库模板。
- 只读审计入口：`bash scripts/audit-launchd-log-paths.sh`。它比较配置目录、磁盘 plist 与 `launchctl print` 的实际目录；三者不一致时以非零退出并打印全部路径。它不 reload、不重启、不修改任何文件。
- 重载 launchd 会中断当前 daemon 的长跑 pid，必须在得到明确运行授权后执行；在此之前只允许使用上述审计和 `doctor` 报告漂移，不得把“代码已修”当作“当前服务已切换日志目录”。

---

## 15. Rust 重构的接口契约（冻结）　← 自 `目标模式.md` 迁入（2026-09-22）

### 15.0 重构的最终目标（验收标准）

1. **长期稳定可用**：`zunzhe-core` 可 **24 小时 × 30 天**持续运行，不崩溃、不丢状态、
   崩溃后重启自恢复（SQLite 持久化 + 任务队列重放 + 日志）。
2. **原子模块化**：每个最小功能是一个独立模块（config / model / db / platform /
   video_library / download / notify / pipeline_bridge / scheduler / training_data / cli），
   模块间只通过公开 trait + 结构体契约连接，像电话线一样可插拔、可单测、可替换。
3. **改动不破坏**：任何模块改动都必须先跑全量回归（`cargo test` + 集成测试），
   已验收功能回归失败 = **不允许提交**。
4. **能力对齐**：Rust 逐个重写旧 Python 能力 —— 直播监控/录制调度、发布视频监控、
   历史视频浏览与选择性下载、转写/切片复盘桥接、邮箱通知、视频库、训练数据 JSONL、
   CLI/MCP 命令集。
5. **平台**：macOS 优先可用，Windows/Linux 可编译运行。
6. **简单可用**：个人用户不用懂内部实现，桌面端/CLI 一键操作；不出现
   "用着用着崩溃""监控断了没人管""状态丢失"等基础稳定性问题。

### 15.1 原子模块清单

| 模块 | 职责 | 关键公开接口 |
|---|---|---|
| M1 `config` | 读写 INI 配置、QQ 邮箱模板 | `Config::load()`, `Config::email_status()` |
| M2 `model` | 统一类型 | `VideoMeta`, `Anchor`, `Task`, `NotifyEvent`, `Transcript`, `KnowledgePoint` |
| M3 `db` | SQLite schema + CRUD | `init_db`, `upsert_video`, `mark_downloaded`, `list_videos`, `enqueue_task` |
| M4 `platform` | 平台适配器 | `Platform::list_videos(url, count)`, `BilibiliAdapter`, `RssAdapter` |
| M5 `video_library` | 视频库/封面缓存/已下载 | `upsert`, `list`, `mark_downloaded`, `cache_cover` |
| M6 `training_data` | 训练数据 JSONL | `export(anchor, output)` |
| M7 `download` | 下载队列 | `Downloader::download(url, name)`, `process_pending` |
| M8 `notify` | 通知总线 | `Notifier::send(event)`，邮箱/微信/不通知 |
| M9 `pipeline_bridge` | 桥接 Python 工具层 | `run_pipeline(media, anchor)` |
| M10 `scheduler` | 24×30 守护调度 | `run_daemon()`，状态机/心跳/崩溃恢复 |
| M11 `cli` | JSON CLI | 子命令与 `fupan.cli` 对齐，每行 JSON |
| M12 `extension` | MCP stdio server | 工具与 Python MCP 对齐 |
| M13 `assembly` | daemon 组装/打包 | `zunzhe-core` 二进制 + 打包脚本 |
| M14 `watchlist` | 监控列表读写、监控偏好 | `Watchlist::list_live/list_video/add/remove` |
| M15 `config_api` | 配置文件 JSON API（逐行保留注释） | `ConfigApi::get/set/set_bulk/schema` |
| M16 `service` | Python 服务兼容层 | `ensure_started/stop_all/status` |
| M17 `live_monitor` | 直播检测/录制状态机 | `LiveAdapter`, `BilibiliLiveAdapter`, `LiveMonitor` |

### 15.2 接口契约（冻结）

- **CLI 每条命令输出一行 JSON。** 命令名与 Python 版一致：
  `status / add-live / remove-live / list-live / check-live / add-video /
  remove-video / list-video / check-videos / download-video / blogger-videos /
  video-library / export-training-data / email-status / email-setup /
  test-email / start / stop / notes / process / notify-list / notify-retry /
  notify-prune`
- **SQLite 表：** `anchors / videos / notify / tasks`，字段与 Python `video_library.py`
  对齐；`videos.downloaded` 是唯一"已下载"事实源。
- **平台适配器**返回统一 `VideoMeta { id, platform, url, title, cover, published_at, duration }`，
  不泄漏平台私有字段。
- **复盘桥接**通过子进程调用 Python 工具层（faster-whisper / ffmpeg），
  Rust 只负责调度、状态、通知与产物索引。

> ⚠️ **已知漂移（2026-09-21 实测）**：上面冻结的是 **kebab-case**（`add-live`），
> 但 MCP 工具实际暴露的是 **snake_case 且词序相反**（`live_add`）。
> 三入口命名不统一详见 `项目宪法.md` 第 3.0b 条。**契约冻结的名称以 CLI 为准，
> MCP 侧待统一（未获批准前不动）。**

---

## 16. 防回归规则（每次改 Rust 必须）　← 自 `目标模式.md` 迁入

1. `cd zunzhe-core && cargo fmt --check && cargo clippy --all-targets && cargo test`
2. 新增模块必须带 `#[cfg(test)]` 单测；涉及跨模块的加集成测试。
3. 全量回归通过后才 `git add` / commit；commit 信息注明模块（如 `feat(zunzhe-core): M1 config`）。
4. 不允许把已验收模块"顺手改坏"：若修改共用接口，**先更新本契约再实现**，并跑全量测试。

---

## 17. 稳定性验收（24×30）　← 自 `目标模式.md` 迁入

- **daemon 循环**用 `tokio::select!`，任何单个任务 panic 不拖垮主循环；顶层安装 panic hook +
  崩溃日志；直播检查与视频检查各自独立任务，`JoinError` 只记日志，主循环继续下一轮。
- **SQLite** 每次写操作事务化；崩溃后重启读取 DB 恢复主播/视频/任务/通知状态。
- **服务守护**：发现进程不在则拉起（launchd KeepAlive / 桌面端 watchdog）；日志轮转（保留 5 份）；
  心跳文件 **30 分钟内**必须刷新，否则 `status` 判为不健康；磁盘满/ffmpeg 缺失等通过 `doctor` 可诊断。
- **冒烟测试**：`run-daemon --once` 与 `run-daemon --duration 30s` 不崩；
  `SIGTERM/SIGINT` 优雅退出并清理 pid 文件；长期稳定性由真实机器持续运行验收。

---

## 18. Rust 重构的完成状态（历史快照，2026-09-22 迁入）

> ⚠️ 这是**重构收尾时**的记录，留作考古。**当前状态以 `项目宪法.md` 第 2 条为准。**

M0–M17 已完成，通过 `cargo test --all-targets` 全量回归（122 个 lib 测试、
1 ignored + 5 集成契约测试）、`cargo fmt --check`、`cargo clippy --all-targets -D warnings`
与端到端冒烟（status / doctor / run-daemon / 崩溃恢复 / MCP 握手 / mcp-info）。

M13 交付物：`scripts/start-rust-daemon.sh`（release 构建 + 常驻运行）、
`scripts/launchd/com.zunzhe.monitor.plist`（macOS 开机自启）、使用说明 Rust 章节、
`PROJECT_GOAL.md` 阶段表。
