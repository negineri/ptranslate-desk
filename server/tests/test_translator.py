"""server/translator.py の試験。標準ライブラリだけで動く。

python3 -m unittest discover -s server/tests -v
"""

from __future__ import annotations

import base64
import http.client
import importlib.util
import json
import sys
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))

import mock_plamo  # noqa: E402

spec = importlib.util.spec_from_file_location("translator", ROOT / "server" / "translator.py")
tr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tr)


def req(**kw):
    body = {"text": "こんにちは", "target_lang": "en"}
    body.update(kw)
    return body


class PromptTest(unittest.TestCase):
    def test_all_options_match_the_model_card(self):
        # モデルカードの "All options" の例 (history、context、glossary を全部使う)
        got = tr.build_prompt(
            tr.parse_request(
                {
                    "text": "オーナーでフィルタリングすることもできます。",
                    "source_lang": "ja",
                    "target_lang": "en",
                    "context": "プロジェクト管理ツールのヘルプページです。",
                    "history": [
                        {
                            "source": "プロジェクトの管理画面を開きます。",
                            "target": "Open the project dashboard.",
                        },
                        {
                            "source": "進行中のタスクがすべて表示されます。",
                            "target": "It lists all active tasks.",
                        },
                    ],
                    "glossary": [{"source": "オーナー", "target": "assignee"}],
                }
            )
        )
        want = (
            "<|plamo:tag|>dataset<|plamo:msg|>translation"
            "<|plamo:tag|>context<|plamo:msg|>プロジェクト管理ツールのヘルプページです。"
            "<|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>プロジェクトの管理画面を開きます。"
            "<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>Open the project dashboard."
            "<|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>進行中のタスクがすべて表示されます。"
            "<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>It lists all active tasks."
            "<|plamo:tag|>glossary<|plamo:index|>オーナー<|plamo:item|>assignee"
            "<|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>オーナーでフィルタリングすることもできます。"
            "<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>"
        )
        self.assertEqual(got, want)

    def test_form_is_after_the_target_language(self):
        got = tr.build_prompt(
            tr.parse_request(req(text="This system is convenient.", target_lang="ja", form="plain"))
        )
        self.assertTrue(
            got.endswith(
                "<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>ja<|plamo:key|>form<|plamo:val|>plain<|plamo:msg|>"
            )
        )

    def test_max_tokens_follows_the_source_length(self):
        self.assertEqual(tr.max_tokens("あ" * 10), 256)
        self.assertEqual(tr.max_tokens("あ" * 1000), 1564)
        self.assertEqual(tr.max_tokens("あ" * 100000), 8000)


class ValidationTest(unittest.TestCase):
    def assertRejected(self, body, status):
        with self.assertRaises(tr.HttpError) as cm:
            tr.parse_request(body)
        self.assertEqual(cm.exception.status, status, cm.exception.message)

    def test_rejects(self):
        self.assertRejected(req(text="  "), 400)
        self.assertRejected(req(text="あ" * (tr.MAX_CHARS + 1)), 413)
        self.assertRejected(req(target_lang="xx"), 400)
        self.assertRejected(req(source_lang="xx"), 400)
        self.assertRejected(req(form="polite"), 400)
        self.assertRejected(req(target_lang="ja", form="casual"), 400)
        self.assertRejected(req(text="a <|plamo:tag|> b"), 400)
        self.assertRejected(req(context="<|plamo:msg|>"), 400)
        self.assertRejected(
            req(glossary=[{"source": "a", "target": "b"}] * (tr.MAX_GLOSSARY + 1)), 400
        )
        self.assertRejected(req(glossary=["a => b"]), 400)
        self.assertRejected(req(history=[{"source": "a", "target": "b"}] * 5), 400)
        self.assertRejected(req(context="あ" * tr.MAX_PROMPT_CHARS), 413)

    def test_accepts_and_drops_empty_pairs(self):
        got = tr.parse_request(
            req(glossary=[{"source": " ", "target": "x"}, {"source": "a", "target": "b"}])
        )
        self.assertEqual(got["glossary"], [("a", "b")])
        self.assertIsNone(got["source"])


class ServerBindTest(unittest.TestCase):
    def test_server_and_mock_bind_without_reverse_dns(self):
        with patch("socket.getfqdn", side_effect=AssertionError("逆引きを呼んでいます")):
            server = tr.TranslationHTTPServer(("127.0.0.1", 0), tr.Handler)
            mock, _ = mock_plamo.make_server()
            for app in (server, mock):
                with app:
                    self.assertEqual(app.server_name, "127.0.0.1")
                    self.assertEqual(app.server_port, app.socket.getsockname()[1])
                    self.assertGreater(app.server_port, 0)


