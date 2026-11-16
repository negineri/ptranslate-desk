#!/usr/bin/env python3
"""HEADの配布タグから、Linux向けワーカーCLI (ptranslate-worker) のtar.gzとSHA-256を作る。"""

import argparse
import gzip
import hashlib
import io
import os
from pathlib import Path
import re
import subprocess
import tarfile

from check_release_version import release_version, version_from_tag

ROOT = Path(__file__).resolve().parent.parent
BINARY = "ptranslate-worker"


def filename(version):
    version_from_tag("v" + version)
    safe = re.sub(r"[^A-Za-z0-9_.-]", ".", version)
    return f"PTranslate.Worker_{safe}_linux-x64.tar.gz"


def archive_bytes(binary):
    """中身と時刻を固定し、同じバイナリから同じtar.gzを作る。"""
    data = binary.read_bytes()
    if not data.startswith(b"\x7fELF"):
        raise ValueError("ワーカーCLIのLinux実行ファイルではありません")
    buffer = io.BytesIO()
    with gzip.GzipFile(fileobj=buffer, mode="wb", mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as archive:
            entry = tarfile.TarInfo(BINARY)
            entry.size = len(data)
            entry.mode = 0o755
            entry.mtime = 0
            entry.uname = entry.gname = ""
            archive.addfile(entry, io.BytesIO(data))
    return buffer.getvalue()


def package(binary, output, version):
    output.mkdir(parents=True, exist_ok=True)
    name = filename(version)
    if any(
        p.is_symlink() or p.name not in {name, name + ".sha256"} or not p.is_file()
        for p in output.iterdir()
    ):
        raise ValueError("配布先に別のファイルがあります。空の出力先を指定してください")
    archive = output / name
    archive.write_bytes(archive_bytes(binary))
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    (output / (name + ".sha256")).write_text(f"{digest}  {name}\n", encoding="utf-8")
    verify(output, version)
    return archive


def verify(directory, version):
    name = filename(version)
    if {p.name for p in directory.iterdir()} != {name, name + ".sha256"}:
        raise ValueError("ワーカーCLIの配布物はtar.gzとSHA-256の2ファイルだけにしてください")
    if any(p.is_symlink() or not p.is_file() for p in directory.iterdir()):
        raise ValueError("ワーカーCLIの配布物は通常のファイルにしてください")
    archive = directory / name
    expected = (directory / (name + ".sha256")).read_text(encoding="utf-8")
    if expected != f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {name}\n":
        raise ValueError("ワーカーCLIのtar.gzのSHA-256が一致しません")
    with tarfile.open(archive, "r:gz") as opened:
        members = opened.getmembers()
        if (
            len(members) != 1
            or members[0].name != BINARY
            or not members[0].isfile()
            or members[0].mode != 0o755
        ):
            raise ValueError("ワーカーCLIのtar.gzには実行ファイルを1つだけ入れてください")
        if not opened.extractfile(members[0]).read(4) == b"\x7fELF":
            raise ValueError("ワーカーCLIのLinux実行ファイルではありません")


def build_release(root, output, tag=""):
    version = release_version(root, tag)
    filename(version)
    subprocess.run(["node", "desktop/build.mjs"], cwd=root, check=True)
    manifest = root / "desktop/src-tauri/Cargo.toml"
    subprocess.run(
        ["cargo", "build", "--release", "--locked", "--manifest-path", str(manifest)]
        + ["-p", BINARY],
        cwd=root,
        check=True,
        env={**os.environ, "PTRANSLATE_VERSION": version},
    )
    binary = root / "desktop/src-tauri/target/release" / BINARY
    reported = subprocess.run(
        [str(binary), "--version"], check=True, capture_output=True, text=True
    ).stdout.strip()
    if reported != f"{BINARY} {version}":
        raise ValueError("ワーカーCLIのバージョンが配布タグと一致しません")
    return package(binary, output, version)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", default="", help="HEADに付いたv*タグ。省略時はHEADから取得")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "dist/worker-cli")
    parser.add_argument("--verify", type=Path, help="配布物のあるディレクトリを検査する")
    args = parser.parse_args()
    try:
        if args.verify:
            verify(args.verify, release_version(ROOT, args.tag))
            print("ワーカーCLIの配布物を確認しました")
        else:
            print(build_release(ROOT, args.output_dir, args.tag))
    except (ValueError, OSError, subprocess.CalledProcessError, tarfile.TarError) as error:
        raise SystemExit(f"ワーカーCLIの配布物を作れません: {error}") from None


if __name__ == "__main__":
    main()
