"""照合・試用・検疫。基準エンジンもワーカーもローカルのモックを使う。"""

import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from contextlib import closing
from pathlib import Path
from unittest.mock import patch

import test_workers as workers
from test_translator import tr, mock_plamo


ADMIN = Path(__file__).resolve().parents[2] / "tools" / "worker_admin.py"


def run_admin(state, *args):
    return subprocess.run(
        [sys.executable, str(ADMIN), *args],
        env={**os.environ, "TR_STATE_FILE": str(state)},
        capture_output=True,
        text=True,
        timeout=5,
        check=True,
    )


def request(text="This is a synthetic translation test."):
    return tr.parse_request({"text": text, "target_lang": "en"})


class TranslationCheckTest(unittest.TestCase):
    def test_similar_output_is_accepted_and_corruption_rejected(self):
        baseline = "The project dashboard displays all active tasks."
        self.assertEqual(
            tr.check_translation(
                request(), "The project dashboard shows all active tasks.", baseline
            ),
            (),
        )
        self.assertIn(
            "similarity",
            tr.check_translation(request(), "Completely corrupted unrelated content.", baseline),
        )
        self.assertIn(
            "links", tr.check_translation(request(), baseline + " https://evil.example/", baseline)
        )
        self.assertIn(
            "links", tr.check_translation(request(), baseline + " ftp://evil.example/", baseline)
        )
        self.assertIn(
            "links", tr.check_translation(request(), baseline + " new@example.com", baseline)
        )
        self.assertIn("numbers", tr.check_translation(request(), baseline + " 987", baseline))
        self.assertEqual(
            tr.check_translation(request(), "The value is １，２３４.", "The value is 1234."), ()
        )
        self.assertIn(
            "language",
            tr.check_translation(
                request(), "翻訳結果を改ざんした日本語の文章をここに書いてあります。", baseline
            ),
        )

    def test_links_can_come_from_source_glossary_or_reference(self):
        req = tr.parse_request(
            {
                "text": "Contact user@example.com",
                "target_lang": "en",
                "glossary": [{"source": "site", "target": "https://example.com/"}],
            }
        )
        output = "Contact user@example.com at https://example.com/ for information."
        self.assertNotIn(
            "links", tr.check_translation(req, output, "Contact the service for information.")
        )
        self.assertEqual(
            tr.translation_links("https://example.com/。 user@example.com"),
            {"https://example.com/", "user@example.com"},
        )
        self.assertTrue(
            tr.matches_target_script("これは試験のための日本語の文章であり問題ありません。", "ja")
        )
        self.assertTrue(tr.matches_target_script("IBM", "ja"))


class WorkerHealthTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.registry = tr.WorkerRegistry(Path(self.directory.name) / "state.sqlite")
        self.health = tr.WorkerHealth(self.registry, probation_jobs=2)
        self.addCleanup(self.directory.cleanup)
        self.addCleanup(self.registry.close)

    def register(self, owner="alice"):
        data = self.registry.register(owner, 1)
        return data, self.registry.authenticate(data["secret"])

    def test_probation_quarantine_recovery_and_restart(self):
        data, worker = self.register()
        self.health.record(worker, True)
        self.assertFalse(self.health.eligible(worker))
        self.health.record(worker, True)
        self.assertTrue(self.health.eligible(worker))
        self.health.record(worker, False)
        self.assertEqual(self.registry.info(worker["id"])["state"], "quarantine")
        self.assertFalse(self.health.eligible(worker))
        for _ in range(2):
            self.health.record(self.registry.info(worker["id"]), True)
        self.assertTrue(self.health.eligible(self.registry.info(worker["id"])))
        restarted = tr.WorkerHealth(self.registry, probation_jobs=2)
        self.assertFalse(restarted.eligible(self.registry.info(worker["id"])))
        self.assertFalse(restarted.workers)

    def test_three_owner_failures_hold_every_worker_until_operator_releases(self):
        first, worker = self.register()
        second, other = self.register()
        for _ in range(3):
            self.health.record(self.registry.info(worker["id"]), False)
        for secret in (first["secret"], second["secret"]):
            held = self.registry.authenticate(secret)
            self.assertEqual(held["state"], "quarantine")
            self.assertTrue(held["owner_quarantined"])
            for _ in range(3):
                self.health.record(held, True)
            self.assertFalse(self.health.eligible(held))
        # 所有者の検疫前に取得した情報でも、自動復帰させない。
        for _ in range(3):
            self.health.record(other, True)
        self.assertEqual(self.registry.info(other["id"])["state"], "quarantine")
        self.assertFalse(self.health.eligible(other))
        self.health.release_owner("alice")
        self.assertFalse(self.registry.info(other["id"])["owner_quarantined"])
        self.assertEqual(self.health.workers[worker["id"]]["streak"], 0)

    def test_transport_failure_is_separate_and_does_not_revive_revoked_worker(self):
        data, worker = self.register()
        for _ in range(10):
            self.health.record(worker, passed=None, timed_out=True)
        self.assertEqual(self.health.workers[worker["id"]]["failed"], 0)
        self.assertEqual(self.health.workers[worker["id"]]["timeouts"], 10)
        self.assertEqual(self.registry.info(worker["id"])["state"], "quarantine")
        self.registry.set_state(worker["id"], "revoked")
        self.health.record(worker, True)
        self.health.record(worker, True)
        self.assertFalse(self.health.eligible(worker))
        with self.assertRaises(tr.HttpError):
            self.registry.authenticate(data["secret"])

    def test_old_schema_is_migrated(self):
        path = Path(self.directory.name) / "legacy.sqlite"
        with closing(tr.sqlite3.connect(path)) as legacy, legacy:
            legacy.execute(
                "CREATE TABLE owners (name TEXT PRIMARY KEY, terms INTEGER, agreed TEXT, revoked INTEGER NOT NULL DEFAULT 0)"
            )
        migrated = tr.WorkerRegistry(path)
        self.addCleanup(migrated.close)
        data = migrated.register("legacy", 1)
        self.assertFalse(migrated.authenticate(data["secret"])["owner_quarantined"])

    def test_owner_hold_survives_restart(self):
        data, worker = self.register()
        self.registry.quarantine_owner("alice")
        self.registry.close()
        self.registry = tr.WorkerRegistry(Path(self.directory.name) / "state.sqlite")
        self.addCleanup(self.registry.close)
        self.assertTrue(self.registry.authenticate(data["secret"])["owner_quarantined"])


