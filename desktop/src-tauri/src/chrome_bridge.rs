//! Chromeの起動する同じEXEを、中継専用モードで使う。
//! 原文・訳は保存せず、利用者限定の名前付きパイプで起動中のアプリへ渡す。
use tauri::Manager;

#[derive(Default)]
pub struct Status {
    error: std::sync::Mutex<Option<String>>,
    pending: std::sync::Mutex<Option<RegistrationRequest>>,
}

#[derive(Clone, serde::Serialize)]
pub struct RegistrationRequest {
    token: String,
    extension_id: String,
}

#[cfg(any(target_os = "windows", test))]
fn extension_id_from_url(input: &str) -> Option<String> {
    if input.len() > 256 {
        return None;
    }
    let url = reqwest::Url::parse(input).ok()?;
    if url.scheme() != "ptranslate"
        || url.host_str() != Some("register-chrome")
        || !matches!(url.path(), "" | "/")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let pairs: Vec<_> = url.query_pairs().collect();
    if pairs.len() != 1 || pairs[0].0 != "extension_id" {
        return None;
    }
    let id = &pairs[0].1;
    (id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))).then(|| id.to_string())
}

impl Status {
    #[cfg(any(target_os = "windows", test))]
    fn offer(&self, input: &str) -> bool {
        let Some(extension_id) = extension_id_from_url(input) else {
            return false;
        };
        let mut pending = self.pending.lock().unwrap();
        // 確認中のIDを後から届いたURLで差し替えない。
        if pending.is_some() {
            return false;
        }
        let mut secret = [0u8; 32];
        if getrandom::fill(&mut secret).is_err() {
            return false;
        }
        use base64::Engine;
        *pending = Some(RegistrationRequest {
            token: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret),
            extension_id,
        });
        true
    }
    fn take(&self, token: &str) -> Result<RegistrationRequest, String> {
        let mut pending = self.pending.lock().unwrap();
        if !pending.as_ref().is_some_and(|r| r.token == token) {
            return Err("登録の確認が失効しました。拡張の設定からやり直してください".into());
        }
        Ok(pending.take().unwrap())
    }
}

#[tauri::command]
pub fn chrome_registration_request(
    window: tauri::WebviewWindow,
) -> Result<Option<RegistrationRequest>, String> {
    if window.label() != "main" {
        return Err("メイン画面から操作してください".into());
    }
    Ok(window.state::<Status>().pending.lock().unwrap().clone())
}

fn finish_registration(
    state: &Status,
    token: &str,
    approve: bool,
    register: impl FnOnce(String) -> Result<(), String>,
) -> Result<(), String> {
    let request = state.take(token)?;
    if approve {
        register(request.extension_id)
    } else {
        Ok(())
    }
}

#[tauri::command]
pub fn finish_chrome_registration(
    window: tauri::WebviewWindow,
    token: String,
    approve: bool,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("メイン画面から操作してください".into());
    }
    finish_registration(&window.state::<Status>(), &token, approve, |id| {
        register_chrome(window.clone(), id)
    })
}

#[cfg(target_os = "windows")]
pub fn setup_links(app: &tauri::AppHandle) {
    use tauri::{Emitter, Manager};
    use tauri_plugin_deep_link::DeepLinkExt;
    let handle = app.clone();
    app.deep_link().on_open_url(move |event| {
        for url in event.urls() {
            if extension_id_from_url(url.as_str()).is_some() {
                let offered = handle.state::<Status>().offer(url.as_str());
                crate::resident::show_main(&handle);
                if offered {
                    let _ = handle.emit_to("main", "chrome-registration-request", ());
                }
                break;
            }
        }
    });
    if let Ok(Some(urls)) = app.deep_link().get_current() {
        for url in urls {
            if app.state::<Status>().offer(url.as_str()) {
                break;
            }
        }
    }
    // 試験用EXEから利用者のプロトコル登録を変更しない。
    #[cfg(not(feature = "smoke-test"))]
    if app.deep_link().register_all().is_err() {
        eprintln!("拡張からデスクトップを開くためのURL登録に失敗しました。拡張IDの手動登録は利用できます。");
    }
}

