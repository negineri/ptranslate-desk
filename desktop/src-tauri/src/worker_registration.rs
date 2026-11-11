//! 提供先の設定と資格を管理する。推論の起動とは別の操作。
use crate::{auth::Auth, connection::Connection, local, Backend};
use reqwest::{Method, Response};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::json;
use std::{
    fs,
    io::ErrorKind,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Manager, WebviewWindow};
use zeroize::{Zeroize, Zeroizing};

const TERMS_VERSION: u32 = 1;
const MAX_REPLY: usize = 16 * 1024;
const MAX_TTL: u64 = 7 * 86400;

#[derive(Default)]
pub struct RegistrationGate(tokio::sync::Mutex<()>);

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    worker_id: String,
    secret: String,
    expires_at: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

#[derive(Deserialize)]
struct Issued {
    worker_id: String,
    #[serde(deserialize_with = "read_secret")]
    secret: Zeroizing<String>,
    expires_in: u64,
}

fn read_secret<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> Result<Zeroizing<String>, D::Error> {
    String::deserialize(decoder).map(Zeroizing::new)
}

#[derive(Deserialize)]
struct Renewal {
    expires_in: u64,
}

#[derive(Serialize)]
pub struct Status {
    connection: Connection,
    supported: bool,
    expires_at: Option<u64>,
    terms_version: u32,
}

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|dir| dir.join("worker-connection.json"))
        .map_err(|_| "提供先の保存先を取得できませんでした".into())
}

fn load(app: &AppHandle) -> Result<Connection, String> {
    match fs::read(config_path(app)?) {
        Ok(bytes) => serde_json::from_slice::<Connection>(&bytes)
            .map_err(|_| "提供先の設定が不正です".to_string())?
            .validated(),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Connection::default()),
        Err(_) => Err("提供先の設定を読み込めませんでした".into()),
    }
}

fn save(app: &AppHandle, connection: &Connection) -> Result<(), String> {
    let path = config_path(app)?;
    let parent = path.parent().ok_or("提供先の保存先が不正です")?;
    fs::create_dir_all(parent).map_err(|_| "提供先を保存できませんでした")?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| "提供先を保存できませんでした")?;
    serde_json::to_writer(&mut temporary, connection)
        .map_err(|_| "提供先を保存できませんでした")?;
    temporary
        .persist(path)
        .map_err(|_| "提供先を保存できませんでした")?;
    Ok(())
}

fn vault(connection: &Connection) -> Result<keyring::Entry, String> {
    keyring::Entry::new(
        "com.witenp.ptranslate-desk",
        &format!("worker-registration-{}", connection.vault_account()),
    )
    .map_err(|_| "ワーカーの資格情報ストアを開けませんでした".into())
}

