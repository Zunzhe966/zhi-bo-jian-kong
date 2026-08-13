use std::path::PathBuf;

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::config::{IniFile, patch_ini_file};

/// 与 Python fupan.config_api 对齐的 JSON 配置读写。
pub struct ConfigApi {
    project_dir: PathBuf,
}

impl ConfigApi {
    pub fn new(project_dir: PathBuf) -> Self {
        Self { project_dir }
    }

    fn path(&self, file: &str) -> Result<PathBuf> {
        match file {
            "config" | "config.ini" | "main" => {
                Ok(self.project_dir.join("config").join("config.ini"))
            }
            "fupan" | "fupan_config" | "fupan_config.ini" => {
                Ok(self.project_dir.join("config").join("fupan_config.ini"))
            }
            _ => bail!("未知配置文件: {file}，可用 config / fupan_config"),
        }
    }

    fn read(&self, file: &str) -> Result<IniFile> {
        let path = self.path(file)?;
        if path.exists() {
            IniFile::load(&path)
        } else {
            Ok(IniFile::default())
        }
    }

    pub fn get(&self, file: &str, section: &str, key: &str) -> Result<Value> {
        let files: Vec<&str> = match file {
            "all" | "*" | "" => vec!["config.ini", "fupan_config.ini"],
            other => vec![other],
        };
        let mut result = serde_json::Map::new();
        for f in files {
            let ini = self.read(f)?;
            let display_name = self
                .path(f)
                .ok()
                .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
                .unwrap_or_else(|| f.to_string());
            let mut file_data = serde_json::Map::new();
            let sections = if section.is_empty() {
                ini.sections()
            } else {
                vec![section.to_string()]
            };
            for sec in sections {
                let sec_ref = sec.as_str();
                let Some(map) = ini.section(sec_ref) else {
                    continue;
                };
                let mut sec_data = serde_json::Map::new();
                let keys: Vec<String> = if key.is_empty() {
                    map.keys().cloned().collect()
                } else {
                    vec![key.to_string()]
                };
                for k in keys {
                    if let Some(v) = map.get(&k) {
                        sec_data.insert(k.clone(), json!(mask_secret(sec_ref, &k, v)));
                    }
                }
                if !sec_data.is_empty() {
                    file_data.insert(sec, Value::Object(sec_data));
                }
            }
            if !file_data.is_empty() {
                result.insert(display_name, Value::Object(file_data));
            }
        }
        Ok(Value::Object(result))
    }

    pub fn set(&self, file: &str, section: &str, key: &str, value: &str) -> Result<Value> {
        let path = self.path(file)?;
        patch_ini_file(&path, &[(section, key, value)])?;
        Ok(
            json!({"file": path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
                  "section": section, "key": key, "set": true}),
        )
    }

    pub fn set_bulk(&self, updates: &Value) -> Result<Value> {
        let mut changed = Vec::new();
        if let Some(files) = updates.as_object() {
            for (fname, sections) in files {
                let file_key = fname.trim_end_matches(".ini").to_string();
                let path = self.path(&file_key)?;
                let mut pairs = Vec::new();
                if let Some(sections_obj) = sections.as_object() {
                    for (section, section_value) in sections_obj {
                        if let Some(pairs_obj) = section_value.as_object() {
                            for (key, value) in pairs_obj {
                                if value.as_str() == Some("***") {
                                    continue;
                                }
                                pairs.push((
                                    section.as_str(),
                                    key.as_str(),
                                    value.as_str().unwrap_or_default(),
                                ));
                                changed.push(json!({"file": path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
                                                    "section": section, "key": key}));
                            }
                        }
                    }
                }
                patch_ini_file(&path, &pairs)?;
            }
        }
        Ok(json!({"changed": changed, "count": changed.len()}))
    }

