#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod settings;

#[cfg(feature = "smoke-test")]
#[tauri::command]
fn smoke_result(app: tauri::AppHandle, passed: bool, step: String) {
    println!(
        "Mac WebView smoke test: {}",
        if passed { "PASS" } else { "FAIL" }
    );
    println!("確認項目: {step}");
    app.exit(if passed { 0 } else { 1 });
}

use reqwest::{Client, Method};
use serde::Serialize;
use serde_json::Value;
use std::{collections::HashMap, sync::Mutex, time::Duration};
use tauri::{async_runtime::JoinHandle, ipc::Channel, State};

const MOCK_SERVER: &str = "http://127.0.0.1:8002";

#[derive(Default)]
struct Session {
    logged_in: bool,
    requests: HashMap<u32, JoinHandle<()>>,
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
}

#[tauri::command]
fn set_mock_login(backend: State<'_, Backend>, logged_in: bool) {
    let mut session = backend.session.lock().unwrap();
    session.logged_in = logged_in;
    if !logged_in {
        for (_, request) in session.requests.drain() {
            request.abort();
        }
    }
}

#[tauri::command]
fn stop_request(backend: State<'_, Backend>, id: u32) {
    if let Some(request) = backend.session.lock().unwrap().requests.remove(&id) {
        request.abort();
    }
}

#[tauri::command]
fn start_request(
    backend: State<'_, Backend>,
    id: u32,
    route: String,
    body: Option<Value>,
    stream: Channel<Message>,
) -> Result<(), String> {
    let mut session = backend.session.lock().unwrap();
    if !session.logged_in {
        return Err("モックにログインしてください".into());
    }
    let (method, path) = match route.as_str() {
        "api/config" => (Method::GET, "/api/config"),
        "api/translate" => (Method::POST, "/api/translate"),
        _ => return Err("不明な API です".into()),
    };
    let mut request = backend
        .client
        .request(method, format!("{MOCK_SERVER}{path}"))
        .header("X-Forwarded-User", "mock-desktop");
    if let Some(body) = body {
        request = request.json(&body);
    }
    if session.requests.contains_key(&id) {
        return Err("リクエスト ID が重複しています".into());
    }
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
    session.requests.insert(id, task);
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
        .manage(Backend {
            client,
            session: Mutex::new(Session::default()),
        });
    #[cfg(not(feature = "smoke-test"))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        set_mock_login,
        start_request,
        stop_request,
        settings::load_settings,
        settings::save_settings
    ]);
    #[cfg(feature = "smoke-test")]
    let builder = builder
        .invoke_handler(tauri::generate_handler![
            set_mock_login,
            start_request,
            stop_request,
            settings::load_settings,
            settings::save_settings,
            smoke_result
        ])
        .on_page_load(|window, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Finished {
                window
                    .eval(include_str!("../../tests/smoke.js"))
                    .expect("試験を開始できませんでした");
            }
        });
    builder
        .run(tauri::generate_context!())
        .expect("翻訳アプリを起動できませんでした");
}
