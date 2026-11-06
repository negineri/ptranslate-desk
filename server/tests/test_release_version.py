"""タグの不一致を公開前に止める試験。"""

import importlib.util
import base64
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location(
    "check_release_version", ROOT / "tools/check_release_version.py"
)
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleaseVersionTest(unittest.TestCase):
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

    def test_current_versions_match(self):
        version = release.check_version(ROOT)
        self.assertEqual(release.check_version(ROOT, f"v{version}"), version)

    def test_rejects_another_tag_and_package_version(self):
        with self.assertRaisesRegex(ValueError, "タグ"):
            release.check_version(ROOT, "v999.0.0")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for relative in (
                "desktop/src-tauri/tauri.conf.json",
                "desktop/src-tauri/Cargo.toml",
                "desktop/src-tauri/Cargo.lock",
                "desktop/package.json",
                "desktop/package-lock.json",
            ):
                target = root / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes((ROOT / relative).read_bytes())
            package = root / "desktop/package.json"
            contents = json.loads(package.read_text())
            contents["version"] = "999.0.0"
            package.write_text(json.dumps(contents))
            with self.assertRaisesRegex(ValueError, "package.json"):
                release.check_version(root)
