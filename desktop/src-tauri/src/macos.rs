//! macOS のコピー操作と、他のアプリでの ⌘C の監視。
use block2::RcBlock;
use objc2::{rc::Retained, runtime::AnyObject};
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::{cell::RefCell, ffi::c_void, ptr::NonNull};
use tauri::AppHandle;

static MONITOR_ACTIVE: AtomicBool = AtomicBool::new(false);
static COPY_EVENTS: AtomicU64 = AtomicU64::new(0);

pub fn monitor_status() -> (bool, u64) {
    (
        MONITOR_ACTIVE.load(Ordering::Relaxed),
        COPY_EVENTS.load(Ordering::Relaxed),
    )
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn CGEventCreateKeyboardEvent(source: *const c_void, key: u16, down: bool) -> *mut c_void;
    fn CGEventSetFlags(event: *mut c_void, flags: u64);
    fn CGEventPost(tap: u32, event: *const c_void);
    fn CFRelease(value: *const c_void);
}

thread_local! {
    static MONITOR: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
}

pub fn trusted() -> bool {
    // 引数や所有権を伴わない、OS の許可状態の読み取り。
    unsafe { AXIsProcessTrusted() }
}

pub fn copy_selection() -> Result<(), String> {
    if !trusted() {
        return Err("選んだ文を取得するには、設定からアクセシビリティを許可してください".into());
    }
    // kVK_ANSI_C = 8。イベントは OS に投稿した後、それぞれ解放する。
    unsafe {
        let down = CGEventCreateKeyboardEvent(std::ptr::null(), 8, true);
        let up = CGEventCreateKeyboardEvent(std::ptr::null(), 8, false);
        if down.is_null() || up.is_null() {
            if !down.is_null() {
                CFRelease(down);
            }
            if !up.is_null() {
                CFRelease(up);
            }
            return Err("コピー操作を送れませんでした".into());
        }
        for event in [down, up] {
            CGEventSetFlags(event, 1 << 20); // kCGEventFlagMaskCommand
            CGEventPost(0, event); // kCGHIDEventTap
            CFRelease(event);
        }
    }
    Ok(())
}

pub fn install_monitor(app: &AppHandle) {
    MONITOR_ACTIVE.store(false, Ordering::Relaxed);
    MONITOR.with(|slot| {
        if let Some(monitor) = slot.borrow_mut().take() {
            // 登録した AppKit の監視は、同じメインスレッドで解除する。
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
    });
    if !trusted() {
        return;
    }
    let handle = app.clone();
    let callback = RcBlock::new(move |event: NonNull<NSEvent>| {
        // AppKit はハンドラーの呼び出し中、この NSEvent を生存させる。
        let event = unsafe { event.as_ref() };
        let modifiers = event.modifierFlags();
        let other = NSEventModifierFlags::Shift
            | NSEventModifierFlags::Control
            | NSEventModifierFlags::Option;
        let copy = event.keyCode() == 8
            && modifiers.contains(NSEventModifierFlags::Command)
            && !modifiers.intersects(other);
        if copy && !event.isARepeat() {
            COPY_EVENTS.fetch_add(1, Ordering::Relaxed);
        }
        crate::resident::copy_key(&handle, copy, event.isARepeat());
    });
    let monitor =
        NSEvent::addGlobalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &callback);
    MONITOR_ACTIVE.store(monitor.is_some(), Ordering::Relaxed);
    MONITOR.with(|slot| *slot.borrow_mut() = monitor);
}

pub fn watch_permission(app: &AppHandle) {
    let mut previous = trusted();
    install_monitor(app);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let current = trusted();
            if current != previous {
                let handle = app.clone();
                if app
                    .run_on_main_thread(move || install_monitor(&handle))
                    .is_err()
                {
                    break;
                }
                previous = current;
            }
        }
    });
}
