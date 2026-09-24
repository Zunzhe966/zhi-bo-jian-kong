//! MCP 更新控制：`check_update`（只读扫更新源）+ `apply_update`（触发桌面端更新）。
//!
//! 与桌面端 `desktop/electron/update-run.cjs` 共享同一套规则：
//! - 更新源 = `<数据根>/releases/`（离线可用，不需要服务器）
//! - 三种投放形式都认：`尊者直播监控.app` 整个包 / `尊者直播监控-<版本>.zip` / 带版本号的文件夹
//! - 版本号唯一权威 = 包内 `Contents/Info.plist` 的 `CFBundleShortVersionString`
//!
//! 本模块只做两件事：**读**（扫描、比较版本，纯只读）和**发信号**（写
//! `.update-request.json`，由桌面端 Electron 轮询到后走它自己的
//! `applyRelease` 链路完成替换 .app + 重启，全程不碰数据目录）。
//! Rust daemon 自己不能替换正在运行的 App，因此安装动作永远交给桌面端。

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// 安装到 /Applications 的 App 名（与 update-run.cjs 一致）。
pub const APP_BUNDLE_NAME: &str = "尊者直播监控.app";
/// 更新请求信号文件名（Electron 轮询到这个文件就触发一次检查更新）。
pub const UPDATE_REQUEST_FILE: &str = ".update-request.json";
/// 安装路径（当前只支持 macOS /Applications）。
fn install_path() -> PathBuf {
    Path::new("/Applications").join(APP_BUNDLE_NAME)
}

/// releases 目录 = <数据根>/releases（与 update-run.cjs `releasesDir()` 同一位置）。
pub fn releases_dir(project_dir: &Path) -> PathBuf {
    project_dir.join("releases")
}

/// 解析版本号 "1.2.3" / "v1.2.3-beta.1" → 数字数组（非数字段忽略，缺位按 0 补）。
fn parse_version(v: &str) -> Vec<u32> {
    let t = v.trim().trim_start_matches(|c| c == 'v' || c == 'V');
    t.split(|c: char| c == '.' || c == '-' || c == '+' || c == '_')
        .filter_map(|s| s.parse::<u32>().ok())
        .collect()
}

/// 版本比较：a > b → Greater；缺位按 0 补齐。
pub fn compare_version(a: &str, b: &str) -> std::cmp::Ordering {
    let x = parse_version(a);
    let y = parse_version(b);
    let n = x.len().max(y.len());
    for i in 0..n {
        let xi = x.get(i).copied().unwrap_or(0);
        let yi = y.get(i).copied().unwrap_or(0);
        if xi != yi {
            return xi.cmp(&yi);
        }
    }
    std::cmp::Ordering::Equal
}

/// 读一个 .app 包的版本（Contents/Info.plist 的 CFBundleShortVersionString）；读不到返回空串。
fn read_bundle_version(app_path: &Path) -> String {
    let plist = app_path.join("Contents").join("Info.plist");
    if !plist.exists() {
        return String::new();
    }
    let out = std::process::Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", "Print :CFBundleShortVersionString", plist.to_str().unwrap_or("")])
        .output();
    match out {
        Ok(r) if r.status.success() => String::from_utf8_lossy(&r.stdout).trim().to_string(),
        _ => String::new(),
    }
}

/// 从 zip 里找 `.app/Contents/Info.plist` 条目，只解出该条目读版本号（不解整个包）。
fn zip_bundle_version(zip_path: &Path, tmp_dir: &Path) -> String {
    let l = std::process::Command::new("/usr/bin/unzip")
        .args(["-Z1", zip_path.to_str().unwrap_or("")])
        .output();
    let Ok(l) = l else {
        return String::new();
    };
    if !l.status.success() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&l.stdout);
    let entry = text
        .lines()
        .map(|s| s.trim().trim_end_matches('/'))
        .find(|s| s.ends_with(".app/Contents/Info.plist"));
    let Some(entry) = entry else {
        return String::new();
    };
    let e = std::process::Command::new("/usr/bin/unzip")
        .args([
            "-o",
            zip_path.to_str().unwrap_or(""),
            entry,
            "-d",
            tmp_dir.to_str().unwrap_or(""),
        ])
        .output();
    let Ok(e) = e else {
        return String::new();
    };
    if !e.status.success() {
        return String::new();
    }
    read_bundle_version(&tmp_dir.join(entry))
}

