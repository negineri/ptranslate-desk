#!/usr/bin/env python3
"""PLaMo 3 Translate で文章を訳す Web アプリのサーバー (ptranslate-desk)。

使い方と設定は README.md、API は docs/SPEC.md の「サーバーの API」。

- 画面が原文を段落ごとに区切り、1 区切りずつ POST /api/translate に送る
- サーバーは PFML3 (PLaMo の素の書式) のプロンプトを組み、翻訳エンジン (PLaMo 3 Translate を
  動かす llama-server の /v1/completions) にストリーミングで投げ、届いた分を Server-Sent Events で返す。
  PLaMo は指示に従わない翻訳専用のモデルなので、プロンプトに指示の文を入れない
- 待っている間も 15 秒ごとにコメント行を送る (Cloudflare は 100 秒応答の無い接続を切る)
- 利用者は前段の認証プロキシ (oauth2-proxy など) が付ける X-Forwarded-User で区別する。
  認証は持たないので、待ち受けは前段からしか届かない場所 (127.0.0.1 など) にする。
  TR_USERINFO_URL を設定したときは、X-Forwarded-User が無く前段が検証した Bearer だけがあれば、
  IdP の userinfo から preferred_username を引く (access token にユーザー名が入らない IdP 向け)
  原文と訳はログにも残さない

Python の標準ライブラリだけを使う。
"""

from __future__ import annotations

import base64
import hashlib
import http.client
import json
import logging
import os
import queue
import re
import select
import signal
import socket
import socketserver
import sys
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from dataclasses import dataclass


def env_int(name: str, default: int) -> int:
    return int(os.environ.get(name, default))


LISTEN_HOST = os.environ.get("TR_LISTEN_HOST", "127.0.0.1")
LISTEN_PORT = env_int("TR_LISTEN_PORT", 8002)
STATIC_DIR = Path(os.environ.get("TR_STATIC_DIR", Path(__file__).resolve().parent / "static"))
# 翻訳エンジン (PLaMo 3 Translate を動かす llama-server の /v1/completions)。必須
UPSTREAM_URL = os.environ.get("TR_UPSTREAM_URL", "")
UPSTREAM_SLOTS = os.environ.get("TR_UPSTREAM_SLOTS", "4")
UPSTREAMS = os.environ.get("TR_UPSTREAMS", "")
POOL_KIND = os.environ.get("TR_POOL_KIND", "direct")
QUEUE_TIMEOUT = env_int("TR_QUEUE_TIMEOUT", 120)
AGING_SECONDS = env_int("TR_AGING_SECONDS", 30)
MAX_QUEUE = env_int("TR_MAX_QUEUE", 256)
API_KEY = os.environ.get("TR_API_KEY", "")
MODEL = os.environ.get("TR_MODEL", "plamo-3-translate")
# 1 回 (1 区切り) の原文の上限 (文字)。PLaMo のコンテキスト (16K トークン) は原文と訳の
# 合計で、約 6,000 トークンを超える原文は繰り返しに入る (README.md の「翻訳エンジン」)
MAX_CHARS = env_int("TR_MAX_CHARS", 4000)
# 文脈・前の訳・用語集を足した、プロンプト全体の上限 (文字)
MAX_PROMPT_CHARS = env_int("TR_MAX_PROMPT_CHARS", 12000)
MAX_GLOSSARY = env_int("TR_MAX_GLOSSARY", 200)
# 1 人が同時に流せる翻訳の数。翻訳エンジンが同時に処理できる数を 1 人で埋めない
MAX_ACTIVE_PER_USER = env_int("TR_MAX_ACTIVE_PER_USER", 2)
# 翻訳エンジンが答えないときに諦めるまでの秒数 (前の人の翻訳を待つ時間を含む)
UPSTREAM_TIMEOUT = env_int("TR_UPSTREAM_TIMEOUT", 600)
# 待っている間に送るコメント行の間隔 (Cloudflare は 100 秒応答の無い接続を切る)
HEARTBEAT_SECONDS = env_int("TR_HEARTBEAT_SECONDS", 15)
# IdP の userinfo (OIDC)。前段が X-Forwarded-User を付けず Bearer だけを通したとき、ここで利用者名を引く。
# Pocket ID のように access token にユーザー名が入らない IdP 向け。空なら使わない
USERINFO_URL = os.environ.get("TR_USERINFO_URL", "")
# userinfo の結果を覚えておく秒数の上限 (token の期限が先に来ればそこまで)
USERINFO_CACHE_SECONDS = env_int("TR_USERINFO_CACHE_SECONDS", 300)
# デスクトップへ入力する公開設定。内部の接続先や要求のヘッダーからは推測しない。
PUBLIC_SERVER_URL = os.environ.get("TR_PUBLIC_SERVER_URL", "")
DESKTOP_DISCOVERY_URL = os.environ.get("TR_DESKTOP_DISCOVERY_URL", "")
DESKTOP_CLIENT_ID = os.environ.get("TR_DESKTOP_CLIENT_ID", "translator-desktop")

