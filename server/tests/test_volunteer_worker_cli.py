"""無人ワーカーの認可・更新をローカルの模擬 IdP と仲介の応答で試す。"""

import contextlib
import io
import http.client
import json
from email.message import Message
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

    def test_remote_metadata_requires_the_release_model_digest_and_context(self):
        engine = cli.RemoteEngine("https://gpu.example.com", "engine-key")
        good = {
            "build_info": "b11804-c7fc50941",
            "model_path": f"/models/{cli.MODEL_FILE}",
            "default_generation_settings": {"n_ctx": cli.CONTEXT_PER_SLOT},
        }
        failures = [
            ({**good, "build_info": "b11804-abcdef0"}, cli.MODEL_SHA),
            ({**good, "build_info": "b12000-c7fc50941"}, cli.MODEL_SHA),
            ({**good, "model_path": "/models/other.gguf"}, cli.MODEL_SHA),
            ({**good, "default_generation_settings": {"n_ctx": 8192}}, cli.MODEL_SHA),
            (
                {**good, "default_generation_settings": {"n_ctx": cli.CONTEXT_PER_SLOT - 1}},
                cli.MODEL_SHA,
            ),
            (good, None),
            (good, "a" * 64),
        ]
        for props, digest in failures:
            with patch.object(
                engine, "open", return_value=remote_response(json.dumps(props).encode(), digest)
            ):
                with self.assertRaises(cli.EngineMismatch):
                    engine.verify()
        with patch.object(
            engine, "open", return_value=remote_response(json.dumps(good).encode(), cli.MODEL_SHA)
        ):
            engine.verify()

    def test_remote_revision_override_preserves_metadata_and_model_checks(self):
        engine = cli.RemoteEngine(
            "https://gpu.example.com", "engine-key", allow_revision_mismatch=True
        )
        props = {
            "build_info": "b11804-abcdef0",
            "model_path": f"/models/{cli.MODEL_FILE}",
            "default_generation_settings": {"n_ctx": cli.CONTEXT_PER_SLOT},
        }
        for override, digest, valid in (
            ({}, cli.MODEL_SHA, True),
            ({}, None, False),
            ({"build_info": ""}, cli.MODEL_SHA, False),
            ({"model_path": "/models/other.gguf"}, cli.MODEL_SHA, False),
            ({"default_generation_settings": {"n_ctx": 8192}}, cli.MODEL_SHA, False),
        ):
            response = remote_response(json.dumps({**props, **override}).encode(), digest)
            with patch.object(engine, "open", return_value=response):
                if valid:
                    engine.verify()
                else:
                    with self.assertRaises(cli.EngineMismatch):
                        engine.verify()

    def test_remote_relay_accepts_only_completion_and_checks_response_digest(self):
        engine = cli.RemoteEngine("https://gpu.example.com", "engine-key")
        sse = b'data: {"choices":[]}\n\ndata: [DONE]\n\n'
        with engine.relay() as (port, key):

            def complete(route, secret):
                connection = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
                try:
                    connection.request("POST", route, b"{}", {"Authorization": f"Bearer {secret}"})
                    response = connection.getresponse()
                    return response.status, response.read()
                finally:
                    connection.close()

            with patch.object(
                engine, "open", return_value=remote_response(sse, cli.MODEL_SHA)
            ) as remote:
                self.assertEqual(complete("/props", key)[0], 404)
                self.assertEqual(complete("/v1/completions", "wrong-key")[0], 401)
                remote.assert_not_called()
                self.assertEqual(complete("/v1/completions", key), (200, sse))
                remote.assert_called_once_with("/v1/completions", b"{}", timeout=30)
            with patch.object(engine, "open", return_value=remote_response(sse, None)):
                self.assertEqual(complete("/v1/completions", key)[0], 502)
            with self.assertRaises(cli.EngineMismatch):
                engine.verify()

    def test_remote_mismatch_prevents_participation_before_contacting_broker(self):
        args = type(
            "Args",
            (),
            {
                "engine_url": "https://gpu.example.com",
                "model": None,
                "allow_engine_revision_mismatch": False,
            },
        )()
        with tempfile.TemporaryDirectory() as directory:
            args.engine_key_file = Path(directory) / "key"
            args.engine_key_file.write_text("test-engine-token")
            with (
                patch.object(cli.RemoteEngine, "verify", side_effect=cli.EngineMismatch("model")),
                patch.object(cli, "participate") as participate,
            ):
                with self.assertRaises(cli.EngineMismatch):
                    cli.run(args, None, {"refresh_token": "test-refresh"})
                participate.assert_not_called()


def remote_response(data, digest):
    response = io.BytesIO(data)
    response.headers = Message()
    response.headers["Content-Type"] = "text/event-stream"
    if digest is not None:
        response.headers[cli.MODEL_DIGEST_HEADER] = digest
    response.status = 200
    return response
