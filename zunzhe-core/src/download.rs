use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use sqlx::sqlite::SqlitePool;
use tokio::process::Command;

use crate::bilibili_auth::{browser_cookie_path, ensure_anon_cookie, ensure_user_cookie_file};
use crate::db;

/// 下载器：优先走平台原生路径（B站浏览器内核提取直链 + ffmpeg 合流），
/// 失败回退 yt-dlp。下载产物统一落到 downloads/短视频/<name>/。
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

        // B站匿名请求容易被 412 风控，浏览器内核提取直链最可靠。
        if is_bilibili(url) {
            let output = bilibili_browser_download(&self.project_dir, url, &dir).await;
            if let Some(file) = output {
                return self.finish_download(url, name, pool, file).await;
            }
            tracing::warn!("B站浏览器下载兜底失败，回退 yt-dlp: {url}");
        }

        let file = self.ytdlp_download(url, &dir).await?;
        self.finish_download(url, name, pool, file).await
    }

    async fn finish_download(
        &self,
        url: &str,
        name: &str,
        pool: &SqlitePool,
        file: String,
    ) -> Result<DownloadResult> {
        let video_id = extract_video_id(url).unwrap_or_else(|| url.to_string());
        // 下载成功但视频库还没有该记录时补一行，保证“已下载”有事实源。
        let _ = db::ensure_video_row(pool, &video_id, url, name, name).await;
        let _ = db::mark_downloaded(pool, &video_id, true).await;
        // 下载完成后入队复盘任务（转写/提炼/切片），不阻塞下载调用方。
        let _ = enqueue_pipeline_after_download(pool, &file, name).await;
        Ok(DownloadResult {
            ok: true,
            file,
            name: name.to_string(),
            pipeline_queued: true,
        })
    }

    async fn ytdlp_download(&self, url: &str, dir: &Path) -> Result<String> {
        let ytdlp = find_ytdlp(&self.project_dir).await;
        let mut cmd = Command::new(&ytdlp);
        cmd.kill_on_drop(true);
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
            // 桌面端「B站授权」导出的浏览器会话 Cookie 最可靠；没有再回退
            // config 里粘贴的 cookie，并写成旧 Python 同路径的 Netscape 文件，
            // 保证 yt-dlp 走 `--cookies` 而不是容易被编码/换行截断的 `--add-header`。
            let browser_cookies = browser_cookie_path(&self.project_dir);
            if browser_cookies.exists() {
                cmd.args(["--cookies", browser_cookies.display().to_string().as_str()]);
            } else {
                let cookie = crate::config::Config::load(&self.project_dir)
                    .ok()
                    .map(|c| c.get_fupan("视频监控", "b站cookie"))
                    .unwrap_or_default();
                if let Some(user_cookies) = ensure_user_cookie_file(&self.project_dir, &cookie) {
                    cmd.args(["--cookies", user_cookies.display().to_string().as_str()]);
                } else if let Some(anon) = ensure_anon_cookie(&self.project_dir).await {
                    cmd.args(["--cookies", anon.display().to_string().as_str()]);
                }
            }
        }
        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("启动 yt-dlp 失败: {ytdlp}"))?;

        let output =
            match tokio::time::timeout(Duration::from_secs(600), child.wait_with_output()).await {
                Ok(output) => output?,
                Err(_) => {
                    // kill_on_drop 已开启：timeout 返回后 child 被丢弃即自动终止。
                    anyhow::bail!("yt-dlp 下载超时（10 分钟），已终止任务");
                }
            };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!(
                "yt-dlp 下载失败: {}",
                stderr.chars().take(500).collect::<String>()
            ));
        }

        // 找实际下载文件，标记已下载（以目录里最新视频文件为准）
        let file = find_latest_video(dir).unwrap_or_default();
        if file.is_empty() {
            return Err(anyhow!("yt-dlp 报告成功但没有产出视频文件"));
        }
        Ok(file)
    }
}

