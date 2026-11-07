#!/usr/bin/env python3
"""Chrome 拡張の開発用に、モック・翻訳サーバー・認証プロキシの代わりをまとめて起動する。

認証プロキシの代わりは 127.0.0.1 だけで待ち受け、開発用の利用者名を X-Forwarded-User に付けて
翻訳サーバーへ中継する。拡張の設定のサーバーURLには表示された URL を入れる。開発の端末でだけ使う。
"""

from __future__ import annotations

import argparse
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
import socket
import subprocess
import sys
import threading
import time

from mock_plamo import make_server

ROOT = Path(__file__).resolve().parent.parent
SERVER_PORT = 8002
DEV_USER = "mock-extension"
# 中継しない (接続ごとの) ヘッダー
HOP_BY_HOP = {
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
}


class DevProxy(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        sys.stderr.write("[proxy] " + (fmt % args) + "\n")

    def do_GET(self):
        self.forward()

    def do_POST(self):
        self.forward()

    def forward(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length) if length else None
        headers = {
            k: v
            for k, v in self.headers.items()
            if k.lower() not in HOP_BY_HOP | {"host", "x-forwarded-user", "content-length"}
        }
        # 本物の認証プロキシが検証した利用者名の代わり
        headers["X-Forwarded-User"] = DEV_USER
        conn = http.client.HTTPConnection("127.0.0.1", SERVER_PORT, timeout=600)
        try:
            conn.request(self.command, self.path, body=body, headers=headers)
            res = conn.getresponse()
            self.send_response(res.status, res.reason)
            for k, v in res.getheaders():
                if k.lower() not in HOP_BY_HOP | {"content-length"}:
                    self.send_header(k, v)
            self.send_header("Connection", "close")
            self.end_headers()
            self.close_connection = True
            # SSE を貯めずに流す
            while chunk := res.read1(65536):
                self.wfile.write(chunk)
                self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass  # 拡張が中止した。conn を閉じて翻訳サーバーにも伝える
        finally:
            conn.close()


def port_free(port: int) -> bool:
    with socket.socket() as probe:
        try:
            probe.bind(("127.0.0.1", port))
        except OSError:
            return False
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--port", type=int, default=8003, help="認証プロキシの代わりの待ち受け")
    parser.add_argument("--delay", type=float, default=0.05, help="モックの断片ごとの待ち (秒)")
    args = parser.parse_args()
    for port in (SERVER_PORT, args.port):
        if not port_free(port):
            parser.error(f"127.0.0.1:{port} は使用中です。先に使用中のサーバーを止めてください")

    mock, _ = make_server(delay=args.delay)
    threading.Thread(target=mock.serve_forever, daemon=True).start()
    env = {key: value for key, value in os.environ.items() if not key.startswith("TR_")}
    env.update(
        TR_LISTEN_HOST="127.0.0.1",
        TR_LISTEN_PORT=str(SERVER_PORT),
        TR_UPSTREAM_URL=f"http://127.0.0.1:{mock.server_port}/v1/completions",
        TR_PUBLIC_SERVER_URL=f"http://127.0.0.1:{args.port}",
    )
    server = subprocess.Popen([sys.executable, "-I", "server/translator.py"], cwd=ROOT, env=env)
    proxy = ThreadingHTTPServer(("127.0.0.1", args.port), DevProxy)
    proxy.daemon_threads = True
    threading.Thread(target=proxy.serve_forever, daemon=True).start()
    try:
        time.sleep(0.5)
        if server.poll() is not None:
            raise RuntimeError("翻訳サーバーを起動できませんでした")
        print(f"拡張の設定のサーバーURL: http://127.0.0.1:{args.port}  (Ctrl+C で終了)", flush=True)
        return server.wait()
    finally:
        proxy.shutdown()
        proxy.server_close()
        if server.poll() is None:
            server.terminate()
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()
        mock.shutdown()
        mock.server_close()


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
