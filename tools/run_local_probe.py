#!/usr/bin/env python3
"""取得済みの実モデルでローカル翻訳を確認する。通常のCIでは実行しない。"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    devices = ["MTL0", "none"] if sys.platform == "darwin" else ["Vulkan0", "Vulkan1", "none"]
    parser.add_argument("--device", choices=devices, required=True)
    parser.add_argument("--auto-release", action="store_true")
    parser.add_argument("--measure", action="store_true")
    args = parser.parse_args()
    binary = ROOT / "desktop/src-tauri/target/debug/ptranslate-desk"
    if sys.platform == "win32":
        binary = binary.with_suffix(".exe")
    with tempfile.TemporaryDirectory(prefix="ptranslate-local-probe-") as config:
        (Path(config) / "local-translation.json").write_text(
            json.dumps(
                {
                    "enabled": True,
                    "device": args.device,
                    "auto_release": args.auto_release,
                    "idle_minutes": 1,
                }
            ),
            encoding="utf-8",
        )
        env = dict(os.environ)
        env.pop("PTRANSLATE_MOCK", None)
        env.update(PTRANSLATE_SMOKE_CONFIG=config, PTRANSLATE_LOCAL_PROBE="1")
        if args.measure:
            env["PTRANSLATE_LOCAL_MEASURE_PROBE"] = "1"
        with subprocess.Popen([str(binary)], cwd=ROOT, env=env) as app:
            try:
                return app.wait(timeout=480)
            except subprocess.TimeoutExpired:
                app.kill()
                app.wait()
                raise RuntimeError("実モデルの確認がタイムアウトしました") from None


if __name__ == "__main__":
    raise SystemExit(main())
