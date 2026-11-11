#![cfg_attr(
    all(target_os = "windows", not(feature = "smoke-test")),
    windows_subsystem = "windows"
)]

mod auth;
mod chrome_bridge;
mod connection;
mod local;
#[cfg(target_os = "macos")]
mod macos;
mod resident;
mod settings;
#[cfg(feature = "smoke-test")]
mod smoke;
mod translation;
mod updates;
#[cfg(target_os = "windows")]
mod windows;
mod worker_agent;
mod worker_power;
mod worker_registration;
mod worker_relay;
mod worker_sandbox;
mod worker_supervisor;

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
    local: bool,
    task: JoinHandle<()>,
    stream: Channel<Message>,
}

impl Request {
    fn cancel(self) {
        self.task.abort();
        let _ = self.stream.send(Message::Canceled);
    }
}

impl Session {
    fn has_pending_translation(&self) -> bool {
        self.requests
            .values()
            .any(|request| request.translation && !request.task.inner().is_finished())
    }

    fn cancel_matching(&mut self, matches: impl Fn(&Request) -> bool) {
        let ids: Vec<_> = self
            .requests
            .iter()
            .filter(|(_, request)| matches(request))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(request) = self.requests.remove(&id) {
                request.cancel();
            }
        }
    }
}

fn cancel_matching(app: &AppHandle, matches: impl Fn(&Request) -> bool) {
    app.state::<Backend>()
        .session
        .lock()
        .unwrap()
        .cancel_matching(matches);
}

fn cancel_window(app: &AppHandle, owner: &str) {
    cancel_matching(app, |r| r.owner == owner);
}
fn cancel_all(app: &AppHandle) {
    cancel_matching(app, |_| true);
}

struct Backend {
    auth: tokio::sync::watch::Sender<Arc<auth::Auth>>,
    client: Client,
    session: Mutex<Session>,
    auth_error: Mutex<Option<String>>,
}

impl Backend {
    fn auth(&self) -> Arc<auth::Auth> {
        self.auth.borrow().clone()
    }
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
    let logged_in = backend.auth().is_authenticated();
    if logged_in || error.is_some() {
        *backend.auth_error.lock().unwrap() = error;
    }
    change_login(app, app.state::<local::Local>().enabled() || logged_in);
}

