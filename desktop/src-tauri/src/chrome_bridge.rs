//! Chromeの起動する同じEXEを、中継専用モードで使う。
//! 原文・訳は保存せず、利用者限定の名前付きパイプで起動中のアプリへ渡す。
use tauri::Manager;

#[derive(Default)]
pub struct Status(std::sync::Mutex<Option<String>>);

pub fn error(app: &tauri::AppHandle) -> Option<String> {
    app.state::<Status>().0.lock().unwrap().clone()
}

#[cfg(target_os = "windows")]
fn unavailable(app: &tauri::AppHandle) {
    *app.state::<Status>().0.lock().unwrap() = Some(
        "Chrome連携を起動できませんでした。同じアプリが起動中なら終了してから、このアプリを起動し直してください。デスクトップでの翻訳は利用できます。".into(),
    );
}

#[tauri::command]
pub fn register_chrome(window: tauri::WebviewWindow, extension_id: String) -> Result<(), String> {
    if window.label() != "main" {
        return Err("メイン画面から操作してください".into());
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(error) = error(window.app_handle()) {
            return Err(error);
        }
        windows::register(&extension_id).map_err(|_| "Chrome連携の登録に失敗しました".into())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = extension_id;
        Err("Chromeのローカル翻訳はWindowsに対応しています".into())
    }
}

#[cfg(target_os = "windows")]
pub use windows::{host_entry, schedule};

