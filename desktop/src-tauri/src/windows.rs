//! Windows のコピー操作と Ctrl+C の監視。入力内容は保存しない。
use std::{
    cell::{Cell, RefCell},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};
use tauri::AppHandle;
use windows_sys::Win32::{
    System::LibraryLoader::GetModuleHandleW,
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static COPY_EVENTS: AtomicU64 = AtomicU64::new(0);
thread_local! {
    static APP: RefCell<Option<AppHandle>> = const { RefCell::new(None) };
    static C_DOWN: Cell<bool> = const { Cell::new(false) };
}

pub fn monitor_status() -> (bool, u64) {
    (
        ACTIVE.load(Ordering::Relaxed),
        COPY_EVENTS.load(Ordering::Relaxed),
    )
}

fn pressed(key: u16) -> bool {
    // キーの現在の押下状態だけを読む。
    unsafe { GetAsyncKeyState(key as i32) < 0 }
}

unsafe extern "system" fn keyboard(code: i32, message: usize, data: isize) -> isize {
    if code == HC_ACTION as i32 {
        // WH_KEYBOARD_LL はコールバックが返るまでこの構造体を保持する。
        let event = &*(data as *const KBDLLHOOKSTRUCT);
        if event.flags & LLKHF_INJECTED == 0 {
            let down = message == WM_KEYDOWN as usize || message == WM_SYSKEYDOWN as usize;
            let up = message == WM_KEYUP as usize || message == WM_SYSKEYUP as usize;
            let is_c = event.vkCode == u32::from(VK_C);
            if is_c && up {
                C_DOWN.set(false);
            }
            if down {
                let repeated = is_c && C_DOWN.replace(is_c);
                let copy = is_c
                    && pressed(VK_CONTROL)
                    && ![VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN]
                        .into_iter()
                        .any(pressed);
                if copy && !repeated {
                    COPY_EVENTS.fetch_add(1, Ordering::Relaxed);
                }
                // 修飾キーの離し直しは連続コピーを妨げない。
                if ![
                    VK_CONTROL,
                    VK_LCONTROL,
                    VK_RCONTROL,
                    VK_SHIFT,
                    VK_LSHIFT,
                    VK_RSHIFT,
                    VK_MENU,
                    VK_LMENU,
                    VK_RMENU,
                    VK_LWIN,
                    VK_RWIN,
                ]
                .contains(&(event.vkCode as u16))
                {
                    APP.with_borrow(|app| {
                        if let Some(app) = app {
                            crate::resident::copy_key(app, copy, repeated);
                        }
                    });
                }
            }
        }
    }
    // コピーを含め、ほかのアプリへのキー入力を妨げない。
    CallNextHookEx(std::ptr::null_mut(), code, message, data)
}

pub fn install_monitor(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || unsafe {
        APP.with_borrow_mut(|slot| *slot = Some(app));
        let hook = SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(keyboard),
            GetModuleHandleW(std::ptr::null()),
            0,
        );
        if hook.is_null() {
            APP.with_borrow_mut(|slot| *slot = None);
            return;
        }
        ACTIVE.store(true, Ordering::Relaxed);
        // 監視を登録したスレッドでメッセージを処理する。
        let mut message = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        UnhookWindowsHookEx(hook);
        ACTIVE.store(false, Ordering::Relaxed);
        APP.with_borrow_mut(|slot| *slot = None);
    });
}

pub async fn copy_selection() -> Result<(), String> {
    // 登録ショートカットの Shift/Alt 等が残ったまま Ctrl+C を送らない。
    for _ in 0..80 {
        if ![VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN]
            .into_iter()
            .any(pressed)
        {
            let keys = [
                (VK_CONTROL, 0),
                (VK_C, 0),
                (VK_C, KEYEVENTF_KEYUP),
                (VK_CONTROL, KEYEVENTF_KEYUP),
            ];
            let inputs = keys.map(|(key, flags)| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: key,
                        dwFlags: flags,
                        ..unsafe { std::mem::zeroed() }
                    },
                },
            });
            // SendInput に渡す配列は呼び出し中ずっと生存する。
            let sent = unsafe {
                SendInput(
                    inputs.len() as u32,
                    inputs.as_ptr(),
                    std::mem::size_of::<INPUT>() as i32,
                )
            };
            return if sent == inputs.len() as u32 {
                Ok(())
            } else {
                Err("コピー操作を送れませんでした。管理者権限で動くアプリからは取得できない場合があります".into())
            };
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Err("ショートカットのキーを離してから、もう一度お試しください".into())
}
