#!/usr/bin/env python3
"""PTranslate Desk、翻訳サーバー、モックをまとめて起動する。"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import plistlib
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request

from mock_plamo import make_server

ROOT = Path(__file__).resolve().parent.parent


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--smoke-test", action="store_true", help="メインウィンドウの自動試験を行う"
    )
    parser.add_argument("--app", type=Path, help="ビルド済みの macOS の .app を起動する")
    args = parser.parse_args()
    if args.app:
        if args.smoke_test:
            parser.error("--app と --smoke-test は同時に指定できません")
        contents = args.app.resolve() / "Contents"
        with (contents / "Info.plist").open("rb") as file:
            binary = contents / "MacOS" / plistlib.load(file)["CFBundleExecutable"]
    else:
        subprocess.run(["node", "desktop/build.mjs"], cwd=ROOT, check=True)
        build = ["cargo", "build", "--locked", "--manifest-path", "desktop/src-tauri/Cargo.toml"]
        if args.smoke_test:
            build += ["--features", "smoke-test"]
        subprocess.run(build, cwd=ROOT, check=True)
        binary = ROOT / "desktop/src-tauri/target/debug/ptranslate-desk"
        if sys.platform == "win32":
            binary = binary.with_suffix(".exe")

    # 使用中なら別のサーバーに接続せず、その場で終了する。
    with socket.socket() as probe:
        try:
            probe.bind(("127.0.0.1", 8002))
        except OSError:
            parser.error("127.0.0.1:8002 は使用中です。先に使用中のサーバーを止めてください")

    mock, stats = make_server(delay=0.25)
    threading.Thread(target=mock.serve_forever, daemon=True).start()
    env = {key: value for key, value in os.environ.items() if not key.startswith("TR_")}
    env.update(
        PTRANSLATE_MOCK="1",
        TR_LISTEN_HOST="127.0.0.1",
        TR_LISTEN_PORT="8002",
        TR_UPSTREAM_URL=f"http://127.0.0.1:{mock.server_port}/v1/completions",
    )
    started = time.monotonic()
    server = subprocess.Popen([sys.executable, "-I", "server/translator.py"], cwd=ROOT, env=env)
    app = None
    clipboard = None
    try:
        if args.smoke_test and sys.platform == "darwin":
            clipboard = subprocess.run(["pbpaste"], capture_output=True, check=True).stdout
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        deadline = started + 30
        while True:
            if server.poll() is not None:
                raise RuntimeError("翻訳サーバーを起動できませんでした")
            try:
                request = urllib.request.Request(
                    "http://127.0.0.1:8002/api/config", headers={"X-Forwarded-User": "mock-desktop"}
                )
                with opener.open(request, timeout=1) as response:
                    if response.status == 200:
                        print(
                            f"翻訳サーバー起動: {time.monotonic() - started:.3f} 秒 (/api/config 応答確認)",
                            flush=True,
                        )
                        break
            except (urllib.error.URLError, TimeoutError):
                if time.monotonic() >= deadline:
                    raise RuntimeError("翻訳サーバーの起動がタイムアウトしました") from None
                time.sleep(0.1)
        with tempfile.TemporaryDirectory(prefix="ptranslate-smoke-") as config:
            if args.smoke_test:
                env["PTRANSLATE_SMOKE_CONFIG"] = config
            app = subprocess.Popen([str(binary)], cwd=ROOT, env=env)
            try:
                result = app.wait(timeout=60 if args.smoke_test else None)
                if args.smoke_test and result == 0:
                    deadline = time.monotonic() + 5
                    while stats["canceled"] < 3 and time.monotonic() < deadline:
                        time.sleep(0.1)
                    if stats["canceled"] < 3:
                        raise RuntimeError("中止が翻訳エンジンに伝わりませんでした")
                    print("翻訳エンジンまでの中止: PASS", flush=True)
                    saved = json.loads((Path(config) / "settings.json").read_text())
                    if set(saved) != {"source", "target", "form", "glossary", "auto", "shortcut"}:
                        raise RuntimeError("設定に保存してはいけない項目があります")
                    print("設定の保存と原文・訳文を保存しないこと: PASS", flush=True)
                return result
            except subprocess.TimeoutExpired:
                raise RuntimeError("メインウィンドウの試験がタイムアウトしました") from None
    finally:
        for process in (app, server):
            if process is not None and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
        mock.shutdown()
        mock.server_close()
        if clipboard is not None:
            subprocess.run(["pbcopy"], input=clipboard, check=True)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
