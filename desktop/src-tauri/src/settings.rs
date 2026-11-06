use serde::{Deserialize, Serialize};
use std::{fs, io::ErrorKind, path::PathBuf};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    source: String,
    target: String,
    form: String,
    glossary: String,
    auto: bool,
    pub shortcut: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            source: String::new(),
            target: "auto".into(),
            form: String::new(),
            glossary: String::new(),
            auto: true,
            shortcut: "double-copy".into(),
        }
    }
}

fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    #[cfg(feature = "smoke-test")]
    if let Some(dir) = std::env::var_os("PTRANSLATE_SMOKE_CONFIG") {
        return Ok(PathBuf::from(dir).join("settings.json"));
    }
    app.path()
        .app_config_dir()
        .map(|dir| dir.join("settings.json"))
        .map_err(|_| "設定の保存先を取得できませんでした".into())
}

#[tauri::command]
pub fn load_settings(app: AppHandle) -> Result<Option<Settings>, String> {
    let path = settings_path(&app)?;
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| "設定ファイルの形式が不正です".into()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(_) => Err("設定を読み込めませんでした".into()),
    }
}

#[tauri::command]
pub fn save_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
    let previous = load_settings(app.clone())?.unwrap_or_default();
    crate::resident::configure_shortcut(&app, &settings.shortcut)?;
    let path = settings_path(&app)?;
    let dir = path.parent().expect("設定ファイルの親ディレクトリ");
    let save = || -> Result<(), Box<dyn std::error::Error>> {
        fs::create_dir_all(dir)?;
        let mut file = tempfile::NamedTempFile::new_in(dir)?;
        serde_json::to_writer(&mut file, &settings)?;
        file.persist(&path)?;
        Ok(())
    };
    if save().is_err() {
        let _ = crate::resident::configure_shortcut(&app, &previous.shortcut);
        return Err("設定を保存できませんでした".into());
    }
    let _ = app.emit("settings-changed", ());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_settings_saved_before_shortcuts_were_added() {
        let settings: Settings = serde_json::from_str(
            r#"{"source":"en","target":"ja","form":"polite","glossary":"","auto":false}"#,
        )
        .unwrap();
        assert_eq!(settings.shortcut, "double-copy");
        assert_eq!(settings.source, "en");
        assert!(!settings.auto);
    }
}
