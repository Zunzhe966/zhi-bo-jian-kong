use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::sqlite::SqlitePool;
use tokio::process::Command;

use crate::config::Config;
use crate::process::{ChildError, ManagedChild};
use crate::service::python_binary_available;

/// 产物根下的系统目录名：这些不是主播目录，扫描产物时必须跳过。
///
/// 只放「系统目录」。三个分类名（直播/短视频/批量下载）刻意不放在这里——
/// `walk_media_files` 会在任意深度按这个表跳过目录，而「最近录制」正是靠
/// `{主播}/直播/` 这种二级目录找文件的，放进来会把直播录像整片漏掉。
pub(crate) const NON_ANCHOR_DIRS: &[&str] = &[
    "database",
    "logs",
    "downloads",
    "fupan_output",
    "covers",
    "run",
];

/// 媒体文件后缀：用于识别场次目录里的视频/音频本体。
pub(crate) const MEDIA_SUFFIXES: &[&str] = &[
    "mp4", "ts", "mkv", "flv", "mov", "webm", "mp3", "m4a", "wav",
];

/// 列出全部转写产物（含绝对路径）——智能体找文件就靠它。
///
/// 布局：主播名在最上层，分类在其下（一个主播的东西都在他自己的文件夹里）。
///   直播    录像 落在  {recordings_root}/{主播}/直播/{场次}/
///   短视频  下载 落在  {video_root}/{主播}/短视频/{标题}/
///   批量下载 落在  {video_root}/{主播}/批量下载/{标题}/
/// 旧布局（`fupan_output/{名字}/{场次}/`，目录合并迁移前的历史产物）一并列出，
/// kind 标为「旧布局」，保证迁移完成前智能体仍能找到旧文件。
pub fn list_notes(
    recordings_root: &Path,
    video_root: &Path,
    fupan_dir: &Path,
    name_filter: &str,
) -> Result<Vec<serde_json::Value>> {
    let mut results = Vec::new();
    // 主播名在最上层，分类（直播/短视频/批量下载）在主播目录下一级。
    // recordings_root 与 video_root 通常同为产物根，这里按 (根, 分类) 组合逐轮
    // 扫描各自的根目录；某个根/分类不存在时直接跳过，不会误报。
    for (base, category) in [
        (recordings_root, "直播"),
        (video_root, "短视频"),
        (video_root, "批量下载"),
    ] {
        if !base.is_dir() {
            continue;
        }
        let mut anchors: Vec<PathBuf> = std::fs::read_dir(base)
            .map_err(anyhow::Error::from)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        anchors.sort();
        for anchor_dir in anchors {
            let anchor = anchor_dir
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if anchor.starts_with('.') || NON_ANCHOR_DIRS.contains(&anchor.as_str()) {
                continue;
            }
            if !name_filter.is_empty() && anchor != name_filter {
                continue;
            }
            // 分类目录在主播目录下一级，不存在就说明这个主播还没有该分类的产物。
            let cat_dir = anchor_dir.join(category);
            if !cat_dir.is_dir() {
                continue;
            }
            let mut sessions: Vec<PathBuf> = std::fs::read_dir(&cat_dir)
                .map_err(anyhow::Error::from)?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            sessions.sort();
            for session_dir in sessions {
                if let Some(entry) = scan_session_dir(&anchor, &session_dir, category) {
                    results.push(entry);
                }
            }
        }
    }
    // 旧布局（迁移前的历史产物）
    results.extend(list_notes_legacy(fupan_dir, name_filter)?);
    Ok(results)
}

