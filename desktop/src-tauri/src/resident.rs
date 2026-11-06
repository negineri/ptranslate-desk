use crate::{settings, Backend};
use serde::Serialize;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, PhysicalPosition, State,
};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_global_shortcut::GlobalShortcutExt;

pub const COMBINATION: &str = "CommandOrControl+Shift+T";

#[derive(Default)]
pub struct DoubleCopy {
    last: Option<Instant>,
}

impl DoubleCopy {
    pub fn press(&mut self, now: Instant) -> bool {
        let double = self
            .last
            .is_some_and(|last| now.duration_since(last) <= Duration::from_millis(500));
        self.last = if double { None } else { Some(now) };
        double
    }
    pub fn reset(&mut self) {
        self.last = None;
    }
}

#[derive(Clone, Serialize)]
pub struct PopupContent {
    pub revision: u64,
    pub text: String,
}

struct Ui {
    shortcut: String,
    paused: bool,
    revision: u64,
    popup: Option<PopupContent>,
    main_text: Option<String>,
}

pub struct Resident {
    ui: Mutex<Ui>,
    capturing: AtomicBool,
    copies: Mutex<DoubleCopy>,
}

impl Default for Resident {
    fn default() -> Self {
        Self {
            ui: Mutex::new(Ui {
                shortcut: "double-copy".into(),
                paused: false,
                revision: 0,
                popup: None,
                main_text: None,
            }),
            capturing: AtomicBool::new(false),
            copies: Mutex::new(DoubleCopy::default()),
        }
    }
}

impl Resident {
    pub fn accepts_popup(&self, revision: Option<u64>) -> bool {
        self.ui
            .lock()
            .unwrap()
            .popup
            .as_ref()
            .is_some_and(|p| Some(p.revision) == revision)
    }
}

#[derive(Serialize)]
pub struct Status {
    logged_in: bool,
    paused: bool,
    shortcut: String,
    accessibility: bool,
}

#[tauri::command]
pub fn session_status(app: AppHandle) -> Status {
    let resident = app.state::<Resident>();
    let ui = resident.ui.lock().unwrap();
    let (paused, shortcut) = (ui.paused, ui.shortcut.clone());
    drop(ui);
    let logged_in = app.state::<Backend>().session.lock().unwrap().logged_in;
    Status {
        logged_in,
        paused,
        shortcut,
        accessibility: accessibility(),
    }
}

fn accessibility() -> bool {
    #[cfg(target_os = "macos")]
    {
        crate::macos::trusted()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

pub fn shortcut_enabled(app: &AppHandle, shortcut: &str) -> bool {
    let resident = app.state::<Resident>();
    let ui = resident.ui.lock().unwrap();
    let enabled = !ui.paused && ui.shortcut == shortcut;
    drop(ui);
    enabled && app.state::<Backend>().session.lock().unwrap().logged_in
}

pub fn copy_key(app: &AppHandle, copy: bool, repeated: bool) {
    if repeated {
        return;
    }
    let enabled = shortcut_enabled(app, "double-copy");
    let resident = app.state::<Resident>();
    let mut presses = resident.copies.lock().unwrap();
    let trigger_now = if copy && enabled {
        presses.press(Instant::now())
    } else {
        presses.reset();
        false
    };
    drop(presses);
    if trigger_now {
        trigger(app, false);
    }
}

pub fn configure_shortcut(app: &AppHandle, shortcut: &str) -> Result<(), String> {
    if !["double-copy", COMBINATION].contains(&shortcut) {
        return Err("ショートカットの設定が不正です".into());
    }
    let resident = app.state::<Resident>();
    let previous = resident.ui.lock().unwrap().shortcut.clone();
    if previous == shortcut {
        return Ok(());
    }
    if shortcut == COMBINATION {
        app.global_shortcut().register(COMBINATION).map_err(|_| {
            "ショートカットを登録できません。ほかのアプリが使っていないか確認してください"
                .to_string()
        })?;
    } else {
        app.global_shortcut()
            .unregister(COMBINATION)
            .map_err(|_| "ショートカットを解除できませんでした".to_string())?;
    }
    resident.ui.lock().unwrap().shortcut = shortcut.into();
    resident.copies.lock().unwrap().reset();
    Ok(())
}

pub fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub fn hide_popup(app: &AppHandle) {
    let resident = app.state::<Resident>();
    let mut ui = resident.ui.lock().unwrap();
    ui.revision += 1;
    ui.popup = None;
    drop(ui);
    crate::cancel_window(app, "popup");
    let _ = app.emit_to("popup", "popup-hidden", ());
    if let Some(window) = app.get_webview_window("popup") {
        let _ = window.hide();
    }
}

pub fn clear_session(app: &AppHandle) {
    hide_popup(app);
    app.state::<Resident>().ui.lock().unwrap().main_text = None;
    app.state::<Resident>().copies.lock().unwrap().reset();
}

pub fn open_popup(app: &AppHandle, text: String) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("選んだ文がありません".into());
    }
    if !app.state::<Backend>().session.lock().unwrap().logged_in {
        show_main(app);
        return Err("モックにログインしてください".into());
    }
    // 既に表示中なら隠さずに内容だけを替える。古いフォーカス喪失で新しい小窓を閉じないため。
    crate::cancel_window(app, "popup");
    let _ = app.emit_to("popup", "popup-hidden", ());
    crate::cancel_window(app, "main");
    let _ = app.emit_to("main", "translation-interrupted", ());
    let resident = app.state::<Resident>();
    let mut ui = resident.ui.lock().unwrap();
    ui.revision += 1;
    ui.popup = Some(PopupContent {
        revision: ui.revision,
        text,
    });
    drop(ui);
    let window = app
        .get_webview_window("popup")
        .ok_or("小窓を開けませんでした")?;
    if let Ok(cursor) = app.cursor_position() {
        let mut point = PhysicalPosition::new(cursor.x as i32 + 12, cursor.y as i32 + 12);
        if let Ok(Some(monitor)) = window.monitor_from_point(cursor.x, cursor.y) {
            let area = monitor.work_area();
            let size = window
                .outer_size()
                .map_err(|_| "小窓の大きさを取得できませんでした")?;
            point.x = point.x.clamp(
                area.position.x,
                (area.position.x + area.size.width as i32 - size.width as i32).max(area.position.x),
            );
            point.y = point.y.clamp(
                area.position.y,
                (area.position.y + area.size.height as i32 - size.height as i32)
                    .max(area.position.y),
            );
        }
        window
            .set_position(point)
            .map_err(|_| "小窓の位置を設定できませんでした")?;
    }
    window.show().map_err(|_| "小窓を開けませんでした")?;
    window.set_focus().map_err(|_| "小窓を開けませんでした")?;
    let _ = app.emit_to("popup", "popup-content", ());
    Ok(())
}

