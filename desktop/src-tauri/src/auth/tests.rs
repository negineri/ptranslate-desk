//! 認証の試験はローカルの OIDC / API とメモリ上の保管だけを使う。
use super::*;
use serde_json::json;
use std::{collections::HashMap, sync::Mutex as SyncMutex};

#[derive(Default)]
struct MemoryVault(SyncMutex<Option<Secret>>);
impl Vault for MemoryVault {
    fn load(&self) -> Result<Option<Secret>, String> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn save(&self, value: &str) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(Zeroizing::new(value.into()));
        Ok(())
    }
    fn clear(&self) -> Result<(), String> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}

struct Model {
    client_id: String,
    code: Secret,
    access: Secret,
    refresh: Secret,
    challenge: String,
    redirect: String,
    refreshes: usize,
    revokes: usize,
    configs: usize,
    force_401: bool,
    always_401: bool,
    html_403: bool,
    reject_refresh: bool,
    bad_request: bool,
    no_revocation: bool,
}
fn jwt() -> Secret {
    let payload = URL_SAFE_NO_PAD.encode(json!({"preferred_username": random_secret().unwrap().as_str(), "aud": CLIENT_ID, "exp": 1}).to_string());
    Zeroizing::new(format!(
        "{}.{}.{}",
        random_secret().unwrap().as_str(),
        payload,
        random_secret().unwrap().as_str()
    ))
}
struct Fixture {
    url: String,
    state: Arc<SyncMutex<Model>>,
    vault: Arc<MemoryVault>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fixture {
    async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(SyncMutex::new(Model {
            client_id: CLIENT_ID.into(),
            code: random_secret().unwrap(),
            access: jwt(),
            refresh: random_secret().unwrap(),
            challenge: String::new(),
            redirect: String::new(),
            refreshes: 0,
            revokes: 0,
            configs: 0,
            force_401: false,
            always_401: false,
            html_403: false,
            reject_refresh: false,
            bad_request: false,
            no_revocation: false,
        }));
        let shared = state.clone();
        let base = url.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let shared = shared.clone();
                let base = base.clone();
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    while !bytes.ends_with(b"\r\n\r\n") && bytes.len() < 8192 {
                        let Ok(byte) = socket.read_u8().await else {
                            return;
                        };
                        bytes.push(byte);
                    }
                    let header = String::from_utf8_lossy(&bytes).into_owned();
                    let first: Vec<_> = header.lines().next().unwrap().split_whitespace().collect();
                    let headers: HashMap<_, _> = header
                        .lines()
                        .skip(1)
                        .filter_map(|line| line.split_once(':'))
                        .map(|(key, value)| (key.to_lowercase(), value.trim().to_string()))
                        .collect();
                    let length: usize = headers
                        .get("content-length")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(0);
                    let mut body = vec![0; length];
                    if socket.read_exact(&mut body).await.is_err() {
                        return;
                    }
                    let form: HashMap<String, String> = reqwest::Url::parse(&format!(
                        "http://localhost/?{}",
                        String::from_utf8_lossy(&body)
                    ))
                    .unwrap()
                    .query_pairs()
                    .into_owned()
                    .collect();
                    let (status, content_type, response, extra) = {
                        let mut model = shared.lock().unwrap();
                        let path = first[1];
                        if path == "/discovery" {
                            let mut discovery = json!({"issuer": base,
                                "authorization_endpoint": format!("{base}/custom-authorize"),
                                "token_endpoint": format!("{base}/custom-token"),
                                "revocation_endpoint": format!("{base}/custom-revoke")});
                            if model.no_revocation {
                                discovery
                                    .as_object_mut()
                                    .unwrap()
                                    .remove("revocation_endpoint");
                            }
                            (
                                200,
                                "application/json",
                                discovery.to_string(),
                                String::new(),
                            )
                        } else if path.starts_with("/custom-authorize?") {
                            let url = Url::parse(&format!("{base}{path}")).unwrap();
                            let params: HashMap<_, _> = url.query_pairs().into_owned().collect();
                            model.bad_request |= params.get("scope").map(String::as_str)
                                != Some(SCOPE)
                                || params.get("code_challenge_method").map(String::as_str)
                                    != Some("S256")
                                || params.get("client_id").map(String::as_str)
                                    != Some(model.client_id.as_str());
                            model.challenge = params["code_challenge"].clone();
                            model.redirect = params["redirect_uri"].clone();
                            let redirect = Url::parse(&model.redirect).unwrap();
                            model.bad_request |= redirect.host_str() != Some("127.0.0.1")
                                || redirect.path() != "/callback";
                            let mut callback = redirect;
                            callback
                                .query_pairs_mut()
                                .append_pair("code", &model.code)
                                .append_pair("state", &params["state"]);
                            (
                                302,
                                "text/plain",
                                String::new(),
                                format!("Location: {callback}\r\n"),
                            )
                        } else if path == "/custom-token" {
                            let refresh =
                                form.get("grant_type").map(String::as_str) == Some("refresh_token");
                            model.bad_request |= form.contains_key("client_secret")
                                || form.get("client_id").map(String::as_str)
                                    != Some(model.client_id.as_str());
                            let valid = if refresh {
                                model.refreshes += 1;
                                form.get("refresh_token")
                                    .is_some_and(|value| value == model.refresh.as_str())
                                    && !model.reject_refresh
                            } else {
                                form.get("grant_type").map(String::as_str)
                                    == Some("authorization_code")
                                    && form
                                        .get("code")
                                        .is_some_and(|value| value == model.code.as_str())
                                    && form.get("redirect_uri") == Some(&model.redirect)
                                    && form
                                        .get("code_verifier")
                                        .is_some_and(|value| challenge(value) == model.challenge)
                            };
                            if valid {
                                model.access = jwt();
                                model.refresh = random_secret().unwrap();
                                (200, "application/json", json!({"access_token": model.access.as_str(),
                                    "refresh_token": model.refresh.as_str(), "expires_in": 600, "token_type": "Bearer"}).to_string(), String::new())
                            } else {
                                (
                                    400,
                                    "application/json",
                                    json!({"error":"invalid_grant"}).to_string(),
                                    String::new(),
                                )
                            }
                        } else if path == "/custom-revoke" {
                            model.revokes += 1;
                            model.bad_request |= form.get("client_id").map(String::as_str)
                                != Some(model.client_id.as_str());
                            model.bad_request |= form
                                .get("token")
                                .is_none_or(|value| value != model.refresh.as_str());
                            (200, "application/json", "{}".into(), String::new())
                        } else if path == "/api/config" {
                            model.configs += 1;
                            model.bad_request |= headers.contains_key("x-forwarded-user");
                            let authorized = headers.get("authorization").is_some_and(|value| {
                                value == &format!("Bearer {}", model.access.as_str())
                            });
                            if model.html_403 {
                                (
                                    403,
                                    "text/html",
                                    "<html>Forbidden</html>".into(),
                                    String::new(),
                                )
                            } else if model.always_401 || model.force_401 || !authorized {
                                model.force_401 = false;
                                (
                                    401,
                                    "application/json",
                                    json!({"error":"ログインしていません"}).to_string(),
                                    String::new(),
                                )
                            } else {
                                (
                                    200,
                                    "application/json",
                                    json!({"user":"fixture"}).to_string(),
                                    String::new(),
                                )
                            }
                        } else {
                            (404, "text/plain", String::new(), String::new())
                        }
                    };
                    let response = format!("HTTP/1.1 {status} Test\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{response}", response.len());
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        Self {
            url,
            state,
            vault: Arc::new(MemoryVault::default()),
            task,
        }
    }
    fn auth(&self) -> Auth {
        let mut auth = Auth::new(
            Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
        );
        auth.mock = false;
        auth.server = self.url.clone();
        auth.discovery_url = format!("{}/discovery", self.url);
        auth.vault = self.vault.clone();
        auth.client_id = self.state.lock().unwrap().client_id.clone();
        auth
    }
    async fn login(&self, auth: &Auth) {
        auth.login_with(|url| {
            let url = url.clone();
            tokio::spawn(async move {
                let client = Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .unwrap();
                let response = client.get(url).send().await.unwrap();
                let callback = response.headers()["location"].to_str().unwrap();
                let _ = client.get(callback).send().await.unwrap().bytes().await;
            });
            Ok(())
        })
        .await
        .unwrap();
        assert!(!self.state.lock().unwrap().bad_request);
    }
}

