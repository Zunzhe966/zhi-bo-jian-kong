use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// 与 Python 版 fupan 兼容的监控列表与偏好读写。
/// 直播列表: config/URL_config.ini（地址[,备注][,画质]）
/// 视频列表: config/视频监控列表.ini（地址,名称[,下载=是/否]）
/// 偏好:     config/监控偏好.json（url -> {notify, name}）
pub struct Watchlist {
    project_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LiveItem {
    pub url: String,
    pub remark: String,
    pub quality: String,
    pub enabled: bool,
    pub notify: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct VideoSource {
    pub url: String,
    pub name: String,
    pub download: Option<bool>,
    pub notify: String,
    pub enabled: bool,
}

/// 直播源在 `anchors` 兼容字段里的展示视图。
///
/// SQLite `anchors` 表只保留旧核心 DB 迁移的历史数据，不参与任何监控决策；
/// 直播监控的唯一权威是 `URL_config.ini`。为了不让旧记录把视频空间地址
/// 显示成“直播主播”，对外展示统一由当前直播列表生成。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnchorView {
    pub url: String,
    pub name: String,
    pub enabled: bool,
    pub notify_channel: String,
}

impl Watchlist {
    /// 返回与 `list_live` 同源的 anchors 兼容视图；不读取 SQLite。
    pub fn live_anchor_view(&self) -> Vec<AnchorView> {
        self.list_live()
            .into_iter()
            .map(|item| AnchorView {
                url: item.url,
                name: item.remark,
                enabled: item.enabled,
                notify_channel: item.notify,
            })
            .collect()
    }
}

impl Watchlist {
    pub fn new(project_dir: PathBuf) -> Self {
        Self { project_dir }
    }

    pub fn url_config(&self) -> PathBuf {
        self.project_dir.join("config").join("URL_config.ini")
    }

    pub fn video_config(&self) -> PathBuf {
        self.project_dir.join("config").join("视频监控列表.ini")
    }

    pub fn prefs_file(&self) -> PathBuf {
        self.project_dir.join("config").join("监控偏好.json")
    }

    /// 返回监控列表里的**所有**直播源，包括被暂停（`#` 注释）的项。
    ///
    /// 暂停项 `enabled=false`，由调用方决定是否参与检测/录制；桌面端界面需要
    /// 把它们也显示出来（灰显“已暂停”），否则点“不监控”会让主播直接从列表消失。
    /// 检测循环（`live_monitor::run_once`）会自行 `filter(|i| i.enabled)`，不受影响。
    pub fn list_live(&self) -> Vec<LiveItem> {
        read_lines(&self.url_config())
            .into_iter()
            .filter(|line| is_source_line(line))
            .map(|line| parse_live_line(&line))
            .filter(|item| !item.url.is_empty())
            .map(|mut item| {
                item.notify = self.notify_for(&item.url, &item.remark);
                item
            })
            .collect()
    }

    pub fn add_live(&self, url: &str, remark: &str, quality: &str) -> Result<()> {
        if url.trim().is_empty() {
            bail!("直播间地址不能为空");
        }
        let url = normalize_url(url);
        let line = format_live_line(&url, remark, quality);
        let mut lines = read_lines(&self.url_config());
        let replaced = lines.iter_mut().any(|old| {
            let old_url = parse_live_line(old).url;
            !old_url.is_empty() && old_url.trim_end_matches('/') == url.trim_end_matches('/')
        });
        if replaced {
            lines.iter_mut().for_each(|old| {
                let old_url = parse_live_line(old).url;
                if !old_url.is_empty() && old_url.trim_end_matches('/') == url.trim_end_matches('/')
                {
                    *old = line.clone();
                }
            });
        } else {
            lines.push(line);
        }
        write_lines(&self.url_config(), &lines)
    }