/// 旧布局扫描：fupan_output/{名字}/{场次}/（目录合并前的历史形态）。
fn list_notes_legacy(fupan_dir: &Path, name_filter: &str) -> Result<Vec<serde_json::Value>> {
    let mut results = Vec::new();
    if !fupan_dir.exists() {
        return Ok(results);
    }
    let mut anchors: Vec<PathBuf> = std::fs::read_dir(fupan_dir)
        .map_err(anyhow::Error::from)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    anchors.sort();
    for anchor_dir in anchors {
        let anchor = anchor_dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if anchor == "run" || anchor == "logs" || anchor == "covers" {
            continue;
        }
        if !name_filter.is_empty() && anchor != name_filter {
            continue;
        }
        let mut sessions: Vec<PathBuf> = std::fs::read_dir(&anchor_dir)
            .map_err(anyhow::Error::from)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        sessions.sort();
        for session_dir in sessions {
            if session_dir
                .file_name()
                .map(|s| s == "视频下载")
                .unwrap_or(false)
            {
                continue;
            }
            if let Some(entry) = scan_session_dir(&anchor, &session_dir, "旧布局") {
                results.push(entry);
            }
        }
    }
    Ok(results)
}

/// 扫描一个场次/视频目录，汇总里面的媒体本体与转写产物。
/// 只有媒体、没有任何复盘产物（笔记/转写/知识点/切片）的目录不算「复盘产物」，
/// 返回 None——否则下载完成但还没转写的几百个目录会把 notes_list 淹没。
fn scan_session_dir(anchor: &str, session_dir: &Path, kind: &str) -> Option<serde_json::Value> {
    let mut entry = serde_json::Map::new();
    entry.insert("anchor".into(), anchor.into());
    entry.insert(
        "session".into(),
        session_dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
            .into(),
    );
    entry.insert("kind".into(), kind.into());
    let mut clips = Vec::new();
    if let Ok(entries) = std::fs::read_dir(session_dir) {
        for f in entries.flatten() {
            let name = f.file_name().to_string_lossy().to_string();
            let path = f.path();
            if name.ends_with(".复盘笔记.md") {
                entry.insert("notes_file".into(), path.display().to_string().into());
            } else if name.ends_with(".转写.txt") {
                entry.insert("transcript_file".into(), path.display().to_string().into());
            } else if name.ends_with(".知识点.json") {
                entry.insert("points_file".into(), path.display().to_string().into());
            } else if path.is_dir()
                && name == "精华切片"
                && let Ok(inner) = std::fs::read_dir(&path)
            {
                clips = inner
                    .flatten()
                    .map(|c| c.path().display().to_string().into())
                    .collect();
            } else if path.is_file()
                && !entry.contains_key("media_file")
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| MEDIA_SUFFIXES.contains(&e.to_ascii_lowercase().as_str()))
                    .unwrap_or(false)
            {
                entry.insert("media_file".into(), path.display().to_string().into());
            }
        }
    }
    if !clips.is_empty() {
        entry.insert("clips".into(), serde_json::Value::Array(clips));
    }
    let has_product = entry.contains_key("notes_file")
        || entry.contains_key("transcript_file")
        || entry.contains_key("points_file")
        || entry.contains_key("clips");
    has_product.then_some(serde_json::Value::Object(entry))
}

/// Python AI 工具层路径（转写/提炼/切片）。
#[derive(Debug, Clone)]
pub struct PythonTool {
    pub python: PathBuf,
    pub module: PathBuf,
}

impl PythonTool {
    pub fn from_project(project_dir: &Path) -> Self {
        let python = crate::service::python_bin(project_dir);
        Self {
            python,
            module: project_dir.join("fupan").join("pipeline.py"),
        }
    }