# PLaMo 3 Translate の対応言語 (モデルカードの Supported Languages の順)
LANGUAGES = [
    ("ja", "日本語"),
    ("en", "英語"),
    ("zh-hans", "中国語 (簡体字)"),
    ("zh-hant", "中国語 (繁体字)"),
    ("ko", "韓国語"),
    ("ar", "アラビア語"),
    ("az", "アゼルバイジャン語"),
    ("be", "ベラルーシ語"),
    ("bg", "ブルガリア語"),
    ("bn", "ベンガル語"),
    ("ca", "カタルーニャ語"),
    ("ceb", "セブアノ語"),
    ("de", "ドイツ語"),
    ("es", "スペイン語"),
    ("et", "エストニア語"),
    ("eu", "バスク語"),
    ("fa", "ペルシア語"),
    ("fr", "フランス語"),
    ("gu", "グジャラート語"),
    ("he", "ヘブライ語"),
    ("hi", "ヒンディー語"),
    ("hr", "クロアチア語"),
    ("hu", "ハンガリー語"),
    ("hy", "アルメニア語"),
    ("id", "インドネシア語"),
    ("it", "イタリア語"),
    ("jv", "ジャワ語"),
    ("ka", "ジョージア語"),
    ("kk", "カザフ語"),
    ("km", "クメール語"),
    ("lt", "リトアニア語"),
    ("lv", "ラトビア語"),
    ("ml", "マラヤーラム語"),
    ("mr", "マラーティー語"),
    ("ms", "マレー語"),
    ("my", "ビルマ語"),
    ("ne", "ネパール語"),
    ("nl", "オランダ語"),
    ("pa", "パンジャーブ語"),
    ("pl", "ポーランド語"),
    ("ru", "ロシア語"),
    ("si", "シンハラ語"),
    ("sk", "スロバキア語"),
    ("sl", "スロベニア語"),
    ("sr", "セルビア語"),
    ("sw", "スワヒリ語"),
    ("ta", "タミル語"),
    ("te", "テルグ語"),
    ("th", "タイ語"),
    ("tl", "タガログ語"),
    ("ur", "ウルドゥー語"),
    ("uz", "ウズベク語"),
    ("vi", "ベトナム語"),
]
LANGUAGE_CODES = {code for code, _ in LANGUAGES}
FORMS = {"plain", "polite"}  # 訳す先が日本語のときの文体 (だ・である / です・ます)

USER_RE = re.compile(r"^[a-z_][a-z0-9_.-]{0,63}$")
# PFML3 の区切りのトークン。利用者の文に含まれていたら断る (プロンプトの形が崩れる)
RESERVED = "<|plamo:"

TAG, MSG, KEY, VAL = "<|plamo:tag|>", "<|plamo:msg|>", "<|plamo:key|>", "<|plamo:val|>"
INDEX, ITEM = "<|plamo:index|>", "<|plamo:item|>"
STOP = ["<|plamo:tag|>", "<|plamo:bos|>"]

log = logging.getLogger("translator")
active_lock = threading.Lock()
active: dict[str, int] = {}
page_active: dict[str, int] = {}
userinfo_lock = threading.Lock()
userinfo_cache: dict[str, tuple[str, float]] = {}  # token の SHA-256 → (利用者名, 期限 (monotonic))


