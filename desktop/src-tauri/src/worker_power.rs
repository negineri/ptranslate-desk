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
    linux_power(std::path::Path::new("/sys/class/power_supply"))
}

#[cfg(any(target_os = "linux", test))]
fn linux_power(root: &std::path::Path) -> Option<bool> {
    let mut battery = false;
    let mut online = false;
    let mut offline = false;
    let mut unknown = false;
    for entry in std::fs::read_dir(root).ok()? {
        let path = entry.ok()?.path();
        let kind = std::fs::read_to_string(path.join("type")).ok()?;
        let scope = std::fs::read_to_string(path.join("scope")).ok();
        let scope = scope.as_deref().map(str::trim);
        if scope == Some("Device") {
            continue;
        }
        match kind.trim() {
            "Battery" | "UPS" => {
                battery = true;
                continue;
            }
            "Mains" => {}
            _ if scope == Some("System") => {}
            _ => continue,
        }
        match std::fs::read_to_string(path.join("online"))
            .as_deref()
            .map(str::trim)
        {
            Ok("1") => online = true,
            Ok("0") => offline = true,
            _ => unknown = true,
        }
    }
    match (online, offline, unknown, battery) {
        (true, _, _, _) | (false, false, false, false) => Some(true),
        (false, true, false, _) => Some(false),
        _ => None,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub(crate) fn external_power() -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_reads_system_supply_even_without_a_battery() {
        let root = tempfile::tempdir().unwrap();
        let supply = root.path().join("AC");
        std::fs::create_dir(&supply).unwrap();
        std::fs::write(supply.join("type"), "Mains\n").unwrap();
        assert_eq!(linux_power(root.path()), None);
        std::fs::write(supply.join("online"), "unknown\n").unwrap();
        assert_eq!(linux_power(root.path()), None);
        std::fs::write(supply.join("online"), "0\n").unwrap();
        assert_eq!(linux_power(root.path()), Some(false));
        std::fs::write(supply.join("online"), "1\n").unwrap();
        assert_eq!(linux_power(root.path()), Some(true));
        std::fs::write(supply.join("scope"), "Device\n").unwrap();
        assert_eq!(linux_power(root.path()), Some(true));
    }

    #[test]
    fn linux_without_system_power_devices_can_offer_work() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(linux_power(root.path()), Some(true));
        assert_eq!(linux_power(&root.path().join("missing")), None);
        let peripheral = root.path().join("mouse");
        std::fs::create_dir(&peripheral).unwrap();
        assert_eq!(linux_power(root.path()), None);
        std::fs::write(peripheral.join("type"), "Battery\n").unwrap();
        std::fs::write(peripheral.join("scope"), "Device\n").unwrap();
        assert_eq!(linux_power(root.path()), Some(true));
    }

    #[test]
    fn linux_battery_requires_a_system_supply_online() {
        let root = tempfile::tempdir().unwrap();
        let battery = root.path().join("BAT0");
        std::fs::create_dir(&battery).unwrap();
        std::fs::write(battery.join("type"), "Battery\n").unwrap();
        assert_eq!(linux_power(root.path()), None);

        let supply = root.path().join("AC");
        std::fs::create_dir(&supply).unwrap();
        std::fs::write(supply.join("type"), "Mains\n").unwrap();
        std::fs::write(supply.join("online"), "0\n").unwrap();
        assert_eq!(linux_power(root.path()), Some(false));
        let usb = root.path().join("USB");
        std::fs::create_dir(&usb).unwrap();
        std::fs::write(usb.join("type"), "USB\n").unwrap();
        std::fs::write(usb.join("scope"), "System\n").unwrap();
        assert_eq!(linux_power(root.path()), None);
        std::fs::write(supply.join("online"), "1\n").unwrap();
        assert_eq!(linux_power(root.path()), Some(true));
        std::fs::remove_file(usb.join("type")).unwrap();
        assert_eq!(linux_power(root.path()), None);
    }

    #[test]
    fn linux_ups_is_a_system_battery() {
        let root = tempfile::tempdir().unwrap();
        let ups = root.path().join("UPS");
        std::fs::create_dir(&ups).unwrap();
        std::fs::write(ups.join("type"), "UPS\n").unwrap();
        assert_eq!(linux_power(root.path()), None);
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
