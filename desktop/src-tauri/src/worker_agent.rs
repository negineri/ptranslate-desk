//! 親プロセスから1件分の接続情報を受け取り、最大1件を処理する。
use crate::translation::{prompt, Decoder};
use crate::worker_sandbox::{self, Ports};
use reqwest::{Client, Response, StatusCode, Url};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{self, BufRead, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::time::{interval, sleep, timeout};
use zeroize::{Zeroize, Zeroizing};

const MAX_MESSAGE: usize = 256 * 1024;
const POST_TIMEOUT: Duration = Duration::from_secs(5);
type Result<T> = std::result::Result<T, ()>;
type RequestResult<T> = std::result::Result<T, RequestError>;

enum RequestError {
    Failed,
    Rejected,
}

impl From<()> for RequestError {
    fn from(_: ()) -> Self {
        Self::Failed
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Bootstrap {
    pub(crate) broker: String,
    pub(crate) worker_secret: String,
    pub(crate) model_sha256: String,
    pub(crate) agent_version: String,
    pub(crate) engine_port: u16,
    pub(crate) engine_secret: String,
    pub(crate) relay_port: Option<u16>,
    pub(crate) relay_secret: Option<String>,
}

impl Drop for Bootstrap {
    fn drop(&mut self) {
        self.worker_secret.zeroize();
        self.engine_secret.zeroize();
        self.relay_secret.zeroize();
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Assignment {
    assignment_id: String,
    deadline_seconds: u64,
    input: Value,
}

enum Poll {
    Assigned(Assignment),
    Idle(&'static str),
}

fn idle_status(response: &Response) -> &'static str {
    if response
        .headers()
        .get("X-PTranslate-Worker-State")
        .is_some_and(|value| value == "quarantine")
    {
        "quarantined"
    } else {
        "waiting"
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultReply {
    stop: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PingReply {
    stop: Vec<String>,
}

struct Agent {
    client: Client,
    broker: Url,
    config: Bootstrap,
}

fn hex(value: &str, size: usize) -> bool {
    value.len() == size
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_version(value: &str) -> bool {
    let parts: Vec<_> = value.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            (1..=9).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_digit())
        })
}

pub(crate) fn broker_origin(value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| ())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(());
    }
    Ok(url)
}

fn assignment_prompt(assignment: &Assignment) -> Result<(String, usize)> {
    if !hex(&assignment.assignment_id, 32) || !(1..=120).contains(&assignment.deadline_seconds) {
        return Err(());
    }
    let input = assignment.input.as_object().ok_or(())?;
    if input.keys().any(|key| {
        !matches!(
            key.as_str(),
            "text" | "target_lang" | "source_lang" | "form" | "context" | "glossary" | "history"
        )
    }) {
        return Err(());
    }
    for name in ["history", "glossary"] {
        if let Some(pairs) = input.get(name) {
            for pair in pairs.as_array().ok_or(())? {
                let pair = pair.as_object().ok_or(())?;
                if pair.len() != 2
                    || !["source", "target"]
                        .iter()
                        .all(|key| pair.get(*key).and_then(Value::as_str).is_some())
                {
                    return Err(());
                }
            }
        }
    }
    prompt(&assignment.input).map_err(|_| ())
}

async fn json_body<T: DeserializeOwned>(mut response: Response) -> Result<T> {
    if response
        .content_length()
        .is_some_and(|len| len > MAX_MESSAGE as u64)
    {
        return Err(());
    }
    let mut body = Zeroizing::new(Vec::new());
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        if body.len() + chunk.len() > MAX_MESSAGE {
            return Err(());
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| ())
}

impl Agent {
    fn new(config: Bootstrap) -> Result<Self> {
        let broker = broker_origin(&config.broker)?;
        if !config
            .worker_secret
            .strip_prefix("ptw_")
            .is_some_and(|value| hex(value, 64))
            || !hex(&config.model_sha256, 64)
            || !valid_version(&config.agent_version)
            || config.engine_port == 0
            || config.engine_secret.is_empty()
        {
            return Err(());
        }
        let mut client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(POST_TIMEOUT)
            .read_timeout(Duration::from_secs(30));
        match (config.relay_port, config.relay_secret.as_deref()) {
            (Some(port), Some(secret)) if port != 0 && !secret.is_empty() => {
                let proxy = reqwest::Proxy::https(format!("http://127.0.0.1:{port}"))
                    .map_err(|_| ())?
                    .basic_auth(crate::worker_relay::PROXY_USER, secret);
                client = client.proxy(proxy);
            }
            (None, None) if cfg!(test) => {}
            _ => return Err(()),
        }
        let client = client.build().map_err(|_| ())?;
        Ok(Self {
            client,
            broker,
            config,
        })
    }

    async fn post(&self, route: &str, body: &Value, limit: Duration) -> RequestResult<Response> {
        let response = self
            .client
            .post(self.broker.join(route).map_err(|_| ())?)
            .bearer_auth(&self.config.worker_secret)
            .timeout(limit)
            .json(body)
            .send()
            .await
            .map_err(|_| ())?;
        match response.status() {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(RequestError::Rejected),
            _ => Ok(response),
        }
    }

    async fn send_result(&self, id: &str, seq: &AtomicU64, mut body: Value) -> RequestResult<bool> {
        body["assignment_id"] = json!(id);
        body["seq"] = json!(seq.load(Ordering::Relaxed));
        let response = self.post("/api/worker/result", &body, POST_TIMEOUT).await?;
        if response.status() != StatusCode::OK {
            return Err(RequestError::Failed);
        }
        let reply: ResultReply = json_body(response).await?;
        seq.fetch_add(1, Ordering::Relaxed);
        Ok(reply.stop)
    }

    async fn stop_requested(&self, id: &str) -> RequestResult<()> {
        loop {
            sleep(Duration::from_secs(5)).await;
            let response = self
                .post("/api/worker/ping", &json!({"active":[id]}), POST_TIMEOUT)
                .await?;
            if response.status() != StatusCode::OK {
                return Err(RequestError::Failed);
            }
            let reply: PingReply = json_body(response).await?;
            if !reply.stop.is_empty() {
                return if reply.stop.len() == 1 && reply.stop[0] == id {
                    Ok(())
                } else {
                    Err(RequestError::Failed)
                };
            }
        }
    }

    async fn flush(&self, id: &str, seq: &AtomicU64, text: &mut String) -> RequestResult<bool> {
        while !text.is_empty() {
            // JSONエスケープ後も1回のPOSTを64KiBより小さく保つ。
            let end = text
                .char_indices()
                .nth(2048)
                .map_or(text.len(), |(index, _)| index);
            let stopped = self
                .send_result(id, seq, json!({"text":&text[..end], "done":false}))
                .await?;
            if stopped {
                return Ok(true);
            }
            text.drain(..end);
        }
        Ok(false)
    }

    async fn translate(
        &self,
        assignment: &Assignment,
        seq: &AtomicU64,
        prompt: String,
        limit: usize,
    ) -> RequestResult<bool> {
        let url = format!(
            "http://127.0.0.1:{}/v1/completions",
            self.config.engine_port
        );
        let mut response = self.client.post(url)
            .bearer_auth(&self.config.engine_secret)
            .json(&json!({
                "prompt": prompt, "max_tokens": limit, "temperature": 0, "cache_prompt": false,
                "stop": ["<|plamo:tag|>", "<|plamo:bos|>"], "add_special_tokens": true, "stream": true
            }))
            .send().await.map_err(|_| ())?;
        if response.status() != StatusCode::OK {
            return Err(RequestError::Failed);
        }
        let id = &assignment.assignment_id;
        let mut decoder = Decoder::default();
        let mut buffer = String::new();
        let mut tail = String::new();
        let mut count = 0;
        let mut tick = interval(Duration::from_millis(200));
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if self.flush(id, seq, &mut buffer).await? {
                        return Ok(false);
                    }
                }
                chunk = response.chunk() => {
                    let chunk = chunk.map_err(|_| ())?.ok_or(())?;
                    for event in decoder.feed(&chunk).map_err(|_| ())? {
                        if let Some(text) = event["t"].as_str() {
                            count += text.chars().count();
                            tail.push_str(text);
                            if count > limit * 6 || tail.contains("<|plamo:") {
                                return Err(RequestError::Failed);
                            }
                            if let Some((start, _)) = tail.char_indices().rev().nth(6) {
                                tail.drain(..start);
                            }
                            buffer.push_str(text);
                            if seq.load(Ordering::Relaxed) == 0
                                && self.flush(id, seq, &mut buffer).await? {
                                return Ok(false);
                            }
                        }
                        if let Some(finish) = event["finish"].as_str() {
                            if !matches!(finish, "stop" | "length") {
                                return Err(RequestError::Failed);
                            }
                            if self.flush(id, seq, &mut buffer).await? {
                                return Ok(false);
                            }
                            return self.send_result(id, seq, json!({"done":true,"finish":finish})).await.map(|stop| !stop);
                        }
                    }
                }
            }
        }
    }

    async fn run(
        &self,
        mut parent_closed: tokio::sync::oneshot::Receiver<()>,
        permission: impl std::future::Future<Output = Result<()>>,
    ) -> Result<&'static str> {
        let poll = json!({
            "agent_version":self.config.agent_version,
            "model":self.config.model_sha256
        });
        let assignment: Assignment = tokio::select! {
            _ = &mut parent_closed => return Ok("paused"),
            result = async {
                let response = self.post("/api/worker/poll", &poll, Duration::from_secs(35)).await?;
                match response.status() {
                    StatusCode::NO_CONTENT => Ok(Poll::Idle(idle_status(&response))),
                    StatusCode::OK => Ok(Poll::Assigned(json_body(response).await?)),
                    _ => Err(RequestError::Failed),
                }
            } => match result {
                Ok(Poll::Assigned(assignment)) => assignment,
                Ok(Poll::Idle(status)) => return Ok(status),
                Err(RequestError::Rejected) => return Ok("rejected"),
                Err(RequestError::Failed) => return Err(()),
            }
        };
        let seq = AtomicU64::new(0);
        let result = match assignment_prompt(&assignment) {
            Ok((prompt, limit)) => {
                tokio::select! {
                    _ = &mut parent_closed => Err(RequestError::Failed),
                    stop = self.stop_requested(&assignment.assignment_id) => stop.map(|_| false),
                    result = timeout(
                        Duration::from_secs(assignment.deadline_seconds),
                        async {
                            permission.await?;
                            status("working");
                            self.translate(&assignment, &seq, prompt, limit).await
                        },
                    ) => result.map_err(|_| RequestError::Failed).and_then(|value| value),
                }
            }
            Err(()) => Err(RequestError::Failed),
        };
        match result {
            Ok(true) => Ok("completed"),
            Ok(false) => Ok("paused"),
            Err(RequestError::Rejected) => Ok("rejected"),
            Err(RequestError::Failed) => {
                if hex(&assignment.assignment_id, 32)
                    && matches!(
                        self.send_result(
                            &assignment.assignment_id,
                            &seq,
                            json!({"error":"engine"})
                        )
                        .await,
                        Err(RequestError::Rejected)
                    )
                {
                    return Ok("rejected");
                }
                Err(())
            }
        }
    }
}

