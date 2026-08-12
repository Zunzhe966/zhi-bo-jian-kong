# 尊者直播监控 — 项目目标（上下文恢复锚点）

> 本文件是项目的"北极星"。任何一次会话开始、或上下文丢失时，先读本文件，
> 再看 `需求梳理与Rust重构方案.md`（需求与设计）和 `git status`（进度），
> 然后从"当前阶段"继续。

## 一、项目是什么

- GitHub 私有仓库：`Zunzhe966/zun-zhe-zhi-bo-jian-kong`
- 本地路径：`/Users/zhangxuetao/Desktop/尊者监控直播短视频`
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
| F | 交付：git 提交、文档图谱更新、汇报 | 🔄 本次收尾 |

### Rust 重构（zunzhe-core）已交付能力

- M0-M12 全部完成并通过 `cargo test`（28 个测试）+ `cargo clippy` + `cargo fmt`
- M13 组装收尾：macOS 守护脚本、launchd 模板、使用说明、图谱更新
- CLI/MCP 命令与 Python 版对齐；SQLite 持久化、崩溃可恢复、任务失败只记日志不拖垮 daemon
- CLI/MCP 已对齐 `delete-live / set-notify / ensure-services / tail-logs /
  get-app-prefs / set-app-prefs / wizard-done`，直播列表同时兼容
  `画质,URL,备注` 与 `URL,备注,画质` 两种旧格式
- 桌面端 Electron 已改为调用 Rust 核心二进制（优先 release、缺省 debug），
  `status` 同时返回 `rust_daemon` 与兼容的 `services` 字段，服务按钮/守护
  只看 Rust daemon，不再启动 Python 双跑
- 视频监控支持监控列表/偏好/服务管理，首次监控只记录基线，之后只对新视频通知/下载
- 平台适配器：B站原生接口（Cookie 防 412）+ RSS 兜底；抖音/快手走 RSSHub 或 yt-dlp
- 已知待补：邮箱授权码需用户提供；B站/抖音风控时需配置 Cookie
- 已知待补：B站视频监控当前环境匿名访问被 352/412 风控，需在
  `config/fupan_config.ini` 的 `[视频监控] b站cookie` 填入浏览器 Cookie
- Rust daemon 统一接管直播+视频监控；`start` 不再并行启动 Python 视频监控，
  `stop` 不自动拉起 Python 兜底（避免双跑与“停不掉”）
- 验证环境遗留的 Python `video_monitor` 进程已清理；桌面端若再次出现“双跑”，
  先查 `desktop/electron/main.cjs` 的 `ensure-services` 调用是否命中旧 Python CLI

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
