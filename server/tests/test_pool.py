"""直結プールの割り当てと待ち行列を検証する。"""

import http.client
import json
import queue
import threading
import time
import unittest
from unittest.mock import MagicMock, patch

from test_translator import tr, mock_plamo


class PoolTest(unittest.TestCase):
    def test_configuration(self):
        self.assertEqual(
            tr.parse_upstreams("", "http://localhost/v1/completions"),
            [("http://localhost/v1/completions", 4)],
        )
        for value in [
            [],
            {},
            [{"url": "ftp://localhost", "slots": 1}],
            [{"url": "http://localhost", "slots": True}],
            [{"url": "http://localhost", "slots": 0}],
        ]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                tr.parse_upstreams(json.dumps(value), "")

    def test_default_four_slots_and_explicit_override(self):
        pool = tr.DirectPool(tr.parse_upstreams("", "http://localhost"))
        jobs = [pool.submit(str(i), "interactive") for i in range(4)]
        for job in jobs:
            self.assertEqual(pool.acquire(job, threading.Event()), 0)
        self.assertEqual(pool.free, [0])
        for slots in ("0", "1001", "x", "1.5"):
            with self.subTest(slots=slots), self.assertRaises(ValueError):
                tr.parse_upstreams("", "http://localhost", slots)
        self.assertEqual(
            tr.parse_upstreams('[{"url":"http://localhost","slots":2}]', "", "invalid"),
            [("http://localhost", 2)],
        )

    def test_priority_fairness_and_aging(self):
        pool = tr.DirectPool([("http://localhost", 1)])
        rest = pool.submit("alice", "page_rest")
        first = pool.submit("alice", "interactive")
        second = pool.submit("alice", "interactive")
        other = pool.submit("bob", "interactive")
        cancel = threading.Event()
        self.assertIs(pool._next(), first)
        index = pool.acquire(first, cancel)
        pool.release(first, index)
        self.assertIs(pool._next(), other)
        index = pool.acquire(other, cancel)
        pool.release(other, index)
        rest.created -= 61
        self.assertIs(pool._next(), second)
        self.assertIn(second, pool.waiting)

    def test_capacity_cancel_timeout_and_page_limit(self):
        pool = tr.DirectPool([("http://a", 1), ("http://b", 2)], max_queue=1)
        job = pool.submit("alice", "page_rest")
        self.assertIsNone(pool.submit("bob", "interactive"))
        index = pool.acquire(job, threading.Event())
        waiting = pool.submit("alice", "page_visible")
        self.assertIsNone(pool._next())
        pool.release(job, index)
        index = pool.acquire(waiting, threading.Event())
        pool.release(waiting, index)
        canceled = pool.submit("bob", "interactive")
        cancel = threading.Event()
        cancel.set()
        self.assertIsNone(pool.acquire(canceled, cancel))
        expired = pool.submit("bob", "interactive")
        expired.created -= 121
        with self.assertRaises(TimeoutError):
            pool.acquire(expired, threading.Event())
        self.assertFalse(pool.waiting)
        self.assertEqual(sum(pool.free), 3)

    def test_three_users_take_turns(self):
        pool = tr.DirectPool([("http://localhost", 1)])
        for user in ("alice", "alice", "bob", "bob", "carol"):
            pool.submit(user, "interactive")
        order = []
        while pool.waiting:
            job = pool._next()
            order.append(job.user)
            index = pool.acquire(job, threading.Event())
            pool.release(job, index)
        self.assertEqual(order, ["alice", "bob", "carol", "alice", "bob"])

    def test_missing_finish_is_an_error(self):
        response = MagicMock()
        response.status = 200
        response.__iter__.return_value = iter(
            [b'data: {"choices":[{"text":"partial"}]}\n', b"data: [DONE]\n"]
        )
        conn = MagicMock()
        conn.getresponse.return_value = response
        events = queue.Queue()
        with (
            patch.object(tr.http.client, "HTTPConnection", return_value=conn),
            patch.object(tr, "UPSTREAM_URL", "http://localhost/v1/completions"),
        ):
            tr.upstream_stream("test", 256, events, tr.UpstreamCancellation(), tr.UPSTREAM_URL)
        self.assertEqual([kind for kind, _ in events.queue], ["text", "error"])

    def test_upstream_error_body_is_not_logged(self):
        response = MagicMock()
        response.status = 500
        response.read.return_value = b"private-source private-translation private-token"
        conn = MagicMock()
        conn.getresponse.return_value = response
        with (
            patch.object(tr.http.client, "HTTPConnection", return_value=conn),
            self.assertLogs(tr.log, level="WARNING") as logs,
        ):
            tr.upstream_stream(
                "private-source",
                256,
                queue.Queue(),
                tr.UpstreamCancellation(),
                "http://localhost/v1/completions",
            )
        self.assertEqual(logs.output, ["WARNING:translator:upstream returned 500"])
        response.read.assert_not_called()

    def test_page_counter_is_held_until_upstream_finishes(self):
        pool = tr.DirectPool([("http://localhost", 1)])
        user = "page-finish-test"
        job = pool.submit(user, "page_rest")
        entered, finish = threading.Event(), threading.Event()

        def upstream(*args):
            entered.set()
            finish.wait(3)

        with tr.active_lock:
            tr.active[user] = tr.page_active[user] = 1
        with patch.object(tr, "upstream_stream", side_effect=upstream):
            worker = threading.Thread(
                target=tr.run_upstream_for_user,
                args=(user, "test", 256, queue.Queue(), tr.UpstreamCancellation(), pool, job),
            )
            worker.start()
            try:
                self.assertTrue(entered.wait(2))
                self.assertEqual(tr.page_active[user], 1)
            finally:
                finish.set()
                worker.join(3)
        self.assertNotIn(user, tr.page_active)
        self.assertNotIn(user, tr.active)
        self.assertEqual(pool.free, [1])

    def test_two_mock_engines(self):
        servers = [mock_plamo.make_server(api_key="test", delay=0) for _ in range(2)]
        try:
            for server, _ in servers:
                threading.Thread(target=server.serve_forever, daemon=True).start()
            pool = tr.DirectPool(
                [
                    (f"http://127.0.0.1:{server.server_port}/v1/completions", 1)
                    for server, _ in servers
                ]
            )
            with patch.object(tr, "API_KEY", "test"):
                for user in ("alice", "bob"):
                    job = pool.submit(user, "interactive")
                    with tr.active_lock:
                        tr.active[user] = tr.active.get(user, 0) + 1
                    events = queue.Queue()
                    tr.run_upstream_for_user(
                        user,
                        tr.build_prompt(tr.parse_request({"text": "test", "target_lang": "ja"})),
                        256,
                        events,
                        tr.UpstreamCancellation(),
                        pool,
                        job,
                    )
                    self.assertIn(("done", "stop"), list(events.queue))
            self.assertEqual([stats["requests"] for _, stats in servers], [1, 1])
        finally:
            for server, _ in servers:
                server.shutdown()
                server.server_close()


