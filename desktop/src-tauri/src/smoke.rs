//! GUI セッションでの試験専用。配布ビルドには含めない。
use serde::Serialize;
use std::sync::Mutex;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_clipboard_manager::ClipboardExt;

#[derive(Default, Clone, Serialize)]
pub struct PopupReport {
    output: String,
    status: String,
}

#[derive(Default)]
pub struct Smoke {
    popup: Mutex<PopupReport>,
}

#[derive(Serialize)]
pub struct Snapshot {
    main_visible: bool,
    popup_visible: bool,
    tray: bool,
    requests: usize,
    popup: PopupReport,
}

pub fn diagnostic(app: &AppHandle) {
    let snapshot = smoke_snapshot(app.clone(), app.state::<Smoke>());
    println!(
        "main={} popup={} tray={} requests={} status={}",
        snapshot.main_visible,
        snapshot.popup_visible,
        snapshot.tray,
        snapshot.requests,
        snapshot.popup.status
    );
}

#[tauri::command]
pub fn smoke_snapshot(app: AppHandle, state: State<'_, Smoke>) -> Snapshot {
    Snapshot {
        main_visible: app
            .get_webview_window("main")
            .unwrap()
            .is_visible()
            .unwrap(),
        popup_visible: app
            .get_webview_window("popup")
            .unwrap()
            .is_visible()
            .unwrap(),
        tray: app.tray_by_id("translator").is_some(),
        requests: app
            .state::<crate::Backend>()
            .session
            .lock()
            .unwrap()
            .requests
            .values()
            .filter(|r| r.translation && !r.task.inner().is_finished())
            .count(),
        popup: state.popup.lock().unwrap().clone(),
    }
}

#[tauri::command]
pub fn smoke_popup_report(state: State<'_, Smoke>, output: String, status: String) {
    *state.popup.lock().unwrap() = PopupReport { output, status };
}

#[tauri::command]
pub fn smoke_action(app: AppHandle, action: String, text: Option<String>) -> Result<(), String> {
    match action.as_str() {
        "shortcut" => {
            app.clipboard()
                .write_text(text.unwrap_or_default())
                .map_err(|e| e.to_string())?;
            crate::resident::copy_key(&app, true, false);
            crate::resident::copy_key(&app, true, false);
        }
        "close-main" => {
            app.get_webview_window("main")
                .unwrap()
                .close()
                .map_err(|e| e.to_string())?;
        }
        "escape" => {
            app.get_webview_window("popup")
                .unwrap()
                .eval("document.dispatchEvent(new KeyboardEvent('keydown', {key: 'Escape'}))")
                .map_err(|e| e.to_string())?;
        }
        "open-main" => {
            app.get_webview_window("popup")
                .unwrap()
                .eval("document.getElementById('open-main').click()")
                .map_err(|e| e.to_string())?;
        }
        "copy" => {
            app.get_webview_window("popup")
                .unwrap()
                .eval("document.getElementById('copy').click()")
                .map_err(|e| e.to_string())?;
        }
        _ => return Err("不明な試験操作です".into()),
    }
    Ok(())
}
