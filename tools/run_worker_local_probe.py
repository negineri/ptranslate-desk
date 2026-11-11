#!/usr/bin/env python3
"""macOSの制限付きワーカーを取得済み実モデルとローカル仲介で確認する。通常のCIでは実行しない。"""

import argparse
import base64
from contextlib import contextmanager
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import secrets
import select
import socket
import socketserver
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from unittest.mock import patch

from bench_worker_delivery import BenchmarkBroker, BenchmarkHandler, serving, tls_contexts, tr
from mock_worker import MockWorker

ROOT = Path(__file__).resolve().parent.parent
TEST = "worker_agent::real_model_probe::restricted_worker"
TEXT = "Hello. This application translates text on your computer."


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def wait(check, message, seconds=15):
    end = time.monotonic() + seconds
    while not check():
        require(time.monotonic() < end, message)
        time.sleep(0.025)


def test_binary():
    build = subprocess.run(
        [
            "cargo",
            "test",
            "--locked",
            "--no-run",
            "--manifest-path",
            "desktop/src-tauri/Cargo.toml",
            "--message-format=json",
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=300,
    )
    require(build.returncode == 0, "実モデル確認用の試験バイナリをビルドできませんでした")
    for line in build.stdout.splitlines():
        artifact = json.loads(line)
        if artifact.get("reason") == "compiler-artifact" and artifact.get("executable"):
            if artifact["target"]["name"] == "ptranslate-desk" and artifact["profile"]["test"]:
                return artifact["executable"]
    raise RuntimeError("実モデル確認用の試験バイナリがありません")


@contextmanager
def engine(device):
    home = Path.home() / "Library/Application Support/com.witenp.ptranslate-desk/inference"
    install = json.loads((home / "installation.json").read_text())
    source = (ROOT / "desktop/src-tauri/src/local/resources.rs").read_text()
    expected = re.search(r'MODEL_SHA: &str =\s*"([a-f0-9]{64})"', source)[1]
    with Path(install["model"]).open("rb") as model:
        model_hash = hashlib.file_digest(model, "sha256").hexdigest()
    require(model_hash == expected, "取得済みモデルのSHA-256が一致しません")
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    secret = secrets.token_urlsafe(32)
    env = {key: value for key, value in os.environ.items() if not key.startswith("LLAMA_")}
    env["LLAMA_API_KEY"] = secret
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
        "1",
        "--flash-attn",
        "on",
        "--no-webui",
        "--log-disable",
        "--device",
        device,
        "--n-gpu-layers",
        "0" if device == "none" else "99",
    ]
    process = subprocess.Popen(
        command,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def get(route, authenticated=True):
        headers = {"Authorization": f"Bearer {secret}"} if authenticated else {}
        with opener.open(
            urllib.request.Request(f"http://127.0.0.1:{port}/{route}", headers=headers), timeout=3
        ) as response:
            return json.load(response)

    try:

        def ready():
            require(process.poll() is None, "実エンジンが起動中に終了しました")
            try:
                return get("health")["status"] == "ok"
            except (urllib.error.URLError, TimeoutError):
                return False

        wait(ready, "実エンジンの起動がタイムアウトしました", 120)
        try:
            get("slots", False)
        except urllib.error.HTTPError as error:
            require(error.code == 401, "認証なしの実エンジン要求が401ではありません")
        else:
            raise RuntimeError("認証なしで実エンジンにアクセスできました")
        print("モデルのSHA-256・実エンジンの起動・認証なしの拒否: PASS", flush=True)
        yield port, secret, model_hash, lambda: not any(s["is_processing"] for s in get("slots"))
    finally:
        if process.poll() is None:
            process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


@contextmanager
def relay(broker_port):
    """子に許可された二つのポートの一方。認証付きCONNECTで固定の仲介にだけ中継する。"""
    secret = secrets.token_urlsafe(32)
    authorization = "Basic " + base64.b64encode(f"worker:{secret}".encode()).decode()

    class Handler(socketserver.StreamRequestHandler):
        def handle(self):
            try:
                self.forward()
            except OSError:
                # 中止時の接続リセットは、TLSを終端しない中継の通常の終了経路。
                return

        def forward(self):
            self.connection.settimeout(5)
            head = bytearray()
            while not head.endswith(b"\r\n\r\n") and len(head) < 4096:
                value = self.connection.recv(1)
                if not value:
                    return
                head.extend(value)
            lines = head.decode("ascii").split("\r\n")
            headers = dict(line.split(": ", 1) for line in lines[1:] if ": " in line)
            if (
                lines[0] != f"CONNECT 127.0.0.1:{broker_port} HTTP/1.1"
                or headers.get("Proxy-Authorization", headers.get("proxy-authorization"))
                != authorization
            ):
                self.connection.sendall(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                return
            with socket.create_connection(("127.0.0.1", broker_port), timeout=5) as broker:
                self.connection.sendall(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                while True:
                    readable, _, _ = select.select([broker, self.connection], [], [], 1)
                    if server.stopping.is_set():
                        return
                    for current in readable:
                        data = current.recv(16384)
                        if not data:
                            return
                        (broker if current is self.connection else self.connection).sendall(data)

    class Server(socketserver.ThreadingTCPServer):
        daemon_threads = True

    server = Server(("127.0.0.1", 0), Handler)
    server.stopping = threading.Event()
    try:
        with serving(server):
            yield server.server_address[1], secret
    finally:
        server.stopping.set()


class Worker:
    def __init__(self, binary, certificate, bootstrap):
        self.states = []
        self.error = None
        self.stopped_at = None
        self.exited_at = None
        self.lock = threading.Lock()
        env = {
            "PTRANSLATE_WORKER_PROBE_PORTS": f"{bootstrap['relay_port']},{bootstrap['engine_port']}",
            "PTRANSLATE_WORKER_PROBE_CA": certificate,
        }
        self.process = subprocess.Popen(
            [binary, "--exact", TEST, "--ignored", "--nocapture", "--test-threads=1"],
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )

        def send(value):
            with self.lock:
                if not self.process.stdin.closed:
                    self.process.stdin.write(value)
                    self.process.stdin.flush()

        def read():
            try:
                for line in self.process.stdout:
                    _, marker, payload = line.partition(b'{"status":')
                    if not marker:
                        continue
                    state = json.loads(marker + payload)["status"]
                    self.states.append(state)
                    if state == "ready":
                        send(json.dumps(bootstrap).encode() + b"\n")
                    elif state == "assigned":
                        send(b"go\n")
            except (OSError, ValueError, KeyError) as error:
                self.error = type(error).__name__
            finally:
                self.process.wait()
                self.exited_at = time.monotonic()

        self.thread = threading.Thread(target=read, daemon=True)
        self.thread.start()

    def stop(self):
        with self.lock:
            if not self.process.stdin.closed:
                self.stopped_at = time.monotonic()
                self.process.stdin.close()

    def finish(self, expected, seconds=15):
        require(self.process.wait(timeout=seconds) == 0, "ワーカーの試験プロセスが異常終了しました")
        self.thread.join(timeout=2)
        require(
            not self.thread.is_alive() and self.error is None, "ワーカーの状態通知を取得できません"
        )
        require(
            self.states and self.states[-1] == expected,
            f"ワーカーの終了状態が不正です: {self.states}",
        )
        if self.stopped_at is not None:
            require(
                self.exited_at - self.stopped_at < 6,
                "親の停止要求から子の回収まで6秒以上かかりました",
            )

    def close(self):
        self.stop()
        try:
            self.process.wait(timeout=6)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        self.thread.join(timeout=2)
        self.process.stdout.close()


class Handler(BenchmarkHandler):
    def client_disconnected(self):
        try:
            return super().client_disconnected()
        except ssl.SSLEOFError:
            return True

    def do_POST(self):
        # ローカル試験の前段。外部からは待ち受けず、ワーカーの地域・ネットワークを模擬する。
        if self.path.startswith("/api/worker/"):
            self.headers["X-Test-Country"] = "JP"
            self.headers["X-Test-IP"] = "192.0.2.1"
        super().do_POST()


class Broker(BenchmarkBroker):
    def __init__(self, server_tls, client_tls):
        super().__init__(server_tls, client_tls)
        self.RequestHandlerClass = Handler


def translate(server, *, cancel=False, stop_parent=None):
    connection = http.client.HTTPSConnection(
        "127.0.0.1", server.server_port, timeout=30, context=server.client_tls
    )
    started = time.monotonic()
    try:
        connection.request(
            "POST",
            "/api/translate",
            json.dumps(
                {
                    "text": TEXT * (40 if cancel or stop_parent else 1),
                    "target_lang": "ja",
                    "consent": tr.CONSENT_VERSION,
                }
            ),
            {
                "Content-Type": "application/json",
                "X-Forwarded-User": "probe-reader",
                "X-Test-Country": "JP",
                "X-Test-IP": "198.51.100.1",
            },
        )
        response = connection.getresponse()
        require(response.status == 200, f"依頼元への応答がHTTP {response.status}です")
        chunks, translated, first, stopped = 0, "", None, None
        for line in response:
            if not line.startswith(b"data: "):
                continue
            event = json.loads(line[6:])
            if event.get("error") and stopped is not None:
                return {"reader_error_ms": round((time.monotonic() - stopped) * 1000)}
            require(not event.get("error"), "依頼元に翻訳のエラーが届きました")
            if event.get("t"):
                chunks += 1
                translated += event["t"]
                if first is None:
                    first = time.monotonic() - started
                    if stop_parent:
                        stopped = time.monotonic()
                        stop_parent()
                if cancel:
                    response.close()
                    return {"first_ms": round(first * 1000), "chunks": chunks}
            if event.get("done"):
                require(stop_parent is None, "親の停止後も翻訳が正常終了しています")
                require(event.get("finish") == "stop", "翻訳が正常終了していません")
                require(re.search(r"[ぁ-んァ-ン一-龯]", translated), "日本語の訳がありません")
                return {
                    "first_ms": round(first * 1000),
                    "total_ms": round((time.monotonic() - started) * 1000),
                    "chunks": chunks,
                    "chars": len(translated),
                }
        raise RuntimeError("依頼元への逐次配信が途中で終了しました")
    finally:
        connection.close()


def probe(binary, directory, device):
    issues = []
    server_tls, client_tls = tls_contexts(directory)
    canary = directory / "canary.json"
    canary.write_text(json.dumps([{"text": "Synthetic check.", "target_lang": "ja"}]))
    canary.chmod(0o600)
    with engine(device) as (port, secret, model, idle):
        settings = {
            "POOL_KIND": "volunteer",
            "STATE_FILE": str(directory / "worker.sqlite"),
            "WORKER_MODEL_SHA256": model,
            "MIN_AGENT_VERSION": "0.5.0",
            "COUNTRY_HEADER": "X-Test-Country",
            "CLIENT_IP_HEADER": "X-Test-IP",
            "ALLOWED_COUNTRIES": frozenset({"JP"}),
            "ANON_ENABLED": False,
            "UPSTREAMS": "",
            "UPSTREAM_URL": f"http://127.0.0.1:{port}/v1/completions",
            "UPSTREAM_SLOTS": 1,
            "API_KEY": secret,
            "CANARY_FILE": str(canary),
            "AUDIT_UPSTREAMS": json.dumps(
                [{"url": f"http://127.0.0.1:{port}/v1/completions", "slots": 1}]
            ),
            "QUEUE_TIMEOUT": 4,
        }
        with patch.multiple(tr, **settings), serving(Broker(server_tls, client_tls)) as server:
            server.get_pool()
            hub = server.get_workers()
            hub.poll_seconds = 20
            registered = MockWorker(
                f"https://127.0.0.1:{server.server_port}", tls_context=client_tls
            )
            status, account = registered.register("probe-owner")
            require(status == 200, "ローカル仲介へのワーカー登録に失敗しました")
            info = hub.registry.info(account["worker_id"])
            # 配信経路に集中するため、試用の成績は試験内で合格にする。品質検査は別試験。
            for _ in range(hub.checks.health.probation_jobs):
                hub.checks.health.record(info, passed=True)
            print("ローカル仲介へのワーカー登録: PASS（試用の成績は模擬）", flush=True)
            result_events = []
            original_result = hub.result

            def observe_result(secret, body):
                with hub.condition:
                    assignment = hub.assignments.get(body.get("assignment_id"))
                    expected = assignment["seq"] if assignment else None
                response = original_result(secret, body)
                if "error" in body:
                    result_events.append({"seq": body.get("seq"), "expected_seq": expected})
                return response

            with (
                patch.object(hub.checks, "after_delivery", return_value=False),
                patch.object(hub, "result", side_effect=observe_result),
                relay(server.server_port) as (relay_port, relay_secret),
            ):
                config = {
                    "broker": f"https://127.0.0.1:{server.server_port}",
                    "worker_secret": registered.secret,
                    "model_sha256": model,
                    "agent_version": "0.5.0",
                    "engine_port": port,
                    "engine_secret": secret,
                    "relay_port": relay_port,
                    "relay_secret": relay_secret,
                }
                certificate = (directory / "tls.crt").read_text()
                for scenario in ("complete", "cancel", "parent-stop", "parent-stop-running"):
                    worker = Worker(binary, certificate, config)
                    try:
                        wait(
                            lambda: (
                                account["worker_id"] in hub.ready
                                or worker.process.poll() is not None
                            ),
                            "ワーカーが待機状態になりません",
                        )
                        require(
                            account["worker_id"] in hub.ready,
                            f"ワーカーが待機前に終了しました: {worker.states}",
                        )
                        if scenario == "parent-stop":
                            started = time.monotonic()
                            worker.stop()
                            worker.finish("paused", 6)
                            print(
                                f"親の入力終了による待機中の停止・回収: PASS ({time.monotonic() - started:.2f}秒)",
                                flush=True,
                            )
                        else:

                            def stop_running():
                                worker.stop()
                                worker.finish("error", 6)
                                wait(idle, "親の停止後に実エンジンの処理枠が空きません", 6)

                            result = translate(
                                server,
                                cancel=scenario == "cancel",
                                stop_parent=stop_running
                                if scenario == "parent-stop-running"
                                else None,
                            )
                            expected = {
                                "complete": "completed",
                                "cancel": "paused",
                                "parent-stop-running": "error",
                            }
                            worker.finish(expected[scenario])
                            if worker.stopped_at is not None:
                                result["worker_stop_ms"] = round(
                                    (worker.exited_at - worker.stopped_at) * 1000
                                )
                                result["error_results"] = list(result_events)
                                if any(
                                    e["expected_seq"] is not None and e["seq"] < e["expected_seq"]
                                    for e in result_events
                                ):
                                    issues.append(
                                        "推論中の親停止でエラー結果に受信済みの連番が使われました"
                                    )
                                if result["reader_error_ms"] >= 6000:
                                    issues.append("親停止後のエラー通知が6秒以上遅れました")
                            require(
                                "assigned" in worker.states and "working" in worker.states,
                                "割り当て・開始の状態通知がありません",
                            )
                            wait(idle, "中止・完了後に実エンジンの処理枠が空きません", 6)
                            print(f"{scenario}: PASS {json.dumps(result)}", flush=True)
                            if issues:
                                print(f"親停止後のエラー通知: FAIL ({issues[-1]})", flush=True)
                    finally:
                        worker.close()
                        wait(
                            lambda: not hub.assignments and not hub.polling,
                            "前の試験の割り当てや待機が仲介に残っています",
                            6,
                        )
                wait(
                    lambda: not hub.assignments and not hub.polling,
                    "仲介に割り当てや待機が残っています",
                    6,
                )
            print(
                "制限付きワーカーの割り当て・実推論・逐次配信・中止・待機停止・処理枠の回収: PASS",
                flush=True,
            )
            status, _ = registered.call(
                f"/api/worker/{account['worker_id']}", owner="probe-owner", method="DELETE"
            )
            require(status == 200, "ローカル仲介のワーカー登録解除に失敗しました")
            require(registered.poll(model)[0] == 401, "登録解除後の資格が拒否されません")
            print("ローカル仲介の登録解除・解除後の資格の拒否: PASS", flush=True)
    print("実エンジンの停止・回収: PASS", flush=True)
    require(not issues, " / ".join(issues))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--device", choices=["MTL0", "none"], default="MTL0")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("この検証はmacOS専用です")
    with tempfile.TemporaryDirectory(prefix="ptranslate-worker-probe-") as directory:
        probe(test_binary(), Path(directory), args.device)


if __name__ == "__main__":
    main()
