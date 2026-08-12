use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use sqlx::sqlite::SqlitePool;
use tokio::process::Command;

use crate::db;

/// 下载器：调用 yt-dlp 子进程下载视频到 downloads/短视频/<name>/。
pub struct Downloader {
    project_dir: PathBuf,
}

impl Downloader {
    pub fn new(project_dir: PathBuf) -> Self {
        Self { project_dir }
    }

    pub async fn download(
        &self,
        url: &str,
        name: &str,
        pool: &SqlitePool,
    ) -> Result<DownloadResult> {
        let dir = self.project_dir.join("downloads").join("短视频").join(name);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("创建下载目录失败: {}", dir.display()))?;

        let ytdlp = find_ytdlp().await;
        let mut cmd = Command::new(&ytdlp);
        cmd.args([
            "-f",
            "bv*+ba/b",
            "--merge-output-format",
            "mp4",
            "-o",
            format!("{}/%(title)s.%(ext)s", dir.display()).as_str(),
            "--no-overwrites",
            url,
        ]);
        // B站风控时带用户 cookie（config/fupan_config.ini [视频监控] b站cookie）
        if url.contains("bilibili.com") || url.contains("b23.tv") {
            let cookie = crate::config::Config::load(&self.project_dir)
                .ok()
                .map(|c| c.get_fupan("视频监控", "b站cookie"))
                .unwrap_or_default();
            if !cookie.is_empty() {
                cmd.args(["--add-header", &format!("Cookie: {cookie}")]);
            }
        }
        let output = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .with_context(|| format!("启动 yt-dlp 失败: {ytdlp}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!(
                "yt-dlp 下载失败: {}",
                stderr.chars().take(500).collect::<String>()
            ));
        }

        // 找实际下载文件，标记已下载（以目录里最新视频文件为准）
        let file = find_latest_video(&dir).unwrap_or_default();
        let video_id = extract_video_id(url).unwrap_or_else(|| url.to_string());
        // 下载成功但视频库还没有该记录时补一行，保证“已下载”有事实源。
        let _ = db::ensure_video_row(pool, &video_id, url, name, name).await;
        let _ = db::mark_downloaded(pool, &video_id, true).await;
        Ok(DownloadResult {
            ok: true,
            file,
            name: name.to_string(),
        })
    }
}

async fn find_ytdlp() -> String {
    if let Ok(found) = Command::new("yt-dlp").arg("--version").output().await
        && found.status.success()
    {
        return "yt-dlp".to_string();
    }
    let candidates = [
        Path::new(".venv").join("bin").join("yt-dlp"),
        Path::new(".venv").join("Scripts").join("yt-dlp.exe"),
    ];
    for c in candidates {
        if c.exists() {
            return c.display().to_string();
        }
    }
    "yt-dlp".to_string()
}

fn find_latest_video(dir: &Path) -> Option<String> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let mtime = entry.metadata().ok().and_then(|m| m.modified().ok());
            if let Some(m) = mtime
                && newest.as_ref().map(|(t, _)| m > *t).unwrap_or(true)
            {
                newest = Some((m, path));
            }
        }
    }
    newest.map(|(_, p)| p.display().to_string())
}

fn extract_video_id(url: &str) -> Option<String> {
    let re = regex::Regex::new(r"(BV[0-9A-Za-z]{10})|/(\d{10,})").ok()?;
    let caps = re.captures(url)?;
    caps.get(1)
        .or_else(|| caps.get(2))
        .map(|m| m.as_str().to_string())
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadResult {
    pub ok: bool,
    pub file: String,
    pub name: String,
}

/// 把 pending 下载任务逐个执行（失败重试，成功标记）。
pub async fn process_pending(pool: &SqlitePool, project_dir: &Path) -> Result<usize> {
    let tasks = db::list_tasks(pool, "pending", 50).await?;
    let downloader = Downloader::new(project_dir.to_path_buf());
    let mut done = 0;
    for task in tasks {
        if task.kind != "download" {
            continue;
        }
        match downloader.download(&task.url, &task.name, pool).await {
            Ok(_) => {
                db::update_task_status(pool, task.id, "done").await?;
                done += 1;
            }
            Err(_) => {
                // 重试 3 次后标记 failed，避免死循环；具体错误可看日志。
                if task.attempts >= 3 {
                    db::update_task_status(pool, task.id, "failed").await?;
                } else {
                    db::update_task_status(pool, task.id, "pending").await?;
                }
            }
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_bv_id() {
        assert_eq!(
            extract_video_id("https://www.bilibili.com/video/BV1abcDEFghi"),
            Some("BV1abcDEFghi".to_string())
        );
    }

    #[test]
    fn latest_video_found() {
        let dir = std::env::temp_dir().join(format!("zunzhe-dl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.mp4");
        let b = dir.join("b.mp4");
        std::fs::write(&a, b"a").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&b, b"bb").unwrap();
        assert_eq!(
            find_latest_video(&dir).as_deref(),
            Some(b.display().to_string()).as_deref()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn write_log_roundtrip() {
        use tokio::io::AsyncWriteExt;
        let dir = std::env::temp_dir().join(format!("zunzhe-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("x.log");
        let mut f = tokio::fs::File::create(&file).await.unwrap();
        f.write_all(b"hello").await.unwrap();
        f.flush().await.unwrap();
        drop(f);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
