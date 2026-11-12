//! 名前空間に二つの中継だけを入れ、起動後は Landlock でファイルへのアクセスを閉じる。
use std::{
    os::fd::{FromRawFd, OwnedFd},
    path::Path,
    process::{Command as StdCommand, Stdio},
    sync::OnceLock,
};
use tokio::process::Command;

pub(crate) const BROKER: &str = "/broker.sock";
pub(crate) const ENGINE: &str = "/engine.sock";
const BWRAP: &str = "/usr/bin/bwrap";

fn abi() -> libc::c_long {
    // バージョン問い合わせではポインターを渡さない。
    unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, 0usize, 0usize, 1u32) }
}

pub(super) fn supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        abi() >= 4
            && base_command()
                .args(["--ro-bind", "/usr/bin/true", "/probe", "--", "/probe"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
    })
}

fn base_command() -> StdCommand {
    let mut command = StdCommand::new(BWRAP);
    command
        .env_clear()
        .args([
            "--unshare-all",
            "--unshare-user",
            "--disable-userns",
            "--die-with-parent",
            "--new-session",
            "--cap-drop",
            "ALL",
        ])
        .args(["--ro-bind", "/usr/lib", "/usr/lib"])
        .args([
            "--symlink",
            "usr/lib",
            "/lib",
            "--symlink",
            "usr/lib64",
            "/lib64",
        ]);
    if Path::new("/usr/lib64").is_dir() {
        command.args(["--ro-bind", "/usr/lib64", "/usr/lib64"]);
    }
    command
}

pub(crate) fn command(executable: &Path, broker: &Path, engine: &Path) -> Command {
    let mut command = base_command();
    command
        .arg("--ro-bind")
        .arg(executable)
        .arg("/agent")
        .arg("--ro-bind")
        .arg(broker)
        .arg(BROKER)
        .arg("--ro-bind")
        .arg(engine)
        .arg(ENGINE)
        .args(["--", "/agent"]);
    Command::from(command)
}

pub(crate) fn apply() -> Result<(), ()> {
    let version = abi();
    if version < 4 {
        return Err(());
    }
    // ABI 4 のファイル操作と TCP の bind/connect に許可規則を足さない。
    let access: [u64; 2] = [(1 << 15) - 1, 0b11];
    let fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            access.as_ptr(),
            std::mem::size_of_val(&access),
            0u32,
        )
    };
    if fd < 0 {
        return Err(());
    }
    // 成功したシステムコールは新しいファイル記述子を返す。
    let rules = unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) };
    use std::os::fd::AsRawFd;
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0
        || unsafe { libc::syscall(libc::SYS_landlock_restrict_self, rules.as_raw_fd(), 0u32) } != 0
    {
        return Err(());
    }
    Ok(())
}
