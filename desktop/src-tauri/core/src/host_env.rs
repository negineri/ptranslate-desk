//! AppImage で起動したとき、端末のプログラムへ AppImage の環境を渡さないようにする。Linux だけで使う。
//!
//! AppImage の AppRun は `LD_LIBRARY_PATH` などの先頭に同梱物の場所 (`$APPDIR/usr/lib` など) を足す。
//! 子のプロセスがそのまま引き継ぐと、端末の推論エンジンやブラウザーが同梱の古いライブラリ
//! (OpenSSL など) を読み込み、版の食い違いで起動できなくなる。

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::Command;

/// AppRun が `$APPDIR` の下の場所を先頭に足す、`:` 区切りの変数。
const PATH_LISTS: &[&str] = &[
    "LD_LIBRARY_PATH",
    "PATH",
    "XDG_DATA_DIRS",
    "PYTHONPATH",
    "PERLLIB",
    "QT_PLUGIN_PATH",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_PLUGIN_SYSTEM_PATH_1_0",
];

/// linuxdeploy の GTK プラグインが `$APPDIR` の下を指すよう設定する変数。
const APPDIR_VALUES: &[&str] = &[
    "GTK_DATA_PREFIX",
    "GSETTINGS_SCHEMA_DIR",
    "GI_TYPELIB_PATH",
    "GIO_MODULE_DIR",
    "GTK_EXE_PREFIX",
    "GTK_PATH",
    "GTK_IM_MODULE_FILE",
    "GDK_PIXBUF_MODULE_FILE",
];

/// 端末のプログラムを起動する前に、AppImage が足した環境を取り除く。AppImage 以外では何もしない。
pub fn use_host_env(command: &mut Command) -> &mut Command {
    let appimage = std::env::var_os("APPIMAGE");
    let appdir = std::env::var_os("APPDIR");
    let (Some(_), Some(appdir)) = (appimage, appdir) else {
        return command;
    };
    for (key, value) in overrides(&appdir, |key| std::env::var_os(key)) {
        match value {
            Some(value) => command.env(key, value),
            None => command.env_remove(key),
        };
    }
    command
}

/// 変数ごとに、新しい値 (`None` は削除) を返す。変える必要のない変数は含めない。
fn overrides(
    appdir: &OsStr,
    get: impl Fn(&str) -> Option<OsString>,
) -> Vec<(&'static str, Option<OsString>)> {
    let appdir = Path::new(appdir);
    if appdir.as_os_str().is_empty() || !appdir.is_absolute() {
        return vec![];
    }
    let mut result = vec![];
    for &key in PATH_LISTS {
        let Some(value) = get(key) else { continue };
        // AppRun は元の値の前に足すため、末尾が空の項目になることがある。空の項目は
        // 作業ディレクトリを意味するので、`$APPDIR` の下の項目と一緒に取り除く。
        let kept: Vec<_> = std::env::split_paths(&value)
            .filter(|path| !path.as_os_str().is_empty() && !path.starts_with(appdir))
            .collect();
        let kept = if kept.is_empty() {
            None
        } else {
            std::env::join_paths(kept).ok()
        };
        if kept.as_ref() != Some(&value) {
            result.push((key, kept));
        }
    }
    for &key in APPDIR_VALUES {
        if get(key).is_some_and(|value| Path::new(&value).starts_with(appdir)) {
            result.push((key, None));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn run(env: &[(&str, &str)]) -> HashMap<&'static str, Option<String>> {
        let env: HashMap<_, _> = env.iter().copied().collect();
        overrides(OsStr::new("/tmp/.mount_PTrans1"), |key| {
            env.get(key).map(OsString::from)
        })
        .into_iter()
        .map(|(key, value)| (key, value.map(|v| v.into_string().unwrap())))
        .collect()
    }

    #[test]
    fn removes_bundled_library_paths_and_keeps_the_hosts() {
        let changed = run(&[
            (
                "LD_LIBRARY_PATH",
                "/tmp/.mount_PTrans1/usr/lib/:/tmp/.mount_PTrans1/lib64/:/opt/host/lib",
            ),
            (
                "PATH",
                "/tmp/.mount_PTrans1/usr/bin/:/tmp/.mount_PTrans1/bin/:/usr/local/bin:/usr/bin",
            ),
            ("XDG_DATA_DIRS", "/tmp/.mount_PTrans1/usr/share:/usr/share:"),
            ("PYTHONPATH", "/tmp/.mount_PTrans1/usr/share/pyshared/:"),
        ]);
        assert_eq!(changed["LD_LIBRARY_PATH"].as_deref(), Some("/opt/host/lib"));
        assert_eq!(changed["PATH"].as_deref(), Some("/usr/local/bin:/usr/bin"));
        assert_eq!(changed["XDG_DATA_DIRS"].as_deref(), Some("/usr/share"));
        // 元の値がなかった変数は、空の項目 (作業ディレクトリ) を残さずに消す。
        assert_eq!(changed["PYTHONPATH"], None);
    }

    #[test]
    fn removes_only_gtk_settings_that_point_into_the_appimage() {
        let changed = run(&[
            ("GIO_MODULE_DIR", "/tmp/.mount_PTrans1//usr/lib/gio/modules"),
            ("GTK_PATH", "/usr/lib/x86_64-linux-gnu/gtk-3.0"),
        ]);
        assert_eq!(changed.get("GIO_MODULE_DIR"), Some(&None));
        assert!(!changed.contains_key("GTK_PATH"));
    }

    #[test]
    fn leaves_host_values_and_similar_prefixes_untouched() {
        let changed = run(&[
            (
                "LD_LIBRARY_PATH",
                "/tmp/.mount_PTrans10/usr/lib:/opt/host/lib",
            ),
            ("PATH", "/usr/local/bin:/usr/bin"),
        ]);
        assert!(changed.is_empty(), "{changed:?}");
        assert!(overrides(OsStr::new(""), |_| Some("/usr/lib".into())).is_empty());
        assert!(overrides(OsStr::new("relative"), |_| Some("relative/lib".into())).is_empty());
    }
}
