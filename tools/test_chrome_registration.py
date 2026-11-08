"""WindowsでURL受信と起動済みアプリへの転送を試験する。登録と実モデルは使わない。"""

import os
from pathlib import Path
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
BINARY = ROOT / "desktop/src-tauri/target/debug/ptranslate-desk.exe"


def main():
    if os.name != "nt":
        raise RuntimeError("Windowsで実行してください")
    for running in (False, True):
        with tempfile.TemporaryDirectory(prefix="ptranslate-registration-") as directory:
            env = dict(
                os.environ,
                PTRANSLATE_SMOKE_CONFIG=directory,
                PTRANSLATE_MOCK="1",
                PTRANSLATE_CHROME_REGISTRATION_PROBE="1",
                LOCALAPPDATA=directory,
            )
            env.pop("PTRANSLATE_EXTENSION_PROBE", None)
            url = "ptranslate://register-chrome?extension_id=" + ("b" if running else "a") * 32
            command = [str(BINARY)] + ([] if running else [url])
            with subprocess.Popen(
                command,
                env=env,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                encoding="utf-8",
                errors="replace",
            ) as app:
                try:
                    if running:
                        # 起動中アプリで受信する経路。要求は画面の準備前でも保持される。
                        time.sleep(2)
                        subprocess.run([str(BINARY), url], env=env, check=True, timeout=15)
                    output, _ = app.communicate(timeout=25)
                    print(output, end="")
                    if (
                        app.returncode != 0
                        or "確認項目: URL受信・ID表示・キャンセル・承認の失効" not in output
                    ):
                        raise RuntimeError(
                            "URL連携の画面試験に失敗しました。同じアプリを終了して再試行してください"
                        )
                    assert not (Path(directory) / "inference").exists()
                    assert not (Path(directory) / "local-translation.json").exists()
                    assert not (Path(directory) / "PTranslateChrome/native-host.json").exists()
                finally:
                    if app.poll() is None:
                        app.kill()
                        app.wait()
    print("Chrome連携URL: 新規起動・起動中アプリへの転送・キャンセル PASS")


if __name__ == "__main__":
    main()
