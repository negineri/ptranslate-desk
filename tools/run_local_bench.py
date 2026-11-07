#!/usr/bin/env python3
"""取得済みの実モデルを使い、ローカル試作の設定でbench.pyを実行する。"""

import argparse
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--device", default="Vulkan0")
    parser.add_argument("--slots", default="1,4")
    parser.add_argument("--parallel", default="1,2,4")
    parser.add_argument("--text", default="ja-short,ja-long,en-long")
    parser.add_argument("--repeat", type=int, default=3)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    install = json.loads(
        (
            Path(os.environ["LOCALAPPDATA"])
            / "com.witenp.ptranslate-desk/inference/installation.json"
        ).read_text(encoding="utf-8")
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with args.output.open("w", encoding="utf-8") as output:
        for slots in map(int, args.slots.split(",")):
            with socket.socket() as listener:
                listener.bind(("127.0.0.1", 0))
                port = listener.getsockname()[1]
            url = f"http://127.0.0.1:{port}"
            env = dict(os.environ)
            key = secrets.token_urlsafe(32)
            env.update(
                LLAMA_API_KEY=key, TR_API_KEY=key, NO_PROXY="127.0.0.1,localhost", PYTHONUTF8="1"
            )
            command = [
                install["engine"],
                "--model",
                install["model"],
                "--host",
                "127.0.0.1",
                "--port",
                str(port),
                "--ctx-size",
                "32768",
                "--parallel",
                str(slots),
                "--flash-attn",
                "on",
                "--no-webui",
                "--log-disable",
                "--device",
                args.device,
                "--n-gpu-layers",
                "0" if args.device == "none" else "99",
            ]
            engine = subprocess.Popen(
                command,
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                creationflags=subprocess.CREATE_NO_WINDOW,
            )
            try:
                deadline = time.monotonic() + 120
                while True:
                    if engine.poll() is not None:
                        raise RuntimeError("推論エンジンが終了しました")
                    try:
                        request = urllib.request.Request(
                            url + "/health", headers={"Authorization": f"Bearer {key}"}
                        )
                        with opener.open(request, timeout=2) as response:
                            if response.status == 200:
                                break
                    except (urllib.error.URLError, TimeoutError):
                        if time.monotonic() > deadline:
                            raise RuntimeError("モデル読み込みがタイムアウトしました") from None
                        time.sleep(0.2)
                print(f"計測開始: device={args.device}, np={slots}, FA=on, ctx=32768", flush=True)
                errors = tempfile.TemporaryFile(mode="w+", encoding="utf-8")
                bench = subprocess.Popen(
                    [
                        sys.executable,
                        "tools/bench.py",
                        "--url",
                        url + "/v1/completions",
                        "--parallel",
                        args.parallel,
                        "--text",
                        args.text,
                        "--repeat",
                        str(args.repeat),
                    ],
                    cwd=ROOT,
                    env=env,
                    stdout=subprocess.PIPE,
                    stderr=errors,
                    text=True,
                    encoding="utf-8",
                )
                timeout = threading.Timer(3600, bench.kill)
                timeout.start()
                try:
                    for line in bench.stdout:
                        row = {
                            "device": args.device,
                            "slots": slots,
                            "ctx": 32768,
                            "flash_attention": True,
                            "repeat": args.repeat,
                            **json.loads(line),
                        }
                        encoded = json.dumps(row, ensure_ascii=False)
                        print(encoded, flush=True)
                        output.write(encoded + "\n")
                        output.flush()
                    if bench.wait():
                        errors.seek(0)
                        raise RuntimeError("bench.pyが失敗しました: " + errors.read())
                finally:
                    timeout.cancel()
                    if bench.poll() is None:
                        bench.kill()
                    bench.wait()
                    bench.stdout.close()
                    errors.close()
            finally:
                if engine.poll() is None:
                    engine.terminate()
                engine.wait(timeout=15)


if __name__ == "__main__":
    main()