/// 更新源条目。
#[derive(Debug, Clone)]
pub struct ReleaseEntry {
    pub version: String,
    pub kind: &'static str,
    pub path: PathBuf,
    pub name: String,
}

/// 在目录里找第一个 .app（只认带 Contents/Info.plist 的），最多下钻 3 层。
fn find_app_in(dir: &Path, depth: usize) -> Option<PathBuf> {
    if depth > 3 {
        return None;
    }
    let Ok(names) = std::fs::read_dir(dir) else {
        return None;
    };
    let mut dirs = Vec::new();
    for n in names.flatten() {
        let full = n.path();
        let fname = n.file_name().to_string_lossy().to_string();
        if fname.starts_with('.') {
            continue;
        }
        if full.is_dir() {
            if fname.ends_with(".app") && full.join("Contents").join("Info.plist").exists() {
                return Some(full);
            }
            dirs.push(full);
        }
    }
    for d in dirs {
        if let Some(hit) = find_app_in(&d, depth + 1) {
            return Some(hit);
        }
    }
    None
}

/// 扫描 releases/ 目录里所有可识别版本条目（.app / .zip / 版本文件夹）。
pub fn list_releases(releases_dir: &Path, tmp_dir: &Path) -> Vec<ReleaseEntry> {
    let mut out = Vec::new();
    let Ok(names) = std::fs::read_dir(releases_dir) else {
        return out;
    };
    let mut entries: Vec<_> = names
        .flatten()
        .map(|e| (e.file_name().to_string_lossy().to_string(), e.path()))
        .collect();
    entries.sort_by(|a, b| a.0.locale_compare(&b.0));
    for (name, full) in entries {
        if name.starts_with('.') {
            continue;
        }
        if full.is_dir() && name.ends_with(".app") {
            let version = read_bundle_version(&full);
            if !version.is_empty() {
                out.push(ReleaseEntry { version, kind: "app", path: full, name });
            }
        } else if !full.is_dir() && name.to_lowercase().ends_with(".zip") {
            let mut version = zip_bundle_version(&full, tmp_dir);
            // 兜底：zip 解不出 Info.plist 就退回文件名里的版本号（尊者直播监控-1.2.0.zip）
            if version.is_empty() {
                if let Some(m) = name
                    .split('-')
                    .nth(1)
                    .and_then(|s| s.strip_suffix(".zip"))
                    .map(|s| s.to_string())
                {
                    if m.split('.').all(|p| p.parse::<u32>().is_ok()) {
                        version = m;
                    }
                }
            }
            if !version.is_empty() {
                out.push(ReleaseEntry { version, kind: "zip", path: full, name });
            }
        } else if full.is_dir() {
            // 版本文件夹：下钻找 .app
            if let Some(inner) = find_app_in(&full, 0) {
                let version = read_bundle_version(&inner);
                if !version.is_empty() {
                    let inner_name = inner.file_name().unwrap_or_default().to_string_lossy().to_string();
                    out.push(ReleaseEntry {
                        version,
                        kind: "app",
                        path: inner,
                        name: format!("{name}/{inner_name}"),
                    });
                }
            }
        }
    }
    out
}

/// 当前已装 App 版本（/Applications/尊者直播监控.app）；未安装返回空串。
pub fn current_app_version() -> String {
    read_bundle_version(&install_path())
}

/// 排序用的中文本地化比较（与 update-run.cjs `localeCompare('zh-Hans-CN')` 对齐）。
trait LocaleCompare {
    fn locale_compare(&self, other: &Self) -> std::cmp::Ordering;
}
impl LocaleCompare for String {
    fn locale_compare(&self, other: &Self) -> std::cmp::Ordering {
        // Rust 无内置 zh 排序，这里只要求「稳定、可复现」；与 JS 的 zh-Hans-CN 排序
        // 可能有细微差异，但只影响返回顺序，不影响「最新版本」的判定。
        self.cmp(other)
    }
}

