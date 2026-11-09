use super::{protocol::*, valid_id};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, DirBuilder, File},
    io,
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, FileTypeExt, MetadataExt},
    },
    path::{Path, PathBuf},
};
use tauri::AppHandle;
use tokio::net::{UnixListener, UnixStream};

fn manifest_path() -> io::Result<PathBuf> {
    #[cfg(feature = "smoke-test")]
    if let Some(dir) = std::env::var_os("PTRANSLATE_SMOKE_CONFIG") {
        return Ok(PathBuf::from(dir).join(format!("{HOST}.json")));
    }
    Ok(std::env::home_dir()
        .ok_or_else(|| io::Error::other("ホームフォルダがありません"))?
        .join(format!(
            "Library/Application Support/Google/Chrome/NativeMessagingHosts/{HOST}.json"
        )))
}

pub(super) fn register(id: &str) -> io::Result<()> {
    write_manifest(&manifest_path()?, id)
}

fn write_manifest(path: &Path, id: &str) -> io::Result<()> {
    if !valid_id(id) {
        return Err(io::Error::other("拡張IDが不正です"));
    }
    let dir = path.parent().unwrap();
    fs::create_dir_all(dir)?;
    let manifest = json!({
        "name": HOST,
        "description": "PTranslateのローカル翻訳",
        "path": executable()?,
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{id}/")],
    });
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer_pretty(&mut file, &manifest)?;
    file.persist(path)?;
    Ok(())
}

fn user_id() -> u32 {
    // geteuidは引数やメモリを参照しない。
    unsafe { libc::geteuid() }
}

fn executable() -> io::Result<PathBuf> {
    std::env::current_exe()?.canonicalize()
}

fn socket_dir() -> io::Result<PathBuf> {
    let executable = executable()?;
    let hash = format!("{:x}", Sha256::digest(executable.as_os_str().as_bytes()));
    // macOSのUnixソケットはパスが短い必要がある。Chromeとアプリの環境変数に依存させない。
    Ok(PathBuf::from(format!(
        "/tmp/ptranslate-{}-{}",
        user_id(),
        &hash[..32]
    )))
}

fn check_dir(dir: &Path) -> io::Result<()> {
    let metadata = dir.symlink_metadata()?;
    if !metadata.is_dir() || metadata.uid() != user_id() || metadata.mode() & 0o777 != 0o700 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "中継用のフォルダが安全ではありません",
        ));
    }
    Ok(())
}

fn listen(dir: &Path) -> io::Result<(UnixListener, File)> {
    match DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    check_dir(dir)?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("lock"))?;
    lock.try_lock()?;
    let path = dir.join("bridge.sock");
    match path.symlink_metadata() {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(&path)?,
        Ok(_) => return Err(io::Error::other("中継用ソケットの場所が使用中です")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok((UnixListener::bind(path)?, lock))
}

pub fn host_entry() -> bool {
    let Some(origin) = std::env::args().nth(1) else {
        return false;
    };
    if !origin.starts_with("chrome-extension://") {
        return false;
    }
    let result = (|| -> io::Result<()> {
        let manifest: Value = serde_json::from_slice(&fs::read(manifest_path()?)?)?;
        if manifest["allowed_origins"] != json!([origin])
            || manifest["path"] != json!(executable()?)
        {
            return Err(io::Error::other("許可されていない拡張です"));
        }
        run_host(async {
            let dir = socket_dir()?;
            check_dir(&dir)?;
            let stream = UnixStream::connect(dir.join("bridge.sock")).await?;
            if stream.peer_cred()?.uid() != user_id() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "別の利用者の中継先です",
                ));
            }
            Ok(stream)
        })
    })();
    exit_host(result);
}

pub fn schedule(app: &AppHandle) {
    let result = tauri::async_runtime::block_on(async {
        let dir = socket_dir()?;
        let started = std::time::Instant::now();
        loop {
            match listen(&dir) {
                // LaunchServicesでの再起動では、前のプロセスの終了を少し待つ。
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        && started.elapsed() < std::time::Duration::from_secs(1) =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                result => return result,
            }
        }
    });
    let Ok((listener, lock)) = result else {
        super::unavailable(app);
        return;
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _lock = lock;
        while let Ok((stream, _)) = listener.accept().await {
            if stream.peer_cred().is_ok_and(|peer| peer.uid() == user_id()) {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = serve(app, stream).await;
                });
            }
        }
        super::unavailable(&app);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_allows_only_the_registered_extension() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(format!("{HOST}.json"));
        let id = "a".repeat(32);
        write_manifest(&path, &id).unwrap();
        assert!(write_manifest(&path, "invalid").is_err());
        let manifest: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(
            manifest["allowed_origins"],
            json!([format!("chrome-extension://{id}/")])
        );
        assert_eq!(manifest["path"], json!(executable().unwrap()));
    }

    #[tokio::test]
    async fn socket_is_private_and_stale_socket_can_be_replaced() {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let dir = temp.path().join("ipc");
        let (listener, lock) = listen(&dir).unwrap();
        assert!(listen(&dir).is_err());
        let client = UnixStream::connect(dir.join("bridge.sock")).await.unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        assert_eq!(client.peer_cred().unwrap().uid(), user_id());
        assert_eq!(stream.peer_cred().unwrap().uid(), user_id());
        drop((listener, lock));
        assert!(listen(&dir).is_ok());
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(check_dir(&link).is_err());
    }
}
