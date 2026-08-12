use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result};
use serde::Serialize;
use tokio::process::Command;

use crate::config::Config;

/// Python AI 工具层路径（转写/提炼/切片）。
#[derive(Debug, Clone)]
pub struct PythonTool {
    pub python: PathBuf,
    pub module: PathBuf,
}

impl PythonTool {
    pub fn from_project(project_dir: &Path) -> Self {
        let python = if cfg!(windows) {
            project_dir.join(".venv").join("Scripts").join("python.exe")
        } else {
            project_dir.join(".venv").join("bin").join("python")
        };
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
        if !self.python.exists() {
            anyhow::bail!("找不到 Python 虚拟环境: {}", self.python.display());
        }
        if !self.module.exists() {
            anyhow::bail!("找不到复盘流水线模块: {}", self.module.display());
        }
        let mut cmd = Command::new(&self.python);
        cmd.arg(&self.module)
            .arg("--save_file_path")
            .arg(media_path)
            .arg("--record_name")
            .arg(record_name)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = cmd.output().await.context("启动复盘流水线失败")?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_tool_paths_are_relative_to_project() {
        let tool = PythonTool::from_project(Path::new("/tmp/proj"));
        assert!(tool.python.to_string_lossy().contains(".venv"));
        assert!(tool.module.to_string_lossy().contains("fupan/pipeline.py"));
    }
}
