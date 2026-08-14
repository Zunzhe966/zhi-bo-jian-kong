use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use serde::Serialize;

/// 简单 INI 解析：保留节与键原名，兼容 Python configparser 的常见写法。
#[derive(Debug, Clone, Default)]
pub struct IniFile {
    /// section -> (key -> value)
    data: HashMap<String, HashMap<String, String>>,
}

impl IniFile {
    pub fn parse(text: &str) -> Result<Self> {
        let mut data: HashMap<String, HashMap<String, String>> = HashMap::new();
        let mut current = String::new();
        for raw in text.lines() {
            // 真实 config/config.ini 文件开头可能带 UTF-8 BOM（Windows 编辑器/旧版
            // Python 保存结果），Python configparser 会按编码 BOM 自动忽略它。
            // 如果不剥掉，第一行 `[录制设置]` 会被当成普通键，整段录制配置被漏读。
            let line = raw.trim_start_matches('\u{feff}').trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                current = name.trim().to_string();
                data.entry(current.clone()).or_default();
                continue;
            }
            if current.is_empty() {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                data.entry(current.clone())
                    .or_default()
                    .insert(k.trim().to_string(), v.trim().to_string());
            }
        }
        Ok(Self { data })
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("读取配置文件失败: {}", path.display()))?;
        Self::parse(&text)
    }

    pub fn get(&self, section: &str, key: &str) -> Option<String> {
        self.data
            .get(section)
            .and_then(|m| m.get(key))
            .map(|s| s.trim().to_string())
    }

    pub fn section(&self, section: &str) -> Option<&HashMap<String, String>> {
        self.data.get(section)
    }

    #[allow(dead_code)] // 公共配置查询 API，供 MCP config_get 使用
    pub fn keys(&self, section: &str) -> Vec<String> {
        self.data
            .get(section)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn sections(&self) -> Vec<String> {
        let mut out: Vec<String> = self.data.keys().cloned().collect();
        out.sort();
        out
    }

    pub fn is_yes(value: &str) -> bool {
        matches!(
            value.trim().to_lowercase().as_str(),
            "是" | "yes" | "true" | "1" | "on"
        )
    }
}

/// 项目运行所需配置：主 config.ini + fupan_config.ini。
#[derive(Debug, Clone)]
pub struct Config {
    pub project_dir: PathBuf,
    #[allow(dead_code)] // 记录来源路径，供诊断
    pub config_ini: PathBuf,
    #[allow(dead_code)] // 记录来源路径，供诊断
    pub fupan_config_ini: PathBuf,
    pub main: IniFile,
    pub fupan: IniFile,
}

impl Config {
    pub fn load(project_dir: &Path) -> Result<Self> {
        let project_dir = find_project_root(project_dir)?;
        let config_ini = project_dir.join("config").join("config.ini");
        let fupan_config_ini = project_dir.join("config").join("fupan_config.ini");
        let main = if config_ini.exists() {
            IniFile::load(&config_ini)?
        } else {
            IniFile::load(&project_dir.join("config").join("config.ini.example"))?
        };
        let fupan = if fupan_config_ini.exists() {
            IniFile::load(&fupan_config_ini)?
        } else {
            IniFile::load(&project_dir.join("config").join("fupan_config.ini.example"))?
        };
        Ok(Self {
            project_dir: project_dir.to_path_buf(),
            config_ini,
            fupan_config_ini,
            main,
            fupan,
        })
    }

    pub fn get_main(&self, section: &str, key: &str) -> String {
        IniFile::get_case_insensitive(&self.main, section, key)
    }

    pub fn get_fupan(&self, section: &str, key: &str) -> String {
        IniFile::get_case_insensitive(&self.fupan, section, key)
    }

    pub fn is_yes_fupan(&self, section: &str, key: &str, default: bool) -> bool {
        let v = self.get_fupan(section, key);
        if v.is_empty() {
            default
        } else {
            IniFile::is_yes(&v)
        }
    }

