use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use sqlx::sqlite::SqlitePool;
use tokio::process::Command;

use crate::bilibili_auth::{browser_cookie_path, ensure_anon_cookie, ensure_user_cookie_file};
use crate::db;
use crate::process::{ChildError, ManagedChild};

/// 把一次手动/批量下载请求加入 daemon 下载队列，返回任务 id。
///
/// CLI/MCP/桌面端必须走这一个入口，而不是直接调用 `Downloader::download`：
/// 否则手动请求会绕过队列，与 daemon 下载 worker 并发下载同一个 URL。
/// `db::enqueue_task` 对同 kind+url 的 pending/processing 任务去重，因此重复
/// 点击不会重复下载；若 daemon 未运行，调用方应先 `ensure_core` 拉起 worker。
pub async fn enqueue_download(pool: &SqlitePool, url: &str, name: &str) -> anyhow::Result<i64> {
    if url.trim().is_empty() {
        anyhow::bail!("下载地址不能为空");
    }
    db::enqueue_task(pool, "download", url, name).await
}

/// 批量入队下载：供“勾选批量下载/全部下载”使用。
///
/// 逐个复用 `enqueue_download` 的队列去重与 daemon 消费语义；已下载的视频
/// 直接跳过，避免“全部下载”重复拉取用户已经有的内容。返回汇总统计而不是
/// 中途失败，单个坏 URL 不影响其余任务入队。
#[derive(Debug, Clone, Serialize)]
pub struct BatchDownloadSummary {
    pub requested: usize,
    pub queued: usize,
    pub duplicated: usize,
    pub skipped_downloaded: usize,
    pub errors: usize,
    pub task_ids: Vec<i64>,
    pub error_messages: Vec<String>,
}