#[test]
fn callback_rejects_wrong_state_paths_and_duplicates() {
    let state = random_secret().unwrap();
    let code = random_secret().unwrap();
    let state = state.as_str();
    let code = code.as_str();
    assert!(
        valid_callback(&format!("/callback?state={state}&code={code}"), &state)
            .unwrap()
            .is_some()
    );
    for target in [
        format!("/callback?state=wrong&code={code}"),
        format!("/other?state={state}&code={code}"),
        format!("/callback?state={state}&state={state}&code={code}"),
        format!("/callback?state={state}&code={code}&code={code}"),
    ] {
        assert!(valid_callback(&target, &state).unwrap().is_none());
    }
    assert!(valid_callback(
        &format!("/callback?state={state}&error=access_denied"),
        &state
    )
    .is_err());
    assert_eq!(challenge(&state).len(), 43);
    assert!(validate_endpoint("http://example.com/token").is_err());
    assert!(validate_endpoint("https://user:password@example.com/token").is_err());
}

#[tokio::test]
async fn unconfigured_connection_does_not_use_credentials_or_open_a_browser() {
    let mut auth = Auth::new(Client::new());
    auth.mock = false;
    let vault = Arc::new(MemoryVault::default());
    vault.save("dummy-refresh").unwrap();
    auth.vault = vault.clone();
    auth.restore().await.unwrap();
    assert!(!auth.is_authenticated());
    let error = auth
        .login_with(|_| panic!("接続先未設定でブラウザを開いてはいけません"))
        .await
        .unwrap_err();
    assert!(error.contains("設定"));
    assert!(!auth.is_busy());
    auth.logout().await.unwrap();
    assert_eq!(
        vault.load().unwrap().as_deref().map(|v| v.as_str()),
        Some("dummy-refresh")
    );
}