#[tauri::command]
pub fn popup_content(resident: State<'_, Resident>) -> Option<PopupContent> {
    resident.ui.lock().unwrap().popup.clone()
}

#[tauri::command]
pub fn take_main_text(resident: State<'_, Resident>) -> Option<String> {
    resident.ui.lock().unwrap().main_text.take()
}

fn report_error(app: &AppHandle, message: &str) {
    show_main(app);
    let _ = app.emit_to("main", "desktop-error", message);
}

pub fn trigger(app: &AppHandle, copy_selection: bool) {
    let mode = if copy_selection {
        COMBINATION
    } else {
        "double-copy"
    };
    if !shortcut_enabled(app, mode) {
        return;
    }
    let resident = app.state::<Resident>();
    if resident.capturing.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = capture_text(&app, copy_selection).await;
        app.state::<Resident>()
            .capturing
            .store(false, Ordering::SeqCst);
        if !shortcut_enabled(&app, mode) {
            return;
        }
        match result {
            Ok(text) => {
                if let Err(error) = open_popup(&app, text) {
                    report_error(&app, &error);
                }
            }
            Err(error) => report_error(&app, &error),
        }
    });
}

async fn capture_text(app: &AppHandle, copy: bool) -> Result<String, String> {
    if !copy {
        // 2 回目のコピーがクリップボードに届くのを待つ。
        tokio::time::sleep(Duration::from_millis(100)).await;
        return app
            .clipboard()
            .read_text()
            .map_err(|_| "クリップボードに文字列がありません".into());
    }
    #[cfg(not(target_os = "macos"))]
    {
        return Err("選んだ文の自動取得は、現在 macOS で利用できます".into());
    }
    #[cfg(target_os = "macos")]
    {
        if !crate::macos::trusted() {
            return Err(
                "選んだ文を取得するには、設定からアクセシビリティを許可してください".into(),
            );
        }
        let previous = app.clipboard().read_text().unwrap_or_default();
        let marker = format!("ptranslate-copy-{:?}", Instant::now());
        app.clipboard()
            .write_text(marker.clone())
            .map_err(|_| "クリップボードを使えませんでした")?;
        let result = async {
            crate::macos::copy_selection()?;
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_millis(25)).await;
                if let Ok(text) = app.clipboard().read_text() {
                    if text != marker {
                        return Ok(text);
                    }
                }
            }
            Err("選んだ文をコピーできませんでした".to_string())
        }
        .await;
        let current = app.clipboard().read_text().ok();
        // コピー待ちの間に別の操作で書き換わった内容は上書きしない。
        if current.as_deref() == Some(marker.as_str()) || current.as_ref() == result.as_ref().ok() {
            app.clipboard()
                .write_text(previous)
                .map_err(|_| "クリップボードを元に戻せませんでした")?;
        }
        result
    }
}

