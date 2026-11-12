//! ワーカーの新規受付に使う電源状態。不明な場合は受付を見送る。

#[cfg(any(target_os = "windows", test))]
fn windows_ac(value: u8) -> Option<bool> {
    match value {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn external_power() -> Option<bool> {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

    let mut status = SYSTEM_POWER_STATUS::default();
    // OS が構造体へ状態を書き込む。失敗した場合は値を使わない。
    if unsafe { GetSystemPowerStatus(&mut status) } == 0 {
        return None;
    }
    windows_ac(status.ACLineStatus)
}

#[cfg(any(target_os = "macos", test))]
fn mac_power_source(value: &[u8]) -> Option<bool> {
    match value {
        b"AC Power" => Some(true),
        b"Battery Power" | b"UPS Power" => Some(false),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn external_power() -> Option<bool> {
    use std::ffi::{c_char, c_void, CStr};

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOPSCopyPowerSourcesInfo() -> *const c_void;
        fn IOPSGetProvidingPowerSourceType(snapshot: *const c_void) -> *const c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringGetCString(
            string: *const c_void,
            buffer: *mut c_char,
            size: isize,
            encoding: u32,
        ) -> u8;
        fn CFRelease(value: *const c_void);
    }

    const UTF8: u32 = 0x0800_0100;
    let mut buffer = [0 as c_char; 32];
    // Copyで得た所有権はここで解放し、借用した電源名は解放前にコピーする。
    let copied = unsafe {
        let snapshot = IOPSCopyPowerSourcesInfo();
        if snapshot.is_null() {
            return None;
        }
        let source = IOPSGetProvidingPowerSourceType(snapshot);
        let copied = !source.is_null()
            && CFStringGetCString(source, buffer.as_mut_ptr(), buffer.len() as isize, UTF8) != 0;
        CFRelease(snapshot);
        copied
    };
    // コピーに成功した場合だけ、NUL終端されたバッファを読む。
    copied
        .then(|| unsafe { CStr::from_ptr(buffer.as_ptr()) })
        .and_then(|source| mac_power_source(source.to_bytes()))
}

#[cfg(target_os = "linux")]
pub(crate) fn external_power() -> Option<bool> {
    static VM: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    linux_power(std::path::Path::new("/sys/class/power_supply"), || {
        *VM.get_or_init(|| {
            std::process::Command::new("/usr/bin/systemd-detect-virt")
                .args(["--vm", "--quiet"])
                .status()
                .is_ok_and(|status| status.success())
        })
    })
}

#[cfg(any(target_os = "linux", test))]
fn linux_power(root: &std::path::Path, is_vm: impl FnOnce() -> bool) -> Option<bool> {
    let mut offline = false;
    let mut unknown = false;
    let mut empty = true;
    for entry in std::fs::read_dir(root).ok()? {
        empty = false;
        let path = entry.ok()?.path();
        let kind = std::fs::read_to_string(path.join("type")).ok()?;
        if matches!(kind.trim(), "Battery" | "UPS") {
            continue;
        }
        // 周辺機器のバッテリーや給電先を、PC の電源として扱わない。
        let scope = std::fs::read_to_string(path.join("scope")).ok();
        let scope = scope.as_deref().map(str::trim);
        if scope == Some("Device") || (kind.trim() != "Mains" && scope != Some("System")) {
            continue;
        }
        match std::fs::read_to_string(path.join("online"))
            .as_deref()
            .map(str::trim)
        {
            Ok("1") => return Some(true),
            Ok("0") => offline = true,
            _ => unknown = true,
        }
    }
    if empty && is_vm() {
        return Some(true);
    }
    (offline && !unknown).then_some(false)
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub(crate) fn external_power() -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_requires_a_system_supply_online() {
        let root = tempfile::tempdir().unwrap();
        let supply = root.path().join("AC");
        std::fs::create_dir(&supply).unwrap();
        std::fs::write(supply.join("type"), "Mains\n").unwrap();
        assert_eq!(linux_power(root.path(), || false), None);
        assert_eq!(linux_power(root.path(), || true), None);
        std::fs::write(supply.join("online"), "0\n").unwrap();
        assert_eq!(linux_power(root.path(), || true), Some(false));
        std::fs::write(supply.join("online"), "1\n").unwrap();
        assert_eq!(linux_power(root.path(), || false), Some(true));
        std::fs::write(supply.join("scope"), "Device\n").unwrap();
        assert_eq!(linux_power(root.path(), || false), None);
    }

    #[test]
    fn linux_vm_without_power_devices_can_offer_work() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(linux_power(root.path(), || true), Some(true));
        assert_eq!(linux_power(root.path(), || false), None);
        assert_eq!(linux_power(&root.path().join("missing"), || true), None);
    }

    #[test]
    fn only_confirmed_ac_power_allows_a_new_job() {
        assert_eq!(windows_ac(1), Some(true));
        assert_eq!(windows_ac(0), Some(false));
        assert_eq!(windows_ac(255), None);
        assert_eq!(mac_power_source(b"AC Power"), Some(true));
        assert_eq!(mac_power_source(b"Battery Power"), Some(false));
        assert_eq!(mac_power_source(b"UPS Power"), Some(false));
        assert_eq!(mac_power_source(b""), None);
    }
}
