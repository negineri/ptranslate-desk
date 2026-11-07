#![cfg_attr(
    all(target_os = "windows", not(feature = "smoke-test")),
    windows_subsystem = "windows"
)]

mod auth;
mod connection;
#[cfg(target_os = "macos")]
mod macos;
mod resident;
mod settings;
#[cfg(feature = "smoke-test")]
mod smoke;
mod updates;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(feature = "smoke-test")]
#[tauri::command]
fn smoke_result(app: tauri::AppHandle, passed: bool, step: String) {
    println!(
        "デスクトップ WebView 試験: {}",
        if passed { "PASS" } else { "FAIL" }
    );
    println!("確認項目: {step}");
    if !passed {
        smoke::diagnostic(&app);
    }
    app.exit(if passed { 0 } else { 1 });
}

use reqwest::{Client, Method};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::{async_runtime::JoinHandle, ipc::Channel, AppHandle, Emitter, Manager, WebviewWindow};

#[derive(Default)]
struct Session {
    logged_in: bool,
    requests: HashMap<String, Request>,
}

struct Request {
    owner: String,
    translation: bool,
    task: JoinHandle<()>,
    stream: Channel<Message>,
}

impl Request {
    fn cancel(self) {
        self.task.abort();
        let _ = self.stream.send(Message::Canceled);
    }
}

fn cancel_matching(app: &AppHandle, matches: impl Fn(&Request) -> bool) {
    let backend = app.state::<Backend>();
    let mut session = backend.session.lock().unwrap();
    let ids: Vec<_> = session
        .requests
        .iter()
        .filter(|(_, r)| matches(r))
        .map(|(id, _)| id.clone())
        .collect();
    for id in ids {
        if let Some(request) = session.requests.remove(&id) {
            request.cancel();
        }
    }
}

fn cancel_window(app: &AppHandle, owner: &str) {
    cancel_matching(app, |r| r.owner == owner);
}
fn cancel_all(app: &AppHandle) {
    cancel_matching(app, |_| true);
}

