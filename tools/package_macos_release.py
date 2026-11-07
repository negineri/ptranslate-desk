#!/usr/bin/env python3
"""ビルドコードを実行せず、macOSとWindowsの配布ファイルを作成する。"""

import argparse
import base64
import hashlib
from datetime import datetime, timezone
import json
import os
from pathlib import Path, PurePosixPath
import plistlib
import re
import shutil
import stat
import struct
import subprocess
import tarfile
import tempfile
from urllib.parse import quote
import zipfile

from check_release_version import check_manifest, version_from_tag
from macos_signing import verify, PUBLIC

ROOT = Path(__file__).resolve().parent.parent
APP = "PTranslate Desk.app"


def run(*args, env=None):
    # 署名ツールの失敗時にも秘密やその変換値をログへ出さない。
    result = subprocess.run(args, env=env, capture_output=True, timeout=300)
    if result.returncode:
        raise ValueError(f"{Path(args[0]).name} の実行に失敗しました")


def validate_archive(archive):
    with zipfile.ZipFile(archive) as bundle:
        seen = set()
        total = 0
        for entry in bundle.infolist():
            path = PurePosixPath(entry.filename)
            mode = entry.external_attr >> 16
            if (
                path.is_absolute()
                or ".." in path.parts
                or "\\" in entry.filename
                or not path.parts
                or path.parts[0] not in (APP, "__MACOSX")
                or any(ord(c) < 32 for c in entry.filename)
                or stat.S_ISLNK(mode)
                or stat.S_IFMT(mode) not in (0, stat.S_IFREG, stat.S_IFDIR)
                or path.as_posix().casefold() in seen
            ):
                raise ValueError("配布zipに許可していないパス・リンク・特殊ファイルがあります")
            seen.add(path.as_posix().casefold())
            total += entry.file_size
        if total > 2 * 1024**3 or len(seen) > 10000:
            raise ValueError("配布zipの展開サイズ・ファイル数が上限を超えています")
        if f"{APP}/Contents/MacOS/ptranslate-desk".casefold() not in seen:
            raise ValueError("配布zipにアプリの実行ファイルがありません")


def prepare(directory, archive, version):
    validate_archive(archive)
    directory.mkdir(mode=0o700, parents=True, exist_ok=False)
    run("/usr/bin/ditto", "-x", "-k", str(archive), str(directory))
    app = directory / APP
    for path in app.rglob("*"):
        if path.is_symlink() or not (path.is_file() or path.is_dir()):
            raise ValueError("展開したアプリにリンク・特殊ファイルがあります")
    with (app / "Contents/Info.plist").open("rb") as stream:
        info = plistlib.load(stream)
    if (
        info.get("CFBundleIdentifier") != "com.witenp.ptranslate-desk"
        or info.get("CFBundleShortVersionString") != version
        or info.get("CFBundleExecutable") != "ptranslate-desk"
    ):
        raise ValueError("アプリのID・バージョン・実行ファイルが一致しません")
    run(
        "/usr/bin/lipo",
        str(app / "Contents/MacOS/ptranslate-desk"),
        "-verify_arch",
        "arm64",
        "x86_64",
    )


def artifact_stem(version):
    # GitHubがアップロード時に行う名前の置換と揃え、manifestと実名のずれを防ぐ。
    safe_version = re.sub(r"[^A-Za-z0-9_.-]", ".", version)
    return f"PTranslate.Desk_{safe_version}_universal"


def package(directory, version):
    app = directory / APP
    verify(app, PUBLIC / "certificate.pem")
    output = directory / "output"
    output.mkdir(exist_ok=False)
    stem = artifact_stem(version)
    # Tauri updaterと同じく、アーカイブの最上位に .app を置く。
    with tarfile.open(output / f"{stem}.app.tar.gz", "w:gz") as archive:
        archive.add(app, arcname=APP)
    with_applications = directory / "dmg"
    with_applications.mkdir()
    shutil.copytree(app, with_applications / APP)
    (with_applications / "Applications").symlink_to("/Applications")
    run(
        "/usr/bin/hdiutil",
        "create",
        "-volname",
        "PTranslate Desk",
        "-srcfolder",
        str(with_applications),
        "-format",
        "UDZO",
        str(output / f"{stem}.dmg"),
    )