#[tokio::test]
async fn login_restore_rotation_and_revoke_use_discovery_and_secure_storage() {
    let fixture = Fixture::new().await;
    let auth = fixture.auth();
    fixture.login(&auth).await;
    assert!(auth.verify().await.unwrap().verified);
    assert!(fixture.vault.load().unwrap().is_some());
    let restored = fixture.auth();
    restored.restore().await.unwrap();
    assert!(restored.is_authenticated());
    assert!(restored.verify().await.unwrap().verified);
    assert_eq!(fixture.state.lock().unwrap().refreshes, 1);
    restored.logout().await.unwrap();
    assert!(!restored.is_authenticated());
    assert!(fixture.vault.load().unwrap().is_none());
    let model = fixture.state.lock().unwrap();
    assert_eq!(model.revokes, 1);
    assert!(!model.bad_request);
}

#[tokio::test]
async fn login_can_refresh_and_revoke_when_os_storage_rejects_writes() {
    struct ReadOnlyVault;
    impl Vault for ReadOnlyVault {
        fn load(&self) -> Result<Option<Secret>, String> {
            Ok(None)
        }
        fn save(&self, _: &str) -> Result<(), String> {
            Err("書き込み不可".into())
        }
        fn clear(&self) -> Result<(), String> {
            Ok(())
        }
    }

    let fixture = Fixture::new().await;
    let mut auth = fixture.auth();
    auth.vault = Arc::new(ReadOnlyVault);
    fixture.login(&auth).await;
    assert!(auth.session_only());
    assert!(auth.verify().await.unwrap().verified);
    {
        let mut credentials = auth.credentials.lock().await;
        auth.refresh_locked(&mut credentials).await.unwrap();
    }
    assert!(auth.session_only());
    auth.logout().await.unwrap();
    assert!(!auth.is_authenticated());
    assert!(auth.credentials.lock().await.session_refresh.is_none());
    let model = fixture.state.lock().unwrap();
    assert_eq!(model.refreshes, 1);
    assert_eq!(model.revokes, 1);
}

