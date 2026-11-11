#!/usr/bin/env python3
"""最初の翻訳断片を、ローカルのモックで直結とボランティア枠で比較する。"""

import argparse
import asyncio
from collections import Counter
from concurrent.futures import Future, ThreadPoolExecutor
from contextlib import contextmanager, nullcontext
import http.client
import json
import math
from pathlib import Path
import queue
import select
import socket
import ssl
import subprocess
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


def tls_contexts(state_dir):
    config = state_dir / "tls.cnf"
    config.write_text(
        "[req]\ndistinguished_name=subject\nx509_extensions=extensions\n"
        "[subject]\n[extensions]\nsubjectAltName=IP:127.0.0.1\n",
        encoding="ascii",
    )
    certificate = state_dir / "tls.crt"
    key = state_dir / "tls.key"
    subprocess.run(
        [
            "openssl",
            "req",
            "-new",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=localhost",
            "-config",
            str(config),
            "-out",
            str(certificate),
            "-keyout",
            str(key),
        ],
        check=True,
        capture_output=True,
        timeout=15,
    )
    server = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    server.load_cert_chain(certificate, key)
    return server, ssl.create_default_context(cafile=str(certificate))


class BenchmarkHandler(tr.Handler):
    def client_disconnected(self):
        if not isinstance(self.connection, ssl.SSLSocket):
            return super().client_disconnected()
        readable, _, _ = select.select([self.connection], [], [], 0)
        if not readable and not self.connection.pending():
            return False
        # SSLSocket は MSG_PEEK に対応しない。計測クライアントは応答を読み終えてから次の要求を送る。
        timeout = self.connection.gettimeout()
        try:
            self.connection.setblocking(False)
            return not self.connection.recv(1)
        except (ssl.SSLWantReadError, ssl.SSLWantWriteError):
            return False
        except OSError:
            return True
        finally:
            self.connection.settimeout(timeout)


class BenchmarkBroker(tr.TranslationHTTPServer):
    def __init__(self, server_tls, client_tls):
        super().__init__(("127.0.0.1", 0), BenchmarkHandler)
        self.server_tls = server_tls
        self.client_tls = client_tls

    def get_request(self):
        connection, address = super().get_request()
        try:
            connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            if self.server_tls is not None:
                # TLS の交渉は各要求のスレッドで行う。
                connection = self.server_tls.wrap_socket(
                    connection, server_side=True, do_handshake_on_connect=False
                )
        except (OSError, ValueError):
            connection.close()
            raise
        return connection, address


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


async def delay_stream(reader, writer, delay):
    clock = asyncio.get_running_loop()
    pending = asyncio.Queue(maxsize=16)

    async def read():
        while True:
            data = await reader.read(16 * 1024)
            await pending.put((clock.time() + delay, data))
            if not data:
                return

    async def write():
        while True:
            due, data = await pending.get()
            await asyncio.sleep(max(0, due - clock.time()))
            if not data:
                writer.write_eof()
                return
            writer.write(data)
            await writer.drain()

    async with asyncio.TaskGroup() as tasks:
        tasks.create_task(read())
        tasks.create_task(write())


@contextmanager
def delayed_worker_port(broker_port, rtt_ms):
    if not rtt_ms:
        yield broker_port
        return
    ready = Future()
    delay = rtt_ms / 2000

    async def relay(client_reader, client_writer):
        broker_writer = None
        try:
            broker_reader, broker_writer = await asyncio.open_connection("127.0.0.1", broker_port)
            async with asyncio.TaskGroup() as tasks:
                tasks.create_task(delay_stream(client_reader, broker_writer, delay))
                tasks.create_task(delay_stream(broker_reader, client_writer, delay))
        except* OSError:
            # 計測クライアントが接続を閉じた場合は、両側の接続を回収する。
            pass
        finally:
            client_writer.close()
            if broker_writer is not None:
                broker_writer.close()
                await asyncio.gather(
                    client_writer.wait_closed(), broker_writer.wait_closed(), return_exceptions=True
                )
            else:
                await asyncio.gather(client_writer.wait_closed(), return_exceptions=True)

    async def run():
        stopped = asyncio.Event()
        try:
            listener = await asyncio.start_server(relay, "127.0.0.1", 0)
        except Exception as error:
            ready.set_exception(error)
            return
        async with listener:
            ready.set_result(
                (listener.sockets[0].getsockname()[1], asyncio.get_running_loop(), stopped)
            )
            await stopped.wait()

    with ThreadPoolExecutor(max_workers=1) as executor:
        task = executor.submit(asyncio.run, run())
        port, loop, stopped = ready.result()
        try:
            yield port
        finally:
            loop.call_soon_threadsafe(stopped.set)
            task.result()