def windows_filename(version):
    safe_version = re.sub(r"[^A-Za-z0-9_.-]", ".", version)
    return f"PTranslate.Desk_{safe_version}_x64-setup.exe"


def prepare_windows(directory, source, version):
    # インストーラーは署名ジョブでは実行しない。単一のGUI実行ファイルだけを受け取る。
    files = list(source.iterdir())
    if len(files) != 1:
        raise ValueError("Windows配布物はインストーラー1個にしてください")
    installer = files[0]
    if (
        installer.is_symlink()
        or not installer.is_file()
        or installer.name
        not in (f"PTranslate Desk_{version}_x64-setup.exe", windows_filename(version))
        or not 128 <= installer.stat().st_size <= 512 * 1024**2
    ):
        raise ValueError("Windowsインストーラーの名前・形式・サイズが不正です")
    with installer.open("rb") as stream:
        header = stream.read(64)
        offset = struct.unpack_from("<I", header, 60)[0]
        if header[:2] != b"MZ" or offset > installer.stat().st_size - 94:
            raise ValueError("Windows実行ファイルではありません")
        stream.seek(offset)
        pe = stream.read(94)
    if (
        pe[:4] != b"PE\0\0"
        or struct.unpack_from("<H", pe, 4)[0] not in (0x14C, 0x8664)
        or struct.unpack_from("<H", pe, 24)[0] not in (0x10B, 0x20B)
        or struct.unpack_from("<H", pe, 92)[0] != 2
    ):
        raise ValueError("Windows GUIインストーラーではありません")
    output = directory / "output"
    output.mkdir(parents=True, exist_ok=True)
    destination = output / windows_filename(version)
    if destination.exists():
        raise ValueError("Windows配布物が既にあります")
    shutil.copyfile(installer, destination)


def sign_update(directory, version, windows=False):
    if not os.environ.get("TAURI_SIGNING_PRIVATE_KEY") or not os.environ.get(
        "TAURI_SIGNING_PRIVATE_KEY_PASSWORD"
    ):
        raise ValueError("更新署名用のActions secretsを設定してください")
    filename = windows_filename(version) if windows else f"{artifact_stem(version)}.app.tar.gz"
    archive = directory / "output" / filename
    # アプリのpackage.jsonのscriptsやビルドフックを経由しない。
    run(
        "node",
        str(ROOT / "desktop/node_modules/@tauri-apps/cli/tauri.js"),
        "signer",
        "sign",
        "--app-version",
        version,
        str(archive),
        env=dict(os.environ),
    )


def verify_update_signature(archive, signature, public_key, version):
    """更新データと版を、アプリに埋め込む公開鍵で検証する。秘密鍵は扱わない。"""
    try:
        public = base64.b64decode(
            base64.b64decode(public_key, validate=True).splitlines()[1], validate=True
        )
        lines = base64.b64decode(signature.strip(), validate=True).splitlines()
        signed = base64.b64decode(lines[1], validate=True)
        trusted = base64.b64decode(lines[3], validate=True)
        if (
            len(public) != 42
            or public[:2] != b"Ed"
            or len(signed) != 74
            or signed[:2] not in (b"ED", b"Ed")
            or len(trusted) != 64
            or len(lines) != 4
            or not lines[2].startswith(b"trusted comment: ")
            or not lines[2].endswith(f"\tversion:{version}".encode())
            or public[2:10] != signed[2:10]
        ):
            raise ValueError("更新署名の鍵または版が一致しません")
        comment = lines[2][len(b"trusted comment: ") :]
        content = archive.read_bytes()
        message = hashlib.blake2b(content).digest() if signed[:2] == b"ED" else content
        with tempfile.TemporaryDirectory(prefix="ptranslate-verify-") as temp:
            directory = Path(temp)
            key = directory / "public.der"
            key.write_bytes(bytes.fromhex("302a300506032b6570032100") + public[10:])
            for data, sig in ((message, signed[10:]), (signed[10:] + comment, trusted)):
                (directory / "message").write_bytes(data)
                (directory / "signature").write_bytes(sig)
                run(
                    "openssl",
                    "pkeyutl",
                    "-verify",
                    "-pubin",
                    "-inkey",
                    str(key),
                    "-keyform",
                    "DER",
                    "-rawin",
                    "-in",
                    str(directory / "message"),
                    "-sigfile",
                    str(directory / "signature"),
                )
    except (ValueError, IndexError):
        raise ValueError("更新データ・版の署名がアプリの公開鍵と一致しません") from None


