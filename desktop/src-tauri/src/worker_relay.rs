//! ワーカー向けのTLS中継。要求で接続先を変更できない。
#[cfg(any(not(target_os = "linux"), test))]
use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::Url;
use std::time::Duration;
#[cfg(any(not(target_os = "linux"), test))]
use std::{io, net::Ipv4Addr};
#[cfg(any(not(target_os = "linux"), test))]
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio::{
    net::TcpStream,
    task::{JoinHandle, JoinSet},
    time::timeout,
};
#[cfg(any(not(target_os = "linux"), test))]
use zeroize::Zeroizing;

#[cfg(target_os = "linux")]
pub(crate) mod linux;

#[cfg(any(not(target_os = "linux"), test))]
const MAX_HEADER: usize = 4096;
const MAX_CONNECTIONS: usize = 4;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const PROXY_USER: &str = "worker";

#[cfg(any(not(target_os = "linux"), test))]
pub(crate) struct BrokerRelay {
    pub(crate) port: u16,
    pub(crate) secret: Zeroizing<String>,
    task: JoinHandle<()>,
}

#[cfg(any(not(target_os = "linux"), test))]
impl Drop for BrokerRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(any(not(target_os = "linux"), test))]
impl BrokerRelay {
    pub(crate) async fn start(broker: &Url) -> Result<Self, String> {
        let host = broker.host_str().ok_or("提供先のURLが不正です")?;
        let port = broker
            .port_or_known_default()
            .ok_or("提供先のポートが不正です")?;
        let authority = format!("{host}:{port}");
        let host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        let mut random = Zeroizing::new([0; 32]);
        getrandom::fill(random.as_mut()).map_err(|_| "中継の資格を生成できませんでした")?;
        let secret = Zeroizing::new(STANDARD.encode(random.as_slice()));
        let credentials = Zeroizing::new(format!("{PROXY_USER}:{}", secret.as_str()));
        let encoded = Zeroizing::new(STANDARD.encode(credentials.as_bytes()));
        let authorization = Zeroizing::new(format!("Basic {}", encoded.as_str()));
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| "提供先への中継を開始できませんでした")?;
        let local_port = listener
            .local_addr()
            .map_err(|_| "中継のポートを取得できませんでした")?
            .port();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    result = listener.accept(), if connections.len() < MAX_CONNECTIONS => {
                        let Ok((socket, _)) = result else { break };
                        let host = host.clone();
                        let authority = authority.clone();
                        let authorization = authorization.clone();
                        connections.spawn(async move {
                            let _ = forward(socket, &host, port, &authority, &authorization).await;
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Ok(Self {
            port: local_port,
            secret,
            task,
        })
    }
}

#[cfg(any(not(target_os = "linux"), test))]
async fn forward(
    mut worker: TcpStream,
    host: &str,
    port: u16,
    authority: &str,
    authorization: &str,
) -> io::Result<()> {
    let mut broker = timeout(CONNECT_TIMEOUT, async {
        let mut header = Zeroizing::new(Vec::new());
        // CONNECT応答より前にはTLSを始めないので、ヘッダー以降は先読みしない。
        while !header.ends_with(b"\r\n\r\n") && header.len() < MAX_HEADER {
            header.push(worker.read_u8().await?);
        }
        let accepted = std::str::from_utf8(&header).ok().is_some_and(|header| {
            if !header.ends_with("\r\n\r\n") {
                return false;
            }
            let mut lines = header.split("\r\n");
            if lines.next() != Some(format!("CONNECT {authority} HTTP/1.1").as_str()) {
                return false;
            }
            lines.any(|line| {
                line.split_once(':').is_some_and(|(name, value)| {
                    name.eq_ignore_ascii_case("Proxy-Authorization")
                        && value.trim() == authorization
                })
            })
        });
        if !accepted {
            worker
                .write_all(
                    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await?;
            return Err(io::Error::other("許可されていない中継要求"));
        }
        let broker = TcpStream::connect((host, port)).await?;
        worker
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        Ok::<_, io::Error>(broker)
    })
    .await??;
    tokio::io::copy_bidirectional(&mut worker, &mut broker).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn https_client_sends_tls_through_authenticated_relay() {
        let broker = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let url = Url::parse(&format!("https://{}/", broker.local_addr().unwrap())).unwrap();
        let relay = BrokerRelay::start(&url).await.unwrap();
        let proxy = reqwest::Proxy::https(format!("http://127.0.0.1:{}", relay.port))
            .unwrap()
            .basic_auth(PROXY_USER, relay.secret.as_str());
        let client = reqwest::Client::builder()
            .no_proxy()
            .proxy(proxy)
            .build()
            .unwrap();
        let (response, record_type) = timeout(CONNECT_TIMEOUT, async {
            tokio::join!(client.get(url).send(), async {
                let (mut socket, _) = broker.accept().await.unwrap();
                socket.read_u8().await.unwrap()
            })
        })
        .await
        .unwrap();
        assert_eq!(record_type, 0x16); // TLS handshake
        assert!(response.is_err()); // モックはTLSの応答を返さない。
    }

    #[tokio::test]
    async fn relay_rejects_other_destinations_and_forwards_only_to_configured_broker() {
        let broker = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = broker.local_addr().unwrap();
        let relay = BrokerRelay::start(&Url::parse(&format!("https://{address}/")).unwrap())
            .await
            .unwrap();
        let auth = STANDARD.encode(format!("{PROXY_USER}:{}", relay.secret.as_str()));
        for (target, credential) in [
            ("other.example:443".into(), auth.as_str()),
            (address.to_string(), "wrong"),
        ] {
            let mut socket = TcpStream::connect((Ipv4Addr::LOCALHOST, relay.port))
                .await
                .unwrap();
            socket.write_all(format!("CONNECT {target} HTTP/1.1\r\nProxy-Authorization: Basic {credential}\r\n\r\n").as_bytes()).await.unwrap();
            let mut response = Vec::new();
            socket.read_to_end(&mut response).await.unwrap();
            assert!(response.starts_with(b"HTTP/1.1 403"));
        }
        let mut worker = TcpStream::connect((Ipv4Addr::LOCALHOST, relay.port))
            .await
            .unwrap();
        worker
            .write_all(
                format!("CONNECT {address} HTTP/1.1\r\nProxy-Authorization: Basic {auth}\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut response = [0; b"HTTP/1.1 200 Connection Established\r\n\r\n".len()];
        worker.read_exact(&mut response).await.unwrap();
        assert_eq!(&response, b"HTTP/1.1 200 Connection Established\r\n\r\n");
        let (mut server, _) = broker.accept().await.unwrap();
        worker.write_all(b"mock TLS").await.unwrap();
        let mut wire = [0; 8];
        server.read_exact(&mut wire).await.unwrap();
        assert_eq!(&wire, b"mock TLS");
    }
}
