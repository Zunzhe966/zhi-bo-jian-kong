use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

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
            let line = raw.trim();
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
        EmailStatus::from_settings(cfg)
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
        PushChannels {
            serverchan: clean_push_value(&self.get_main("推送配置", "server酱SendKey")),
            pushplus: clean_push_value(&self.get_main("推送配置", "pushplus推送token")),
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
    Ok(start.to_path_buf())
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
    let mut ini = IniFile::load(&path)?;
    let section = ini.data.entry("推送配置".to_string()).or_default();
    section.insert("直播状态推送渠道".to_string(), "邮箱".to_string());
    section.insert("SMTP邮件服务器".to_string(), "smtp.qq.com".to_string());
    section.insert(
        "是否使用SMTP服务SSL加密(是/否)".to_string(),
        "是".to_string(),
    );
    section.insert("SMTP邮件服务器端口".to_string(), "465".to_string());
    section.insert("邮箱登录账号".to_string(), email.to_string());
    section.insert("发件人密码(授权码)".to_string(), auth.to_string());
    section.insert("发件人邮箱".to_string(), email.to_string());
    section.insert("发件人显示昵称".to_string(), "尊者直播监控".to_string());
    section.insert(
        "收件人邮箱".to_string(),
        if to.is_empty() {
            email.to_string()
        } else {
            to.to_string()
        },
    );
    section.insert("开播推送开启(是/否)".to_string(), "是".to_string());
    write_ini(&path, &ini)?;
    Ok(())
}

fn write_ini(path: &Path, ini: &IniFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut out = String::new();
    for section in ini.sections() {
        out.push_str(&format!("[{section}]\n"));
        let keys = ini.section(&section).unwrap();
        let mut list: Vec<(&String, &String)> = keys.iter().collect();
        list.sort_by(|a, b| a.0.cmp(b.0));
        for (k, v) in list {
            out.push_str(&format!("{k} = {v}\n"));
        }
        out.push('\n');
    }
    fs::write(path, out).with_context(|| format!("写配置失败: {}", path.display()))?;
    Ok(())
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
        assert!(status.hint.contains("授权码"));
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
}