class CanaryApiTest(unittest.TestCase):
    def setUp(self):
        self.helper = workers.WorkerApiTest()
        self.helper.setUp()
        self.addCleanup(self.helper.tearDown)
        self.engine, self.stats = mock_plamo.make_server(delay=0)
        threading.Thread(target=self.engine.serve_forever, daemon=True).start()
        self.addCleanup(self.engine.server_close)
        self.addCleanup(self.engine.shutdown)
        self.canaries = Path(self.helper.directory.name) / "canaries.json"
        # 公開試験専用の合成入力。本番の確認用ジョブ集ではない。
        self.canaries.write_text(
            json.dumps(
                [
                    {"text": f"Synthetic verification sentence number {n}.", "target_lang": "en"}
                    for n in range(8)
                ]
            )
        )
        self.canaries.chmod(0o600)
        self.checks = tr.CanaryChecks(
            self.helper.hub,
            [(f"http://127.0.0.1:{self.engine.server_port}/v1/completions", 1)],
            self.canaries,
            probation_jobs=2,
        )
        self.helper.hub.checks = self.checks
        self.addCleanup(self.checks.close)

    def answer(self, mock, wrong=False):
        status, assignment = mock.poll(workers.MODEL)
        self.assertEqual(status, 200, assignment)
        self.assertEqual(set(assignment), {"assignment_id", "deadline_seconds", "input"})
        translated = (
            "https://evil.example/999 corrupted answer"
            if wrong
            else "[en] " + assignment["input"]["text"]
        )
        self.assertEqual(
            mock.result(assignment["assignment_id"], 0, text=translated, done=True, finish="stop")[
                0
            ],
            200,
        )
        return assignment["input"]["text"]

    def test_mock_worker_qualifies_and_bad_answer_is_quarantined(self):
        mock, data = self.helper.worker()
        self.checks.start()
        used = [self.answer(mock), self.answer(mock)]
        self.helper.wait(lambda: data["worker_id"] in self.checks.health.qualified)
        status, listing = mock.call("/api/worker/mine", owner="alice", method="GET")
        self.assertEqual(listing["workers"][0]["state"], "ready")
        self.assertEqual(len(set(used)), 2)
        bad, bad_data = self.helper.worker("bob", "198.51.100.1")
        self.answer(bad, wrong=True)
        self.helper.wait(
            lambda: self.checks.health.workers.get(bad_data["worker_id"], {}).get("failed") == 1
        )
        self.assertEqual(
            self.helper.hub.registry.info(bad_data["worker_id"])["state"], "quarantine"
        )
        self.assertGreaterEqual(self.stats["requests"], 3)
        database = self.helper.state.read_bytes()
        for private in (used[0], "evil.example", mock.secret):
            self.assertNotIn(private.encode(), database)

    def test_bad_reference_is_not_a_worker_failure_and_shutdown_stops_engine(self):
        mock, data = self.helper.worker()
        with patch.object(self.checks, "reference", return_value=None):
            self.checks.start()
            self.assertEqual(mock.poll(workers.MODEL)[0], 204)
            self.assertNotIn(data["worker_id"], self.checks.health.workers)
        self.checks.close()
        self.assertFalse(self.checks.thread.is_alive())
        self.assertEqual(self.helper.hub.registry.info(data["worker_id"])["state"], "probation")

    def test_reference_engine_failure_does_not_log_content_or_change_scores(self):
        with self.assertLogs(tr.log, "WARNING") as logs:
            reference = self.checks.reference(request("[error] private synthetic probe"))
        self.assertIsNone(reference)
        self.assertEqual(logs.output, ["WARNING:translator:upstream returned 500"])
        self.assertFalse(self.checks.health.workers)
        self.assertEqual(self.checks.pool.free, [1])

    def test_exhausted_corpus_does_not_repeat_same_owner(self):
        mock, data = self.helper.worker()
        self.canaries.write_text(
            json.dumps([{"text": "Single synthetic verification sentence.", "target_lang": "en"}])
        )
        self.assertTrue(self.checks.reload_if_changed())
        self.checks.start()
        self.answer(mock)
        self.helper.wait(lambda: data["worker_id"] in self.checks.health.workers)
        self.assertEqual(mock.poll(workers.MODEL)[0], 204)
        self.assertEqual(len(self.checks.used["alice"]), 1)
        self.assertNotIn(data["worker_id"], self.checks.health.qualified)

    def test_three_slots_assign_three_workers_at_once(self):
        self.checks.close()
        self.checks = tr.CanaryChecks(
            self.helper.hub,
            [(f"http://127.0.0.1:{self.engine.server_port}/v1/completions", 3)],
            self.canaries,
            probation_jobs=2,
        )
        self.helper.hub.checks = self.checks
        self.addCleanup(self.checks.close)
        mocks = [self.helper.worker(owner)[0] for owner in ("alice", "alice", "bob")]
        self.checks.start()
        results = [
            self.helper.run_thread(lambda mock=mock: mock.poll(workers.MODEL)) for mock in mocks
        ]
        assignments = []
        for result in results:
            status, assignment = result.get(timeout=3)
            self.assertEqual(status, 200, assignment)
            assignments.append(assignment)
        self.assertEqual(len(self.helper.hub.assignments), 3)
        self.assertNotEqual(assignments[0]["input"]["text"], assignments[1]["input"]["text"])
        self.assertEqual(mocks[0].poll(workers.MODEL)[0], 409)
        self.assertEqual(self.stats["requests"], 3)
        for mock, assignment in zip(mocks, assignments):
            self.assertEqual(
                mock.result(
                    assignment["assignment_id"],
                    0,
                    text="[en] " + assignment["input"]["text"],
                    done=True,
                    finish="stop",
                )[0],
                200,
            )

    def test_reload_keeps_used_and_scores_and_logs_only_owner_count(self):
        first = {"text": "First private synthetic sentence.", "target_lang": "en"}
        second = {"text": "Second private synthetic sentence.", "target_lang": "en"}
        self.canaries.write_text(json.dumps([first]))
        self.checks.reload_if_changed()
        self.checks.reload_interval = 0.05
        mock, data = self.helper.worker()
        self.checks.start()
        self.assertEqual(self.answer(mock), first["text"])
        self.helper.wait(lambda: data["worker_id"] in self.checks.health.workers)
        with self.assertLogs(tr.log, "INFO") as logs:
            self.assertEqual(self.checks.report_exhaustion(), 1)
        self.assertEqual(logs.output, ["INFO:translator:canary exhausted_owners=1"])
        replacement = self.canaries.with_suffix(".new")
        replacement.write_text(json.dumps([first, second]))
        replacement.chmod(0o600)
        replacement.replace(self.canaries)
        self.helper.wait(lambda: len(self.checks.samples) == 2)
        self.assertEqual(self.answer(mock), second["text"])
        self.helper.wait(lambda: data["worker_id"] in self.checks.health.qualified)
        self.assertEqual(self.checks.health.workers[data["worker_id"]]["streak"], 2)
        self.assertEqual(len(self.checks.used["alice"]), 2)

    def test_invalid_reload_keeps_previous_corpus_without_logging_content(self):
        previous = self.checks.samples
        self.canaries.write_text("private invalid content")
        with self.assertLogs(tr.log, "WARNING") as logs:
            self.assertFalse(self.checks.reload_if_changed())
            self.assertFalse(self.checks.reload_if_changed())
        self.assertEqual(logs.output, ["WARNING:translator:canary reload failed: ValueError"])
        self.assertIs(self.checks.samples, previous)

    def test_admin_revoke_owner_ends_poll_on_database_recheck(self):
        self.helper.hub.poll_seconds = 10
        mock, data = self.helper.worker()
        mock.timeout = 8
        waiting = self.helper.run_thread(lambda: mock.poll(workers.MODEL))
        self.helper.wait(lambda: data["worker_id"] in self.helper.hub.ready)
        output = run_admin(self.helper.state, "list-owners")
        self.assertEqual(
            json.loads(output.stdout),
            [{"owner": "alice", "workers": 1, "quarantined": False, "revoked": False}],
        )
        run_admin(self.helper.state, "revoke-owner", "alice")
        self.assertNotIn("alice", self.helper.hub.revoked_owners)
        self.assertEqual(waiting.get(timeout=6)[0], 401)
        self.assertTrue(
            json.loads(run_admin(self.helper.state, "list-owners").stdout)[0]["revoked"]
        )

    def test_admin_release_invalidates_streak_and_can_revoke_one_worker(self):
        mock, data = self.helper.worker()
        registry = self.helper.hub.registry
        health = self.checks.health
        worker = registry.info(data["worker_id"])
        health.record(worker, True)
        health.record(worker, True)
        self.assertTrue(health.eligible(worker))
        registry.quarantine_owner("alice")
        run_admin(self.helper.state, "release-owner", "alice")
        refreshed = registry.info(data["worker_id"])
        self.assertFalse(health.eligible(refreshed))
        health.record(refreshed, True)
        self.assertEqual(health.workers[data["worker_id"]]["streak"], 1)
        self.assertFalse(health.eligible(refreshed))
        run_admin(self.helper.state, "revoke-worker", data["worker_id"])
        self.assertEqual(mock.poll(workers.MODEL)[0], 401)
        self.assertFalse(
            json.loads(run_admin(self.helper.state, "list-owners").stdout)[0]["revoked"]
        )

    def qualified_worker(self):
        mock, data = self.helper.worker()
        worker = self.helper.hub.registry.info(data["worker_id"])
        for _ in range(2):
            self.checks.health.record(worker, passed=True)
        return mock, worker

    def test_completed_delivery_can_schedule_canary_for_qualified_worker(self):
        mock, worker = self.qualified_worker()
        with patch.object(self.checks, "sampled", side_effect=[True, False]) as sample:
            self.assertFalse(
                self.checks.after_delivery(worker, request(), "Delivered translation.")
            )
        self.assertEqual(
            [call.args[0] for call in sample.call_args_list],
            [self.checks.canary_rate, self.checks.audit_rate],
        )
        self.checks.start()
        self.answer(mock)
        self.helper.wait(lambda: worker["id"] not in self.checks.due_canaries)
        self.helper.wait(lambda: self.checks.health.workers[worker["id"]]["streak"] == 3)

    def test_delivery_audit_is_bounded_and_quarantines_corruption(self):
        _, worker = self.qualified_worker()
        req = request()
        self.checks.audit_queue.maxsize = 1
        with patch.object(self.checks, "sampled", side_effect=[False, True, False, True]):
            self.assertTrue(
                self.checks.after_delivery(worker, req, "Corrupted https://evil.example/")
            )
            self.assertFalse(self.checks.after_delivery(worker, req, "Dropped second sample."))
        with self.assertNoLogs(tr.log, "WARNING"):
            self.checks.start()
            self.helper.wait(
                lambda: self.helper.hub.registry.info(worker["id"])["state"] == "quarantine"
            )
        self.assertEqual(self.stats["requests"], 1)
        self.assertTrue(self.checks.audit_queue.empty())
        self.assertNotIn(b"evil.example", self.helper.state.read_bytes())

    def test_suspicious_audit_quota_is_per_user_and_full_queue_does_not_charge_it(self):
        _, worker = self.qualified_worker()
        req = request()
        output = "Unexpected https://evil.example/"
        self.checks.audit_queue.maxsize = 1
        with patch.object(self.checks, "sampled", return_value=False):
            for _ in range(5):
                self.assertTrue(self.checks.after_delivery(worker, req, output, user="reader"))
                self.assertFalse(self.checks.after_delivery(worker, req, output, user="reader"))
                self.checks.audit_queue.get_nowait()
                self.checks.audit_queue.task_done()
            self.assertFalse(self.checks.after_delivery(worker, req, output, user="reader"))
            self.assertTrue(self.checks.after_delivery(worker, req, output, user="other-reader"))
        self.assertTrue(self.checks.health.eligible(worker))

    def test_passing_audit_does_not_count_towards_probation(self):
        _, worker = self.qualified_worker()
        req = request()
        self.checks.audit(worker["id"], worker["owner_generation"], req, "[en] " + req["text"])
        self.assertEqual(self.checks.health.workers[worker["id"]]["streak"], 2)
        self.assertEqual(self.checks.health.workers[worker["id"]]["passed"], 3)

    def test_private_corpus_permissions_and_input_are_checked(self):
        self.canaries.chmod(0o644)
        if tr.os.name != "nt":
            with self.assertRaises(ValueError):
                tr.CanaryChecks(self.helper.hub, [("http://localhost", 1)], self.canaries)
        self.canaries.chmod(0o600)
        self.canaries.write_text('[{"text":"<|plamo:","target_lang":"en"}]')
        with self.assertRaises(ValueError):
            tr.CanaryChecks(self.helper.hub, [("http://localhost", 1)], self.canaries)