pub fn refresh_menu(app: &AppHandle) -> tauri::Result<()> {
    let status = session_status(app.clone());
    let first = MenuItem::with_id(
        app,
        "main",
        if status.logged_in {
            "主窓を開く"
        } else {
            "ログイン"
        },
        true,
        None::<&str>,
    )?;
    let pause = CheckMenuItem::with_id(
        app,
        "pause",
        "ショートカットを一時的に止める",
        true,
        status.paused,
        None::<&str>,
    )?;
    let settings = MenuItem::with_id(app, "settings", "設定", true, None::<&str>)?;
    let logout = MenuItem::with_id(app, "logout", "ログアウト", status.logged_in, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "終了", true, None::<&str>)?;
    let line = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&first, &pause, &settings, &logout, &line, &quit])?;
    if let Some(tray) = app.tray_by_id("translator") {
        tray.set_menu(Some(menu))?;
        tray.set_icon_with_as_template(Some(tray_image(status.logged_in)), true)?;
        tray.set_tooltip(Some(if status.logged_in {
            "翻訳"
        } else {
            "翻訳 — ログインしてください"
        }))?;
    }
    Ok(())
}

// 小さいメニューバーでもログイン状態を見分けられる、背景が透明の図形。
fn tray_image(logged_in: bool) -> tauri::image::Image<'static> {
    let mut rgba = vec![0; 22 * 22 * 4];
    for y in 3..19 {
        for x in 3..19 {
            let border = x == 3 || x == 18 || y == 3 || y == 18;
            let mark = logged_in
                && ((y == 8 && (7..15).contains(&x)) || (x == 11 && (6..16).contains(&y)));
            if border || mark {
                rgba[(y * 22 + x) * 4 + 3] = 255;
            }
        }
    }
    tauri::image::Image::new_owned(rgba, 22, 22)
}

pub fn setup(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    TrayIconBuilder::with_id("translator")
        .icon(tray_image(false))
        .icon_as_template(true)
        .tooltip("翻訳 — ログインしてください")
        .on_menu_event(|app, event| {
            let _ = menu_action(app, event.id.as_ref());
        })
        .build(app)?;
    refresh_menu(app)?;
    if let Ok(Some(settings)) = settings::load_settings(app.clone()) {
        if let Err(error) = configure_shortcut(app, &settings.shortcut) {
            report_error(app, &error);
        }
    }
    #[cfg(target_os = "macos")]
    crate::macos::install_monitor(app);
    Ok(())
}

pub fn menu_action(app: &AppHandle, action: &str) -> Result<(), String> {
    match action {
        "main" => show_main(app),
        "settings" => {
            show_main(app);
            let _ = app.emit_to("main", "show-settings", ());
        }
        "pause" => {
            let resident = app.state::<Resident>();
            let mut ui = resident.ui.lock().unwrap();
            ui.paused = !ui.paused;
            drop(ui);
            resident.copies.lock().unwrap().reset();
            refresh_menu(app).map_err(|_| "メニューを更新できませんでした")?;
            let _ = app.emit("session-changed", ());
        }
        "logout" => crate::change_login(app, false),
        "quit" => {
            crate::cancel_all(app);
            app.exit(0);
        }
        "close-popup" => hide_popup(app),
        "open-main" => {
            let resident = app.state::<Resident>();
            let text = resident
                .ui
                .lock()
                .unwrap()
                .popup
                .as_ref()
                .map(|p| p.text.clone());
            hide_popup(app);
            resident.ui.lock().unwrap().main_text = text;
            show_main(app);
            let _ = app.emit_to("main", "main-text", ());
        }
        "hide-main" => {
            crate::cancel_window(app, "main");
            let _ = app.emit_to("main", "translation-interrupted", ());
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.hide();
            }
        }
        _ => return Err("不明な操作です".into()),
    }
    Ok(())
}

#[tauri::command]
pub fn desktop_action(app: AppHandle, action: String) -> Result<(), String> {
    menu_action(&app, &action)
}

#[tauri::command]
pub fn open_accessibility_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            .spawn()
            .map_err(|_| "システム設定を開けませんでした".to_string())?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    Err("この設定は macOS で利用できます".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn double_copy_requires_two_presses_within_half_a_second() {
        let now = Instant::now();
        let mut copy = DoubleCopy::default();
        assert!(!copy.press(now));
        assert!(!copy.press(now + Duration::from_millis(501)));
        assert!(copy.press(now + Duration::from_millis(1001)));
        assert!(!copy.press(now + Duration::from_millis(1100)));
        copy.reset();
        assert!(!copy.press(now + Duration::from_millis(1200)));
        assert!(copy.press(now + Duration::from_millis(1300)));
    }
}
