# 尊者直播监控 — 项目目标（上下文恢复锚点）

> 本文件是项目的"北极星"。任何一次会话开始、或上下文丢失时，先读本文件，
> 再看 `需求梳理与Rust重构方案.md`（需求与设计）和 `git status`（进度），
> 然后从"当前阶段"继续。

## 一、项目是什么

- GitHub 私有仓库：`Zunzhe966/zun-zhe-zhi-bo-jian-kong`
- 正式本地路径：`/Users/zhangxuetao/Projects/尊者监控直播短视频`
- 桌面旧副本：`/Users/zhangxuetao/Desktop/尊者监控直播短视频`（仅保留待确认清理）
- 技术栈（现状）：Python 3.14 + main.py（录制引擎）+ fupan/（复盘层）+ Electron 桌面端 + MCP/CLI

## 二、最终目标

**Rust 重构核心 daemon（zunzhe-core），Python 保留为 AI 工具层，现有 Python 版保持可用，分阶段替换。**

目标架构：
```
界面层    Electron 界面（保留，接口对接 daemon）
Rust 核心  zunzhe-core daemon（tokio + sqlx/SQLite + reqwest + clap + tracing）
  ├─ 调度器 + 主播生命周期状态机
  ├─ SQLite：anchors / videos / notify / tasks
  ├─ Platform trait（bilibili / douyin / kuaishou ... 每平台一适配器）
  ├─ 下载队列（单/批量/全部，失败重试，成功标记 downloaded）
  ├─ 复盘流水线桥接（调 Python 子进程）
  ├─ 通知总线（邮箱默认 + 每主播覆盖 + 本地兜底）
  ├─ 训练数据导出（JSONL）
  └─ MCP Server + JSON CLI（命令集与 Python 版一致）
Python 工具层  faster-whisper / LLM 提炼 / ffmpeg 切片 / yt-dlp 兜底
```

## 三、核心需求（用户澄清后）

1. 个人自用，非工业级，但底层按可靠设计
2. 平台：**macOS 优先（现在就能跑）**，兼容 Windows，未来 Linux/云服务器
3. 直播监控：多平台开播检测 + 自动录制
4. 复盘流水线：转写/提炼/切片**三个独立开关**（全局+每主播可覆盖）
5. 通知：开播→邮箱；发视频→邮箱；每主播可覆盖（微信/不通知）；本地兜底
6. **需求A**：博主已发布视频浏览+选择性下载（标题/封面/日期/时长/是否已下载，单选/批量/全部）
7. **需求B**：训练数据沉淀（转写+知识点结构化保存，导出 JSONL）

## 四、阶段与验收（当前进度）

| 阶段 | 内容 | 状态 |
|---|---|---|
| 前置 | 仓库克隆、跨平台修复、ffmpeg官方源、测试/CI修复、配置模板、图谱、需求文档 | ✅ 已完成 |
| A | macOS 全链路：ffmpeg 装好、doctor 通过、CLI/MCP 可用 | ✅ 已完成 |
| B | 历史视频浏览+选择性下载（封面/已下载标记/批量/全部） | ✅ 已完成 |
| C | 训练数据导出 JSONL | ✅ 已完成 |
| D | 通知简化（邮箱默认+覆盖+兜底） | ✅ 已完成 |
| E | Rust 骨架 zunzhe-core（cargo/schema/CLI 对齐/Platform trait/BilibiliAdapter） | ✅ 已完成 |
| G | QQ 邮箱通知链路：配置助手/状态诊断/老项目迁移/桌面端一键填入 | ✅ 已完成（授权码需用户提供） |
| F | 交付：git 提交、文档图谱更新、三平台 CI、汇报 | 🔄 部分完成（代码与文档已交付；三平台 CI 被 GitHub 账户计费拦截，24×30 长跑未满） |

### Rust 三平台 CI
- `.github/workflows/rust-core.yml` 会在 push/PR 时对 `zunzhe-core` 在
  macOS / Windows / Ubuntu 三平台跑 `cargo fmt --check`、`clippy -D warnings`、
  `cargo test --all-targets`、release 构建与 CLI 契约冒烟，作为“三平台可编译运行”
  的自动化证据。
- 当前真实状态：GitHub Actions 账户计费未恢复，三平台 CI 全部 `not started`；
  本机 macOS 全量回归已绿（210 passed + 1 ignored + 7 集成契约）；Windows/Linux
  已用 Zig 0.16 完成交叉链接（`check-cross-zig.sh`），Linux 另有 Docker 真实
  构建与 `run-daemon --once` 实跑验证。

### Rust 重构（zunzhe-core）已交付能力

- M0-M17 全部完成并通过 `cargo test --all-targets`（210 passed + 1 ignored + 7
  集成契约测试）+ `cargo clippy --all-targets -D warnings` + `cargo fmt --check`
