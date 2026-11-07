"""署名鍵の残留・ログ漏出と、成果物経由のパス逸脱を拒否する試験。"""

import base64
from contextlib import redirect_stdout
import importlib.util
import io
import json
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

TOOLS = Path(__file__).resolve().parents[2] / "tools"


def load(name):
    spec = importlib.util.spec_from_file_location(name, TOOLS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


with patch.object(sys, "path", [str(TOOLS), *sys.path]):
    signing = load("macos_signing")
    package = load("package_macos_release")
    registration = load("register_signing_secrets")


class SigningSecurityTest(unittest.TestCase):
    def test_import_failure_removes_p12(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)

            def command(*args):
                if args[:2] == ("security", "list-keychains"):
                    return '"/tmp/original.keychain-db"'
                if args[:2] == ("security", "import"):
                    self.assertTrue((directory / "certificate.p12").exists())
                    raise ValueError("インポート失敗")
                return ""

            with patch.object(signing, "run", side_effect=command):
                with self.assertRaises(ValueError):
                    signing.import_identity(directory, directory / "public.pem", b"dummy", "dummy")
            self.assertFalse((directory / "certificate.p12").exists())

    def test_cleanup_removes_files_even_when_keychain_commands_fail(self):
        for has_state in (False, True):
            with self.subTest(has_state=has_state), tempfile.TemporaryDirectory() as temp:
                directory = Path(temp) / "identity"
                directory.mkdir()
                (directory / "certificate.p12").write_bytes(b"dummy")
                (directory / "signing.keychain-db").touch()
                if has_state:
                    (directory / "keychains.json").write_text("[]")
                with patch.object(signing, "run", side_effect=ValueError("失敗")):
                    with self.assertRaises(ValueError):
                        signing.cleanup(directory, None)
                self.assertFalse(directory.exists())

    def test_sign_failure_always_removes_identity_directory(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp) / "identity"
            with (
                patch.dict(
                    signing.os.environ,
                    {"MACOS_SIGNING_CERTIFICATE": "ZHVtbXk=", "MACOS_SIGNING_PASSWORD": "dummy"},
                ),
                patch.object(signing, "import_identity", side_effect=ValueError("失敗")),
            ):
                with self.assertRaises(ValueError):
                    signing.sign(Path("unused.app"), directory)
            self.assertFalse(directory.exists())

    def test_tool_errors_do_not_print_secrets_or_inherit_them(self):
        result = subprocess.CompletedProcess([], 1, "dummy-private-value", "dummy-private-value")
        with (
            patch.dict(signing.os.environ, {"MACOS_SIGNING_PASSWORD": "dummy-private-value"}),
            patch.object(signing.subprocess, "run", return_value=result) as command,
            redirect_stdout(io.StringIO()) as output,
        ):
            with self.assertRaises(ValueError) as error:
                signing.run("security", "import", "-P", "dummy-private-value")
        self.assertNotIn("dummy-private-value", output.getvalue() + str(error.exception))
        self.assertNotIn("MACOS_SIGNING_PASSWORD", command.call_args.kwargs["env"])

    def test_archive_rejects_traversal_links_and_case_collisions(self):
        invalid = [
            "../outside",
            "/tmp/outside",
            f"{package.APP}/../outside",
            "other.app/file",
            f"{package.APP}/Contents/\\outside",
        ]
        for name in invalid:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temp:
                archive = Path(temp) / "app.zip"
                with zipfile.ZipFile(archive, "w") as bundle:
                    bundle.writestr(name, "dummy")
                with self.assertRaises(ValueError):
                    package.validate_archive(archive)
        for mode in (stat.S_IFLNK, stat.S_IFIFO):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as temp:
                archive = Path(temp) / "app.zip"
                entry = zipfile.ZipInfo(f"{package.APP}/Contents/link")
                entry.external_attr = (mode | 0o777) << 16
                with zipfile.ZipFile(archive, "w") as bundle:
                    bundle.writestr(entry, "../../outside")
                with self.assertRaises(ValueError):
                    package.validate_archive(archive)
        with tempfile.TemporaryDirectory() as temp:
            archive = Path(temp) / "app.zip"
            with zipfile.ZipFile(archive, "w") as bundle:
                bundle.writestr(f"{package.APP}/Contents/MacOS/ptranslate-desk", "dummy")
            package.validate_archive(archive)
            with zipfile.ZipFile(archive, "a") as bundle:
                bundle.writestr(f"{package.APP}/contents/MacOS/ptranslate-desk", "dummy")
            with self.assertRaises(ValueError):
                package.validate_archive(archive)

    def test_manifest_requires_signed_version_and_exact_publish_files(self):
        # 暗号学的な検証は別の公開テストベクトルで試す。
        with (
            tempfile.TemporaryDirectory() as temp,
            patch.object(package, "verify_update_signature"),
        ):
            directory = Path(temp)
            output = directory / "output"
            output.mkdir()
            name = "PTranslate Desk_1.2.3_universal.app.tar.gz"
            (output / name).write_bytes(b"dummy")
            (output / "PTranslate Desk_1.2.3_universal.dmg").write_bytes(b"dummy")
            sig = output / (name + ".sig")
            sig.write_text(
                base64.b64encode(
                    b"untrusted comment: dummy\nvalue\ntrusted comment: timestamp:0\tversion:1.2.3\nvalue\n"
                ).decode()
            )
            package.manifest(directory, "v1.2.3")
            value = json.loads((output / "latest.json").read_text())
            self.assertEqual(
                value["platforms"]["darwin-aarch64"], value["platforms"]["darwin-x86_64"]
            )
            (output / "certificate.p12").write_bytes(b"dummy")
            with self.assertRaises(ValueError):
                package.manifest(directory, "v1.2.3")
            sig.write_text(
                base64.b64encode(
                    b"untrusted comment: dummy\nvalue\ntrusted comment: timestamp:0\tversion:9.9.9\nvalue\n"
                ).decode()
            )
            with self.assertRaises(ValueError):
                package.manifest(directory, "v1.2.3")

    def test_update_signature_verifies_payload_version_and_public_key(self):
        # Rustのupdater試験と同じ公開テストベクトル。秘密鍵は含まない。
        public = b"untrusted comment: minisign public key: A49EF4746FA6E6D7\nRWTX5qZvdPSepDGWg2bz8n6ArQnBMwgQK1geVosorcbLzcunD+KJ+cYe"
        signature = b"untrusted comment: signature from tauri secret key\nRUTX5qZvdPSepPF2zsjvP/L/TQtBLRnWRvaq+4omK1UEugtQp/USrGiI1v762sDy3id3aor7bRC9ETyeot6lMHHrWoZ5hjUM1Qc=\ntrusted comment: timestamp:1791649051\tfile:payload\tversion:2.0.0\nDDq2CLOEl5cQmC2eJRHlnZPVViOZsu1CjpPKQoei4A/XBfkDqGO0B+QMrcrHgEQ5qV3mne75tdFWhJpqUXh9AA=="
        encoded_key = base64.b64encode(public).decode()
        encoded_signature = base64.b64encode(signature).decode()
        with tempfile.TemporaryDirectory() as temp:
            archive = Path(temp) / "payload"
            archive.write_bytes(b"test")
            package.verify_update_signature(archive, encoded_signature, encoded_key, "2.0.0")
            archive.write_bytes(b"tampered")
            with self.assertRaises(ValueError):
                package.verify_update_signature(archive, encoded_signature, encoded_key, "2.0.0")
            archive.write_bytes(b"test")
            with self.assertRaises(ValueError):
                package.verify_update_signature(archive, encoded_signature, encoded_key, "3.0.0")
            tampered = base64.b64encode(
                signature.replace(b"version:2.0.0", b"version:3.0.0")
            ).decode()
            with self.assertRaises(ValueError):
                package.verify_update_signature(archive, tampered, encoded_key, "3.0.0")
            lines = public.splitlines()
            wrong_key = bytearray(base64.b64decode(lines[1]))
            wrong_key[-1] ^= 1
            encoded_wrong_key = base64.b64encode(
                lines[0] + b"\n" + base64.b64encode(wrong_key)
            ).decode()
            with self.assertRaises(ValueError):
                package.verify_update_signature(
                    archive, encoded_signature, encoded_wrong_key, "2.0.0"
                )

    def test_failed_environment_registration_keeps_repository_secrets(self):
        with tempfile.TemporaryDirectory() as temp:
            certificate = Path(temp) / "certificate.p12"
            key = Path(temp) / "updater.key"
            certificate.write_bytes(b"dummy")
            key.write_text("dummy")
            with (
                patch.object(registration, "getpass", return_value="dummy"),
                patch.object(
                    registration,
                    "gh",
                    side_effect=[
                        json.dumps(
                            {
                                "can_admins_bypass": False,
                                "protection_rules": [
                                    {"type": "required_reviewers", "reviewers": ["dummy"]}
                                ],
                                "deployment_branch_policy": {
                                    "protected_branches": False,
                                    "custom_branch_policies": True,
                                },
                            }
                        ),
                        json.dumps({"branch_policies": [{"name": "v*", "type": "tag"}]}),
                        "",
                        ValueError("失敗"),
                    ],
                ) as command,
                redirect_stdout(io.StringIO()),
            ):
                with self.assertRaises(ValueError):
                    registration.register(certificate, key)
            self.assertTrue(
                any(call.args[:2] == ("secret", "set") for call in command.call_args_list)
            )
            self.assertFalse(
                any(call.args[:2] == ("secret", "delete") for call in command.call_args_list)
            )

    def test_unprotected_environment_refuses_secrets_before_reading_files(self):
        with patch.object(registration, "gh", side_effect=["{}", "{}"]) as command:
            with self.assertRaisesRegex(ValueError, "未設定"):
                registration.register(Path("does-not-exist.p12"), Path("does-not-exist.key"))
        self.assertFalse(any(call.args[:2] == ("secret", "set") for call in command.call_args_list))


if __name__ == "__main__":
    unittest.main()