class PoolApiTest(unittest.TestCase):
    def test_busy_timeout_and_validation(self):
        pool = tr.DirectPool([("http://127.0.0.1:1/v1/completions", 1)], timeout=0.2, max_queue=1)
        held = pool.submit("holder", "interactive")
        index = pool.acquire(held, threading.Event())
        app = tr.TranslationHTTPServer(("127.0.0.1", 0), tr.Handler)
        app.daemon_threads = True
        app.pool = pool
        threading.Thread(target=app.serve_forever, daemon=True).start()
        connections = []

        def request(body):
            conn = http.client.HTTPConnection("127.0.0.1", app.server_port, timeout=3)
            connections.append(conn)
            conn.request(
                "POST",
                "/api/translate",
                json.dumps(body),
                {"X-Forwarded-User": "alice", "Content-Type": "application/json"},
            )
            return conn.getresponse()

        try:
            waiting = pool.submit("other", "interactive")
            response = request({"text": "test", "target_lang": "ja"})
            self.assertEqual(response.status, 503)
            self.assertEqual(response.getheader("Retry-After"), "5")
            self.assertEqual(json.loads(response.read())["code"], "busy")
            cancel = threading.Event()
            cancel.set()
            pool.acquire(waiting, cancel)
            response = request({"text": "test", "target_lang": "ja"})
            self.assertEqual(response.status, 200)
            self.assertIn('"code": "queue_timeout"', response.read().decode())
            self.assertFalse(pool.waiting)
            for extra in ({"purpose": []}, {"group": "https://example.com"}):
                response = request({"text": "test", "target_lang": "ja", **extra})
                self.assertEqual(response.status, 400)
                response.read()
        finally:
            pool.release(held, index)
            for conn in connections:
                conn.close()
            app.shutdown()
            app.server_close()

    def test_waiting_page_reserves_interactive_capacity(self):
        pool = tr.DirectPool([("http://127.0.0.1:1/v1/completions", 1)], timeout=0.4)
        held = pool.submit("holder", "interactive")
        index = pool.acquire(held, threading.Event())
        app = tr.TranslationHTTPServer(("127.0.0.1", 0), tr.Handler)
        app.daemon_threads = True
        app.pool = pool
        threading.Thread(target=app.serve_forever, daemon=True).start()
        connections = []
        user = "pool-page-test"

        def request(purpose):
            conn = http.client.HTTPConnection("127.0.0.1", app.server_port, timeout=3)
            connections.append(conn)
            conn.request(
                "POST",
                "/api/translate",
                json.dumps({"text": "test", "target_lang": "ja", "purpose": purpose}),
                {"X-Forwarded-User": user, "Content-Type": "application/json"},
            )
            return conn.getresponse()

        try:
            with patch.object(tr, "MAX_ACTIVE_PER_USER", 2):
                page = request("page_rest")
                self.assertEqual(page.status, 200)
                rejected = request("page_rest")
                self.assertEqual(rejected.status, 429)
                rejected.read()
                interactive = request("interactive")
                self.assertEqual(interactive.status, 200)
                self.assertIn("queue_timeout", page.read().decode())
                self.assertIn("queue_timeout", interactive.read().decode())
                deadline = time.monotonic() + 2
                while user in tr.active and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertNotIn(user, tr.active)
                self.assertNotIn(user, tr.page_active)
                with patch.object(
                    tr.threading.Thread, "start", side_effect=RuntimeError("開始失敗")
                ):
                    # HTTPサーバーのスレッド開始には影響させず、受け付けだけを直接検証する。
                    handler = object.__new__(tr.Handler)
                    handler.server = app
                    handler.read_json_body = lambda: {
                        "text": "test",
                        "target_lang": "ja",
                        "purpose": "page_rest",
                    }
                    with self.assertRaises(RuntimeError):
                        handler.translate(user)
                self.assertNotIn(user, tr.active)
                self.assertNotIn(user, tr.page_active)
                self.assertFalse(pool.waiting)
                pool.max_queue = 0
                with self.assertRaises(tr.HttpError) as error:
                    handler.translate(user)
                self.assertEqual(error.exception.status, 503)
                self.assertNotIn(user, tr.active)
                self.assertNotIn(user, tr.page_active)
        finally:
            pool.release(held, index)
            for conn in connections:
                conn.close()
            app.shutdown()
            app.server_close()
