use std::path::{Path, PathBuf};

/// 桌面端「B站授权」导出的浏览器 Cookie 固定相对路径。
pub fn browser_cookie_path(project_dir: &Path) -> PathBuf {
    project_dir
        .join("fupan_output")
        .join("run")
        .join("bilibili_browser_cookies.txt")
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

/// 解析 Netscape Cookie 文本，忽略空行和注释。列数为 7 且名称不为 `0` 时保留。
fn cookie_header_from_netscape(text: &str) -> String {
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
}
