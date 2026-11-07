#!/usr/bin/env python3
"""翻訳エンジン (PLaMo 3 Translate を動かす llama-server の /v1/completions) のふり。

試験と、翻訳エンジンの無い手元での開発に使う。原文を「[訳す先] 原文」として、単語ごとに
Server-Sent Events で返す。原文の中に次の印があれば、その動きをする。

- `[slow]`   最初の断片を返す前に --slow 秒待つ (前段の生存確認の試験)
- `[length]` `finish_reason: length` で終える (長すぎて切れた場合)
- `[error]`  500 を返す

    python3 tools/mock_plamo.py [--port 18082] [--api-key KEY] [--delay 0.05] [--slow 40]

GET / は受けたリクエストの数 ({"requests": n, "canceled": m}) を返す。
"""

from __future__ import annotations

import argparse
import json
import re
import socketserver
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

INPUT_RE = re.compile(
    r"<\|plamo:tag\|>input(?:<\|plamo:key\|>lang<\|plamo:val\|>[a-z()-]+)?<\|plamo:msg\|>"
)
LANG_RE = re.compile(r"<\|plamo:tag\|>output<\|plamo:key\|>lang<\|plamo:val\|>([a-z()-]+)")


class MockHTTPServer(ThreadingHTTPServer):
    def server_bind(self):
        # モックも名前の逆引きを待たずに起動する。
        socketserver.TCPServer.server_bind(self)
        self.server_name, self.server_port = self.server_address[:2]


def make_server(port: int = 0, api_key: str = "", delay: float = 0.05, slow: float = 40.0):
    stats = {"requests": 0, "canceled": 0, "prompts": []}
    lock = threading.Lock()

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *args):
            pass

        def do_GET(self):
            with lock:
                body = json.dumps({k: stats[k] for k in ("requests", "canceled")}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            if api_key and self.headers.get("Authorization") != f"Bearer {api_key}":
                self.send_response(401)
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            prompt = body["prompt"]
            with lock:
                stats["requests"] += 1
                stats["prompts"].append(prompt)
            text = INPUT_RE.split(prompt)[-1].split("<|plamo:tag|>", 1)[0]
            lang = LANG_RE.findall(prompt)[-1]
            if "[error]" in text:
                self.send_response(500)
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Connection", "close")
            self.end_headers()
            finish = "length" if "[length]" in text else "stop"
            try:
                if "[slow]" in text:
                    time.sleep(slow)
                for word in f"[{lang}] {text}".split(" "):
                    chunk = {"choices": [{"text": word + " ", "finish_reason": None}]}
                    self.wfile.write(f"data: {json.dumps(chunk, ensure_ascii=False)}\n\n".encode())
                    self.wfile.flush()
                    time.sleep(delay)
                done = {"choices": [{"text": "", "finish_reason": finish}]}
                self.wfile.write(f"data: {json.dumps(done)}\n\ndata: [DONE]\n\n".encode())
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                with lock:
                    stats["canceled"] += 1

    server = MockHTTPServer(("127.0.0.1", port), Handler)
    server.daemon_threads = True
    return server, stats


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--port", type=int, default=18082)
    ap.add_argument("--api-key", default="")
    ap.add_argument("--delay", type=float, default=0.05, help="断片ごとの待ち (秒)")
    ap.add_argument("--slow", type=float, default=40.0, help="[slow] のときの最初の待ち (秒)")
    args = ap.parse_args()
    server, _ = make_server(args.port, args.api_key, args.delay, args.slow)
    print(f"mock PLaMo on http://127.0.0.1:{server.server_address[1]}/v1/completions", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
