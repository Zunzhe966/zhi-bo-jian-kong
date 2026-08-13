use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 桌面端「B站授权」导出的浏览器 Cookie 固定相对路径。
pub fn browser_cookie_path(project_dir: &Path) -> PathBuf {
    project_dir
        .join("fupan_output")
        .join("run")
        .join("bilibili_browser_cookies.txt")
}

/// 用户粘贴在 `[视频监控] b站cookie` 里的 Cookie 生成的 Netscape 文件路径。
///
/// 与旧 Python `fupan/video_monitor.py::_bilibili_user_cookies_file()` 保持同一路径，
/// 让桌面端/CLI 和旧 Python 在混用期间共享同一份缓存，避免两边生成不同文件。
pub fn user_cookie_path(project_dir: &Path) -> PathBuf {
    project_dir
        .join("fupan_output")
        .join("run")
        .join("bilibili_user_cookies.txt")
}

/// 把 Netscape 格式的 Cookie 文件转成 `name=value; ...` 请求头字符串。
///
/// 桌面端 Electron 导出的文件可能是注释 + 以 tab 分隔的 7 列；这里只保留
/// 非空的持久 Cookie，兼容同一项目里多处复用的解析逻辑。
pub fn cookie_header_from_project(project_dir: &Path) -> String {
    let path = browser_cookie_path(project_dir);
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    cookie_header_from_netscape(&text)
}

/// B站匿名设备 Cookie 文件（buvid3/buvid4）固定相对路径。
///
/// 这个文件不是账号登录态，仅用于给 yt-dlp 一个匿名设备标识，
/// 降低无 Cookie 请求触发 412 的概率。
pub fn anon_cookie_path(project_dir: &Path) -> PathBuf {
    project_dir
        .join("fupan_output")
        .join("run")
        .join("bilibili_anon_cookies.txt")
}

/// 从 B站 finger/spi 返回的数据生成 Netscape Cookie 文本。
///
/// `b_3`/`b_4` 是公开接口返回的匿名设备 ID，不包含账号凭据。
pub fn build_anon_cookie_netscape(b_3: &str, b_4: &str, expire: i64) -> String {
    let mut lines = vec!["# Netscape HTTP Cookie File".to_string()];
    for (key, value) in [("buvid3", b_3), ("buvid4", b_4)] {
        if value.is_empty() {
            continue;
        }
        lines.push(format!(
            ".bilibili.com\tTRUE\t/\tFALSE\t{expire}\t{key}\t{value}"
        ));
    }
    lines.push(String::new());
    lines.join("\n")
}

/// 把浏览器 `name=value; name2=value2` Cookie 字符串转成 Netscape 文本。
///
/// 旧 Python 用 `.bilibili.com`、路径 `/`、`secure=false`，expire 设置为未来
/// 半年；这里保持同一形状，供 `yt-dlp --cookies` 与 B站接口 Cookie 使用。
pub fn build_user_cookie_netscape(cookie: &str, expire: i64) -> String {
    let mut lines = vec!["# Netscape HTTP Cookie File".to_string()];
    for pair in cookie.split(';') {
        let pair = pair.trim();
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            continue;
        }
        lines.push(format!(
            ".bilibili.com\tTRUE\t/\tFALSE\t{expire}\t{key}\t{value}"
        ));
    }
    lines.push(String::new());
    lines.join("\n")
}

/// 若用户配置了 `b站cookie`，把它写成旧 Python 同路径的 Netscape Cookie 文件。
///
/// 返回 `Some(path)` 仅表示写文件成功；调用方仍需按优先级决定是否使用它。
pub fn ensure_user_cookie_file(project_dir: &Path, cookie: &str) -> Option<PathBuf> {
    let cookie = cookie.trim();
    if cookie.is_empty() {
        return None;
    }
    let path = user_cookie_path(project_dir);
    let expire = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64 + 86400 * 180)
        .unwrap_or(86400 * 180);
    let text = build_user_cookie_netscape(cookie, expire);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    crate::config::atomic_write(&path, text).ok()?;
    Some(path)
}

/// 从 B站匿名设备接口获取 buvid3/buvid4，并缓存为 Netscape Cookie 文件。
///
/// 失败返回 None，调用方应继续使用已有浏览器 Cookie 或直接匿名下载。
pub async fn ensure_anon_cookie(project_dir: &Path) -> Option<PathBuf> {
    let path = anon_cookie_path(project_dir);
    if path.exists() {
        return Some(path);
    }

    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)")
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .ok()?;
    let value: serde_json::Value = client
        .get("https://api.bilibili.com/x/frontend/finger/spi")
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let b_3 = value["data"]["b_3"].as_str().unwrap_or("").to_string();
    let b_4 = value["data"]["b_4"].as_str().unwrap_or("").to_string();
    if b_3.is_empty() && b_4.is_empty() {
        return None;
    }

    let expire = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64 + 86400 * 180)
        .unwrap_or(86400 * 180);
    let text = build_anon_cookie_netscape(&b_3, &b_4, expire);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if crate::config::atomic_write(&path, text).is_ok() {
        Some(path)
    } else {
        None
    }
}

/// 解析 Netscape Cookie 文本，忽略空行和注释。列数为 7 且名称不为 `0` 时保留。
fn cookie_header_from_netscape(text: &str) -> String {
    cookie_header_from_netscape_public(text)
}

/// 公开解析入口，供 `platform` 读取匿名 Cookie 后转成请求头。
pub fn cookie_header_from_netscape_public(text: &str) -> String {
    let mut pairs = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() >= 7 && fields[5] != "0" {
            pairs.push(format!("{}={}", fields[5], fields[6]));
        }
    }
    pairs.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_netscape_cookie_lines() {
        let text = "\
# Netscape HTTP Cookie File
.bilibili.com\tTRUE\t/\tFALSE\t0\tSESSDATA\tabc123
.bilibili.com\tTRUE\t/\tFALSE\t0\t0\tignored
";
        let header = cookie_header_from_netscape(text);
        assert_eq!(header, "SESSDATA=abc123");
    }

    #[test]
    fn missing_file_returns_empty() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-cookie-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert_eq!(cookie_header_from_project(&dir), "");
    }

    #[test]
    fn builds_anon_cookie_netscape_without_login_credentials() {
        let text = build_anon_cookie_netscape("b3-token", "b4-token", 12345);
        assert!(text.starts_with("# Netscape HTTP Cookie File"));
        assert!(text.contains(".bilibili.com\tTRUE\t/\tFALSE\t12345\tbuvid3\tb3-token"));
        assert!(text.contains(".bilibili.com\tTRUE\t/\tFALSE\t12345\tbuvid4\tb4-token"));
        assert!(!text.contains("SESSDATA="));
    }

    #[test]
    fn builds_anon_cookie_ignores_empty_values() {
        let text = build_anon_cookie_netscape("", "b4-token", 1);
        assert!(!text.contains("buvid3"));
        assert!(text.contains("buvid4\tb4-token"));
    }

    #[test]
    fn builds_user_cookie_netscape_like_python_fallback() {
        let text = build_user_cookie_netscape("SESSDATA=abc; bili_jct=xyz; ; empty=", 1800000);
        assert!(text.starts_with("# Netscape HTTP Cookie File"));
        assert!(text.contains(".bilibili.com\tTRUE\t/\tFALSE\t1800000\tSESSDATA\tabc"));
        assert!(text.contains(".bilibili.com\tTRUE\t/\tFALSE\t1800000\tbili_jct\txyz"));
        assert!(!text.contains("empty="));
    }
}