fn valid_id(id: &str, len: usize) -> bool {
    id.len() == len
        && id
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

impl Registration {
    fn valid(&self) -> bool {
        valid_id(&self.worker_id, 32)
            && self
                .secret
                .strip_prefix("ptw_")
                .is_some_and(|secret| valid_id(secret, 64))
    }
}

fn credential(entry: &keyring::Entry) -> Result<Option<Registration>, String> {
    match entry.get_password() {
        Ok(secret) => {
            let secret = Zeroizing::new(secret);
            let value: Registration =
                serde_json::from_str(&secret).map_err(|_| "ワーカーの資格が不正です")?;
            if !value.valid() {
                return Err("ワーカーの資格が不正です".into());
            }
            Ok(Some(value))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(_) => Err("ワーカーの資格を読み込めませんでした".into()),
    }
}

fn write_credential(entry: &keyring::Entry, registration: &Registration) -> Result<(), String> {
    let value = Zeroizing::new(
        serde_json::to_string(registration).map_err(|_| "ワーカーの資格を保存できませんでした")?,
    );
    entry
        .set_password(&value)
        .map_err(|_| "ワーカーの資格を保存できませんでした".into())
}

fn expiry(seconds: u64) -> Result<u64, String> {
    if !(1..=MAX_TTL).contains(&seconds) {
        return Err("ワーカー資格の有効期限が不正です".into());
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|now| now.as_secs().checked_add(seconds))
        .ok_or_else(|| "ワーカー資格の有効期限を計算できませんでした".into())
}

async fn reply<T: DeserializeOwned>(mut response: Response) -> Result<T, String> {
    if !response.status().is_success() {
        return Err(format!(
            "ワーカーの要求が失敗しました（HTTP {}）",
            response.status().as_u16()
        ));
    }
    let mut body = Zeroizing::new(Vec::new());
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "仲介の応答を読み込めませんでした")?
    {
        if body.len() + chunk.len() > MAX_REPLY {
            return Err("仲介の応答が大きすぎます".into());
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| "仲介の応答が不正です".into())
}

fn require_main(window: &WebviewWindow) -> Result<(), String> {
    if window.label() != "main" {
        return Err("ワーカーの設定はメインウィンドウで操作してください".into());
    }
    Ok(())
}

async fn authenticate(app: &AppHandle, connection: &Connection) -> Result<Auth, String> {
    let auth = Auth::for_worker(app.state::<Backend>().client.clone(), connection.clone());
    let _ = auth.restore().await;
    if !auth.is_authenticated() {
        auth.login().await?;
    }
    Ok(auth)
}

fn status(connection: Connection, registration: Option<&Registration>) -> Status {
    Status {
        connection,
        supported: local::worker_platform_supported(),
        expires_at: registration.map(|value| value.expires_at),
        terms_version: TERMS_VERSION,
    }
}

#[tauri::command]
pub async fn worker_registration_status(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<Status, String> {
    require_main(&window)?;
    let connection = load(&app)?;
    let registration = if connection.server_url.is_empty() {
        None
    } else {
        credential(&vault(&connection)?)?
    };
    Ok(status(connection, registration.as_ref()))
}

#[tauri::command]
pub async fn register_worker(
    app: AppHandle,
    window: WebviewWindow,
    connection: Connection,
    terms_version: u32,
) -> Result<Status, String> {
    require_main(&window)?;
    let state = app.state::<RegistrationGate>();
    let _gate = state
        .0
        .try_lock()
        .map_err(|_| "ワーカーの設定を処理中です")?;
    if terms_version != TERMS_VERSION {
        return Err("参加の規約を確認して同意してください".into());
    }
    let previous = load(&app)?;
    if !previous.server_url.is_empty() && credential(&vault(&previous)?)?.is_some() {
        return Err("登録済みです。提供先を変えるには登録を解除してください".into());
    }
    let connection = connection.validated()?;
    let entry = vault(&connection)?;
    if credential(&entry)?.is_some() {
        return Err("提供先に保存済みのワーカー資格があります".into());
    }
    local::verify_worker_model(&app).await?;
    save(&app, &connection)?;
    let auth = authenticate(&app, &connection).await?;
    let issued: Issued = reply(
        auth.send(
            Method::POST,
            "/api/worker/register",
            Some(&json!({
                "terms_version":terms_version
            })),
        )
        .await?,
    )
    .await?;
    let worker_id = issued.worker_id;
    let stored = expiry(issued.expires_in).and_then(|expires_at| {
        let registration = Registration {
            worker_id: worker_id.clone(),
            secret: issued.secret.to_string(),
            expires_at,
        };
        if !registration.valid() {
            return Err("仲介から受け取ったワーカー資格が不正です".into());
        }
        write_credential(&entry, &registration)?;
        Ok(registration)
    });
    match stored {
        Ok(registration) => Ok(status(connection, Some(&registration))),
        Err(error) => {
            if valid_id(&worker_id, 32) {
                let route = format!("/api/worker/{worker_id}");
                if !auth
                    .send(Method::DELETE, &route, None)
                    .await
                    .is_ok_and(|r| r.status().is_success())
                {
                    return Err(format!("{error}。仲介での登録解除を確認できませんでした"));
                }
            }
            Err(error)
        }
    }
}

#[tauri::command]
pub async fn renew_worker(app: AppHandle, window: WebviewWindow) -> Result<Status, String> {
    require_main(&window)?;
    let state = app.state::<RegistrationGate>();
    let _gate = state
        .0
        .try_lock()
        .map_err(|_| "ワーカーの設定を処理中です")?;
    let connection = load(&app)?;
    let entry = vault(&connection)?;
    let mut registration = credential(&entry)?.ok_or("ワーカーが登録されていません")?;
    let auth = authenticate(&app, &connection).await?;
    let renewal: Renewal = reply(
        auth.send(
            Method::POST,
            "/api/worker/renew",
            Some(&json!({
                "worker_id":registration.worker_id
            })),
        )
        .await?,
    )
    .await?;
    registration.expires_at = expiry(renewal.expires_in)?;
    write_credential(&entry, &registration)?;
    Ok(status(connection, Some(&registration)))
}

#[tauri::command]
pub async fn revoke_worker(app: AppHandle, window: WebviewWindow) -> Result<Status, String> {
    require_main(&window)?;
    let state = app.state::<RegistrationGate>();
    let _gate = state
        .0
        .try_lock()
        .map_err(|_| "ワーカーの設定を処理中です")?;
    let connection = load(&app)?;
    let entry = vault(&connection)?;
    let registration = credential(&entry)?.ok_or("ワーカーが登録されていません")?;
    let auth = authenticate(&app, &connection).await?;
    let route = format!("/api/worker/{}", registration.worker_id);
    let response = auth.send(Method::DELETE, &route, None).await?;
    if !response.status().is_success() {
        return Err(format!(
            "登録を解除できませんでした（HTTP {}）",
            response.status().as_u16()
        ));
    }
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(status(connection, None)),
        Err(_) => Err("仲介で登録を解除しました。OSの資格の削除に失敗しました".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_metadata_omits_secrets() {
        let registration = Registration {
            worker_id: "a".repeat(32),
            secret: format!("ptw_{}", "b".repeat(64)),
            expires_at: 123,
        };
        assert!(registration.valid());
        let exposed =
            serde_json::to_value(status(Connection::default(), Some(&registration))).unwrap();
        assert_eq!(exposed["expires_at"], 123);
        assert!(exposed.get("worker_id").is_none());
        assert!(exposed.get("secret").is_none());
    }

    #[test]
    fn ttl_and_identifier_are_bounded() {
        assert!(expiry(0).is_err());
        assert!(expiry(MAX_TTL + 1).is_err());
        assert!(expiry(MAX_TTL).is_ok());
        assert!(!valid_id("../api", 6));
    }
}