    pub fn remove_live(&self, url: &str) -> Result<bool> {
        // Python 语义：remove-live = 暂停（注释掉），保留在列表。
        let before = read_lines(&self.url_config());
        let mut changed = false;
        let after: Vec<String> = before
            .into_iter()
            .map(|old| {
                let stripped = old.trim();
                let parsed = parse_live_line(stripped);
                if !stripped.starts_with('#')
                    && parsed.url.trim_end_matches('/') == url.trim_end_matches('/')
                {
                    changed = true;
                    format!("#{stripped}")
                } else {
                    old
                }
            })
            .collect();
        if !changed {
            return Ok(false);
        }
        write_lines(&self.url_config(), &after)?;
        Ok(true)
    }

    pub fn delete_live(&self, url: &str) -> Result<bool> {
        // Python 语义：delete-live = 彻底删除，并清理偏好。
        let before = read_lines(&self.url_config());
        let original_len = before.len();
        let after: Vec<String> = before
            .into_iter()
            .filter(|old| !same_url(&parse_live_line(old).url, url))
            .collect();
        if after.len() == original_len {
            return Ok(false);
        }
        write_lines(&self.url_config(), &after)?;
        self.delete_pref(url.trim());
        Ok(true)
    }

    /// 返回监控列表里的**所有**视频源，包括被暂停（`#` 注释）的项。
    /// 暂停项 `enabled=false`，桌面端界面需要它们灰显“已暂停”而不是直接消失。
    pub fn list_video(&self) -> Vec<VideoSource> {
        read_lines(&self.video_config())
            .into_iter()
            .filter(|line| is_source_line(line))
            .map(|line| parse_video_line(&line))
            .filter(|item| !item.url.is_empty())
            .map(|mut item| {
                let n = item.name.clone();
                item.notify = self.notify_for(&item.url, &n);
                item
            })
            .collect()
    }

    pub fn add_video(
        &self,
        url: &str,
        name: &str,
        download: Option<bool>,
        notify: Option<&str>,
    ) -> Result<()> {
        if url.trim().is_empty() {
            bail!("监控地址不能为空");
        }
        let name = if name.trim().is_empty() { url } else { name };
        let line = format_video_line(url, name, download);
        let mut lines = read_lines(&self.video_config());
        let replaced = lines.iter_mut().any(|old| {
            let old_url = parse_video_line(old).url;
            !old_url.is_empty() && old_url == url.trim()
        });
        if replaced {
            lines.iter_mut().for_each(|old| {
                let old_url = parse_video_line(old).url;
                if !old_url.is_empty() && old_url == url.trim() {
                    *old = line.clone();
                }
            });
        } else {
            lines.push(line);
        }
        write_lines(&self.video_config(), &lines)?;
        if let Some(channel) = notify {
            self.set_notify(url, channel, name)?;
        }
        Ok(())
    }

    pub fn remove_video(&self, url: &str) -> Result<bool> {
        let before = read_lines(&self.video_config());
        let after: Vec<String> = before
            .iter()
            .filter(|old| !same_video_url(&parse_video_line(old).url, url))
            .cloned()
            .collect();
        if after.len() == before.len() {
            return Ok(false);
        }
        write_lines(&self.video_config(), &after)?;
        self.delete_pref(url.trim());
        Ok(true)
    }

