//! 接続先の公開設定。資格情報は含めず、変更はログアウト後の再起動で反映する。
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, io::ErrorKind, path::PathBuf};
use tauri::{AppHandle, Manager, WebviewWindow};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub server_url: String,
    pub discovery_url: String,
    pub client_id: String,
}

impl Default for Connection {
    fn default() -> Self {
        Self {
            server_url: String::new(),
            discovery_url: String::new(),
            client_id: crate::auth::CLIENT_ID.into(),
        }
    }
}

impl Connection {
    pub fn validated(mut self) -> Result<Self, String> {
        self.server_url = self.server_url.trim().into();
        self.discovery_url = self.discovery_url.trim().into();
        self.client_id = self.client_id.trim().into();
        for value in [&self.server_url, &self.discovery_url] {
            let url = Url::parse(value).map_err(|_| "接続先のURLが不正です")?;
            if url.scheme() != "https"
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err("接続先には、ユーザー情報・クエリー・フラグメントを含まないHTTPSのURLを指定してください".into());
            }
        }
        let server = Url::parse(&self.server_url).unwrap();
        if server.path() != "/" {
            return Err("サーバーURLにはパスを付けず、https://translate.example.com の形式で指定してください".into());
        }
        self.server_url = server.origin().ascii_serialization();
        self.discovery_url = Url::parse(&self.discovery_url).unwrap().to_string();
        if self.client_id.is_empty()
            || self.client_id.len() > 256
            || self.client_id.chars().any(char::is_control)
        {
            return Err("client IDを1〜256文字で指定してください".into());
        }
        Ok(self)
    }

    pub fn vault_account(&self) -> String {
        // 既存利用者のキーチェーン項目を維持。他の接続先は3項目すべてで分離する。
        if self.server_url == crate::auth::SERVER
            && self.discovery_url == crate::auth::DISCOVERY
            && self.client_id == crate::auth::CLIENT_ID
        {
            return crate::auth::CLIENT_ID.into();
        }
        let encoded = serde_json::to_vec(self).expect("接続先を直列化できませんでした");
        format!("connection-{:x}", Sha256::digest(encoded))
    }
}

fn path(app: &AppHandle) -> Result<PathBuf, String> {
    #[cfg(feature = "smoke-test")]
    if let Some(dir) = std::env::var_os("PTRANSLATE_SMOKE_CONFIG") {
        return Ok(PathBuf::from(dir).join("connection.json"));
    }
    app.path()
        .app_config_dir()
        .map(|dir| dir.join("connection.json"))
        .map_err(|_| "接続先設定の保存先を取得できませんでした".into())
}

pub fn load(app: &AppHandle) -> Result<Connection, String> {
    match fs::read(path(app)?) {
        Ok(bytes) => serde_json::from_slice::<Connection>(&bytes)
            .map_err(|_| "接続先設定の形式が不正です".to_string())?
            .validated(),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Connection::default()),
        Err(_) => Err("接続先設定を読み込めませんでした".into()),
    }
}

fn require_main(window: &WebviewWindow) -> Result<(), String> {
    if window.label() != "main" {
        return Err("接続先の変更はメインウィンドウで行ってください".into());
    }
    Ok(())
}

#[tauri::command]
pub fn load_connection(app: AppHandle, window: WebviewWindow) -> Result<Connection, String> {
    require_main(&window)?;
    load(&app)
}

#[tauri::command]
pub async fn save_connection(
    app: AppHandle,
    window: WebviewWindow,
    connection: Connection,
) -> Result<(), String> {
    require_main(&window)?;
    let connection = connection.validated()?;
    let backend = app.state::<crate::Backend>();
    let _changing = backend.auth.begin_connection_change()?;
    let destination = path(&app)?;
    let parent = destination.parent().ok_or("接続先設定の保存先が不正です")?;
    fs::create_dir_all(parent).map_err(|_| "接続先設定を保存できませんでした")?;
    let mut file =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| "接続先設定を保存できませんでした")?;
    serde_json::to_writer(&mut file, &connection)
        .map_err(|_| "接続先設定を保存できませんでした")?;
    crate::cancel_all(&app);
    if let Err(error) = backend.auth.logout().await {
        crate::publish_auth(&app, Some(error.clone()));
        return Err(error);
    }
    if file.persist(&destination).is_err() {
        crate::publish_auth(&app, None);
        return Err("接続先設定を保存できませんでした。もう一度ログインしてください".into());
    }
    app.restart();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_and_normalizes_public_settings() {
        let value = Connection {
            server_url: " https://translate.example.com/ ".into(),
            discovery_url: "https://auth.example.com/.well-known/openid-configuration".into(),
            client_id: " custom-desktop ".into(),
        }
        .validated()
        .unwrap();
        assert_eq!(value.server_url, "https://translate.example.com");
        assert_eq!(value.client_id, "custom-desktop");
        for url in [
            "http://translate.example.com",
            "https://user:password@example.com",
            "https://example.com/path",
            "https://example.com/?token=secret",
            "https://example.com/#fragment",
        ] {
            let mut invalid = value.clone();
            invalid.server_url = url.into();
            assert!(invalid.validated().is_err());
        }
        for url in [
            "http://auth.example.com/discovery",
            "https://auth.example.com/discovery?token=secret",
            "https://user:password@auth.example.com/discovery",
        ] {
            let mut invalid = value.clone();
            invalid.discovery_url = url.into();
            assert!(invalid.validated().is_err());
        }
        let mut invalid = value;
        invalid.client_id = "  ".into();
        assert!(invalid.validated().is_err());
    }
    #[test]
    fn isolates_credentials_by_server_issuer_and_client() {
        let default = Connection {
            server_url: crate::auth::SERVER.into(),
            discovery_url: crate::auth::DISCOVERY.into(),
            client_id: crate::auth::CLIENT_ID.into(),
        };
        assert_ne!(
            Connection::default().vault_account(),
            default.vault_account()
        );
        assert_eq!(default.vault_account(), crate::auth::CLIENT_ID);
        assert_eq!(
            default.clone().validated().unwrap().vault_account(),
            default.vault_account()
        );
        let mut custom = default.clone();
        custom.server_url = "https://translate.example.com".into();
        let first = custom.vault_account();
        assert_ne!(first, default.vault_account());
        custom.discovery_url = "https://auth.example.com/.well-known/openid-configuration".into();
        let second = custom.vault_account();
        assert_ne!(first, second);
        custom.client_id = "another-client".into();
        assert_ne!(second, custom.vault_account());
    }
}