    pub fn is_yes_main(&self, section: &str, key: &str, default: bool) -> bool {
        let v = self.get_main(section, key);
        if v.is_empty() {
            default
        } else {
            IniFile::is_yes(&v)
        }
    }

    pub fn is_yes(&self, section: &str, key: &str, default: bool) -> bool {
        if section == "录制设置" {
            self.is_yes_main(section, key, default)
        } else {
            self.is_yes_fupan(section, key, default)
        }
    }

    #[allow(dead_code)] // 快速判断是否已配置，供界面使用
    pub fn email_configured(&self) -> bool {
        let host = self.get_main("推送配置", "SMTP邮件服务器");
        let login = self.get_main("推送配置", "邮箱登录账号");
        let auth = self.get_main("推送配置", "发件人密码(授权码)");
        let to = self.get_main("推送配置", "收件人邮箱");
        !host.is_empty() && !login.is_empty() && !auth.is_empty() && !to.is_empty()
    }

    pub fn email_status(&self) -> EmailStatus {
        let cfg = self.email_settings();
        let status = EmailStatus::from_settings(cfg.clone());
        // 只列出真正缺失的项；host 有默认值，不要把“SMTP服务器”误报为缺失。
        let missing: Vec<String> = ["邮箱登录账号", "发件人密码(授权码)", "收件人邮箱"]
            .iter()
            .filter(|name| {
                let value = match **name {
                    "邮箱登录账号" => &cfg.login,
                    "发件人密码(授权码)" => &cfg.auth,
                    _ => &cfg.to,
                };
                value.trim().is_empty()
            })
            .map(|name| (*name).to_string())
            .collect();
        // 只有 host/登录账号/授权码/收件人四项齐全才算“已配置”；
        // 单独给一条可操作 hint，避免 `configured=false` 时只看到“缺少授权码”，
        // 而实际缺的是服务器/账号/收件人。
        if !status.configured {
            let hint = if missing.len() == 1 && missing[0] == "发件人密码(授权码)" {
                "缺少 SMTP 授权码：请在 QQ 邮箱设置-账户-开启 SMTP 后生成授权码，并在桌面端「全局设置」填写".to_string()
            } else {
                format!(
                    "邮箱未配置，缺少: {}. 请在桌面端「全局设置」填写 QQ 邮箱账号、授权码与收件邮箱（服务器默认 smtp.qq.com，端口 465，SSL 开启）",
                    missing.join("、")
                )
            };
            EmailStatus {
                missing,
                hint,
                ..status
            }
        } else {
            status
        }
    }

    pub fn email_settings(&self) -> EmailSettings {
        EmailSettings {
            host: self.get_main("推送配置", "SMTP邮件服务器"),
            port: self.get_main("推送配置", "SMTP邮件服务器端口"),
            ssl: IniFile::is_yes(&self.get_main("推送配置", "是否使用SMTP服务SSL加密(是/否)")),
            login: self.get_main("推送配置", "邮箱登录账号"),
            auth: self.get_main("推送配置", "发件人密码(授权码)"),
            sender: self.get_main("推送配置", "发件人邮箱"),
            sender_name: self.get_main("推送配置", "发件人显示昵称"),
            to: self.get_main("推送配置", "收件人邮箱"),
        }
    }

    pub fn push_channels(&self) -> PushChannels {
        // 旧 Python 文档里的 `pushplus_token` 与当前真实配置文件里的
        // `pushplus推送token` 可能混用；读取时优先真实键，缺失再兼容旧键，
        // 避免用户在旧文档/配置里填入 token 后通知通道被静默漏掉。
        let pushplus = self.get_main("推送配置", "pushplus推送token");
        let pushplus = if pushplus.is_empty() {
            self.get_main("推送配置", "pushplus_token")
        } else {
            pushplus
        };
        PushChannels {
            serverchan: clean_push_value(&self.get_main("推送配置", "server酱SendKey")),
            pushplus: clean_push_value(&pushplus),
            bark: clean_push_value(&self.get_main("推送配置", "bark推送接口链接")),
            ntfy: clean_push_value(&self.get_main("推送配置", "ntfy推送地址")),
        }
    }
}