pub fn error(app: &tauri::AppHandle) -> Option<String> {
    app.state::<Status>().error.lock().unwrap().clone()
}

#[cfg(target_os = "windows")]
fn unavailable(app: &tauri::AppHandle) {
    *app.state::<Status>().error.lock().unwrap() = Some(
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
    fn unregister_protocol() -> io::Result<()> {
        let key = r"Software\Classes\ptranslate";
        let command = windows_registry::CURRENT_USER
            .open(format!(r"{key}\shell\open\command"))
            .and_then(|key| key.get_string(""));
        let expected = format!("\"{}\" \"%1\"", std::env::current_exe()?.display());
        if command.is_ok_and(|command| command == expected) {
            windows_registry::CURRENT_USER
                .remove_tree(key)
                .map_err(io::Error::other)?;
        }
        Ok(())
    }
    fn unregister() -> io::Result<()> {
        unregister_protocol()?;
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

#[cfg(test)]
mod registration_tests {
    use super::*;
    fn link(id: &str) -> String {
        format!("ptranslate://register-chrome?extension_id={id}")
    }
    #[test]
    fn accepts_only_registration_url_with_one_valid_id() {
        let id = "abcdefghijklmnopabcdefghijklmnop";
        assert_eq!(extension_id_from_url(&link(id)).as_deref(), Some(id));
        for value in [
            "https://register-chrome?extension_id=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ptranslate://other?extension_id=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ptranslate://register-chrome/path?extension_id=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ptranslate://user@register-chrome?extension_id=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ptranslate://register-chrome:80?extension_id=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ptranslate://register-chrome?extension_id=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa#fragment",
            "ptranslate://register-chrome?extension_id=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa&extra=1",
            "ptranslate://register-chrome?extension_id=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa&extension_id=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "ptranslate://register-chrome", "ptranslate://register-chrome?extension_id=",
        ] { assert!(extension_id_from_url(value).is_none(), "{value}"); }
        for id in [
            "a".repeat(31),
            "a".repeat(33),
            "q".repeat(32),
            "A".repeat(32),
            "é".repeat(16),
        ] {
            assert!(extension_id_from_url(&link(&id)).is_none());
        }
    }
    #[test]
    fn only_explicit_approval_registers_and_cancel_consumes_request() {
        let state = Status::default();
        let id = "a".repeat(32);
        state.offer(&link(&id));
        let token = state
            .pending
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .token
            .clone();
        finish_registration(&state, &token, false, |_| {
            panic!("キャンセルで登録しました")
        })
        .unwrap();
        assert!(
            finish_registration(&state, &token, true, |_| panic!("古い要求で登録しました"))
                .is_err()
        );
        state.offer(&link(&id));
        let token = state
            .pending
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .token
            .clone();
        let mut registered = None;
        finish_registration(&state, &token, true, |value| {
            registered = Some(value);
            Ok(())
        })
        .unwrap();
        assert_eq!(registered, Some(id));
        assert!(state.pending.lock().unwrap().is_none());
    }
    #[test]
    fn pending_request_cannot_be_replaced_or_approved_with_stale_token() {
        let state = Status::default();
        assert!(state.offer(&link(&"a".repeat(32))));
        let first = state.pending.lock().unwrap().clone().unwrap();
        assert!(!state.offer(&link(&"b".repeat(32))));
        assert!(state.take("wrong-token").is_err());
        assert_eq!(
            state.take(&first.token).unwrap().extension_id,
            "a".repeat(32)
        );
        assert!(state.take(&first.token).is_err());
        assert!(state.offer(&link(&"b".repeat(32))));
        assert!(state.take(&first.token).is_err());
        let second = state.pending.lock().unwrap().clone().unwrap();
        assert_ne!(first.token, second.token);
        assert_eq!(
            state.take(&second.token).unwrap().extension_id,
            "b".repeat(32)
        );
    }
}
