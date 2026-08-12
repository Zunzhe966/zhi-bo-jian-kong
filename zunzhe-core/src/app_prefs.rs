use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Value, json};

/// 桌面端偏好：config/桌面偏好.json。
/// 与 Python 桌面端/旧版 CLI 保持一致，未写出的键回落到默认值。
pub fn app_prefs_path(project_dir: &Path) -> PathBuf {
    project_dir.join("config").join("桌面偏好.json")
}

pub fn app_prefs_defaults() -> Value {
    json!({
        "watchdog": true,
        "autoStartServices": true,
        "openAtLogin": false,
        "wizardDone": false,
        "localNotify": true,
    })
}

pub fn read_app_prefs(project_dir: &Path) -> Value {
    let mut prefs = app_prefs_defaults();
    let path = app_prefs_path(project_dir);
    if let Ok(text) = std::fs::read_to_string(&path)
        && let Ok(v) = serde_json::from_str::<Value>(&text)
        && let Some(obj) = v.as_object()
        && let Some(base) = prefs.as_object_mut()
    {
        for (k, v) in obj {
            if base.contains_key(k) {
                base.insert(k.clone(), v.clone());
            }
        }
    }
    prefs
}

pub fn write_app_prefs(project_dir: &Path, updates: &Value) -> Result<Value> {
    let mut prefs = read_app_prefs(project_dir);
    if let (Some(base), Some(updates_obj)) = (prefs.as_object_mut(), updates.as_object()) {
        for (k, v) in updates_obj {
            if base.contains_key(k) {
                base.insert(k.clone(), v.clone());
            }
        }
    }
    let path = app_prefs_path(project_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(&prefs)?;
    std::fs::write(&path, text + "\n")?;
    Ok(prefs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_stable_and_unknown_keys_are_dropped() {
        let dir = std::env::temp_dir().join(format!("zunzhe-prefs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();

        let before = read_app_prefs(&dir);
        assert_eq!(before["watchdog"], json!(true));
        assert_eq!(before["openAtLogin"], json!(false));

        let written =
            write_app_prefs(&dir, &json!({"wizardDone": true, "unknownKey": "应被忽略"})).unwrap();
        assert_eq!(written["wizardDone"], json!(true));
        assert!(written.get("unknownKey").is_none());

        // 重新读取验证落盘，而不是只返回内存值。
        let after = read_app_prefs(&dir);
        assert_eq!(after["wizardDone"], json!(true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_missing_file_returns_defaults() {
        let dir = std::env::temp_dir().join(format!("zunzhe-prefs-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        let prefs = read_app_prefs(&dir);
        assert_eq!(prefs["watchdog"], json!(true));
        assert_eq!(prefs["autoStartServices"], json!(true));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