    /// 修改单个直播源开关：`key` ∈ {enabled, notify, quality}。
    /// - enabled：`是/否` → 注释/取消注释整行（暂停/恢复监控）
    /// - notify：通知通道 → 写监控偏好.json
    /// - quality：画质 → 重写行（保留 URL 与备注）
    /// 返回是否真的改动了列表。
    pub fn set_live(&self, url: &str, key: &str, value: &str) -> Result<bool> {
        match key {
            "notify" => {
                let item = self
                    .list_live()
                    .into_iter()
                    .find(|i| same_url(&i.url, url))
                    .ok_or_else(|| anyhow::anyhow!("直播监控不存在: {url}"))?;
                self.set_notify(&item.url, value, &item.remark)?;
                Ok(true)
            }
            "enabled" => {
                let on = parse_switch(value)?;
                let mut changed = false;
                let before = read_lines(&self.url_config());
                let after: Vec<String> = before
                    .into_iter()
                    .map(|old| {
                        let stripped = old.trim();
                        let parsed = parse_live_line(stripped);
                        if !parsed.url.is_empty() && same_url(&parsed.url, url) {
                            let mut next = stripped.to_string();
                            if on {
                                next = next.trim_start_matches('#').trim().to_string();
                            } else if !next.starts_with('#') {
                                next = format!("#{next}");
                            }
                            if next != old {
                                changed = true;
                            }
                            next
                        } else {
                            old
                        }
                    })
                    .collect();
                write_lines(&self.url_config(), &after)?;
                Ok(changed)
            }
            "quality" => {
                let item = self
                    .list_live()
                    .into_iter()
                    .find(|i| same_url(&i.url, url))
                    .ok_or_else(|| anyhow::anyhow!("直播监控不存在: {url}"))?;
                let line = format_live_line(&item.url, &item.remark, value);
                let mut changed = false;
                let before = read_lines(&self.url_config());
                let after: Vec<String> = before
                    .into_iter()
                    .map(|old| {
                        let parsed = parse_live_line(&old);
                        if !parsed.url.is_empty() && same_url(&parsed.url, url) {
                            changed = true;
                            line.clone()
                        } else {
                            old
                        }
                    })
                    .collect();
                write_lines(&self.url_config(), &after)?;
                Ok(changed)
            }
            other => bail!("不支持的直播源开关: {other}（可用 enabled/notify/quality）"),
        }
    }

    /// 修改单个视频源开关：`key` ∈ {enabled, notify, download, name}。
    /// - enabled：`是/否` → 注释/取消注释整行（暂停/恢复监控）
    /// - notify：通知通道 → 写监控偏好.json
    /// - download：`是/否` → 重写行内的 `下载=` 段（保留名称）
    /// - name：改名 → 重写行（保留 download 设置）
    pub fn set_video(&self, url: &str, key: &str, value: &str) -> Result<bool> {
        match key {
            "notify" => {
                let item = self
                    .list_video()
                    .into_iter()
                    .find(|i| same_video_url(&i.url, url))
                    .ok_or_else(|| anyhow::anyhow!("视频监控不存在: {url}"))?;
                self.set_notify(&item.url, value, &item.name)?;
                Ok(true)
            }
            "enabled" => {
                let on = parse_switch(value)?;
                let mut changed = false;
                let before = read_lines(&self.video_config());
                let after: Vec<String> = before
                    .into_iter()
                    .map(|old| {
                        let stripped = old.trim();
                        let parsed = parse_video_line(stripped);
                        if !parsed.url.is_empty() && same_video_url(&parsed.url, url) {
                            let mut next = stripped.to_string();
                            if on {
                                next = next.trim_start_matches('#').trim().to_string();
                            } else if !next.starts_with('#') {
                                next = format!("#{next}");
                            }
                            if next != old {
                                changed = true;
                            }
                            next
                        } else {
                            old
                        }
                    })
                    .collect();
                write_lines(&self.video_config(), &after)?;
                Ok(changed)
            }
            "download" => {
                let on = parse_switch(value)?;
                let item = self
                    .list_video()
                    .into_iter()
                    .find(|i| same_video_url(&i.url, url))
                    .ok_or_else(|| anyhow::anyhow!("视频监控不存在: {url}"))?;
                let line = format_video_line(&item.url, &item.name, Some(on));
                let mut changed = false;
                let before = read_lines(&self.video_config());
                let after: Vec<String> = before
                    .into_iter()
                    .map(|old| {
                        let parsed = parse_video_line(&old);
                        if !parsed.url.is_empty() && same_video_url(&parsed.url, url) {
                            changed = true;
                            line.clone()
                        } else {
                            old
                        }
                    })
                    .collect();
                write_lines(&self.video_config(), &after)?;
                Ok(changed)
            }
            "name" => {
                if value.trim().is_empty() {
                    bail!("主播名不能为空");
                }
                let item = self
                    .list_video()
                    .into_iter()
                    .find(|i| same_video_url(&i.url, url))
                    .ok_or_else(|| anyhow::anyhow!("视频监控不存在: {url}"))?;
                let line = format_video_line(&item.url, value.trim(), item.download);
                let mut changed = false;
                let before = read_lines(&self.video_config());
                let after: Vec<String> = before
                    .into_iter()
                    .map(|old| {
                        let parsed = parse_video_line(&old);
                        if !parsed.url.is_empty() && same_video_url(&parsed.url, url) {
                            changed = true;
                            line.clone()
                        } else {
                            old
                        }
                    })
                    .collect();
                write_lines(&self.video_config(), &after)?;
                Ok(changed)
            }
            other => bail!("不支持的视频源开关: {other}（可用 enabled/notify/download/name）"),
        }
    }