fn status(value: &'static str) {
    // 親に状態だけを通知する。入力・訳・資格・割り当てIDは出さない。
    let _ = writeln!(io::stdout(), "{{\"status\":\"{value}\"}}");
}

pub(crate) const READY_LINE: &[u8] = b"{\"status\":\"ready\"}\n";

pub(crate) const ASSIGNED_LINE: &[u8] = b"{\"status\":\"assigned\"}\n";
pub(crate) const START_COMMAND: &[u8] = b"go\n";

fn restricted_runtime(ports: Ports) -> Result<tokio::runtime::Runtime> {
    worker_sandbox::apply(ports)?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| ())
}

fn run_from_parent(ports: Ports) -> Result<&'static str> {
    let rt = restricted_runtime(ports)?;
    {
        let mut output = io::stdout().lock();
        output.write_all(READY_LINE).map_err(|_| ())?;
        output.flush().map_err(|_| ())?;
    }
    let mut input = Zeroizing::new(Vec::new());
    io::stdin()
        .lock()
        .take(16 * 1024 + 1)
        .read_until(b'\n', &mut input)
        .map_err(|_| ())?;
    if input.len() > 16 * 1024 || !input.ends_with(b"\n") {
        return Err(());
    }
    let config: Bootstrap = serde_json::from_slice(&input).map_err(|_| ())?;
    drop(input);
    if config.relay_port != Some(ports.relay) || config.engine_port != ports.engine {
        return Err(());
    }
    let agent = Agent::new(config)?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (allow, permission) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut command = [0; START_COMMAND.len()];
        if stdin.read_exact(&mut command).is_ok() && command == START_COMMAND {
            let _ = allow.send(());
            let _ = stdin.read(&mut [0]);
        }
        let _ = tx.send(());
    });
    rt.block_on(agent.run(rx, async {
        {
            let mut output = io::stdout().lock();
            output.write_all(ASSIGNED_LINE).map_err(|_| ())?;
            output.flush().map_err(|_| ())?;
        }
        permission.await.map_err(|_| ())
    }))
}

