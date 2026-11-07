//! 固定した配布元から取得し、検証が済んだファイルだけを有効にする。
use super::*;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

const ENGINE_URL: &str = "https://github.com/ggml-org/llama.cpp/releases/download/b11804/llama-b11804-bin-win-vulkan-x64.zip";
const ENGINE_SHA: &str = "db9ab31997e93bf713d3dfec6eb1cf1b5a6a5f2940a3d4d111ad0ffab54c87d5";
const MODEL_URL: &str = "https://huggingface.co/pfnet/plamo-3-translate-2b-GGUF/resolve/d44589ecd5287cd64452e9cd1e2cfdad904fe216/plamo-3-translate-2b-Q4_K_M.gguf";
const MODEL_SHA: &str = "2189a8a0b4b359f59443ec36cfb9c9c3f603a678c00dbcee8598b3f7ea7c12fb";
const ENGINE_BYTES: u64 = 36_865_203;
const MODEL_BYTES: u64 = 1_807_202_144;

#[derive(Default)]
pub(super) struct Downloads {
    gate: tokio::sync::Mutex<()>,
    cancel: Operation,
}
#[derive(Clone, Serialize)]
pub struct Info {
    supported: bool,
    model_name: &'static str,
    model_bytes: u64,
    engine_bytes: u64,
    model_url: &'static str,
    engine_url: &'static str,
}
pub(super) fn supported() -> bool {
    cfg!(all(target_os = "windows", target_arch = "x86_64"))
}
#[tauri::command]
pub fn local_download_info() -> Info {
    Info {
        supported: supported(),
        model_name: "PLaMo 3 Translate 2B（Q4_K_M）",
        model_bytes: MODEL_BYTES,
        engine_bytes: ENGINE_BYTES,
        model_url: MODEL_URL,
        engine_url: ENGINE_URL,
    }
}
#[derive(Clone, Serialize)]
pub struct Progress {
    phase: &'static str,
    item: &'static str,
    received: u64,
    total: u64,
}
fn report(
    channel: &Channel<Progress>,
    phase: &'static str,
    item: &'static str,
    received: u64,
    total: u64,
) -> Result<(), String> {
    channel
        .send(Progress {
            phase,
            item,
            received,
            total,
        })
        .map_err(|_| "ダウンロード画面が閉じられました".into())
}
fn require_main(window: &WebviewWindow) -> Result<(), String> {
    if window.label() == "main" {
        Ok(())
    } else {
        Err("メインウィンドウで操作してください".into())
    }
}
#[tauri::command]
pub fn cancel_local_download(app: AppHandle, window: WebviewWindow) -> Result<(), String> {
    require_main(&window)?;
    app.state::<Local>().downloads.cancel.cancel();
    Ok(())
}
fn verify(path: PathBuf, expected: &'static str, cancel: Arc<AtomicBool>) -> Result<bool, String> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err("取得済みのファイルを読み込めませんでした".into()),
    };
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err("ダウンロードを中止しました".into());
        }
        let size = file
            .read(&mut buffer)
            .map_err(|_| "ファイルを検証できませんでした")?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    Ok(format!("{:x}", hash.finalize()) == expected)
}
async fn fetch(
    client: &reqwest::Client,
    operation: &Operation,
    channel: &Channel<Progress>,
    item: &'static str,
    url: &str,
    path: &std::path::Path,
    expected: &'static str,
    total: u64,
) -> Result<(), String> {
    report(channel, "verify", item, 0, total)?;
    let cached = {
        let path = path.to_owned();
        let canceled = operation.canceled.clone();
        tauri::async_runtime::spawn_blocking(move || verify(path, expected, canceled))
            .await
            .map_err(|_| "検証を実行できませんでした")??
    };
    if cached {
        return report(channel, "download", item, total, total);
    }
    let mut response = tokio::select! {
        _ = operation.canceled() => return Err("ダウンロードを中止しました".into()),
        response = client.get(url).send() => response.map_err(|_| "ダウンロード先に接続できませんでした。通信状態を確認してやり直してください")?.error_for_status().map_err(|_| "ダウンロード先がエラーを返しました。時間をおいてやり直してください")?,
    };
    let mut file = tempfile::NamedTempFile::new_in(path.parent().ok_or("保存先が不正です")?)
        .map_err(|_| "保存用のファイルを作成できませんでした")?;
    let mut hash = Sha256::new();
    let mut received = 0;
    let mut last = Instant::now();
    report(channel, "download", item, 0, total)?;
    loop {
        let chunk = tokio::select! {
            _ = operation.canceled() => return Err("ダウンロードを中止しました".into()),
            chunk = response.chunk() => chunk.map_err(|_| "ダウンロードが途切れました。やり直してください")?,
        };
        let Some(chunk) = chunk else {
            break;
        };
        received += chunk.len() as u64;
        if received > total {
            return Err("取得したファイルのサイズが一致しません".into());
        }
        file.write_all(&chunk)
            .map_err(|_| "ファイルを保存できませんでした。空き容量を確認してください")?;
        hash.update(&chunk);
        if last.elapsed() >= Duration::from_millis(100) {
            report(channel, "download", item, received, total)?;
            last = Instant::now();
        }
    }
    report(channel, "verify", item, received, total)?;
    if received != total || format!("{:x}", hash.finalize()) != expected {
        return Err("取得したファイルの検証に失敗しました。やり直してください".into());
    }
    if operation.canceled.load(Ordering::SeqCst) {
        return Err("ダウンロードを中止しました".into());
    }
    file.as_file()
        .sync_all()
        .map_err(|_| "ファイルを保存できませんでした")?;
    file.persist(path)
        .map_err(|_| "ファイルを配置できませんでした")?;
    report(channel, "download", item, total, total)
}
fn extract(archive: &std::path::Path, destination: &std::path::Path) -> Result<(), String> {
    let file = fs::File::open(archive).map_err(|_| "推論エンジンを読み込めませんでした")?;
    let mut zip = zip::ZipArchive::new(file).map_err(|_| "推論エンジンのzipが不正です")?;
    let mut size = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|_| "zipの項目を読み込めませんでした")?;
        let name = entry.enclosed_name().ok_or("zipの保存先が不正です")?;
        if entry.is_symlink()
            || entry
                .name()
                .map_err(|_| "zipの項目名が不正です")?
                .contains(['\\', ':'])
        {
            return Err("zipに不正な項目があります".into());
        }
        size = size.checked_add(entry.size()).ok_or("zipが大きすぎます")?;
        if size > 512 * 1024 * 1024 {
            return Err("zipが大きすぎます".into());
        }
        let path = destination.join(name);
        if entry.is_dir() {
            fs::create_dir_all(path).map_err(|_| "展開先を作成できませんでした")?;
        } else {
            fs::create_dir_all(path.parent().ok_or("展開先が不正です")?)
                .map_err(|_| "展開先を作成できませんでした")?;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|_| "推論エンジンを展開できませんでした")?;
            std::io::copy(&mut entry, &mut output)
                .map_err(|_| "推論エンジンを展開できませんでした")?;
        }
    }
    Ok(())
}
#[tauri::command]
pub async fn download_local_resources(
    app: AppHandle,
    window: WebviewWindow,
    confirmed: bool,
    progress: Channel<Progress>,
) -> Result<(), String> {
    require_main(&window)?;
    if !confirmed {
        return Err("ダウンロードの確認が必要です".into());
    }
    if !supported() {
        return Err("ローカル翻訳の導入はWindows x64に対応しています".into());
    }
    let local = app.state::<Local>();
    let downloads = &local.downloads;
    let _gate = downloads
        .gate
        .try_lock()
        .map_err(|_| "ダウンロードはすでに実行中です")?;
    downloads.cancel.reset();
    if installation(&app).is_ok() {
        return report(&progress, "complete", "model", MODEL_BYTES, MODEL_BYTES);
    }
    let home = home(&app)?;
    fs::create_dir_all(&home).map_err(|_| "モデルの保存先を作成できませんでした")?;
    let cache_lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(home.join("download.lock"))
        .map_err(|_| "モデルの保存先を使用できませんでした")?;
    cache_lock
        .try_lock()
        .map_err(|_| "別のアプリでモデルを取得中です。完了してからやり直してください")?;
    let client = reqwest::Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|_| "ダウンロードの準備に失敗しました")?;
    let archive = home.join("llama-b11804-vulkan.zip");
    let model = home.join("plamo-3-translate-2b-Q4_K_M.gguf");
    fetch(
        &client,
        &downloads.cancel,
        &progress,
        "engine",
        ENGINE_URL,
        &archive,
        ENGINE_SHA,
        ENGINE_BYTES,
    )
    .await?;
    fetch(
        &client,
        &downloads.cancel,
        &progress,
        "model",
        MODEL_URL,
        &model,
        MODEL_SHA,
        MODEL_BYTES,
    )
    .await?;
    report(&progress, "extract", "engine", 0, ENGINE_BYTES)?;
    let staging = tempfile::Builder::new()
        .prefix("llama-b11804-")
        .tempdir_in(&home)
        .map_err(|_| "展開先を準備できませんでした")?;
    let archive_copy = archive.clone();
    let staging_path = staging.path().to_owned();
    tauri::async_runtime::spawn_blocking(move || extract(&archive_copy, &staging_path))
        .await
        .map_err(|_| "推論エンジンを展開できませんでした")??;
    if downloads.cancel.canceled.load(Ordering::SeqCst) {
        return Err("ダウンロードを中止しました".into());
    }
    // 検証済みの新しいディレクトリを採用し、途中の展開結果は参照させない。
    if !staging.path().join("llama-server.exe").is_file() {
        return Err("推論エンジンの実行ファイルが見つかりません".into());
    }
    let engine = staging.keep();
    let executable = engine.join("llama-server.exe");
    if !executable.is_file() {
        return Err("推論エンジンの実行ファイルが見つかりません".into());
    }
    let mut marker =
        tempfile::NamedTempFile::new_in(&home).map_err(|_| "モデルの設定を保存できませんでした")?;
    serde_json::to_writer(&mut marker, &json!({"engine":executable,"model":model}))
        .map_err(|_| "モデルの設定を保存できませんでした")?;
    marker
        .persist(home.join("installation.json"))
        .map_err(|_| "モデルの設定を保存できませんでした")?;
    report(&progress, "complete", "model", MODEL_BYTES, MODEL_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    const HELLO: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    async fn server(body: &'static [u8]) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = vec![0; 4096];
            let _ = socket.read(&mut bytes).await.unwrap();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            socket.write_all(body).await.unwrap();
        });
        (url, task)
    }

    #[tokio::test]
    async fn verified_download_is_atomic_and_cached() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("model");
        let (url, task) = server(b"hello").await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let events = Arc::new(Mutex::new(Vec::<Value>::new()));
        let saved = events.clone();
        let channel = Channel::new(move |body| {
            let value = match body {
                tauri::ipc::InvokeResponseBody::Json(body) => serde_json::from_str(&body).unwrap(),
                tauri::ipc::InvokeResponseBody::Raw(body) => serde_json::from_slice(&body).unwrap(),
            };
            saved.lock().unwrap().push(value);
            Ok(())
        });
        let operation = Operation::default();
        // テスト用サーバーのURLも、呼び出し中は固定して使う。
        fetch(
            &client, &operation, &channel, "model", &url, &path, HELLO, 5,
        )
        .await
        .unwrap();
        task.await.unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello");
        assert_eq!(events.lock().unwrap().last().unwrap()["received"], 5);
        assert!(events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event["phase"] == "verify"));
        fetch(
            &client,
            &operation,
            &channel,
            "model",
            "http://127.0.0.1:1/not-used",
            &path,
            HELLO,
            5,
        )
        .await
        .unwrap();
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }
    #[tokio::test]
    async fn bad_hash_and_cancel_never_replace_existing_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("model");
        fs::write(&path, b"old").unwrap();
        let (url, task) = server(b"wrong").await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let channel = Channel::new(|_| Ok(()));
        let operation = Operation::default();
        assert!(
            fetch(&client, &operation, &channel, "model", &url, &path, HELLO, 5)
                .await
                .is_err()
        );
        task.await.unwrap();
        operation.cancel();
        assert!(fetch(
            &client,
            &operation,
            &channel,
            "model",
            "http://127.0.0.1:1/not-used",
            &path,
            HELLO,
            5
        )
        .await
        .is_err());
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }
    #[test]
    fn deflated_engine_can_be_extracted() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("engine.zip");
        let mut writer = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
        writer
            .start_file(
                "llama-server.exe",
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated),
            )
            .unwrap();
        writer.write_all(b"mock engine").unwrap();
        writer.finish().unwrap();
        let destination = temp.path().join("out");
        fs::create_dir(&destination).unwrap();
        extract(&archive, &destination).unwrap();
        assert_eq!(
            fs::read(destination.join("llama-server.exe")).unwrap(),
            b"mock engine"
        );
    }
    #[tokio::test]
    async fn cancel_in_progress_cleans_partial_download() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("model");
        fs::write(&path, b"old").unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/model", listener.local_addr().unwrap());
        let release = Arc::new(tokio::sync::Notify::new());
        let server_release = release.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 4096];
            let _ = socket.read(&mut buffer).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(150)).await;
            socket.write_all(b"h").await.unwrap();
            server_release.notified().await;
        });
        let operation = Arc::new(Operation::default());
        let cancel = operation.clone();
        let channel = Channel::new(move |body| {
            if let tauri::ipc::InvokeResponseBody::Json(body) = body {
                let event: Value = serde_json::from_str(&body).unwrap();
                if event["phase"] == "download" && event["received"].as_u64().unwrap_or(0) > 0 {
                    cancel.cancel();
                }
            }
            Ok(())
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            fetch(
                &client, &operation, &channel, "model", &url, &path, HELLO, 100,
            ),
        )
        .await
        .unwrap();
        assert!(result.unwrap_err().contains("中止"));
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
        release.notify_one();
        server.await.unwrap();
    }
    #[test]
    fn archive_cannot_escape_destination_or_use_windows_special_names() {
        for name in ["../outside", "C:/outside", "dir\\outside", "file:stream"] {
            let temp = tempfile::tempdir().unwrap();
            let archive = temp.path().join("engine.zip");
            let mut writer = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"test").unwrap();
            writer.finish().unwrap();
            let destination = temp.path().join("out");
            fs::create_dir(&destination).unwrap();
            assert!(extract(&archive, &destination).is_err(), "{name}");
            assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
        }
    }
}
