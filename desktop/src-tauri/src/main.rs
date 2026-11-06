#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "macos")]
mod macos;
mod resident;
mod settings;
#[cfg(feature = "smoke-test")]
mod smoke;

#[cfg(feature = "smoke-test")]
#[tauri::command]
fn smoke_result(app: tauri::AppHandle, passed: bool, step: String) {
    println!(
        "Mac WebView smoke test: {}",
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
use std::{collections::HashMap, sync::Mutex, time::Duration};
use tauri::{async_runtime::JoinHandle, ipc::Channel, AppHandle, Emitter, Manager, WebviewWindow};

const MOCK_SERVER: &str = "http://127.0.0.1:8002";

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
    client: Client,
    session: Mutex<Session>,
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Message {
    Start { status: u16 },
    Bytes { bytes: Vec<u8> },
    End,
    Error,
    Canceled,
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
fn set_mock_login(app: AppHandle, logged_in: bool) {
    change_login(&app, logged_in);
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
        return Err("モックにログインしてください".into());
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
        return Err("小窓は閉じています".into());
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
    let mut request = backend
        .client
        .request(method, format!("{MOCK_SERVER}{path}"))
        .header("X-Forwarded-User", "mock-desktop");
    if let Some(body) = body {
        request = request.json(&body);
    }
    let cancel_stream = stream.clone();
    let task = tauri::async_runtime::spawn(async move {
        let result: Result<(), ()> = async {
            let mut response = request.send().await.map_err(|_| ())?;
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
        .manage(Backend {
            client,
            session: Mutex::new(Session::default()),
        })
        .setup(|app| resident::setup(app.handle()))
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
            tauri::WindowEvent::Focused(false) if window.label() == "popup" => {
                resident::hide_popup(window.app_handle())
            }
            _ => {}
        });
    #[cfg(not(feature = "smoke-test"))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        set_mock_login,
        start_request,
        stop_request,
        settings::load_settings,
        settings::save_settings,
        resident::session_status,
        resident::popup_content,
        resident::take_main_text,
        resident::desktop_action,
        resident::open_accessibility_settings
    ]);
    #[cfg(feature = "smoke-test")]
    let builder = builder
        .manage(smoke::Smoke::default())
        .invoke_handler(tauri::generate_handler![
            set_mock_login,
            start_request,
            stop_request,
            settings::load_settings,
            settings::save_settings,
            resident::session_status,
            resident::popup_content,
            resident::take_main_text,
            resident::desktop_action,
            resident::open_accessibility_settings,
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
                    .expect("小窓の試験を開始できませんでした");
            }
        });
    let app = builder
        .build(tauri::generate_context!())
        .expect("翻訳アプリを起動できませんでした");
    app.run(|app, event| {
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = event {
            resident::show_main(app);
        }
    });
}
