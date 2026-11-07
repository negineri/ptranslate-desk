"""タグの不一致を公開前に止める試験。"""

import importlib.util
import base64
import json
from pathlib import Path
import tempfile
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location(
    "check_release_version", ROOT / "tools/check_release_version.py"
)
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleaseVersionTest(unittest.TestCase):
    def test_windows_manifest_requires_https_signature_and_explicit_platform(self):
        entry = {
            "url": "https://example.invalid/update",
            "signature": base64.b64encode(b"public-signature").decode(),
        }
        manifest = {
            "version": "1.0.0",
            "platforms": {name: entry.copy() for name in ("darwin-aarch64", "darwin-x86_64")},
        }
        with self.assertRaisesRegex(ValueError, "Windows"):
            release.check_manifest(manifest, "1.0.0", require_windows=True)
        manifest["platforms"]["windows-x86_64"] = entry.copy()
        release.check_manifest(manifest, "1.0.0", require_windows=True)
        manifest["platforms"]["windows-x86_64"]["url"] = "http://example.invalid/update"
        with self.assertRaisesRegex(ValueError, "HTTPS"):
            release.check_manifest(manifest, "1.0.0", require_windows=True)
        manifest["platforms"]["windows-x86_64"]["url"] = entry["url"]
        manifest["platforms"]["windows-x86_64"]["signature"] = ""
        with self.assertRaisesRegex(ValueError, "署名"):
            release.check_manifest(manifest, "1.0.0", require_windows=True)

    def test_universal_manifest_requires_both_platforms_and_signature(self):
        entry = {
            "url": "https://example.invalid/app.tar.gz",
            "signature": base64.b64encode(b"public-signature").decode(),
        }
        manifest = {
            "version": "1.0.0",
            "platforms": {"darwin-aarch64": entry.copy(), "darwin-x86_64": entry.copy()},
        }
        release.check_manifest(manifest, "1.0.0")
        with self.assertRaises(ValueError):
            release.check_manifest(manifest, "2.0.0")
        manifest["platforms"]["darwin-x86_64"]["signature"] = ""
        with self.assertRaisesRegex(ValueError, "署名"):
            release.check_manifest(manifest, "1.0.0")
        del manifest["platforms"]["darwin-x86_64"]
        with self.assertRaisesRegex(ValueError, "両方"):
            release.check_manifest(manifest, "1.0.0")

    def test_default_is_development_even_on_a_tag(self):
        self.assertEqual(release.check_version(ROOT), "0.0.0-dev")
        with tempfile.TemporaryDirectory() as directory:
            root = self.make_repo(directory)
            self.git(root, "tag", "v1.2.3")
            self.assertEqual(release.check_version(root), "0.0.0-dev")
            self.assertEqual(release.release_version(root), "1.2.3")

    def git(self, root, *args):
        return subprocess.check_output(
            ["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid", *args],
            cwd=root,
            text=True,
            stderr=subprocess.PIPE,
        ).strip()

    def make_repo(self, directory):
        root = Path(directory)
        for relative in (
            "desktop/src-tauri/tauri.conf.json",
            "desktop/src-tauri/tauri.release.conf.json",
            "desktop/src-tauri/Cargo.toml",
            "desktop/src-tauri/Cargo.lock",
            "desktop/package.json",
            "desktop/package-lock.json",
        ):
            target = root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes((ROOT / relative).read_bytes())
        self.git(root, "init", "-q")
        self.git(root, "add", ".")
        self.git(root, "commit", "-qm", "初期状態")
        return root

    def test_release_requires_head_tag_and_clean_sources(self):
        with tempfile.TemporaryDirectory() as directory:
            root = self.make_repo(directory)
            with self.assertRaisesRegex(ValueError, "gitタグ"):
                release.release_version(root)
            self.git(root, "tag", "-a", "v12.3.4-beta.1", "-m", "配布")
            self.assertEqual(release.check_version(root, "v12.3.4-beta.1"), "12.3.4-beta.1")
            output = root / "generated.json"
            release.write_release_config(root, output, "12.3.4-beta.1")
            config = json.loads(output.read_text())
            self.assertEqual(config["version"], "12.3.4-beta.1")
            self.assertTrue(config["bundle"]["createUpdaterArtifacts"])
            # 生成した未追跡ファイルでは配布を妨げない。
            self.assertEqual(release.release_version(root), "12.3.4-beta.1")
            package = root / "desktop/package.json"
            package.write_text(package.read_text() + "\n")
            with self.assertRaisesRegex(ValueError, "コミット"):
                release.release_version(root)
            self.git(root, "add", "desktop/package.json")
            self.git(root, "commit", "-qm", "変更")
            with self.assertRaisesRegex(ValueError, "HEAD"):
                release.release_version(root, "v12.3.4-beta.1")

    def test_rejects_invalid_tags_and_package_version(self):
        for tag in ("v01.2.3", "v1.2", "v1.2.3-01", "1.2.3", "v1.2.3/path"):
            with self.subTest(tag=tag), self.assertRaisesRegex(ValueError, "形式"):
                release.release_version(ROOT, tag)
        with tempfile.TemporaryDirectory() as directory:
            root = self.make_repo(directory)
            package = root / "desktop/package.json"
            contents = json.loads(package.read_text())
            contents["version"] = "999.0.0"
            package.write_text(json.dumps(contents))
            with self.assertRaisesRegex(ValueError, "package.json"):
                release.check_version(root)
