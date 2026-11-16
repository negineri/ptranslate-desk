//! ワーカーが使う推論エンジン。自動で取得して起動するか、既存の llama-server を使う。
use crate::http::{client, json_object, limited_body, Failure, Result};
use ptranslate_core::engine::{
    executable, extract, server_args, verify, CONTEXT_PER_SLOT, ENGINE_BYTES, ENGINE_COMMIT,
    ENGINE_SHA, ENGINE_TAG, ENGINE_URL, MODEL_BYTES, MODEL_FILE, MODEL_SHA, MODEL_URL,
};
use reqwest::Client;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    os::unix::io::AsRawFd,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    process::{Child, Command},
};
use zeroize::Zeroizing;

pub const MODEL_DIGEST_HEADER: &str = "x-ptranslate-model-sha256";

/// ワーカーに渡す推論先。
pub struct Target {
    pub port: u16,
    pub secret: Zeroizing<String>,
    pub model_sha256: String,
}

fn random_key() -> Result<Zeroizing<String>> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let mut secret = Zeroizing::new([0u8; 32]);
    getrandom::fill(secret.as_mut()).map_err(|_| Failure::new("認証キーを生成できませんでした"))?;
    Ok(Zeroizing::new(URL_SAFE_NO_PAD.encode(secret.as_ref())))
}

// ---- 自動で取得して起動する推論エンジン ----

pub struct Installation {
    pub engine: PathBuf,
    pub model: PathBuf,
}

/// 推論エンジンとモデルを取得して検証し、展開する。取得済みなら検証だけ行う。
pub async fn prepare(data: &Path, log: &(dyn Fn(&str) + Sync)) -> Result<Installation> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(data)
        .map_err(|_| Failure::new("データの保存先を作成できませんでした"))?;
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(data.join("download.lock"))
        .map_err(|_| Failure::new("データの保存先を使用できませんでした"))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Failure::new("同じデータの保存先を別のプロセスが使用中です"));
    }
    let client = Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|_| Failure::new("ダウンロードの準備に失敗しました"))?;
    let archive = data.join(format!("llama-{ENGINE_TAG}-ubuntu.tar.gz"));
    let model = data.join(MODEL_FILE);
    fetch(
        &client,
        "推論エンジン",
        ENGINE_URL,
        &archive,
        ENGINE_SHA,
        ENGINE_BYTES,
        log,
    )
    .await?;
    fetch(
        &client,
        "モデル",
        MODEL_URL,
        &model,
        MODEL_SHA,
        MODEL_BYTES,
        log,
    )
    .await?;
    let marker = data.join("installation.json");
    if let Some(engine) = fs::read(&marker)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("engine").and_then(|e| e.as_str()).map(PathBuf::from))
        .filter(|engine| engine.starts_with(data) && engine.is_file())
    {
        return Ok(Installation { engine, model });
    }
    log("推論エンジンを展開しています");
    let staging = tempfile::Builder::new()
        .prefix(&format!("llama-{ENGINE_TAG}-"))
        .tempdir_in(data)
        .map_err(|_| Failure::new("展開先を準備できませんでした"))?;
    let (archive_copy, staging_path) = (archive.clone(), staging.path().to_owned());
    tokio::task::spawn_blocking(move || extract(&archive_copy, &staging_path))
        .await
        .map_err(|_| Failure::new("推論エンジンを展開できませんでした"))?
        .map_err(Failure::new)?;
    let engine = executable(staging.path()).map_err(Failure::new)?;
    let _kept = staging.keep();
    let mut file = tempfile::NamedTempFile::new_in(data)
        .map_err(|_| Failure::new("設定を保存できませんでした"))?;
    serde_json::to_writer(
        &mut file,
        &serde_json::json!({"engine": engine, "model": model}),
    )
    .map_err(|_| Failure::new("設定を保存できませんでした"))?;
    file.persist(marker)
        .map_err(|_| Failure::new("設定を保存できませんでした"))?;
    Ok(Installation { engine, model })
}