class HttpError(Exception):
    def __init__(self, status: int, message: str, code=None):
        super().__init__(message)
        self.status = status
        self.message = message
        self.code = code


def token_expiry(token: str) -> float | None:
    """JWT の exp (UNIX 時刻)。署名は前段と userinfo が確かめるので、ここでは覚えておく期間にだけ使う。"""
    try:
        payload = token.split(".")[1]
        exp = json.loads(base64.urlsafe_b64decode(payload + "=" * (-len(payload) % 4)))["exp"]
        return float(exp)
    except (IndexError, KeyError, TypeError, ValueError):
        return None


def user_from_userinfo(token: str) -> str:
    """Bearer の token で IdP の userinfo を呼び、preferred_username を返す。結果はしばらく覚える。"""
    key = hashlib.sha256(token.encode()).hexdigest()
    now = time.monotonic()
    with userinfo_lock:
        hit = userinfo_cache.get(key)
        if hit and hit[1] > now:
            return hit[0]
    url = urllib.parse.urlsplit(USERINFO_URL)
    conn_cls = http.client.HTTPSConnection if url.scheme == "https" else http.client.HTTPConnection
    conn = conn_cls(url.hostname, url.port, timeout=15)
    try:
        conn.request(
            "GET",
            url.path or "/",
            headers={"Authorization": f"Bearer {token}", "Accept": "application/json"},
        )
        resp = conn.getresponse()
        body = resp.read(65536)
    except (OSError, http.client.HTTPException) as e:
        log.warning("userinfo failed: %s", e)
        raise HttpError(502, "認証サーバーに接続できませんでした")
    finally:
        conn.close()
    if resp.status != 200:
        raise HttpError(401, "ログインしていません")
    try:
        user = json.loads(body).get("preferred_username") or ""
    except (ValueError, AttributeError):
        user = ""
    if not isinstance(user, str) or not USER_RE.match(user):
        raise HttpError(401, "ログインしていません")
    ttl = USERINFO_CACHE_SECONDS
    exp = token_expiry(token)
    if exp is not None:
        ttl = min(ttl, exp - time.time())
    if ttl > 0:
        with userinfo_lock:
            if len(userinfo_cache) >= 1024:
                for stale in [k for k, (_, until) in userinfo_cache.items() if until <= now]:
                    del userinfo_cache[stale]
                if len(userinfo_cache) >= 1024:
                    userinfo_cache.clear()
            userinfo_cache[key] = (user, now + ttl)
    return user


def text_field(body: dict, name: str, label: str, limit: int, required: bool = False) -> str:
    value = body.get(name) or ""
    if not isinstance(value, str):
        raise HttpError(400, f"{label}が不正です")
    value = value.replace("\r\n", "\n")
    if required and not value.strip():
        raise HttpError(400, "原文が空です")
    if len(value) > limit:
        raise HttpError(413, f"{label}が長すぎます ({limit} 文字まで)")
    if RESERVED in value:
        raise HttpError(400, "使えない文字列 (<|plamo:) が含まれています")
    return value


def parse_request(body: dict) -> dict:
    """POST /api/translate の本文を確かめ、プロンプトを組むための値にする。"""
    text = text_field(body, "text", "原文", MAX_CHARS, required=True)
    target = body.get("target_lang")
    if target not in LANGUAGE_CODES:
        raise HttpError(400, "訳す先の言語が不正です")
    source = body.get("source_lang") or None
    if source is not None and source not in LANGUAGE_CODES:
        raise HttpError(400, "原文の言語が不正です")
    form = body.get("form") or None
    if form is not None and (form not in FORMS or target != "ja"):
        raise HttpError(400, "文体は訳す先が日本語のときだけ選べます")
    context = text_field(body, "context", "文脈", MAX_PROMPT_CHARS)
    glossary = []
    raw = body.get("glossary") or []
    if not isinstance(raw, list) or len(raw) > MAX_GLOSSARY:
        raise HttpError(400, f"用語集が不正です ({MAX_GLOSSARY} 件まで)")
    for item in raw:
        if not isinstance(item, dict):
            raise HttpError(400, "用語集が不正です")
        src = text_field(item, "source", "用語集の原語", 200).strip()
        dst = text_field(item, "target", "用語集の訳語", 200).strip()
        if src and dst:
            glossary.append((src, dst))
    history = []
    raw = body.get("history") or []
    if not isinstance(raw, list) or len(raw) > 4:
        raise HttpError(400, "前の訳が不正です")
    for item in raw:
        if not isinstance(item, dict):
            raise HttpError(400, "前の訳が不正です")
        src = text_field(item, "source", "前の原文", MAX_CHARS)
        dst = text_field(item, "target", "前の訳", MAX_CHARS * 3)
        if src.strip() and dst.strip():
            history.append((src, dst))
    req = {
        "text": text,
        "target": target,
        "source": source,
        "form": form,
        "context": context.strip(),
        "glossary": glossary,
        "history": history,
    }
    if len(build_prompt(req)) > MAX_PROMPT_CHARS:
        raise HttpError(413, "文脈・前の訳・用語集を合わせて長すぎます")
    return req


