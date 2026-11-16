//! 提供先・IdP との JSON の通信と、URL の検査。
use reqwest::{header, Client, Url};
use serde_json::{Map, Value};
use std::{fmt, time::Duration};

/// 応答の JSON の上限 (バイト)。
const METADATA_LIMIT: usize = 65536;

#[derive(Debug)]
pub struct Failure {
    pub message: String,
    /// HTTP の状態コード。通信できなかった場合は 0。
    pub status: u16,
    /// OAuth のエラー (`error`)。
    pub oauth_error: String,
    /// 推論先の設定の不一致など、再試行しても直らない失敗。
    pub fatal: bool,
}

impl Failure {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: 0,
            oauth_error: String::new(),
            fatal: false,
        }
    }

    pub fn fatal(message: impl Into<String>) -> Self {
        Self {
            fatal: true,
            ..Self::new(message)
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

pub type Result<T> = std::result::Result<T, Failure>;

/// HTTPS の URL のオリジン (ホストとポート) を返す。`origin_only` ならパスも許さない。
/// 試験ではループバックの HTTP も許す。
pub fn https_url(value: &str, origin_only: bool) -> Result<(String, u16)> {
    let invalid = || Failure::new("HTTPS の URL が不正です");
    if value
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return Err(invalid());
    }
    let url = Url::parse(value).map_err(|_| invalid())?;
    let scheme_ok = url.scheme() == "https"
        || (cfg!(test) && url.scheme() == "http" && url.host_str() == Some("127.0.0.1"));
    if !scheme_ok
        || url.host_str().is_none_or(str::is_empty)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
        || (origin_only && url.path() != "/")
    {
        return Err(invalid());
    }
    Ok((
        url.host_str().unwrap_or_default().to_ascii_lowercase(),
        url.port_or_known_default().ok_or_else(invalid)?,
    ))
}

/// プロキシを使わず、リダイレクトを追わないクライアント。
pub fn client(timeout: Duration) -> Client {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(timeout)
        .build()
        .expect("HTTP クライアントを作れませんでした")
}

pub enum Body<'a> {
    None,
    Form(&'a [(&'a str, &'a str)]),
    Json(&'a Value),
}

/// JSON のオブジェクトを返す API を呼ぶ。失敗した応答の `error` は `oauth_error` に入れる。
pub async fn api(
    client: &Client,
    url: &str,
    body: Body<'_>,
    token: Option<&str>,
) -> Result<Map<String, Value>> {
    let unreachable = || Failure::new("接続できないか応答が不正です");
    let mut request = match body {
        Body::None => client.get(url),
        Body::Form(form) => client.post(url).form(form),
        Body::Json(value) => client.post(url).json(value),
    }
    .header(header::ACCEPT, "application/json");
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.map_err(|_| unreachable())?;
    let status = response.status();
    let bytes = limited_body(response, METADATA_LIMIT).await;
    if !status.is_success() {
        let code = bytes
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_default();
        return Err(Failure {
            message: format!("HTTP {}", status.as_u16()),
            status: status.as_u16(),
            oauth_error: code,
            fatal: false,
        });
    }
    json_object(&bytes.map_err(|_| unreachable())?).map_err(|_| unreachable())
}

pub async fn limited_body(
    mut response: reqwest::Response,
    limit: usize,
) -> std::result::Result<Vec<u8>, ()> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        if body.len() + chunk.len() > limit {
            return Err(());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub fn json_object(bytes: &[u8]) -> std::result::Result<Map<String, Value>, ()> {
    match serde_json::from_slice(bytes) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err(()),
    }
}

/// 認証の応答の、空白や制御文字のない ASCII の値を取り出す。
pub fn required_token(payload: &Map<String, Value>, name: &str) -> Result<String> {
    match payload.get(name).and_then(Value::as_str) {
        Some(value)
            if !value.is_empty()
                && value.len() <= 16384
                && value.bytes().all(|b| b.is_ascii_graphic()) =>
        {
            Ok(value.to_owned())
        }
        _ => Err(Failure::new("認証の応答が不正です")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_origins_are_strict() {
        assert_eq!(
            https_url("https://Example.com", true).unwrap(),
            ("example.com".into(), 443)
        );
        assert_eq!(
            https_url("https://example.com:8443/", true).unwrap(),
            ("example.com".into(), 8443)
        );
        assert!(https_url("https://example.com/path", false).is_ok());
        for bad in [
            "http://example.com",
            "https://user@example.com",
            "https://example.com/?a=1",
            "https://example.com/#x",
            "https://example.com/path",
            "https://exa mple.com",
            "https://example.com\\",
        ] {
            assert!(https_url(bad, true).is_err(), "{bad}");
        }
    }

    #[test]
    fn tokens_must_be_printable_ascii_without_spaces() {
        let map = json_object(r#"{"a":"abc.DEF-123","b":"a b","c":"","d":1,"e":"あ"}"#.as_bytes())
            .unwrap();
        assert_eq!(required_token(&map, "a").unwrap(), "abc.DEF-123");
        for name in ["b", "c", "d", "e", "missing"] {
            assert!(required_token(&map, name).is_err(), "{name}");
        }
    }
}