/// 固定の配布元から取得し、サイズと SHA-256 が一致したものだけを置く。
async fn fetch(
    client: &Client,
    item: &str,
    url: &str,
    path: &Path,
    expected: &'static str,
    total: u64,
    log: &(dyn Fn(&str) + Sync),
) -> Result<()> {
    let cached = {
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            verify(path, expected, Arc::new(AtomicBool::new(false)))
        })
        .await
        .map_err(|_| Failure::new("検証を実行できませんでした"))?
        .map_err(Failure::new)?
    };
    if cached {
        return Ok(());
    }
    log(&format!(
        "{item}を取得しています ({} MiB)",
        total / (1024 * 1024)
    ));
    let mut response = client
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|_| Failure::new("ダウンロード先に接続できませんでした"))?;
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))
        .map_err(|_| Failure::new("保存用のファイルを作成できませんでした"))?;
    let mut hash = Sha256::new();
    let mut received = 0u64;
    let mut reported = 0u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| Failure::new("ダウンロードが途切れました"))?
    {
        received += chunk.len() as u64;
        if received > total {
            return Err(Failure::new("取得したファイルのサイズが一致しません"));
        }
        file.write_all(&chunk).map_err(|_| {
            Failure::new("ファイルを保存できませんでした。空き容量を確認してください")
        })?;
        hash.update(&chunk);
        if received * 10 / total > reported {
            reported = received * 10 / total;
            log(&format!("{item}: {}%", reported * 10));
        }
    }
    if received != total || format!("{:x}", hash.finalize()) != expected {
        return Err(Failure::new("取得したファイルの検証に失敗しました"));
    }
    file.as_file()
        .sync_all()
        .map_err(|_| Failure::new("ファイルを保存できませんでした"))?;
    file.persist(path)
        .map_err(|_| Failure::new("ファイルを配置できませんでした"))?;
    Ok(())
}

/// 自分で起動した llama-server。破棄すると止める。
pub struct Managed {
    child: Child,
    pub target: Target,
}

