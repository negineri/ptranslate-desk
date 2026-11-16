//! 所有者の OIDC 認可 (デバイスコード) と、ワーカー資格の登録・更新。
use crate::{
    http::{api, https_url, required_token, Body, Failure, Result},
    state::{State, StateDir},
};
use reqwest::{Client, Url};
use serde_json::{json, Map, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_TTL: u64 = 30 * 86400;
const DAY: u64 = 86400;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub struct Configuration {
    pub broker: String,
    pub client_id: String,
    pub token_url: String,
    pub device_url: String,
    pub origin: (String, u16),
}

/// 提供先の `GET /api/client-config` と IdP の discovery から、認可の接続先を決める。
pub async fn configuration(client: &Client, broker: &str) -> Result<Configuration> {
    https_url(broker, true)?;
    let broker = broker.trim_end_matches('/').to_owned();
    let config = api(
        client,
        &format!("{broker}/api/client-config"),
        Body::None,
        None,
    )
    .await?;
    let discovery = config
        .get("discovery_url")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let origin = https_url(discovery, false)?;
    let document = api(client, discovery, Body::None, None).await?;
    let text = |name: &str| {
        document
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let token_url = text("token_endpoint");
    let mut device_url = text("device_authorization_endpoint");
    if device_url.is_empty() {
        // Authentik の Device Authorization は発行元と同じオリジンの固定経路。
        device_url = Url::parse(discovery)
            .and_then(|url| url.join("/application/o/device/"))
            .map_err(|_| Failure::new("IdP の設定が不正です"))?
            .to_string();
    }
    for url in [text("issuer"), token_url.clone(), device_url.clone()] {
        if https_url(&url, false)? != origin {
            return Err(Failure::new("IdP の発行元が一致しません"));
        }
    }
    let client_id = config
        .get("client_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if client_id.is_empty() || client_id.len() > 256 {
        return Err(Failure::new("公開クライアントの設定が不正です"));
    }
    Ok(Configuration {
        broker,
        client_id: client_id.to_owned(),
        token_url,
        device_url,
        origin,
    })
}

/// デバイスコードで所有者の認可を待ち、トークンの応答を返す。
async fn device_authorization(
    client: &Client,
    config: &Configuration,
    show: &mut impl FnMut(&str, &str),
) -> Result<Map<String, Value>> {
    let started = api(
        client,
        &config.device_url,
        Body::Form(&[
            ("client_id", &config.client_id),
            ("scope", "openid profile offline_access"),
        ]),
        None,
    )
    .await?;
    let verification = started
        .get("verification_uri")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if https_url(verification, false)? != config.origin {
        return Err(Failure::new("認可画面の発行元が一致しません"));
    }
    let code = required_token(&started, "user_code")?;
    let device_code = required_token(&started, "device_code")?;
    let mut interval = started.get("interval").map_or(Some(5), Value::as_u64);
    let duration = started.get("expires_in").and_then(Value::as_u64);
    let (Some(mut interval), Some(duration)) = (interval.take(), duration) else {
        return Err(Failure::new("認可の待ち時間が不正です"));
    };
    if interval == 0 || duration == 0 {
        return Err(Failure::new("認可の待ち時間が不正です"));
    }
    show(verification, &code);
    let deadline = std::time::Instant::now() + Duration::from_secs(duration.min(1800));
    while std::time::Instant::now() + Duration::from_secs(interval) < deadline {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        match api(
            client,
            &config.token_url,
            Body::Form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", &config.client_id),
                ("device_code", &device_code),
            ]),
            None,
        )
        .await
        {
            Ok(tokens) => return Ok(tokens),
            Err(e) if e.oauth_error == "slow_down" => interval += 5,
            Err(e) if e.oauth_error == "authorization_pending" => {}
            Err(e) => return Err(e),
        }
    }
    Err(Failure::new("初回認可の待ち時間を過ぎました"))
}

