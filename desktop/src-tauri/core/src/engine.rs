//! 推論エンジン (llama.cpp) と承認済みモデルの固定の配布元・検証・展開・起動引数。
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

/// 1件あたりのコンテキスト。サーバーの上限 (プロンプト12000文字、訳2314トークン) で、
/// 1文字1トークン以下の文が切れずに収まる量 (docs/LOCAL_TRANSLATION.md)。
pub const CONTEXT_PER_SLOT: usize = 16384;
/// 承認済みモデルのファイル名。
pub const MODEL_FILE: &str = "plamo-3-translate-2b-Q4_K_M.gguf";
/// 対応する llama.cpp のリリースとコミット。
pub const ENGINE_TAG: &str = "b11804";
pub const ENGINE_COMMIT: &str = "c7fc5094179bf8a9c73c18593db714f8da41a402";

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const ENGINE_URL: &str = "https://github.com/ggml-org/llama.cpp/releases/download/b11804/llama-b11804-bin-macos-arm64.tar.gz";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const ENGINE_SHA: &str = "f54ecd2d9a552a1b4b55ba6ecad51694ce8e737e61061bbbe28fc71507777985";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const ENGINE_BYTES: u64 = 16_145_423;
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub const ENGINE_URL: &str = "https://github.com/ggml-org/llama.cpp/releases/download/b11804/llama-b11804-bin-macos-x64.tar.gz";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub const ENGINE_SHA: &str = "e5baad4fd328f22fd19b291ba0841515923f97aa20228af5b49152ec27116703";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub const ENGINE_BYTES: u64 = 11_882_142;
#[cfg(target_os = "linux")]
pub const ENGINE_URL: &str = "https://github.com/ggml-org/llama.cpp/releases/download/b11804/llama-b11804-bin-ubuntu-x64.tar.gz";
#[cfg(target_os = "linux")]
pub const ENGINE_SHA: &str = "bb800fbd5e905fb8b9887c9a6cb0ab4f14c7267c5f99302874521e7558b0613f";
#[cfg(target_os = "linux")]
pub const ENGINE_BYTES: u64 = 22_331_121;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub const ENGINE_URL: &str = "https://github.com/ggml-org/llama.cpp/releases/download/b11804/llama-b11804-bin-win-vulkan-x64.zip";
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub const ENGINE_SHA: &str = "db9ab31997e93bf713d3dfec6eb1cf1b5a6a5f2940a3d4d111ad0ffab54c87d5";
pub const MODEL_URL: &str = "https://huggingface.co/pfnet/plamo-3-translate-2b-GGUF/resolve/d44589ecd5287cd64452e9cd1e2cfdad904fe216/plamo-3-translate-2b-Q4_K_M.gguf";
pub const MODEL_SHA: &str = "2189a8a0b4b359f59443ec36cfb9c9c3f603a678c00dbcee8598b3f7ea7c12fb";
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub const ENGINE_BYTES: u64 = 36_865_203;
pub const MODEL_BYTES: u64 = 1_807_202_144;