impl Managed {
    pub async fn start(install: &Installation, threads: Option<usize>) -> Result<Self> {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0")
                .map_err(|_| Failure::new("推論エンジンのポートを確保できませんでした"))?;
            listener
                .local_addr()
                .map_err(|_| Failure::new("ポートを取得できませんでした"))?
                .port()
        };
        let key = random_key()?;
        let mut command = Command::new(&install.engine);
        command
            .args(server_args(&install.model, port, 1))
            // このビルドは CPU だけで動かす。
            .args(["--device", "none", "--n-gpu-layers", "0"])
            .env("LLAMA_API_KEY", key.as_str())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(threads) = threads {
            command.args(["--threads", &threads.to_string()]);
        }
        let child = command
            .spawn()
            .map_err(|_| Failure::new("推論エンジンを起動できませんでした"))?;
        let mut managed = Self {
            child,
            target: Target {
                port,
                secret: key,
                model_sha256: MODEL_SHA.into(),
            },
        };
        managed.wait_ready().await?;
        Ok(managed)
    }

    async fn wait_ready(&mut self) -> Result<()> {
        let client = client(Duration::from_secs(2));
        let base = format!("http://127.0.0.1:{}", self.target.port);
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(300) {
            if !self.alive() {
                return Err(Failure::new("推論エンジンが終了しました"));
            }
            let ready = client
                .get(format!("{base}/health"))
                .bearer_auth(self.target.secret.as_str())
                .send()
                .await
                .is_ok_and(|r| r.status().is_success());
            if ready {
                let protected = client
                    .get(format!("{base}/v1/models"))
                    .send()
                    .await
                    .is_ok_and(|r| r.status() == 401);
                if !protected {
                    return Err(Failure::fatal(
                        "推論エンジンのローカル認証を確認できませんでした",
                    ));
                }
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Err(Failure::new("モデルの読み込みがタイムアウトしました"))
    }

    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

// ---- 既存のローカルの llama-server ----

pub fn read_key(path: &Path) -> Result<Zeroizing<String>> {
    let text = Zeroizing::new(
        fs::read_to_string(path)
            .map_err(|_| Failure::new("推論エンジンの資格を読み込めませんでした"))?,
    );
    let key = text.trim();
    if key.is_empty() || key.len() > 4096 {
        return Err(Failure::new("推論エンジンの資格が不正です"));
    }
    Ok(Zeroizing::new(key.to_owned()))
}

pub fn model_digest(path: &Path) -> Result<String> {
    let mut file =
        fs::File::open(path).map_err(|_| Failure::new("モデルを読み込めませんでした"))?;
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash)
        .map_err(|_| Failure::new("モデルを読み込めませんでした"))?;
    Ok(format!("{:x}", hash.finalize()))
}

// ---- 別のサーバーの llama-server (HTTPS) ----

pub struct Remote {
    url: String,
    key: Zeroizing<String>,
    allow_revision_mismatch: bool,
    client: Client,
    stream_client: Client,
    mismatch: AtomicBool,
}

impl Remote {
    pub fn new(url: &str, key: Zeroizing<String>, allow_revision_mismatch: bool) -> Result<Self> {
        crate::http::https_url(url, true)?;
        Ok(Self {
            url: url.trim_end_matches('/').to_owned(),
            key,
            allow_revision_mismatch,
            client: client(Duration::from_secs(5)),
            stream_client: Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(15))
                .read_timeout(Duration::from_secs(30))
                .build()
                .map_err(|_| Failure::new("推論先への接続を準備できませんでした"))?,
            mismatch: AtomicBool::new(false),
        })
    }

    fn digest_ok(headers: &reqwest::header::HeaderMap) -> bool {
        let values: Vec<_> = headers.get_all(MODEL_DIGEST_HEADER).iter().collect();
        values.len() == 1 && values[0] == MODEL_SHA
    }

    /// `GET /props` のビルド情報・モデル・コンテキスト長と、モデルの SHA-256 を照合する。
    /// 設定の不一致は `fatal`、通信の失敗はそれ以外で返す。
    pub async fn verify(&self) -> Result<()> {
        if self.mismatch.load(Ordering::SeqCst) {
            return Err(Failure::fatal(
                "リモート推論先の応答でモデルのSHA-256が一致しません",
            ));
        }
        let unavailable = || Failure::new("リモート推論先の設定を取得できません");
        let response = self
            .client
            .get(format!("{}/props", self.url))
            .bearer_auth(self.key.as_str())
            .send()
            .await
            .map_err(|_| unavailable())?;
        if matches!(response.status().as_u16(), 401 | 403 | 404) {
            return Err(Failure::fatal(
                "リモート推論先の認証または /props を確認してください",
            ));
        }
        if response.status() != 200 {
            return Err(unavailable());
        }
        if !Self::digest_ok(response.headers()) {
            return Err(Failure::fatal(
                "リモート推論先のモデルのSHA-256を確認できません",
            ));
        }
        let props = json_object(
            &limited_body(response, 65536)
                .await
                .map_err(|_| unavailable())?,
        )
        .map_err(|_| unavailable())?;
        let build = props
            .get("build_info")
            .and_then(|v| v.as_str())
            .filter(|b| !b.trim().is_empty() && !b.chars().any(char::is_control))
            .ok_or_else(|| {
                Failure::fatal("リモート推論先のllama.cppのビルド情報を確認できません")
            })?;
        let (tag, commit) = build.split_once('-').unwrap_or((build, ""));
        if !self.allow_revision_mismatch
            && (tag != ENGINE_TAG || commit.len() < 7 || !ENGINE_COMMIT.starts_with(commit))
        {
            return Err(Failure::fatal(format!(
                "リモート推論先のllama.cppが対応リビジョン {ENGINE_TAG} と一致しません"
            )));
        }
        let model = props
            .get("model_path")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if Path::new(model).file_name().and_then(|n| n.to_str()) != Some(MODEL_FILE) {
            return Err(Failure::fatal(
                "リモート推論先が承認済みのモデルを読み込んでいません",
            ));
        }
        let context = props
            .get("default_generation_settings")
            .and_then(|s| s.get("n_ctx"))
            .and_then(|n| n.as_u64());
        if context.is_none_or(|n| n < CONTEXT_PER_SLOT as u64) {
            return Err(Failure::fatal(format!(
                "リモート推論先には1枠あたり{CONTEXT_PER_SLOT}以上のコンテキストが必要です"
            )));
        }
        Ok(())
    }

    /// ワーカーからの `POST /v1/completions` だけを推論先へ中継する、ループバックの口を開く。
    pub async fn relay(self: Arc<Self>) -> Result<Target> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| Failure::new("中継のポートを確保できませんでした"))?;
        let port = listener
            .local_addr()
            .map_err(|_| Failure::new("中継のポートを確保できませんでした"))?
            .port();
        let secret = random_key()?;
        let expected = Arc::new(Zeroizing::new(format!("Bearer {}", secret.as_str())));
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let remote = self.clone();
                let expected = expected.clone();
                tokio::spawn(async move {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(600),
                        remote.handle(socket, &expected),
                    )
                    .await;
                });
            }
        });
        Ok(Target {
            port,
            secret,
            model_sha256: MODEL_SHA.into(),
        })
    }

    async fn handle(&self, mut socket: tokio::net::TcpStream, expected: &str) {
        let Ok(Ok(Some((head, mut body)))) =
            tokio::time::timeout(Duration::from_secs(10), read_head(&mut socket)).await
        else {
            return;
        };
        let status = match check_request(&head, expected) {
            Ok(length) => {
                if body.len() > length {
                    return;
                }
                let mut rest = vec![0; length - body.len()];
                if tokio::time::timeout(Duration::from_secs(10), socket.read_exact(&mut rest))
                    .await
                    .map_or(true, |r| r.is_err())
                {
                    return;
                }
                body.extend_from_slice(&rest);
                match self.forward(&mut socket, body).await {
                    Ok(()) => return,
                    Err(sent) if sent => return,
                    Err(_) => 502,
                }
            }
            Err(status) => status,
        };
        let _ = socket
            .write_all(
                format!(
                    "HTTP/1.1 {status} Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await;
    }

    /// 応答を中継する。失敗時は、応答を書き始めていたら true を返す。
    async fn forward(
        &self,
        socket: &mut tokio::net::TcpStream,
        body: Vec<u8>,
    ) -> std::result::Result<(), bool> {
        let mut response = self
            .stream_client
            .post(format!("{}/v1/completions", self.url))
            .bearer_auth(self.key.as_str())
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| false)?;
        if !Self::digest_ok(response.headers()) {
            self.mismatch.store(true, Ordering::SeqCst);
            return Err(false);
        }
        let event_stream = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(';').next().unwrap_or_default().trim() == "text/event-stream");
        if response.status() != 200 || !event_stream {
            return Err(false);
        }
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .await
            .map_err(|_| true)?;
        while let Some(chunk) = response.chunk().await.map_err(|_| true)? {
            socket.write_all(&chunk).await.map_err(|_| true)?;
        }
        Ok(())
    }
}