def build_prompt(req: dict) -> str:
    """PFML3 のプロンプト。順は dataset、context、前の原文と訳、glossary、原文、訳す先
    (モデルカードの "All options" の例と同じ)。BOS は llama-server が付ける。"""

    def lang(code: str | None) -> str:
        return f"{KEY}lang{VAL}{code}" if code else ""

    parts = [f"{TAG}dataset{MSG}translation"]
    if req["context"]:
        parts.append(f"{TAG}context{MSG}{req['context']}")
    for src, dst in req["history"]:
        parts.append(f"{TAG}input{lang(req['source'])}{MSG}{src}")
        parts.append(f"{TAG}output{lang(req['target'])}{MSG}{dst}")
    if req["glossary"]:
        parts.append(f"{TAG}glossary" + "".join(f"{INDEX}{s}{ITEM}{t}" for s, t in req["glossary"]))
    parts.append(f"{TAG}input{lang(req['source'])}{MSG}{req['text']}")
    form = f"{KEY}form{VAL}{req['form']}" if req["form"] else ""
    parts.append(f"{TAG}output{lang(req['target'])}{form}{MSG}")
    return "".join(parts)


def max_tokens(text: str) -> int:
    """訳の長さの上限。訳は原文の 1 倍前後のトークンになる。PLaMo が繰り返しに入っても
    すぐ止まるよう、原文の長さから決める (日本語は 1 文字およそ 0.5 トークン、英語は
    4 文字およそ 1 トークンなので、文字数の 1.5 倍で足りる)"""
    return max(256, min(8000, int(len(text) * 1.5) + 64))


PURPOSES = ("interactive", "page_visible", "page_rest")


def parse_upstreams(raw, fallback, fallback_slots="4"):
    if raw:
        entries = json.loads(raw)
    else:
        slots = int(fallback_slots)
        if isinstance(fallback_slots, bool) or str(slots) != str(fallback_slots):
            raise ValueError("TR_UPSTREAM_SLOTS は1〜1000の整数にしてください")
        entries = [{"url": fallback, "slots": slots}]
    if not isinstance(entries, list) or not entries:
        raise ValueError("TR_UPSTREAMS は空でない配列にしてください")
    result = []
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != {"url", "slots"}:
            raise ValueError("エンジンには url と slots を指定してください")
        url, slots = entry["url"], entry["slots"]
        if not isinstance(url, str):
            raise ValueError("エンジンの URL が不正です")
        parsed = urllib.parse.urlsplit(url)
        if (
            parsed.scheme not in ("http", "https")
            or not parsed.hostname
            or parsed.username is not None
            or parsed.password is not None
            or parsed.fragment
            or type(slots) is not int
            or not 1 <= slots <= 1000
        ):
            raise ValueError("エンジンの URL または枠の数が不正です")
        parsed.port  # 不正なポートも起動時に断る。
        result.append((url, slots))
    return result


@dataclass(eq=False)
class Job:
    user: str
    purpose: str
    created: float


