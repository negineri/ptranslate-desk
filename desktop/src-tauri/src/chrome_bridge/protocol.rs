use crate::{local, Message};
use serde_json::Value;
use std::io::{self, Read, Write};
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub(super) const HOST: &str = "com.witenp.ptranslate";
const MAX_MESSAGE: usize = 256 * 1024;

fn read_frame(input: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut header = [0; 4];
    input.read_exact(&mut header)?;
    let len = u32::from_le_bytes(header) as usize;
    if len == 0 || len > MAX_MESSAGE {
        return Err(io::Error::other("メッセージが大きすぎます"));
    }
    let mut body = vec![0; len];
    input.read_exact(&mut body)?;
    Ok(body)
}
fn write_frame(output: &mut impl Write, body: &[u8]) -> io::Result<()> {
    if body.len() > MAX_MESSAGE {
        return Err(io::Error::other("メッセージが大きすぎます"));
    }
    output.write_all(&(body.len() as u32).to_le_bytes())?;
    output.write_all(body)?;
    output.flush()
}

pub(super) fn run_host<S: AsyncRead + AsyncWrite + Unpin>(
    connect: impl std::future::Future<Output = io::Result<S>>,
) -> io::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let result = rt.block_on(async {
        let (mut reader, mut writer) = tokio::io::split(connect.await?);
        let incoming = tokio::task::spawn_blocking(|| read_frame(&mut io::stdin())).await??;
        writer
            .write_all(&(incoming.len() as u32).to_le_bytes())
            .await?;
        writer.write_all(&incoming).await?;
        // Chromeがポートを閉じたらソケットも閉じて推論を中止する。
        let mut eof = tokio::task::spawn_blocking(|| {
            let mut byte = [0];
            let _ = io::stdin().read(&mut byte);
        });
        loop {
            let mut header = [0; 4];
            tokio::select! {
                _ = &mut eof => return Ok(()),
                result = reader.read_exact(&mut header) => { result?; }
            }
            let len = u32::from_le_bytes(header) as usize;
            if len == 0 || len > MAX_MESSAGE {
                return Err(io::Error::other("応答のサイズが不正です"));
            }
            let mut body = vec![0; len];
            tokio::select! {
                _ = &mut eof => return Ok(()),
                result = reader.read_exact(&mut body) => { result?; }
            }
            write_frame(&mut io::stdout(), &body)?;
            let message: Value = serde_json::from_slice(&body)?;
            if matches!(message["kind"].as_str(), Some("end" | "error" | "canceled")) {
                return Ok(());
            }
        }
    });
    rt.shutdown_background();
    result
}

pub(super) fn exit_host(result: io::Result<()>) -> ! {
    if result.is_err() {
        let _ = write_frame(&mut io::stdout(), br#"{"kind":"error"}"#);
    }
    // stdin待ちのスレッドを残さず終了する。翻訳内容をログに出さない。
    std::process::exit(if result.is_ok() { 0 } else { 1 });
}

struct Output(tokio::sync::mpsc::Sender<Message>);
impl local::Output for Output {
    fn send(&self, message: Message) -> local::SendResult<'_> {
        Box::pin(async move { self.0.send(message).await.map_err(|_| ()) })
    }
}
pub(super) async fn serve(
    app: AppHandle,
    pipe: impl AsyncRead + AsyncWrite + Unpin,
) -> io::Result<()> {
    let (mut reader, mut writer) = tokio::io::split(pipe);
    let mut header = [0; 4];
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        reader.read_exact(&mut header),
    )
    .await??;
    let len = u32::from_le_bytes(header) as usize;
    if len == 0 || len > MAX_MESSAGE {
        return Err(io::Error::other("不正な要求です"));
    }
    let mut body = vec![0; len];
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        reader.read_exact(&mut body),
    )
    .await??;
    let request: Value = serde_json::from_slice(&body)?;
    let route = request["route"].as_str().unwrap_or("");
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let output = Output(tx.clone());
    let work = async {
        if request["version"] != 1
            || !matches!(route, "api/config" | "api/translate")
            || !app.state::<local::Local>().enabled()
        {
            let _ = tx.send(Message::Error).await;
            return Ok(());
        }
        let result = local::request(
            &app,
            route,
            request.get("body").filter(|v| !v.is_null()),
            &output,
        )
        .await;
        let _ = tx
            .send(if result.is_ok() {
                Message::End
            } else {
                Message::Error
            })
            .await;
        Ok(())
    };
    let send = async {
        while let Some(msg) = rx.recv().await {
            let terminal = matches!(msg, Message::End | Message::Error | Message::Canceled);
            let body = serde_json::to_vec(&msg)?;
            if body.len() > MAX_MESSAGE {
                return Err(io::Error::other("応答が大きすぎます"));
            }
            writer.write_all(&(body.len() as u32).to_le_bytes()).await?;
            writer.write_all(&body).await?;
            if terminal {
                return Ok::<(), io::Error>(());
            }
        }
        Ok(())
    };
    let disconnected = async {
        let mut byte = [0];
        let _ = reader.read(&mut byte).await;
    };
    tokio::select! { _ = disconnected => Ok(()), result = async { tokio::try_join!(work, send).map(|_| ()) } => result }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn burst_waits_for_capacity_and_preserves_all_events_in_order() {
        use local::Output as _;
        let (tx, mut rx) = tokio::sync::mpsc::channel(256);
        let output = Output(tx);
        // 1回の読み取りに300イベントがまとめて届く場合。
        let wire: String = (0..300)
            .map(|i| format!("data: {}\n\n", json!({"choices":[{"text":i.to_string()}]})))
            .collect();
        let events = local::Decoder::default().feed(wire.as_bytes()).unwrap();
        assert_eq!(events.len(), 300);
        let producer = tokio::spawn(async move {
            for event in events {
                output
                    .send(Message::Bytes {
                        bytes: serde_json::to_vec(&event).unwrap(),
                    })
                    .await
                    .unwrap();
            }
            output.send(Message::End).await.unwrap();
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while rx.len() < 256 {
                assert!(!producer.is_finished());
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(rx.len(), 256);
        assert!(!producer.is_finished());
        for i in 0..300 {
            let Some(Message::Bytes { bytes }) = rx.recv().await else {
                panic!("イベントが欠落しました")
            };
            assert_eq!(
                serde_json::from_slice::<Value>(&bytes).unwrap(),
                json!({"t":i.to_string()})
            );
        }
        assert!(matches!(rx.recv().await, Some(Message::End)));
        producer.await.unwrap();
    }

    #[tokio::test]
    async fn disconnect_interrupts_output_waiting_for_capacity() {
        use local::Output as _;
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let output = Output(tx);
        output.send(Message::Start { status: 200 }).await.unwrap();
        let pending = output.send(Message::End);
        tokio::pin!(pending);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut pending)
                .await
                .is_err()
        );
        drop(rx);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), pending)
                .await
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn frames_reject_invalid_sizes_and_preserve_unicode() {
        let body = serde_json::to_vec(&json!({"text":"日本語"})).unwrap();
        let mut wire = Vec::new();
        write_frame(&mut wire, &body).unwrap();
        assert_eq!(read_frame(&mut &wire[..]).unwrap(), body);
        assert!(read_frame(&mut &0u32.to_le_bytes()[..]).is_err());
        assert!(read_frame(&mut &((MAX_MESSAGE + 1) as u32).to_le_bytes()[..]).is_err());
        assert!(read_frame(&mut &wire[..wire.len() - 1]).is_err());
    }
}
