#!/usr/bin/env python3
"""HEADの配布タグから、手動導入用のChrome拡張ZIPとSHA-256を作る。"""

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import zipfile

from check_release_version import release_version, version_from_tag

ROOT = Path(__file__).resolve().parent.parent


def chrome_version(version):
    # version_nameにSemVer全体を残し、versionはChromeが受け付ける数値にする。
    version_from_tag("v" + version)
    core = re.split(r"[-+]", version, maxsplit=1)[0]
    numbers = [int(n) for n in core.split(".")]
    if not any(numbers) or any(n > 65535 for n in numbers):
        raise ValueError("Chrome拡張のバージョンは各数値が0〜65535で、すべて0にはできません")
    return core


def filename(version):
    chrome_version(version)
    safe = re.sub(r"[^A-Za-z0-9_.-]", ".", version)
    return f"PTranslate.Chrome_{safe}.zip"


def validate_files(files, version):
    manifest = json.loads(files["manifest.json"])
    if manifest.get("manifest_version") != 3:
        raise ValueError("Manifest V3の拡張が必要です")
    if (manifest.get("version"), manifest.get("version_name")) != (
        chrome_version(version),
        version,
    ):
        raise ValueError("拡張のバージョンが配布タグと一致しません")
    required = {
        manifest["background"]["service_worker"],
        manifest["action"]["default_popup"],
        manifest["options_ui"]["page"],
        *manifest["icons"].values(),
        *manifest["action"]["default_icon"].values(),
    }
    for resource in manifest.get("web_accessible_resources", []):
        required.update(resource["resources"])
    for name, content in files.items():
        if name.endswith(".html"):
            for resource in re.findall(
                r'<(?:script|img)\b[^>]*\bsrc=[\'"]([^\'"]+)[\'"]', content.decode()
            ):
                required.add((Path(name).parent / resource).as_posix())
    if not required.issubset(files):
        raise ValueError("拡張が参照するファイルがZIPにありません")
    # モジュールの書き忘れも梱包時に検知する。
    for name, content in files.items():
        if name.endswith((".js", ".mjs")):
            for module in re.findall(r'from\s+[\'"](\./[^\'"]+)[\'"]', content.decode()):
                target = (Path(name).parent / module).as_posix()
                if target not in files:
                    raise ValueError(f"拡張のモジュールがありません: {target}")


def package(dist, output, version):
    numeric = chrome_version(version)
    dist, output = dist.resolve(), output.resolve()
    if output == dist or dist in output.parents:
        raise ValueError("ZIPの保存先は拡張のdistの外にしてください")
    files = {}
    for path in sorted(dist.rglob("*")):
        if path.is_symlink():
            raise ValueError("拡張の配布物にシンボリックリンクは入れられません")
        if path.is_file():
            files[path.relative_to(dist).as_posix()] = path.read_bytes()
    manifest = json.loads(files["manifest.json"])
    manifest.update(version=numeric, version_name=version)
    files["manifest.json"] = (json.dumps(manifest, ensure_ascii=False, indent=2) + "\n").encode()
    validate_files(files, version)
    output.mkdir(parents=True, exist_ok=True)
    expected = {filename(version), filename(version) + ".sha256"}
    if any(p.is_symlink() or p.name not in expected or not p.is_file() for p in output.iterdir()):
        raise ValueError("配布先に別のファイルがあります。空の出力先を指定してください")
    archive = output / filename(version)
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as zipped:
        for name, data in sorted(files.items()):
            entry = zipfile.ZipInfo(name, (1980, 1, 1, 0, 0, 0))
            entry.compress_type = zipfile.ZIP_DEFLATED
            entry.external_attr = 0o100644 << 16
            zipped.writestr(entry, data)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    (output / (archive.name + ".sha256")).write_text(
        f"{digest}  {archive.name}\n", encoding="utf-8"
    )
    verify(output, version)
    return archive


def verify(directory, version):
    archive = directory / filename(version)
    if {p.name for p in directory.iterdir()} != {archive.name, archive.name + ".sha256"}:
        raise ValueError("Chrome拡張の配布物はZIPとSHA-256の2ファイルだけにしてください")
    if any(p.is_symlink() or not p.is_file() for p in directory.iterdir()):
        raise ValueError("Chrome拡張の配布物は通常のファイルにしてください")
    expected = (directory / (archive.name + ".sha256")).read_text(encoding="utf-8")
    actual = f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n"
    if expected != actual:
        raise ValueError("Chrome拡張ZIPのSHA-256が一致しません")
    with zipfile.ZipFile(archive) as zipped:
        names = zipped.namelist()
        if len(names) != len(set(names)) or any(
            name.startswith("/") or "\\" in name or ".." in name.split("/") for name in names
        ):
            raise ValueError("Chrome拡張ZIPのパスが不正です")
        validate_files({name: zipped.read(name) for name in names}, version)


def build_release(root, output, tag=""):
    version = release_version(root, tag)
    chrome_version(version)
    subprocess.run(["node", "chrome-extension/build.mjs"], cwd=root, check=True)
    return package(root / "chrome-extension/dist", output, version)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", default="", help="HEADに付いたv*タグ。省略時はHEADから取得")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "dist/chrome-extension")
    parser.add_argument("--verify", type=Path, help="配布物のあるディレクトリを検査する")
    args = parser.parse_args()
    try:
        if args.verify:
            version = release_version(ROOT, args.tag)
            verify(args.verify, version)
            print("Chrome拡張の配布物を確認しました")
        else:
            print(build_release(ROOT, args.output_dir, args.tag))
    except (
        ValueError,
        KeyError,
        OSError,
        zipfile.BadZipFile,
        subprocess.CalledProcessError,
    ) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
