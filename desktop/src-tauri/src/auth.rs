//! 公開クライアントの PKCE 認証。資格情報を画面・ファイル・ログへ渡さない。
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use reqwest::{Client, Method, Response, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{watch, Mutex},
};
use zeroize::Zeroizing;

pub const SERVER: &str = "https://translate.witenp.com";
pub const DISCOVERY: &str =
    "https://atk01.witenp.com/application/o/translator-desktop/.well-known/openid-configuration";
pub const CLIENT_ID: &str = "translator-desktop";
const SCOPE: &str = "openid profile offline_access";
const LOGIN_REQUIRED: &str = "ログインし直してください";

type Secret = Zeroizing<String>;

trait Vault: Send + Sync {
    fn load(&self) -> Result<Option<Secret>, String>;
    fn save(&self, value: &str) -> Result<(), String>;
    fn clear(&self) -> Result<(), String>;
}

struct OsVault {
    account: String,
}
impl OsVault {
    fn entry(&self) -> Result<keyring::Entry, String> {
        keyring::Entry::new("com.witenp.ptranslate-desk", &self.account)
            .map_err(|_| "OS の資格情報の保管を開けませんでした".into())
    }
}
impl Vault for OsVault {
    fn load(&self) -> Result<Option<Secret>, String> {
        match self.entry()?.get_password() {
            Ok(value) => Ok(Some(Zeroizing::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err("OS の資格情報を読み込めませんでした".into()),
        }
    }
    fn save(&self, value: &str) -> Result<(), String> {
        self.entry()?
            .set_password(value)
            .map_err(|_| "OS の資格情報に保存できませんでした".into())
    }
    fn clear(&self) -> Result<(), String> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err("OS の資格情報を削除できませんでした".into()),
        }
    }
}

#[derive(Clone, Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    // RFC 7009 の revoke を持たない IdP (Pocket ID など) もある。無ければログアウトは端末から消すだけ。
    #[serde(default)]
    revocation_endpoint: Option<String>,
}

#[derive(Deserialize)]
struct TokenReply {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
    token_type: String,
}

struct Access {
    value: Secret,
    expires: Instant,
}
#[derive(Default)]
struct Credentials {
    access: Option<Access>,
    discovery: Option<Discovery>,
    initialized: bool,
}

pub struct Auth {
    client: Client,
    server: String,
    discovery_url: String,
    client_id: String,
    vault: Arc<dyn Vault>,
    credentials: Mutex<Credentials>,
    cancellation: watch::Sender<u64>,
    authenticated: AtomicBool,
    busy: AtomicBool,
    mock: bool,
    changed: tokio::sync::Notify,
}

#[derive(Clone, Default, Serialize)]
pub struct LoginReport {
    pub api_status: u16,
    pub verified: bool,
    pub claim_names: Vec<String>,
}

pub struct ConnectionChange<'a>(&'a Auth);
impl Drop for ConnectionChange<'_> {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::SeqCst);
    }
}

