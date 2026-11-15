"""マイページ、貢献量と退会をローカルの仲介で試す。"""

import http.client
import json
import unittest
from datetime import datetime, timezone
from unittest.mock import patch

import test_workers as worker_tests
from test_translator import tr


class VolunteerAccountTest(unittest.TestCase):
    def setUp(self):
        self.fixture = worker_tests.WorkerApiTest()
        self.fixture.setUp()
        self.addCleanup(self.fixture.tearDown)
        self.registry = self.fixture.hub.registry
        settings = patch.multiple(
            tr,
            POOL_KIND="volunteer",
            PUBLIC_SERVER_URL="https://translate.example.com",
            ACCOUNT_DELETE_KEY="test-only-delete-key-000000000000",
            ACCOUNT_SIGNUP_URL="https://id.example.com/signup?a=1&b=2",
            ACCOUNT_LINKS=json.dumps({"パスワードの変更": "https://id.example.com/password"}),
            CONTRIBUTION_DAILY_LIMIT=7,
        )
        settings.start()
        self.addCleanup(settings.stop)

    def request(self, path, method="GET", body=None, **headers):
        conn = http.client.HTTPConnection("127.0.0.1", self.fixture.app.server_port, timeout=3)
        try:
            conn.request(
                method,
                path,
                json.dumps(body) if body is not None else None,
                {"Content-Type": "application/json", "X-Test-Country": "JP", **headers},
            )
            response = conn.getresponse()
            return response.status, response.read()
        finally:
            conn.close()

    def register(self, owner="alice"):
        mock, data = self.fixture.worker(owner)
        return mock, {**self.registry.info(data["worker_id"]), "network": "192.0.2.0/24"}

    def delivery(self, worker, user="reader", tier="registered", network=""):
        job = tr.Job(user, "interactive", 0, tier, network)
        self.registry.record_contribution(worker, job, "input", "日本語")

    def test_account_and_ranking_are_authenticated_and_opt_in(self):
        _, worker = self.register()
        self.delivery(worker)
        alice = {"X-Forwarded-User-Id": "alice", "X-Forwarded-User": "Alice"}
        bob = {"X-Forwarded-User-Id": "bob"}
        for path in ("/account", "/ranking", "/api/account", "/api/account/ranking"):
            self.assertEqual(self.request(path)[0], 401)
        self.assertEqual(
            self.request("/api/account", Authorization="Bearer pta_test", **alice)[0], 401
        )
        for path in ("/account", "/ranking", "/account.mjs", "/account.css"):
            self.assertEqual(self.request(path, **alice)[0], 200)
        home = self.request("/")[1]
        self.assertIn(b"/account", home)
        self.assertIn(b"signup?a=1&amp;b=2", home)
        own = json.loads(self.request("/api/account", **alice)[1])
        self.assertEqual(own["terms"]["version"], 1)
        self.assertEqual(own["workers"][0]["worker_id"], worker["id"])
        self.assertEqual(
            own["total"], {"jobs": 1, "input_tokens": 2, "output_tokens": 2, "days": 1}
        )
        self.assertEqual(own["links"]["パスワードの変更"], "https://id.example.com/password")
        self.assertEqual(json.loads(self.request("/api/account", **bob)[1])["workers"], [])
        ranking = "/api/account/ranking?period=all"
        self.assertEqual(json.loads(self.request(ranking, **bob)[1])["entries"], [])
        body = {"mode": "nickname", "nickname": "<example>"}
        self.assertEqual(self.request("/api/account", "POST", body, **alice)[0], 403)
        origin = {"Origin": tr.PUBLIC_SERVER_URL}
        self.assertEqual(self.request("/api/account", "POST", body, **alice, **origin)[0], 200)
        expected = [{"name": "<example>", "tokens": 4, "days": 1}]
        for path in (ranking, "/api/account/ranking"):
            self.assertEqual(json.loads(self.request(path, **bob)[1])["entries"], expected)
        self.assertEqual(self.request("/api/account/ranking?period=2025-13", **bob)[0], 400)
        body = {"mode": "username", "nickname": ""}
        self.assertEqual(self.request("/api/account", "POST", body, **alice, **origin)[0], 200)
        self.assertEqual(json.loads(self.request(ranking, **bob)[1])["entries"][0]["name"], "Alice")
        body["mode"] = "hidden"
        self.assertEqual(self.request("/api/account", "POST", body, **alice, **origin)[0], 200)
        self.assertEqual(json.loads(self.request(ranking, **bob)[1])["entries"], [])

    def test_daily_limit_and_self_exclusion(self):
        _, first = self.register()
        _, second = self.register()
        self.delivery(first, user="alice")
        self.delivery(first, tier="anonymous", network="192.0.2.0/24")
        self.delivery(first)
        self.delivery(second)
        self.delivery(first, tier="anonymous", network="198.51.100.0/24")
        self.delivery(second, user="new-cookie", tier="anonymous", network="198.51.100.0/24")
        summary = self.registry.account("alice")["total"]
        self.assertEqual(summary, {"jobs": 2, "input_tokens": 4, "output_tokens": 4, "days": 1})

    def test_owner_quarantine_retracts_contributions_across_workers_once(self):
        _, first = self.register()
        _, second = self.register()
        self.delivery(first)
        self.delivery(second, user="other-reader")
        health = tr.WorkerHealth(self.registry)
        health.record(first, passed=False)
        self.assertEqual(self.registry.account("alice")["total"]["jobs"], 1)
        health.record(first, passed=False)
        health.record(first, passed=False)
        self.assertTrue(self.registry.info(second["id"])["owner_quarantined"])
        self.assertEqual(self.registry.account("alice")["total"]["jobs"], 0)
        self.assertEqual(
            self.registry.db.execute("SELECT sum(bad_jobs) FROM contribution").fetchone(), (2,)
        )

    def test_quarantine_and_monthly_compaction_survive_restart(self):
        _, first = self.register()
        _, second = self.register()
        with patch.object(
            tr.time, "time", return_value=datetime(2024, 3, 28, tzinfo=timezone.utc).timestamp()
        ):
            self.delivery(first)
        with patch.object(
            tr.time, "time", return_value=datetime(2024, 3, 29, tzinfo=timezone.utc).timestamp()
        ):
            self.delivery(first)
            self.delivery(second, user="second-reader")
        with patch.object(
            tr.time, "time", return_value=datetime(2024, 3, 30, tzinfo=timezone.utc).timestamp()
        ):
            self.delivery(first)
            self.registry.quality_failure(first)
            self.registry.set_listing("alice", "nickname", "Alice")
            summary = self.registry.account("alice")["total"]
            self.assertEqual(summary, {"jobs": 2, "input_tokens": 4, "output_tokens": 4, "days": 2})
            self.assertEqual(
                self.registry.db.execute("SELECT sum(bad_jobs) FROM contribution").fetchone()[0], 2
            )
        with patch.object(
            tr.time, "time", return_value=datetime(2025, 6, 1, tzinfo=timezone.utc).timestamp()
        ):
            registry = tr.WorkerRegistry(self.fixture.state)
            try:
                self.assertEqual(registry.account("alice")["total"], summary)
                self.assertEqual(
                    registry.ranking("2024-03"), [{"name": "Alice", "tokens": 8, "days": 2}]
                )
                self.assertEqual(
                    registry.db.execute("SELECT period FROM contribution").fetchall(),
                    [("2024-03",)],
                )
                self.assertEqual(
                    registry.db.execute("SELECT count(*) FROM contribution_recent").fetchone(), (0,)
                )
            finally:
                registry.close()

    def test_withdrawal_cancels_work_and_preserves_only_existing_revocation(self):
        mock, worker = self.register()
        self.delivery(worker)
        self.registry.set_listing("alice", "nickname", "Alice")
        aid, _, cancel = self.fixture.assign(mock)
        self.addCleanup(cancel.set)
        endpoint = "/api/account/delete"
        body = {"sub": "alice"}
        self.assertEqual(self.request(endpoint, "POST", body)[0], 401)
        headers = {"Authorization": "Bearer " + tr.ACCOUNT_DELETE_KEY}
        for _ in range(2):
            self.assertEqual(self.request(endpoint, "POST", body, **headers)[0], 200)
        self.assertEqual(mock.result(aid, 0, text="late", done=True, finish="stop")[0], 401)
        self.fixture.wait(lambda: not self.fixture.hub.assignments)
        self.delivery(worker)
        self.assertEqual(self.registry.account("alice")["total"]["jobs"], 0)
        self.assertIsNone(self.registry.account("alice")["terms"])
        self.assertEqual(self.registry.ranking("all"), [])
        self.assertEqual(list(self.registry.contribution_requests), [])
        self.assertEqual(self.registry.db.execute("SELECT name FROM owners").fetchall(), [])
        _, worker = self.register("revoked")
        self.delivery(worker)
        self.registry.revoke_owner("revoked")
        self.assertEqual(self.request(endpoint, "POST", {"sub": "revoked"}, **headers)[0], 200)
        with self.assertRaises(tr.HttpError) as refusal:
            self.registry.register("revoked", 1)
        self.assertEqual(refusal.exception.status, 403)
        self.assertEqual(
            self.registry.db.execute("SELECT name,terms,agreed,revoked FROM owners").fetchall(),
            [("revoked", None, None, 1)],
        )