pub(crate) fn entry() {
    let result = Ports::parse(std::env::args_os().skip(2)).and_then(run_from_parent);
    status(result.unwrap_or("error"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_agent_communicates_after_seatbelt() {
        use crate::worker_relay::BrokerRelay;
        use std::process::Stdio;
        use tokio::{
            io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
            net::TcpListener,
            process::Command,
        };

        const PROBE: &str = "PTRANSLATE_SEATBELT_AGENT_TEST";
        if let Ok(ports) = std::env::var(PROBE) {
            let ports = Ports::parse(ports.split(',').map(std::ffi::OsString::from)).unwrap();
            let runtime = restricted_runtime(ports).expect("制限後にランタイムを起動できません");
            let mut input = Zeroizing::new(Vec::new());
            io::stdin().lock().read_to_end(&mut input).unwrap();
            let agent = Agent::new(serde_json::from_slice(&input).unwrap())
                .ok()
                .expect("制限後にクライアントを構築できません");
            drop(input);
            runtime.block_on(async {
                let response = agent.client.get(agent.broker.clone()).send().await;
                // 仲介のモックは ClientHello を確認して接続を閉じる。
                assert!(response.is_err());
                let response = agent
                    .client
                    .get(format!("http://127.0.0.1:{}/", ports.engine))
                    .send()
                    .await
                    .expect("制限後にエンジンへ接続できません");
                assert_eq!(response.status(), StatusCode::OK);
                assert_eq!(response.text().await.unwrap(), "mock engine");
            });
            return;
        }

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let broker = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let engine = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url =
                    Url::parse(&format!("https://{}/", broker.local_addr().unwrap())).unwrap();
                let relay = BrokerRelay::start(&url).await.unwrap();
                let engine_port = engine.local_addr().unwrap().port();
                let input = Zeroizing::new(
                    serde_json::to_vec(&Bootstrap {
                        broker: url.to_string(),
                        worker_secret: format!("ptw_{}", "a".repeat(64)),
                        model_sha256: "b".repeat(64),
                        agent_version: "0.5.0".into(),
                        engine_port,
                        engine_secret: "mock".into(),
                        relay_port: Some(relay.port),
                        relay_secret: Some(relay.secret.as_str().to_owned()),
                    })
                    .unwrap(),
                );
                let mut child = Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "worker_agent::tests::macos_agent_communicates_after_seatbelt",
                        "--nocapture",
                    ])
                    .env_clear()
                    .env(PROBE, format!("{},{}", relay.port, engine_port))
                    .stdin(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .unwrap();
                timeout(Duration::from_secs(10), async {
                    child.stdin.take().unwrap().write_all(&input).await.unwrap();
                    let serve = async {
                        let (mut socket, _) = broker.accept().await.unwrap();
                        assert_eq!(socket.read_u8().await.unwrap(), 0x16);
                        drop(socket);

                        let (socket, _) = engine.accept().await.unwrap();
                        let mut request = BufReader::new(socket);
                        let mut line = String::new();
                        request.read_line(&mut line).await.unwrap();
                        assert_eq!(line, "GET / HTTP/1.1\r\n");
                        while line != "\r\n" {
                            line.clear();
                            assert_ne!(request.read_line(&mut line).await.unwrap(), 0);
                        }
                        mock_response(request.into_inner(), "mock engine").await;
                    };
                    let (_, status) = tokio::join!(serve, child.wait());
                    assert!(status.unwrap().success());
                })
                .await
                .expect("子の通信試験が時間内に終わりませんでした");
            });
    }

    fn assignment() -> Assignment {
        serde_json::from_value(json!({
            "assignment_id": "0123456789abcdef0123456789abcdef",
            "deadline_seconds": 120,
            "input": {"text":"Hello", "target_lang":"ja"}
        }))
        .unwrap()
    }

    #[test]
    fn assignment_uses_shared_prompt_and_rejects_unrecognized_instructions() {
        let mut item = assignment();
        assert_eq!(
            assignment_prompt(&item).unwrap(),
            prompt(&item.input).unwrap()
        );
        item.input["engine_url"] = json!("https://example.com");
        assert!(assignment_prompt(&item).is_err());
        let mut item = assignment();
        item.input["history"] = json!([{"source":"a","target":"b","extra":"c"}]);
        assert!(assignment_prompt(&item).is_err());
    }

    #[test]
    fn assignment_rejects_oversize_text_reserved_tokens_and_deadline() {
        let mut item = assignment();
        item.input["text"] = json!("あ".repeat(4001));
        assert!(assignment_prompt(&item).is_err());
        item.input["text"] = json!("<|plamo:msg|>");
        assert!(assignment_prompt(&item).is_err());
        item.input["text"] = json!("Hello");
        item.deadline_seconds = 121;
        assert!(assignment_prompt(&item).is_err());
    }

    async fn mock_request(
        listener: &tokio::net::TcpListener,
    ) -> (tokio::net::TcpStream, String, Value) {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

        let (socket, _) = listener.accept().await.unwrap();
        let mut input = BufReader::new(socket);
        let mut line = String::new();
        input.read_line(&mut line).await.unwrap();
        let route = line.split_whitespace().nth(1).unwrap().to_owned();
        let mut length = 0;
        loop {
            line.clear();
            assert_ne!(input.read_line(&mut line).await.unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut bytes = vec![0; length];
        input.read_exact(&mut bytes).await.unwrap();
        (
            input.into_inner(),
            route,
            serde_json::from_slice(&bytes).unwrap(),
        )
    }

    async fn mock_response(mut socket: tokio::net::TcpStream, body: &str) {
        use tokio::io::AsyncWriteExt;
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn poll_distinguishes_rejection_from_temporary_errors_and_quarantine() {
        use tokio::io::AsyncWriteExt;
        let broker = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut agent = Agent::new(Bootstrap {
            broker: "https://example.com/".into(),
            worker_secret: format!("ptw_{}", "a".repeat(64)),
            model_sha256: "b".repeat(64),
            agent_version: "0.5.0".into(),
            engine_port: 1,
            engine_secret: "mock".into(),
            relay_port: None,
            relay_secret: None,
        })
        .ok()
        .unwrap();
        agent.broker = Url::parse(&format!("http://{}/", broker.local_addr().unwrap())).unwrap();
        for (response, expected) in [
            (
                "204 No Content\r\nX-PTranslate-Worker-State: quarantine",
                Ok("quarantined"),
            ),
            ("401 Unauthorized", Ok("rejected")),
            ("403 Forbidden", Ok("rejected")),
            ("503 Service Unavailable", Err(())),
        ] {
            let (_parent, closed) = tokio::sync::oneshot::channel();
            let serve = async {
                let (mut socket, route, _) = mock_request(&broker).await;
                assert_eq!(route, "/api/worker/poll");
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 {response}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            };
            let (result, ()) = timeout(Duration::from_secs(5), async {
                tokio::join!(agent.run(closed, std::future::ready(Err(()))), serve)
            })
            .await
            .unwrap();
            assert_eq!(result, expected);
        }
    }

    async fn mock_assignment(stop: bool, allowed: bool) {
        let broker = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let engine = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut agent = Agent::new(Bootstrap {
            broker: "https://example.com/".into(),
            worker_secret: format!("ptw_{}", "a".repeat(64)),
            model_sha256: "b".repeat(64),
            agent_version: "0.5.0".into(),
            engine_port: engine.local_addr().unwrap().port(),
            engine_secret: "test-engine-secret".into(),
            relay_port: None,
            relay_secret: None,
        })
        .ok()
        .unwrap();
        // 本体はHTTPS以外を拒否する。試験だけループバックのHTTPモックに差し替える。
        agent.broker = Url::parse(&format!("http://{}/", broker.local_addr().unwrap())).unwrap();
        let (_parent, receiver) = tokio::sync::oneshot::channel();
        let broker_work = async {
            let (socket, route, body) = mock_request(&broker).await;
            assert_eq!(route, "/api/worker/poll");
            assert_eq!(body["model"], "b".repeat(64));
            assert_eq!(body["agent_version"], "0.5.0");
            mock_response(
                socket,
                &json!({
                    "assignment_id": "0123456789abcdef0123456789abcdef",
                    "deadline_seconds": 10,
                    "input": {"text":"Hello","target_lang":"ja"}
                })
                .to_string(),
            )
            .await;
            let (socket, route, body) = mock_request(&broker).await;
            assert_eq!(route, "/api/worker/result");
            assert_eq!(body["seq"], 0);
            if !allowed {
                assert_eq!(body["error"], "engine");
                mock_response(socket, r#"{"stop":false}"#).await;
                return;
            }
            assert_eq!(body["text"], "こんにちは");
            assert_eq!(body["done"], false);
            mock_response(socket, &json!({"stop":stop}).to_string()).await;
            if !stop {
                let (socket, route, body) = mock_request(&broker).await;
                assert_eq!(route, "/api/worker/result");
                assert_eq!(body["seq"], 1);
                assert_eq!(body["finish"], "stop");
                assert_eq!(body["done"], true);
                mock_response(socket, r#"{"stop":false}"#).await;
            }
        };
        let engine_work = async {
            if !allowed {
                assert!(timeout(Duration::from_millis(100), engine.accept())
                    .await
                    .is_err());
                return;
            }
            let (socket, route, body) = mock_request(&engine).await;
            assert_eq!(route, "/v1/completions");
            assert_eq!(body["temperature"], 0);
            assert_eq!(body["cache_prompt"], false);
            assert_eq!(body["max_tokens"], 256);
            assert_eq!(body["prompt"], prompt(&assignment().input).unwrap().0);
            let wire = format!(
                "data: {}\n\ndata: {}\n\n",
                json!({"choices":[{"text":"こんにちは"}]}),
                json!({"choices":[{"finish_reason":"stop"}]})
            );
            mock_response(socket, &wire).await;
        };
        let (result, _, _) = timeout(Duration::from_secs(5), async {
            tokio::join!(
                agent.run(
                    receiver,
                    std::future::ready(allowed.then_some(()).ok_or(()))
                ),
                broker_work,
                engine_work
            )
        })
        .await
        .unwrap();
        if allowed {
            assert_eq!(result.unwrap(), if stop { "paused" } else { "completed" });
        } else {
            assert!(result.is_err());
        }
    }

    #[tokio::test]
    async fn denied_assignment_is_returned_without_starting_the_engine() {
        mock_assignment(false, false).await;
    }

    #[tokio::test]
    async fn worker_relays_one_assignment_to_a_local_engine() {
        mock_assignment(false, true).await;
    }

    #[tokio::test]
    async fn worker_stops_when_broker_refuses_further_results() {
        mock_assignment(true, true).await;
    }

    #[test]
    fn broker_is_a_verified_https_origin() {
        assert!(broker_origin("https://example.com:8443/").is_ok());
        for url in [
            "http://example.com/",
            "https://user:secret@example.com/",
            "https://example.com/extra",
            "https://example.com/?extra",
            "https://example.com/#extra",
        ] {
            assert!(broker_origin(url).is_err());
        }
    }
}
