#!/usr/bin/env python3
"""開発用の既定値と、gitタグから取得した配布バージョンを検証する。"""

import argparse
import base64
import json
import re
import subprocess
from pathlib import Path
import tomllib
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parent.parent


def check_version(root: Path, tag: str = "") -> str:
    desktop = root / "desktop"
    version = json.loads((desktop / "src-tauri/tauri.conf.json").read_text())["version"]
    package = json.loads((desktop / "package.json").read_text())
    npm_lock = json.loads((desktop / "package-lock.json").read_text())
    cargo = tomllib.loads((desktop / "src-tauri/Cargo.toml").read_text())
    cargo_lock = tomllib.loads((desktop / "src-tauri/Cargo.lock").read_text())
    versions = {
        "package.json": package["version"],
        "package-lock.json": npm_lock["version"],
        "package-lock.json のルート": npm_lock["packages"][""]["version"],
        "Cargo.toml": cargo["package"]["version"],
        "Cargo.lock": next(
            item["version"]
            for item in cargo_lock["package"]
            if item["name"] == cargo["package"]["name"]
        ),
    }
    for name, actual in versions.items():
        if actual != version:
            raise ValueError(f"{name} の version が tauri.conf.json の {version} と一致しません")
    if version != "0.0.0-dev":
        raise ValueError("既定のバージョンは開発用の 0.0.0-dev にしてください")
    return release_version(root, tag) if tag else version


def release_version(root: Path, tag: str = "") -> str:
    def git(*args):
        try:
            return subprocess.check_output(
                ["git", *args], cwd=root, text=True, stderr=subprocess.PIPE
            ).strip()
        except subprocess.CalledProcessError as error:
            raise ValueError("配布にはHEADに付いた v* のgitタグが必要です") from error

    tag = tag or git("describe", "--tags", "--exact-match", "--match", "v[0-9]*", "HEAD")
    version = version_from_tag(tag)
    if git("rev-parse", f"refs/tags/{tag}^{{commit}}") != git("rev-parse", "HEAD"):
        raise ValueError("配布タグがHEADを指していません")
    if git("status", "--porcelain", "--untracked-files=no"):
        raise ValueError("配布する前に追跡ファイルの変更をコミットしてください")
    return version


def version_from_tag(tag: str) -> str:
    number = r"(?:0|[1-9][0-9]*)"
    identifier = r"(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
    if not re.fullmatch(
        rf"v{number}\.{number}\.{number}(?:-{identifier}(?:\.{identifier})*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?",
        tag,
    ):
        raise ValueError("タグは v1.2.3 や v1.2.3-beta.1 の形式にしてください")
    return tag[1:]


def write_release_config(root: Path, output: Path, version: str) -> None:
    config = json.loads((root / "desktop/src-tauri/tauri.release.conf.json").read_text())
    config["version"] = version
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(config, ensure_ascii=False, indent=2) + "\n")


def check_manifest(manifest: dict, version: str) -> None:
    if manifest.get("version") != version:
        raise ValueError("latest.json の version がアプリと一致しません")
    platforms = manifest.get("platforms", {})
    entries = [platforms.get(name) for name in ("darwin-aarch64", "darwin-x86_64")]
    if not all(isinstance(entry, dict) for entry in entries):
        raise ValueError("latest.json に Apple Silicon と Intel の両方が必要です")
    for entry in entries:
        if urlsplit(entry.get("url", "")).scheme != "https":
            raise ValueError("更新の URL は HTTPS が必要です")
        signature = entry.get("signature", "").strip()
        if not signature:
            raise ValueError("更新の署名がありません")
        base64.b64decode(signature, validate=True)
    if entries[0]["url"] != entries[1]["url"] or entries[0]["signature"] != entries[1]["signature"]:
        raise ValueError("両アーキテクチャが同じ universal 更新ファイルを指していません")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", default="")
    parser.add_argument("--manifest", type=Path)
    parser.add_argument(
        "--write-config", type=Path, help="タグのバージョンを入れたTauri配布設定を生成"
    )
    args = parser.parse_args()
    try:
        version = check_version(ROOT, args.tag)
        if args.write_config:
            version = release_version(ROOT, args.tag)
            write_release_config(ROOT, args.write_config, version)
        if args.manifest:
            check_manifest(json.loads(args.manifest.read_text()), version)
    except (ValueError, KeyError, StopIteration) as error:
        parser.exit(1, f"バージョン確認に失敗しました: {error}\n")
    print(f"ビルドバージョン: {version}")