    pub fn schema(&self) -> Value {
        json!({
            "description": "尊者直播监控工具配置。大模型由外部智能体自带，通过 MCP/CLI 写入 [知识提炼] 即可。",
            "files": {
                "config.ini": {
                    "录制设置": {"循环时间(秒)": "监控轮询间隔", "视频保存格式ts|mkv|flv|mp4|mp3音频|m4a音频": "保存格式"},
                    "推送配置": {
                        "直播状态推送渠道": "填「邮箱」启用邮件通知",
                        "server酱SendKey": "Server酱 SendKey，可推微信（secret）",
                        "pushplus推送token": "PushPlus token（secret）",
                        "bark推送接口链接": "iOS Bark 推送地址（secret）",
                        "ntfy推送地址": "ntfy 订阅地址（secret）",
                        "SMTP邮件服务器": "如 smtp.qq.com",
                        "是否使用SMTP服务SSL加密(是/否)": "是/否",
                        "SMTP邮件服务器端口": "SSL 通常 465",
                        "邮箱登录账号": "发件邮箱",
                        "发件人密码(授权码)": "SMTP 授权码（secret）",
                        "发件人邮箱": "发件邮箱",
                        "发件人显示昵称": "邮件显示名",
                        "收件人邮箱": "通知收件人",
                        "开播推送开启(是/否)": "是/否"
                    }
                },
                "fupan_config.ini": {
                    "转写": {"是否启用转写(是/否)": "是/否", "whisper模型": "tiny/base/small/medium/large-v3", "运行设备": "auto/cpu/cuda"},
                    "知识提炼": {"是否启用知识提炼(是/否)": "是/否", "api地址": "OpenAI 兼容 API", "api密钥": "API Key（secret）", "模型": "模型名"},
                    "切片": {"是否启用切片(是/否)": "是/否", "最多片段数": "整数"},
                    "复盘邮件": {"录制完成后发送复盘笔记(是/否)": "是/否", "发现新视频时发送通知(是/否)": "是/否"},
                    "视频监控": {"检查间隔秒": "轮询间隔", "自动下载新视频(是/否)": "是/否"}
                }
            }
        })
    }
}

fn mask_secret<'a>(section: &str, key: &str, value: &'a str) -> &'a str {
    if value.is_empty() {
        return value;
    }
    let key_l = key.to_lowercase();
    let secret = key_l.contains("密码")
        || key_l.contains("授权码")
        || key_l.contains("api密钥")
        || key_l.contains("cookie")
        || key_l.contains("token")
        || key_l.contains("令牌")
        || key_l.contains("密钥")
        || key_l.contains("pass")
        || section.trim() == "Cookie";
    if secret { "***" } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_secret_keys() {
        assert_eq!(mask_secret("推送配置", "发件人密码(授权码)", "abc"), "***");
        assert_eq!(mask_secret("视频监控", "b站cookie", "x"), "***");
        assert_eq!(mask_secret("推送配置", "收件人邮箱", "a@b.com"), "a@b.com");
    }

    #[test]
    fn set_and_get_roundtrip() {
        let dir = std::env::temp_dir().join(format!("zunzhe-config-api-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(
            dir.join("config").join("fupan_config.ini"),
            "[视频监控]\n检查间隔秒 = 900\n",
        )
        .unwrap();
        let api = ConfigApi::new(dir.clone());
        api.set("fupan_config", "视频监控", "检查间隔秒", "600")
            .unwrap();
        let got = api.get("fupan_config", "视频监控", "检查间隔秒").unwrap();
        assert_eq!(got["fupan_config.ini"]["视频监控"]["检查间隔秒"], "600");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn patch_preserves_comments_and_order() {
        let dir =
            std::env::temp_dir().join(format!("zunzhe-config-comment-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("config")).unwrap();
        let path = dir.join("config").join("fupan_config.ini");
        std::fs::write(
            &path,
            "# 顶部注释\n[视频监控]\n# 间隔注释\n检查间隔秒 = 900\nb站cookie =\n",
        )
        .unwrap();
        let api = ConfigApi::new(dir.clone());
        api.set("fupan_config", "视频监控", "检查间隔秒", "600")
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# 顶部注释"));
        assert!(text.contains("# 间隔注释"));
        assert!(text.contains("检查间隔秒 = 600"));
        assert!(!text.contains("检查间隔秒 = 900"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn patch_matches_existing_key_case_insensitively() {
        let dir = std::env::temp_dir().join(format!("zunzhe-config-case-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("config")).unwrap();
        let path = dir.join("config").join("config.ini");
        std::fs::write(
            &path,
            "[推送配置]\nsmtp邮件服务器 = smtp.qq.com\n是否使用smtp服务ssl加密(是/否) = 是\n",
        )
        .unwrap();
        let api = ConfigApi::new(dir.clone());
        api.set("config", "推送配置", "SMTP邮件服务器", "smtp.qq.com")
            .unwrap();
        api.set("config", "推送配置", "是否使用SMTP服务SSL加密(是/否)", "是")
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("smtp邮件服务器 = smtp.qq.com"));
        assert!(text.contains("是否使用smtp服务ssl加密(是/否) = 是"));
        assert!(!text.contains("SMTP邮件服务器 ="));
        assert!(!text.contains("是否使用SMTP服务SSL加密(是/否) ="));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