#[tokio::test]
async fn discovery_without_revocation_logs_out_locally() {
    let fixture = Fixture::new().await;
    fixture.state.lock().unwrap().no_revocation = true;
    let auth = fixture.auth();
    fixture.login(&auth).await;
    assert!(auth.verify().await.unwrap().verified);
    let restored = fixture.auth();
    restored.restore().await.unwrap();
    assert!(restored.is_authenticated());
    restored.logout().await.unwrap();
    assert!(!restored.is_authenticated());
    assert!(fixture.vault.load().unwrap().is_none());
    let model = fixture.state.lock().unwrap();
    assert_eq!(model.revokes, 0);
    assert!(!model.bad_request);
}

#[tokio::test]
async fn refresh_before_expiry_and_after_401_retries_once() {
    let fixture = Fixture::new().await;
    let auth = fixture.auth();
    fixture.login(&auth).await;
    auth.credentials
        .lock()
        .await
        .access
        .as_mut()
        .unwrap()
        .expires = Instant::now() + Duration::from_secs(59);
    assert!(auth.verify().await.unwrap().verified);
    fixture.state.lock().unwrap().force_401 = true;
    assert!(auth.verify().await.unwrap().verified);
    assert_eq!(fixture.state.lock().unwrap().refreshes, 2);
    fixture.state.lock().unwrap().reject_refresh = true;
    fixture.state.lock().unwrap().force_401 = true;
    assert!(auth.verify().await.is_err());
    assert!(!auth.is_authenticated());
    assert!(fixture.vault.load().unwrap().is_none());
}

#[tokio::test]
async fn html_403_and_repeated_401_require_login_without_decoding_html() {
    for status in [401, 403] {
        let fixture = Fixture::new().await;
        let auth = fixture.auth();
        fixture.login(&auth).await;
        if status == 401 {
            fixture.state.lock().unwrap().always_401 = true;
        } else {
            fixture.state.lock().unwrap().html_403 = true;
        }
        let report = auth.verify().await.unwrap();
        assert_eq!(report.api_status, status);
        assert!(!report.verified);
        assert!(!auth.is_authenticated());
        assert!(fixture.vault.load().unwrap().is_none());
        assert_eq!(
            report
                .claim_names
                .contains(&"preferred_username".to_string()),
            status == 401
        );
        assert_eq!(
            fixture.state.lock().unwrap().configs,
            if status == 401 { 2 } else { 1 }
        );
    }
}

#[tokio::test]
async fn cancel_closes_loopback_and_never_installs_a_session() {
    let fixture = Fixture::new().await;
    let auth = fixture.auth();
    let result = auth
        .login_with(|_| {
            auth.cancel_login();
            Ok(())
        })
        .await;
    assert!(result.is_err());
    assert!(!auth.is_authenticated());
    assert!(!auth.is_busy());
    assert!(fixture.vault.load().unwrap().is_none());
}

#[tokio::test]
async fn custom_client_configuration_is_used_for_login_refresh_and_revoke() {
    let fixture = Fixture::new().await;
    fixture.state.lock().unwrap().client_id = "self-hosted-desktop".into();
    let auth = fixture.auth();
    fixture.login(&auth).await;
    assert!(auth.verify().await.unwrap().verified);
    let restored = fixture.auth();
    restored.restore().await.unwrap();
    restored.logout().await.unwrap();
    let state = fixture.state.lock().unwrap();
    assert!(!state.bad_request);
    assert_eq!(state.refreshes, 1);
    assert_eq!(state.revokes, 1);
}

#[tokio::test]
async fn connection_change_rejects_login_and_releases_lock_after_failure() {
    let fixture = Fixture::new().await;
    let auth = fixture.auth();
    let guard = auth.begin_connection_change().unwrap();
    assert!(auth.begin_connection_change().is_err());
    assert!(auth
        .login_with(|_| panic!("設定変更中はブラウザーを開かない"))
        .await
        .is_err());
    drop(guard);
    assert!(!auth.is_busy());
    fixture.login(&auth).await;
    assert!(auth.is_authenticated());
}
