"""ワーカーの登録と通信。実エンジンと本番にはつながない。"""

import json
import os
import queue
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import patch

from test_translator import tr
from mock_worker import MockWorker

MODEL = "a" * 64


class WorkerApiTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.state = Path(self.directory.name) / "workers.sqlite"
        self.settings = patch.multiple(
            tr,
            STATE_FILE=str(self.state),
            WORKER_MODEL_SHA256=MODEL,
            MIN_AGENT_VERSION="0.5.0",
            COUNTRY_HEADER="X-Test-Country",
            ALLOWED_COUNTRIES=frozenset({"JP"}),
            CLIENT_IP_HEADER="X-Test-IP",
        )
        self.settings.start()
        self.app = tr.TranslationHTTPServer(("127.0.0.1", 0), tr.Handler)
        self.app.daemon_threads = True
        self.hub = self.app.get_workers()
        self.hub.poll_seconds = 0.5
        self.workers = []
        self.threads = []
        threading.Thread(target=self.app.serve_forever, daemon=True).start()

    def tearDown(self):
        with self.hub.condition:
            self.hub.closed = True
            self.hub.condition.notify_all()
        for thread in self.threads:
            thread.join(3)
        self.app.shutdown()
        self.app.server_close()
        self.settings.stop()
        self.directory.cleanup()

    def worker(self, owner="alice", network="192.0.2.1"):
        mock = MockWorker(f"http://127.0.0.1:{self.app.server_port}", network=network)
        status, data = mock.register(owner)
        self.assertEqual(status, 200, data)
        self.workers.append(mock)
        return mock, data

    def run_thread(self, fn):
        result = queue.Queue()

        def run():
            try:
                result.put(fn())
            except BaseException as error:
                result.put(error)

        thread = threading.Thread(target=run, daemon=True)
        self.threads.append(thread)
        thread.start()
        return result

    def wait(self, predicate):
        deadline = time.monotonic() + 2
        while not predicate() and time.monotonic() < deadline:
            time.sleep(0.01)
        self.assertTrue(predicate())

    def assign(self, mock, events=None, cancel=None):
        events = queue.Queue() if events is None else events
        cancel = threading.Event() if cancel is None else cancel
        self.run_thread(
            lambda: self.hub.execute_probe(
                tr.parse_request({"text": "synthetic probe", "target_lang": "ja"}),
                events,
                cancel,
                timeout=0.4,
            )
        )
        status, assignment = mock.poll(MODEL)
        self.assertEqual(status, 200, assignment)
        self.assertEqual(set(assignment), {"assignment_id", "deadline_seconds", "input"})
        self.assertEqual(
            set(assignment["input"]),
            {"text", "target_lang", "source_lang", "form", "context", "glossary", "history"},
        )
        return assignment["assignment_id"], events, cancel

    def test_registration_ownership_renewal_and_delete(self):
        mock, data = self.worker()
        self.assertEqual(self.state.stat().st_mode & 0o777, 0o600)
        self.assertNotIn(data["secret"].encode(), self.state.read_bytes())
        self.assertEqual(
            mock.call("/api/worker/mine", owner="alice", method="GET")[1]["workers"][0]["state"],
            "probation",
        )
        self.assertEqual(
            mock.call("/api/worker/renew", {"worker_id": data["worker_id"]}, "bob")[0], 404
        )
        self.assertEqual(
            mock.call("/api/worker/renew", {"worker_id": data["worker_id"]}, "alice")[0], 200
        )
        self.assertEqual(mock.call("/api/worker/register", {"terms_version": True}, "bob")[0], 400)
        self.assertEqual(mock.register("alice")[0], 200)
        self.assertEqual(mock.register("alice")[0], 429)
        secret = data["secret"]
        self.assertEqual(
            mock.call("/api/worker/" + data["worker_id"], owner="alice", method="DELETE")[0], 200
        )
        self.assertEqual(mock.call("/api/worker/ping", {"active": []}, secret=secret)[0], 401)

    def test_authentication_region_version_and_no_job(self):
        mock, data = self.worker()
        self.assertEqual(
            mock.call("/api/translate", {"text": "probe", "target_lang": "ja"})[0], 401
        )
        self.assertEqual(
            mock.call("/api/worker/ping", {"active": []}, secret="pta_invalid")[0], 401
        )
        self.assertEqual(mock.call("/api/worker/mine", method="GET")[0], 401)
        self.assertEqual(mock.poll(MODEL, "0.4.0")[0], 403)
        self.assertEqual(mock.poll("b" * 64)[0], 403)
        self.assertEqual(mock.poll(MODEL)[0], 204)
        mock.country = "US"
        self.assertEqual(mock.poll(MODEL)[0], 403)
        mock.country = "JP"
        self.hub.registry.set_state(data["worker_id"], "revoked")
        self.assertEqual(mock.poll(MODEL)[0], 401)

    def test_stream_duplicate_sequence_and_cleanup(self):
        mock, _ = self.worker()
        aid, events, _ = self.assign(mock)
        self.assertEqual(mock.result(aid, 0, text="probe\x01\u202e\n", done=False)[0], 200)
        self.assertEqual(mock.result(aid, 0, text="probe", done=False)[0], 200)
        self.assertEqual(mock.result(aid, 1, done=True, finish="stop")[0], 200)
        self.wait(lambda: not self.hub.assignments)
        self.assertEqual(list(events.queue), [("text", "probe\n"), ("done", "stop")])
        self.assertEqual(mock.result(aid, 2, text="late")[1], {"stop": True})

    def test_wrong_worker_cancel_and_expired(self):
        mock, _ = self.worker()
        other, _ = self.worker("bob", "198.51.100.1")
        aid, events, cancel = self.assign(mock)
        self.assertEqual(other.result(aid, 0, text="wrong")[1], {"stop": True})
        self.assertTrue(events.empty())
        cancel.set()
        self.wait(lambda: not self.hub.assignments)
        self.assertEqual(mock.call("/api/worker/ping", {"active": [aid]})[1], {"stop": [aid]})
        aid, events, cancel = self.assign(mock)
        with self.hub.condition:
            self.hub.assignments[aid]["deadline"] = time.monotonic() - 1
        self.assertEqual(mock.result(aid, 0, text="late")[1], {"stop": True})
        cancel.set()

    def test_malformed_result_and_size_fail_assignment(self):
        mock, _ = self.worker()
        for fields in (
            {"seq": 2, "text": "skip"},
            {"seq": 0, "text": "<|plamo:"},
            {"seq": 0, "text": "x" * 2000},
            {"seq": 0, "done": True, "finish": "unknown"},
            {"seq": 0, "text": "x" * 65536},
        ):
            aid, events, cancel = self.assign(mock)
            seq = fields.pop("seq")
            self.assertIn(mock.result(aid, seq, **fields)[0], (400, 413))
            self.assertNotIn(aid, self.hub.assignments)
            cancel.set()

    def test_control_filter_cannot_hide_reserved_marker(self):
        mock, _ = self.worker()
        aid, events, cancel = self.assign(mock)
        self.assertEqual(mock.result(aid, 0, text="<|pla")[0], 200)
        self.assertEqual(mock.result(aid, 1, text="m\x01o:")[0], 400)
        self.assertNotIn(aid, self.hub.assignments)
        cancel.set()

    def test_first_fragment_timeout_and_private_storage(self):
        with self.assertNoLogs(tr.log):
            mock, data = self.worker()
            aid, events, cancel = self.assign(mock)
            self.hub.first_seconds = 0.05
            self.wait(lambda: not self.hub.assignments)
            self.assertEqual(mock.result(aid, 0, text="late")[1], {"stop": True})
            cancel.set()
        contents = self.state.read_bytes()
        for value in (data["secret"], aid, "synthetic probe", "192.0.2.1"):
            self.assertNotIn(value.encode(), contents)

    def test_poll_is_single_and_retry_only_before_text(self):
        mock, _ = self.worker()
        result = self.run_thread(lambda: mock.poll(MODEL))
        self.wait(lambda: bool(self.hub.ready))
        self.assertEqual(mock.poll(MODEL)[0], 409)
        self.assertEqual(result.get(timeout=2)[0], 204)
        other, _ = self.worker("bob", "198.51.100.1")
        aid, events, cancel = self.assign(mock)
        self.assertEqual(mock.result(aid, 0, error="engine")[0], 200)
        status, assignment = other.poll(MODEL)
        self.assertEqual(status, 200, assignment)
        self.assertEqual(
            other.result(assignment["assignment_id"], 0, done=True, finish="stop")[0], 200
        )
        self.wait(lambda: not self.hub.assignments)
        self.assertEqual(list(events.queue), [("done", "stop")])
        aid, events, cancel = self.assign(mock)
        mock.result(aid, 0, text="first")
        self.wait(lambda: not events.empty())
        mock.result(aid, 1, error="engine")
        self.wait(lambda: events.qsize() == 2)
        self.assertEqual(list(events.queue)[1][0], "worker_failed")
        self.assertEqual(other.poll(MODEL)[0], 204)

    def test_restart_keeps_only_registration_states_and_owner_revocation(self):
        mock, data = self.worker()
        self.hub.registry.set_state(data["worker_id"], "quarantine")
        self.hub.registry.close()
        self.hub.registry = tr.WorkerRegistry(self.state)
        self.assertEqual(self.hub.registry.authenticate(data["secret"])["state"], "quarantine")
        self.hub.registry.revoke_owner("alice")
        self.assertEqual(mock.register("alice")[0], 403)
        self.assertEqual(mock.call("/api/worker/ping", {"active": []})[0], 401)
        self.assertEqual(
            self.hub.registry.db.execute(
                "SELECT terms,agreed,revoked FROM owners WHERE name='alice'"
            ).fetchone(),
            (None, None, 1),
        )

    def test_credentials_expire_and_direct_pool_remains_separate(self):
        mock, data = self.worker()
        with patch.object(tr.time, "time", return_value=time.time() + tr.WORKER_TTL + 1):
            self.assertEqual(mock.poll(MODEL)[0], 401)
        with patch.object(tr, "UPSTREAM_URL", "http://127.0.0.1:1/v1/completions"):
            self.assertIsInstance(self.app.get_pool(), tr.DirectPool)
        self.assertEqual(mock.poll(MODEL)[0], 204)
        with patch.object(tr, "POOL_KIND", "volunteer"):
            del self.app.pool
            with self.assertRaises(ValueError):
                self.app.get_pool()


class WorkerStorageTest(unittest.TestCase):
    def test_symlink_and_shared_directory_are_refused(self):
        if os.name == "nt":
            self.skipTest("POSIXの権限の試験")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / "target"
            target.touch()
            link = root / "link"
            link.symlink_to(target)
            with self.assertRaises(OSError):
                tr.WorkerRegistry(link)
            root.chmod(0o755)
            try:
                with self.assertRaises(ValueError):
                    tr.WorkerRegistry(root / "state")
            finally:
                root.chmod(0o700)
