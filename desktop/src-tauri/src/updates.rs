//! 更新の確認と、利用者が承認した更新だけを適用する。
use serde::Serialize;
use std::{sync::Mutex, time::Duration};
use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewWindow};
use tauri_plugin_updater::{Update, UpdaterExt};

const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Serialize)]
pub struct UpdateStatus {
    phase: &'static str,
    current_version: String,
    version: Option<String>,
    message: String,
    downloaded: u64,
    total: Option<u64>,
}

#[derive(Default)]
pub struct Updates {
    operation: tokio::sync::Mutex<()>,
    pending: Mutex<Option<Update>>,
    status: Mutex<Option<UpdateStatus>>,
}

fn set_status<R: Runtime>(app: &AppHandle<R>, status: UpdateStatus) {
    *app.state::<Updates>().status.lock().unwrap() = Some(status.clone());
    let _ = app.emit_to("main", "update-status", status);
}

fn initial_status<R: Runtime>(app: &AppHandle<R>) -> UpdateStatus {
    UpdateStatus {
        phase: "idle",
        current_version: app.package_info().version.to_string(),
        version: None,
        message: "更新を確認できます".into(),
        downloaded: 0,
        total: None,
    }
}

#[tauri::command]
pub fn update_status(app: AppHandle) -> UpdateStatus {
    app.state::<Updates>()
        .status
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| initial_status(&app))
}

pub async fn check<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let manager = app.state::<Updates>();
    let _operation = manager
        .operation
        .try_lock()
        .map_err(|_| "更新の確認またはインストール中です")?;
    if manager
        .status
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|s| s.phase == "restart_required")
    {
        return Ok(());
    }
    let mut status = initial_status(app);
    *manager.pending.lock().unwrap() = None;
    status.phase = "checking";
    status.message = "更新を確認しています".into();
    set_status(app, status.clone());
    let result = async {
        app.updater_builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| "更新の設定を読み込めませんでした")?
            .check()
            .await
            .map_err(|_| "更新情報を取得できませんでした")
    }
    .await;
    match result {
        Ok(Some(update)) => {
            if update.download_url.scheme() != "https" {
                status.phase = "error";
                status.message = "更新ファイルの URL は HTTPS が必要です".into();
            } else {
                status.phase = "available";
                status.version = Some(update.version.clone());
                status.message = format!("バージョン {} を利用できます", update.version);
                *manager.pending.lock().unwrap() = Some(update);
            }
        }
        Ok(None) => {
            *manager.pending.lock().unwrap() = None;
            status.phase = "up_to_date";
            status.message = "最新のバージョンです".into();
        }
        Err(error) => {
            status.phase = "error";
            status.message = error.into();
        }
    }
    let error = (status.phase == "error").then(|| status.message.clone());
    set_status(app, status);
    if let Some(error) = error {
        Err(error)
    } else {
        Ok(())
    }
}

#[tauri::command]
pub async fn check_updates(app: AppHandle, window: WebviewWindow) -> Result<(), String> {
    require_main(&window)?;
    check(&app).await
}

fn require_main(window: &WebviewWindow) -> Result<(), String> {
    if window.label() == "main" {
        Ok(())
    } else {
        Err("更新の操作はメインウィンドウで行ってください".into())
    }
}

fn approved_update(
    pending: &mut Option<Update>,
    version: &str,
    approved: bool,
) -> Result<Update, String> {
    if !approved {
        return Err("更新のインストールには利用者の承認が必要です".into());
    }
    let update = pending
        .as_ref()
        .ok_or("更新を確認してからインストールしてください")?;
    if update.version != version {
        return Err("更新内容が変わりました。もう一度確認してください".into());
    }
    Ok(pending.take().unwrap())
}

#[tauri::command]
pub async fn install_update(
    app: AppHandle,
    window: WebviewWindow,
    version: String,
    approved: bool,
) -> Result<(), String> {
    require_main(&window)?;
    install(&app, &version, approved).await
}

async fn install<R: Runtime>(
    app: &AppHandle<R>,
    version: &str,
    approved: bool,
) -> Result<(), String> {
    let manager = app.state::<Updates>();
    let _operation = manager
        .operation
        .try_lock()
        .map_err(|_| "更新の確認またはインストール中です")?;
    let update = approved_update(&mut manager.pending.lock().unwrap(), version, approved)?;
    let mut status = initial_status(app);
    status.phase = "installing";
    status.version = Some(version.into());
    status.message = "更新をダウンロードしてインストールしています".into();
    set_status(&app, status.clone());
    let mut downloaded = 0;
    let result = update
        .download_and_install(
            |length, total| {
                downloaded += length as u64;
                let mut progress = status.clone();
                progress.downloaded = downloaded;
                progress.total = total;
                set_status(&app, progress);
            },
            || {},
        )
        .await;
    match result {
        Ok(()) => {
            status.phase = "restart_required";
            status.message = "更新をインストールしました。再起動すると反映されます".into();
            set_status(&app, status);
            Ok(())
        }
        Err(_) => {
            // 署名の不一致を含め、失敗した更新は適用しない。
            *manager.pending.lock().unwrap() = Some(update);
            status.phase = "error";
            status.message = "更新をインストールできませんでした。署名の検証・通信・書き込み権限を確認し、もう一度更新を確認してください".into();
            let error = status.message.clone();
            set_status(&app, status);
            Err(error)
        }
    }
}