    pub fn notify_for(&self, url: &str, name: &str) -> String {
        let prefs = self.load_prefs();
        if let Some(v) = prefs.get(url.trim())
            && let Some(channel) = v.get("notify").map(|s| s.as_str())
            && !channel.is_empty()
        {
            return channel.to_string();
        }
        if !name.is_empty() {
            for v in prefs.values() {
                if v.get("name").map(|s| s.as_str()) == Some(name)
                    && let Some(channel) = v.get("notify").map(|s| s.as_str())
                    && !channel.is_empty()
                {
                    return channel.to_string();
                }
            }
        }
        "跟随全局".to_string()
    }

    pub fn set_notify(&self, url: &str, channel: &str, name: &str) -> Result<()> {
        let mut prefs = self.load_prefs();
        match channel.trim() {
            "" | "跟随全局" => {
                prefs.remove(url.trim());
            }
            "微信" | "邮箱" | "PushPlus" | "Bark" | "ntfy" | "不通知" => {
                let entry = prefs.entry(url.trim().to_string()).or_default();
                entry.insert("notify".to_string(), channel.trim().to_string());
                if !name.is_empty() {
                    entry.insert("name".to_string(), name.to_string());
                }
            }
            other => bail!("未知通知通道: {other}"),
        }
        self.save_prefs(&prefs)
    }

    pub fn delete_pref(&self, url: &str) {
        let mut prefs = self.load_prefs();
        prefs.remove(url.trim());
        let _ = self.save_prefs(&prefs);
    }

