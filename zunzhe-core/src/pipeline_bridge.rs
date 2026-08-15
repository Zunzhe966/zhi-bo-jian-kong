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

/// 列出 fupan_output/<主播>/<场次>/ 下的复盘产物（与 Python notes 对齐）。
pub fn list_notes(fupan_dir: &Path, name_filter: &str) -> Result<Vec<serde_json::Value>> {
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
            let mut entry = serde_json::Map::new();
            entry.insert("anchor".into(), anchor.clone().into());
            entry.insert(
                "session".into(),
                session_dir
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default()
                    .into(),
            );
            let mut clips = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&session_dir) {
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
                    }
                }
            }
            if !clips.is_empty() {
                entry.insert("clips".into(), serde_json::Value::Array(clips));
            }
            results.push(serde_json::Value::Object(entry));
        }
    }
    Ok(results)
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
        let output = match child.wait_with_output(Duration::from_secs(3600)).await {
            Ok(output) => output,
            Err(ChildError::Io(e)) => return Err(e).context("读取复盘流水线输出失败"),
            Err(ChildError::Timeout) => {
                anyhow::bail!("复盘流水线超时（60 分钟），已终止整个进程组")
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            anyhow::bail!(
                "复盘流水线退出码 {}: {}",
                output.status,
                stderr.chars().take(500).collect::<String>()
            );
        }
        Ok(PipelineSummary {
            ok: true,
            media: media_path.to_string(),
            record_name: record_name.to_string(),
            output_tail: stdout.chars().take(800).collect::<String>(),
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PipelineSummary {
    pub ok: bool,
    pub media: String,
    pub record_name: String,
    pub output_tail: String,
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
/// 不参与 70 秒 tick 护栏，单个任务最多 60 分钟；失败重试 3 次后标 failed。
pub async fn run_pipeline_worker(
    pool: SqlitePool,
    project_dir: PathBuf,
    mut stop: tokio::sync::watch::Receiver<bool>,
    task_timeout: Duration,
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
        let _ = crate::db::update_task_status(pool, task.id, "failed").await;
        return false;
    }
    let outcome = tokio::time::timeout(
        timeout_duration,
        run_pipeline_by_project(project_dir, &task.url, &task.name),
    )
    .await;
    match outcome {
        Ok(Ok(_)) => crate::db::update_task_status(pool, task.id, "done")
            .await
            .is_ok(),
        Ok(Err(e)) => {
            tracing::warn!(
                "复盘任务 id={} 失败（attempts={}）: {e:#}",
                task.id,
                task.attempts
            );
            let next = if task.attempts >= 3 {
                "failed"
            } else {
                "pending"
            };
            let _ = crate::db::update_task_status(pool, task.id, next).await;
            false
        }
        Err(_) => {
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
            let _ = crate::db::update_task_status(pool, task.id, next).await;
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