class DirectPool:
    def __init__(self, upstreams, timeout=120, aging=30, max_queue=256):
        self.urls = [url for url, _ in upstreams]
        self.free = [slots for _, slots in upstreams]
        self.timeout = timeout
        self.aging = aging
        self.max_queue = max_queue
        self.condition = threading.Condition()
        self.waiting = []
        self.pages = set()
        self.turns = {}
        self.cursor = 0

    def submit(self, user, purpose):
        with self.condition:
            if len(self.waiting) >= self.max_queue:
                return None
            job = Job(user, purpose, time.monotonic())
            self.waiting.append(job)
            self.condition.notify_all()
            return job

    def _next(self, now=None):
        now = time.monotonic() if now is None else now
        eligible = [
            j for j in self.waiting if j.purpose == "interactive" or j.user not in self.pages
        ]
        if not eligible:
            return None

        def priority(j):
            return max(0, PURPOSES.index(j.purpose) - int(now - j.created > self.aging))

        rank = min(map(priority, eligible))
        candidates = [j for j in eligible if priority(j) == rank]
        users = list(dict.fromkeys(j.user for j in candidates))
        turns = [user for user in self.turns.get(rank, []) if user in users]
        turns.extend(user for user in users if user not in turns)
        self.turns[rank] = turns
        return next(j for j in candidates if j.user == turns[0])

    def acquire(self, job, cancel):
        with self.condition:
            while True:
                if cancel.is_set():
                    self._remove(job)
                    return None
                if time.monotonic() - job.created >= self.timeout:
                    self._remove(job)
                    raise TimeoutError
                now = time.monotonic()
                if any(self.free) and self._next(now) is job:
                    index = next(
                        (self.cursor + n) % len(self.free)
                        for n in range(len(self.free))
                        if self.free[(self.cursor + n) % len(self.free)]
                    )
                    self.free[index] -= 1
                    self.cursor = (index + 1) % len(self.free)
                    rank = max(
                        0,
                        PURPOSES.index(job.purpose) - int(now - job.created > self.aging),
                    )
                    turns = self.turns[rank]
                    turns.remove(job.user)
                    turns.append(job.user)
                    if job.purpose != "interactive":
                        self.pages.add(job.user)
                    self._remove(job)
                    return index
                self.condition.wait(0.1)

    def _remove(self, job):
        self.waiting.remove(job)
        self.condition.notify_all()

    def release(self, job, index):
        with self.condition:
            self.free[index] += 1
            if job.purpose != "interactive":
                self.pages.discard(job.user)
            self.condition.notify_all()


class UpstreamCancellation:
    """読み取り待ちでも中止できるよう、上流ソケットを保持する。"""

    def __init__(self):
        self._event = threading.Event()
        self._lock = threading.Lock()
        self._socket = None

    def is_set(self):
        return self._event.is_set()

    def set(self):
        self._event.set()
        with self._lock:
            if self._socket is not None:
                self._shutdown(self._socket)

    def attach(self, sock):
        with self._lock:
            self._socket = sock
            if self._event.is_set():
                self._shutdown(sock)

    def detach(self):
        with self._lock:
            self._socket = None

    @staticmethod
    def _shutdown(sock):
        try:
            sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass  # 既に切断されている。


def release_active(user, purpose="interactive"):
    with active_lock:
        active[user] -= 1
        if not active[user]:
            del active[user]
        if purpose != "interactive":
            page_active[user] -= 1
            if not page_active[user]:
                del page_active[user]


def run_upstream_for_user(user, prompt, limit, events, cancel, pool, job):
    index = None
    try:
        index = pool.acquire(job, cancel)
        if index is not None:
            upstream_stream(prompt, limit, events, cancel, pool.urls[index])
    except TimeoutError:
        events.put(("queue_timeout", "翻訳の順番待ちが長すぎます。しばらくしてから送ってください"))
    finally:
        if index is not None:
            pool.release(job, index)
        # HTTPハンドラーではなく、上流通信の終了時点で枠を返す。
        release_active(user, job.purpose)