    fn load_prefs(&self) -> HashMap<String, HashMap<String, String>> {
        let path = self.prefs_file();
        if !path.exists() {
            return HashMap::new();
        }
        fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save_prefs(&self, prefs: &HashMap<String, HashMap<String, String>>) -> Result<()> {
        let path = self.prefs_file();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(prefs).context("序列化监控偏好")?;
        crate::config::atomic_write(&path, text + "\n").context("写监控偏好")
    }
}

fn read_lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .ok()
        .map(|text| {
            text.lines()
                .map(|s| s.trim_start_matches('\u{feff}').trim_end().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn write_lines(path: &Path, lines: &[String]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    crate::config::atomic_write(path, lines.join("\n") + "\n")
        .with_context(|| format!("写监控列表失败: {}", path.display()))
}

fn normalize_url(url: &str) -> String {
    let u = url.trim();
    if u.contains("://") {
        u.to_string()
    } else {
        format!("https://{u}")
    }
}

fn parse_switch(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "是" | "yes" | "true" | "1" | "on" => Ok(true),
        "否" | "no" | "false" | "0" | "off" => Ok(false),
        other => bail!("开关值必须是 是/否: {other}"),
    }
}

fn split_comma(text: &str) -> Vec<String> {
    text.split(['、', '，', ','])
        .map(|s| s.trim().to_string())
        .collect()
}

fn parse_live_line(line: &str) -> LiveItem {
    let s = line.trim();
    let enabled = !s.starts_with('#');
    let clean = s.trim_start_matches('#').trim();
    let parts = split_comma(clean);
    let (quality, url, remark) = if parts.len() == 1 {
        (String::new(), parts[0].clone(), String::new())
    } else if contains_url(&parts[0]) {
        // 兼容旧格式：URL,备注[,画质]
        let remark = parts.get(1).cloned().unwrap_or_default();
        let quality = parts
            .get(2)
            .cloned()
            .filter(|v| is_quality(v))
            .unwrap_or_default();
        (quality, parts[0].clone(), remark)
    } else if contains_url(&parts[1]) {
        // 标准格式：画质,URL[,备注]
        let remark = parts.get(2).cloned().unwrap_or_default();
        (parts[0].clone(), parts[1].clone(), remark)
    } else if parts.len() >= 3 {
        // 兼容旧格式：URL,备注,画质（未加协议时按 URL 开头处理）
        let quality = parts
            .get(2)
            .cloned()
            .filter(|v| is_quality(v))
            .unwrap_or_default();
        (quality, parts[0].clone(), parts[1].clone())
    } else {
        (String::new(), parts[0].clone(), parts[1].clone())
    };
    let url = if url.is_empty() || url.contains("://") {
        url
    } else {
        format!("https://{url}")
    };
    LiveItem {
        url,
        remark,
        quality,
        enabled,
        notify: String::new(),
    }
}

fn parse_video_line(line: &str) -> VideoSource {
    let raw = line.trim();
    // 以 `#` 开头表示暂停监控：数据仍完整保留，只是 enabled=false。
    let enabled = !raw.starts_with('#');
    let s = raw.trim_start_matches('#').trim();
    // 配置格式为「地址,名称[,下载=是/否]」。地址是第一个分隔段；名称允许包含
    // 逗号，因此把剩余段里不是下载开关的部分合并回名称，避免“博主,老师”被切坏。
    let (url, rest) = split_first_video_field(s);
    let rest_parts = split_video_rest(rest);
    let mut name_parts = Vec::new();
    let mut download = None;
    for extra in rest_parts {
        if let Some(v) = extra.strip_prefix("下载=") {
            let v = v.trim().to_ascii_lowercase();
            download = Some(match v.as_str() {
                "是" | "yes" | "true" | "1" => true,
                "否" | "no" | "false" | "0" => false,
                // 未知值不静默当作“否”，保留 None 让上层跟随全局开关。
                _ => {
                    return VideoSource {
                        url: url.clone(),
                        name: if name_parts.is_empty() {
                            url.clone()
                        } else {
                            name_parts.join("，")
                        },
                        download: None,
                        notify: String::new(),
                        enabled,
                    };
                }
            });
        } else {
            name_parts.push(extra);
        }
    }
    let name = if name_parts.is_empty() {
        url.clone()
    } else {
        name_parts.join("，")
    };
    VideoSource {
        url,
        name,
        download,
        notify: String::new(),
        enabled,
    }
}

fn split_first_video_field(line: &str) -> (String, &str) {
    // 用字符边界而不是“字节下标 + 1”切片：中文全角逗号占 3 字节，
    // 之前删除含中文逗号的博主名时会在 UTF-8 中间切出 panic。
    match line.find([',', '，']) {
        Some(idx) => {
            let (head, tail) = line.split_at(idx);
            let rest = tail.get(1..).unwrap_or("");
            (head.trim().to_string(), rest)
        }
        None => (line.trim().to_string(), ""),
    }
}

fn split_video_rest(rest: &str) -> Vec<String> {
    rest.split([',', '，'])
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// 两个 URL 是否属于同一个监控源：忽略大小写和末尾斜杠。
fn same_url(a: &str, b: &str) -> bool {
    let a = a.trim().trim_end_matches('/');
    let b = b.trim().trim_end_matches('/');
    a.eq_ignore_ascii_case(b)
}

/// 视频源 URL 匹配：视频列表可能带查询参数/结尾斜杠差异，删除时统一按
/// “同源”判断，避免界面传回带尾斜杠或大小写不同的地址时误删失败。
fn same_video_url(a: &str, b: &str) -> bool {
    same_url(a, b)
}

fn format_live_line(url: &str, remark: &str, quality: &str) -> String {
    let q = match quality {
        "原画" | "蓝光" | "超清" | "高清" | "标清" | "流畅" => quality,
        _ => "",
    };
    if !q.is_empty() && !remark.is_empty() {
        format!("{q},{url},{remark}")
    } else if !q.is_empty() {
        format!("{q},{url}")
    } else if !remark.is_empty() {
        format!("{url},{remark}")
    } else {
        url.to_string()
    }
}

fn format_video_line(url: &str, name: &str, download: Option<bool>) -> String {
    match download {
        Some(true) => format!("{url},{name},下载=是"),
        Some(false) => format!("{url},{name},下载=否"),
        None => format!("{url},{name}"),
    }
}

/// 判断一段文本是否像一个监控地址。
///
/// 除了带协议的 `http(s)://...` 和 `www.` 前缀，还要识别**裸域名**——
/// 用户常写 `live.bilibili.com/123`、`space.bilibili.com/456`。旧实现只认
/// `://` 和 `www.`，导致 `原画,live.bilibili.com/123` 这种标准格式的行里，
/// 两个字段都被判为非 URL，最终把画质 `原画` 当成地址写进监控列表。
///
/// 判定规则（保守，避免把中文备注误判成域名）：
/// - 含 `://`，或
/// - 以 `www.` 开头，或
/// - 是「ASCII 域名 + 可选路径」形态：只含 ASCII 可见字符、不含空格/中文，
///   且 `.` 后紧跟 2 个以上字母的 TLD（如 `live.bilibili.com/1`）。
fn contains_url(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    if t.contains("://") || t.starts_with("www.") {
        return true;
    }
    looks_like_bare_domain(&t)
}

/// 裸域名判断：取 `,`/`/`/`?`/`#` 之前的主机部分，要求
/// 全部是 ASCII（域名只能是 ASCII），且形如 `label.tld`（TLD ≥2 位字母）。
fn looks_like_bare_domain(t: &str) -> bool {
    if t.is_empty() || t.contains(char::is_whitespace) {
        return false;
    }
    // 只取主机名部分，去掉路径/查询/锚点。
    let host = t
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('.');
    if host.is_empty() {
        return false;
    }
    // 逐字符校验：仅允许 ASCII 字母数字、`-`、`.`。
    // 重点是不把中文/emoji 等备注误判成域名。
    if !host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    {
        return false;
    }
    let labels: Vec<&str> = host.split('.').collect();
    // 至少 `label.tld` 两段，首段非空，末段是 ≥2 位纯字母的 TLD。
    if labels.len() < 2 {
        return false;
    }
    let first = labels[0];
    let tld = labels[labels.len() - 1];
    !first.is_empty() && tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic())
}

/// 判断一行是否是一个「真正的监控源」，而不是文件头的说明注释。
///
/// 说明文字（如 `# 以 # 开头的行会被暂停监控`、`# 支持三类直播监控地址...`）
/// 去掉 `#` 后并没有真实 URL，`parse_*_line` 会给它强行拼出 `https://说明文字` 这样的
/// 伪地址，污染监控列表。真正源（含暂停源）去掉 `#` 后，必有一个以 `http(s)://`
/// 或 `www.` 开头的字段（直播源画质在前、URL 在后；视频源 URL 在行首）。
fn is_source_line(line: &str) -> bool {
    let clean = line
        .trim_start_matches('\u{feff}')
        .trim()
        .trim_start_matches('#')
        .trim();
    clean
        .split(['、', '，', ','])
        .map(|p| p.trim().to_lowercase())
        .any(|p| p.starts_with("http://") || p.starts_with("https://") || p.starts_with("www."))
}

fn is_quality(text: &str) -> bool {
    matches!(
        text.trim(),
        "原画" | "蓝光" | "超清" | "高清" | "标清" | "流畅"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_project(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zunzhe-watch-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("config")).unwrap();
        dir
    }

    #[test]
    fn parses_live_lines() {
        let item = parse_live_line("原画,https://live.bilibili.com/1,主播甲");
        assert_eq!(item.url, "https://live.bilibili.com/1");
        assert_eq!(item.remark, "主播甲");
        assert_eq!(item.quality, "原画");
        let legacy = parse_live_line("https://live.bilibili.com/2,主播乙,高清");
        assert_eq!(legacy.url, "https://live.bilibili.com/2");
        assert_eq!(legacy.remark, "主播乙");
        assert_eq!(legacy.quality, "高清");
        assert!(item.enabled);
        assert!(!parse_live_line("#https://live.bilibili.com/2").enabled);
    }

    #[test]
    fn parses_bare_domain_live_lines() {
        // 裸域名（无协议、无 www.）：标准格式「画质,域名/路径」。
        // 旧实现会把「原画」误当成地址，这里锁定修复后的语义。
        let item = parse_live_line("原画,live.bilibili.com/123");
        assert_eq!(item.url, "https://live.bilibili.com/123");
        assert_eq!(item.quality, "原画");

        // 旧格式「域名/路径,备注」。
        let legacy = parse_live_line("live.bilibili.com/456,主播乙");
        assert_eq!(legacy.url, "https://live.bilibili.com/456");
        assert_eq!(legacy.remark, "主播乙");

        // 中文备注不能被误判成域名。
        assert!(!super::contains_url("主播甲"));
        assert!(!super::contains_url("原画"));
        // 纯数字/无点也不能当域名。
        assert!(!super::contains_url("123"));
    }

    #[test]
    fn contains_url_recognizes_domains_conservatively() {
        assert!(super::contains_url("https://live.bilibili.com/1"));
        assert!(super::contains_url("www.example.com"));
        assert!(super::contains_url("live.bilibili.com/1"));
        assert!(super::contains_url("space.bilibili.com"));
        // 中文、空格、无 TLD 的串都判否，避免污染监控列表。
        assert!(!super::contains_url("说明文字"));
        assert!(!super::contains_url("foo bar.com"));
        assert!(!super::contains_url("v1.2"));
    }

    #[test]
    fn live_anchor_view_comes_from_url_config_not_sqlite() {
        let dir = temp_project("anchor-view");
        fs::write(
            dir.join("config").join("URL_config.ini"),
            "https://live.bilibili.com/123,主播甲,原画\n",
        )
        .unwrap();
        let w = super::Watchlist::new(dir.clone());
        let view = w.live_anchor_view();
        assert_eq!(view.len(), 1);
        assert_eq!(view[0].url, "https://live.bilibili.com/123");
        assert_eq!(view[0].name, "主播甲");
        assert!(view[0].enabled);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_video_lines() {
        let item = parse_video_line("https://space.bilibili.com/1,博主,下载=否");
        assert_eq!(item.url, "https://space.bilibili.com/1");
        assert_eq!(item.name, "博主");
        assert_eq!(item.download, Some(false));
    }

    #[test]
    fn parses_video_lines_with_comma_in_name_and_unknown_download() {
        let comma = parse_video_line("https://space.bilibili.com/1,博主,老师,下载=是");
        assert_eq!(comma.url, "https://space.bilibili.com/1");
        assert_eq!(comma.name, "博主，老师");
        assert_eq!(comma.download, Some(true));

        let unknown = parse_video_line("https://space.bilibili.com/2,博主,下载=也许");
        assert_eq!(unknown.url, "https://space.bilibili.com/2");
        assert_eq!(unknown.name, "博主");
        assert_eq!(
            unknown.download, None,
            "未知下载值应跟随全局，不能静默当成否"
        );

        let fullwidth = parse_video_line("https://space.bilibili.com/3,博主，老师，下载=否");
        assert_eq!(fullwidth.url, "https://space.bilibili.com/3");
        assert_eq!(fullwidth.name, "博主，老师");
        assert_eq!(fullwidth.download, Some(false));
    }

    #[test]
    fn fullwidth_comma_split_never_panics_on_utf8_boundary() {
        // 回归：删除/解析含中文逗号的博主名时曾用字节下标 +1 切片，
        // 全角逗号占 3 字节导致 panic；现在必须安全切在字符边界。
        let item = parse_video_line("https://space.bilibili.com/1,测试,老师,下载=是");
        assert_eq!(item.url, "https://space.bilibili.com/1");
        assert_eq!(item.name, "测试，老师");
        assert_eq!(item.download, Some(true));

        let fullwidth = parse_video_line("https://space.bilibili.com/1,博主，老师，下载=否");
        assert_eq!(fullwidth.url, "https://space.bilibili.com/1");
        assert_eq!(fullwidth.name, "博主，老师");
        assert_eq!(fullwidth.download, Some(false));
    }

    #[test]
    fn remove_video_tolerates_trailing_slash_and_case() {
        let dir = temp_project("remove-video-tolerance");
        fs::write(
            dir.join("config").join("视频监控列表.ini"),
            "https://space.bilibili.com/1,测试,老师,下载=是\n",
        )
        .unwrap();
        let w = super::Watchlist::new(dir.clone());
        assert!(
            w.remove_video("HTTPS://SPACE.BILIBILI.COM/1/").unwrap(),
            "带尾斜杠/大小写的同源地址也应能删除"
        );
        assert_eq!(w.list_video().len(), 0, "删除后列表应为空");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_video_replaces_by_url_when_name_contains_comma() {
        let dir = temp_project("video-comma");
        let w = Watchlist::new(dir.clone());
        w.add_video(
            "https://space.bilibili.com/1",
            "博主,老师",
            Some(true),
            None,
        )
        .unwrap();
        w.add_video("https://space.bilibili.com/1", "新名字", Some(false), None)
            .unwrap();
        let items = w.list_video();
        assert_eq!(items.len(), 1, "同名地址应替换旧行而不是追加重复行");
        assert_eq!(items[0].name, "新名字");
        assert_eq!(items[0].download, Some(false));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn live_and_video_roundtrip() {
        let dir = temp_project("roundtrip");
        let w = Watchlist::new(dir.clone());
        w.add_live("live.bilibili.com/1", "主播甲", "原画").unwrap();
        assert_eq!(w.list_live().len(), 1);
        assert_eq!(w.list_live()[0].remark, "主播甲");
        assert!(w.remove_live("https://live.bilibili.com/1").unwrap());

        w.add_video(
            "https://space.bilibili.com/1",
            "博主",
            Some(true),
            Some("邮箱"),
        )
        .unwrap();
        let items = w.list_video();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].download, Some(true));
        assert_eq!(items[0].notify, "邮箱");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn notify_prefs_fallback_to_follow_global() {
        let dir = temp_project("notify");
        let w = Watchlist::new(dir.clone());
        assert_eq!(w.notify_for("https://x.test/1", ""), "跟随全局");
        w.set_notify("https://x.test/1", "微信", "博主").unwrap();
        assert_eq!(w.notify_for("https://x.test/1", ""), "微信");
        assert_eq!(w.notify_for("https://x.test/2", "博主"), "微信");
        w.set_notify("https://x.test/1", "跟随全局", "").unwrap();
        assert_eq!(w.notify_for("https://x.test/1", ""), "跟随全局");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_live_is_disable_not_delete() {
        let dir = temp_project("disable-live");
        let w = Watchlist::new(dir.clone());
        w.add_live("https://live.bilibili.com/1", "主播甲", "原画")
            .unwrap();
        assert_eq!(w.list_live().len(), 1, "启用状态应先出现在监控列表");
        assert!(w.list_live()[0].enabled, "新增源应处于启用状态");

        assert!(w.remove_live("https://live.bilibili.com/1").unwrap());
        // 暂停后源仍在列表里（enabled=false），供桌面端灰显“已暂停”，而不是直接消失。
        assert_eq!(
            w.list_live().len(),
            1,
            "remove-live 后源应保留在列表（enabled=false）"
        );
        assert!(!w.list_live()[0].enabled, "暂停的源 enabled 应为 false");

        let text = fs::read_to_string(w.url_config()).unwrap();
        assert!(
            text.lines().any(|line| {
                let s = line.trim();
                s.starts_with('#') && s.contains("https://live.bilibili.com/1")
            }),
            "remove-live 应保留为注释项，便于恢复: {text}"
        );

        // 恢复被暂停的主播，确认列表仍只有一条且重新启用。
        w.add_live("https://live.bilibili.com/1", "主播甲", "原画")
            .unwrap();
        assert_eq!(w.list_live().len(), 1);
        assert!(w.list_live()[0].enabled, "恢复后源应重新启用");
        let _ = fs::remove_dir_all(&dir);
    }
}