pub async fn enqueue_downloads(
    pool: &SqlitePool,
    items: &[(String, String)],
) -> anyhow::Result<BatchDownloadSummary> {
    let mut summary = BatchDownloadSummary {
        requested: items.len(),
        queued: 0,
        duplicated: 0,
        skipped_downloaded: 0,
        errors: 0,
        task_ids: Vec::new(),
        error_messages: Vec::new(),
    };
    let mut seen = HashSet::new();
    for (url, name) in items {
        let url = url.trim();
        if url.is_empty() {
            summary.errors += 1;
            summary.error_messages.push("空 URL".to_string());
            continue;
        }
        // “全部下载”不能重复拉已下载内容；按 URL 查询视频库，存在且已下载
        // 时跳过。视频库缺失时仍允许下载（浏览列表还没入库的旧链接）。
        let known_downloaded =
            sqlx::query_scalar::<_, i64>("SELECT downloaded FROM videos WHERE url=? LIMIT 1")
                .bind(url)
                .fetch_optional(pool)
                .await
                .context("查询视频库下载状态")?;
        if known_downloaded == Some(1) {
            summary.skipped_downloaded += 1;
            continue;
        }
        // 同一批内重复 URL 只入队一次；后续相同项计入 duplicated，避免
        // “全部下载”里重复条目被并发创建多个任务。
        if !seen.insert(url.to_string()) {
            summary.duplicated += 1;
            continue;
        }
        // 同一 URL 已在 pending/processing：不重复入队，计入 duplicated。
        let existing = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM tasks WHERE kind='download' AND url=? AND status IN ('pending','processing') LIMIT 1",
        )
        .bind(url)
        .fetch_optional(pool)
        .await
        .context("查询已有下载任务")?;
        if let Some(id) = existing {
            summary.duplicated += 1;
            summary.task_ids.push(id);
            continue;
        }
        match db::enqueue_task(pool, "download", url, name).await {
            Ok(id) => {
                summary.queued += 1;
                summary.task_ids.push(id);
            }
            Err(e) => {
                summary.errors += 1;
                summary.error_messages.push(format!("{url}: {e:#}"));
            }
        }
    }
    Ok(summary)
}

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
        // 主播名来自外部输入，必须清洗后再拼路径：防止 `../`、`/`、Windows
        // 保留设备名等把文件写到 downloads/短视频/ 之外或产生非法目录。
        let safe_name = safe_dir_name(name);
        let dir = self
            .project_dir
            .join("downloads")
            .join("短视频")
            .join(&safe_name);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("创建下载目录失败: {}", dir.display()))?;

        // B站匿名请求容易被 412 风控，浏览器内核提取直链最可靠。
        if is_bilibili(url) {
            let output = bilibili_browser_download(&self.project_dir, url, &dir).await;
            if let Some(file) = output {
                return self.finish_download(url, &safe_name, pool, file).await;
            }
            tracing::warn!("B站浏览器下载兜底失败，回退 yt-dlp: {url}");
        }

        let file = self.ytdlp_download(url, &dir).await?;
        self.finish_download(url, &safe_name, pool, file).await
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
        if let Err(e) = db::ensure_video_row(pool, &video_id, url, name, name).await {
            // 视频库记录失败不能把已成功下载的文件当作失败：文件仍保留，
            // 但必须留下可诊断日志，否则“已下载”状态会悄悄丢失。
            tracing::warn!("下载成功但补写视频库记录失败 video_id={video_id}: {e:#}");
        }
        if let Err(e) = db::mark_downloaded(pool, &video_id, true).await {
            tracing::warn!("下载成功但标记视频库已下载失败 video_id={video_id}: {e:#}");
        }
        // 非 B站/RSS/YouTube 等平台没有统一的 URL→id 提取规则，浏览历史里的
        // id 可能是 GUID 或完整 URL；按 URL 再标记一次，避免同一视频在库里
        // 同时存在 downloaded=1（id 推导命中）和 downloaded=0（历史浏览记录）。
        if let Err(e) = db::mark_downloaded_by_url(pool, url, true).await {
            tracing::warn!("下载成功但按 URL 标记视频库已下载失败 url={url}: {e:#}");
        }
        // 下载完成后是否入队复盘任务（转写/提炼/切片），由 fupan_config.ini
        // 三个独立开关决定；三项全关时不再入队，避免每次下载都白跑一次
        // Python 流水线。手动 process_media 仍可强制处理。
        let pipeline_enabled = pipeline_enabled(&self.project_dir);
        let mut pipeline_queued = false;
        if pipeline_enabled {
            pipeline_queued = enqueue_pipeline_after_download(pool, &self.project_dir, &file, name)
                .await
                .is_ok();
        }
        Ok(DownloadResult {
            ok: true,
            file,
            name: name.to_string(),
            pipeline_queued,
        })
    }

    async fn ytdlp_download(&self, url: &str, dir: &Path) -> Result<String> {
        let ytdlp = find_ytdlp(&self.project_dir).await;
        let mut cmd = Command::new(&ytdlp);
        // 必须在 yt-dlp 启动前拍下目录快照；等它退出后再取，无法区分
        // “这次新写出的文件”和“执行前就存在的旧文件”。
        let before = files_in_dir(dir);
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
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child =
            ManagedChild::spawn(&mut cmd).with_context(|| format!("启动 yt-dlp 失败: {ytdlp}"))?;
        let output = match child.wait_with_output(Duration::from_secs(600)).await {
            Ok(output) => output,
            Err(ChildError::Io(e)) => return Err(e).context("读取 yt-dlp 输出失败"),
            Err(ChildError::Timeout) => {
                anyhow::bail!("yt-dlp 下载超时（10 分钟），已终止整个进程组")
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!(
                "yt-dlp 下载失败: {}",
                stderr.chars().take(500).collect::<String>()
            ));
        }

        // yt-dlp 已存在同名文件且 `--no-overwrites` 生效时仍会返回 0，但目录里
        // 这次没有新文件；此时不能把“别的旧文件”当成本次下载成功。
        let file = find_latest_video(dir).unwrap_or_default();
        if file.is_empty() {
            return Err(anyhow!("yt-dlp 报告成功但没有产出视频文件"));
        }
        if before.contains(&file) {
            let output_text = String::from_utf8_lossy(&output.stdout).to_string()
                + &String::from_utf8_lossy(&output.stderr);
            if output_text.contains("has already been downloaded") {
                tracing::info!("yt-dlp 跳过已存在文件: {file}");
                return Ok(file);
            }
            return Err(anyhow!(
                "yt-dlp 报告成功但没有产出新的视频文件（同名文件已存在且未明确跳过）"
            ));
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
    let cookie_files = crate::bilibili_auth::electron_cookie_files(project_dir);
    let cookie_env: Vec<(&str, &std::path::Path)> = cookie_files
        .iter()
        .enumerate()
        .map(|(i, p)| {
            if i == 0 {
                ("FUPAN_COOKIE_FILE", p.as_path())
            } else {
                ("FUPAN_USER_COOKIE_FILE", p.as_path())
            }
        })
        .collect();
    let mut cmd = Command::new(&electron);
    cmd.kill_on_drop(true)
        .arg(&extract)
        .env("FUPAN_VIDEO_URL", url)
        .env("FUPAN_OUT_FILE", &out_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (key, value) in &cookie_env {
        cmd.env(key, value);
    }
    let child = match ManagedChild::spawn(&mut cmd) {
        Ok(child) => child,
        Err(e) => {
            tracing::warn!("B站浏览器提取进程启动失败: {e}");
            return None;
        }
    };
    let status = match child.wait(Duration::from_secs(60)).await {
        Ok(status) => status,
        Err(ChildError::Timeout) => {
            tracing::warn!("B站浏览器提取超时（60 秒）");
            let _ = std::fs::remove_file(&out_file);
            return None;
        }
        Err(ChildError::Io(e)) => {
            tracing::warn!("B站浏览器提取进程读取失败: {e}");
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
    let child = ManagedChild::spawn(&mut cmd).ok()?;
    let output = match child.wait_with_output(Duration::from_secs(600)).await {
        Ok(output) => output,
        Err(ChildError::Timeout) | Err(ChildError::Io(_)) => return None,
    };
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
    crate::service::find_binary(project_dir, "ffmpeg", "ffmpeg.exe")
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

/// 下载目录名必须是一个安全的单层目录名：继续复用 `clean_name` 的非法字符清洗，
/// 并额外处理 Windows 保留设备名，避免 `CON`、`NUL` 等名字在 Windows 上写失败。
fn safe_dir_name(input: &str) -> String {
    let cleaned = clean_name(input);
    if cleaned.is_empty() {
        return "bilibili".to_string();
    }
    const WINDOWS_RESERVED: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    if WINDOWS_RESERVED.contains(&cleaned.to_ascii_lowercase().as_str()) {
        format!("{cleaned}_")
    } else {
        cleaned
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
            // 只认 yt-dlp 的 mp4 输出；目录里的 .part/.ytdl 临时文件、
            // 封面图/描述文件都不算下载产物，避免把不完整文件或无关文件
            // 当成“最新视频”。
            if path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .as_deref()
                != Some("mp4")
            {
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

fn files_in_dir(dir: &Path) -> std::collections::HashSet<String> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.path().display().to_string())
        .collect()
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
pub async fn enqueue_process(
    pool: &SqlitePool,
    project_dir: &Path,
    file: &str,
    name: &str,
) -> anyhow::Result<i64> {
    // 复盘流水线会调用本地 Python 处理该文件；只有项目内可信媒体路径才允许
    // 入队。绝对路径必须落在 downloads/ 或 fupan_output/，相对路径不能含 `..`，
    // 防止外部调用者让 daemon 任意处理本机路径（例如读取/转写敏感文件）。
    let media_path = Path::new(file);
    let trusted = if media_path.is_absolute() {
        let downloads = project_dir.join("downloads");
        let fupan = project_dir.join("fupan_output");
        media_path.starts_with(&downloads) || media_path.starts_with(&fupan)
    } else {
        !media_path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    };
    if !trusted {
        anyhow::bail!("复盘任务只接受 downloads/ 或 fupan_output/ 下的媒体路径: {file}");
    }
    // 同一媒体文件已经在排队、正在处理或已经完成时不再重复入队，避免重复
    // 转写/切片；只有 failed 是重试耗尽后的终态，手动重试应创建新任务。
    let existing: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM tasks WHERE kind='process' AND url=? AND status IN ('pending','processing','done') LIMIT 1",
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
    project_dir: &Path,
    file: &str,
    name: &str,
) -> anyhow::Result<i64> {
    enqueue_process(pool, project_dir, file, name).await
}

/// 复盘流水线是否启用：转写/知识提炼/切片任一开启即认为启用。
///
/// 下载完成与直播录制完成共用同一判断，避免两处逻辑漂移；配置读取失败时
/// 按“启用”处理，保证配置损坏也不会让自动复盘静默关闭。
pub fn pipeline_enabled(project_dir: &Path) -> bool {
    PipelineSwitches::load(project_dir).any_enabled()
}

/// 复盘流水线的三个独立开关。
///
/// 需求契约：转写/知识提炼/精华切片各自可开可关，关闭的环节完全不运行。
/// 配置缺失或读取失败时按“启用”处理，保证配置损坏不会让自动复盘静默关闭。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipelineSwitches {
    pub transcribe: bool,
    pub distill: bool,
    pub clips: bool,
}

impl PipelineSwitches {
    pub fn load(project_dir: &Path) -> Self {
        let cfg = crate::config::Config::load(project_dir);
        let enabled = |section: &str, key: &str| -> bool {
            cfg.as_ref()
                .map(|c| c.is_yes_fupan(section, key, true))
                .unwrap_or(true)
        };
        Self {
            transcribe: enabled("转写", "是否启用转写(是/否)"),
            distill: enabled("知识提炼", "是否启用知识提炼(是/否)"),
            clips: enabled("切片", "是否启用切片(是/否)"),
        }
    }

    pub fn any_enabled(&self) -> bool {
        self.transcribe || self.distill || self.clips
    }

    pub fn all_disabled(&self) -> bool {
        !self.any_enabled()
    }
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
                // 运行中自愈：正常路径不会留下超限 processing，但 DB 更新失败
                // 或崩溃恢复异常时不能让任务永久卡死。
                if let Err(e) = db::fail_stale_processing_by_kind(&pool, "download", 3).await {
                    tracing::warn!("下载 worker 自愈超限任务失败: {e:#}");
                }
            }
            Err(e) => {
                tracing::error!("下载 worker 处理队列失败，稍后重试: {e:#}");
                // 用 select 等待而不是普通 sleep：daemon 收 SIGTERM 时立即退出，
                // 不会因队列失败重试窗口再拖 10 秒。
                tokio::select! {
                    _ = stop.changed() => {
                        if *stop.borrow() {
                            tracing::info!("下载 worker 在重试等待中收到停止信号，退出");
                            return Ok(());
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_secs(10)) => {}
                }
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
    fn latest_video_ignores_non_mp4_files() {
        let dir = std::env::temp_dir().join(format!("zunzhe-dl-ext-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join("partial.mp4.part");
        let json = dir.join("meta.json");
        let cover = dir.join("cover.jpg");
        let mp4 = dir.join("video.mp4");
        std::fs::write(&part, b"partial").unwrap();
        std::fs::write(&json, b"{}").unwrap();
        std::fs::write(&cover, b"img").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&mp4, b"real").unwrap();
        assert_eq!(
            find_latest_video(&dir).as_deref(),
            Some(mp4.display().to_string()).as_deref(),
            "只应返回完整 mp4，忽略 .part/封面/描述文件"
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

    #[test]
    fn safe_dir_name_never_escapes_download_root() {
        // clean_name 会把 `.` `/` 一并换成 `_` 并裁掉首尾，因此 ../evil
        // 最终变成单层 `evil`，不可能形成父目录跳转。
        assert_eq!(safe_dir_name("../evil"), "evil");
        assert_eq!(safe_dir_name("a/b\\c"), "a_b_c");
        assert_eq!(safe_dir_name(".."), "bilibili");
        assert_eq!(safe_dir_name("."), "bilibili");
        assert_eq!(safe_dir_name("////"), "bilibili");
        assert_eq!(safe_dir_name("CON"), "CON_");
        assert_eq!(safe_dir_name("NUL"), "NUL_");
        assert_eq!(safe_dir_name(""), "bilibili");
    }

    #[test]
    fn pipeline_enabled_respects_three_switches() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-pipeline-switch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("config")).unwrap();

        // 三项全关：自动复盘不入队。
        std::fs::write(
            dir.join("config").join("fupan_config.ini"),
            "[转写]\n是否启用转写(是/否) = 否\n\
             [知识提炼]\n是否启用知识提炼(是/否) = 否\n\
             [切片]\n是否启用切片(是/否) = 否\n",
        )
        .unwrap();
        std::fs::write(dir.join("config").join("config.ini"), "[推送配置]\n").unwrap();
        std::fs::write(
            dir.join("config").join("fupan_config.ini.example"),
            "[转写]\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("config").join("config.ini.example"),
            "[推送配置]\n",
        )
        .unwrap();
        assert!(!pipeline_enabled(&dir), "三项全关时不应启用自动复盘");

        // 任一开启：自动复盘启用。
        std::fs::write(
            dir.join("config").join("fupan_config.ini"),
            "[转写]\n是否启用转写(是/否) = 否\n\
             [知识提炼]\n是否启用知识提炼(是/否) = 否\n\
             [切片]\n是否启用切片(是/否) = 是\n",
        )
        .unwrap();
        assert!(pipeline_enabled(&dir), "切片开启时应启用自动复盘");

        // 配置读取失败：按启用处理，不静默关闭自动复盘。
        std::fs::remove_file(dir.join("config").join("fupan_config.ini")).unwrap();
        assert!(pipeline_enabled(&dir), "配置缺失时应按启用处理");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pipeline_switches_are_independent() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-pipeline-indep-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(dir.join("config").join("config.ini"), "[推送配置]\n").unwrap();
        std::fs::write(
            dir.join("config").join("fupan_config.ini"),
            "[转写]\n是否启用转写(是/否) = 是\n\
             [知识提炼]\n是否启用知识提炼(是/否) = 否\n\
             [切片]\n是否启用切片(是/否) = 否\n",
        )
        .unwrap();
        let switches = PipelineSwitches::load(&dir);
        assert!(switches.transcribe);
        assert!(!switches.distill);
        assert!(!switches.clips);
        assert!(switches.any_enabled());

        // 配置损坏仍按“启用”兜底，不能让自动复盘静默关闭。
        std::fs::write(dir.join("config").join("fupan_config.ini"), "bad").unwrap();
        let switches = PipelineSwitches::load(&dir);
        assert!(switches.transcribe && switches.distill && switches.clips);
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

        let project_dir = std::env::temp_dir().join("zunzhe-enqueue-project");
        let media_dir = project_dir.join("downloads/短视频/主播甲");
        std::fs::create_dir_all(&media_dir).unwrap();
        let media = media_dir.join("same-media.mp4");
        let media = media.display().to_string();
        let pending_id = enqueue_process(&pool, &project_dir, &media, "主播甲")
            .await
            .unwrap();
        let duplicate_pending = enqueue_process(&pool, &project_dir, &media, "主播乙")
            .await
            .unwrap();
        assert_eq!(
            pending_id, duplicate_pending,
            "同一媒体文件在 pending 时不应重复入队"
        );

        crate::db::update_task_status(&pool, pending_id, "done")
            .await
            .unwrap();
        let after_done = enqueue_process(&pool, &project_dir, &media, "主播丙")
            .await
            .unwrap();
        assert_eq!(
            after_done, pending_id,
            "同一媒体文件已完成时不应再创建重复 process 任务"
        );

        crate::db::set_task_status(&pool, pending_id, "failed")
            .await
            .unwrap();
        let after_failed = enqueue_process(&pool, &project_dir, &media, "主播丁")
            .await
            .unwrap();
        assert_ne!(after_failed, pending_id, "失败后手动重试应创建新任务");
        assert_eq!(
            crate::db::list_tasks(&pool, "pending", 10)
                .await
                .unwrap()
                .len(),
            1,
            "失败后重新入队应创建新的 pending 任务，而不是返回旧 failed id"
        );
        let dup_retry = enqueue_process(&pool, &project_dir, &media, "主播戊")
            .await
            .unwrap();
        assert_eq!(dup_retry, after_failed, "pending 中再次入队应去重");
        std::mem::forget(file);
    }

    #[tokio::test]
    async fn enqueue_downloads_batch_skips_downloaded_and_dedupes() {
        let file = std::env::temp_dir().join(format!(
            "zunzhe-batch-dl-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = crate::db::open_pool(&file).await.unwrap();
        crate::db::init_db(&pool).await.unwrap();
        // 已下载记录：批量“全部下载”应跳过。
        crate::db::upsert_video(
            &pool,
            &crate::model::VideoMeta {
                id: "BVdownloaded".into(),
                platform: "bilibili".into(),
                url: "https://www.bilibili.com/video/BVdownloaded".into(),
                title: "已下载".into(),
                cover: String::new(),
                published_at: String::new(),
                duration: 0,
            },
            "主播",
            "https://space.bilibili.com/1",
            "",
        )
        .await
        .unwrap();
        crate::db::mark_downloaded(&pool, "BVdownloaded", true)
            .await
            .unwrap();
        let url_new = "https://www.bilibili.com/video/BVnew";
        let url_dup = "https://www.bilibili.com/video/BVdup";
        let summary = enqueue_downloads(
            &pool,
            &[
                (
                    "https://www.bilibili.com/video/BVdownloaded".to_string(),
                    "主播".to_string(),
                ),
                (url_new.to_string(), "主播".to_string()),
                (url_dup.to_string(), "主播".to_string()),
                (url_dup.to_string(), "主播".to_string()),
                (String::new(), "主播".to_string()),
            ],
        )
        .await
        .unwrap();
        assert_eq!(summary.requested, 5);
        assert_eq!(summary.queued, 2, "新 URL 与重复 pending 只算一次入队");
        assert_eq!(summary.duplicated, 1);
        assert_eq!(summary.skipped_downloaded, 1);
        assert_eq!(summary.errors, 1);
        assert_eq!(summary.task_ids.len(), 2);
        let tasks = crate::db::list_tasks(&pool, "pending", 10).await.unwrap();
        assert_eq!(tasks.len(), 2);
        std::mem::forget(file);
    }

    #[tokio::test]
    async fn enqueue_process_rejects_paths_outside_project() {
        let file = std::env::temp_dir().join(format!(
            "zunzhe-process-reject-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = crate::db::open_pool(&file).await.unwrap();
        let project_dir = std::env::temp_dir().join("zunzhe-enqueue-project-reject");

        let outside = std::env::temp_dir().join("untrusted-media.mp4");
        let err = enqueue_process(
            &pool,
            &project_dir,
            outside.display().to_string().as_str(),
            "外部",
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("只接受 downloads/ 或 fupan_output/")
        );

        let traversal = "../etc/passwd";
        let err = enqueue_process(&pool, &project_dir, traversal, "穿越")
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("只接受 downloads/ 或 fupan_output/")
        );
        std::mem::forget(file);
    }

    #[tokio::test]
    async fn enqueue_download_deduplicates_and_validates_url() {
        let file = std::env::temp_dir().join(format!(
            "zunzhe-dl-enqueue-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = crate::db::open_pool(&file).await.unwrap();

        let err = enqueue_download(&pool, "  ", "测试").await.unwrap_err();
        assert!(err.to_string().contains("不能为空"));

        let id = enqueue_download(&pool, "https://x.test/video", "博主")
            .await
            .unwrap();
        let dup = enqueue_download(&pool, "https://x.test/video", "博主2")
            .await
            .unwrap();
        assert_eq!(id, dup, "同一 URL 未完成任务必须去重，不能并发下载");

        // 失败终态允许手动重试：enqueue_task 会新建任务而不是被旧记录挡死。
        crate::db::set_task_status(&pool, id, "failed")
            .await
            .unwrap();
        let retry = enqueue_download(&pool, "https://x.test/video", "博主")
            .await
            .unwrap();
        assert_ne!(retry, id, "failed 终态应允许用户再次入队");
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
            // 退避已由 db 测试单独覆盖；这里把失败任务拨到 1 小时前，
            // 快速验证“3 次失败后落定 failed”的计数语义，不等待真实退避。
            sqlx::query(
                "UPDATE tasks SET updated_at=datetime('now','localtime','-1 hour') WHERE id=?",
            )
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
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
        // attempts=2 有 5 分钟退避；测试把时间拨到 1 小时前，worker 才能立即认领。
        sqlx::query("UPDATE tasks SET updated_at=datetime('now','localtime','-1 hour') WHERE id=?")
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
