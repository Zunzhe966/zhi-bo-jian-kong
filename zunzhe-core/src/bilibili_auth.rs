use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;

/// 桌面端「B站授权」导出的浏览器 Cookie 固定相对路径。
pub fn browser_cookie_path(project_dir: &Path) -> PathBuf {
    crate::paths::run_dir(project_dir).join("bilibili_browser_cookies.txt")
}

/// 用户粘贴在 `[视频监控] b站cookie` 里的 Cookie 生成的 Netscape 文件路径。
///
/// 与旧 Python `fupan/video_monitor.py::_bilibili_user_cookies_file()` 保持同一路径，
/// 让桌面端/CLI 和旧 Python 在混用期间共享同一份缓存，避免两边生成不同文件。
pub fn user_cookie_path(project_dir: &Path) -> PathBuf {
    crate::paths::run_dir(project_dir).join("bilibili_user_cookies.txt")
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

/// 同步读取桌面端导出的 Netscape Cookie 文件。
///
/// `cookie_header_from_project` 为了兼容旧调用直接吞掉 IO 错误，导致“文件存在但
/// 打不开/被截断”与“从未授权”都返回空串。这里返回可区分的 `Result`，供新调用
/// 在失败时报告真实原因，避免把权限问题伪装成“没有 Cookie”。
pub fn read_browser_cookie_file(project_dir: &Path) -> anyhow::Result<Option<String>> {
    let path = browser_cookie_path(project_dir);
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("读取 B站 Cookie 文件失败: {}", path.display()))?;
    Ok(Some(text))
}

/// B站匿名设备 Cookie 文件（buvid3/buvid4）固定相对路径。
///
/// 这个文件不是账号登录态，仅用于给 yt-dlp 一个匿名设备标识，
/// 降低无 Cookie 请求触发 412 的概率。
pub fn anon_cookie_path(project_dir: &Path) -> PathBuf {
    crate::paths::run_dir(project_dir).join("bilibili_anon_cookies.txt")
}

/// 可注入 Electron 会话的 B站 Cookie 文件，按优先级排序：浏览器授权 > 用户配置。
///
/// Rust 会把它们通过 `FUPAN_COOKIE_FILE` / `FUPAN_USER_COOKIE_FILE` 传给独立的
/// Electron 进程，让页面脚本/直链提取在真实 Chromium 会话里带上已授权登录态，
/// 而不是让每次独立启动的 Electron 用空的临时 userData 去访问 B站。
pub fn electron_cookie_files(project_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for path in [
        browser_cookie_path(project_dir),
        user_cookie_path(project_dir),
    ] {
        if path.is_file() {
            files.push(path);
        }
    }
    files
}

/// 从 B站 finger/spi 返回的数据生成 Netscape Cookie 文本。
///
/// `b_3`/`b_4` 是公开接口返回的匿名设备 ID，不包含账号凭据。
///
/// 24× 实测：只带 `buvid3` 请求 `space/wbi/arc/search` 会返回
/// `-352 风控校验失败`；补齐 `buvid3 + buvid4 + b_lsid` 后风控校验通过，
/// 错误收敛为 `-403 访问权限不足`（即仅差登录态）。因此这里一并写入
/// `b_lsid`（本地生成的设备会话标识，非账号凭据），让匿名请求尽可能
/// 通过风控层，错误提示才能准确区分“风控”与“需登录”。
pub fn build_anon_cookie_netscape(b_3: &str, b_4: &str, expire: i64) -> String {
    let b_lsid = generate_b_lsid();
    let mut lines = vec!["# Netscape HTTP Cookie File".to_string()];
    for (key, value) in [
        ("buvid3", b_3),
        ("buvid4", b_4),
        ("b_lsid", b_lsid.as_str()),
    ] {
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

/// 生成 B站 `b_lsid` 形态的匿名设备会话标识。
///
/// 官方形态为「16 位十六进制大写 + 下划线 + 13 位毫秒时间戳」，不含账号信息。
/// 单一来源的随机/时间戳，无需引入额外随机数依赖。
fn generate_b_lsid() -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    // 用时间戳叠加进程号/纳秒做哈希，得到稳定的十六进制片段。
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let mut state =
        now_ms ^ (nanos << 17) ^ (std::process::id() as u64).wrapping_mul(0x9E3779B97F4A7C15);
    let mut hex = String::with_capacity(16);
    for _ in 0..16 {
        // xorshift64*，避免依赖外部 rand。
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let nibble = (state >> 60) & 0xF;
        hex.push(
            char::from_digit(nibble as u32, 16)
                .unwrap_or('0')
                .to_ascii_uppercase(),
        );
    }
    format!("{hex}_{now_ms}")
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

    let client = crate::net::client_builder()
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
        // 实测：补齐 b_lsid 才能通过风控校验（-352 → -403），因此匿名 cookie
        // 必须同时带上 b_lsid，且它不含任何账号凭据。
        assert!(
            text.contains("\tb_lsid\t"),
            "匿名 cookie 必须含 b_lsid：{text}"
        );
        assert!(!text.contains("SESSDATA="));
    }

    #[test]
    fn generated_b_lsid_has_official_shape_and_no_credentials() {
        let lsid = generate_b_lsid();
        // 官方形态：16 位十六进制大写 + 下划线 + 时间戳。
        let (hex, ts) = lsid.split_once('_').expect("b_lsid 应含下划线分隔");
        assert_eq!(hex.len(), 16, "十六进制段应为 16 位：{lsid}");
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()),
            "应为大写十六进制：{lsid}"
        );
        assert!(
            ts.chars().all(|c| c.is_ascii_digit()),
            "时间戳段应为数字：{lsid}"
        );
        // 两次生成应不同（含时间/随机成分），且都不含登录凭据字面量。
        let other = generate_b_lsid();
        assert!(!lsid.contains("SESSDATA"));
        assert!(!other.contains("SESSDATA"));
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

    #[test]
    fn electron_cookie_files_only_include_existing_files() {
        let dir = crate::test_support::isolated_project("electron-cookies");
        std::fs::create_dir_all(crate::paths::run_dir(&dir)).unwrap();
        assert!(electron_cookie_files(&dir).is_empty());

        let browser = browser_cookie_path(&dir);
        std::fs::write(&browser, "# Netscape HTTP Cookie File\n").unwrap();
        let files = electron_cookie_files(&dir);
        assert_eq!(files, vec![browser.clone()]);

        let user = user_cookie_path(&dir);
        std::fs::write(&user, "# Netscape HTTP Cookie File\n").unwrap();
        let files = electron_cookie_files(&dir);
        assert_eq!(files, vec![browser, user]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
