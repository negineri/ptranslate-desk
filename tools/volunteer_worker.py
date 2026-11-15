#!/usr/bin/env python3
"""Linux で提供先を認可し、制限付き翻訳ワーカーを無人運用する。"""

import argparse
import contextlib
from contextlib import contextmanager
import fcntl
import hashlib
import hmac
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import secrets
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import time
from urllib import error, parse, request

from prepare_local_engine import MODEL_SHA

MAX_TTL = 30 * 86400
DAY = 86400
MODEL_FILE = "plamo-3-translate-2b-Q4_K_M.gguf"
MODEL_DIGEST_HEADER = "X-PTranslate-Model-SHA256"
ENGINE_TAG = "b11804"
ENGINE_COMMIT = "c7fc5094179bf8a9c73c18593db714f8da41a402"
METADATA_LIMIT = 65536


class Failure(Exception):
    def __init__(self, message, status=0, oauth_error=""):
        super().__init__(message)
        self.status, self.oauth_error = status, oauth_error


class NoRedirect(request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


HTTP = request.build_opener(request.ProxyHandler({}), NoRedirect())


def https_url(value, origin_only=False):
    try:
        url = parse.urlsplit(value)
        port = url.port or 443
        if (
            url.scheme != "https"
            or not url.hostname
            or url.username is not None
            or url.password is not None
            or url.fragment
            or url.query
            or any(char.isspace() or ord(char) < 32 or char == "\\" for char in value)
            or (origin_only and url.path not in ("", "/"))
        ):
            raise ValueError
    except (TypeError, ValueError, AttributeError):
        raise Failure("HTTPS の URL が不正です") from None
    return (url.hostname, port)


def api(url, *, form=None, body=None, token=None):
    headers = {"Accept": "application/json"}
    data = None
    if form is not None:
        data = parse.urlencode(form).encode()
        headers["Content-Type"] = "application/x-www-form-urlencoded"
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    if token is not None:
        headers["Authorization"] = f"Bearer {token}"
    try:
        with HTTP.open(request.Request(url, data, headers), timeout=15) as response:
            return json_object(response)
    except error.HTTPError as exc:
        with exc:
            try:
                payload = json.loads(exc.read(8192))
                code = payload.get("error", "") if isinstance(payload, dict) else ""
            except (ValueError, OSError, http.client.HTTPException):
                code = ""
        raise Failure(f"HTTP {exc.code}", exc.code, code) from None
    except (ValueError, OSError, http.client.HTTPException):
        raise Failure("接続できないか応答が不正です") from None


def json_object(response):
    body = response.read(METADATA_LIMIT + 1)
    if len(body) > METADATA_LIMIT:
        raise ValueError
    data = json.loads(body)
    if not isinstance(data, dict):
        raise ValueError
    return data


class EngineMismatch(Failure):
    pass


class RemoteEngine:
    def __init__(self, url, key, *, allow_revision_mismatch=False):
        https_url(url, origin_only=True)
        self.url = url.rstrip("/")
        self.key = key
        self.allow_revision_mismatch = allow_revision_mismatch
        self.recheck_failed = threading.Event()

    def open(self, route, data=None, timeout=5):
        return HTTP.open(
            request.Request(
                self.url + route,
                data=data,
                headers={"Authorization": f"Bearer {self.key}", "Content-Type": "application/json"},
            ),
            timeout=timeout,
        )

    @staticmethod
    def check_digest(response):
        if response.headers.get_all(MODEL_DIGEST_HEADER, []) != [MODEL_SHA]:
            raise EngineMismatch("リモート推論先のモデルのSHA-256を確認できません")

    def verify(self):
        if self.recheck_failed.is_set():
            raise EngineMismatch("リモート推論先の応答でモデルのSHA-256が一致しません")
        try:
            with self.open("/props") as response:
                if response.status != 200:
                    raise Failure("リモート推論先の設定を取得できません")
                self.check_digest(response)
                props = json_object(response)
        except error.HTTPError as exc:
            with exc:
                if exc.code in (401, 403, 404):
                    raise EngineMismatch(
                        "リモート推論先の認証または /props を確認してください"
                    ) from None
            raise Failure("リモート推論先の設定を取得できません") from None
        except (ValueError, OSError, http.client.HTTPException):
            raise Failure("リモート推論先の設定を取得できません") from None
        build = props.get("build_info")
        if not isinstance(build, str) or not build.strip() or not build.isprintable():
            raise EngineMismatch("リモート推論先のllama.cppのビルド情報を確認できません")
        tag, _, commit = build.partition("-")
        if not self.allow_revision_mismatch and (
            tag != ENGINE_TAG or len(commit) < 7 or not ENGINE_COMMIT.startswith(commit)
        ):
            raise EngineMismatch(
                f"リモート推論先のllama.cppが対応リビジョン {ENGINE_TAG} と一致しません"
            )
        model = props.get("model_path")
        if not isinstance(model, str) or Path(model).name != MODEL_FILE:
            raise EngineMismatch("リモート推論先が承認済みのモデルを読み込んでいません")
        settings = props.get("default_generation_settings")
        context_size = settings.get("n_ctx") if isinstance(settings, dict) else None
        if type(context_size) is not int or context_size < 32768:
            raise EngineMismatch("リモート推論先には1枠あたり32768以上のコンテキストが必要です")

    @contextmanager
    def relay(self):
        engine = self
        secret = secrets.token_urlsafe(32)

        class Handler(BaseHTTPRequestHandler):
            timeout = 10

            def log_message(self, *_):
                pass

            def do_POST(self):
                if self.path != "/v1/completions":
                    self.send_error(404)
                    return
                supplied = self.headers.get("Authorization", "").encode()
                if not hmac.compare_digest(supplied, f"Bearer {secret}".encode()):
                    self.send_error(401)
                    return
                lengths = self.headers.get_all("Content-Length", [])
                if (
                    self.headers.get("Transfer-Encoding")
                    or len(lengths) != 1
                    or not re.fullmatch(r"[0-9]{1,6}", lengths[0])
                    or not 0 < int(lengths[0]) <= 256 * 1024
                ):
                    self.send_error(400)
                    return
                sent = False
                try:
                    count = int(lengths[0])
                    body = self.rfile.read(count)
                    if len(body) != count:
                        return
                    with engine.open(self.path, body, timeout=30) as response:
                        engine.check_digest(response)
                        if (
                            response.status != 200
                            or response.headers.get_content_type() != "text/event-stream"
                        ):
                            raise Failure("リモート推論先がストリームを返しませんでした")
                        sent = True
                        self.send_response(200)
                        self.send_header("Content-Type", "text/event-stream")
                        self.end_headers()
                        while chunk := response.read1(8192):
                            self.wfile.write(chunk)
                except error.HTTPError as exc:
                    with exc, contextlib.suppress(OSError):
                        self.send_error(502)
                except EngineMismatch:
                    engine.recheck_failed.set()
                    with contextlib.suppress(OSError):
                        self.send_error(502)
                except (Failure, OSError, http.client.HTTPException):
                    if not sent:
                        with contextlib.suppress(OSError):
                            self.send_error(502)

        with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                yield server.server_port, secret
            finally:
                server.shutdown()
                thread.join()


@contextmanager
def state_lock(directory):
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = directory.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise Failure("状態ディレクトリは実行ユーザー所有の 0700 にしてください")
    fd = os.open(directory / "lock", os.O_WRONLY | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise Failure("同じ状態ディレクトリを別のプロセスが使用中です") from None
        yield directory / "worker.json"


def save(path, state):
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as file:
        temporary = Path(file.name)
        try:
            json.dump(state, file)
            file.flush()
            os.fsync(file.fileno())
            os.replace(temporary, path)
            parent = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(parent)
            finally:
                os.close(parent)
        finally:
            temporary.unlink(missing_ok=True)


def load(path):
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except FileNotFoundError:
        return {}
    with os.fdopen(fd) as file:
        info = os.fstat(file.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise Failure("状態ファイルは実行ユーザー所有の 0600 にしてください")
        state = json.load(file)
    if not isinstance(state, dict):
        raise Failure("状態ファイルが不正です")
    return state


def begin_configuration(broker):
    https_url(broker, origin_only=True)
    broker = broker.rstrip("/")
    config = api(f"{broker}/api/client-config")
    discovery = config.get("discovery_url")
    origin = https_url(discovery)
    document = api(discovery)
    token_url = document.get("token_endpoint")
    device_url = document.get("device_authorization_endpoint")
    if not device_url:
        # Authentik の Device Authorization は発行元と同じオリジンの固定経路。
        device_url = parse.urljoin(discovery, "/application/o/device/")
    if any(https_url(url) != origin for url in (document.get("issuer"), token_url, device_url)):
        raise Failure("IdP の発行元が一致しません")
    client_id = config.get("client_id")
    if not isinstance(client_id, str) or not client_id or len(client_id) > 256:
        raise Failure("公開クライアントの設定が不正です")
    return dict(broker=broker, client_id=client_id, token_url=token_url), device_url, origin


def required_token(payload, name):
    value = payload.get(name)
    if (
        not isinstance(value, str)
        or not value
        or len(value) > 16384
        or not value.isascii()
        or not value.isprintable()
        or any(c.isspace() for c in value)
    ):
        raise Failure("認証の応答が不正です")
    return value


def device_authorization(state, device_url, origin):
    config = api(
        device_url, form={"client_id": state["client_id"], "scope": "openid profile offline_access"}
    )
    verification = config.get("verification_uri")
    if https_url(verification) != origin:
        raise Failure("認可画面の発行元が一致しません")
    code = required_token(config, "user_code")
    device_code = required_token(config, "device_code")
    interval = config.get("interval", 5)
    duration = config.get("expires_in")
    if type(interval) is not int or interval <= 0 or type(duration) is not int or duration <= 0:
        raise Failure("認可の待ち時間が不正です")
    print(f"ブラウザーで {verification} を開き、コード {code} を入力してください。", flush=True)
    deadline = time.monotonic() + min(duration, 1800)
    while time.monotonic() + interval < deadline:
        time.sleep(interval)
        try:
            return api(
                state["token_url"],
                form={
                    "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
                    "client_id": state["client_id"],
                    "device_code": device_code,
                },
            )
        except Failure as exc:
            if exc.oauth_error == "slow_down":
                interval += 5
            elif exc.oauth_error != "authorization_pending":
                raise
    raise Failure("初回認可の待ち時間を過ぎました")


def update_expiry(state, response):
    ttl = response.get("expires_in")
    if type(ttl) is not int or not 1 <= ttl <= MAX_TTL:
        raise Failure("ワーカー資格の有効期間が不正です")
    now = time.time()
    state.update(expires_at=int(now + ttl), renew_at=int(now + min(DAY, max(1, ttl / 2))))


def renew(state, access):
    response = api(
        f"{state['broker']}/api/worker/renew", body={"worker_id": state["worker_id"]}, token=access
    )
    update_expiry(state, response)


def authorize(args, path, previous):
    state, device_url, origin = begin_configuration(args.server)
    if previous and previous["broker"] != state["broker"]:
        raise Failure("別の提供先には別の状態ディレクトリを使ってください")
    tokens = device_authorization(state, device_url, origin)
    access = required_token(tokens, "access_token")
    state["refresh_token"] = required_token(tokens, "refresh_token")
    if previous.get("worker_id"):
        state.update(worker_id=previous["worker_id"], worker_secret=previous["worker_secret"])
        try:
            renew(state, access)
        except Failure as exc:
            if exc.status != 404:
                raise
            del state["worker_id"]
    if "worker_id" not in state:
        response = api(
            f"{state['broker']}/api/worker/register", body={"terms_version": 1}, token=access
        )
        worker_id = response.get("worker_id", "")
        if not isinstance(worker_id, str) or not re.fullmatch(r"[0-9a-f]{32}", worker_id):
            raise Failure("ワーカーIDが不正です")
        secret = required_token(response, "secret")
        if not re.fullmatch(r"ptw_[0-9a-f]{64}", secret):
            raise Failure("ワーカー資格が不正です")
        state.update(worker_id=worker_id, worker_secret=secret)
        update_expiry(state, response)
    save(path, state)
    print("提供先の認可と登録を保存しました。", flush=True)


def refresh(path, state):
    try:
        tokens = api(
            state["token_url"],
            form={
                "grant_type": "refresh_token",
                "client_id": state["client_id"],
                "refresh_token": state["refresh_token"],
            },
        )
    except Failure as exc:
        if exc.oauth_error == "invalid_grant":
            state.pop("refresh_token", None)
            save(path, state)
            return "reauthorize"
        raise
    if tokens.get("refresh_token"):
        state["refresh_token"] = required_token(tokens, "refresh_token")
        save(path, state)
    try:
        renew(state, required_token(tokens, "access_token"))
    except Failure as exc:
        if exc.status == 401:
            return "reauthorize"
        if exc.status in (403, 404):
            return "rejected"
        raise
    save(path, state)
    return "renewed"


def run_worker(binary, config, stopping):
    with subprocess.Popen(
        [str(binary), "--worker-once"], stdin=subprocess.PIPE, stdout=subprocess.PIPE
    ) as child:
        try:
            child.stdin.write(json.dumps(config).encode() + b"\n")
            child.stdin.flush()
            while child.poll() is None and not stopping():
                time.sleep(0.25)
            if child.poll() is None:
                child.stdin.close()
                try:
                    child.wait(10)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
            if child.returncode:
                return "error"
            if not stopping():
                return json.loads(child.stdout.read(256))
        except (OSError, ValueError):
            return "error"


def run(args, path, state):
    if not state.get("refresh_token"):
        raise Failure("authorize で提供先を認可してください")
    engine_secret = args.engine_key_file.read_text().strip()
    if not engine_secret or len(engine_secret) > 4096:
        raise Failure("推論エンジンの資格が不正です")
    if args.engine_url:
        if args.model is not None:
            raise Failure("--model はローカル推論先に指定してください")
        engine = RemoteEngine(
            args.engine_url,
            engine_secret,
            allow_revision_mismatch=args.allow_engine_revision_mismatch,
        )
        engine.verify()
        with engine.relay() as (port, secret):
            participate(args, path, state, port, secret, MODEL_SHA, engine.verify)
    else:
        if args.model is None:
            raise Failure("--engine-port を使う場合は --model を指定してください")
        if not 1 <= args.engine_port <= 65535:
            raise Failure("推論エンジンのポートが不正です")
        with args.model.open("rb") as model:
            model_hash = hashlib.file_digest(model, "sha256").hexdigest()
        participate(args, path, state, args.engine_port, engine_secret, model_hash)


def participate(args, path, state, port, secret, model_hash, verify=None):
    config = dict(
        broker=state["broker"],
        worker_secret=state["worker_secret"],
        model_sha256=model_hash,
        agent_version="",
        engine_port=port,
        engine_secret=secret,
    )
    stopped = False

    def stop(*_):
        nonlocal stopped
        stopped = True

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    next_attempt = 0
    delay = 5
    while not stopped:
        now = time.time()
        if now >= next_attempt or now >= state["expires_at"]:
            try:
                outcome = refresh(path, state)
            except Failure:
                if now >= state["expires_at"]:
                    raise Failure("ワーカー資格が期限切れで更新できません") from None
                next_attempt = now + 60
                print("資格を更新できませんでした。再試行します。", file=sys.stderr, flush=True)
            else:
                if outcome == "reauthorize":
                    raise Failure("提供先の再認可が必要です。authorize を実行してください")
                if outcome == "rejected":
                    raise Failure("提供先が資格の更新を拒否しました")
                next_attempt = state["renew_at"]
        if stopped:
            break
        try:
            if verify is not None:
                verify()
        except EngineMismatch:
            raise
        except Failure:
            result = "engine_unavailable"
        else:
            if stopped:
                break
            result = run_worker(args.binary.resolve(), config, lambda: stopped)
        if result == "rejected":
            raise Failure("提供先がワーカー資格を拒否しました")
        if result in ("completed", "waiting", "quarantined"):
            delay = 5
            sleep_seconds = 2 if result == "quarantined" else 0.25
        else:
            message = (
                "リモート推論先の確認に失敗したため、ジョブの受付を待機します。"
                if result == "engine_unavailable"
                else "ワーカーの処理を続けられませんでした。再試行します。"
            )
            print(message, file=sys.stderr, flush=True)
            sleep_seconds = delay
            delay = min(delay * 2, 300)
        until = time.monotonic() + sleep_seconds
        while not stopped and time.monotonic() < until:
            time.sleep(0.25)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--state", type=Path, required=True, help="実行ユーザー専用の状態ディレクトリ"
    )
    commands = parser.add_subparsers(dest="command", required=True)
    setup = commands.add_parser("authorize", help="提供先への初回認可または再認可")
    setup.add_argument("server")
    setup.add_argument("--accept-terms", action="store_true", required=True)
    service = commands.add_parser("run", help="提供を開始し、資格を自動更新する")
    service.add_argument("--binary", type=Path, required=True)
    service.add_argument("--model", type=Path)
    target = service.add_mutually_exclusive_group(required=True)
    target.add_argument("--engine-port", type=int, help="ローカルの llama-server のポート")
    target.add_argument("--engine-url", help="リモートの llama-server の HTTPS オリジン")
    service.add_argument(
        "--allow-engine-revision-mismatch",
        action="store_true",
        help="リモートの llama.cpp のリビジョン不一致を許可する",
    )
    service.add_argument("--engine-key-file", type=Path, required=True)
    args = parser.parse_args()
    if args.command == "run" and args.allow_engine_revision_mismatch and not args.engine_url:
        service.error("--allow-engine-revision-mismatch には --engine-url が必要です")
    try:
        with state_lock(args.state) as path:
            state = load(path)
            if args.command == "authorize":
                authorize(args, path, state)
            else:
                run(args, path, state)
    except (Failure, OSError, ValueError, KeyError) as exc:
        print(
            f"提供先ワーカー: {exc if isinstance(exc, Failure) else '設定またはローカル状態を確認してください'}",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