/// 用 Electron 内置浏览器内核提取 B站 DASH 直链，再用 ffmpeg 下载并合流。
/// 返回最终 mp4 路径；任何一步失败返回 None（由上层回退 yt-dlp）。
async fn bilibili_browser_download(project_dir: &Path, url: &str, dir: &Path) -> Option<String> {
    let electron = find_electron(project_dir)?;
    tracing::warn!("B站浏览器下载兜底: electron={}", electron.display());
    let extract = project_dir
        .join("desktop")
        .join("electron")
        .join("extract-run.cjs");
    if !extract.exists() {
        tracing::warn!("B站浏览器下载兜底不可用: 缺少 {}", extract.display());
        return None;
    }
    let out_file =
        std::env::temp_dir().join(format!("zunzhe_bili_streams_{}.json", std::process::id()));
    let _ = std::fs::remove_file(&out_file);
    let mut cmd = Command::new(&electron);
    cmd.kill_on_drop(true)
        .arg(&extract)
        .env("FUPAN_VIDEO_URL", url)
        .env("FUPAN_OUT_FILE", &out_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = match tokio::time::timeout(Duration::from_secs(60), cmd.status()).await {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => {
            tracing::warn!("B站浏览器提取进程启动失败: {e}");
            return None;
        }
        _ => {
            tracing::warn!("B站浏览器提取超时（60 秒）");
            let _ = std::fs::remove_file(&out_file);
            return None;
        }
    };
    if !status.success() {
        tracing::warn!("B站浏览器提取进程退出码: {}", status);
        let _ = std::fs::remove_file(&out_file);
        return None;
    }
    let raw = std::fs::read_to_string(&out_file).unwrap_or_default();
    let value: Option<serde_json::Value> = serde_json::from_str(&raw).ok();
    if value.is_none() {
        tracing::warn!("B站浏览器提取输出不是合法 JSON: {raw:?}");
    }
    let _ = std::fs::remove_file(&out_file);
    let value = value?;
    if value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        tracing::warn!("B站浏览器提取返回失败: {value}");
        return None;
    }
    let video = value.get("video").and_then(|v| v.as_str()).unwrap_or("");
    let audio = value.get("audio").and_then(|v| v.as_str()).unwrap_or("");
    if video.is_empty() {
        return None;
    }
    let title = value
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("bilibili");
    let title = clean_name(title);
    let output_file = dir.join(format!("{title}.mp4"));
    let ffmpeg = find_ffmpeg(project_dir);
    let mut args = vec![
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-user_agent".to_string(),
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36".to_string(),
        "-headers".to_string(),
        "Referer: https://www.bilibili.com/\r\n".to_string(),
        "-i".to_string(),
        video.to_string(),
    ];
    if !audio.is_empty() {
        args.extend([
            "-user_agent".to_string(),
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36".to_string(),
            "-headers".to_string(),
            "Referer: https://www.bilibili.com/\r\n".to_string(),
            "-i".to_string(),
            audio.to_string(),
            "-map".to_string(),
            "0:v".to_string(),
            "-map".to_string(),
            "1:a".to_string(),
        ]);
    }
    args.extend(["-c".to_string(), "copy".to_string()]);
    args.push(output_file.display().to_string());

    let mut cmd = Command::new(&ffmpeg);
    cmd.kill_on_drop(true)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = cmd.spawn().ok()?;
    let output = tokio::time::timeout(Duration::from_secs(600), child.wait_with_output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success()
        || !std::fs::metadata(&output_file)
            .map(|m| m.is_file())
            .unwrap_or(false)
    {
        None
    } else {
        Some(output_file.display().to_string())
    }
}

fn find_electron(project_dir: &Path) -> Option<PathBuf> {
    let base = project_dir
        .join("desktop")
        .join("node_modules")
        .join("electron")
        .join("dist");
    let candidates = [
        base.join("Electron.app")
            .join("Contents")
            .join("MacOS")
            .join("Electron"),
        base.join("electron"),
        base.join("electron.exe"),
    ];
    candidates.into_iter().find(|c| c.exists())
}

fn find_ffmpeg(project_dir: &Path) -> PathBuf {
    let bundled = project_dir.join("ffmpeg").join("ffmpeg");
    if bundled.exists() {
        bundled
    } else {
        PathBuf::from("ffmpeg")
    }
}

fn is_bilibili(url: &str) -> bool {
    url.contains("bilibili.com") || url.contains("b23.tv")
}

fn clean_name(input: &str) -> String {
    let mut out = String::new();
    for ch in input.trim().chars() {
        match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '&' | '#' | '.' | ',' | '，'
            | '。' | '~' | '！' | '·' | ' ' => out.push('_'),
            _ => out.push(ch),
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "bilibili".to_string()
    } else {
        trimmed
    }
}

async fn find_ytdlp(project_dir: &Path) -> String {
    if let Ok(found) = Command::new("yt-dlp").arg("--version").output().await
        && found.status.success()
    {
        return "yt-dlp".to_string();
    }
    let candidates = [
        project_dir.join(".venv").join("bin").join("yt-dlp"),
        project_dir.join(".venv").join("Scripts").join("yt-dlp.exe"),
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
    /// 下载完成后已自动入队复盘任务（转写/提炼/切片）。
    pub pipeline_queued: bool,
}

/// 把 pending 下载任务逐个执行（失败重试，成功标记）。
pub async fn process_pending(pool: &SqlitePool, project_dir: &Path) -> Result<usize> {
    // 上次崩溃遗留的 processing 任务先归还队列，避免永久卡死。
    db::recover_processing_by_kind(pool, "download").await?;
    let downloader = Downloader::new(project_dir.to_path_buf());
    let mut done = 0;
    while let Some(task) = db::claim_pending_task_by_kind(pool, "download", 1).await? {
        let outcome =
            run_single_task_with_timeout(pool, &task, &downloader, Duration::from_secs(600)).await;
        if outcome {
            done += 1;
        }
        if done >= 50 {
            break;
        }
    }
    Ok(done)
}

/// 入队一个复盘任务：kind="process"、url 是媒体文件绝对路径、name 是主播/博主名。
///
/// 手动 CLI/MCP 和下载/录制完成都走这一个入口。已存在未完成任务（pending/
/// processing/failed）时不再重复插入，避免同一媒体文件被重复转写/切片；
/// 已完成的任务保留历史记录，需要重新处理时应让调用方显式创建新任务或未来提供
/// “重新处理”入口，而不是悄悄覆盖 done。
pub async fn enqueue_process(pool: &SqlitePool, file: &str, name: &str) -> anyhow::Result<i64> {
    // 同一媒体文件已经排队或正在处理时不再重复入队。
    let existing: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM tasks WHERE kind='process' AND url=? AND status IN ('pending','processing','failed','done') LIMIT 1",
    )
    .bind(file)
    .fetch_optional(pool)
    .await
    .context("查询已有复盘任务")?;
    if let Some(id) = existing {
        return Ok(id);
    }
    db::enqueue_task(pool, "process", file, name).await
}

/// 下载完成后立即入队复盘任务，保持旧调用名兼容。
pub async fn enqueue_pipeline_after_download(
    pool: &SqlitePool,
    file: &str,
    name: &str,
) -> anyhow::Result<i64> {
    enqueue_process(pool, file, name).await
}

/// 独立下载队列 worker：不参与 70 秒 tick 护栏，每个任务自身有 600 秒超时，
/// 下载耗时不会阻塞直播/视频检查，也不会被 tick abort 后永远卡 processing。
pub async fn run_download_worker(
    pool: SqlitePool,
    project_dir: PathBuf,
    mut stop: tokio::sync::watch::Receiver<bool>,
    task_timeout: Duration,
    poll_interval: Duration,
) -> anyhow::Result<()> {
    // 启动前把上次崩溃/被 tick 截断遗留的 processing 任务归还队列。
    let recovered = db::recover_processing_by_kind(&pool, "download")
        .await
        .context("下载 worker 恢复处理中任务")?;
    if recovered > 0 {
        tracing::info!("下载 worker 启动，恢复 {recovered} 条 processing 任务");
    }
    let downloader = Downloader::new(project_dir);
    let mut idle = 0u64;
    loop {
        tokio::select! {
            _ = stop.changed() => {
                if *stop.borrow() {
                    tracing::info!("下载 worker 收到停止信号，退出");
                    return Ok(());
                }
            }
            _ = tokio::time::sleep(poll_interval) => {
                idle += 1;
            }
        }
        // 每轮最多处理 10 个任务；处理完即使还有队列，也先回到 select 响应停止。
        match drain_pending(&pool, &downloader, task_timeout, &stop).await {
            Ok(done) => {
                if done > 0 {
                    tracing::info!("下载 worker 本轮处理 {done} 个任务");
                    idle = 0;
                } else if idle >= 12 {
                    // 队列连续空闲约 1 分钟时写一条可见日志，便于确认 worker 活着。
                    tracing::info!("下载 worker 空闲中，等待下载队列");
                    idle = 0;
                }
            }
            Err(e) => {
                tracing::error!("下载 worker 处理队列失败，稍后重试: {e:#}");
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }
    }
}

/// 认领并执行队列里的下载任务，直到队列暂时为空或收到停止信号。
async fn drain_pending(
    pool: &SqlitePool,
    downloader: &Downloader,
    task_timeout: Duration,
    stop: &tokio::sync::watch::Receiver<bool>,
) -> Result<usize> {
    let mut done = 0usize;
    while let Some(task) = db::claim_pending_task_by_kind(pool, "download", 1).await? {
        let outcome = run_single_task_with_timeout(pool, &task, downloader, task_timeout).await;
        if outcome {
            done += 1;
        }
        if *stop.borrow() {
            tracing::info!("下载 worker 在任务间收到停止信号，退出");
            return Ok(done);
        }
    }
    Ok(done)
}

/// 执行单个下载任务并更新状态。返回是否成功完成。
/// 超时/失败都会显式更新状态，绝不把任务永久留在 processing。
async fn run_single_task_with_timeout(
    pool: &SqlitePool,
    task: &crate::model::Task,
    downloader: &Downloader,
    timeout_duration: Duration,
) -> bool {
    if task.kind != "download" {
        let _ = db::update_task_status(pool, task.id, "failed").await;
        return false;
    }
    let outcome = tokio::time::timeout(
        timeout_duration,
        downloader.download(&task.url, &task.name, pool),
    )
    .await;
    match outcome {
        Ok(Ok(_)) => db::update_task_status(pool, task.id, "done").await.is_ok(),
        Ok(Err(e)) => {
            tracing::warn!(
                "下载任务 id={} 失败（attempts={}）: {e:#}",
                task.id,
                task.attempts
            );
            // 失败后前 3 次回到 pending 重试；超过上限标 failed 并停止重试。
            let next = if task.attempts >= 3 {
                "failed"
            } else {
                "pending"
            };
            let _ = db::update_task_status(pool, task.id, next).await;
            false
        }
        Err(_) => {
            tracing::warn!(
                "下载任务 id={} 超时（attempts={}），已显式结束本轮",
                task.id,
                task.attempts
            );
            let next = if task.attempts >= 3 {
                "failed"
            } else {
                "pending"
            };
            let _ = db::update_task_status(pool, task.id, next).await;
            false
        }
    }
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

    #[test]
    fn bilibili_detection_and_name_cleanup() {
        assert!(is_bilibili("https://www.bilibili.com/video/BV1xx411c7mD"));
        assert!(is_bilibili("https://b23.tv/abc"));
        assert!(!is_bilibili("https://example.com/video"));
        assert_eq!(clean_name(" 主播/甲:测试? "), "主播_甲_测试");
        assert_eq!(clean_name("////"), "bilibili");
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

    #[tokio::test]
    async fn enqueue_process_deduplicates_active_and_completed_tasks() {
        let file = std::env::temp_dir().join(format!(
            "zunzhe-process-dedup-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = crate::db::open_pool(&file).await.unwrap();
        let media = "/tmp/same-media.mp4";

        let pending_id = enqueue_process(&pool, media, "主播甲").await.unwrap();
        let duplicate_pending = enqueue_process(&pool, media, "主播乙").await.unwrap();
        assert_eq!(
            pending_id, duplicate_pending,
            "同一媒体文件在 pending 时不应重复入队"
        );

        crate::db::update_task_status(&pool, pending_id, "done")
            .await
            .unwrap();
        let after_done = enqueue_process(&pool, media, "主播丙").await.unwrap();
        assert_eq!(
            after_done, pending_id,
            "同一媒体文件已完成时不应再创建重复 process 任务"
        );

        crate::db::set_task_status(&pool, pending_id, "failed")
            .await
            .unwrap();
        let after_failed = enqueue_process(&pool, media, "主播丁").await.unwrap();
        assert_eq!(
            after_failed, pending_id,
            "同一媒体文件失败后应复用任务，而不是重复生成"
        );
        std::mem::forget(file);
    }

    #[tokio::test]
    async fn timed_out_task_is_not_left_processing() {
        let file = std::env::temp_dir().join(format!(
            "zunzhe-dl-queue-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = crate::db::open_pool(&file).await.unwrap();
        let id = crate::db::enqueue_task(&pool, "download", "https://example.invalid/x", "测试")
            .await
            .unwrap();
        let task = crate::db::claim_pending_task(&pool, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.id, id);
        // 用不存在的项目目录构造 Downloader：下载必然快速失败，不应卡 processing。
        let downloader = Downloader::new(std::env::temp_dir().join("zunzhe-nonexistent"));
        let ok =
            run_single_task_with_timeout(&pool, &task, &downloader, Duration::from_secs(1)).await;
        assert!(!ok);
        let rows: Vec<crate::model::Task> =
            crate::db::list_tasks(&pool, "pending", 10).await.unwrap();
        assert!(
            rows.iter().any(|t| t.id == id),
            "失败任务应回到 pending 等待重试，而不是卡 processing"
        );
        std::mem::forget(file);
    }

    #[tokio::test]
    async fn repeated_failures_finally_mark_failed_and_stop() {
        let file = std::env::temp_dir().join(format!(
            "zunzhe-dl-fail-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = crate::db::open_pool(&file).await.unwrap();
        let id = crate::db::enqueue_task(&pool, "download", "https://example.invalid/x", "测试")
            .await
            .unwrap();
        let downloader = Downloader::new(std::env::temp_dir().join("zunzhe-nonexistent"));
        for _ in 0..3 {
            let task = crate::db::claim_pending_task_by_kind(&pool, "download", 1)
                .await
                .unwrap()
                .unwrap();
            assert!(
                !run_single_task_with_timeout(&pool, &task, &downloader, Duration::from_secs(1))
                    .await,
                "失败任务不应成功"
            );
        }
        let rows: Vec<crate::model::Task> =
            crate::db::list_tasks(&pool, "failed", 10).await.unwrap();
        assert!(
            rows.iter().any(|t| t.id == id),
            "第三次失败后应标 failed，当前: {rows:?}"
        );
        assert!(
            crate::db::claim_pending_task_by_kind(&pool, "download", 1)
                .await
                .unwrap()
                .is_none(),
            "failed 后不能再被自动认领"
        );
        std::mem::forget(file);
    }

    #[tokio::test]
    async fn download_worker_marks_stuck_task_failed() {
        let file = std::env::temp_dir().join(format!(
            "zunzhe-dl-worker-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = crate::db::open_pool(&file).await.unwrap();
        let id = crate::db::enqueue_task(&pool, "download", "https://example.invalid/x", "测试")
            .await
            .unwrap();
        // 预置两次重试次数：worker 超时一次后 attempts=3，直接标 failed。
        sqlx::query("UPDATE tasks SET attempts=2 WHERE id=?")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let (tx, rx) = tokio::sync::watch::channel(false);
        let worker = tokio::spawn(super::run_download_worker(
            pool.clone(),
            std::env::temp_dir().join("zunzhe-nonexistent"),
            rx,
            Duration::from_millis(100),
            Duration::from_millis(5),
        ));
        // 给 worker 一个短窗口处理任务，随后触发停止信号。
        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = tx.send(true);
        worker.await.unwrap().unwrap();
        let rows: Vec<crate::model::Task> =
            crate::db::list_tasks(&pool, "failed", 10).await.unwrap();
        assert!(
            rows.iter().any(|t| t.id == id),
            "超时任务应标 failed，当前: {rows:?}"
        );
        assert!(
            crate::db::claim_pending_task(&pool, 1)
                .await
                .unwrap()
                .is_none(),
            "failed 任务不应被下载 worker 重新认领"
        );
        let stuck: Vec<crate::model::Task> = crate::db::list_tasks(&pool, "processing", 10)
            .await
            .unwrap();
        assert!(
            stuck.iter().all(|t| t.id != id),
            "任务不能卡在 processing: {stuck:?}"
        );
        std::mem::forget(file);
    }

    #[tokio::test]
    #[ignore = "需要真实 Electron 与 B站网络，CI 不跑"]
    async fn real_bilibili_browser_download_smoke() {
        let project = std::env::current_dir()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let dir = std::env::temp_dir().join(format!("zunzhe-browser-dl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let result = bilibili_browser_download(
            &project,
            "https://www.bilibili.com/video/BV1UKgL6SEyA",
            &dir,
        )
        .await;
        assert!(result.is_some(), "真实浏览器下载兜底应成功");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