/// `check_update`：只读扫描更新源，返回当前版本 / 最新版本 / 是否有更新。
pub fn check_update(project_dir: &Path) -> Value {
    let rd = releases_dir(project_dir);
    let current = current_app_version();
    let tmp = std::env::temp_dir().join(format!(
        "zunzhe-check-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let all = list_releases(&rd, &tmp);
    let _ = std::fs::remove_dir_all(&tmp);
    let usable: Vec<&ReleaseEntry> = all.iter().filter(|r| !r.version.is_empty()).collect();
    if usable.is_empty() {
        let why = if all.is_empty() {
            "releases 文件夹是空的"
        } else {
            "releases 里有条目但读不出版本号（zip 里要能找到 .app）"
        };
        return json!({
            "ok": true,
            "current_version": current,
            "has_update": false,
            "latest_version": Value::Null,
            "latest_name": Value::Null,
            "releases_dir": rd.display().to_string(),
            "note": why,
        });
    }
    let latest = usable
        .iter()
        .max_by(|a, b| compare_version(&a.version, &b.version))
        .unwrap();
    json!({
        "ok": true,
        "current_version": current,
        "has_update": compare_version(&latest.version, &current) == std::cmp::Ordering::Greater,
        "latest_version": latest.version,
        "latest_name": latest.name,
        "releases_dir": rd.display().to_string(),
    })
}

/// `apply_update`：写更新请求信号文件，桌面端 Electron 轮询到后走它的更新链路。
pub fn request_update(project_dir: &Path) -> Result<Value, anyhow::Error> {
    let rd = releases_dir(project_dir);
    std::fs::create_dir_all(&rd)?;
    let req_path = rd.join(UPDATE_REQUEST_FILE);
    let body = json!({
        "ts": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        "requested_by": "mcp",
        "current_version": current_app_version(),
    });
    std::fs::write(&req_path, serde_json::to_string_pretty(&body)?)?;
    Ok(json!({
        "ok": true,
        "requested": true,
        "signal": req_path.display().to_string(),
        "hint": "已通知桌面端。Electron 收到信号后会检查更新源：有更高版本则弹确认框，确认后替换 .app 并重启（数据目录完全不动）",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parse_and_compare() {
        assert_eq!(parse_version("1.2.3"), vec![1u32, 2, 3]);
        assert_eq!(parse_version("v1.2.3-beta.1"), vec![1u32, 2, 3, 1]);
        assert_eq!(parse_version("1.2.3+build.7"), vec![1u32, 2, 3, 7]);
        assert_eq!(parse_version(""), Vec::<u32>::new());
        assert_eq!(compare_version("1.2.3", "1.2.3"), std::cmp::Ordering::Equal);
        assert_eq!(compare_version("1.4.0", "1.5.8"), std::cmp::Ordering::Less);
        assert_eq!(compare_version("1.5.8", "1.5.7"), std::cmp::Ordering::Greater);
        // 缺位按 0 补齐
        assert_eq!(compare_version("1.5", "1.5.8"), std::cmp::Ordering::Less);
        assert_eq!(compare_version("1.5.9", "1.5.8"), std::cmp::Ordering::Greater);
    }

    #[test]
    fn scan_releases_recognizes_zip_filename_fallback() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-updater-scan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 假 zip：unzip 读不出 Info.plist → 走文件名兜底
        std::fs::write(dir.join("尊者直播监控-9.9.9.zip"), b"not a real zip").unwrap();
        std::fs::write(dir.join(".DS_Store"), b"x").unwrap();
        let all = list_releases(&dir, &dir.join("tmp"));
        assert_eq!(all.len(), 1, "只应识别出可读版本号的 zip");
        assert_eq!(all[0].version, "9.9.9");
        assert_eq!(all[0].kind, "zip");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_update_detects_higher_release_regardless_of_installed_version() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-updater-check-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("releases")).unwrap();
        // 用不可能被已装版本追上的假版本号：无论本机 /Applications 装了什么，都应有更新。
        std::fs::write(
            dir.join("releases").join("尊者直播监控-999.9.9.zip"),
            b"not a real zip",
        )
        .unwrap();
        let v = check_update(&dir);
        assert_eq!(v["ok"], true);
        assert_eq!(v["latest_version"], "999.9.9");
        assert_eq!(v["has_update"], true, "999.9.9 必须高于任何已装版本: {v}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
