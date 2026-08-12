use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

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

    pub fn list_live(&self) -> Vec<LiveItem> {
        read_lines(&self.url_config())
            .into_iter()
            .filter(|line| {
                let s = line.trim_start_matches('\u{feff}').trim();
                !s.is_empty() && !s.starts_with('#')
            })
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
            .filter(|old| {
                parse_live_line(old).url.trim_end_matches('/') != url.trim_end_matches('/')
            })
            .collect();
        if after.len() == original_len {
            return Ok(false);
        }
        write_lines(&self.url_config(), &after)?;
        self.delete_pref(url);
        Ok(true)
    }

    pub fn list_video(&self) -> Vec<VideoSource> {
        read_lines(&self.video_config())
            .into_iter()
            .filter(|line| {
                let s = line.trim_start_matches('\u{feff}').trim();
                !s.is_empty() && !s.starts_with('#')
            })
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
            let old_url = old.trim().split(',').next().unwrap_or("").trim();
            !old_url.is_empty() && old_url == url.trim()
        });
        if replaced {
            lines.iter_mut().for_each(|old| {
                let old_url = old.trim().split(',').next().unwrap_or("").trim();
                if old_url == url.trim() {
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
            .filter(|old| old.trim().split(',').next().unwrap_or("").trim() != url.trim())
            .cloned()
            .collect();
        if after.len() == before.len() {
            return Ok(false);
        }
        write_lines(&self.video_config(), &after)?;
        self.delete_pref(url);
        Ok(true)
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
            "微信" | "邮箱" | "不通知" => {
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
        fs::write(&path, text + "\n").context("写监控偏好")
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
    fs::write(path, lines.join("\n") + "\n")
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
    let s = line.trim();
    let parts: Vec<String> = s.split(',').map(|p| p.trim().to_string()).collect();
    let url = parts.first().cloned().unwrap_or_default();
    let name = parts.get(1).cloned().unwrap_or_else(|| url.clone());
    let mut download = None;
    for extra in parts.iter().skip(2) {
        if let Some(v) = extra.strip_prefix("下载=") {
            download = Some(
                v.trim().eq_ignore_ascii_case("是")
                    || v.trim() == "yes"
                    || v.trim() == "true"
                    || v.trim() == "1",
            );
        }
    }
    VideoSource {
        url,
        name,
        download,
        notify: String::new(),
    }
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

fn contains_url(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    t.contains("://") || t.starts_with("www.")
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
    fn parses_video_lines() {
        let item = parse_video_line("https://space.bilibili.com/1,博主,下载=否");
        assert_eq!(item.url, "https://space.bilibili.com/1");
        assert_eq!(item.name, "博主");
        assert_eq!(item.download, Some(false));
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
}