#[tauri::command]
pub fn restart_after_update(app: AppHandle, window: WebviewWindow) -> Result<(), String> {
    require_main(&window)?;
    if !app
        .state::<Updates>()
        .status
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|s| s.phase == "restart_required")
    {
        return Err("再起動が必要な更新はありません".into());
    }
    crate::cancel_all(&app);
    app.restart();
}

pub fn schedule(app: &AppHandle) {
    // モックと開発時は GitHub に接続しない。手動の確認は開発時も利用できる。
    if app.package_info().version.pre.as_str() == "dev"
        || cfg!(debug_assertions)
        || cfg!(feature = "smoke-test")
        || app.state::<crate::Backend>().auth.is_mock()
    {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let _ = check(&app).await;
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    // tauri-plugin-updater の公開テストベクトル。秘密鍵は含まない。
    // https://github.com/tauri-apps/plugins-workspace/blob/v2/plugins/updater/src/updater.rs
    const PUBLIC_KEY: &str = "untrusted comment: minisign public key: A49EF4746FA6E6D7\nRWTX5qZvdPSepDGWg2bz8n6ArQnBMwgQK1geVosorcbLzcunD+KJ+cYe";
    const SIGNATURE: &str = "untrusted comment: signature from tauri secret key\nRUTX5qZvdPSepPF2zsjvP/L/TQtBLRnWRvaq+4omK1UEugtQp/USrGiI1v762sDy3id3aor7bRC9ETyeot6lMHHrWoZ5hjUM1Qc=\ntrusted comment: timestamp:1791649051\tfile:payload\tversion:2.0.0\nDDq2CLOEl5cQmC2eJRHlnZPVViOZsu1CjpPKQoei4A/XBfkDqGO0B+QMrcrHgEQ5qV3mne75tdFWhJpqUXh9AA==";

    #[test]
    fn install_requires_explicit_approval_even_when_no_update_is_pending() {
        assert!(approved_update(&mut None, "1.0.0", false)
            .err()
            .unwrap()
            .contains("承認"));
        assert!(approved_update(&mut None, "1.0.0", true).is_err());
        assert_eq!(CHECK_INTERVAL, Duration::from_secs(86400));
    }

    #[tokio::test]
    async fn check_never_downloads_without_approval_and_signatures_reject_tampering() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let downloads = Arc::new(AtomicUsize::new(0));
        let tampered = Arc::new(AtomicBool::new(false));
        let insecure = Arc::new(AtomicBool::new(false));
        let (count, corrupt, http_url) = (downloads.clone(), tampered.clone(), insecure.clone());
        let task = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let (count, corrupt, http_url) = (count.clone(), corrupt.clone(), http_url.clone());
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        let Ok(byte) = stream.read_u8().await else {
                            return;
                        };
                        request.push(byte);
                    }
                    let package = request.starts_with(b"GET /package ");
                    let body = if package {
                        count.fetch_add(1, Ordering::SeqCst);
                        if corrupt.load(Ordering::SeqCst) {
                            "tampered".into()
                        } else {
                            "test".into()
                        }
                    } else {
                        json!({ "version": "2.0.0", "platforms": { "darwin-aarch64": {
                            "url": if http_url.load(Ordering::SeqCst) { "http://example.invalid/update.app.tar.gz" } else { "https://example.invalid/update.app.tar.gz" },
                            "signature": STANDARD.encode(SIGNATURE)
                        }}}).to_string()
                    };
                    let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context.config_mut().plugins.0.insert(
            "updater".into(),
            json!({
                "pubkey": STANDARD.encode(PUBLIC_KEY), "endpoints": [format!("{url}/latest.json")],
                "dangerousInsecureTransportProtocol": true, "requireSignedVersion": true
            }),
        );
        let app = tauri::test::mock_builder()
            .plugin(
                tauri_plugin_updater::Builder::new()
                    .target("darwin-aarch64")
                    .build(),
            )
            .manage(Updates::default())
            .build(context)
            .unwrap();
        check(app.handle()).await.unwrap();
        assert_eq!(
            app.state::<Updates>()
                .status
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .phase,
            "available"
        );
        assert!(install(app.handle(), "2.0.0", false).await.is_err());
        assert!(install(app.handle(), "3.0.0", true).await.is_err());
        assert_eq!(downloads.load(Ordering::SeqCst), 0);
        let mut update = app
            .state::<Updates>()
            .pending
            .lock()
            .unwrap()
            .clone()
            .unwrap();
        // テストだけローカルに向け、実アプリの置換はせず署名検証までを確認する。
        update.download_url = format!("{url}/package").parse().unwrap();
        let bytes = update.download(|_, _| {}, || {}).await.unwrap();
        assert_eq!(bytes, b"test");
        tampered.store(true, Ordering::SeqCst);
        assert!(update.download(|_, _| {}, || {}).await.is_err());
        insecure.store(true, Ordering::SeqCst);
        assert!(check(app.handle()).await.is_err());
        assert!(app.state::<Updates>().pending.lock().unwrap().is_none());
        task.abort();
    }
}