#[cfg(target_os = "windows")]
mod windows {
    use crate::{local, Message};
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::{
        io::{self, Read, Write},
        path::PathBuf,
    };
    use tauri::{AppHandle, Manager};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::windows::named_pipe::{ClientOptions, ServerOptions},
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, LocalFree},
        Security::{Authorization::*, *},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    const HOST: &str = "com.witenp.ptranslate";
    const MAX_MESSAGE: usize = 256 * 1024;
    fn manifest_path() -> io::Result<PathBuf> {
        Ok(PathBuf::from(
            std::env::var_os("LOCALAPPDATA")
                .ok_or_else(|| io::Error::other("保存先がありません"))?,
        )
        .join("PTranslateChrome")
        .join("native-host.json"))
    }
    fn user_sid() -> io::Result<String> {
        unsafe {
            let mut token = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut size = 0;
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut size);
            // TOKEN_USERのアラインメントを満たす領域。
            let mut buf = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
            let ok =
                GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), size, &mut size);
            CloseHandle(token);
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            let user = &*(buf.as_ptr() as *const TOKEN_USER);
            let mut ptr = std::ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut ptr) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut len = 0;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            LocalFree(ptr.cast());
            Ok(sid)
        }
    }
    fn pipe_name() -> io::Result<String> {
        let mut hash = Sha256::new();
        hash.update(user_sid()?.as_bytes());
        hash.update(std::env::current_exe()?.to_string_lossy().as_bytes());
        Ok(format!(r"\\.\pipe\ptranslate-{:x}", hash.finalize()))
    }
    fn create_pipe(first: bool) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
        // 現在の利用者にだけ接続とパイプ作成を許す。リモート接続は拒否する。
        let sddl: Vec<u16> = format!("D:P(A;;GA;;;{})\0", user_sid()?)
            .encode_utf16()
            .collect();
        unsafe {
            let mut descriptor = std::ptr::null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let mut attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            };
            let result = ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(
                    pipe_name()?,
                    (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
                );
            LocalFree(descriptor);
            result
        }
    }
    pub fn register(id: &str) -> io::Result<()> {
        if id.len() != 32 || !id.bytes().all(|b| (b'a'..=b'p').contains(&b)) {
            return Err(io::Error::other("拡張IDが不正です"));
        }
        let path = manifest_path()?;
        std::fs::create_dir_all(path.parent().unwrap())?;
        let manifest = json!({"name":HOST,"description":"PTranslateのローカル翻訳", "path":std::env::current_exe()?, "type":"stdio", "allowed_origins":[format!("chrome-extension://{id}/")]});
        std::fs::write(&path, serde_json::to_vec_pretty(&manifest)?)?;
        let status = std::process::Command::new("reg.exe")
            .args([
                "add",
                &format!(r"HKCU\Software\Google\Chrome\NativeMessagingHosts\{HOST}"),
                "/ve",
                "/t",
                "REG_SZ",
                "/d",
            ])
            .arg(path)
            .arg("/f")
            .creation_flags_hidden()
            .status()?;
        if !status.success() {
            return Err(io::Error::other("登録できませんでした"));
        }
        Ok(())
    }
    trait Hidden {
        fn creation_flags_hidden(&mut self) -> &mut Self;
    }
    impl Hidden for std::process::Command {
        fn creation_flags_hidden(&mut self) -> &mut Self {
            use std::os::windows::process::CommandExt;
            self.creation_flags(0x08000000)
        }
    }
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
    fn unregister() -> io::Result<()> {
        let path = manifest_path()?;
        if !path.exists() {
            return Ok(());
        }
        let config: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        // 通常版・試作版など別のEXEが登録した連携には触れない。
        if config["path"] != json!(std::env::current_exe()?) {
            return Ok(());
        }
        let status = std::process::Command::new("reg.exe")
            .args([
                "delete",
                &format!(r"HKCU\Software\Google\Chrome\NativeMessagingHosts\{HOST}"),
                "/f",
            ])
            .creation_flags_hidden()
            .status()?;
        if !status.success() {
            return Err(io::Error::other("登録を解除できませんでした"));
        }
        std::fs::remove_file(path)
    }
    pub fn host_entry() -> bool {
        let Some(origin) = std::env::args().nth(1) else {
            return false;
        };
        if origin == "--unregister-chrome" {
            std::process::exit(if unregister().is_ok() { 0 } else { 1 });
        }
        if !origin.starts_with("chrome-extension://") {
            return false;
        }
        let result = (|| -> io::Result<()> {
            let config: Value = serde_json::from_slice(&std::fs::read(manifest_path()?)?)?;
            if config["allowed_origins"] != json!([origin])
                || config["path"] != json!(std::env::current_exe()?)
            {
                return Err(io::Error::other("許可されていない拡張です"));
            }
            let rt = tokio::runtime::Runtime::new()?;
            let result = rt.block_on(async {
                let name = pipe_name()?;
                let started = std::time::Instant::now();
                let pipe = loop {
                    match ClientOptions::new().open(&name) {
                        Ok(pipe) => break pipe,
                        Err(error) if error.raw_os_error() == Some(231) && started.elapsed().as_secs() < 2 => {
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        }
                        Err(error) => return Err(error),
                    }
                };
                let (mut reader, mut writer) = tokio::io::split(pipe);
                // stdinのEOFをすぐパイプへ伝え、中止した推論を残さない。
                let incoming = tokio::task::spawn_blocking(|| read_frame(&mut io::stdin())).await??;
                writer.write_all(&(incoming.len() as u32).to_le_bytes()).await?;
                writer.write_all(&incoming).await?;
                let mut eof = tokio::task::spawn_blocking(|| { let mut b = [0]; let _ = io::stdin().read(&mut b); });
                loop {
                    let mut header = [0; 4];
                    tokio::select! { _ = &mut eof => return Ok::<(), io::Error>(()), result = reader.read_exact(&mut header) => { result?; } }
                    let len = u32::from_le_bytes(header) as usize;
                    if len == 0 || len > MAX_MESSAGE { return Err(io::Error::other("応答が大きすぎます")); }
                    let mut body = vec![0;len]; reader.read_exact(&mut body).await?;
                    write_frame(&mut io::stdout(), &body)?;
                    let message: Value = serde_json::from_slice(&body)?;
                    if matches!(message["kind"].as_str(), Some("end" | "error" | "canceled")) { return Ok(()); }
                }
            });
            rt.shutdown_background();
            result
        })();
        if result.is_err() {
            let _ = write_frame(&mut io::stdout(), br#"{"kind":"error"}"#);
        }
        // stdin待ちのスレッドを残さず終了する。翻訳内容を標準エラーにも出さない。
        std::process::exit(if result.is_ok() { 0 } else { 1 });
    }
    struct Output(tokio::sync::mpsc::Sender<Message>);
    impl local::Output for Output {
        fn send(&self, message: Message) -> local::SendResult<'_> {
            Box::pin(async move { self.0.send(message).await.map_err(|_| ()) })
        }
    }
    pub fn schedule(app: &AppHandle) {
        let mut listener = match tauri::async_runtime::block_on(async { create_pipe(true) }) {
            Ok(listener) => listener,
            Err(_) => {
                super::unavailable(app);
                return;
            }
        };
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                if listener.connect().await.is_err() {
                    super::unavailable(&app);
                    break;
                }
                let next = match create_pipe(false) {
                    Ok(p) => p,
                    Err(_) => {
                        super::unavailable(&app);
                        break;
                    }
                };
                let pipe = std::mem::replace(&mut listener, next);
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = serve(app, pipe).await;
                });
            }
        });
    }
    async fn serve(
        app: AppHandle,
        pipe: tokio::net::windows::named_pipe::NamedPipeServer,
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
                return;
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
        tokio::select! { _ = disconnected => Ok(()), result = async { tokio::join!(work, send).1 } => result }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
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
        #[tokio::test]
        async fn user_only_pipe_accepts_local_client_and_detects_disconnect() {
            let pipe = create_pipe(true).unwrap();
            assert!(create_pipe(true).is_err());
            let client = ClientOptions::new().open(pipe_name().unwrap()).unwrap();
            pipe.connect().await.unwrap();
            let (mut reader, _) = tokio::io::split(pipe);
            drop(client);
            let mut byte = [0];
            let result =
                tokio::time::timeout(std::time::Duration::from_secs(1), reader.read(&mut byte))
                    .await
                    .unwrap();
            assert!(matches!(result, Ok(0)) || result.is_err());
        }
    }
}