- M13 组装收尾：macOS 守护脚本、launchd 模板、使用说明、图谱更新
- CLI/MCP 命令与 Python 版对齐；SQLite 持久化、崩溃可恢复、任务失败只记日志不拖垮 daemon
- CLI/MCP 已对齐 `delete-live / set-notify / ensure-services / tail-logs /
  get-app-prefs / set-app-prefs / wizard-done`，直播列表同时兼容
  `画质,URL,备注` 与 `URL,备注,画质` 两种旧格式
- 桌面端 Electron 已改为调用 Rust 核心二进制（优先 release、缺省 debug），
  `status` 同时返回 `rust_daemon` 与兼容的 `services` 字段，服务按钮/守护
  只看 Rust daemon，不再启动 Python 双跑
- 桌面端历史视频封面经受限制的 `local-file://` 协议读取项目内缓存；渲染
  进程不再直连 `file://` 本地路径，路径穿越/越权读取一律拒绝
- 视频监控支持监控列表/偏好/服务管理，首次监控只记录基线，之后只对新视频通知/下载
- 下载成功后自动入队复盘（独立后台 worker，转写/提炼/切片），录制结束同样自动复盘；
  复盘队列按 kind 独立认领/恢复，失败 3 次落定 failed，不再无限循环
- 平台适配器：B站官方 WBI 接口 + RSSHub + yt-dlp 三级兜底；B站下载优先 Electron
  浏览器内核提取直链，失败再回退 yt-dlp；抖音/快手走 RSSHub 或 yt-dlp
- 供应链：ffmpeg 已从蓝奏云改为官方源（Windows=BtbN GitHub + SHA-256、
  macOS=Homebrew/evermeet.cx 官方静态构建 + 大小校验、Linux=系统包管理器）；
  视频/直播内容下载只走平台官方地址，不经过第三方网盘中转
- 原子写临时文件名加入纳秒时间戳，崩溃残留不再阻塞后续写入；桌面端通知
  队列对原子替换窗口可容错，seen 文件原子写，邮箱状态只报真实缺失项；
  `setup-macos.sh` 自动安装 launchd 守护 + 60 秒健康看门狗
- B站投稿监控带失败冷却：连续失败 3 次起冷却 15 分钟、指数增长上限 2 小时，
  成功即清零；yt-dlp 同一 Cookie 组失败后等待 5 秒再重试，减少 412 风控
- B站 Cookie 链路：用户配置 Cookie、桌面端浏览器授权 Cookie、匿名 buvid 设备
  Cookie 自动按序使用，间歇 412 会重试并切换；桌面端「B站授权」导出的
  `fupan_output/run/bilibili_browser_cookies.txt` 已接入下载、投稿列表、Electron
  直链提取与直播检测
- 已知待补：邮箱授权码需用户提供；未配置任何 B站 Cookie 时，平台风控（352/412）
  仍可能要求先完成桌面端「B站授权」或在 `config/fupan_config.ini` 的
  `[视频监控] b站cookie` 填入浏览器 Cookie
- Rust daemon 统一接管直播+视频监控；`start` 不再并行启动 Python 视频监控，
  `stop` 不自动拉起 Python 兜底（避免双跑与“停不掉”）
- 验证环境遗留的 Python `video_monitor` 进程已清理；桌面端若再次出现“双跑”，
  先查 `desktop/electron/main.cjs` 的 `ensure-services` 调用是否命中旧 Python CLI
- 24×30 长跑：daemon 由 launchd KeepAlive 托管，健康审计每分钟落盘
  `logs/zunzhe-core.audit.log`；当前 `pid=25141` 持续心跳，但 30 天累计
  验收尚未完成。最终验收以“同一 pid 连续心跳 + RSS 稳定 + 日志无断档 +
  三平台 CI 全绿”为准。
- 数据库单一权威：CLI/daemon/MCP 统一使用 `项目根/zunzhe-core.db`；历史
  `zunzhe-core/zunzhe-core.db` 已合并归档，不得再作为第二套状态打开。

## 五、关键约定

- 所有改动先在 macOS 实测通过再提交
- CLI/MCP 命令集冻结，与 Python 版一致
- 改动即提交（git），保持工作区清晰
- 图谱重生成：`graphify . --update`（输出 graphify-out/）
- 训练数据格式：JSONL，一行一条知识点记录（含时间戳/主播/日期/元数据）

## 六、上下文丢失恢复步骤

1. 读本文件
2. 读 `需求梳理与Rust重构方案.md`
3. `git status` 看未提交改动
4. 看 `graphify-out/GRAPH_REPORT.md` 了解代码结构
5. 从"当前阶段"继续（见上表 🔄 标记）