struct Backend {
    auth: Arc<auth::Auth>,
    session: Mutex<Session>,
    auth_error: Mutex<Option<String>>,
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Message {
    Start { status: u16 },
    Bytes { bytes: Vec<u8> },
    End,
    Error,
    Canceled,
    LoginRequired { status: u16 },
}

fn publish_auth(app: &AppHandle, error: Option<String>) {
    let backend = app.state::<Backend>();
    let logged_in = backend.auth.is_authenticated();
    if logged_in || error.is_some() {
        *backend.auth_error.lock().unwrap() = error;
    }
    change_login(app, logged_in);
}

fn change_login(app: &AppHandle, logged_in: bool) {
    {
        let backend = app.state::<Backend>();
        let mut session = backend.session.lock().unwrap();
        session.logged_in = logged_in;
        if !logged_in {
            for (_, request) in session.requests.drain() {
                request.cancel();
            }
        }
    }
    if !logged_in {
        resident::clear_session(app);
    }
    let _ = resident::refresh_menu(app);
    let _ = app.emit("session-changed", ());
}

#[tauri::command]
async fn restore_session(app: AppHandle) -> Result<(), String> {
    let result = app.state::<Backend>().auth.restore().await;
    publish_auth(&app, result.as_ref().err().cloned());
    result
}

#[tauri::command]
async fn login(app: AppHandle) -> Result<auth::LoginReport, String> {
    let auth = app.state::<Backend>().auth.clone();
    if auth.is_busy() {
        return Err("ログインを待っています".into());
    }
    let _ = app.emit("login-started", ());
    let result = async {
        auth.login().await?;
        if auth.is_mock() { return Ok(auth::LoginReport::default()); }
        let report = auth.verify().await?;
        if !report.verified {
            let message = if report.api_status == 401 {
                format!("ログイン後もサーバーが 401 を返しました。サーバー側で利用者名の設定を確認してください。claim の名前: {}", report.claim_names.join(", "))
            } else { "サーバーが 403 を返しました。ログインし直してください".into() };
            return Err(message);
        }
        Ok(report)
    }.await;
    publish_auth(&app, result.as_ref().err().cloned());
    result
}

#[tauri::command]
fn cancel_login(app: AppHandle) {
    app.state::<Backend>().auth.cancel_login();
}

#[tauri::command]
async fn logout(app: AppHandle) -> Result<(), String> {
    let auth = app.state::<Backend>().auth.clone();
    // ネットワークの revoke を待つ間にも画面と翻訳を止める。
    auth.cancel_login();
    change_login(&app, false);
    let result = auth.logout().await;
    if result.is_ok() {
        *app.state::<Backend>().auth_error.lock().unwrap() = None;
    }
    publish_auth(&app, result.as_ref().err().cloned());
    result
}

#[tauri::command]
fn stop_request(app: AppHandle, window: WebviewWindow, id: String) {
    let backend = app.state::<Backend>();
    let mut session = backend.session.lock().unwrap();
    if session
        .requests
        .get(&id)
        .is_some_and(|r| r.owner == window.label())
    {
        if let Some(request) = session.requests.remove(&id) {
            request.cancel();
        }
    }
}

#[tauri::command]
fn start_request(
    app: AppHandle,
    window: WebviewWindow,
    id: String,
    route: String,
    body: Option<Value>,
    popup_revision: Option<u64>,
    stream: Channel<Message>,
) -> Result<(), String> {
    let backend = app.state::<Backend>();
    let mut session = backend.session.lock().unwrap();
    if !session.logged_in {
        return Err("ログインしてください".into());
    }
    let (method, path) = match route.as_str() {
        "api/config" => (Method::GET, "/api/config"),
        "api/translate" => (Method::POST, "/api/translate"),
        _ => return Err("不明な API です".into()),
    };
    if window.label() == "popup"
        && !app
            .state::<resident::Resident>()
            .accepts_popup(popup_revision)
    {
        return Err("翻訳ポップアップは閉じています".into());
    }
    session
        .requests
        .retain(|_, request| !request.task.inner().is_finished());
    if session.requests.contains_key(&id) {
        return Err("リクエスト ID が重複しています".into());
    }
    if route == "api/translate" {
        let ids: Vec<_> = session
            .requests
            .iter()
            .filter(|(_, r)| r.translation)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(request) = session.requests.remove(&id) {
                request.cancel();
            }
        }
    }
    let auth = backend.auth.clone();
    let app = app.clone();
    let cancel_stream = stream.clone();
    let task = tauri::async_runtime::spawn(async move {
        let result: Result<(), ()> = async {
            let mut response = match auth.send(method, path, body.as_ref()).await {
                Ok(response) => response,
                Err(_) if !auth.is_authenticated() => {
                    let _ = stream.send(Message::LoginRequired { status: 401 });
                    publish_auth(&app, Some("ログインし直してください".into()));
                    return Ok(());
                }
                Err(_) => return Err(()),
            };
            if response.status() == 401 || response.status() == 403 {
                let status = response.status().as_u16();
                // nginx の 403 HTML は読まず、状態の番号だけを渡す。
                let _ = stream.send(Message::LoginRequired { status });
                let _ = auth.invalidate().await;
                publish_auth(
                    &app,
                    Some(format!(
                        "サーバーが {status} を返しました。ログインし直してください"
                    )),
                );
                return Ok(());
            }
            stream
                .send(Message::Start {
                    status: response.status().as_u16(),
                })
                .map_err(|_| ())?;
            while let Some(bytes) = response.chunk().await.map_err(|_| ())? {
                stream
                    .send(Message::Bytes {
                        bytes: bytes.to_vec(),
                    })
                    .map_err(|_| ())?;
            }
            Ok(())
        }
        .await;
        let _ = stream.send(if result.is_ok() {
            Message::End
        } else {
            Message::Error
        });
    });
    session.requests.insert(
        id,
        Request {
            owner: window.label().into(),
            translation: route == "api/translate",
            task,
            stream: cancel_stream,
        },
    );
    Ok(())
}

