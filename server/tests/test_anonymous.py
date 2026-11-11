"""匿名の資格、入口の制限、層の順番をローカルで検証する。"""

import http.client
import json
import socket
import threading
import time
import unittest
from unittest.mock import patch

from test_translator import tr, mock_plamo


class AnonymousTest(unittest.TestCase):
    def setUp(self):
        self.mock, self.stats = mock_plamo.make_server(api_key="test", delay=0)
        self.settings = patch.multiple(
            tr,
            ANON_ENABLED=True,
            TOKEN_KEY=b"test-key-only-not-for-deployment-32",
            ANON_ISSUE_PER_MINUTE=100,
            ANON_SHARE=0.5,
            COUNTRY_HEADER="X-Test-Country",
            ALLOWED_COUNTRIES=frozenset({"JP"}),
            CLIENT_IP_HEADER="X-Test-IP",
            PUBLIC_SERVER_URL="https://translate.example.com",
            UPSTREAM_URL=f"http://127.0.0.1:{self.mock.server_port}/v1/completions",
            UPSTREAMS="",
            API_KEY="test",
        )
        self.settings.start()
        self.app = tr.TranslationHTTPServer(("127.0.0.1", 0), tr.Handler)
        self.app.daemon_threads = True
        self.connections = []
        for server in (self.mock, self.app):
            threading.Thread(target=server.serve_forever, daemon=True).start()

    def tearDown(self):
        for conn in self.connections:
            conn.close()
        for server in (self.app, self.mock):
            server.shutdown()
            server.server_close()
        self.settings.stop()

    def call(self, method, path, body=None, headers=None, network="192.0.2.1"):
        conn = http.client.HTTPConnection("127.0.0.1", self.app.server_port, timeout=5)
        self.connections.append(conn)
        conn.request(
            method,
            path,
            json.dumps(body) if body is not None else None,
            {
                "Content-Type": "application/json",
                "X-Test-IP": network,
                "X-Test-Country": "JP",
                **(headers or {}),
            },
        )
        response = conn.getresponse()
        data = response.read()
        return response, data

    def issue(self, network="192.0.2.1"):
        response, data = self.call("POST", "/api/anonymous", {}, network=network)
        self.assertEqual(response.status, 200, data)
        return json.loads(data)["credential"]

    def test_bearer_translation_and_private_logs(self):
        token = self.issue()
        headers = {"Authorization": f"Bearer {token}"}
        response, data = self.call("GET", "/api/config", headers=headers)
        config = json.loads(data)
        self.assertEqual(response.status, 200)
        self.assertEqual(config["tier"], "anonymous")
        self.assertEqual(config["user"], "")
        self.assertEqual(config["concurrency"]["max_in_flight"], 1)
        with self.assertLogs(tr.log, "INFO") as logs:
            response, result = self.call(
                "POST", "/api/translate", {"text": "private source", "target_lang": "ja"}, headers
            )
        self.assertEqual(response.status, 200)
        self.assertIn(b'"done": true', result)
        for private in (token, "private source", "192.0.2", "pta_"):
            self.assertNotIn(private, "\n".join(logs.output))
        self.assertFalse(hasattr(self.app, "credentials"))

    def test_signature_expiry_disabled_and_worker_credentials(self):
        token = self.issue()
        with patch.object(tr, "TOKEN_KEY", b"another-key-for-testing-only-32bytes"):
            bad_key = tr.anonymous_credential()
        with patch.object(tr.time, "time", return_value=time.time() - tr.ANON_TTL - 1):
            expired = tr.anonymous_credential()
        for credential in (
            token[:-1] + ("0" if token[-1] != "0" else "1"),
            bad_key,
            expired,
            "ptw_invalid",
            "pta_invalid",
        ):
            with self.subTest(credential=credential[:4]):
                response, _ = self.call(
                    "GET", "/api/config", headers={"Authorization": f"Bearer {credential}"}
                )
                self.assertEqual(response.status, 401)
        with patch.object(tr, "ANON_ENABLED", False):
            response, _ = self.call("POST", "/api/anonymous", {})
            self.assertEqual(response.status, 404)
            response, _ = self.call(
                "GET", "/api/config", headers={"Authorization": f"Bearer {token}"}
            )
            self.assertEqual(response.status, 401)
        with (
            patch.object(tr, "USERINFO_URL", "http://unused.example.com"),
            patch.object(tr, "user_from_userinfo") as userinfo,
        ):
            response, _ = self.call(
                "GET", "/api/config", headers={"Authorization": "Bearer pta_invalid"}
            )
            self.assertEqual(response.status, 401)
            userinfo.assert_not_called()
        response, data = self.call(
            "GET",
            "/api/config",
            headers={"Authorization": f"Bearer {token}", "X-Forwarded-User": "alice"},
        )
        self.assertEqual(json.loads(data)["tier"], "registered")

    def test_cookie_and_origin(self):
        origin = {"Origin": tr.PUBLIC_SERVER_URL}
        response, data = self.call("POST", "/api/anonymous", {}, origin)
        self.assertNotIn("credential", json.loads(data))
        cookie = response.getheader("Set-Cookie")
        for attribute in ("__Host-pta=", "HttpOnly", "Secure", "SameSite=Strict", "Path=/"):
            self.assertIn(attribute, cookie)
        cookie_header = {"Cookie": cookie.split(";", 1)[0]}
        for headers in (cookie_header, {**cookie_header, "Origin": "https://evil.example.com"}):
            response, _ = self.call("GET", "/api/config", headers=headers)
            self.assertEqual(response.status, 403)
        for site in ("cross-site", "same-site"):
            response, _ = self.call(
                "GET", "/api/config", headers={**cookie_header, "Sec-Fetch-Site": site}
            )
            self.assertEqual(response.status, 403)
        response, _ = self.call(
            "GET", "/api/config", headers={**cookie_header, "Sec-Fetch-Site": "same-origin"}
        )
        self.assertEqual(response.status, 200)
        response, _ = self.call(
            "POST",
            "/api/translate",
            {"text": "test", "target_lang": "ja"},
            {**cookie_header, "Sec-Fetch-Site": "same-origin"},
        )
        self.assertEqual(response.status, 403)
        response, _ = self.call("GET", "/api/config", headers={**cookie_header, **origin})
        self.assertEqual(response.status, 200)
        response, _ = self.call(
            "POST", "/api/anonymous", {}, {"Origin": "https://evil.example.com"}
        )
        self.assertEqual(response.status, 403)
        response, _ = self.call(
            "POST", "/api/anonymous", {}, {"Content-Type": "application/json-fake"}
        )
        self.assertEqual(response.status, 415)

    def test_extension_issuance_and_no_cors(self):
        response, data = self.call(
            "POST", "/api/anonymous", {}, {"Origin": "chrome-extension://test-id"}
        )
        self.assertEqual(response.status, 200)
        self.assertIn("credential", json.loads(data))
        self.assertIsNone(response.getheader("Set-Cookie"))
        self.assertIsNone(response.getheader("Access-Control-Allow-Origin"))
        response, _ = self.call(
            "OPTIONS",
            "/api/anonymous",
            headers={
                "Origin": "https://evil.example",
                "Access-Control-Request-Method": "POST",
                "Access-Control-Request-Headers": "content-type",
            },
        )
        self.assertEqual(response.status, 501)
        self.assertFalse(
            any(name.lower().startswith("access-control-") for name, _ in response.getheaders())
        )

    def test_anonymous_capacity_does_not_block_registered(self):
        self.app.character_limits_anonymous = tr.WindowCounter(3600, capacity=1)
        self.app.character_limits_anonymous.charge("filled", 1, 100000)
        token = self.issue()
        response, _ = self.call(
            "POST",
            "/api/translate",
            {"text": "test", "target_lang": "ja"},
            {"Authorization": f"Bearer {token}"},
        )
        self.assertEqual(response.status, 503)
        self.assertFalse(self.app.get_pool().waiting)
        response, data = self.call(
            "POST",
            "/api/translate",
            {"text": "test", "target_lang": "ja"},
            {"X-Forwarded-User": "new-user"},
        )
        self.assertEqual(response.status, 200)
        self.assertIn(b'"done": true', data)
        self.assertIn("new-user", self.app.character_limits_registered.entries)

    def test_internal_error_logs_type_and_safe_path_only(self):
        with (
            patch.object(
                tr.Handler, "user", side_effect=ValueError("private-source private-token 192.0.2.1")
            ),
            self.assertLogs(tr.log, "ERROR") as logs,
        ):
            response, _ = self.call("GET", "/api/config?private-query")
            self.assertEqual(response.status, 500)
            response, _ = self.call("GET", "/api/private-token")
            self.assertEqual(response.status, 500)
        self.assertEqual(
            logs.output,
            [
                "ERROR:translator:request GET /api/config failed: ValueError",
                "ERROR:translator:request GET <unknown> failed: ValueError",
            ],
        )

    def test_issuance_ip_global_and_region(self):
        for suffix in range(1, 6):
            self.issue(f"192.0.2.{suffix}")
        response, _ = self.call("POST", "/api/anonymous", {}, network="192.0.2.99")
        self.assertEqual(response.status, 429)
        for suffix in range(1, 6):
            self.issue(f"2001:db8:1::{suffix}")
        response, _ = self.call("POST", "/api/anonymous", {}, network="2001:db8:1:abcd::1")
        self.assertEqual(response.status, 429)
        with patch.object(tr, "ANON_ISSUE_PER_MINUTE", 1):
            response, _ = self.call("POST", "/api/anonymous", {}, network="198.51.100.1")
            self.assertEqual(response.status, 503)
            self.assertEqual(response.getheader("Retry-After"), "5")
        for path in ("/api/anonymous", "/api/config", "/api/translate", "/api/worker/poll", "/"):
            response, data = self.call("POST", path, {}, {"X-Test-Country": "US"})
            self.assertEqual(response.status, 403)
            self.assertEqual(json.loads(data)["code"], "region")
        response, _ = self.call("POST", "/api/anonymous", {}, network="invalid")
        self.assertEqual(response.status, 403)

    def test_character_limit_and_waiting_concurrency(self):
        token = self.issue()
        user = tr.verify_anonymous(token)
        headers = {"Authorization": f"Bearer {token}"}
        self.app.character_limits_anonymous.charge(user, 100000, 100000)
        response, _ = self.call(
            "POST", "/api/translate", {"text": "test", "target_lang": "ja"}, headers
        )
        self.assertEqual(response.status, 429)
        self.assertFalse(self.app.get_pool().waiting)
        token = self.issue()
        user = tr.verify_anonymous(token)
        with tr.active_lock:
            tr.active[user] = 1
        try:
            response, _ = self.call(
                "POST",
                "/api/translate",
                {"text": "test", "target_lang": "ja"},
                {"Authorization": f"Bearer {token}"},
            )
            self.assertEqual(response.status, 429)
        finally:
            tr.release_active(user)

    def test_bad_origin_configuration_and_identity_namespace(self):
        token = self.issue()
        self.assertFalse(tr.USER_RE.fullmatch(tr.verify_anonymous(token)))
        del self.app.pool  # 起動時と同じく、設定を未検証の状態で確かめる。
        for url in (
            "http://translate.example.com",
            "https://translate.example.com/path",
            "https://user@translate.example.com",
        ):
            with (
                self.subTest(url=url),
                patch.object(tr, "PUBLIC_SERVER_URL", url),
                self.assertRaises(ValueError),
            ):
                self.app.get_pool()

    def test_bad_key_fails_before_issuance(self):
        with patch.object(tr, "TOKEN_KEY", b"short"):
            with self.assertRaises(ValueError):
                self.app.get_pool()