def manifest(directory, tag, require_windows=False):
    version = version_from_tag(tag)
    output = directory / "output"
    filename = f"{artifact_stem(version)}.app.tar.gz"
    signature = (output / (filename + ".sig")).read_text().strip()
    decoded = base64.b64decode(signature, validate=True).decode()
    lines = decoded.splitlines()
    if len(lines) != 4 or not lines[2].endswith(f"\tversion:{version}"):
        raise ValueError("更新署名に配布バージョンがありません")
    config = json.loads((ROOT / "desktop/src-tauri/tauri.conf.json").read_text())
    verify_update_signature(
        output / filename, signature, config["plugins"]["updater"]["pubkey"], version
    )
    entry = {
        "signature": signature,
        "url": f"https://github.com/negineri/ptranslate-desk/releases/download/{quote(tag, safe='')}/{quote(filename, safe='')}",
    }
    value = {
        "version": version,
        "notes": "PTranslate Desk の更新",
        "pub_date": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "platforms": {name: entry for name in ("darwin-aarch64", "darwin-x86_64")},
    }
    expected = {
        filename,
        filename + ".sig",
        "latest.json",
        f"{artifact_stem(version)}.dmg",
    }
    if require_windows:
        windows = windows_filename(version)
        windows_signature = (output / (windows + ".sig")).read_text(encoding="utf-8").strip()
        verify_update_signature(
            output / windows, windows_signature, config["plugins"]["updater"]["pubkey"], version
        )
        value["platforms"]["windows-x86_64"] = {
            "signature": windows_signature,
            "url": f"https://github.com/negineri/ptranslate-desk/releases/download/{quote(tag, safe='')}/{quote(windows, safe='')}",
        }
        expected.update((windows, windows + ".sig"))
    check_manifest(value, version, require_windows=require_windows)
    (output / "latest.json").write_text(
        json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    if {p.name for p in output.iterdir()} != expected:
        raise ValueError("公開するファイルの一覧が一致しません")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "action",
        choices=[
            "prepare",
            "prepare-windows",
            "package",
            "sign-update",
            "sign-windows",
            "manifest",
        ],
    )
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--archive", type=Path)
    parser.add_argument("--require-windows", action="store_true")
    args = parser.parse_args()
    try:
        version = version_from_tag(args.tag)
        if args.action in ("prepare", "prepare-windows"):
            if not args.archive:
                parser.error("--archive が必要です")
            if args.action == "prepare-windows":
                prepare_windows(args.directory, args.archive, version)
            else:
                prepare(args.directory, args.archive, version)
        elif args.action == "package":
            package(args.directory, version)
        elif args.action in ("sign-update", "sign-windows"):
            sign_update(args.directory, version, windows=args.action == "sign-windows")
        else:
            manifest(args.directory, args.tag, require_windows=args.require_windows)
    except (ValueError, OSError, subprocess.TimeoutExpired, zipfile.BadZipFile):
        parser.exit(1, "配布処理に失敗しました（秘密の漏出を避けるため詳細は表示しません）\n")


if __name__ == "__main__":
    main()