fn main() {
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(Duration::from_secs(120))
        .build()
        .expect("HTTP クライアントの作成に失敗しました");
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _, event| {
                    if event.state() == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        resident::trigger(app, true);
                    }
                })
                .build(),
        )
        .manage(resident::Resident::default())
        .manage(updates::Updates::default())
        .setup(move |app| {
            let connection = connection::load(app.handle())?;
            app.manage(Backend {
                auth: Arc::new(auth::Auth::with_connection(client.clone(), connection)),
                session: Mutex::new(Session::default()),
                auth_error: Mutex::new(None),
            });
            resident::setup(app.handle())?;
            updates::schedule(app.handle());
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    let auth = handle.state::<Backend>().auth.clone();
                    auth.wait_for_refresh().await;
                    if let Err(error) = auth.renew_if_needed().await {
                        publish_auth(&handle, Some(error));
                    }
                }
            });
            if std::env::args().any(|arg| arg == "--verify-login") {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let result = login(handle.clone()).await;
                    match result {
                        Ok(report) => println!(
                            "本番ログイン確認: verified={} status={}",
                            report.verified, report.api_status
                        ),
                        Err(error) => println!("本番ログイン確認: {error}"),
                    }
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let action = if window.label() == "popup" {
                    "close-popup"
                } else {
                    "hide-main"
                };
                let _ = resident::menu_action(window.app_handle(), action);
            }
            // 閉じてすぐ開き直した場合、古いフォーカス喪失イベントが後から届くことがある。
            tauri::WindowEvent::Focused(false)
                if window.label() == "popup" && !window.is_focused().unwrap_or(false) =>
            {
                resident::hide_popup(window.app_handle())
            }
            _ => {}
        });
    #[cfg(not(feature = "smoke-test"))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        login,
        logout,
        restore_session,
        cancel_login,
        start_request,
        stop_request,
        connection::load_connection,
        connection::save_connection,
        settings::load_settings,
        settings::save_settings,
        resident::session_status,
        resident::popup_content,
        resident::resize_popup,
        resident::take_main_text,
        resident::desktop_action,
        resident::open_accessibility_settings,
        updates::update_status,
        updates::check_updates,
        updates::install_update,
        updates::restart_after_update
    ]);
    #[cfg(feature = "smoke-test")]
    let builder = builder
        .manage(smoke::Smoke::default())
        .invoke_handler(tauri::generate_handler![
            login,
            logout,
            restore_session,
            cancel_login,
            start_request,
            stop_request,
            connection::load_connection,
            connection::save_connection,
            settings::load_settings,
            settings::save_settings,
            resident::session_status,
            resident::popup_content,
            resident::resize_popup,
            resident::take_main_text,
            resident::desktop_action,
            resident::open_accessibility_settings,
            updates::update_status,
            updates::check_updates,
            updates::install_update,
            updates::restart_after_update,
            smoke::smoke_snapshot,
            smoke::smoke_popup_report,
            smoke::smoke_action,
            smoke_result
        ])
        .on_page_load(|window, payload| {
            if window.label() == "main"
                && payload.event() == tauri::webview::PageLoadEvent::Finished
            {
                window
                    .eval(include_str!("../../tests/smoke.js"))
                    .expect("試験を開始できませんでした");
            }
            if window.label() == "popup"
                && payload.event() == tauri::webview::PageLoadEvent::Finished
            {
                window
                    .eval(include_str!("../../tests/popup-smoke.js"))
                    .expect("翻訳ポップアップの試験を開始できませんでした");
            }
        });
    let app = builder
        .build(tauri::generate_context!())
        .expect("翻訳アプリを起動できませんでした");
    app.run(|_app, _event| {
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = _event {
            resident::show_main(_app);
        }
    });
}
