#!/usr/bin/env python3
"""配布するアプリのバージョンと v* タグが一致することを確かめる。"""

import argparse
import base64
import json
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
    if tag and tag != f"v{version}":
        raise ValueError(f"タグ {tag!r} と tauri.conf.json の version {version} が一致しません")
    return version


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
    args = parser.parse_args()
    try:
        version = check_version(ROOT, args.tag)
        if args.manifest:
            check_manifest(json.loads(args.manifest.read_text()), version)
    except (ValueError, KeyError, StopIteration) as error:
        parser.exit(1, f"バージョン確認に失敗しました: {error}\n")
    print(f"配布バージョンの一致: {version}")
