"""Windowsの中継モードをモックの名前付きパイプで確認する。登録と実モデルは使わない。"""

import ctypes
from ctypes import wintypes
import hashlib
import json
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import threading

ROOT = Path(__file__).resolve().parent.parent
BINARY = ROOT / "desktop/src-tauri/target/debug/ptranslate-desk.exe"
ORIGIN = "chrome-extension://" + "a" * 32 + "/"
MAX_MESSAGE = 256 * 1024


def read_frame(stream):
    header = stream.read(4)
    if len(header) != 4:
        raise RuntimeError("中継の応答がありません")
    length = struct.unpack("<I", header)[0]
    if not 0 < length <= MAX_MESSAGE:
        raise RuntimeError("中継の応答サイズが不正です")
    return json.loads(stream.read(length))


def main():
    if os.name != "nt":
        raise RuntimeError("Windowsで実行してください")
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.CreateNamedPipeW.restype = wintypes.HANDLE
    kernel.CreateNamedPipeW.argtypes = [
        wintypes.LPCWSTR,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.DWORD,
        ctypes.c_void_p,
    ]
    kernel.ConnectNamedPipe.argtypes = [wintypes.HANDLE, ctypes.c_void_p]
    kernel.ReadFile.argtypes = [
        wintypes.HANDLE,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.POINTER(wintypes.DWORD),
        ctypes.c_void_p,
    ]
    kernel.WriteFile.argtypes = kernel.ReadFile.argtypes
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    sid = subprocess.check_output(
        [
            "powershell.exe",
            "-NoProfile",
            "-Command",
            "[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value",
        ],
        text=True,
    ).strip()
    name = "\\\\.\\pipe\\ptranslate-" + hashlib.sha256((sid + str(BINARY)).encode()).hexdigest()
    with tempfile.TemporaryDirectory(prefix="ptranslate-chrome-test-") as directory:
        manifest = Path(directory) / "PTranslateChrome/native-host.json"
        manifest.parent.mkdir()
        manifest.write_text(
            json.dumps({"path": str(BINARY), "allowed_origins": [ORIGIN]}),
            encoding="utf-8",
        )
        env = dict(os.environ, LOCALAPPDATA=directory)
        for cancel in (False, True):
            pipe = kernel.CreateNamedPipeW(name, 3, 0, 1, 65536, 65536, 0, None)
            if pipe == ctypes.c_void_p(-1).value:
                raise ctypes.WinError(ctypes.get_last_error())
            errors = []
            disconnected = threading.Event()

            def serve():
                try:
                    if not kernel.ConnectNamedPipe(pipe, None):
                        if ctypes.get_last_error() != 535:
                            raise ctypes.WinError(ctypes.get_last_error())
                    count = wintypes.DWORD()

                    def read(length):
                        result = bytearray()
                        while len(result) < length:
                            buffer = ctypes.create_string_buffer(length - len(result))
                            if not kernel.ReadFile(
                                pipe, buffer, len(buffer), ctypes.byref(count), None
                            ):
                                raise ctypes.WinError(ctypes.get_last_error())
                            result.extend(buffer.raw[: count.value])
                        return bytes(result)

                    length = struct.unpack("<I", read(4))[0]
                    request = json.loads(read(length))
                    assert request == {"version": 1, "route": "api/config", "body": None}
                    messages = [{"kind": "start", "status": 200}]
                    if not cancel:
                        messages += [
                            {"kind": "bytes", "bytes": list("日本語".encode())},
                            {"kind": "end"},
                        ]
                    for message in messages:
                        body = json.dumps(message).encode()
                        frame = struct.pack("<I", len(body)) + body
                        if not kernel.WriteFile(pipe, frame, len(frame), ctypes.byref(count), None):
                            raise ctypes.WinError(ctypes.get_last_error())
                    if cancel:
                        buffer = ctypes.create_string_buffer(1)
                        assert not kernel.ReadFile(pipe, buffer, 1, ctypes.byref(count), None)
                        disconnected.set()
                except Exception as error:
                    errors.append(error)
                finally:
                    kernel.CloseHandle(pipe)

            worker = threading.Thread(target=serve, daemon=True)
            worker.start()
            with subprocess.Popen(
                [str(BINARY), ORIGIN],
                env=env,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            ) as host:
                watchdog = threading.Timer(10, host.kill)
                watchdog.start()
                try:
                    request = json.dumps(
                        {"version": 1, "route": "api/config", "body": None}
                    ).encode()
                    host.stdin.write(struct.pack("<I", len(request)) + request)
                    host.stdin.flush()
                    assert read_frame(host.stdout) == {"kind": "start", "status": 200}
                    if cancel:
                        host.stdin.close()
                        assert disconnected.wait(5), "中止がパイプへ届いていません"
                    else:
                        assert bytes(read_frame(host.stdout)["bytes"]).decode() == "日本語"
                        assert read_frame(host.stdout) == {"kind": "end"}
                    assert host.wait(timeout=5) == 0
                finally:
                    watchdog.cancel()
                    if host.poll() is None:
                        host.kill()
                        host.wait()
            worker.join(timeout=5)
            if errors:
                raise errors[0]
            assert not worker.is_alive()
    print("中継EXE: フレーム・日本語・完了・切断による中止 PASS")


if __name__ == "__main__":
    main()