def upstream_stream(
    prompt: str, limit: int, events: queue.Queue, cancel: UpstreamCancellation, upstream_url: str
):
    """翻訳エンジンに投げ、届いた訳の断片を events に入れる。最後に ("done", 理由) か
    ("error", 文) を入れる。cancel が立ったら接続を切る (llama-server は生成を止める)。"""
    url = urllib.parse.urlsplit(upstream_url)
    body = json.dumps(
        {
            "model": MODEL,
            "prompt": prompt,
            "max_tokens": limit,
            "temperature": 0,
            "stop": STOP,
            "add_special_tokens": True,
            "stream": True,
        },
        ensure_ascii=False,
    ).encode()
    conn_cls = http.client.HTTPSConnection if url.scheme == "https" else http.client.HTTPConnection
    # 接続確立までの待ちを短くし、確立後は通常の読み取り時限に戻す。
    conn = conn_cls(url.hostname, url.port, timeout=min(5, UPSTREAM_TIMEOUT))
    resp = None
    try:
        if cancel.is_set():
            return
        conn.connect()
        cancel.attach(conn.sock)
        if cancel.is_set():
            return
        conn.sock.settimeout(UPSTREAM_TIMEOUT)
        conn.timeout = UPSTREAM_TIMEOUT
        conn.request(
            "POST",
            url.path,
            body=body,
            headers={
                "Content-Type": "application/json",
                **({"Authorization": f"Bearer {API_KEY}"} if API_KEY else {}),
                "Accept": "text/event-stream",
            },
        )
        resp = conn.getresponse()
        if resp.status != 200:
            log.warning("upstream returned %d", resp.status)
            events.put(("error", f"翻訳サーバーがエラーを返しました ({resp.status})"))
            return
        finish = None
        for raw in resp:
            if cancel.is_set():
                return
            line = raw.decode(errors="replace").strip()
            if not line.startswith("data:"):
                continue
            data = line[5:].strip()
            if data == "[DONE]":
                break
            try:
                chunk = json.loads(data)
                choice = chunk["choices"][0]
            except (ValueError, KeyError, IndexError):
                continue
            if choice.get("text"):
                events.put(("text", choice["text"]))
            if choice.get("finish_reason"):
                finish = choice["finish_reason"]
        if finish in ("stop", "length"):
            events.put(("done", finish))
        else:
            events.put(("error", "翻訳サーバーとの接続が途中で切れました"))
    except (OSError, http.client.HTTPException) as e:
        if not cancel.is_set():
            log.warning("upstream failed: %s", type(e).__name__)
            events.put(("error", "翻訳サーバーに接続できませんでした"))
    finally:
        cancel.detach()
        if resp is not None:
            resp.close()
        conn.close()


class TranslationHTTPServer(ThreadingHTTPServer):
    def get_pool(self):
        with active_lock:
            if not hasattr(self, "pool"):
                if POOL_KIND != "direct":
                    raise ValueError("ボランティア枠はまだ実装していません")
                if min(QUEUE_TIMEOUT, AGING_SECONDS, MAX_QUEUE) <= 0:
                    raise ValueError("待ち行列の設定は正の整数にしてください")
                self.pool = DirectPool(
                    parse_upstreams(UPSTREAMS, UPSTREAM_URL, UPSTREAM_SLOTS),
                    QUEUE_TIMEOUT,
                    AGING_SECONDS,
                    MAX_QUEUE,
                )
            return self.pool

    def server_bind(self):
        # HTTPServer.server_bind の getfqdn による逆引きは不要。
        # macOS のランナーでは名前解決待ちで起動が遅れることがある。
        socketserver.TCPServer.server_bind(self)
        self.server_name, self.server_port = self.server_address[:2]