def first_fragment(server, index=0):
    if server.client_tls is None:
        conn = http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=8)
    else:
        conn = http.client.HTTPSConnection(
            "127.0.0.1", server.server_port, timeout=8, context=server.client_tls
        )
    try:
        started = time.perf_counter()
        conn.request(
            "POST",
            "/api/translate",
            json.dumps({**INPUT, "consent": tr.CONSENT_VERSION}),
            {
                "Content-Type": "application/json",
                "X-Forwarded-User": f"bench-reader-{index}",
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


def deliver(worker, engine, *, background=False, reuse_connection=False):
    with worker.reuse_connection() if reuse_connection else nullcontext():
        deliver_assignment(worker, engine, background)


def deliver_assignment(worker, engine, background):
    status, assignment = worker.poll(MODEL)
    if background and status in (204, 409):
        return
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


def concurrent_samples(server, count, concurrency):
    with ThreadPoolExecutor(max_workers=concurrency) as executor:
        return list(executor.map(lambda index: first_fragment(server, index), range(count)))


def register_worker(server, hub, index, port):
    scheme = "https" if server.client_tls is not None else "http"
    worker = MockWorker(
        f"{scheme}://127.0.0.1:{port}",
        network=f"192.0.2.{index + 1}",
        tls_context=server.client_tls,
    )
    worker.timeout = 6
    status, registered = worker.register(f"bench-owner-{index}")
    if status != 200:
        raise RuntimeError(f"模擬ワーカーを登録できませんでした (HTTP {status})")
    info = hub.registry.info(registered["worker_id"])
    for _ in range(hub.checks.health.probation_jobs):
        hub.checks.health.record(info, passed=True)
    return worker


def samples_with_checks(server, hub, count, concurrency, engine, worker_port, reuse_connection):
    # 検査中のワーカーがいても、同時送信に必要な枠を確保する。
    workers = [
        register_worker(server, hub, index, worker_port) for index in range(max(5, concurrency * 2))
    ]
    stop = threading.Event()
    completed = threading.Condition()
    checks = Counter()
    record = hub.checks.health.record

    def record_check(worker, passed=None, timed_out=False, canary=True):
        record(worker, passed=passed, timed_out=timed_out, canary=canary)
        with completed:
            checks[("canary" if canary else "audit") if passed else "failed"] += 1
            completed.notify_all()

    def serve_worker(worker):
        while not stop.is_set():
            deliver(worker, engine, background=True, reuse_connection=reuse_connection)
            # 終了通知を受けた割り当てを仲介が回収する時間を取る。
            stop.wait(0.01)

    with (
        patch.object(hub.checks.health, "record", record_check),
        ThreadPoolExecutor(max_workers=len(workers)) as executor,
    ):
        tasks = [executor.submit(serve_worker, worker) for worker in workers]
        try:
            with hub.condition:
                if not hub.condition.wait_for(lambda: len(hub.ready) == len(workers), timeout=5):
                    raise RuntimeError("模擬ワーカーの待機を確認できませんでした")
            times = concurrent_samples(server, count, concurrency)
            with completed:
                during_delivery = {
                    "canaries_passed_during_delivery": checks["canary"],
                    "audits_passed_during_delivery": checks["audit"],
                }
                if not completed.wait_for(
                    lambda: checks["failed"] or (checks["audit"] == count and checks["canary"]),
                    timeout=10,
                ):
                    raise RuntimeError("確認用ジョブと抜き取りの完了を確認できませんでした")
                if checks["failed"]:
                    raise RuntimeError("模擬ワーカーの検査に失敗しました")
        finally:
            # 処理中の割り当ては完了させ、次のポーリングを始めずに抜ける。
            stop.set()
        for task in tasks:
            task.result()
        with completed:
            if checks["failed"]:
                raise RuntimeError("模擬ワーカーの検査に失敗しました")
            totals = {
                **during_delivery,
                "canaries_passed": checks["canary"],
                "audits_passed": checks["audit"],
            }
    return times, totals


def samples(
    count,
    engine,
    state_dir,
    concurrency=1,
    with_checks=False,
    *,
    tls=False,
    worker_rtt_ms=0,
    reuse_connection=False,
):
    server_tls, client_tls = tls_contexts(state_dir) if tls else (None, None)
    canary = state_dir / "canary.json"
    canary.write_text(
        json.dumps(
            [{"text": f"Synthetic canary {index}.", "target_lang": "en"} for index in range(count)]
        ),
        encoding="utf-8",
    )
    canary.chmod(0o600)
    settings = {
        "UPSTREAM_URL": engine,
        "UPSTREAMS": "",
        "UPSTREAM_SLOTS": concurrency,
        "STATE_FILE": str(state_dir / "worker.sqlite"),
        "WORKER_MODEL_SHA256": MODEL,
        "MIN_AGENT_VERSION": "0.5.0",
        "COUNTRY_HEADER": "X-Test-Country",
        "CLIENT_IP_HEADER": "X-Test-IP",
        "ALLOWED_COUNTRIES": frozenset({"JP"}),
        "ANON_ENABLED": False,
        "API_KEY": "",
        "CANARY_FILE": str(canary),
        "AUDIT_UPSTREAMS": json.dumps([{"url": engine, "slots": concurrency}]),
        "CANARY_RATE": "1",
        "AUDIT_RATE": "1",
        "AUDIT_BACKLOG": str(count),
        "QUEUE_TIMEOUT": 4,
    }
    with (
        patch.multiple(tr, POOL_KIND="direct", **settings),
        serving(BenchmarkBroker(server_tls, client_tls)) as direct,
    ):
        direct.get_pool()
        if with_checks:
            direct_times = concurrent_samples(direct, count, concurrency)
        else:
            direct_times = [first_fragment(direct) for _ in range(count)]
    with (
        patch.multiple(tr, POOL_KIND="volunteer", **settings),
        serving(BenchmarkBroker(server_tls, client_tls)) as server,
        delayed_worker_port(server.server_port, worker_rtt_ms) as worker_port,
        ThreadPoolExecutor(max_workers=1) as executor,
    ):
        server.get_pool()
        hub = server.get_workers()
        hub.poll_seconds = 5
        if with_checks:
            volunteer_times, totals = samples_with_checks(
                server, hub, count, concurrency, engine, worker_port, reuse_connection
            )
            return direct_times, volunteer_times, totals
        worker = register_worker(server, hub, 0, worker_port)
        # 空き枠の配信経路を測るため、測定中は確認用ジョブと抜き取りを動かさない。
        with patch.object(hub.checks, "after_delivery", return_value=False):
            volunteer_times = []
            for _ in range(count):
                task = executor.submit(deliver, worker, engine, reuse_connection=reuse_connection)
                with hub.condition:
                    if not hub.condition.wait_for(lambda: bool(hub.ready), timeout=5):
                        raise RuntimeError("模擬ワーカーの待機を確認できませんでした")
                volunteer_times.append(first_fragment(server))
                task.result(timeout=8)
    return direct_times, volunteer_times, {}


def p95(values):
    return sorted(values)[math.ceil(0.95 * len(values)) - 1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument("--concurrency", type=int, default=1)
    parser.add_argument("--with-checks", action="store_true")
    parser.add_argument(
        "--tls", action="store_true", help="仲介への接続をHTTPSにする（openssl が必要）"
    )
    parser.add_argument(
        "--worker-rtt-ms",
        type=int,
        default=0,
        help="仲介・ワーカー間のTCPストリームの往復に足す模擬遅延（ms）",
    )
    parser.add_argument(
        "--reuse-worker-connection",
        action="store_true",
        help="ワーカーが割り当ての取得と結果の送信に同じ接続を使う",
    )
    args = parser.parse_args()
    if not 1 <= args.samples <= 100:
        parser.error("--samples は 1〜100 を指定してください")
    if not 1 <= args.concurrency <= min(8, args.samples):
        parser.error("--concurrency は 1〜8 かつ --samples 以下を指定してください")
    if not 0 <= args.worker_rtt_ms <= 200:
        parser.error("--worker-rtt-ms は 0〜200 を指定してください")
    engine, _ = make_server(delay=0.01)
    with serving(engine), tempfile.TemporaryDirectory() as directory:
        url = f"http://127.0.0.1:{engine.server_port}/v1/completions"
        try:
            direct, volunteer, totals = samples(
                args.samples,
                url,
                Path(directory),
                args.concurrency,
                args.with_checks or args.concurrency > 1,
                tls=args.tls,
                worker_rtt_ms=args.worker_rtt_ms,
                reuse_connection=args.reuse_worker_connection,
            )
        except (OSError, subprocess.SubprocessError, RuntimeError) as error:
            parser.exit(
                1,
                f"測定できませんでした ({error if isinstance(error, RuntimeError) else type(error).__name__})\n",
            )
    print(
        json.dumps(
            {
                "samples_per_pool": args.samples,
                "concurrency": args.concurrency,
                "tls": args.tls,
                "worker_rtt_ms": args.worker_rtt_ms,
                "reuse_worker_connection": args.reuse_worker_connection,
                "direct_p95_ms": round(p95(direct), 2),
                "volunteer_p95_ms": round(p95(volunteer), 2),
                "p95_delta_ms": round(p95(volunteer) - p95(direct), 2),
                **totals,
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
