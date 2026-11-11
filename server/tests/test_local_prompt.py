"""PythonとRustが同じPFMLを生成することを共通データで確かめる。"""

import importlib.util
import json
import sys
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("local_prompt_server", ROOT / "server/translator.py")
tr = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = tr
spec.loader.exec_module(tr)


class LocalPromptTests(unittest.TestCase):
    def test_shared_fixtures(self):
        cases = json.loads(
            (ROOT / "shared/local-translation-fixtures.json").read_text(encoding="utf-8")
        )
        for case in cases:
            with self.subTest(case=case["name"]):
                req = tr.parse_request(case["body"])
                self.assertEqual(tr.build_prompt(req), case["prompt"])
                self.assertEqual(tr.max_tokens(req["text"]), case["max_tokens"])
