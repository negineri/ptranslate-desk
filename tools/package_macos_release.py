#!/usr/bin/env python3
"""ビルドコードを実行せず、macOSの配布ファイルを作成する。"""

import argparse
import base64
from datetime import datetime, timezone
import json
import os
from pathlib import Path, PurePosixPath
import plistlib
import shutil
import stat
import subprocess
import tarfile
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


def package(directory, version):
    app = directory / APP
    verify(app, PUBLIC / "certificate.pem")
    output = directory / "output"
    output.mkdir(exist_ok=False)
    stem = f"PTranslate Desk_{version}_universal"
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


def sign_update(directory, version):
    if not os.environ.get("TAURI_SIGNING_PRIVATE_KEY") or not os.environ.get(
        "TAURI_SIGNING_PRIVATE_KEY_PASSWORD"
    ):
        raise ValueError("更新署名用のActions secretsを設定してください")
    archive = directory / "output" / f"PTranslate Desk_{version}_universal.app.tar.gz"
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


def manifest(directory, tag):
    version = version_from_tag(tag)
    output = directory / "output"
    filename = f"PTranslate Desk_{version}_universal.app.tar.gz"
    signature = (output / (filename + ".sig")).read_text().strip()
    decoded = base64.b64decode(signature, validate=True).decode()
    lines = decoded.splitlines()
    if len(lines) != 4 or not lines[2].endswith(f"\tversion:{version}"):
        raise ValueError("更新署名に配布バージョンがありません")
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
    check_manifest(value, version)
    (output / "latest.json").write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")
    expected = {
        filename,
        filename + ".sig",
        "latest.json",
        f"PTranslate Desk_{version}_universal.dmg",
    }
    if {p.name for p in output.iterdir()} != expected:
        raise ValueError("公開するファイルの一覧が一致しません")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["prepare", "package", "sign-update", "manifest"])
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--archive", type=Path)
    args = parser.parse_args()
    try:
        version = version_from_tag(args.tag)
        if args.action == "prepare":
            if not args.archive:
                parser.error("--archive が必要です")
            prepare(args.directory, args.archive, version)
        elif args.action == "package":
            package(args.directory, version)
        elif args.action == "sign-update":
            sign_update(args.directory, version)
        else:
            manifest(args.directory, args.tag)
    except (ValueError, OSError, subprocess.TimeoutExpired, zipfile.BadZipFile):
        parser.exit(1, "配布処理に失敗しました（秘密の漏出を避けるため詳細は表示しません）\n")


if __name__ == "__main__":
    main()