impl Auth {
    #[cfg(test)]
    pub fn new(client: Client) -> Self {
        Self::with_connection(client, crate::connection::Connection::default())
    }
    pub fn with_connection(client: Client, connection: crate::connection::Connection) -> Self {
        // モックでも本番へのヘッダー偽装はできない。接続先はループバック固定。
        let mock = std::env::var("PTRANSLATE_MOCK").as_deref() == Ok("1");
        Self {
            client,
            server: if mock {
                "http://127.0.0.1:8002"
            } else {
                &connection.server_url
            }
            .into(),
            discovery_url: connection.discovery_url.clone(),
            client_id: connection.client_id.clone(),
            vault: Arc::new(OsVault {
                account: connection.vault_account(),
            }),
            credentials: Mutex::new(Credentials::default()),
            cancellation: watch::channel(0).0,
            authenticated: AtomicBool::new(false),
            busy: AtomicBool::new(false),
            mock,
            changed: tokio::sync::Notify::new(),
        }
    }
    pub fn is_mock(&self) -> bool {
        self.mock
    }
    pub fn is_authenticated(&self) -> bool {
        self.authenticated.load(Ordering::SeqCst)
    }
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    pub fn begin_connection_change(&self) -> Result<ConnectionChange<'_>, String> {
        if self.mock {
            return Err("モック実行中は接続先を変更できません".into());
        }
        self.busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| "ログイン処理が終わるか、中止してから接続先を変更してください")?;
        Ok(ConnectionChange(self))
    }

    async fn discovery(&self, credentials: &mut Credentials) -> Result<Discovery, String> {
        if let Some(discovery) = &credentials.discovery {
            return Ok(discovery.clone());
        }
        let response = self
            .client
            .get(&self.discovery_url)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|_| "ログインサーバーに接続できませんでした")?;
        if !response.status().is_success() {
            return Err("ログインの設定を取得できませんでした".into());
        }
        let discovery: Discovery = response
            .json()
            .await
            .map_err(|_| "ログインの設定が不正です")?;
        for endpoint in [
            &discovery.issuer,
            &discovery.authorization_endpoint,
            &discovery.token_endpoint,
        ]
        .into_iter()
        .chain(&discovery.revocation_endpoint)
        {
            validate_endpoint(endpoint)?;
        }
        let issuer = Url::parse(&discovery.issuer).map_err(|_| "ログインの設定が不正です")?;
        let source = Url::parse(&self.discovery_url).map_err(|_| "ログインの設定が不正です")?;
        if issuer.origin() != source.origin() {
            return Err("ログインの発行元が一致しません".into());
        }
        credentials.discovery = Some(discovery.clone());
        Ok(discovery)
    }

    async fn exchange(&self, endpoint: &str, form: &[(&str, &str)]) -> Result<TokenReply, String> {
        let response = self
            .client
            .post(endpoint)
            .form(form)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|_| "ログインサーバーに接続できませんでした")?;
        if !response.status().is_success() {
            return Err(LOGIN_REQUIRED.into());
        }
        let reply: TokenReply = response
            .json()
            .await
            .map_err(|_| "ログインの応答が不正です")?;
        if reply.access_token.is_empty()
            || !reply.token_type.eq_ignore_ascii_case("bearer")
            || reply.expires_in <= 60
        {
            return Err("ログインの応答が不正です".into());
        }
        Ok(reply)
    }

    fn install(
        &self,
        credentials: &mut Credentials,
        reply: TokenReply,
        previous: Option<&str>,
    ) -> Result<(), String> {
        let refresh = reply.refresh_token.map(Zeroizing::new);
        let refresh = refresh
            .as_deref()
            .map(|s| s.as_str())
            .or(previous)
            .filter(|value| !value.is_empty())
            .ok_or("ログインの応答に更新用の資格情報がありません")?;
        let expires = Instant::now()
            .checked_add(Duration::from_secs(reply.expires_in))
            .ok_or("ログインの有効期限が不正です")?;
        self.vault.save(refresh)?;
        credentials.access = Some(Access {
            value: Zeroizing::new(reply.access_token),
            expires,
        });
        self.authenticated.store(true, Ordering::SeqCst);
        self.changed.notify_one();
        Ok(())
    }

    async fn refresh_locked(&self, credentials: &mut Credentials) -> Result<(), String> {
        let epoch = *self.cancellation.borrow();
        let previous = self.vault.load()?.ok_or(LOGIN_REQUIRED)?;
        let discovery = self.discovery(credentials).await?;
        let reply = self
            .exchange(
                &discovery.token_endpoint,
                &[
                    ("grant_type", "refresh_token"),
                    ("client_id", self.client_id.as_str()),
                    ("refresh_token", &previous),
                ],
            )
            .await?;
        if *self.cancellation.borrow() != epoch {
            return Err("ログインを中止しました".into());
        }
        self.install(credentials, reply, Some(&previous))
    }

    fn forget(&self, credentials: &mut Credentials) -> Result<(), String> {
        self.authenticated.store(false, Ordering::SeqCst);
        self.changed.notify_one();
        credentials.access = None;
        if self.mock {
            Ok(())
        } else {
            self.vault.clear()
        }
    }

    pub async fn restore(&self) -> Result<(), String> {
        let mut credentials = self.credentials.lock().await;
        if credentials.initialized {
            return Ok(());
        }
        credentials.initialized = true;
        if self.mock || self.vault.load()?.is_none() {
            return Ok(());
        }
        if let Err(error) = self.refresh_locked(&mut credentials).await {
            self.forget(&mut credentials)?;
            return Err(error);
        }
        Ok(())
    }

    pub async fn login(&self) -> Result<(), String> {
        self.login_with(open_browser).await
    }

    async fn login_with(
        &self,
        open: impl FnOnce(&Url) -> Result<(), String>,
    ) -> Result<(), String> {
        if self.busy.swap(true, Ordering::SeqCst) {
            return Err("ログインを待っています".into());
        }
        struct Busy<'a>(&'a AtomicBool);
        impl Drop for Busy<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _busy = Busy(&self.busy);
        if self.mock {
            self.authenticated.store(true, Ordering::SeqCst);
            return Ok(());
        }
        let mut canceled = self.cancellation.subscribe();
        let discovery = { self.discovery(&mut *self.credentials.lock().await).await? };
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| "ログイン用の待ち受けを開始できませんでした")?;
        let redirect = format!(
            "http://127.0.0.1:{}/callback",
            listener
                .local_addr()
                .map_err(|_| "ログインの待ち受けが不正です")?
                .port()
        );
        let verifier = random_secret()?;
        let state = random_secret()?;
        let mut authorize = Url::parse(&discovery.authorization_endpoint)
            .map_err(|_| "ログインの設定が不正です")?;
        authorize.query_pairs_mut().extend_pairs([
            ("client_id", self.client_id.as_str()),
            ("response_type", "code"),
            ("scope", SCOPE),
            ("redirect_uri", redirect.as_str()),
            ("state", state.as_str()),
            ("code_challenge", challenge(&verifier).as_str()),
            ("code_challenge_method", "S256"),
        ]);
        if canceled.has_changed().unwrap_or(true) {
            return Err("ログインを中止しました".into());
        }
        open(&authorize)?;
        let (code, mut browser) = tokio::select! {
            callback = tokio::time::timeout(Duration::from_secs(180), receive_callback(listener, &state)) =>
                callback.map_err(|_| "ログインがタイムアウトしました")??,
            _ = canceled.changed() => return Err("ログインを中止しました".into()),
        };
        let reply = self
            .exchange(
                &discovery.token_endpoint,
                &[
                    ("grant_type", "authorization_code"),
                    ("client_id", self.client_id.as_str()),
                    ("code", &code),
                    ("redirect_uri", &redirect),
                    ("code_verifier", &verifier),
                ],
            )
            .await;
        let result = async {
            let reply = reply?;
            let mut credentials = self.credentials.lock().await;
            if canceled.has_changed().unwrap_or(true) {
                return Err("ログインを中止しました".into());
            }
            self.install(&mut credentials, reply, None)
        }
        .await;
        let message = if result.is_ok() {
            "ログインしました。このタブは閉じて構いません"
        } else {
            "ログインできませんでした。アプリに戻って確認してください"
        };
        let _ = browser_reply(&mut browser, "200 OK", message).await;
        result
    }

    pub fn cancel_login(&self) {
        self.cancellation.send_modify(|epoch| *epoch += 1);
    }

    pub async fn logout(&self) -> Result<(), String> {
        self.cancel_login();
        self.authenticated.store(false, Ordering::SeqCst);
        if self.mock {
            return Ok(());
        }
        let mut credentials = self.credentials.lock().await;
        // 更新処理が終わってから最新のrefresh tokenを読み、再認証も止める。
        self.authenticated.store(false, Ordering::SeqCst);
        credentials.initialized = true;
        let refresh = self.vault.load();
        credentials.access = None;
        let cleared = self.vault.clear();
        let refresh = refresh?;
        cleared?;
        if let Some(refresh) = refresh {
            let discovery = self.discovery(&mut credentials).await?;
            let Some(revocation_endpoint) = &discovery.revocation_endpoint else {
                return Ok(());
            };
            let response = self
                .client
                .post(revocation_endpoint)
                .form(&[
                    ("client_id", self.client_id.as_str()),
                    ("token", refresh.as_str()),
                    ("token_type_hint", "refresh_token"),
                ])
                .timeout(Duration::from_secs(15))
                .send()
                .await
                .map_err(|_| {
                    "端末からログアウトしましたが、サーバーでの無効化を確認できませんでした"
                })?;
            if !response.status().is_success() {
                return Err(
                    "端末からログアウトしましたが、サーバーでの無効化を確認できませんでした".into(),
                );
            }
        }
        Ok(())
    }

    pub async fn renew_if_needed(&self) -> Result<(), String> {
        if self.mock || !self.is_authenticated() {
            return Ok(());
        }
        self.access_token(None).await.map(|_| ())
    }

    pub async fn wait_for_refresh(&self) {
        let delay = {
            let credentials = self.credentials.lock().await;
            credentials
                .access
                .as_ref()
                .map(|access| {
                    access
                        .expires
                        .saturating_duration_since(Instant::now())
                        .saturating_sub(Duration::from_secs(60))
                })
                .unwrap_or(Duration::from_secs(30))
        };
        tokio::select! {
            _ = tokio::time::sleep(delay) => {},
            _ = self.changed.notified() => {},
        }
    }

    async fn access_token(&self, rejected: Option<&str>) -> Result<Secret, String> {
        let mut credentials = self.credentials.lock().await;
        if !self.is_authenticated() {
            return Err(LOGIN_REQUIRED.into());
        }
        let refresh = credentials.access.as_ref().is_none_or(|access| {
            access.expires.saturating_duration_since(Instant::now()) <= Duration::from_secs(60)
                || rejected.is_some_and(|rejected| rejected == access.value.as_str())
        });
        if refresh {
            if let Err(error) = self.refresh_locked(&mut credentials).await {
                self.forget(&mut credentials)?;
                return Err(error);
            }
        }
        Ok(credentials
            .access
            .as_ref()
            .ok_or(LOGIN_REQUIRED)?
            .value
            .clone())
    }

    pub async fn invalidate(&self) -> Result<(), String> {
        let mut credentials = self.credentials.lock().await;
        self.forget(&mut credentials)
    }

    pub async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Response, String> {
        if !self.is_authenticated() {
            return Err(LOGIN_REQUIRED.into());
        }
        let token = if self.mock {
            None
        } else {
            Some(self.access_token(None).await?)
        };
        let send = |token: Option<&Secret>| {
            let mut request = self
                .client
                .request(method.clone(), format!("{}{path}", self.server));
            if let Some(token) = token {
                request = request.bearer_auth(token.as_str());
            }
            if self.mock {
                request = request.header("X-Forwarded-User", "mock-desktop");
            }
            if let Some(body) = body {
                request = request.json(body);
            }
            request.send()
        };
        let response = send(token.as_ref())
            .await
            .map_err(|_| "翻訳サーバーに接続できませんでした")?;
        if response.status() == 401 && !self.mock {
            drop(response);
            let fresh = self
                .access_token(token.as_deref().map(|value| value.as_str()))
                .await?;
            return send(Some(&fresh))
                .await
                .map_err(|_| "翻訳サーバーに接続できませんでした".into());
        }
        Ok(response)
    }

    pub async fn verify(&self) -> Result<LoginReport, String> {
        let response = self.send(Method::GET, "/api/config", None).await?;
        let status = response.status().as_u16();
        let claims = {
            let credentials = self.credentials.lock().await;
            credentials
                .access
                .as_ref()
                .map(|a| claim_names(&a.value))
                .unwrap_or_default()
        };
        if status == 401 || status == 403 {
            self.invalidate().await?;
            return Ok(LoginReport {
                api_status: status,
                verified: false,
                claim_names: if status == 401 { claims } else { vec![] },
            });
        }
        if status != 200 {
            return Err("ログイン後の利用者確認に失敗しました".into());
        }
        let config: Value = response
            .json()
            .await
            .map_err(|_| "利用者確認の応答が不正です")?;
        if !config
            .get("user")
            .and_then(Value::as_str)
            .is_some_and(|user| !user.is_empty())
        {
            return Err("ログイン後の利用者名を確認できませんでした".into());
        }
        Ok(LoginReport {
            api_status: status,
            verified: true,
            claim_names: vec![],
        })
    }
}