class Handler(BaseHTTPRequestHandler):
    server_version = "translator"
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):  # アクセスログは nginx が取る
        pass

    def user(self) -> str:
        user = self.headers.get("X-Forwarded-User", "")
        if not user and USERINFO_URL:
            scheme, _, token = self.headers.get("Authorization", "").partition(" ")
            if scheme.lower() == "bearer" and token.strip():
                return user_from_userinfo(token.strip())
        if not USER_RE.match(user):
            raise HttpError(401, "ログインしていません")
        return user

    def send_json(self, status: int, data) -> None:
        body = json.dumps(data, ensure_ascii=False).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        if status == 503:
            self.send_header("Retry-After", "5")
        self.send_header("Cache-Control", "private, no-store")
        if status >= 400:
            self.send_header("Connection", "close")
            self.close_connection = True
        self.end_headers()
        self.wfile.write(body)

    def read_json_body(self) -> dict:
        if not self.headers.get("Content-Type", "").startswith("application/json"):
            # 別のサイトのフォームから送らせない (JSON はプリフライトが要る)
            raise HttpError(415, "Content-Type は application/json にしてください")
        length = int(self.headers.get("Content-Length") or 0)
        limit = (MAX_PROMPT_CHARS + MAX_CHARS * 4) * 4 + 64 * 1024
        if length > limit:
            raise HttpError(413, "リクエストが大きすぎます")
        try:
            data = json.loads(self.rfile.read(length) or b"{}")
        except ValueError:
            raise HttpError(400, "JSON が不正です")
        if not isinstance(data, dict):
            raise HttpError(400, "JSON が不正です")
        return data

    def dispatch(self, method: str) -> None:
        path = urllib.parse.urlsplit(self.path).path
        try:
            if method == "GET" and path in ("/", "/index.html"):
                return self.serve_static("index.html", "text/html; charset=utf-8")
            if method == "GET" and path == "/translation.mjs":
                return self.serve_static("translation.mjs", "text/javascript; charset=utf-8")
            if method == "GET" and path == "/logo.png":
                return self.serve_static("logo.png", "image/png")
            if method == "GET" and path == "/favicon.ico":
                return self.serve_static("favicon.ico", "image/vnd.microsoft.icon")
            if not path.startswith("/api/"):
                raise HttpError(404, "Not found")
            user = self.user()
            if method == "GET" and path == "/api/config":
                return self.send_json(
                    200,
                    {
                        "user": user,
                        "pool": {"kind": "direct"},
                        "tier": "registered",
                        "languages": [{"code": c, "name": n} for c, n in LANGUAGES],
                        "max_chars": MAX_CHARS,
                        "max_glossary": MAX_GLOSSARY,
                        "concurrency": {
                            "max_in_flight": MAX_ACTIVE_PER_USER,
                            "scope": "user",
                        },
                        "desktop_connection": {
                            "server_url": PUBLIC_SERVER_URL,
                            "discovery_url": DESKTOP_DISCOVERY_URL,
                            "client_id": DESKTOP_CLIENT_ID,
                        },
                    },
                )
            if method == "POST" and path == "/api/translate":
                return self.translate(user)
            raise HttpError(404, "Not found")
        except HttpError as e:
            self.send_json(e.status, {"error": e.message, **({"code": e.code} if e.code else {})})
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass
        except Exception:
            log.exception("request %s %s failed", method, path)
            try:
                self.send_json(500, {"error": "サーバーの内部エラーです"})
            except Exception:
                pass

    def do_GET(self):
        self.dispatch("GET")

    def do_POST(self):
        self.dispatch("POST")

    def serve_static(self, filename: str, content_type: str) -> None:
        body = (STATIC_DIR / filename).read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
        self.wfile.write(body)

    def sse(self, event: dict | None) -> None:
        line = (
            ": ping\n\n" if event is None else f"data: {json.dumps(event, ensure_ascii=False)}\n\n"
        )
        self.wfile.write(line.encode())
        self.wfile.flush()

    def client_disconnected(self):
        try:
            readable, _, _ = select.select([self.connection], [], [], 0)
            return bool(readable) and not self.connection.recv(1, socket.MSG_PEEK)
        except (OSError, ValueError):
            return True

    def translate(self, user: str) -> None:
        body = self.read_json_body()
        purpose = body.get("purpose", "interactive")
        if purpose not in PURPOSES:
            raise HttpError(400, "purpose が不正です")
        group = body.get("group")
        if group is not None and (
            not isinstance(group, str) or not re.fullmatch(r"[A-Za-z0-9_-]{22,128}", group)
        ):
            raise HttpError(400, "group が不正です")
        req = parse_request(body)
        pool = self.server.get_pool()
        with active_lock:
            if active.get(user, 0) >= MAX_ACTIVE_PER_USER or (
                purpose != "interactive"
                and page_active.get(user, 0) >= max(1, MAX_ACTIVE_PER_USER - 1)
            ):
                raise HttpError(
                    429,
                    "同時に訳せる数を超えています。前の翻訳が終わってから送ってください",
                )
            job = pool.submit(user, purpose)
            if job is None:
                raise HttpError(503, "翻訳の待ち行列が一杯です", "busy")
            active[user] = active.get(user, 0) + 1
            if purpose != "interactive":
                page_active[user] = page_active.get(user, 0) + 1
        started = time.monotonic()
        events: queue.Queue = queue.Queue()
        cancel = UpstreamCancellation()
        worker = threading.Thread(
            target=run_upstream_for_user,
            args=(user, build_prompt(req), max_tokens(req["text"]), events, cancel, pool, job),
            daemon=True,
        )
        try:
            worker.start()
        except Exception:
            with pool.condition:
                pool._remove(job)
            release_active(user, purpose)
            raise
        finish, out_chars = "error", 0
        try:
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream; charset=utf-8")
            self.send_header("Cache-Control", "private, no-store")
            # nginx に溜めさせない (届いた分をすぐ画面に出す)
            self.send_header("X-Accel-Buffering", "no")
            self.send_header("Connection", "close")
            self.close_connection = True
            self.end_headers()
            next_ping = time.monotonic() + HEARTBEAT_SECONDS
            while True:
                # 上流が無応答でも、pingの間隔を待たずにクライアントの切断を拾う。
                if self.client_disconnected():
                    finish = "canceled"
                    break
                try:
                    kind, value = events.get(timeout=min(0.1, max(0, next_ping - time.monotonic())))
                except queue.Empty:
                    if time.monotonic() >= next_ping:
                        self.sse(None)
                        next_ping = time.monotonic() + HEARTBEAT_SECONDS
                    continue
                if kind == "text":
                    out_chars += len(value)
                    self.sse({"t": value})
                    next_ping = time.monotonic() + HEARTBEAT_SECONDS
                elif kind == "done":
                    finish = value
                    self.sse({"done": True, "finish": value})
                    break
                else:
                    self.sse(
                        {"error": value, **({"code": kind} if kind == "queue_timeout" else {})}
                    )
                    break
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            finish = "canceled"
        finally:
            cancel.set()
            # 原文と訳は残さない (長さと言語だけ)
            log.info(
                "user=%s %s->%s chars=%d out=%d glossary=%d history=%d finish=%s seconds=%.1f",
                user,
                req["source"] or "auto",
                req["target"],
                len(req["text"]),
                out_chars,
                len(req["glossary"]),
                len(req["history"]),
                finish,
                time.monotonic() - started,
            )


