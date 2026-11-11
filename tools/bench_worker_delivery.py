#!/usr/bin/env python3
"""空き枠がある場合の最初の翻訳断片を、ローカルのモックだけで比較する。"""

import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import http.client
import json
import math
from pathlib import Path
import queue
import sys
import tempfile
import threading
import time
from unittest.mock import patch

from mock_plamo import make_server
from mock_worker import MockWorker

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "server"))
import translator as tr  # noqa: E402

MODEL = "a" * 64
INPUT = {"text": "A synthetic translation benchmark sentence.", "target_lang": "en"}


@contextmanager
def serving(server):
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    try:
        yield server
    finally:
        server.shutdown()
        thread.join()
        server.server_close()


def first_fragment(server):
    conn = http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=8)
    try:
        started = time.perf_counter()
        conn.request(
            "POST",
            "/api/translate",
            json.dumps({**INPUT, "consent": tr.CONSENT_VERSION}),
            {
                "Content-Type": "application/json",
                "X-Forwarded-User": "bench-reader",
                "X-Test-Country": "JP",
                "X-Test-IP": "198.51.100.1",
            },
        )
        response = conn.getresponse()
        if response.status != 200:
            raise RuntimeError(f"翻訳の要求に失敗しました (HTTP {response.status})")
        first = None
        for line in response:
            if not line.startswith(b"data: "):
                continue
            event = json.loads(line[6:])
            if first is None and event.get("t"):
                first = (time.perf_counter() - started) * 1000
            if event.get("done") and event.get("finish") == "stop" and first is not None:
                return first
        raise RuntimeError("翻訳が正常に終わりませんでした")
    finally:
        conn.close()


def deliver(worker, engine):
    status, assignment = worker.poll(MODEL)
    if status != 200:
        raise RuntimeError(f"割り当てに失敗しました (HTTP {status})")
    request = tr.parse_request(assignment["input"])
    events = queue.Queue()
    cancel = tr.UpstreamCancellation()
    with ThreadPoolExecutor(max_workers=1) as producer:
        future = producer.submit(
            tr.upstream_stream,
            tr.build_prompt(request),
            tr.max_tokens(request["text"]),
            events,
            cancel,
            engine,
        )
        try:
            seq = 0
            while True:
                kind, value = events.get(timeout=5)
                if kind == "text":
                    fields = {"text": value, "done": False}
                elif kind == "done":
                    fields = {"done": True, "finish": value}
                else:
                    raise RuntimeError("模擬エンジンが失敗しました")
                status, result = worker.result(assignment["assignment_id"], seq, **fields)
                if status != 200 or result.get("stop"):
                    raise RuntimeError(f"結果の返送に失敗しました (HTTP {status})")
                if kind == "done":
                    break
                seq += 1
        finally:
            cancel.set()
        future.result()


def samples(count, engine, state_dir):
    canary = state_dir / "canary.json"
    canary.write_text(json.dumps([INPUT]), encoding="utf-8")
    canary.chmod(0o600)
    settings = {
        "UPSTREAM_URL": engine,
        "UPSTREAMS": "",
        "UPSTREAM_SLOTS": 1,
        "STATE_FILE": str(state_dir / "worker.sqlite"),
        "WORKER_MODEL_SHA256": MODEL,
        "MIN_AGENT_VERSION": "0.5.0",
        "COUNTRY_HEADER": "X-Test-Country",
        "CLIENT_IP_HEADER": "X-Test-IP",
        "ALLOWED_COUNTRIES": frozenset({"JP"}),
        "ANON_ENABLED": False,
        "API_KEY": "",
        "CANARY_FILE": str(canary),
        "AUDIT_UPSTREAMS": json.dumps([{"url": engine, "slots": 1}]),
        "QUEUE_TIMEOUT": 4,
    }
    with (
        patch.multiple(tr, POOL_KIND="direct", **settings),
        serving(tr.TranslationHTTPServer(("127.0.0.1", 0), tr.Handler)) as direct,
    ):
        direct.get_pool()
        direct_times = [first_fragment(direct) for _ in range(count)]
    with (
        patch.multiple(tr, POOL_KIND="volunteer", **settings),
        serving(tr.TranslationHTTPServer(("127.0.0.1", 0), tr.Handler)) as server,
        ThreadPoolExecutor(max_workers=1) as executor,
    ):
        server.get_pool()
        hub = server.get_workers()
        worker = MockWorker(f"http://127.0.0.1:{server.server_port}")
        worker.timeout = 6
        status, registered = worker.register("bench-owner")
        if status != 200:
            raise RuntimeError(f"模擬ワーカーを登録できませんでした (HTTP {status})")
        info = hub.registry.info(registered["worker_id"])
        for _ in range(hub.checks.health.probation_jobs):
            hub.checks.health.record(info, passed=True)
        hub.poll_seconds = 5
        # 空き枠の配信経路を測るため、測定中は確認用ジョブと抜き取りを動かさない。
        with patch.object(hub.checks, "after_delivery", return_value=False):
            volunteer_times = []
            for _ in range(count):
                task = executor.submit(deliver, worker, engine)
                with hub.condition:
                    if not hub.condition.wait_for(lambda: info["id"] in hub.ready, timeout=5):
                        raise RuntimeError("模擬ワーカーの待機を確認できませんでした")
                volunteer_times.append(first_fragment(server))
                task.result(timeout=8)
    return direct_times, volunteer_times


def p95(values):
    return sorted(values)[math.ceil(0.95 * len(values)) - 1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--samples", type=int, default=30)
    args = parser.parse_args()
    if not 1 <= args.samples <= 100:
        parser.error("--samples は 1〜100 を指定してください")
    engine, _ = make_server(delay=0.01)
    with serving(engine), tempfile.TemporaryDirectory() as directory:
        url = f"http://127.0.0.1:{engine.server_port}/v1/completions"
        direct, volunteer = samples(args.samples, url, Path(directory))
    print(
        json.dumps(
            {
                "samples_per_pool": args.samples,
                "direct_p95_ms": round(p95(direct), 2),
                "volunteer_p95_ms": round(p95(volunteer), 2),
                "p95_delta_ms": round(p95(volunteer) - p95(direct), 2),
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
