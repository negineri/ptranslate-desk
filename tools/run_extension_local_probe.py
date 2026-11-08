"""実Chrome拡張の手動通し確認用。取得済みモデルと一時設定を使う（Windows専用）。"""

import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import winreg

ROOT = Path(__file__).resolve().parent.parent
REGISTRY = r"Software\Google\Chrome\NativeMessagingHosts\com.witenp.ptranslate"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--parallel", type=int, choices=[1, 2, 4], default=1)
    parser.add_argument("--device", default="none")
    parser.add_argument("--port", type=int, default=0)
    args = parser.parse_args()
    manifest = Path(os.environ["LOCALAPPDATA"]) / "PTranslateChrome/native-host.json"
    saved_manifest = manifest.read_bytes() if manifest.exists() else None
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, REGISTRY) as key:
            saved_registration = winreg.QueryValueEx(key, "")
    except FileNotFoundError:
        saved_registration = None

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            paragraphs = [
                "This application translates text on your computer.",
                "The library opens at nine in the morning. Please return your books on time.",
                "We can read an article in another language while keeping its links and formatting.",
            ]
            if self.path.startswith("/long"):
                paragraphs *= 150
            body = (
                '<!doctype html><html lang="en"><meta charset="utf-8">'
                "<title>PTranslate local test</title><style>body{font:20px sans-serif;max-width:800px;margin:40px auto;line-height:1.8}p{margin:24px 0}</style>"
                "<main><h1>Local translation test</h1>"
                + "".join(f"<p>{text}</p>" for text in paragraphs)
                + "</main></html>"
            ).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    with tempfile.TemporaryDirectory(prefix="ptranslate-extension-probe-") as config:
        Path(config, "local-translation.json").write_text(
            json.dumps(
                {
                    "enabled": True,
                    "device": args.device,
                    "parallel": args.parallel,
                    "auto_release": True,
                    "idle_minutes": 1,
                }
            ),
            encoding="utf-8",
        )
        env = dict(os.environ)
        env.pop("PTRANSLATE_MOCK", None)
        env.update(PTRANSLATE_SMOKE_CONFIG=config, PTRANSLATE_EXTENSION_PROBE="1")
        binary = ROOT / "desktop/src-tauri/target/debug/ptranslate-desk.exe"
        state = {"url": f"http://127.0.0.1:{server.server_port}/", "config": config}
        (ROOT / "desktop/src-tauri/target/extension-probe.json").write_text(
            json.dumps(state), encoding="utf-8"
        )
        with subprocess.Popen([str(binary)], env=env, cwd=ROOT) as app:
            try:
                print(f"通し確認ページ: {state['url']}", flush=True)
                print(f"一時設定で開始: device={args.device}, parallel={args.parallel}", flush=True)
                return app.wait()
            finally:
                if app.poll() is None:
                    app.terminate()
                    app.wait(timeout=10)
                server.shutdown()
                server.server_close()
                # 試験で登録した連携を戻し、既存の通常版・試作版の登録を維持する。
                if saved_manifest is not None:
                    manifest.write_bytes(saved_manifest)
                elif manifest.exists():
                    manifest.unlink()
                if saved_registration is not None:
                    with winreg.CreateKey(winreg.HKEY_CURRENT_USER, REGISTRY) as key:
                        winreg.SetValueEx(key, "", 0, saved_registration[1], saved_registration[0])
                else:
                    try:
                        winreg.DeleteKey(winreg.HKEY_CURRENT_USER, REGISTRY)
                    except FileNotFoundError:
                        pass


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        pass