class JournalFormatter(logging.Formatter):
    """journald が優先度として読む <N> を行頭に付ける (SyslogLevelPrefix)。"""

    PRIORITY = {"DEBUG": 7, "INFO": 6, "WARNING": 4, "ERROR": 3, "CRITICAL": 2}

    def format(self, record):
        return f"<{self.PRIORITY.get(record.levelname, 6)}>{super().format(record)}"


def main() -> None:
    started = time.monotonic()
    handler = logging.StreamHandler(sys.stdout)
    handler.setFormatter(JournalFormatter("%(message)s"))
    logging.basicConfig(level=logging.INFO, handlers=[handler])
    if not UPSTREAM_URL and not UPSTREAMS:
        log.error("TR_UPSTREAM_URL is not set")
        sys.exit(2)
    if not API_KEY:
        log.warning("TR_API_KEY is empty; requests are sent without Authorization")
    server = TranslationHTTPServer((LISTEN_HOST, LISTEN_PORT), Handler)
    try:
        pool = server.get_pool()
    except (ValueError, TypeError) as error:
        log.error("プールの設定が不正です: %s", error)
        server.server_close()
        sys.exit(2)
    server.daemon_threads = True
    signal.signal(
        signal.SIGTERM,
        lambda *_: threading.Thread(target=server.shutdown, daemon=True).start(),
    )
    log.info(
        "listening on %s:%d engines=%d slots=%d startup_seconds=%.3f",
        LISTEN_HOST,
        server.server_port,
        len(pool.urls),
        sum(pool.free),
        time.monotonic() - started,
    )
    server.serve_forever()


if __name__ == "__main__":
    main()