/// 要求の頭を読む。`(頭, 頭の後に読めた本文)` を返す。
async fn read_head(
    socket: &mut tokio::net::TcpStream,
) -> std::io::Result<Option<(String, Vec<u8>)>> {
    let mut buffer = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..n]);
        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8(buffer[..end].to_vec()).ok();
            return Ok(head.map(|h| (h, buffer[end + 4..].to_vec())));
        }
        if buffer.len() > 16 * 1024 {
            return Ok(None);
        }
    }
}

/// 中継してよい要求なら本文の長さを、そうでなければ返す状態コードを返す。
fn check_request(head: &str, expected: &str) -> std::result::Result<usize, u16> {
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let (method, path) = (parts.next(), parts.next());
    if method != Some("POST") || path != Some("/v1/completions") {
        return Err(404);
    }
    let mut authorization = Vec::new();
    let mut lengths = Vec::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(400);
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "authorization" => authorization.push(value.trim()),
            "content-length" => lengths.push(value.trim()),
            "transfer-encoding" => return Err(400),
            _ => {}
        }
    }
    let authorized = authorization.len() == 1
        && authorization[0].len() == expected.len()
        && authorization[0]
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0;
    if !authorized {
        return Err(401);
    }
    match lengths.as_slice() {
        [value] if (1..=6).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit()) => {
            let length: usize = value.parse().map_err(|_| 400u16)?;
            if (1..=256 * 1024).contains(&length) {
                Ok(length)
            } else {
                Err(400)
            }
        }
        _ => Err(400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::tests::{json_reply, serve, Request};
    use serde_json::json;
    use std::sync::Mutex;

    fn props(build: &str, n_ctx: u64) -> serde_json::Value {
        json!({
            "build_info": build,
            "model_path": format!("/models/{MODEL_FILE}"),
            "default_generation_settings": {"n_ctx": n_ctx},
        })
    }

    async fn remote_with(
        body: serde_json::Value,
        digest: Option<&'static str>,
        allow: bool,
    ) -> Result<()> {
        let (base, _) = serve(Arc::new(move |r: &Request| {
            assert_eq!(r.path, "/props");
            assert!(r
                .headers
                .contains(&("authorization".into(), "Bearer key".into())));
            let (status, mut headers, bytes) = json_reply(200, body.clone());
            if let Some(d) = digest {
                headers.push(("X-PTranslate-Model-SHA256".into(), d.into()));
            }
            (status, headers, bytes)
        }))
        .await;
        Remote::new(&base, Zeroizing::new("key".into()), allow)
            .unwrap()
            .verify()
            .await
    }

    #[tokio::test]
    async fn remote_metadata_requires_release_build_model_digest_and_context() {
        let good = props(&format!("{ENGINE_TAG}-c7fc50941"), CONTEXT_PER_SLOT as u64);
        remote_with(good.clone(), Some(MODEL_SHA), false)
            .await
            .unwrap();
        let mut other_model = good.clone();
        other_model["model_path"] = json!("/models/other.gguf");
        for (body, digest) in [
            (props("b11804-abcdef0", 16384), Some(MODEL_SHA)),
            (props("b12000-c7fc50941", 16384), Some(MODEL_SHA)),
            (props("", 16384), Some(MODEL_SHA)),
            (other_model, Some(MODEL_SHA)),
            (
                props("b11804-c7fc50941", CONTEXT_PER_SLOT as u64 - 1),
                Some(MODEL_SHA),
            ),
            (good.clone(), None),
            (
                good.clone(),
                Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            ),
        ] {
            let error = remote_with(body.clone(), digest, false).await.unwrap_err();
            assert!(error.fatal, "{body} {digest:?}: {}", error.message);
        }
        // リビジョンの不一致だけを許し、モデルとコンテキストは引き続き照合する。
        remote_with(props("b12000-abcdef0", 16384), Some(MODEL_SHA), true)
            .await
            .unwrap();
        assert!(
            remote_with(props("b12000-abcdef0", 8192), Some(MODEL_SHA), true)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn relay_accepts_only_authorized_completions_and_checks_the_digest() {
        let digest = Arc::new(Mutex::new(MODEL_SHA));
        let (base, log) = {
            let digest = digest.clone();
            serve(Arc::new(move |r: &Request| {
                assert_eq!(r.path, "/v1/completions");
                assert!(r
                    .headers
                    .contains(&("authorization".into(), "Bearer key".into())));
                assert_eq!(r.body, r#"{"prompt":"x"}"#);
                (
                    200,
                    vec![
                        ("Content-Type".into(), "text/event-stream".into()),
                        (
                            "X-PTranslate-Model-SHA256".into(),
                            (*digest.lock().unwrap()).into(),
                        ),
                    ],
                    b"data: {}\n\n".to_vec(),
                )
            }))
            .await
        };
        let remote = Arc::new(Remote::new(&base, Zeroizing::new("key".into()), false).unwrap());
        let target = remote.clone().relay().await.unwrap();
        let local = format!("http://127.0.0.1:{}", target.port);
        let http = client(Duration::from_secs(5));
        let post = |path: &str, auth: &str| {
            http.post(format!("{local}{path}"))
                .header("Authorization", auth.to_owned())
                .header("Content-Type", "application/json")
                .body(r#"{"prompt":"x"}"#)
                .send()
        };
        let auth = format!("Bearer {}", target.secret.as_str());
        let ok = post("/v1/completions", &auth).await.unwrap();
        assert_eq!(ok.status(), 200);
        assert_eq!(ok.text().await.unwrap(), "data: {}\n\n");
        assert_eq!(
            post("/v1/completions", "Bearer wrong")
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(post("/props", &auth).await.unwrap().status(), 404);
        assert_eq!(log.lock().unwrap().len(), 1);
        *digest.lock().unwrap() =
            "0000000000000000000000000000000000000000000000000000000000000000";
        assert_eq!(post("/v1/completions", &auth).await.unwrap().status(), 502);
        assert!(remote.verify().await.unwrap_err().fatal);
    }

    #[test]
    fn request_checks_reject_smuggling_and_oversized_bodies() {
        let ok = "POST /v1/completions HTTP/1.1\r\nAuthorization: Bearer s\r\nContent-Length: 10";
        assert_eq!(check_request(ok, "Bearer s"), Ok(10));
        for (head, status) in [
            ("GET /v1/completions HTTP/1.1\r\nAuthorization: Bearer s\r\nContent-Length: 1", 404),
            ("POST /v1/completions HTTP/1.1\r\nContent-Length: 1", 401),
            ("POST /v1/completions HTTP/1.1\r\nAuthorization: Bearer s\r\nAuthorization: Bearer s\r\nContent-Length: 1", 401),
            ("POST /v1/completions HTTP/1.1\r\nAuthorization: Bearer s\r\nContent-Length: 1\r\nTransfer-Encoding: chunked", 400),
            ("POST /v1/completions HTTP/1.1\r\nAuthorization: Bearer s\r\nContent-Length: 1\r\nContent-Length: 1", 400),
            ("POST /v1/completions HTTP/1.1\r\nAuthorization: Bearer s\r\nContent-Length: 999999", 400),
            ("POST /v1/completions HTTP/1.1\r\nAuthorization: Bearer s\r\nContent-Length: 0", 400),
        ] {
            assert_eq!(check_request(head, "Bearer s"), Err(status), "{head}");
        }
    }
}
