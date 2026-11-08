"""Chrome拡張の配布タグ・梱包・改ざん検査。外部のサーバーには接続しない。"""

import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
import package_chrome_extension as chrome


class ChromeReleaseTest(unittest.TestCase):
    def fixture(self, directory):
        dist = Path(directory) / "source"
        dist.mkdir()
        manifest = {
            "manifest_version": 3,
            "version": "0.0.1",
            "version_name": "0.0.0-dev",
            "background": {"service_worker": "background.mjs"},
            "action": {"default_popup": "action.html", "default_icon": {"16": "icon.png"}},
            "options_ui": {"page": "options.html"},
            "icons": {"16": "icon.png"},
            "permissions": ["activeTab", "storage"],
            "web_accessible_resources": [{"resources": ["popup.html"]}],
        }
        (dist / "manifest.json").write_text(json.dumps(manifest))
        for name in ("action.html", "options.html", "popup.html"):
            (dist / name).write_text("<!doctype html><p>試験</p>")
        (dist / "background.mjs").write_text('import { translate } from "./translation.mjs";')
        (dist / "translation.mjs").write_text("export const translate = () => {};")
        (dist / "icon.png").write_bytes(b"test-icon")
        return dist

    def test_zip_has_root_manifest_and_tag_version_without_changing_sources(self):
        with tempfile.TemporaryDirectory() as directory:
            dist = self.fixture(directory)
            original = (dist / "manifest.json").read_bytes()
            output = Path(directory) / "artifacts"
            archive = chrome.package(dist, output, "1.2.3-beta.1+build.2")
            with zipfile.ZipFile(archive) as zipped:
                manifest = json.loads(zipped.read("manifest.json"))
                self.assertEqual(manifest["version"], "1.2.3")
                self.assertEqual(manifest["version_name"], "1.2.3-beta.1+build.2")
                self.assertEqual(manifest["permissions"], ["activeTab", "storage"])
                self.assertIn("background.mjs", zipped.namelist())
                self.assertNotIn("source/manifest.json", zipped.namelist())
            self.assertEqual((dist / "manifest.json").read_bytes(), original)
            first = archive.read_bytes()
            extra = output / "unexpected.txt"
            extra.write_text("unexpected")
            with self.assertRaisesRegex(ValueError, "2ファイル"):
                chrome.verify(output, "1.2.3-beta.1+build.2")
            extra.unlink()
            chrome.package(dist, output, "1.2.3-beta.1+build.2")
            self.assertEqual(archive.read_bytes(), first)
            chrome.verify(output, "1.2.3-beta.1+build.2")
            with self.assertRaises((ValueError, OSError)):
                chrome.verify(output, "1.2.4")
            archive.write_bytes(first + b"modified")
            with self.assertRaisesRegex(ValueError, "SHA-256"):
                chrome.verify(output, "1.2.3-beta.1+build.2")

    def test_rejects_unsupported_versions(self):
        for value in ("0.0.0", "65536.1.2", "1.2.65536", "01.2.3", "1.2", "1.2.3/escape"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                chrome.chrome_version(value)
        self.assertEqual(chrome.chrome_version("65535.0.1"), "65535.0.1")

    def test_missing_runtime_files_and_symlinks_are_not_packaged(self):
        with tempfile.TemporaryDirectory() as directory:
            dist = self.fixture(directory)
            output = Path(directory) / "artifacts"
            (dist / "translation.mjs").unlink()
            with self.assertRaisesRegex(ValueError, "モジュール"):
                chrome.package(dist, output, "1.2.3")
            (dist / "translation.mjs").write_text("export const translate = () => {};")
            (dist / "action.html").write_text('<script src="./missing-ui.mjs"></script>')
            with self.assertRaisesRegex(ValueError, "参照するファイル"):
                chrome.package(dist, output, "1.2.3")
            (dist / "action.html").write_text("<!doctype html><p>試験</p>")
            (dist / "popup.html").unlink()
            with self.assertRaisesRegex(ValueError, "参照するファイル"):
                chrome.package(dist, output, "1.2.3")
            (dist / "popup.html").symlink_to(dist / "action.html")
            with self.assertRaisesRegex(ValueError, "シンボリックリンク"):
                chrome.package(dist, output, "1.2.3")

    def test_verification_rejects_archive_paths_even_with_a_matching_digest(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            archive = output / chrome.filename("1.2.3")
            with zipfile.ZipFile(archive, "w") as zipped:
                zipped.writestr("../escape", "test")
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            (output / (archive.name + ".sha256")).write_text(f"{digest}  {archive.name}\n")
            with self.assertRaisesRegex(ValueError, "パス"):
                chrome.verify(output, "1.2.3")

    def git(self, root, *args):
        return subprocess.check_output(
            ["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid", *args],
            cwd=root,
            stderr=subprocess.PIPE,
            text=True,
        ).strip()

    def test_release_requires_a_clean_head_tag_and_builds_a_versioned_zip(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.fixture(root)
            extension = root / "chrome-extension"
            extension.mkdir()
            (extension / "build.mjs").write_text(
                'import {cp, rm} from "node:fs/promises";'
                'await rm("chrome-extension/dist", {recursive:true, force:true});'
                'await cp("source", "chrome-extension/dist", {recursive:true});'
            )
            (root / ".gitignore").write_text("chrome-extension/dist/\nartifacts/\n")
            self.git(root, "init", "-q")
            self.git(root, "add", ".")
            self.git(root, "commit", "-qm", "fixture")
            output = root / "artifacts"
            with self.assertRaises(ValueError):
                chrome.build_release(root, output)
            self.git(root, "tag", "v1.2.3-beta.1")
            archive = chrome.build_release(root, output, "v1.2.3-beta.1")
            self.assertEqual(archive.name, "PTranslate.Chrome_1.2.3-beta.1.zip")
            chrome.verify(output, "1.2.3-beta.1")
            (root / "source/action.html").write_text("changed")
            with self.assertRaises(ValueError):
                chrome.build_release(root, output, "v1.2.3-beta.1")
            self.git(root, "add", ".")
            self.git(root, "commit", "-qm", "new HEAD")
            with self.assertRaises(ValueError):
                chrome.build_release(root, output, "v1.2.3-beta.1")
