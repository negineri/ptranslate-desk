#!/usr/bin/env python3
"""Windows向けローカル翻訳の試作に必要な推論エンジンとモデルを取得する。"""

import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import tempfile
import urllib.request
import zipfile

ENGINE_URL = "https://github.com/ggml-org/llama.cpp/releases/download/b11804/llama-b11804-bin-win-vulkan-x64.zip"
ENGINE_SHA = "db9ab31997e93bf713d3dfec6eb1cf1b5a6a5f2940a3d4d111ad0ffab54c87d5"
MODEL_URL = "https://huggingface.co/pfnet/plamo-3-translate-2b-GGUF/resolve/d44589ecd5287cd64452e9cd1e2cfdad904fe216/plamo-3-translate-2b-Q4_K_M.gguf"
MODEL_SHA = "2189a8a0b4b359f59443ec36cfb9c9c3f603a678c00dbcee8598b3f7ea7c12fb"


def download(url, destination, expected):
    if destination.exists():
        with destination.open("rb") as stream:
            if hashlib.file_digest(stream, "sha256").hexdigest() == expected:
                print(f"確認済み: {destination.name}", flush=True)
                return
        raise ValueError(f"既存ファイルのSHA-256が一致しません: {destination.name}")
    digest = hashlib.sha256()
    size = 0
    with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as output:
        partial = Path(output.name)
        try:
            with urllib.request.urlopen(url, timeout=120) as response:
                while chunk := response.read(1024**2):
                    output.write(chunk)
                    digest.update(chunk)
                    size += len(chunk)
                    if size % (64 * 1024**2) == 0:
                        print(f"{destination.name}: {size // 1024**2} MiB", flush=True)
            if digest.hexdigest() != expected:
                raise ValueError("取得したファイルのSHA-256が一致しません")
        except BaseException:
            output.close()
            partial.unlink(missing_ok=True)
            raise
    partial.replace(destination)
    print(f"取得・検証済み: {destination.name}", flush=True)


def main():
    if platform.system() != "Windows" or platform.machine().lower() not in ("amd64", "x86_64"):
        raise SystemExit("この準備スクリプトはWindows x64向けです")
    home = Path(os.environ["LOCALAPPDATA"]) / "com.witenp.ptranslate-desk" / "inference"
    home.mkdir(parents=True, exist_ok=True)
    archive = home / "llama-b11804-vulkan.zip"
    model = home / "plamo-3-translate-2b-Q4_K_M.gguf"
    download(ENGINE_URL, archive, ENGINE_SHA)
    engine = home / "b11804-vulkan"
    if not engine.exists():
        with zipfile.ZipFile(archive) as bundle:
            for entry in bundle.infolist():
                path = PurePosixPath(entry.filename)
                if (
                    path.is_absolute()
                    or ".." in path.parts
                    or "\\" in entry.filename
                    or ":" in entry.filename
                ):
                    raise ValueError("推論エンジンのzipに不正なパスがあります")
            bundle.extractall(engine)
    executables = list(engine.rglob("llama-server.exe"))
    if len(executables) != 1:
        raise ValueError("推論エンジンの実行ファイルが見つかりません")
    download(MODEL_URL, model, MODEL_SHA)
    (home / "installation.json").write_text(
        json.dumps({"engine": str(executables[0]), "model": str(model)}, ensure_ascii=False),
        encoding="utf-8",
    )
    print(f"準備完了: {home}", flush=True)


if __name__ == "__main__":
    main()