    /// 调 fupan.pipeline --save_file_path <media> --record_name <name>。
    /// Rust 只做调度/状态，AI 计算仍在 Python 层。
    pub async fn run_pipeline(
        &self,
        media_path: &str,
        record_name: &str,
    ) -> Result<PipelineSummary> {
        if !python_binary_available(&self.python) {
            anyhow::bail!(
                "找不到复盘 Python 环境: {}。自动转写/知识提炼/精华切片需要虚拟环境，\
                 请运行 scripts/setup-macos.sh（Windows/Linux 安装对应依赖后创建 .venv）",
                self.python.display()
            );
        }
        let mut cmd = Command::new(&self.python);
        cmd.kill_on_drop(true);
        // 三个独立开关由 Rust 侧读取并显式传给 Python；关闭的环节直接跳过，
        // 避免只靠 Python 内部读取配置时两边不一致。
        let switches = crate::download::PipelineSwitches::load(
            self.module
                .parent()
                .and_then(|p| p.parent())
                .unwrap_or_else(|| Path::new(".")),
        );
        // 用 -m 以包方式运行，fupan 内部相对导入才能正常工作。
        cmd.arg("-m")
            .arg("fupan.pipeline")
            .arg("--save_file_path")
            .arg(media_path)
            .arg("--record_name")
            .arg(record_name)
            .arg("--transcribe")
            .arg(if switches.transcribe { "是" } else { "否" })
            .arg("--distill")
            .arg(if switches.distill { "是" } else { "否" })
            .arg("--clips")
            .arg(if switches.clips { "是" } else { "否" })
            .current_dir(
                self.module
                    .parent()
                    .and_then(|p| p.parent())
                    .unwrap_or_else(|| Path::new(".")),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = ManagedChild::spawn(&mut cmd)
            .with_context(|| format!("启动复盘流水线失败: {}", self.python.display()))?;
        // Python 子进程内部也要有上限，避免 worker 超时后残骸继续跑；两者
        // 共用同一个配置值，默认 6 小时，长视频不再被旧 60 分钟硬砍。
        let project_dir = self
            .module
            .parent()
            .and_then(|p| p.parent())
            .unwrap_or_else(|| Path::new("."));
        let timeout_secs = Config::load(project_dir)
            .map(|cfg| cfg.pipeline_timeout_secs())
            .unwrap_or(6 * 3600);
        let output = match child
            .wait_with_output(Duration::from_secs(timeout_secs))
            .await
        {
            Ok(output) => output,
            Err(ChildError::Io(e)) => return Err(e).context("读取复盘流水线输出失败"),
            Err(ChildError::Timeout) => {
                anyhow::bail!("复盘流水线超时（{timeout_secs} 秒），已终止整个进程组")
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            // 500 字符会在 Python traceback 的关键异常行之前截断，导致
            // “看似网络错误实为媒体文件问题”这类误诊。放宽到 4000。
            anyhow::bail!(
                "复盘流水线退出码 {}: {}",
                output.status,
                stderr.chars().take(4000).collect::<String>()
            );
        }
        let incomplete_note = validate_pipeline_summary(&stdout)?;
        Ok(PipelineSummary {
            ok: true,
            media: media_path.to_string(),
            record_name: record_name.to_string(),
            output_tail: stdout.chars().take(800).collect::<String>(),
            incomplete_note,
        })
    }
}

/// 校验 Python 流水线的结构化摘要。退出码 0 只说明脚本没有崩溃，不代表复盘
/// 真正完成；例如“转写为空/未启用”也会以 0 返回。没有摘要时保持兼容旧脚本，
/// 有摘要且状态不是“完成”时必须作为失败处理。
fn validate_pipeline_summary(stdout: &str) -> Result<Option<String>> {
    let Some(line) = stdout
        .lines()
        .rev()
        .find(|line| line.trim().starts_with("{\"pipeline_summary\""))
    else {
        return Ok(None);
    };
    let value: serde_json::Value =
        serde_json::from_str(line.trim()).context("解析 Python 复盘流水线摘要失败")?;
    let status = value
        .get("pipeline_summary")
        .and_then(|summary| summary.get("status"))
        .and_then(|status| status.as_str())
        .unwrap_or("复盘流水线未返回完成状态");
    if status != "完成" {
        // 转写已经落盘、只是后续提炼没配置，不是转写失败；由 worker 标 done
        // 并把说明保留给前端。真正的转写为空/无音频仍由 Python 返回同一类
        // 未完成状态，界面会显示“转写为空或未启用”，不会伪造成功文件。
        if status.contains("未配置大模型接口") && status.contains("已保留转写文本") {
            return Ok(Some(status.to_string()));
        }
        anyhow::bail!("{status}");
    }
    Ok(None)
}

#[derive(Debug, Clone, Serialize)]
pub struct PipelineSummary {
    pub ok: bool,
    pub media: String,
    pub record_name: String,
    pub output_tail: String,
    pub incomplete_note: Option<String>,
}

/// 便捷入口：从 Config 构造并运行。
pub async fn run_pipeline(
    config: &Config,
    media_path: &str,
    record_name: &str,
) -> Result<PipelineSummary> {
    PythonTool::from_project(&config.project_dir)
        .run_pipeline(media_path, record_name)
        .await
}

/// 仅凭项目根构造并运行，供录制后台任务使用。
pub async fn run_pipeline_by_project(
    project_dir: &Path,
    media_path: &str,
    record_name: &str,
) -> Result<PipelineSummary> {
    PythonTool::from_project(project_dir)
        .run_pipeline(media_path, record_name)
        .await
}

/// 独立复盘队列 worker：处理 kind="process" 的任务（下载完成后自动转写/提炼/切片）。
/// 不参与 70 秒 tick 护栏；每个任务超时从 `[复盘] 任务超时秒数` 即时读取，
/// 失败重试 3 次后标 failed。
pub async fn run_pipeline_worker(
    pool: SqlitePool,
    project_dir: PathBuf,
    mut stop: tokio::sync::watch::Receiver<bool>,
    _task_timeout: Duration,
    poll_interval: Duration,
) -> anyhow::Result<()> {
    let recovered = crate::db::recover_processing_by_kind(&pool, "process")
        .await
        .context("复盘 worker 恢复处理中任务")?;
    if recovered > 0 {
        tracing::info!("复盘 worker 启动，恢复 {recovered} 条 processing 任务");
    }
    let mut idle = 0u64;
    loop {
        tokio::select! {
            _ = stop.changed() => {
                if *stop.borrow() {
                    tracing::info!("复盘 worker 收到停止信号，退出");
                    return Ok(());
                }
            }
            _ = tokio::time::sleep(poll_interval) => {
                idle += 1;
            }
        }
        // 配置热更新后，下一轮任务立即使用新上限；旧进程启动参数只作为
        // 兼容占位，避免调用方需要同时维护两份超时状态。
        let task_timeout = Config::load(&project_dir)
            .map(|cfg| Duration::from_secs(cfg.pipeline_timeout_secs()))
            .unwrap_or(_task_timeout);
        match drain_process_tasks(&pool, &project_dir, task_timeout, &stop).await {
            Ok(done) => {
                if done > 0 {
                    tracing::info!("复盘 worker 本轮处理 {done} 个任务");
                    idle = 0;
                } else if idle >= 12 {
                    tracing::info!("复盘 worker 空闲中，等待复盘队列");
                    idle = 0;
                }
                // 与下载 worker 一致：异常留下的超限 processing 任务要落定
                // failed，不能永久卡住复盘队列。
                if let Err(e) = crate::db::fail_stale_processing_by_kind(&pool, "process", 3).await
                {
                    tracing::warn!("复盘 worker 自愈超限任务失败: {e:#}");
                }
            }
            Err(e) => {
                tracing::error!("复盘 worker 处理队列失败，稍后重试: {e:#}");
                // 与下载 worker 一致：停止信号应立即打断重试等待，不等满 10 秒。
                tokio::select! {
                    _ = stop.changed() => {
                        if *stop.borrow() {
                            tracing::info!("复盘 worker 在重试等待中收到停止信号，退出");
                            return Ok(());
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_secs(10)) => {}
                }
            }
        }
    }
}

/// 认领并执行复盘任务，直到队列暂时为空或收到停止信号。
async fn drain_process_tasks(
    pool: &SqlitePool,
    project_dir: &Path,
    task_timeout: Duration,
    stop: &tokio::sync::watch::Receiver<bool>,
) -> Result<usize> {
    let mut done = 0usize;
    while let Some(task) = crate::db::claim_pending_task_by_kind(pool, "process", 1).await? {
        if run_single_process_task(pool, project_dir, &task, task_timeout).await {
            done += 1;
        }
        if *stop.borrow() {
            tracing::info!("复盘 worker 在任务间收到停止信号，退出");
            return Ok(done);
        }
    }
    Ok(done)
}

/// 执行单个复盘任务并更新状态。超时/失败都显式落定，绝不永久卡 processing。
async fn run_single_process_task(
    pool: &SqlitePool,
    project_dir: &Path,
    task: &crate::model::Task,
    timeout_duration: Duration,
) -> bool {
    if task.kind != "process" {
        let _ = crate::db::record_task_failure(
            pool,
            task.id,
            &format!("任务类型错误：期望 process，实际 {}", task.kind),
            "failed",
        )
        .await;
        return false;
    }
    let outcome = tokio::time::timeout(
        timeout_duration,
        run_pipeline_by_project(project_dir, &task.url, &task.name),
    )
    .await;
    match outcome {
        Ok(Ok(summary)) => {
            let result = if let Some(note) = &summary.incomplete_note {
                crate::db::complete_task_with_note(pool, task.id, note).await
            } else {
                crate::db::update_task_status(pool, task.id, "done").await
            };
            result.is_ok()
        }
        Ok(Err(e)) => {
            let reason = format!("{e:#}");
            tracing::warn!(
                "复盘任务 id={} 失败（attempts={}）: {reason}",
                task.id,
                task.attempts
            );
            let next = if task.attempts >= 3 {
                "failed"
            } else {
                "pending"
            };
            let _ = crate::db::record_task_failure(pool, task.id, &reason, next).await;
            false
        }
        Err(_) => {
            let reason = format!("复盘任务超时（attempts={}）", task.attempts);
            tracing::warn!(
                "复盘任务 id={} 超时（attempts={}），已显式结束本轮",
                task.id,
                task.attempts
            );
            let next = if task.attempts >= 3 {
                "failed"
            } else {
                "pending"
            };
            let _ = crate::db::record_task_failure(pool, task.id, &reason, next).await;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_tool_paths_are_relative_to_project() {
        let tool = PythonTool::from_project(Path::new("/tmp/proj"));
        if cfg!(windows) {
            assert_eq!(tool.python, PathBuf::from("python.exe"));
        } else {
            assert_eq!(tool.python, PathBuf::from("python3"));
        }
        assert!(tool.module.ends_with("pipeline.py"));
    }

    #[test]
    fn list_notes_scans_anchor_first_layout() {
        // 布局：主播名在最上层，分类（直播/短视频/批量下载）在主播目录下一级。
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-listnotes-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let media = dir.join("产物");
        // 直播：{media}/侯翠翠/直播/2026-09-22 场次一
        let live_session = media.join("侯翠翠").join("直播").join("2026-09-22 场次一");
        std::fs::create_dir_all(&live_session).unwrap();
        std::fs::write(live_session.join("场次一.复盘笔记.md"), "# 笔记").unwrap();
        std::fs::write(live_session.join("场次一.ts"), b"x").unwrap();
        // 短视频：{media}/影视飓风/短视频/某标题
        let vid_session = media.join("影视飓风").join("短视频").join("某标题");
        std::fs::create_dir_all(&vid_session).unwrap();
        std::fs::write(vid_session.join("某标题.转写.txt"), "文字").unwrap();
        std::fs::write(vid_session.join("某标题.mp4"), b"y").unwrap();
        // 批量下载：{media}/老白/批量下载/合集A
        let batch_session = media.join("老白").join("批量下载").join("合集A");
        std::fs::create_dir_all(&batch_session).unwrap();
        std::fs::write(batch_session.join("合集A.知识点.json"), "{}").unwrap();
        std::fs::write(batch_session.join("合集A.mp4"), b"z").unwrap();

        let notes = list_notes(&media, &media, &dir.join("fupan_output"), "").unwrap();
        assert_eq!(notes.len(), 3, "三个分类下的复盘产物都应被列出: {notes:?}");
        let mut by_anchor: std::collections::HashMap<String, serde_json::Value> = notes
            .into_iter()
            .map(|n| (n["anchor"].as_str().unwrap().to_string(), n))
            .collect();
        assert!(by_anchor.contains_key("侯翠翠"));
        assert!(by_anchor.contains_key("影视飓风"));
        assert!(by_anchor.contains_key("老白"));
        // 路径顺序必须是 主播/分类/场次（主播名在最上层）。
        let live = by_anchor.remove("侯翠翠").unwrap();
        assert_eq!(live["session"].as_str().unwrap(), "2026-09-22 场次一");
        assert!(
            live["notes_file"]
                .as_str()
                .unwrap()
                .contains("侯翠翠/直播/2026-09-22 场次一/场次一.复盘笔记.md"),
            "直播笔记路径应按 主播/直播/场次 组织: {live:?}"
        );
        let vid = by_anchor.remove("影视飓风").unwrap();
        assert!(
            vid["transcript_file"]
                .as_str()
                .unwrap()
                .contains("影视飓风/短视频/某标题/某标题.转写.txt"),
            "短视频转写路径应按 主播/短视频/标题 组织: {vid:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn python_tool_falls_back_to_system_python_when_venv_missing() {
        // 容器/云服务器不强制项目内 .venv；系统 python3 也能跑复盘桥接。
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-python-fallback-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let tool = PythonTool::from_project(&dir);
        if cfg!(windows) {
            assert_eq!(tool.python, PathBuf::from("python.exe"));
        } else {
            assert_eq!(tool.python, PathBuf::from("python3"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn python_tool_prefers_project_venv_when_present() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-python-venv-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let venv = if cfg!(windows) {
            dir.join(".venv").join("Scripts").join("python.exe")
        } else {
            dir.join(".venv").join("bin").join("python")
        };
        std::fs::create_dir_all(venv.parent().unwrap()).unwrap();
        std::fs::write(&venv, "").unwrap();
        let tool = PythonTool::from_project(&dir);
        assert!(tool.python.to_string_lossy().contains(".venv"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rust_switch_parsing_matches_python_defaults() {
        // Rust 三个开关默认值与 Python fupan/pipeline.py 的默认值必须一致，
        // 防止 Rust 显式传参后与 Python 内部读取的默认行为产生偏差。
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-switch-contract-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(dir.join("config").join("config.ini"), "[推送配置]\n").unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "").unwrap();
        let switches = crate::download::PipelineSwitches::load(&dir);
        assert!(
            switches.transcribe && switches.distill && switches.clips,
            "默认配置下三个复盘开关都应启用"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pipeline_summary_with_empty_transcription_is_rejected() {
        // Python 流水线可能以退出码 0 返回“转写为空/未启用”。桥接层不能把
        // 这种没有产物的状态当作成功，否则任务显示 done 却没有文件。
        let output = concat!(
            "[1/4] 转写: sample.mp4\n",
            "转写为空或未启用，流水线结束\n",
            "{\"pipeline_summary\":{\"status\":\"转写为空或未启用，流水线结束\"}}\n",
        );
        let err = validate_pipeline_summary(output).unwrap_err();
        assert!(
            err.to_string().contains("转写为空或未启用"),
            "应保留 Python 给出的未完成原因: {err:#}"
        );
    }

    #[test]
    fn pipeline_summary_completed_is_accepted() {
        let output = concat!(
            "[4/4] 邮件通知\n",
            "复盘完成，输出目录: /tmp/out\n",
            "{\"pipeline_summary\":{\"status\":\"完成\"}}\n",
        );
        assert_eq!(validate_pipeline_summary(output).unwrap(), None);
    }

    #[test]
    fn pipeline_summary_without_llm_is_a_completed_transcription_note() {
        let output = concat!(
            "[2/4] 知识提炼\n",
            "未配置大模型接口，已保留转写文本，流水线结束\n",
            "{\"pipeline_summary\":{\"status\":\"未配置大模型接口，已保留转写文本，流水线结束\"}}\n",
        );
        let note = validate_pipeline_summary(output).unwrap();
        assert_eq!(
            note.as_deref(),
            Some("未配置大模型接口，已保留转写文本，流水线结束")
        );
    }

    #[tokio::test]
    async fn pipeline_worker_marks_stuck_task_failed() {
        let file = std::env::temp_dir().join(format!(
            "zunzhe-pipeline-worker-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = crate::db::open_pool(&file).await.unwrap();
        crate::db::init_db(&pool).await.unwrap();
        let id = crate::db::enqueue_task(&pool, "process", "/tmp/不存在.mp4", "测试")
            .await
            .unwrap();
        // attempts=2 有 5 分钟退避；测试拨旧时间让 worker 立即认领。
        sqlx::query("UPDATE tasks SET attempts=2 WHERE id=?")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE tasks SET updated_at=datetime('now','localtime','-1 hour') WHERE id=?")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let (tx, rx) = tokio::sync::watch::channel(false);
        let worker = tokio::spawn(super::run_pipeline_worker(
            pool.clone(),
            std::env::temp_dir().join("zunzhe-nonexistent"),
            rx,
            Duration::from_millis(100),
            Duration::from_millis(5),
        ));
        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = tx.send(true);
        worker.await.unwrap().unwrap();
        let rows: Vec<crate::model::Task> =
            crate::db::list_tasks(&pool, "failed", 10).await.unwrap();
        assert!(
            rows.iter().any(|t| t.id == id),
            "超时复盘任务应标 failed，当前: {rows:?}"
        );
        assert!(
            crate::db::claim_pending_task_by_kind(&pool, "process", 1)
                .await
                .unwrap()
                .is_none(),
            "failed 复盘任务不应被自动重新认领"
        );
        std::mem::forget(file);
    }

    #[tokio::test]
    async fn pipeline_repeated_failures_stop_after_three_attempts() {
        let file = std::env::temp_dir().join(format!(
            "zunzhe-pipeline-fail-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = crate::db::open_pool(&file).await.unwrap();
        crate::db::init_db(&pool).await.unwrap();
        let id = crate::db::enqueue_task(&pool, "process", "/tmp/不存在.mp4", "测试")
            .await
            .unwrap();
        for _ in 0..3 {
            // 退避由 db 测试单独覆盖；这里拨旧时间快速验证失败计数终态。
            sqlx::query(
                "UPDATE tasks SET updated_at=datetime('now','localtime','-1 hour') WHERE id=?",
            )
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
            let task = crate::db::claim_pending_task_by_kind(&pool, "process", 1)
                .await
                .unwrap()
                .unwrap();
            assert!(
                !run_single_process_task(
                    &pool,
                    &std::env::temp_dir().join("zunzhe-nonexistent"),
                    &task,
                    Duration::from_millis(100),
                )
                .await,
                "失败复盘任务不应成功"
            );
        }
        let rows: Vec<crate::model::Task> =
            crate::db::list_tasks(&pool, "failed", 10).await.unwrap();
        assert!(
            rows.iter().any(|t| t.id == id),
            "第三次失败后应标 failed，当前: {rows:?}"
        );
        assert!(
            crate::db::claim_pending_task_by_kind(&pool, "process", 1)
                .await
                .unwrap()
                .is_none(),
            "failed 复盘任务不能再被自动认领"
        );
        std::mem::forget(file);
    }
}
