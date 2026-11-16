"""tools/package_worker_cli.py の試験。"""

import hashlib
import io
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))

import package_worker_cli as worker  # noqa: E402


class WorkerReleaseTest(unittest.TestCase):
    def setUp(self):
        self.temp = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: __import__("shutil").rmtree(self.temp))
        self.binary = self.temp / "ptranslate-worker"
        self.binary.write_bytes(b"\x7fELF" + b"\0" * 64)

    def test_package_is_reproducible_and_verified(self):
        first = worker.package(self.binary, self.temp / "a", "0.9.0-beta.8")
        second = worker.package(self.binary, self.temp / "b", "0.9.0-beta.8")
        self.assertEqual(first.name, "PTranslate.Worker_0.9.0-beta.8_linux-x64.tar.gz")
        self.assertEqual(first.read_bytes(), second.read_bytes())
        digest = (self.temp / "a" / (first.name + ".sha256")).read_text()
        self.assertEqual(
            digest, f"{hashlib.sha256(first.read_bytes()).hexdigest()}  {first.name}\n"
        )
        with tarfile.open(first) as archive:
            (member,) = archive.getmembers()
            self.assertEqual(
                (member.name, member.mode, member.mtime), ("ptranslate-worker", 0o755, 0)
            )

    def test_rejects_non_elf_tampered_or_extra_files(self):
        script = self.temp / "script"
        script.write_bytes(b"#!/bin/sh\n")
        with self.assertRaises(ValueError):
            worker.package(script, self.temp / "x", "0.9.0")
        output = self.temp / "out"
        archive = worker.package(self.binary, output, "0.9.0")
        archive.write_bytes(archive.read_bytes() + b"\0")
        with self.assertRaises(ValueError):
            worker.verify(output, "0.9.0")
        archive.unlink()
        worker.package(self.binary, output, "0.9.0")
        (output / "extra").write_text("x")
        with self.assertRaises(ValueError):
            worker.verify(output, "0.9.0")
        with self.assertRaises(ValueError):
            worker.package(self.binary, output, "0.9.0")

    def test_rejects_archives_with_other_members(self):
        output = self.temp / "out"
        output.mkdir()
        name = worker.filename("0.9.0")
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
            for member in ("ptranslate-worker", "../evil"):
                entry = tarfile.TarInfo(member)
                entry.size = 4
                entry.mode = 0o755
                archive.addfile(entry, io.BytesIO(b"\x7fELF"))
        (output / name).write_bytes(buffer.getvalue())
        (output / (name + ".sha256")).write_text(
            f"{hashlib.sha256(buffer.getvalue()).hexdigest()}  {name}\n"
        )
        with self.assertRaises(ValueError):
            worker.verify(output, "0.9.0")


if __name__ == "__main__":
    unittest.main()
