"""起動中の試験用アプリと取得済みモデルで中継・並列翻訳・中止を確認する（手動専用）。"""

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import re
import struct
import subprocess
import threading
import time

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--extension-id", required=True)
    parser.add_argument("--parallel", type=int, choices=range(1, 65), required=True)
    args = parser.parse_args()
    if not re.fullmatch("[a-p]{32}", args.extension_id):
        parser.error("拡張IDが不正です")
    origin = f"chrome-extension://{args.extension_id}/"
    binary = ROOT / "desktop/src-tauri/target/debug/ptranslate-desk.exe"

    def call(route, body=None, cancel=False):
        with subprocess.Popen(
            [str(binary), origin],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        ) as host:
            watchdog = threading.Timer(45, host.kill)
            watchdog.start()
            started = time.monotonic()
            data = bytearray()
            status = None
            try:
                request = json.dumps({"version": 1, "route": route, "body": body}).encode()
                host.stdin.write(struct.pack("<I", len(request)) + request)
                host.stdin.flush()
                while True:
                    header = host.stdout.read(4)
                    if len(header) != 4:
                        raise RuntimeError("中継の応答が切れました")
                    length = struct.unpack("<I", header)[0]
                    if not 0 < length <= 256 * 1024:
                        raise RuntimeError("中継の応答サイズが不正です")
                    message = json.loads(host.stdout.read(length))
                    if message["kind"] == "start":
                        status = message["status"]
                        if cancel:
                            host.stdin.close()
                            assert host.wait(timeout=5) == 0
                            return None
                    elif message["kind"] == "bytes":
                        data.extend(message["bytes"])
                    elif message["kind"] == "end":
                        break
                    elif message["kind"] == "error":
                        raise RuntimeError("中継がエラーを返しました")
                assert status == 200
                assert host.wait(timeout=5) == 0
                if route == "api/config":
                    return json.loads(data)
                events = [
                    json.loads(line[6:])
                    for line in data.decode().splitlines()
                    if line.startswith("data: ")
                ]
                text = "".join(event.get("t", "") for event in events)
                assert any(event.get("done") for event in events)
                assert not any("error" in event for event in events)
                assert re.search("[ぁ-んァ-ン一-龯]", text)
                return {"seconds": round(time.monotonic() - started, 3), "characters": len(text)}
            finally:
                watchdog.cancel()
                if host.poll() is None:
                    host.kill()
                    host.wait()

    config = call("api/config")
    assert config["concurrency"]["max_in_flight"] == args.parallel
    assert config["concurrency"]["scope"] == "application"
    print(f"並列数の通知: {args.parallel} PASS", flush=True)
    with ThreadPoolExecutor(max_workers=args.parallel) as pool:
        results = list(
            pool.map(
                lambda i: call(
                    "api/translate",
                    {
                        "text": f"Example {i + 1}. This application translates text on your computer. Please return your books to the library on time.",
                        "target_lang": "ja",
                    },
                ),
                range(args.parallel),
            )
        )
    print("同時要求の翻訳完了: " + json.dumps(results), flush=True)
    call(
        "api/translate",
        {"text": "This application translates text on your computer. " * 60, "target_lang": "ja"},
        cancel=True,
    )
    result = call(
        "api/translate", {"text": "The library opens at nine in the morning.", "target_lang": "ja"}
    )
    print("中止後の再翻訳: " + json.dumps(result) + " PASS", flush=True)


if __name__ == "__main__":
    main()