fn random_secret() -> Result<Secret, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "ログイン用の乱数を生成できませんでした")?;
    Ok(Zeroizing::new(URL_SAFE_NO_PAD.encode(bytes)))
}
fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn validate_endpoint(endpoint: &str) -> Result<(), String> {
    let url = Url::parse(endpoint).map_err(|_| "ログインの URL が不正です")?;
    let secure = url.scheme() == "https";
    #[cfg(test)]
    let secure = secure || (url.scheme() == "http" && url.host_str() == Some("127.0.0.1"));
    if !secure
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("ログインの URL は安全な HTTPS が必要です".into());
    }
    Ok(())
}

fn claim_names(token: &str) -> Vec<String> {
    // 診断専用。JWT の値を本人確認やログ出力には使わない。
    let Some(payload) = token.split('.').nth(1).filter(|p| p.len() < 65536) else {
        return vec![];
    };
    let Ok(bytes) = URL_SAFE_NO_PAD.decode(payload) else {
        return vec![];
    };
    let Ok(Value::Object(claims)) = serde_json::from_slice::<Value>(&bytes) else {
        return vec![];
    };
    claims.into_iter().map(|(key, _)| key).collect()
}

fn open_browser(url: &Url) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url.as_str()).spawn();
    #[cfg(target_os = "linux")]
    let result = std::process::Command::new("xdg-open")
        .arg(url.as_str())
        .spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", url.as_str()])
        .spawn();
    result
        .map(|_| ())
        .map_err(|_| "ブラウザを開けませんでした".into())
}

