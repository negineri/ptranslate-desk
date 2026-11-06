//! macOS のコピー操作と、他のアプリでの ⌘C の監視。
use block2::RcBlock;
use objc2::{rc::Retained, runtime::AnyObject};
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};
use std::{cell::RefCell, ffi::c_void, ptr::NonNull};
use tauri::AppHandle;

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
        crate::resident::copy_key(&handle, copy, event.isARepeat());
    });
    let monitor =
        NSEvent::addGlobalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &callback);
    MONITOR.with(|slot| *slot.borrow_mut() = monitor);
}
