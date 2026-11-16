//! 実行ユーザー専用の状態ディレクトリ。refresh token とワーカー資格を保存する。
//! 形式は以前の Python 版の CLI (`tools/volunteer_worker.py`) と同じ `worker.json`。
use crate::http::{Failure, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    os::unix::io::AsRawFd,
    path::{Path, PathBuf},
};
use zeroize::Zeroize;

#[derive(Default, Deserialize, Serialize)]
pub struct State {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub broker: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_secret: Option<String>,
    #[serde(default)]
    pub expires_at: u64,
    #[serde(default)]
    pub renew_at: u64,
}

impl Drop for State {
    fn drop(&mut self) {
        self.refresh_token.zeroize();
        self.worker_secret.zeroize();
    }
}

/// 状態ディレクトリの排他ロック。保持している間だけ読み書きする。
pub struct StateDir {
    path: PathBuf,
    _lock: fs::File,
}

impl StateDir {
    pub fn open(directory: &Path) -> Result<Self> {
        let bad = || Failure::new("状態ディレクトリは実行ユーザー所有の 0700 にしてください");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)
            .map_err(|_| Failure::new("状態ディレクトリを作成できませんでした"))?;
        let info = fs::symlink_metadata(directory).map_err(|_| bad())?;
        if !info.is_dir() || info.uid() != unsafe { libc::getuid() } || info.mode() & 0o077 != 0 {
            return Err(bad());
        }
        let lock = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(directory.join("lock"))
            .map_err(|_| bad())?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(Failure::new(
                "同じ状態ディレクトリを別のプロセスが使用中です",
            ));
        }
        Ok(Self {
            path: directory.join("worker.json"),
            _lock: lock,
        })
    }

    pub fn load(&self) -> Result<State> {
        let file = match fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
            Err(_) => return Err(Failure::new("状態ファイルを読み込めませんでした")),
        };
        let info = file
            .metadata()
            .map_err(|_| Failure::new("状態ファイルを読み込めませんでした"))?;
        if !info.is_file() || info.uid() != unsafe { libc::getuid() } || info.mode() & 0o077 != 0 {
            return Err(Failure::new(
                "状態ファイルは実行ユーザー所有の 0600 にしてください",
            ));
        }
        serde_json::from_reader(std::io::BufReader::new(std::io::Read::take(
            file,
            1024 * 1024,
        )))
        .map_err(|_| Failure::new("状態ファイルが不正です"))
    }

    pub fn save(&self, state: &State) -> Result<()> {
        let failed = || Failure::new("状態ファイルを保存できませんでした");
        let parent = self.path.parent().ok_or_else(failed)?;
        // NamedTempFile は 0600 で作られる。
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|_| failed())?;
        let mut bytes = serde_json::to_vec(state).map_err(|_| failed())?;
        let written = file
            .write_all(&bytes)
            .and_then(|_| file.as_file().sync_all());
        bytes.zeroize();
        written.map_err(|_| failed())?;
        file.persist(&self.path).map_err(|_| failed())?;
        fs::File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|_| failed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn saves_with_owner_only_permissions_and_rejects_shared_files() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("state");
        let state_dir = StateDir::open(&dir).unwrap();
        assert_eq!(fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
        assert_eq!(state_dir.load().unwrap().broker, "");
        let mut state = State::default();
        state.broker = "https://translate.example.com".into();
        state.refresh_token = Some("refresh".into());
        state.expires_at = 10;
        state_dir.save(&state).unwrap();
        let path = dir.join("worker.json");
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            saved,
            serde_json::json!({
                "broker": "https://translate.example.com",
                "refresh_token": "refresh",
                "expires_at": 10,
                "renew_at": 0,
            })
        );
        assert_eq!(
            state_dir.load().unwrap().refresh_token.as_deref(),
            Some("refresh")
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(state_dir.load().is_err());
    }

    #[test]
    fn rejects_open_directories_and_concurrent_use() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("state");
        let first = StateDir::open(&dir).unwrap();
        assert!(StateDir::open(&dir)
            .err()
            .unwrap()
            .message
            .contains("使用中"));
        drop(first);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(StateDir::open(&dir).is_err());
    }

    #[test]
    fn reads_state_written_by_the_python_cli() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = StateDir::open(&temp.path().join("state")).unwrap();
        let path = temp.path().join("state/worker.json");
        fs::write(
            &path,
            br#"{"broker":"https://b.example","client_id":"c","token_url":"https://i.example/t","refresh_token":"r","worker_id":"0123456789abcdef0123456789abcdef","worker_secret":"ptw_x","expires_at":100,"renew_at":50}"#,
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let state = state_dir.load().unwrap();
        assert_eq!(state.token_url, "https://i.example/t");
        assert_eq!(state.worker_secret.as_deref(), Some("ptw_x"));
        assert_eq!((state.expires_at, state.renew_at), (100, 50));
    }
}