impl IniFile {
    /// 按 section 和 key 大小写不敏感地读取值。
    ///
    /// 主配置 `config.ini` 与旧 Python 使用 `RawConfigParser(optionxform=str)`，
    /// 因此同一个键可能以 `SMTP` 或 `smtp` 混用；Rust 若严格匹配会漏读邮箱、
    /// SSL、推送通道等关键项。这里按现存键做一次大小写折叠匹配。
    fn get_case_insensitive(&self, section: &str, key: &str) -> String {
        if let Some(map) = self.data.get(section) {
            if let Some(value) = map.get(key) {
                return value.trim().to_string();
            }
            let needle = key.to_lowercase();
            for (existing, value) in map {
                if existing.to_lowercase() == needle {
                    return value.trim().to_string();
                }
            }
        }
        String::new()
    }
}

#[cfg(test)]
impl Config {
    /// 测试用最小配置：避免单测依赖真实项目配置文件。
    pub fn defaults_for_test() -> Self {
        Self {
            project_dir: PathBuf::from("."),
            config_ini: PathBuf::from("config.ini"),
            fupan_config_ini: PathBuf::from("fupan_config.ini"),
            main: IniFile::default(),
            fupan: IniFile::default(),
        }
    }
}

/// 向上找含 config/config.ini(.example) 的项目根，兼容从子目录运行。
fn find_project_root(start: &Path) -> Result<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join("config").join("config.ini").exists()
            || d.join("config").join("config.ini.example").exists()
        {
            return Ok(d.to_path_buf());
        }
        dir = d.parent();
    }
    // 找不到配置不能静默把当前目录当项目根：在 zunzhe-core/、桌面旧副本或
    // 任意子目录跑 doctor 时会把 DB/心跳/日志读错位置，误报 daemon 不健康。
    // 显式报错让调用方知道需要从项目根（含 config/）运行。
    bail!(
        "找不到项目根（需要 config/config.ini 或 config/config.ini.example）：{}",
        start.display()
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct EmailStatus {
    pub ok: bool,
    pub configured: bool,
    pub host: String,
    pub port: String,
    pub ssl: bool,
    pub sender: String,
    pub to: String,
    pub masked_login: String,
    pub masked_to: String,
    pub missing: Vec<String>,
    pub hint: String,
}

impl EmailStatus {
    pub fn from_settings(cfg: EmailSettings) -> Self {
        let filled = [
            ("host", !cfg.host.is_empty(), "SMTP服务器"),
            ("login", !cfg.login.is_empty(), "邮箱登录账号"),
            ("auth", !cfg.auth.is_empty(), "发件人密码(授权码)"),
            ("to", !cfg.to.is_empty(), "收件人邮箱"),
        ];
        let missing: Vec<String> = filled
            .iter()
            .filter(|(_, ok, _)| !ok)
            .map(|(_, _, name)| name.to_string())
            .collect();
        let ok = missing.is_empty();
        let hint = if ok {
            "已配置".to_string()
        } else {
            format!("缺少: {}", missing.join("、"))
        };
        Self {
            ok,
            configured: ok,
            host: if cfg.host.is_empty() {
                "smtp.qq.com".into()
            } else {
                cfg.host
            },
            port: if cfg.port.is_empty() {
                "465".into()
            } else {
                cfg.port
            },
            ssl: cfg.ssl,
            sender: cfg.sender,
            to: cfg.to.clone(),
            masked_login: mask(&cfg.login),
            masked_to: mask(&cfg.to),
            missing,
            hint,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EmailSettings {
    pub host: String,
    pub port: String,
    pub ssl: bool,
    pub login: String,
    pub auth: String,
    pub sender: String,
    pub sender_name: String,
    pub to: String,
}

/// HTTPS 推送通道配置。与旧 Python `fupan/notify.py::_push_channels` 对齐，
/// 避免代理/TUN 环境只依赖 SMTP 时通知失效。
#[derive(Debug, Clone, Default)]
pub struct PushChannels {
    pub serverchan: String,
    pub pushplus: String,
    pub bark: String,
    pub ntfy: String,
}

impl PushChannels {
    /// 至少有一个真实凭据才认为已配置，避免把空占位符当成可发送通道。
    pub fn configured(&self) -> bool {
        !self.serverchan.is_empty()
            || !self.pushplus.is_empty()
            || !self.bark.is_empty()
            || !self.ntfy.is_empty()
    }
}

/// 过滤基础项目遗留的占位符，但保留真实 Server酱/PushPlus 密钥。
fn clean_push_value(value: &str) -> String {
    let v = value.trim();
    if v.is_empty() {
        return String::new();
    }
    let low = v.to_lowercase();
    if low == "sctxxxxxxxx" || low == "sctxxxxxxxx..." {
        return String::new();
    }
    if (low.contains("xxx") && low.chars().filter(|c| *c == 'x').count() > 6)
        || (low.contains("key/") && low.contains("xxx"))
    {
        return String::new();
    }
    v.to_string()
}

fn mask(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    if value.chars().count() <= 4 {
        return "*".repeat(value.chars().count());
    }
    let chars: Vec<char> = value.chars().collect();
    format!(
        "{}***{}",
        chars[..2].iter().collect::<String>(),
        chars[chars.len() - 2..].iter().collect::<String>()
    )
}

/// QQ 邮箱模板写入（用于 email-setup）。
pub fn write_qq_email(project_dir: &Path, email: &str, auth: &str, to: &str) -> Result<()> {
    if email.is_empty() || auth.is_empty() {
        bail!("邮箱账号和授权码不能为空");
    }
    let path = project_dir.join("config").join("config.ini");
    let to = if to.is_empty() { email } else { to };
    // 逐行 patch 保留注释与顺序，并按大小写不敏感匹配已有键，避免旧文件里
    // `smtp邮件服务器` 与新增 `SMTP邮件服务器` 并存造成重复键。
    patch_ini_file(
        &path,
        &[
            ("推送配置", "直播状态推送渠道", "邮箱"),
            ("推送配置", "SMTP邮件服务器", "smtp.qq.com"),
            ("推送配置", "是否使用SMTP服务SSL加密(是/否)", "是"),
            ("推送配置", "SMTP邮件服务器端口", "465"),
            ("推送配置", "邮箱登录账号", email),
            ("推送配置", "发件人密码(授权码)", auth),
            ("推送配置", "发件人邮箱", email),
            ("推送配置", "发件人显示昵称", "尊者直播监控"),
            ("推送配置", "收件人邮箱", to),
            ("推送配置", "开播推送开启(是/否)", "是"),
        ],
    )?;
    Ok(())
}

/// 从老项目 `accounts.json` 迁移 QQ 邮箱账号。
///
/// 安全边界与 Python `email_setup.import_legacy_account` 一致：只读取
/// 账号文件，绝不猜测授权码。若密码字段是明文，直接写入；若是加密字符串
/// 且本机没有老项目解密密钥，则返回可读错误，让用户重新填写授权码。
pub fn import_legacy_email(project_dir: &Path, legacy_file: &str) -> Result<serde_json::Value> {
    use serde_json::json;

    let default_path = dirs::home_dir()
        .unwrap_or_default()
        .join("Projects")
        .join("owned")
        .join("zunzhe-dev")
        .join("mcp_server")
        .join("data")
        .join("email")
        .join("accounts.json");
    let path = if legacy_file.is_empty() {
        default_path
    } else {
        PathBuf::from(legacy_file)
    };
    if !path.exists() {
        return Ok(json!({
            "ok": false,
            "error": format!("老项目邮箱账号文件不存在: {}。老项目里只有代码支持，没有实际账号，需要你提供 QQ 邮箱授权码。", path.display()),
            "file": path.display().to_string()
        }));
    }

    let text = fs::read_to_string(&path)
        .with_context(|| format!("读取老项目账号文件失败: {}", path.display()))?;
    let data: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("老项目账号文件 JSON 解析失败: {}", path.display()))?;
    let accounts = match &data {
        serde_json::Value::Array(list) => list.clone(),
        serde_json::Value::Object(map) => map
            .get("accounts")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    if accounts.is_empty() {
        return Ok(
            json!({"ok": false, "error": "账号文件为空，没有可迁移的邮箱账号", "file": path.display().to_string()}),
        );
    }

    let account = accounts
        .iter()
        .filter_map(|v| v.as_object())
        .find(|m| {
            m.get("email")
                .or_else(|| m.get("email_addr"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_lowercase().ends_with("@qq.com"))
                .unwrap_or(false)
        })
        .or_else(|| accounts.first().and_then(|v| v.as_object()));

    let Some(account) = account else {
        return Ok(
            json!({"ok": false, "error": "账号文件没有可识别的账号", "file": path.display().to_string()}),
        );
    };
    let email = account
        .get("email")
        .or_else(|| account.get("email_addr"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    // 老项目密码字段为 AES-256-GCM 密文；Rust 不猜解密密钥，因此先识别
    // `_password`，避免后面的明文 `auth/password` 字段掩盖加密风险。
    let encrypted = account
        .get("_password")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let auth = account
        .get("auth")
        .or_else(|| account.get("password"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if email.is_empty() {
        return Ok(json!({
            "ok": false,
            "error": "账号记录缺少邮箱，无法迁移。请在桌面端「全局设置」重新填写邮箱。",
            "file": path.display().to_string()
        }));
    }
    if !encrypted.is_empty() {
        return Ok(json!({
            "ok": false,
            "error": "找到 QQ 邮箱账号，但授权码是加密存储且无法在本项目解密。请在桌面端「全局设置」重新填写授权码。",
            "email": email,
            "file": path.display().to_string()
        }));
    }
    if auth.is_empty() {
        return Ok(json!({
            "ok": false,
            "error": "账号记录缺少授权码，无法迁移。请在桌面端「全局设置」重新填写授权码。",
            "file": path.display().to_string()
        }));
    }
    write_qq_email(project_dir, email, auth, email)?;
    Ok(json!({
        "ok": true,
        "email": email,
        "to": email,
        "hint": "已从老项目迁移 QQ 邮箱账号。"
    }))
}

/// 逐行原地修改 INI，保留注释、空行、顺序与已有键大小写。
///
/// 更新已有键时按大小写不敏感匹配（对齐 Python `config_api._match_existing_key`），
/// 避免旧文件使用小写键时插入大写同名键；新增节/键追加到文件末尾。
pub fn patch_ini_file(path: &Path, pairs: &[(&str, &str, &str)]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = if path.exists() {
        fs::read_to_string(path).with_context(|| format!("读取配置文件失败: {}", path.display()))?
    } else {
        String::new()
    };
    let mut lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    let mut pending: Vec<(&str, &str, &str)> = pairs.to_vec();
    let mut i = 0;
    while i < lines.len() {
        let raw = lines[i].trim_start_matches('\u{feff}').trim().to_string();
        if let Some(name) = raw.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            let section = name.trim().to_string();
            let mut j = i + 1;
            while j < lines.len() {
                let inner = lines[j].trim_start_matches('\u{feff}').trim().to_string();
                if inner.starts_with('[') {
                    break;
                }
                if let Some((k, _)) = inner.split_once('=') {
                    let k = k.trim().to_string();
                    if let Some(pos) = pending.iter().position(|(s, pk, _)| {
                        *s == section && pk.to_lowercase() == k.to_lowercase()
                    }) {
                        let (_, _, v) = pending.remove(pos);
                        lines[j] = format!("{k} = {v}");
                    }
                }
                j += 1;
            }
            i = j;
            continue;
        }
        i += 1;
    }

    for (section, key, value) in pending {
        let sec_line = format!("[{section}]");
        let sec_pos = lines
            .iter()
            .position(|l| l.trim_start_matches('\u{feff}').trim() == sec_line);
        if let Some(pos) = sec_pos {
            let mut insert_at = pos + 1;
            while insert_at < lines.len() {
                let t = lines[insert_at]
                    .trim_start_matches('\u{feff}')
                    .trim()
                    .to_string();
                if t.starts_with('[') {
                    break;
                }
                insert_at += 1;
            }
            lines.insert(insert_at, format!("{key} = {value}"));
        } else {
            if !lines.is_empty() && !lines.last().map(|l| l.trim().is_empty()).unwrap_or(true) {
                lines.push(String::new());
            }
            lines.push(sec_line);
            lines.push(format!("{key} = {value}"));
        }
    }

    atomic_write(path, format!("{}\n", lines.join("\n")))
        .with_context(|| format!("写配置失败: {}", path.display()))?;
    Ok(())
}

/// 原子写文件：先写同目录临时文件并 fsync，再 rename 覆盖目标。
///
/// 配置文件包含 QQ 授权码等敏感信息，任何情况下都不能因为写入中断而变成
/// 半截文件。临时文件放在同一目录，保证 rename 不跨文件系统，Unix/Windows
/// 都具备替换语义；失败时原始文件保持原样。
pub fn atomic_write(path: &Path, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.tmp");
    // pid + 纳秒时间戳 + 进程内计数，四重保证同目录临时文件不碰撞。不要在这里
    // 清理“看起来像残留”的旧 tmp：同一目录可能正有另一个进程在原子写同一目标，
    // 全局清理会误删别人正在写的文件。崩溃残留最多阻塞旧文件名本身，新名字
    // 永远可用，写操作不会因此永久失败。
    let tmp = parent.join(format!(
        ".{file_name}.tmp.{}.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let original_permissions = fs::metadata(path).ok().map(|meta| meta.permissions());
    let write_result = (|| -> std::io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(contents.as_ref())?;
        file.flush()?;
        file.sync_all()?;
        if let Some(permissions) = original_permissions {
            fs::set_permissions(&tmp, permissions)?;
        }
        rename_replace(&tmp, path)?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write_result
}

/// 用原子替换语义覆盖目标文件。
///
/// Unix 的 `rename(2)` 天然覆盖；Windows 的 `MoveFileExW` 默认不能覆盖已存在
/// 文件，必须显式传 `MOVEFILE_REPLACE_EXISTING`，否则配置/通知队列在 Windows
/// 上第二次写入会失败并留下旧内容。
#[cfg(not(windows))]
fn rename_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::rename(from, to)
}

#[cfg(windows)]
fn rename_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let from_wide: Vec<u16> = from
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let to_wide: Vec<u16> = to
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let ok = unsafe {
        MoveFileExW(
            from_wide.as_ptr(),
            to_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ini_and_yes() {
        let text = r#"
[推送配置]
SMTP邮件服务器 = smtp.qq.com
是否使用SMTP服务SSL加密(是/否) = 是
"#;
        let ini = IniFile::parse(text).unwrap();
        assert_eq!(
            ini.get("推送配置", "SMTP邮件服务器").unwrap(),
            "smtp.qq.com"
        );
        assert!(IniFile::is_yes(
            &ini.get("推送配置", "是否使用SMTP服务SSL加密(是/否)")
                .unwrap()
        ));
    }

    #[test]
    fn strips_utf8_bom_from_first_section() {
        // Windows/旧 Python 保存的 config.ini 经常以 UTF-8 BOM 开头；若不剥离，
        // 第一段会被 Rust 当成畸形内容整段忽略，导致录制设置等主配置静默回退默认值。
        let text = "\u{feff}[录制设置]\n循环时间(秒) = 120\n";
        let ini = IniFile::parse(text).unwrap();
        assert_eq!(ini.get("录制设置", "循环时间(秒)").as_deref(), Some("120"));
    }

    #[test]
    fn atomic_write_replaces_existing_file_without_leftover_tmp() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-atomic-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.ini");
        atomic_write(&path, "v1").unwrap();
        atomic_write(&path, "v2").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v2");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "原子写失败后不应残留临时文件");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_write_survives_stale_tmp_from_other_process() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-atomic-stale-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.ini");
        // 模拟上一次进程在写临时文件时崩溃：残留文件存在且内容陈旧。
        // 新写入必须选用全新临时名，不能因为同名残留而永久失败，也不能
        // 为了“清理”误删另一个并发进程正在使用的临时文件。
        // 使用本测试进程的真实 pid，但文件名只是“看起来属于本进程”的旧残留：
        // 它不代表当前正在写的临时文件，新写入必须绕开它而不是被它阻塞。
        let stale = dir.join(format!(".config.ini.tmp.{}.0", std::process::id()));
        std::fs::write(&stale, "stale").unwrap();
        atomic_write(&path, "v1").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v1");
        assert_eq!(std::fs::read_to_string(&stale).unwrap(), "stale");
        let current_tmps: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let stale_name = stale
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                (name.starts_with(".config.ini.tmp.") && name != stale_name).then_some(name)
            })
            .collect();
        assert!(
            current_tmps.is_empty(),
            "正常写入不应留下本次临时文件: {current_tmps:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn email_status_missing_hint() {
        let status = EmailStatus::from_settings(EmailSettings {
            host: String::new(),
            port: String::new(),
            ssl: true,
            login: String::new(),
            auth: String::new(),
            sender: String::new(),
            sender_name: String::new(),
            to: String::new(),
        });
        assert!(!status.configured);
        assert!(status.hint.contains("SMTP服务器"));
    }

    #[test]
    fn email_status_lists_all_missing_fields() {
        // 空配置时 hint 必须把登录账号、授权码、收件人都列出来；SMTP 服务器
        // 有 smtp.qq.com 默认值，不应误报为缺失，但 host 缺失时 email_status
        // 仍要显示“未配置”。
        let cfg = Config::defaults_for_test();
        let status = cfg.email_status();
        assert!(!status.configured);
        for field in ["邮箱登录账号", "发件人密码(授权码)", "收件人邮箱"] {
            assert!(
                status.hint.contains(field),
                "hint 应包含 {field}，实际: {}",
                status.hint
            );
        }
        assert!(!status.hint.contains("SMTP服务器"));
    }

    #[test]
    fn find_project_root_walks_up_from_nested_dir() {
        let root = std::env::temp_dir().join(format!(
            "zunzhe-root-probe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("zunzhe-core").join("src")).unwrap();
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::write(
            root.join("config").join("config.ini.example"),
            "[推送配置]\n",
        )
        .unwrap();
        std::fs::write(
            root.join("config").join("fupan_config.ini.example"),
            "[视频监控]\n",
        )
        .unwrap();

        // 从任意子目录（如 zunzhe-core/src）调用必须定位到项目根，不能把
        // 子目录当成项目根，否则 DB/心跳/日志路径全部读错。
        let cfg = Config::load(&root.join("zunzhe-core").join("src")).unwrap();
        assert_eq!(cfg.project_dir, root);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn mask_hides_middle() {
        assert_eq!(mask("abc@qq.com"), "ab***om");
    }

    #[test]
    fn main_config_keys_are_case_insensitive() {
        let text = r#"
[推送配置]
smtp邮件服务器 = smtp.qq.com
是否使用smtp服务ssl加密(是/否) = 是
SMTP邮件服务器端口 = 465
"#;
        let ini = IniFile::parse(text).unwrap();
        assert_eq!(
            IniFile::get_case_insensitive(&ini, "推送配置", "SMTP邮件服务器"),
            "smtp.qq.com"
        );
        assert_eq!(
            IniFile::get_case_insensitive(&ini, "推送配置", "smtp邮件服务器端口"),
            "465"
        );
    }

    #[test]
    fn fupan_is_yes_reads_fupan_file_only() {
        let text = "[视频监控]\n自动下载新视频(是/否) = 否\n";
        let ini = IniFile::parse(text).unwrap();
        let cfg = Config {
            project_dir: PathBuf::from("."),
            config_ini: PathBuf::from("config.ini"),
            fupan_config_ini: PathBuf::from("fupan_config.ini"),
            main: IniFile::default(),
            fupan: ini,
        };
        assert!(!cfg.is_yes_fupan("视频监控", "自动下载新视频(是/否)", true));
    }

    #[test]
    fn pushplus_reads_legacy_and_current_key_names() {
        let cfg = |text: &str| {
            let ini = IniFile::parse(text).unwrap();
            Config {
                project_dir: PathBuf::from("."),
                config_ini: PathBuf::from("config.ini"),
                fupan_config_ini: PathBuf::from("fupan_config.ini"),
                main: ini,
                fupan: IniFile::default(),
            }
        };

        let modern = cfg("[推送配置]\npushplus推送token = modern-token\n");
        assert_eq!(modern.push_channels().pushplus, "modern-token");

        let legacy = cfg("[推送配置]\npushplus_token = legacy-token\n");
        assert_eq!(legacy.push_channels().pushplus, "legacy-token");

        let both =
            cfg("[推送配置]\npushplus_token = legacy-token\npushplus推送token = modern-token\n");
        assert_eq!(both.push_channels().pushplus, "modern-token");
    }

    #[test]
    fn write_qq_email_reuses_lowercase_existing_keys() {
        let dir = std::env::temp_dir().join(format!("zunzhe-qq-email-{}", std::process::id()));
        let cfg_dir = dir.join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.ini"),
            "[推送配置]\nsmtp邮件服务器 = smtp.qq.com\n",
        )
        .unwrap();
        write_qq_email(&dir, "me@qq.com", "authcode", "you@qq.com").unwrap();
        let text = std::fs::read_to_string(cfg_dir.join("config.ini")).unwrap();
        assert!(text.contains("smtp邮件服务器 = smtp.qq.com"));
        assert!(!text.contains("SMTP邮件服务器 ="));
        assert!(text.contains("邮箱登录账号 = me@qq.com"));
        assert!(text.contains("收件人邮箱 = you@qq.com"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn patch_ini_file_is_atomic_and_leaves_no_temp_files() {
        let dir = std::env::temp_dir().join(format!("zunzhe-atomic-{}", std::process::id()));
        let cfg_dir = dir.join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let path = cfg_dir.join("fupan_config.ini");
        std::fs::write(&path, "[视频监控]\n检查间隔秒 = 900\n").unwrap();

        patch_ini_file(&path, &[("视频监控", "检查间隔秒", "600")]).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("检查间隔秒 = 600"));
        assert!(!text.contains("检查间隔秒 = 900"));

        let temp_files: Vec<_> = std::fs::read_dir(&cfg_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".fupan_config.ini.tmp.")
            })
            .collect();
        assert!(
            temp_files.is_empty(),
            "原子写完成后不应留下临时文件: {temp_files:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_legacy_email_rejects_encrypted_password() {
        let dir = std::env::temp_dir().join(format!("zunzhe-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = dir.join("accounts.json");
        std::fs::write(
            &legacy,
            r#"[{"email":"me@qq.com","_password":"aes-cipher-not-plaintext"}]"#,
        )
        .unwrap();

        let result = import_legacy_email(&dir, legacy.to_str().unwrap()).unwrap();
        assert_eq!(result["ok"], false);
        assert!(result["error"].as_str().unwrap().contains("加密存储"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_legacy_email_writes_plaintext_auth() {
        let dir = std::env::temp_dir().join(format!("zunzhe-import-plain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = dir.join("accounts.json");
        std::fs::write(&legacy, r#"[{"email":"me@qq.com","auth":"qq-smtp-code"}]"#).unwrap();

        let result = import_legacy_email(&dir, legacy.to_str().unwrap()).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["email"], "me@qq.com");
        let text = std::fs::read_to_string(dir.join("config").join("config.ini")).unwrap();
        assert!(text.contains("邮箱登录账号 = me@qq.com"));
        assert!(text.contains("发件人密码(授权码) = qq-smtp-code"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