fn change_login(app: &AppHandle, logged_in: bool) {
    {
        let backend = app.state::<Backend>();
        let mut session = backend.session.lock().unwrap();
        session.logged_in = logged_in;
        if !logged_in {
            session.cancel_matching(|request| !request.local);
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
    if app.state::<local::Local>().enabled() {
        change_login(&app, true);
        return Ok(());
    }
    restore_server_session(app).await
}

#[tauri::command]
async fn restore_server_session(app: AppHandle) -> Result<(), String> {
    if !app.state::<Backend>().auth().is_mock() && connection::load(&app)?.server_url.is_empty() {
        publish_auth(&app, None);
        return Ok(());
    }
    let result = app.state::<Backend>().auth().restore().await;
    publish_auth(&app, result.as_ref().err().cloned());
    result
}

#[tauri::command]
async fn login(app: AppHandle) -> Result<auth::LoginReport, String> {
    let auth = app.state::<Backend>().auth();
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
async fn login_anonymous(app: AppHandle, window: WebviewWindow) -> Result<(), String> {
    if window.label() != "main" {
        return Err("匿名利用はメインウィンドウの設定で開始してください".into());
    }
    let auth = app.state::<Backend>().auth();
    let result = auth.login_anonymous().await;
    if Arc::ptr_eq(&auth, &app.state::<Backend>().auth()) {
        publish_auth(&app, result.as_ref().err().cloned());
    }
    result
}

#[tauri::command]
fn cancel_login(app: AppHandle) {
    app.state::<Backend>().auth().cancel_login();
}

#[tauri::command]
async fn logout(app: AppHandle) -> Result<(), String> {
    let auth = app.state::<Backend>().auth();
    // ネットワークの revoke を待つ間にも画面と翻訳を止める。
    auth.cancel_login();
    if app.state::<local::Local>().enabled() {
        cancel_matching(&app, |request| !request.local);
    } else {
        change_login(&app, false);
    }
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

async fn read_translation_config(mut response: reqwest::Response) -> Result<Value, ()> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        if body.len() + chunk.len() > 64 * 1024 {
            return Err(());
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice::<Value>(&body)
        .ok()
        .filter(Value::is_object)
        .ok_or(())
}

fn translation_body(server: &str, config: &Value, mut body: Value) -> Option<Value> {
    let fields = body.as_object_mut()?;
    if fields.remove("desktop_server")?.as_str()? != server {
        return None;
    }
    if config["pool"]["kind"] == "volunteer" {
        let version = config["pool"]["consent_version"]
            .as_u64()
            .filter(|v| *v > 0)?;
        if fields.get("consent").and_then(Value::as_u64) != Some(version) {
            return None;
        }
    } else {
        fields.remove("consent");
    }
    Some(body)
}

fn json_response(stream: &Channel<Message>, status: u16, body: &Value) -> Result<(), ()> {
    stream.send(Message::Start { status }).map_err(|_| ())?;
    stream
        .send(Message::Bytes {
            bytes: serde_json::to_vec(body).map_err(|_| ())?,
        })
        .map_err(|_| ())
}

async fn login_required(
    app: &AppHandle,
    auth: &Arc<auth::Auth>,
    stream: &Channel<Message>,
    status: u16,
) -> Result<(), ()> {
    let _ = stream.send(Message::LoginRequired { status });
    let _ = auth.invalidate().await;
    if Arc::ptr_eq(auth, &app.state::<Backend>().auth()) {
        publish_auth(
            app,
            Some(format!(
                "サーバーが {status} を返しました。ログインし直してください"
            )),
        );
    }
    Ok(())
}

#[tauri::command]
fn start_request(
    app: AppHandle,
    window: WebviewWindow,
    id: String,
    route: String,
    body: Option<Value>,
    popup_revision: Option<u64>,
    local_override: Option<bool>,
    stream: Channel<Message>,
) -> Result<(), String> {
    let backend = app.state::<Backend>();
    let mut session = backend.session.lock().unwrap();
    let engine = app.state::<local::Local>();
    if local_override.is_some() && window.label() != "main" {
        return Err("翻訳先の切り替えはメイン画面で行ってください".into());
    }
    let use_local = local_override.unwrap_or_else(|| engine.enabled());
    if use_local && !engine.enabled() && route == "api/translate" {
        local::check_temporary_local(&app)?;
    }
    let auth = backend.auth();
    if !use_local
        && (auth.is_busy() || !auth.is_authenticated() || (!engine.enabled() && !session.logged_in))
    {
        return Err("サーバーで翻訳するにはログインしてください".into());
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
        session.cancel_matching(|request| request.translation);
    }
    let app = app.clone();
    let cancel_stream = stream.clone();
    let translation = route == "api/translate";
    let task = tauri::async_runtime::spawn(async move {
        let result: Result<(), ()> = async {
            if use_local {
                return local::request(&app, &route, body.as_ref(), &stream).await;
            }
            // config と翻訳を同じ認証・接続先に固定する。
            let send =
                async {
                    let config = auth.send(Method::GET, "/api/config", None).await?;
                    if !config.status().is_success() {
                        return Ok::<_, String>(Some(config));
                    }
                    let mut config = read_translation_config(config)
                        .await
                        .map_err(|_| "翻訳の設定を読み込めませんでした".to_string())?;
                    if !translation {
                        config["desktop_server"] = Value::String(auth.server().into());
                        json_response(&stream, 200, &config).map_err(|_| String::new())?;
                        return Ok(None);
                    }
                    let Some(body) =
                        body.and_then(|body| translation_body(auth.server(), &config, body))
                    else {
                        json_response(&stream, 409, &serde_json::json!({
                        "error": "翻訳先または同意の版が変わりました。もう一度翻訳してください"
                    })).map_err(|_| String::new())?;
                        return Ok(None);
                    };
                    auth.send(method, path, Some(&body)).await.map(Some)
                };
            let mut response = match send.await {
                Ok(Some(response)) => response,
                Ok(None) => return Ok(()),
                Err(error) if error == auth::ANONYMOUS_RENEWED => {
                    return json_response(&stream, 409, &serde_json::json!({ "error": error }));
                }
                Err(_) if !auth.is_authenticated() => {
                    let _ = stream.send(Message::LoginRequired { status: 401 });
                    if Arc::ptr_eq(&auth, &app.state::<Backend>().auth()) {
                        publish_auth(&app, Some("ログインし直してください".into()));
                    }
                    return Ok(());
                }
                Err(_) => return Err(()),
            };
            if translation && response.status() == 403 {
                // 同意の版が確認直後に変わった場合は、ログイン状態を維持する。
                let is_json = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.starts_with("application/json"));
                if is_json {
                    let error = read_translation_config(response).await?;
                    if error["code"] == "consent_required" {
                        return json_response(
                            &stream,
                            409,
                            &serde_json::json!({
                                "error": "同意の版が変わりました。もう一度翻訳してください"
                            }),
                        );
                    }
                    if auth.is_anonymous() {
                        return json_response(&stream, 403, &error);
                    }
                    return login_required(&app, &auth, &stream, 403).await;
                }
            }
            if response.status() == 401 || response.status() == 403 {
                if auth.is_anonymous() {
                    return json_response(
                        &stream,
                        502,
                        &serde_json::json!({
                            "error": "匿名では翻訳サーバーに接続できませんでした"
                        }),
                    );
                }
                return login_required(&app, &auth, &stream, response.status().as_u16()).await;
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
            translation,
            local: use_local,
            task,
            stream: cancel_stream,
        },
    );
    Ok(())
}

fn main() {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--worker-agent")) {
        worker_agent::entry();
        return;
    }
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    if chrome_bridge::host_entry() {
        return;
    }
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(Duration::from_secs(120))
        .build()
        .expect("HTTP クライアントの作成に失敗しました");
    let builder = tauri::Builder::default();
    #[cfg(target_os = "windows")]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _| {
        if args.len() == 1 {
            resident::show_main(app);
        }
    }));
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let builder = builder.plugin(tauri_plugin_deep_link::init());
    let builder = builder
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
        .manage(chrome_bridge::Status::default())
        .manage(resident::Resident::default())
        .manage(updates::Updates::default())
        .manage(worker_registration::RegistrationGate::default())
        .manage(worker_supervisor::Lifecycle::default())
        .setup(move |app| {
            app.manage(local::Local::load(app.handle())?);
            let connection = connection::load(app.handle())?;
            app.manage(Backend {
                auth: tokio::sync::watch::channel(Arc::new(auth::Auth::with_connection(
                    client.clone(),
                    connection,
                )))
                .0,
                client: client.clone(),
                session: Mutex::new(Session::default()),
                auth_error: Mutex::new(None),
            });
            resident::setup(app.handle())?;
            local::Local::schedule(app.handle());
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            chrome_bridge::schedule(app.handle());
            updates::schedule(app.handle());
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            chrome_bridge::setup_links(app.handle());
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut connections = handle.state::<Backend>().auth.subscribe();
                loop {
                    let auth = connections.borrow_and_update().clone();
                    tokio::select! {
                        _ = connections.changed() => {},
                        result = async {
                            auth.wait_for_refresh().await;
                            auth.renew_if_needed().await
                        } => {
                            if let Err(error) = result {
                                if Arc::ptr_eq(&auth, &handle.state::<Backend>().auth()) {
                                    publish_auth(&handle, Some(error));
                                }
                            }
                        }
                    }
                }
            });
            // IPCを受け付ける画面は、状態と常駐機能の初期化が終わってから作る。
            for config in &app.config().app.windows {
                let builder = tauri::WebviewWindowBuilder::from_config(app, config)?;
                #[cfg(feature = "smoke-test")]
                let builder = if std::env::var_os("PTRANSLATE_SMOKE_CONFIG").is_some() {
                    builder.incognito(true)
                } else {
                    builder
                };
                builder.build()?;
            }
            if app.config().identifier.ends_with(".local-prototype") {
                if let Some(window) = app.get_webview_window("main") {
                    window.set_title("PTranslate Desk Local Prototype")?;
                }
            }
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
        login_anonymous,
        logout,
        restore_session,
        restore_server_session,
        chrome_bridge::register_chrome,
        chrome_bridge::chrome_registration_request,
        chrome_bridge::finish_chrome_registration,
        local::local_status,
        local::select_server_mode,
        local::open_model_information,
        local::open_plamo_license,
        local::measure_local_devices,
        local::cancel_local_measurement,
        local::resources::local_download_info,
        local::resources::download_local_resources,
        local::resources::cancel_local_download,
        local::local_devices,
        local::save_local_settings,
        local::set_local_active,
        cancel_login,
        start_request,
        stop_request,
        worker_supervisor::worker_activity,
        worker_registration::worker_registration_status,
        worker_registration::set_worker_participation,
        worker_registration::register_worker,
        worker_registration::renew_worker,
        worker_registration::revoke_worker,
        worker_registration::forget_worker_registration,
        connection::load_connection,
        connection::save_connection,
        settings::load_settings,
        settings::save_settings,
        settings::setup_state,
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
            login_anonymous,
            logout,
            restore_session,
            restore_server_session,
            chrome_bridge::register_chrome,
            chrome_bridge::chrome_registration_request,
            chrome_bridge::finish_chrome_registration,
            local::local_status,
            local::select_server_mode,
            local::open_model_information,
            local::open_plamo_license,
            local::measure_local_devices,
            local::cancel_local_measurement,
            local::resources::local_download_info,
            local::resources::download_local_resources,
            local::resources::cancel_local_download,
            local::local_devices,
            local::save_local_settings,
            local::set_local_active,
            cancel_login,
            start_request,
            stop_request,
            worker_supervisor::worker_activity,
            worker_registration::worker_registration_status,
            worker_registration::set_worker_participation,
            worker_registration::register_worker,
            worker_registration::renew_worker,
            worker_registration::revoke_worker,
            worker_registration::forget_worker_registration,
            connection::load_connection,
            connection::save_connection,
            settings::load_settings,
            settings::save_settings,
            settings::setup_state,
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
            if std::env::var("PTRANSLATE_EXTENSION_PROBE").as_deref() == Ok("1") {
                return;
            }
            if window.label() == "main"
                && payload.event() == tauri::webview::PageLoadEvent::Finished
            {
                window
                    .eval(
                        if std::env::var("PTRANSLATE_CHROME_REGISTRATION_PROBE").as_deref()
                            == Ok("1")
                        {
                            include_str!("../../tests/chrome-registration-probe.js")
                        } else if std::env::var("PTRANSLATE_BRIDGE_FAILURE_PROBE").as_deref()
                            == Ok("1")
                        {
                            include_str!("../../tests/bridge-failure-probe.js")
                        } else if std::env::var("PTRANSLATE_LOCAL_MEASURE_PROBE").as_deref()
                            == Ok("1")
                        {
                            include_str!("../../tests/local-setup-probe.js")
                        } else if std::env::var("PTRANSLATE_LOCAL_PROBE").as_deref() == Ok("1") {
                            include_str!("../../tests/local-probe.js")
                        } else {
                            include_str!("../../tests/smoke.js")
                        },
                    )
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
        if let tauri::RunEvent::ExitRequested { api, code, .. } = &_event {
            let (first, finished) = _app
                .state::<worker_supervisor::Lifecycle>()
                .begin_shutdown();
            if finished.is_some() && *code != Some(tauri::RESTART_EXIT_CODE) {
                api.prevent_exit();
                if first {
                    let app = _app.clone();
                    let code = code.unwrap_or(0);
                    tauri::async_runtime::spawn(async move {
                        worker_supervisor::Lifecycle::wait_for_exit(finished).await;
                        app.exit(code);
                    });
                }
            }
        }
        if let tauri::RunEvent::Exit = _event {
            _app.state::<local::Local>().stop();
        }
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = _event {
            resident::show_main(_app);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn only_pending_translation_requests_pause_worker_admission() {
        let mut session = Session::default();
        let request = |translation, task| Request {
            owner: "main".into(),
            translation,
            local: false,
            task,
            stream: Channel::new(|_| Ok(())),
        };
        session.requests.insert(
            "config".into(),
            request(false, tauri::async_runtime::spawn(std::future::pending())),
        );
        assert!(!session.has_pending_translation());
        let (finish, done) = tokio::sync::oneshot::channel();
        session.requests.insert(
            "translation".into(),
            request(
                true,
                tauri::async_runtime::spawn(async move {
                    let _ = done.await;
                }),
            ),
        );
        assert!(session.has_pending_translation());
        finish.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while session.has_pending_translation() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        session.cancel_matching(|_| true);
    }

    #[test]
    fn volunteer_translation_requires_the_current_origin_and_version() {
        let server = "https://translate.example.com";
        let config = json!({"pool": {"kind": "volunteer", "consent_version": 2}});
        for body in [
            json!({"desktop_server": "https://other.example.com", "consent": 2}),
            json!({"desktop_server": server, "consent": 1}),
            json!({"desktop_server": server}),
        ] {
            assert!(translation_body(server, &config, body).is_none());
        }
        let body = json!({"desktop_server": server, "consent": 2, "text": "Hello"});
        assert_eq!(
            translation_body(server, &config, body.clone()),
            Some(json!({"consent": 2, "text": "Hello"}))
        );
        assert_eq!(
            translation_body(server, &json!({}), body),
            Some(json!({"text": "Hello"}))
        );
    }
}