class ServerTest(unittest.TestCase):
    """モックの翻訳エンジンを相手に、HTTP の動きを確かめる。"""

    @classmethod
    def setUpClass(cls):
        cls.mock, cls.stats = mock_plamo.make_server(api_key="k", delay=0.01, slow=2.5)
        threading.Thread(target=cls.mock.serve_forever, daemon=True).start()
        tr.UPSTREAM_URL = f"http://127.0.0.1:{cls.mock.server_address[1]}/v1/completions"
        tr.API_KEY = "k"
        tr.HEARTBEAT_SECONDS = 1
        tr.STATIC_DIR = ROOT / "server" / "static"
        cls.app = tr.TranslationHTTPServer(("127.0.0.1", 0), tr.Handler)
        cls.app.daemon_threads = True
        threading.Thread(target=cls.app.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.app.shutdown()
        cls.mock.shutdown()

    def call(self, method, path, body=None, user="alice", ctype="application/json"):
        conn = http.client.HTTPConnection("127.0.0.1", self.app.server_address[1], timeout=30)
        headers = {"Content-Type": ctype}
        if user:
            headers["X-Forwarded-User"] = user
        conn.request(
            method,
            path,
            body=json.dumps(body, ensure_ascii=False).encode() if body is not None else None,
            headers=headers,
        )
        return conn, conn.getresponse()

    def events(self, resp):
        out = []
        for raw in resp:
            line = raw.decode().strip()
            if line.startswith(": ping"):
                out.append("ping")
            elif line.startswith("data:"):
                out.append(json.loads(line[5:]))
        return out

    def test_index_and_config(self):
        _, resp = self.call("GET", "/", user=None)
        self.assertEqual(resp.status, 200)
        page = resp.read()
        self.assertIn(b"<title>", page)
        self.assertIn(b'from "./translation.mjs"', page)
        conn, resp = self.call("GET", "/translation.mjs", user=None)
        with resp:
            self.assertEqual(resp.status, 200)
            self.assertEqual(resp.getheader("Content-Type"), "text/javascript; charset=utf-8")
            self.assertIn(b"export function splitText", resp.read())
        conn.close()
        _, resp = self.call("GET", "/api/config")
        cfg = json.loads(resp.read())
        self.assertEqual(cfg["user"], "alice")
        self.assertEqual(len(cfg["languages"]), 53)

    def test_requires_a_user(self):
        _, resp = self.call("GET", "/api/config", user=None)
        self.assertEqual(resp.status, 401)
        _, resp = self.call("POST", "/api/translate", req(), user="Bad User")
        self.assertEqual(resp.status, 401)

    def test_config_announces_actual_user_concurrency_limit(self):
        with patch.object(tr, "MAX_ACTIVE_PER_USER", 4):
            conn, response = self.call("GET", "/api/config")
            try:
                config = json.loads(response.read())
            finally:
                conn.close()
        self.assertEqual(
            config["concurrency"], {"max_in_flight": 4, "recommended_in_flight": 4, "scope": "user"}
        )

    def test_config_exposes_only_explicit_public_desktop_settings(self):
        for discovery in ("", "https://auth.example.com/.well-known/openid-configuration"):
            with (
                self.subTest(discovery=discovery),
                patch.multiple(
                    tr,
                    PUBLIC_SERVER_URL="https://translate.example.com",
                    DESKTOP_DISCOVERY_URL=discovery,
                    DESKTOP_CLIENT_ID="translator-desktop",
                    USERINFO_URL="http://internal.invalid/userinfo",
                ),
            ):
                conn, response = self.call("GET", "/api/config")
                with response:
                    config = json.loads(response.read())
                conn.close()
                self.assertEqual(
                    config["desktop_connection"],
                    {
                        "server_url": "https://translate.example.com",
                        "discovery_url": discovery,
                        "client_id": "translator-desktop",
                    },
                )
                self.assertNotIn("internal.invalid", json.dumps(config))

    def test_requires_json(self):
        _, resp = self.call("POST", "/api/translate", req(), ctype="text/plain")
        self.assertEqual(resp.status, 415)

    def test_streams_the_translation(self):
        _, resp = self.call("POST", "/api/translate", req(text="こんにちは 世界"))
        self.assertEqual(resp.status, 200)
        self.assertTrue(resp.getheader("Content-Type").startswith("text/event-stream"))
        ev = self.events(resp)
        self.assertEqual(
            "".join(e.get("t", "") for e in ev if isinstance(e, dict)).strip(),
            "[en] こんにちは 世界",
        )
        self.assertEqual(ev[-1], {"done": True, "finish": "stop"})

    def test_reports_length_and_errors(self):
        _, resp = self.call("POST", "/api/translate", req(text="[length] 長い"))
        self.assertEqual(self.events(resp)[-1], {"done": True, "finish": "length"})
        _, resp = self.call("POST", "/api/translate", req(text="[error]"))
        self.assertIn("error", self.events(resp)[-1])

    def test_pings_while_waiting(self):
        _, resp = self.call(
            "POST",
            "/api/translate",
            req(
                text="[slow] 待つ",
            ),
        )
        ev = self.events(resp)
        self.assertGreaterEqual(ev.count("ping"), 2)
        self.assertEqual(ev[-1], {"done": True, "finish": "stop"})

    def test_limits_concurrent_translations_per_user(self):
        conns = [
            self.call("POST", "/api/translate", req(text="[slow] 1"), user="bob")
            for _ in range(tr.MAX_ACTIVE_PER_USER)
        ]
        time.sleep(0.3)
        _, resp = self.call("POST", "/api/translate", req(text="x"), user="bob")
        self.assertEqual(resp.status, 429)
        _, other = self.call("POST", "/api/translate", req(text="ほかの人"), user="carol")
        self.assertEqual(other.status, 200)
        other.read()
        for conn, r in conns:
            r.read()
            r.close()
            conn.close()

    def test_closing_the_connection_cancels_upstream(self):
        before = self.stats["canceled"]
        conn, resp = self.call("POST", "/api/translate", req(text="長い " * 200))
        resp.readline()
        # 応答の側もソケットを持っているので、両方を閉じて切る
        resp.close()
        conn.close()
        for _ in range(50):
            if self.stats["canceled"] > before:
                break
            time.sleep(0.1)
        self.assertGreater(self.stats["canceled"], before)


def jwt(exp: float) -> str:
    payload = base64.urlsafe_b64encode(json.dumps({"exp": exp}).encode()).rstrip(b"=")
    return f"e30.{payload.decode()}.sig"


class UserinfoTest(unittest.TestCase):
    """X-Forwarded-User が無く Bearer だけがあるときに、IdP の userinfo から利用者名を引く。"""

    @classmethod
    def setUpClass(cls):
        cls.calls = []
        cls.good = jwt(time.time() + 600)
        users = {cls.good: "bob", "opaque": "carol", "bad-name": "Bad User"}
        calls = cls.calls

        class Userinfo(BaseHTTPRequestHandler):
            def do_GET(self):
                token = self.headers.get("Authorization", "").removeprefix("Bearer ")
                calls.append(token)
                user = users.get(token) or ("dave" if token.startswith("expired.") else None)
                body = json.dumps({"sub": "x", "preferred_username": user}).encode()
                self.send_response(200 if user else 401)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        cls.idp = ThreadingHTTPServer(("127.0.0.1", 0), Userinfo)
        threading.Thread(target=cls.idp.serve_forever, daemon=True).start()
        cls.app = tr.TranslationHTTPServer(("127.0.0.1", 0), tr.Handler)
        cls.app.daemon_threads = True
        threading.Thread(target=cls.app.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.app.shutdown()
        cls.idp.shutdown()

    def setUp(self):
        url = f"http://127.0.0.1:{self.idp.server_address[1]}/api/oidc/userinfo"
        patcher = patch.multiple(tr, USERINFO_URL=url, userinfo_cache={})
        patcher.start()
        self.addCleanup(patcher.stop)
        self.calls.clear()

    def config(self, headers):
        conn = http.client.HTTPConnection("127.0.0.1", self.app.server_address[1], timeout=30)
        conn.request("GET", "/api/config", headers=headers)
        resp = conn.getresponse()
        body = json.loads(resp.read())
        conn.close()
        return resp.status, body

    def test_bearer_without_user_header_uses_userinfo_and_caches(self):
        for _ in range(2):
            status, body = self.config({"Authorization": f"Bearer {self.good}"})
            self.assertEqual((status, body.get("user")), (200, "bob"))
        self.assertEqual(self.calls, [self.good])
        # JWT でない token も、上限の秒数まで覚える
        for _ in range(2):
            self.assertEqual(self.config({"Authorization": "Bearer opaque"})[1]["user"], "carol")
        self.assertEqual(self.calls, [self.good, "opaque"])

    def test_user_header_wins_and_bad_tokens_are_rejected(self):
        status, body = self.config({"X-Forwarded-User": "alice", "Authorization": "Bearer opaque"})
        self.assertEqual((status, body.get("user")), (200, "alice"))
        self.assertEqual(self.calls, [])
        self.assertEqual(self.config({"Authorization": "Bearer unknown"})[0], 401)
        self.assertEqual(self.config({"Authorization": "Bearer bad-name"})[0], 401)
        self.assertEqual(self.config({"Authorization": "Basic opaque"})[0], 401)
        # 不正な X-Forwarded-User は userinfo に頼らず断る
        status, _ = self.config({"X-Forwarded-User": "Bad User", "Authorization": "Bearer opaque"})
        self.assertEqual(status, 401)

    def test_expired_tokens_are_not_cached_and_userinfo_is_off_by_default(self):
        token = "expired." + jwt(time.time() - 10).split(".", 1)[1]
        for _ in range(2):
            self.assertEqual(self.config({"Authorization": f"Bearer {token}"})[1]["user"], "dave")
        self.assertEqual(len(self.calls), 2)
        with patch.object(tr, "USERINFO_URL", ""):
            self.assertEqual(self.config({"Authorization": "Bearer opaque"})[0], 401)

    def test_unreachable_userinfo_is_a_gateway_error(self):
        with patch.object(tr, "USERINFO_URL", "http://127.0.0.1:9/userinfo"):
            self.assertEqual(self.config({"Authorization": "Bearer opaque"})[0], 502)


if __name__ == "__main__":
    unittest.main()
