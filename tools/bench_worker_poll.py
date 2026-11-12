#!/usr/bin/env python3
"""ローカルの仲介で待機中のワーカーによる負荷を測る。"""

import argparse
import concurrent.futures
import json
from pathlib import Path
import sys
import tempfile
import threading
import time
from unittest.mock import patch

from mock_worker import MockWorker

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "server"))
import translator as tr  # noqa: E402


def measure(workers, seconds):
    model = "a" * 64
    with (
        tempfile.TemporaryDirectory() as directory,
        patch.multiple(
            tr,
            STATE_FILE=str(Path(directory) / "workers.sqlite"),
            WORKER_MODEL_SHA256=model,
            MIN_AGENT_VERSION="0.5.0",
            COUNTRY_HEADER="X-Test-Country",
            ALLOWED_COUNTRIES=frozenset({"JP"}),
            CLIENT_IP_HEADER="X-Test-IP",
            CANARY_FILE="",
            AUDIT_UPSTREAMS="",
            MAX_OPEN_REQUESTS=workers + 1,
        ),
    ):
        app = tr.TranslationHTTPServer(("127.0.0.1", 0), tr.Handler)
        server = threading.Thread(target=app.serve_forever)
        server.start()
        try:
            hub = app.get_workers()
            clients = []
            for index in range(workers):
                client = MockWorker(f"http://127.0.0.1:{app.server_port}")
                status, _ = client.register(f"bench-worker-{index}")
                if status != 200:
                    raise RuntimeError(f"モックの登録に失敗しました (HTTP {status})")
                client.timeout = hub.poll_seconds + 5
                clients.append(client)

            queries = 0
            lock = threading.Lock()
            authenticate = hub.registry.authenticate

            def count_authentication(secret):
                nonlocal queries
                with lock:
                    queries += 1
                return authenticate(secret)

            with (
                patch.object(hub.registry, "authenticate", count_authentication),
                concurrent.futures.ThreadPoolExecutor(max_workers=workers) as executor,
            ):
                futures = []
                try:
                    for client in clients:
                        futures.append(executor.submit(client.poll, model))
                        # 接続開始が同じ瞬間に集中するのを避ける。
                        time.sleep(0.005)
                    with hub.condition:
                        if not hub.condition.wait_for(lambda: len(hub.ready) == workers, timeout=5):
                            raise RuntimeError("測定開始前に全ワーカーが待機できませんでした")
                    with lock:
                        baseline = queries
                        started = time.monotonic()
                        cpu = time.process_time()
                    time.sleep(seconds)
                    with lock:
                        elapsed = time.monotonic() - started
                        cpu_seconds = time.process_time() - cpu
                        rechecks = queries - baseline
                    with hub.condition:
                        if len(hub.ready) != workers:
                            raise RuntimeError("測定中にワーカーの待機が途切れました")
                finally:
                    with hub.condition:
                        hub.closed = True
                        hub.condition.notify_all()
                for future in futures:
                    status, _ = future.result()
                    if status != 204:
                        raise RuntimeError(f"待機の終了に失敗しました (HTTP {status})")
            return {
                "workers": workers,
                "elapsed_seconds": round(elapsed, 3),
                "process_cpu_seconds": round(cpu_seconds, 3),
                "registry_authentications": rechecks,
                "registry_authentications_per_second": round(rechecks / elapsed, 2),
            }
        finally:
            app.shutdown()
            server.join()
            app.server_close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workers", type=int, default=200)
    parser.add_argument("--seconds", type=int, default=12)
    args = parser.parse_args()
    if not 1 <= args.workers <= 200 or not 6 <= args.seconds <= 15:
        parser.error("--workers は 1〜200、--seconds は 6〜15 を指定してください")
    try:
        result = measure(args.workers, args.seconds)
    except (OSError, RuntimeError) as error:
        parser.exit(1, f"測定できませんでした ({type(error).__name__})\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
