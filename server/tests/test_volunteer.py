"""ボランティア枠を利用者のHTTP接続からローカルのモックまで試す。"""

import http.client
import json
import time
import unittest
from unittest.mock import patch

import test_worker_checks as checks_tests
from test_translator import tr
from test_workers import MODEL


class VolunteerTest(unittest.TestCase):
    def setUp(self):
        self.fixture = checks_tests.CanaryApiTest()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.helper = self.fixture.helper
        self.checks = self.fixture.checks
        self.helper.hub.poll_seconds = 2
        self.settings = patch.multiple(
            tr,
            POOL_KIND="volunteer",
            CANARY_FILE=str(self.fixture.canaries),
            AUDIT_UPSTREAMS=json.dumps([{"url": self.checks.pool.urls[0], "slots": 1}]),
            QUEUE_TIMEOUT=2,
        )
        self.settings.start()
        self.addCleanup(self.settings.stop)
        self.pool = self.helper.app.get_pool()
        self.checks.start()

    def request(self, body=None, method="POST", path="/api/translate"):
        conn = http.client.HTTPConnection("127.0.0.1", self.helper.app.server_port, timeout=5)
        self.addCleanup(conn.close)
        conn.request(
            method,
            path,
            json.dumps(body) if body is not None else None,
            {
                "Content-Type": "application/json",
                "X-Forwarded-User": "test-reader",
                "X-Test-Country": "JP",
                "X-Test-IP": "198.51.100.11",
            },
        )
        return conn, conn.getresponse()

    def qualified(self, owner, network):
        mock, data = self.helper.worker(owner, network)
        worker = self.helper.hub.registry.info(data["worker_id"])
        for _ in range(self.checks.health.probation_jobs):
            self.checks.health.record(worker, passed=True)
        return mock, worker

    def poll(self, mock, worker):
        result = self.helper.run_thread(lambda: mock.poll(MODEL))
        self.helper.wait(lambda: worker["id"] in self.helper.hub.ready)
        return result

    @staticmethod
    def body(**fields):
        return {
            "text": "Private synthetic paragraph for translation.",
            "target_lang": "en",
            "consent": 1,
            **fields,
        }

    def test_missing_consent_never_assigns(self):
        mock, worker = self.qualified("owner-a", "192.0.2.1")
        waiting = self.poll(mock, worker)
        _, config = self.request(method="GET", path="/api/config")
        self.assertEqual(json.load(config)["pool"], {"kind": "volunteer", "consent_version": 1})
        for consent in (None, True, 0, 2):
            _, response = self.request(self.body(consent=consent))
            self.assertEqual(response.status, 403)
            self.assertEqual(json.load(response)["code"], "consent_required")
        self.assertEqual(waiting.get(timeout=3)[0], 204)
        self.assertFalse(self.pool.history)

    def test_delivery_schedules_audit_and_qualified_canary_and_logs_only_totals(self):
        mock, worker = self.qualified("owner-a", "192.0.2.1")
        self.checks.canary_rate = self.checks.audit_rate = 1
        waiting = self.poll(mock, worker)
        _, response = self.request(self.body(group="g" * 22))
        status, assignment = waiting.get(timeout=3)
        self.assertEqual(status, 200)
        self.assertEqual(set(assignment), {"assignment_id", "deadline_seconds", "input"})
        self.assertEqual(
            set(assignment["input"]),
            {
                "text",
                "target_lang",
                "source_lang",
                "form",
                "context",
                "glossary",
                "history",
            },
        )
        text = "[en] " + assignment["input"]["text"]
        self.assertEqual(
            mock.result(assignment["assignment_id"], 0, text=text, done=True, finish="stop")[0], 200
        )
        self.assertIn(b'"done": true', response.read())
        self.fixture.answer(mock)
        self.helper.wait(lambda: self.fixture.stats["requests"] == 2)
        self.helper.wait(lambda: not self.pool.held)
        with self.assertLogs(tr.log, "INFO") as logs:
            self.pool.report()
        self.assertEqual(len(logs.output), 1)
        self.assertIn("count=1", logs.output[0])
        self.assertIn("spread=degraded", logs.output[0])
        for value in ("test-reader", "owner-a", text, "192.0.2", mock.secret):
            self.assertNotIn(value, logs.output[0])
        state = self.helper.state.read_bytes()
        for value in ("test-reader", text, "192.0.2", mock.secret):
            self.assertNotIn(value.encode(), state)

    def test_unsampled_output_is_checked_after_delivery(self):
        mock, worker = self.qualified("owner-a", "192.0.2.1")
        waiting = self.poll(mock, worker)
        with patch.object(self.checks, "sampled", return_value=False):
            _, response = self.request(self.body())
            _, assignment = waiting.get(timeout=3)
            mock.result(
                assignment["assignment_id"],
                0,
                text="https://evil.example/",
                done=True,
                finish="stop",
            )
            self.assertIn(b"evil.example", response.read())
            self.helper.wait(
                lambda: self.helper.hub.registry.info(worker["id"])["state"] == "quarantine"
            )
        self.assertEqual(self.fixture.stats["requests"], 0)

    def test_cancel_queued_and_running_requests(self):
        conn, response = self.request(self.body())
        self.helper.wait(lambda: bool(self.pool.waiting))
        response.close()
        conn.close()
        self.helper.wait(lambda: not self.pool.waiting)
        mock, worker = self.qualified("owner-a", "192.0.2.1")
        waiting = self.poll(mock, worker)
        conn, response = self.request(self.body())
        _, assignment = waiting.get(timeout=3)
        response.close()
        conn.close()
        self.helper.wait(lambda: not self.helper.hub.assignments)
        aid = assignment["assignment_id"]
        self.assertEqual(mock.call("/api/worker/ping", {"active": [aid]})[1], {"stop": [aid]})

    def test_retry_uses_another_worker_and_reports_failure_after_text(self):
        pair = [
            self.qualified("owner-a", "192.0.2.1"),
            self.qualified("owner-b", "203.0.113.1"),
        ]
        waiting = [self.poll(mock, worker) for mock, worker in pair]
        _, response = self.request(self.body())
        self.helper.wait(lambda: bool(self.helper.hub.assignments))
        with self.helper.hub.condition:
            first_id = next(iter(self.helper.hub.assignments.values()))["worker"]
        first = next(i for i, (_, worker) in enumerate(pair) if worker["id"] == first_id)
        second = 1 - first
        _, assignment = waiting[first].get(timeout=3)
        pair[first][0].result(assignment["assignment_id"], 0, error="engine")
        _, assignment = waiting[second].get(timeout=3)
        aid = assignment["assignment_id"]
        pair[second][0].result(aid, 0, text="Visible fragment.", done=False)
        self.assertIn(b"Visible fragment.", response.readline())
        pair[second][0].result(aid, 1, error="engine")
        self.assertIn(b'"code": "worker_failed"', response.read())
        self.assertEqual(len(self.pool.history), 2)

    def test_spread_uses_network_and_owner_as_one_group(self):
        now = time.monotonic()
        job = tr.Job("reader", "page_visible", now, group="g" * 22)
        a = ("a", {"owner": "owner-a"}, "192.0.2.0/24")
        alias = ("alias", {"owner": "owner-b"}, "192.0.2.0/24")
        other = ("other", {"owner": "owner-c"}, "198.51.100.0/24")
        online = [(item[1]["owner"], item[2]) for item in (a, alias, other)]
        self.assertEqual(self.pool.spread(job, [a], online, now), a)
        self.assertEqual(self.pool.spread(job, [alias, other], online, now), other)
        self.assertIsNone(self.pool.spread(job, [alias], online, now, [a[1]]))