class TierSchedulingTest(unittest.TestCase):
    def take(self, pool):
        job = pool._next()
        self.assertIsNotNone(job)
        return job, pool.acquire(job, threading.Event())

    def test_four_to_one_and_user_rotation(self):
        pool = tr.DirectPool([("http://localhost", 1)])
        for user in ("alice", "bob"):
            for _ in range(6):
                pool.submit(user, "interactive")
        for _ in range(4):
            pool.submit("anon", "interactive", "anonymous")
        order = []
        for _ in range(10):
            job, index = self.take(pool)
            order.append((job.tier, job.user))
            pool.release(job, index)
        self.assertEqual(
            [tier for tier, _ in order],
            ["registered"] * 4 + ["anonymous"] + ["registered"] * 4 + ["anonymous"],
        )
        self.assertEqual(
            [user for tier, user in order if tier == "registered"], ["alice", "bob"] * 4
        )

    def test_anonymous_share_borrowing_and_network_limit(self):
        with patch.object(tr, "ANON_SHARE", 0.5):
            pool = tr.DirectPool([("http://localhost", 10)])
            running = []
            for n in range(5):
                pool.submit(f"a{n}", "interactive", "anonymous", "192.0.2.0/24")
            for _ in range(4):
                running.append(self.take(pool))
            self.assertIsNone(pool._next())
            pool.submit("elsewhere", "interactive", "anonymous", "198.51.100.0/24")
            running.append(self.take(pool))
            pool.submit("borrow", "interactive", "anonymous", "198.51.100.0/24")
            running.append(self.take(pool))
            registered = pool.submit("alice", "interactive")
            self.assertIs(pool._next(), registered)
            reg_index = pool.acquire(registered, threading.Event())
            pool.submit("waiting", "interactive", "anonymous", "198.51.100.0/24")
            pool.submit("bob", "page_rest")
            self.assertEqual(pool._next().user, "bob")
            for job, index in running:
                pool.release(job, index)
            pool.release(registered, reg_index)
            self.assertEqual(pool.anonymous_running, 0)
            self.assertFalse(pool.network_running)

    def test_window_expiry_and_capacity(self):
        counter = tr.WindowCounter(3600, capacity=1)
        with patch.object(tr.time, "monotonic", return_value=100):
            counter.charge("user", 100, 100)
            with self.assertRaises(tr.HttpError):
                counter.charge("user", 1, 100)
            with self.assertRaises(tr.HttpError) as error:
                counter.charge("another", 1, 100)
            self.assertEqual(error.exception.status, 503)
        with patch.object(tr.time, "monotonic", return_value=3701):
            counter.charge("another", 1, 100)
        self.assertNotIn("user", counter.entries)

    def test_open_request_limit(self):
        with patch.object(tr, "MAX_OPEN_REQUESTS", 1):
            server = tr.TranslationHTTPServer(("127.0.0.1", 0), tr.Handler)
        server.daemon_threads = True
        threading.Thread(target=server.serve_forever, daemon=True).start()
        first = socket.create_connection(server.server_address)
        second = None
        try:
            # 先の接続のスレッドが枠を取るまで待つ。
            deadline = time.monotonic() + 2
            while server.open_requests._value and time.monotonic() < deadline:
                time.sleep(0.01)
            second = socket.create_connection(server.server_address)
            second.settimeout(2)
            self.assertIn(b"503 Service Unavailable", second.recv(4096))
        finally:
            first.close()
            if second:
                second.close()
            server.shutdown()
            server.server_close()
