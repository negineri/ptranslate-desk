"""無人ワーカーの認可・更新をローカルの模擬 IdP と仲介の応答で試す。"""

import contextlib
import io
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import test_translator  # noqa: F401  tools を読み込めるようにする

if sys.platform == "linux":
    import volunteer_worker as cli


@unittest.skipUnless(sys.platform == "linux", "無人ワーカーはLinux用")
class WorkerCliTest(unittest.TestCase):
    def test_device_flow_and_registration_store_only_refresh_token(self):
        responses = [
            {"discovery_url": "https://id.example.com/discovery", "client_id": "worker"},
            {
                "issuer": "https://id.example.com",
                "token_endpoint": "https://id.example.com/token",
                "device_authorization_endpoint": "https://id.example.com/device",
            },
            {
                "verification_uri": "https://id.example.com/verify",
                "user_code": "ABCD-EFGH",
                "device_code": "test-device",
                "expires_in": 300,
                "interval": 1,
            },
            cli.Failure("pending", 400, "authorization_pending"),
            cli.Failure("slow", 400, "slow_down"),
            {"access_token": "test-access", "refresh_token": "test-refresh"},
            {"worker_id": "a" * 32, "secret": "ptw_" + "b" * 64, "expires_in": 30 * 86400},
        ]
        args = type("Args", (), {"server": "https://translate.example.com/"})()
        with tempfile.TemporaryDirectory() as directory:
            with (
                cli.state_lock(Path(directory)) as path,
                patch.object(cli, "api", side_effect=responses) as mock,
                patch.object(cli.time, "sleep") as sleep,
                contextlib.redirect_stdout(io.StringIO()),
            ):
                cli.authorize(args, path, {})
                state = cli.load(path)
                self.assertNotIn("access_token", state)
                self.assertEqual(state["refresh_token"], "test-refresh")
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                self.assertEqual([call.args for call in sleep.call_args_list], [(1,), (1,), (6,)])
                self.assertEqual(mock.call_args.kwargs["token"], "test-access")
                self.assertEqual(state["expires_at"] - state["renew_at"], 29 * 86400)

    def test_rotation_is_persisted_before_retry_and_invalid_grant_stops_renewal(self):
        state = {
            "broker": "https://translate.example.com",
            "token_url": "https://id.example.com/token",
            "client_id": "worker",
            "worker_id": "a" * 32,
            "refresh_token": "old-refresh",
            "expires_at": 123,
        }
        with tempfile.TemporaryDirectory() as directory, cli.state_lock(Path(directory)) as path:
            cli.save(path, state)
            with patch.object(cli, "api", side_effect=cli.Failure("unavailable", 503)):
                with self.assertRaises(cli.Failure):
                    cli.refresh(path, state)
                self.assertEqual(cli.load(path)["refresh_token"], "old-refresh")
            with patch.object(
                cli,
                "api",
                side_effect=[
                    {"access_token": "test-access", "refresh_token": "new-refresh"},
                    cli.Failure("unavailable", 503),
                ],
            ):
                with self.assertRaises(cli.Failure):
                    cli.refresh(path, state)
                self.assertEqual(cli.load(path)["refresh_token"], "new-refresh")
                self.assertEqual(cli.load(path)["expires_at"], 123)
            with patch.object(cli, "api", side_effect=cli.Failure("expired", 400, "invalid_grant")):
                self.assertEqual(cli.refresh(path, state), "reauthorize")
                self.assertNotIn("refresh_token", cli.load(path))

    def test_discovery_rejects_token_endpoint_on_another_origin(self):
        with patch.object(
            cli,
            "api",
            side_effect=[
                {"discovery_url": "https://id.example.com/discovery"},
                {
                    "issuer": "https://id.example.com",
                    "token_endpoint": "https://other.example.com/token",
                },
            ],
        ) as mock:
            with self.assertRaises(cli.Failure):
                cli.begin_configuration("https://translate.example.com")
            self.assertEqual(mock.call_count, 2)