pub fn verify(
    path: PathBuf,
    expected: &'static str,
    cancel: Arc<AtomicBool>,
) -> Result<bool, String> {
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
#[cfg(any(not(any(target_os = "macos", target_os = "linux")), test))]
pub fn extract_zip(archive: &std::path::Path, destination: &std::path::Path) -> Result<(), String> {
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
pub fn extract(archive: &std::path::Path, destination: &std::path::Path) -> Result<(), String> {
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return extract_zip(archive, destination);
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let file = fs::File::open(archive).map_err(|_| "推論エンジンを読み込めませんでした")?;
        extract_tar(flate2::read::GzDecoder::new(file), destination)
    }
}
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn extract_tar(reader: impl Read, destination: &std::path::Path) -> Result<(), String> {
    let mut archive = tar::Archive::new(reader);
    let mut size = 0u64;
    for entry in archive
        .entries()
        .map_err(|_| "推論エンジンのtarが不正です")?
    {
        let mut entry = entry.map_err(|_| "tarの項目を読み込めませんでした")?;
        let kind = entry.header().entry_type();
        // 配布物のdylibリンクは、展開先の内側を指すものだけ許可する。
        if !(kind.is_file() || kind.is_dir() || kind.is_symlink()) {
            return Err("tarに不正な項目があります".into());
        }
        let path = entry
            .path()
            .map_err(|_| "tarの保存先が不正です")?
            .into_owned();
        if path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err("tarの保存先が不正です".into());
        }
        if kind.is_symlink() {
            let target = entry
                .link_name()
                .map_err(|_| "リンクが不正です")?
                .ok_or("リンクが不正です")?;
            if target.is_absolute()
                || target
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err("リンクの保存先が不正です".into());
            }
        }
        size = size.checked_add(entry.size()).ok_or("tarが大きすぎます")?;
        if size > 512 * 1024 * 1024
            || !entry
                .unpack_in(destination)
                .map_err(|_| "推論エンジンを展開できませんでした")?
        {
            return Err("推論エンジンを展開できませんでした".into());
        }
    }
    Ok(())
}
pub fn executable(root: &std::path::Path) -> Result<PathBuf, String> {
    let name = if cfg!(target_os = "windows") {
        "llama-server.exe"
    } else {
        "llama-server"
    };
    let mut found = Vec::new();
    fn visit(dir: &std::path::Path, name: &str, found: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                visit(&entry.path(), name, found)?;
            } else if entry.file_type()?.is_file() && entry.file_name() == name {
                found.push(entry.path());
            }
        }
        Ok(())
    }
    visit(root, name, &mut found).map_err(|_| "推論エンジンを確認できませんでした")?;
    if found.len() != 1 {
        return Err("推論エンジンの実行ファイルが見つかりません".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&found[0], fs::Permissions::from_mode(0o755))
            .map_err(|_| "実行権限を設定できませんでした")?;
    }
    Ok(found.remove(0))
}
/// llama-server の共通の起動引数。認証キーは環境変数 LLAMA_API_KEY で渡す。
pub fn server_args(model: &Path, port: u16, parallel: usize) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec!["--model".into(), model.into()];
    for arg in [
        "--host".to_string(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
        "--ctx-size".into(),
        (CONTEXT_PER_SLOT * parallel).to_string(),
        "--parallel".into(),
        parallel.to_string(),
        "--flash-attn".into(),
        "on".into(),
        "--no-webui".into(),
        "--log-disable".into(),
    ] {
        args.push(arg.into());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_archive_preserves_libraries_and_executable() {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "llama-b11804/llama-server", &b"mock"[..])
            .unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_size(3);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "llama-b11804/libggml.0.dylib", &b"lib"[..])
            .unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        builder
            .append_link(&mut header, "llama-b11804/libggml.dylib", "libggml.0.dylib")
            .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("engine.tar.gz");
        let mut gzip = flate2::write::GzEncoder::new(
            fs::File::create(&archive).unwrap(),
            flate2::Compression::default(),
        );
        gzip.write_all(&builder.into_inner().unwrap()).unwrap();
        gzip.finish().unwrap();
        extract(&archive, temp.path()).unwrap();
        let engine = executable(temp.path()).unwrap();
        assert_eq!(
            fs::read(engine.with_file_name("libggml.dylib")).unwrap(),
            b"lib"
        );
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            fs::metadata(engine).unwrap().permissions().mode() & 0o111,
            0
        );
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn tar_archive_rejects_escaping_links() {
        for target in ["../outside", "/tmp/outside"] {
            let mut builder = tar::Builder::new(Vec::new());
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            builder.append_link(&mut header, "escape", target).unwrap();
            let temp = tempfile::tempdir().unwrap();
            assert!(extract_tar(&builder.into_inner().unwrap()[..], temp.path()).is_err());
        }
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
        extract_zip(&archive, &destination).unwrap();
        assert_eq!(
            fs::read(destination.join("llama-server.exe")).unwrap(),
            b"mock engine"
        );
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
            assert!(extract_zip(&archive, &destination).is_err(), "{name}");
            assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
        }
    }
}