fn update_expiry(state: &mut State, response: &Map<String, Value>) -> Result<()> {
    let ttl = response
        .get("expires_in")
        .and_then(Value::as_u64)
        .filter(|ttl| (1..=MAX_TTL).contains(ttl))
        .ok_or_else(|| Failure::new("ワーカー資格の有効期間が不正です"))?;
    let now = now();
    state.expires_at = now + ttl;
    state.renew_at = now + DAY.min((ttl / 2).max(1));
    Ok(())
}

async fn renew(client: &Client, state: &mut State, access: &str) -> Result<()> {
    let response = api(
        client,
        &format!("{}/api/worker/renew", state.broker),
        Body::Json(&json!({"worker_id": state.worker_id})),
        Some(access),
    )
    .await?;
    update_expiry(state, &response)
}

/// 初回認可または再認可。既存のワーカー資格があれば更新し、なければ登録する。
pub async fn authorize(
    client: &Client,
    dir: &StateDir,
    server: &str,
    mut show: impl FnMut(&str, &str),
) -> Result<()> {
    let previous = dir.load()?;
    let config = configuration(client, server).await?;
    if !previous.broker.is_empty() && previous.broker != config.broker {
        return Err(Failure::new(
            "別の提供先には別の状態ディレクトリを使ってください",
        ));
    }
    let tokens = device_authorization(client, &config, &mut show).await?;
    let access = required_token(&tokens, "access_token")?;
    let mut state = State::default();
    state.broker = config.broker;
    state.client_id = config.client_id;
    state.token_url = config.token_url;
    state.refresh_token = Some(required_token(&tokens, "refresh_token")?);
    if let (Some(id), Some(secret)) = (&previous.worker_id, &previous.worker_secret) {
        state.worker_id = Some(id.clone());
        state.worker_secret = Some(secret.clone());
        if let Err(e) = renew(client, &mut state, &access).await {
            if e.status != 404 {
                return Err(e);
            }
            state.worker_id = None;
            state.worker_secret = None;
        }
    }
    if state.worker_id.is_none() {
        let response = api(
            client,
            &format!("{}/api/worker/register", state.broker),
            Body::Json(&json!({"terms_version": 1})),
            Some(&access),
        )
        .await?;
        let worker_id = response
            .get("worker_id")
            .and_then(Value::as_str)
            .filter(|id| {
                id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            })
            .ok_or_else(|| Failure::new("ワーカーIDが不正です"))?;
        let secret = required_token(&response, "secret")?;
        let hex = secret.strip_prefix("ptw_").unwrap_or_default();
        if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(Failure::new("ワーカー資格が不正です"));
        }
        state.worker_id = Some(worker_id.to_owned());
        state.worker_secret = Some(secret);
        update_expiry(&mut state, &response)?;
    }
    dir.save(&state)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Renewal {
    Renewed,
    Reauthorize,
    Rejected,
}