fn valid_callback(target: &str, expected: &str) -> Result<Option<Secret>, String> {
    let url =
        Url::parse(&format!("http://127.0.0.1{target}")).map_err(|_| "コールバックが不正です")?;
    if url.path() != "/callback" || !target.starts_with("/callback?") {
        return Ok(None);
    }
    let one = |name: &str| {
        let values: Vec<_> = url
            .query_pairs()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
            .collect();
        if values.len() == 1 {
            values.into_iter().next()
        } else {
            None
        }
    };
    let Some(state) = one("state") else {
        return Ok(None);
    };
    if state.len() != expected.len()
        || state
            .bytes()
            .zip(expected.bytes())
            .fold(0, |diff, (a, b)| diff | a ^ b)
            != 0
    {
        return Ok(None);
    }
    if one("error").is_some() {
        return Err("ログインが許可されなかったか、中止されました".into());
    }
    Ok(one("code")
        .filter(|code| !code.is_empty())
        .map(Zeroizing::new))
}

async fn receive_callback(
    listener: TcpListener,
    state: &str,
) -> Result<(Secret, TcpStream), String> {
    loop {
        let (mut browser, _) = listener
            .accept()
            .await
            .map_err(|_| "ログインの応答を受信できませんでした")?;
        let read = async {
            let mut header = Vec::new();
            while header.len() < 8192 {
                let byte = browser
                    .read_u8()
                    .await
                    .map_err(|_| "ログインの応答を読めませんでした")?;
                header.push(byte);
                if header.ends_with(b"\r\n\r\n") {
                    return Ok(header);
                }
            }
            Err("ログインの応答が長すぎます")
        };
        let Ok(Ok(header)) = tokio::time::timeout(Duration::from_secs(3), read).await else {
            continue;
        };
        let request = String::from_utf8_lossy(&header);
        let mut first = request.lines().next().unwrap_or("").split_whitespace();
        let method = first.next().unwrap_or("");
        let target = first.next().unwrap_or("");
        let result = if method == "GET" {
            valid_callback(target, state)
        } else {
            Ok(None)
        };
        match result {
            Ok(Some(code)) => return Ok((code, browser)),
            Err(error) => {
                let _ = browser_reply(
                    &mut browser,
                    "400 Bad Request",
                    "ログインを完了できませんでした",
                )
                .await;
                return Err(error);
            }
            Ok(None) => {
                let _ = browser_reply(
                    &mut browser,
                    "400 Bad Request",
                    "このログインの応答は受け付けられません",
                )
                .await;
            }
        }
    }
}

async fn browser_reply(
    browser: &mut TcpStream,
    status: &str,
    message: &str,
) -> std::io::Result<()> {
    let body = format!("<!doctype html><html lang=\"ja\"><meta charset=\"utf-8\"><title>PTranslate Desk</title><p>{message}</p></html>");
    let response = format!("HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}", body.len());
    tokio::time::timeout(
        Duration::from_secs(3),
        browser.write_all(response.as_bytes()),
    )
    .await??;
    Ok(())
}

#[cfg(test)]
mod tests;
