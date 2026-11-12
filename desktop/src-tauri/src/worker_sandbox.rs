//! 資格を読む前に子プロセスへ適用する OS の制限。

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Ports {
    pub(crate) relay: u16,
    pub(crate) engine: u16,
}

#[cfg(any(not(target_os = "linux"), test))]
impl Ports {
    pub(crate) fn new(relay: u16, engine: u16) -> Result<Self, ()> {
        if relay == 0 || engine == 0 || relay == engine {
            return Err(());
        }
        Ok(Self { relay, engine })
    }

    pub(crate) fn parse(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<Self, ()> {
        let mut port = || args.next()?.to_str()?.parse::<u16>().ok();
        let ports = Self::new(port().ok_or(())?, port().ok_or(())?)?;
        if args.next().is_some() {
            return Err(());
        }
        Ok(ports)
    }
}

#[cfg(target_os = "linux")]
pub(crate) mod linux;

pub(crate) fn supported() -> bool {
    #[cfg(target_os = "linux")]
    return linux::supported();
    #[cfg(not(target_os = "linux"))]
    cfg!(target_os = "macos")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn apply(_: Ports) -> Result<(), ()> {
    Err(())
}

#[cfg(target_os = "macos")]
pub(crate) fn apply(ports: Ports) -> Result<(), ()> {
    use std::ffi::{c_char, c_int, CString};

    #[link(name = "sandbox")]
    extern "C" {
        fn sandbox_init(profile: *const c_char, flags: u64, error: *mut *mut c_char) -> c_int;
        fn sandbox_free_error(error: *mut c_char);
    }

    let profile = CString::new(format!(
        r#"(version 1)
(deny default)
(allow system-socket (socket-domain AF_INET))
(allow network-outbound (remote ip "localhost:{}") (remote ip "localhost:{}"))"#,
        ports.relay, ports.engine
    ))
    .map_err(|_| ())?;
    let mut error = std::ptr::null_mut();
    // profile は NUL 終端で、error は sandbox_init が返した場合だけ解放する。
    let result = unsafe { sandbox_init(profile.as_ptr(), 0, &mut error) };
    if !error.is_null() {
        unsafe { sandbox_free_error(error) };
    }
    if result == 0 {
        Ok(())
    } else {
        Err(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_two_distinct_nonzero_ports() {
        let parse = |args: &[&str]| Ports::parse(args.iter().map(std::ffi::OsString::from));
        assert_eq!(parse(&["3001", "3002"]), Ports::new(3001, 3002));
        for args in [
            &["3001"][..],
            &["3001", "3002", "3003"],
            &["3001", "3001"],
            &["0", "3002"],
            &["3001", "65536"],
            &["3001", "localhost"],
        ] {
            assert_eq!(parse(args), Err(()));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_seatbelt_limits_worker_connections_and_files() {
        use std::{
            fs::{File, OpenOptions},
            io::ErrorKind,
            net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
            process::Command,
            time::Duration,
        };

        const PROBE: &str = "PTRANSLATE_SEATBELT_TEST";
        if let Ok(ports) = std::env::var(PROBE) {
            let mut ports = ports.split(',').map(|port| port.parse::<u16>().unwrap());
            let allowed = Ports::new(ports.next().unwrap(), ports.next().unwrap()).unwrap();
            let denied = ports.next().unwrap();
            let path = std::env::var_os("PTRANSLATE_SEATBELT_TEST_FILE").unwrap();
            File::open(&path).unwrap();
            OpenOptions::new().write(true).open(&path).unwrap();
            apply(allowed).unwrap();
            for port in [allowed.relay, allowed.engine] {
                TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            }
            assert_eq!(
                TcpStream::connect_timeout(
                    &SocketAddr::from(([192, 0, 2, 1], allowed.relay)),
                    Duration::from_secs(1)
                )
                .unwrap_err()
                .kind(),
                ErrorKind::PermissionDenied
            );
            assert_eq!(
                TcpStream::connect((Ipv4Addr::LOCALHOST, denied))
                    .unwrap_err()
                    .kind(),
                ErrorKind::PermissionDenied
            );
            assert_eq!(
                TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                    .unwrap_err()
                    .kind(),
                ErrorKind::PermissionDenied
            );
            assert_eq!(
                File::open(&path).unwrap_err().kind(),
                ErrorKind::PermissionDenied
            );
            assert_eq!(
                OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap_err()
                    .kind(),
                ErrorKind::PermissionDenied
            );
            return;
        }
        let listeners = [(); 3].map(|()| TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap());
        let [relay, engine, denied] = listeners.each_ref().map(|s| s.local_addr().unwrap().port());
        let file = tempfile::NamedTempFile::new().unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "worker_sandbox::tests::macos_seatbelt_limits_worker_connections_and_files",
                "--nocapture",
            ])
            .env(PROBE, format!("{relay},{engine},{denied}"))
            .env("PTRANSLATE_SEATBELT_TEST_FILE", file.path())
            .status()
            .unwrap();
        assert!(status.success());
    }
}