/// refresh token で access token を取り直し、ワーカー資格を更新する。
/// 回転した refresh token は資格の更新より先に保存する。
pub async fn refresh(client: &Client, dir: &StateDir, state: &mut State) -> Result<Renewal> {
    let refresh_token = state
        .refresh_token
        .clone()
        .ok_or_else(|| Failure::new("authorize で提供先を認可してください"))?;
    let tokens = match api(
        client,
        &state.token_url,
        Body::Form(&[
            ("grant_type", "refresh_token"),
            ("client_id", &state.client_id),
            ("refresh_token", &refresh_token),
        ]),
        None,
    )
    .await
    {
        Ok(tokens) => tokens,
        Err(e) if e.oauth_error == "invalid_grant" => {
            state.refresh_token = None;
            dir.save(state)?;
            return Ok(Renewal::Reauthorize);
        }
        Err(e) => return Err(e),
    };
    if tokens
        .get("refresh_token")
        .is_some_and(|v| !v.is_null() && v != "")
    {
        state.refresh_token = Some(required_token(&tokens, "refresh_token")?);
        dir.save(state)?;
    }
    let access = required_token(&tokens, "access_token")?;
    match renew(client, state, &access).await {
        Ok(()) => {}
        Err(e) if e.status == 401 => return Ok(Renewal::Reauthorize),
        Err(e) if matches!(e.status, 403 | 404) => return Ok(Renewal::Rejected),
        Err(e) => return Err(e),
    }
    dir.save(state)?;
    Ok(Renewal::Renewed)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub struct Request {
        pub method: String,
        pub path: String,
        pub headers: Vec<(String, String)>,
        pub body: String,
    }

    pub type Handler = Arc<dyn Fn(&Request) -> (u16, Vec<(String, String)>, Vec<u8>) + Send + Sync>;

    /// ループバックの HTTP の簡易なモック。1 接続 1 要求で応答して閉じる。
    pub async fn serve(handler: Handler) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let log = Arc::new(Mutex::new(Vec::new()));
        let seen = log.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let handler = handler.clone();
                let seen = seen.clone();
                tokio::spawn(async move {
                    let mut buffer = Vec::new();
                    let mut chunk = [0; 4096];
                    let head_end = loop {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buffer.extend_from_slice(&chunk[..n]);
                        if let Some(i) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i;
                        }
                    };
                    let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
                    let mut lines = head.split("\r\n");
                    let mut first = lines.next().unwrap().split(' ');
                    let method = first.next().unwrap().to_owned();
                    let path = first.next().unwrap().to_owned();
                    let headers: Vec<(String, String)> = lines
                        .filter_map(|l| l.split_once(':'))
                        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                        .collect();
                    let length: usize = headers
                        .iter()
                        .find(|(k, _)| k == "content-length")
                        .map_or(0, |(_, v)| v.parse().unwrap());
                    let mut body = buffer[head_end + 4..].to_vec();
                    while body.len() < length {
                        let n = socket.read(&mut chunk).await.unwrap();
                        body.extend_from_slice(&chunk[..n]);
                    }
                    let request = Request {
                        method,
                        path,
                        headers,
                        body: String::from_utf8_lossy(&body).to_string(),
                    };
                    seen.lock()
                        .unwrap()
                        .push(format!("{} {}", request.method, request.path));
                    let (status, headers, body) = handler(&request);
                    let mut response = format!(
                        "HTTP/1.1 {status} X\r\nConnection: close\r\nContent-Length: {}\r\n",
                        body.len()
                    );
                    for (k, v) in headers {
                        response += &format!("{k}: {v}\r\n");
                    }
                    response += "\r\n";
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                });
            }
        });
        (origin, log)
    }

    pub fn json_reply(status: u16, value: Value) -> (u16, Vec<(String, String)>, Vec<u8>) {
        (
            status,
            vec![("Content-Type".into(), "application/json".into())],
            value.to_string().into_bytes(),
        )
    }

    fn client() -> Client {
        crate::http::client(Duration::from_secs(5))
    }

    const SECRET: &str = "ptw_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    #[tokio::test]
    async fn device_flow_and_registration_store_only_the_refresh_token() {
        let polls = Arc::new(Mutex::new(0));
        let origin = Arc::new(Mutex::new(String::new()));
        let (base, log) = {
            let polls = polls.clone();
            let origin = origin.clone();
            serve(Arc::new(move |r: &Request| {
                let o = origin.lock().unwrap().clone();
                match r.path.as_str() {
                    "/api/client-config" => json_reply(
                        200,
                        json!({"client_id": "translator-desktop", "discovery_url": format!("{o}/idp/.well-known/openid-configuration")}),
                    ),
                    "/idp/.well-known/openid-configuration" => json_reply(
                        200,
                        json!({"issuer": format!("{o}/idp/"), "token_endpoint": format!("{o}/token")}),
                    ),
                    "/application/o/device/" => {
                        assert!(r.body.contains("scope=openid+profile+offline_access"));
                        json_reply(200, json!({"verification_uri": format!("{o}/device"), "user_code": "ABCD", "device_code": "dev", "interval": 1, "expires_in": 60}))
                    }
                    "/token" => {
                        let mut n = polls.lock().unwrap();
                        *n += 1;
                        if *n == 1 {
                            json_reply(400, json!({"error": "authorization_pending"}))
                        } else {
                            json_reply(200, json!({"access_token": "access", "refresh_token": "refresh"}))
                        }
                    }
                    "/api/worker/register" => {
                        assert!(r.headers.contains(&("authorization".into(), "Bearer access".into())));
                        assert_eq!(r.body, r#"{"terms_version":1}"#);
                        json_reply(200, json!({"worker_id": "0123456789abcdef0123456789abcdef", "secret": SECRET, "expires_in": 604800}))
                    }
                    _ => json_reply(404, json!({})),
                }
            }))
            .await
        };
        *origin.lock().unwrap() = base.clone();
        let temp = tempfile::tempdir().unwrap();
        let dir = StateDir::open(&temp.path().join("state")).unwrap();
        let mut shown = Vec::new();
        authorize(&client(), &dir, &base, |uri, code| {
            shown.push(format!("{uri} {code}"))
        })
        .await
        .unwrap();
        assert_eq!(shown, [format!("{base}/device ABCD")]);
        assert_eq!(*polls.lock().unwrap(), 2);
        let raw = std::fs::read_to_string(temp.path().join("state/worker.json")).unwrap();
        assert!(raw.contains("\"refresh_token\":\"refresh\""));
        assert!(!raw.contains("access"));
        let state = dir.load().unwrap();
        assert_eq!(state.worker_secret.as_deref(), Some(SECRET));
        assert!(state.renew_at > now() && state.renew_at <= now() + DAY);
        assert!(log
            .lock()
            .unwrap()
            .contains(&"POST /api/worker/register".to_string()));
    }

    #[tokio::test]
    async fn discovery_rejects_endpoints_on_another_origin() {
        let origin = Arc::new(Mutex::new(String::new()));
        let (base, _) = {
            let origin = origin.clone();
            serve(Arc::new(move |r: &Request| {
                let o = origin.lock().unwrap().clone();
                match r.path.as_str() {
                    "/api/client-config" => json_reply(
                        200,
                        json!({"client_id": "c", "discovery_url": format!("{o}/idp/")}),
                    ),
                    "/idp/" => json_reply(
                        200,
                        json!({"issuer": format!("{o}/idp/"), "token_endpoint": "http://127.0.0.1:1/token"}),
                    ),
                    _ => json_reply(404, json!({})),
                }
            }))
            .await
        };
        *origin.lock().unwrap() = base.clone();
        let error = configuration(&client(), &base).await.err().unwrap();
        assert!(error.message.contains("一致"), "{}", error.message);
    }

    #[tokio::test]
    async fn rotation_is_saved_and_invalid_grant_requires_reauthorization() {
        let step = Arc::new(Mutex::new(0));
        let (base, _) = {
            let step = step.clone();
            serve(Arc::new(move |r: &Request| {
                let mut n = step.lock().unwrap();
                *n += 1;
                match (r.path.as_str(), *n) {
                    ("/token", 1) => json_reply(
                        200,
                        json!({"access_token": "a", "refresh_token": "rotated"}),
                    ),
                    ("/api/worker/renew", 2) => json_reply(503, json!({})),
                    ("/token", 3) => json_reply(400, json!({"error": "invalid_grant"})),
                    _ => json_reply(500, json!({})),
                }
            }))
            .await
        };
        let temp = tempfile::tempdir().unwrap();
        let dir = StateDir::open(&temp.path().join("state")).unwrap();
        let mut state = State::default();
        state.broker = base.clone();
        state.client_id = "c".into();
        state.token_url = format!("{base}/token");
        state.refresh_token = Some("first".into());
        state.worker_id = Some("0123456789abcdef0123456789abcdef".into());
        state.worker_secret = Some(SECRET.into());
        assert!(refresh(&client(), &dir, &mut state).await.is_err());
        assert_eq!(
            dir.load().unwrap().refresh_token.as_deref(),
            Some("rotated")
        );
        assert_eq!(
            refresh(&client(), &dir, &mut state).await.unwrap(),
            Renewal::Reauthorize
        );
        assert!(dir.load().unwrap().refresh_token.is_none());
    }
}
