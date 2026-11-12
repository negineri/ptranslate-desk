use super::{timeout, JoinHandle, JoinSet, TcpStream, Url, CONNECT_TIMEOUT, MAX_CONNECTIONS};
use std::{fs::Permissions, os::unix::fs::PermissionsExt, path::PathBuf};
use tokio::net::UnixListener;

pub(crate) struct Relays {
    directory: tempfile::TempDir,
    tasks: [JoinHandle<()>; 2],
}

impl Relays {
    pub(crate) fn start(broker: &Url, engine_port: u16) -> Result<Self, String> {
        let host = broker.host_str().ok_or("提供先のURLが不正です")?;
        let port = broker
            .port_or_known_default()
            .ok_or("提供先のポートが不正です")?;
        let directory = tempfile::Builder::new()
            .permissions(Permissions::from_mode(0o700))
            .tempdir()
            .map_err(|_| "ワーカーの中継先を準備できませんでした")?;
        let broker = UnixListener::bind(directory.path().join("broker.sock"))
            .map_err(|_| "提供先への中継を開始できませんでした")?;
        let engine = UnixListener::bind(directory.path().join("engine.sock"))
            .map_err(|_| "推論エンジンへの中継を開始できませんでした")?;
        let tasks = [
            spawn(broker, host.trim_matches(['[', ']']).to_owned(), port),
            spawn(engine, "127.0.0.1".into(), engine_port),
        ];
        Ok(Self { directory, tasks })
    }

    pub(crate) fn paths(&self) -> (PathBuf, PathBuf) {
        let root = self.directory.path();
        (root.join("broker.sock"), root.join("engine.sock"))
    }
}

impl Drop for Relays {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

fn spawn(listener: UnixListener, host: String, port: u16) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                result = listener.accept(), if connections.len() < MAX_CONNECTIONS => {
                    let Ok((mut child, _)) = result else { break };
                    let host = host.clone();
                    connections.spawn(async move {
                        if let Ok(Ok(mut remote)) = timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), port))).await {
                            let _ = tokio::io::copy_bidirectional(&mut child, &mut remote).await;
                        }
                    });
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    })
}
