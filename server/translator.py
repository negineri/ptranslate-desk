#!/usr/bin/env python3
"""PLaMo 3 Translate で文章を訳す Web アプリのサーバー (ptranslate-desk)。

使い方と設定は README.md、API は docs/SPEC.md の「サーバーの API」。

- 画面が原文を段落ごとに区切り、1 区切りずつ POST /api/translate に送る
- サーバーは PFML3 (PLaMo の素の書式) のプロンプトを組み、翻訳エンジン (PLaMo 3 Translate を
  動かす llama-server の /v1/completions) にストリーミングで投げ、届いた分を Server-Sent Events で返す。
  PLaMo は指示に従わない翻訳専用のモデルなので、プロンプトに指示の文を入れない
- 待っている間も 15 秒ごとにコメント行を送る (Cloudflare は 100 秒応答の無い接続を切る)
- 利用者は前段の認証プロキシ (oauth2-proxy など) が付ける X-Forwarded-User で区別する。
  OIDC の認証は前段で行う。匿名を有効にした場合は仲介が署名した資格を検証する。
  待ち受けは前段からしか届かない場所 (127.0.0.1 など) にする。
  TR_USERINFO_URL を設定したときは、X-Forwarded-User が無く前段が検証した Bearer だけがあれば、
  IdP の userinfo から preferred_username を引く (access token にユーザー名が入らない IdP 向け)
  原文と訳はログにも残さない

Python の標準ライブラリだけを使う。
"""

from __future__ import annotations

import base64
import hashlib
import hmac
import ipaddress
import secrets
import http.client
import json
import math
import logging
import os
import queue
import re
import select
import signal
import socket
import socketserver
import sqlite3
import stat
import unicodedata
import sys
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from dataclasses import dataclass
from collections import Counter, deque
from concurrent.futures import ThreadPoolExecutor
from http.cookies import SimpleCookie


def env_int(name: str, default: int) -> int:
    return int(os.environ.get(name, default))


LISTEN_HOST = os.environ.get("TR_LISTEN_HOST", "127.0.0.1")
LISTEN_PORT = env_int("TR_LISTEN_PORT", 8002)
STATIC_DIR = Path(os.environ.get("TR_STATIC_DIR", Path(__file__).resolve().parent / "static"))
# 翻訳エンジン (PLaMo 3 Translate を動かす llama-server の /v1/completions)。必須
UPSTREAM_URL = os.environ.get("TR_UPSTREAM_URL", "")
UPSTREAM_SLOTS = os.environ.get("TR_UPSTREAM_SLOTS", "4")
UPSTREAMS = os.environ.get("TR_UPSTREAMS", "")
ANON_ENABLED = os.environ.get("TR_ANON_ENABLED", "0") == "1"
TOKEN_KEY = os.environ.get("TR_TOKEN_KEY", "").encode()
ANON_TTL = 30 * 86400
ANON_SHARE = float(os.environ.get("TR_ANON_SHARE", "0.5"))
ANON_ISSUE_PER_MINUTE = env_int("TR_ANON_ISSUE_PER_MINUTE", 100)
CLIENT_IP_HEADER = os.environ.get("TR_CLIENT_IP_HEADER", "")
COUNTRY_HEADER = os.environ.get("TR_COUNTRY_HEADER", "")
ALLOWED_COUNTRIES = frozenset(os.environ.get("TR_ALLOWED_COUNTRIES", "").split(",")) - {""}
POOL_KIND = os.environ.get("TR_POOL_KIND", "direct")
CONSENT_VERSION = 1
SPREAD_WINDOW = os.environ.get("TR_SPREAD_WINDOW", "8")
OWNER_SHARE_MAX = os.environ.get("TR_OWNER_SHARE_MAX", "0.2")
QUEUE_TIMEOUT = env_int("TR_QUEUE_TIMEOUT", 120)
AGING_SECONDS = env_int("TR_AGING_SECONDS", 30)
MAX_QUEUE = env_int("TR_MAX_QUEUE", 256)
MAX_OPEN_REQUESTS = env_int("TR_MAX_OPEN_REQUESTS", 1000)
STATE_FILE = os.environ.get("TR_STATE_FILE", "")
WORKERS_PER_OWNER = env_int("TR_WORKERS_PER_OWNER", 2)
WORKER_MODEL_SHA256 = os.environ.get("TR_WORKER_MODEL_SHA256", "")
MIN_AGENT_VERSION = os.environ.get("TR_MIN_AGENT_VERSION", "")
AUDIT_UPSTREAMS = os.environ.get("TR_AUDIT_UPSTREAMS", "")
CANARY_FILE = os.environ.get("TR_CANARY_FILE", "")
CANARY_RATE = os.environ.get("TR_CANARY_RATE", "0.05")
AUDIT_RATE = os.environ.get("TR_AUDIT_RATE", "0.02")
AUDIT_BACKLOG = os.environ.get("TR_AUDIT_BACKLOG", "50")
PROBATION_JOBS = env_int("TR_PROBATION_JOBS", 20)
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


class WindowCounter:
    """時間窓の集計だけをメモリに保持。満杯時に生きた制限を消さない。"""

    def __init__(self, seconds, capacity=10000):
        self.seconds = seconds
        self.capacity = capacity
        self.entries = {}
        self.lock = threading.Lock()

    def charge(self, key, amount, limit):
        now = time.monotonic()
        with self.lock:
            for old in list(self.entries):
                buckets, total = self.entries[old]
                while buckets and buckets[0][0] <= now - self.seconds:
                    total -= buckets.popleft()[1]
                if buckets:
                    self.entries[old] = (buckets, total)
                else:
                    del self.entries[old]
            buckets, total = self.entries.get(key, (deque(), 0))
            if total + amount > limit:
                raise HttpError(429, "利用の上限に達しました。しばらくしてから送ってください")
            if key not in self.entries and len(self.entries) >= self.capacity:
                raise HttpError(503, "利用が集中しています", "busy")
            # 秒単位でまとめ、期限は最後の要求に合わせる（最大1秒保守的）。
            if buckets and int(buckets[-1][0]) == int(now):
                _, count = buckets.pop()
                buckets.append((now, count + amount))
            else:
                buckets.append((now, amount))
            self.entries[key] = (buckets, total + amount)


def anonymous_credential():
    payload = f"pta_{secrets.token_hex(16)}.{int(time.time()) + ANON_TTL}"
    signature = hmac.new(TOKEN_KEY, payload.encode(), hashlib.sha256).hexdigest()
    return f"{payload}.{signature}"


def verify_anonymous(token):
    if not ANON_ENABLED or not re.fullmatch(r"pta_[0-9a-f]{32}\.[0-9]{1,12}\.[0-9a-f]{64}", token):
        raise HttpError(401, "匿名の資格が無効です。取り直してください")
    payload, signature = token.rsplit(".", 1)
    expected = hmac.new(TOKEN_KEY, payload.encode(), hashlib.sha256).hexdigest()
    if not hmac.compare_digest(signature, expected) or int(payload.split(".")[1]) <= time.time():
        raise HttpError(401, "匿名の資格が無効です。取り直してください")
    return "anonymous:" + payload.split(".")[0][4:]  # 登録済みの利用者名と衝突させない。


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
        log.warning("userinfo failed: %s", type(e).__name__)
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
    tier: str = "registered"
    network: str = ""
    group: str | None = None


class DirectPool:
    def __init__(self, upstreams, timeout=120, aging=30, max_queue=256):
        self.urls = [url for url, _ in upstreams]
        self.free = [slots for _, slots in upstreams]
        self.total_slots = sum(self.free)
        self.anonymous_running = 0
        self.network_running = {}
        self.tier_turn = {}
        self.timeout = timeout
        self.aging = aging
        self.max_queue = max_queue
        self.condition = threading.Condition()
        self.waiting = []
        self.pages = set()
        self.turns = {}
        self.cursor = 0

    def submit(self, user, purpose, tier="registered", network=""):
        with self.condition:
            if len(self.waiting) >= self.max_queue:
                return None
            if sum(j.user == user for j in self.waiting) >= (4 if tier == "anonymous" else 8):
                raise HttpError(429, "待ち行列に置ける数を超えています")
            job = Job(user, purpose, time.monotonic(), tier, network)
            self.waiting.append(job)
            self.condition.notify_all()
            return job

    def _next(self, now=None):
        now = time.monotonic() if now is None else now
        eligible = [
            j for j in self.waiting if j.purpose == "interactive" or j.user not in self.pages
        ]
        registered_waiting = any(j.tier == "registered" for j in self.waiting)
        # 1枠のプールでも匿名を進めるため、比率の枠は最低1とする。
        anonymous_limit = max(1, int(self.total_slots * ANON_SHARE))
        eligible = [
            j
            for j in eligible
            if j.tier != "anonymous"
            or (
                (not registered_waiting or self.anonymous_running < anonymous_limit)
                and (not j.network or self.network_running.get(j.network, 0) < 4)
            )
        ]
        if not eligible:
            return None

        def priority(j):
            return max(0, PURPOSES.index(j.purpose) - int(now - j.created > self.aging))

        rank = min(map(priority, eligible))
        candidates = [j for j in eligible if priority(j) == rank]
        tiers = {j.tier for j in candidates}
        tier = (
            ("anonymous" if self.tier_turn.get(rank, 0) == 4 else "registered")
            if len(tiers) == 2
            else next(iter(tiers))
        )
        candidates = [j for j in candidates if j.tier == tier]
        rank = (rank, tier)
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
                    self._begin(job, now)
                    return index
                self.condition.wait(0.1)

    def _begin(self, job, now):
        rank = max(
            0,
            PURPOSES.index(job.purpose) - int(now - job.created > self.aging),
        )
        self.tier_turn[rank] = (self.tier_turn.get(rank, 0) + 1) % 5
        turns = self.turns[(rank, job.tier)]
        if job.tier == "anonymous":
            self.anonymous_running += 1
            if job.network:
                self.network_running[job.network] = self.network_running.get(job.network, 0) + 1
        turns.remove(job.user)
        turns.append(job.user)
        if job.purpose != "interactive":
            self.pages.add(job.user)
        self._remove(job)

    def _remove(self, job):
        self.waiting.remove(job)
        self.condition.notify_all()

    def release(self, job, index):
        with self.condition:
            self.free[index] += 1
            self._end(job)

    def _end(self, job):
        if job.tier == "anonymous":
            self.anonymous_running -= 1
            if job.network:
                self.network_running[job.network] -= 1
                if not self.network_running[job.network]:
                    del self.network_running[job.network]
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

    def wait(self, timeout):
        return self._event.wait(timeout)

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
    def get_workers(self):
        with self.worker_lock:
            if self.workers is None:
                if not STATE_FILE:
                    raise HttpError(404, "ワーカーの登録は無効です")
                registry = WorkerRegistry(STATE_FILE, WORKERS_PER_OWNER)
                hub = None
                try:
                    hub = WorkerHub(registry, WORKER_MODEL_SHA256, MIN_AGENT_VERSION)
                    if CANARY_FILE or AUDIT_UPSTREAMS:
                        if not CANARY_FILE or not AUDIT_UPSTREAMS:
                            raise ValueError("確認用ジョブと検証用エンジンは両方指定してください")
                        hub.checks = CanaryChecks(
                            hub, parse_upstreams(AUDIT_UPSTREAMS, ""), CANARY_FILE, PROBATION_JOBS
                        )
                        hub.checks.start()
                    self.workers = hub
                except Exception:
                    if hub is not None:
                        hub.close()
                    else:
                        registry.close()
                    raise
            return self.workers

    def server_close(self):
        super().server_close()
        if isinstance(getattr(self, "pool", None), VolunteerPool):
            self.pool.close()
        if self.workers is not None:
            self.workers.close()
            self.workers = None

    def process_request(self, request, client_address):
        if not self.open_requests.acquire(blocking=False):
            try:
                request.settimeout(1)
                request.sendall(
                    b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\nContent-Length: 0\r\nRetry-After: 5\r\n\r\n"
                )
            except OSError:
                pass
            finally:
                self.shutdown_request(request)
            return
        try:
            super().process_request(request, client_address)
        except BaseException:
            self.open_requests.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self.open_requests.release()

    def handle_error(self, request, client_address):
        log.error("HTTP接続の処理に失敗しました")

    def get_pool(self):
        with active_lock:
            if not hasattr(self, "pool"):
                if ANON_ENABLED and (len(TOKEN_KEY) < 32 or ANON_ISSUE_PER_MINUTE <= 0):
                    raise ValueError("匿名の資格には32バイト以上の鍵と正の発行上限が必要です")
                if ANON_ENABLED and PUBLIC_SERVER_URL:
                    public = urllib.parse.urlsplit(PUBLIC_SERVER_URL)
                    if (
                        public.scheme != "https"
                        or not public.hostname
                        or public.username is not None
                        or public.password is not None
                        or public.path
                        or public.query
                        or public.fragment
                    ):
                        raise ValueError("匿名のCookieにはHTTPSの公開オリジンを指定してください")
                    public.port
                if not 0 < ANON_SHARE <= 1:
                    raise ValueError("TR_ANON_SHARE は0より大きく1以下にしてください")
                if ALLOWED_COUNTRIES and not COUNTRY_HEADER:
                    raise ValueError("国の制限には TR_COUNTRY_HEADER が必要です")
                if min(QUEUE_TIMEOUT, AGING_SECONDS, MAX_QUEUE) <= 0:
                    raise ValueError("待ち行列の設定は正の整数にしてください")
                if POOL_KIND == "volunteer":
                    if not (
                        STATE_FILE
                        and CANARY_FILE
                        and AUDIT_UPSTREAMS
                        and CLIENT_IP_HEADER
                        and COUNTRY_HEADER
                        and ALLOWED_COUNTRIES == {"JP"}
                    ):
                        raise ValueError(
                            "ボランティア枠には検証・登録・日本限定の前段設定が必要です"
                        )
                    self.pool = VolunteerPool(
                        self.get_workers(), QUEUE_TIMEOUT, AGING_SECONDS, MAX_QUEUE
                    )
                elif POOL_KIND == "direct":
                    self.pool = DirectPool(
                        parse_upstreams(UPSTREAMS, UPSTREAM_URL, UPSTREAM_SLOTS),
                        QUEUE_TIMEOUT,
                        AGING_SECONDS,
                        MAX_QUEUE,
                    )
                else:
                    raise ValueError("TR_POOL_KIND が不正です")
            return self.pool

    def server_bind(self):
        self.worker_lock = threading.Lock()
        self.workers = None
        if MAX_OPEN_REQUESTS <= 0:
            raise ValueError("TR_MAX_OPEN_REQUESTS は正の整数にしてください")
        self.open_requests = threading.BoundedSemaphore(MAX_OPEN_REQUESTS)
        self.issue_global = WindowCounter(60)
        self.issue_network = WindowCounter(3600)
        self.character_limits_registered = WindowCounter(3600)
        self.character_limits_anonymous = WindowCounter(3600)
        # HTTPServer.server_bind の getfqdn による逆引きは不要。
        # macOS のランナーでは名前解決待ちで起動が遅れることがある。
        socketserver.TCPServer.server_bind(self)
        self.server_name, self.server_port = self.server_address[:2]


class Handler(BaseHTTPRequestHandler):
    server_version = "translator"
    protocol_version = "HTTP/1.1"
    timeout = 120

    def log_message(self, fmt, *args):  # アクセスログは nginx が取る
        pass

    def user(self) -> str:
        user = self.headers.get("X-Forwarded-User", "")
        self.tier = "registered"
        scheme, _, token = self.headers.get("Authorization", "").partition(" ")
        token = token.strip()
        if scheme.lower() == "bearer" and token.startswith("ptw_"):
            raise HttpError(401, "ワーカーの資格では利用できません")
        if not user and scheme.lower() == "bearer" and token.startswith("pta_"):
            self.tier = "anonymous"
            return verify_anonymous(token)
        if not user and not self.headers.get("Authorization") and self.headers.get("Cookie"):
            cookies = SimpleCookie()
            try:
                cookies.load(self.headers["Cookie"])
            except Exception:
                raise HttpError(401, "Cookie が不正です")
            if "__Host-pta" in cookies:
                self.check_origin()
                self.tier = "anonymous"
                return verify_anonymous(cookies["__Host-pta"].value)
        if not user and USERINFO_URL:
            if scheme.lower() == "bearer" and token.strip():
                return user_from_userinfo(token.strip())
        if not USER_RE.match(user):
            raise HttpError(401, "ログインしていません")
        return user

    def check_origin(self):
        origin = self.headers.get("Origin")
        if PUBLIC_SERVER_URL and (
            origin == PUBLIC_SERVER_URL
            or (
                origin is None
                and self.headers.get("Sec-Fetch-Site") == "same-origin"
                and self.command in ("GET", "HEAD")
            )
        ):
            return
        raise HttpError(403, "送信元を確認できません")

    def client_network(self):
        if not CLIENT_IP_HEADER:
            return ""
        try:
            address = ipaddress.ip_address(self.headers.get(CLIENT_IP_HEADER, ""))
            if isinstance(address, ipaddress.IPv6Address) and address.ipv4_mapped:
                address = address.ipv4_mapped
            return str(
                ipaddress.ip_network(
                    f"{address}/{24 if address.version == 4 else 48}", strict=False
                )
            )
        except ValueError:
            raise HttpError(403, "接続元を確認できません")

    def issue_anonymous(self):
        if not ANON_ENABLED:
            raise HttpError(404, "Not found")
        self.server.get_pool()  # 鍵と設定の検証を発行前に行う。
        if self.read_json_body() != {}:
            raise HttpError(400, "本文は空のJSONオブジェクトにしてください")
        origin = self.headers.get("Origin")
        web = bool(PUBLIC_SERVER_URL) and origin == PUBLIC_SERVER_URL
        if not web and origin is not None and not origin.startswith("chrome-extension://"):
            raise HttpError(403, "送信元を確認できません")
        network = self.client_network()
        try:
            self.server.issue_global.charge("global", 1, ANON_ISSUE_PER_MINUTE)
        except HttpError as error:
            if error.status == 429:
                raise HttpError(503, "資格の発行が集中しています", "busy")
            raise
        if network:
            self.server.issue_network.charge(network, 1, 5)
        credential = anonymous_credential()
        self.response_cookie = (
            f"__Host-pta={credential}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age={ANON_TTL}"
            if web
            else None
        )
        self.send_json(200, {"expires_in": ANON_TTL, **({} if web else {"credential": credential})})

    def send_json(self, status: int, data) -> None:
        body = json.dumps(data, ensure_ascii=False).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        if getattr(self, "response_cookie", None):
            self.send_header("Set-Cookie", self.response_cookie)
            self.response_cookie = None
        if status == 503:
            self.send_header("Retry-After", "5")
        self.send_header("Cache-Control", "private, no-store")
        if status >= 400:
            self.send_header("Connection", "close")
            self.close_connection = True
        self.end_headers()
        self.wfile.write(body)

    def read_json_body(self, max_bytes=None) -> dict:
        if (
            self.headers.get("Content-Type", "").split(";", 1)[0].strip().lower()
            != "application/json"
        ):
            # 別のサイトのフォームから送らせない (JSON はプリフライトが要る)
            raise HttpError(415, "Content-Type は application/json にしてください")
        try:
            length = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            raise HttpError(400, "Content-Length が不正です")
        if length < 0 or self.headers.get("Transfer-Encoding"):
            raise HttpError(400, "要求の長さが不正です")
        limit = (MAX_PROMPT_CHARS + MAX_CHARS * 4) * 4 + 64 * 1024
        if length > (limit if max_bytes is None else max_bytes):
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
            if ALLOWED_COUNTRIES and self.headers.get(COUNTRY_HEADER, "") not in ALLOWED_COUNTRIES:
                raise HttpError(403, "この地域からは利用できません", "region")
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
            if path.startswith("/api/worker/"):
                return self.worker_api(method, path)
            if method == "POST" and path == "/api/anonymous":
                return self.issue_anonymous()
            user = self.user()
            if method == "GET" and path == "/api/config":
                return self.send_json(
                    200,
                    {
                        "user": "" if self.tier == "anonymous" else user,
                        "pool": (
                            {"kind": "volunteer", "consent_version": CONSENT_VERSION}
                            if POOL_KIND == "volunteer"
                            else {"kind": "direct"}
                        ),
                        "tier": self.tier,
                        "languages": [{"code": c, "name": n} for c, n in LANGUAGES],
                        "max_chars": MAX_CHARS,
                        "max_glossary": MAX_GLOSSARY,
                        "concurrency": {
                            "max_in_flight": 1 if self.tier == "anonymous" else MAX_ACTIVE_PER_USER,
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
        except Exception as error:
            # 未知のパスやクエリーには資格や入力が含まれ得る。
            log_path = (
                path
                if path
                in (
                    "/",
                    "/index.html",
                    "/translation.mjs",
                    "/logo.png",
                    "/favicon.ico",
                    "/api/anonymous",
                    "/api/config",
                    "/api/translate",
                    "/api/worker/register",
                    "/api/worker/renew",
                    "/api/worker/mine",
                    "/api/worker/poll",
                    "/api/worker/result",
                    "/api/worker/ping",
                )
                else "<unknown>"
            )
            log.error("request %s %s failed: %s", method, log_path, type(error).__name__)
            try:
                self.send_json(500, {"error": "サーバーの内部エラーです"})
            except Exception:
                pass

    def do_GET(self):
        self.dispatch("GET")

    def do_POST(self):
        self.dispatch("POST")

    def do_DELETE(self):
        self.dispatch("DELETE")

    def worker_api(self, method, path):
        hub = self.server.get_workers()
        if method == "POST" and path in (
            "/api/worker/poll",
            "/api/worker/result",
            "/api/worker/ping",
        ):
            scheme, _, secret = self.headers.get("Authorization", "").partition(" ")
            if scheme.lower() != "bearer":
                raise HttpError(401, "ワーカーの資格が必要です")
            worker = hub.valid(secret)
            try:
                body = self.read_json_body(max_bytes=65536)
            except HttpError:
                with hub.condition:
                    for aid, assignment in list(hub.assignments.items()):
                        if assignment["worker"] == worker["id"]:
                            hub._fail(aid)
                raise
            if path == "/api/worker/poll":
                result = hub.poll(secret, body, self.client_network(), self.client_disconnected)
                if result is None:
                    self.send_response(204)
                    self.send_header("Cache-Control", "private, no-store")
                    self.end_headers()
                    return
            elif path == "/api/worker/result":
                result = hub.result(secret, body)
            else:
                result = hub.ping(secret, body)
            return self.send_json(200, result)
        # 管理APIは前段が検証したOIDCのヘッダーだけを受ける。
        owner = self.headers.get("X-Forwarded-User", "")
        if not USER_RE.fullmatch(owner):
            raise HttpError(401, "アカウントへのログインが必要です")
        if method == "POST" and path == "/api/worker/register":
            body = self.read_json_body(max_bytes=65536)
            if set(body) != {"terms_version"}:
                raise HttpError(400, "参加の規約の版を指定してください")
            return self.send_json(200, hub.registry.register(owner, body["terms_version"]))
        if method == "POST" and path == "/api/worker/renew":
            body = self.read_json_body(max_bytes=65536)
            if set(body) != {"worker_id"} or not isinstance(body["worker_id"], str):
                raise HttpError(400, "ワーカーを指定してください")
            return self.send_json(200, hub.registry.renew(owner, body["worker_id"]))
        if method == "GET" and path == "/api/worker/mine":
            listing = hub.registry.mine(owner)
            if hub.checks is not None:
                for record in listing:
                    try:
                        if hub.checks.health.eligible(hub.registry.info(record["worker_id"])):
                            record["state"] = "ready"
                    except HttpError:
                        pass
            return self.send_json(200, {"workers": listing})
        if method == "DELETE" and re.fullmatch(r"/api/worker/[0-9a-f]{32}", path):
            hub.registry.delete(owner, path.rsplit("/", 1)[1])
            return self.send_json(200, {"deleted": True})
        raise HttpError(404, "Not found")

    def serve_static(self, filename: str, content_type: str) -> None:
        body = (STATIC_DIR / filename).read_bytes()
        if filename == "index.html" and ANON_ENABLED and PUBLIC_SERVER_URL:
            body = body.replace(
                b'data-web-anonymous="disabled"', b'data-web-anonymous="enabled"', 1
            )
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
        if POOL_KIND == "volunteer" and (
            type(body.get("consent")) is not int or body["consent"] != CONSENT_VERSION
        ):
            raise HttpError(403, "ボランティア枠の利用には同意が必要です", "consent_required")
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
        volunteer = isinstance(pool, VolunteerPool)
        tier = getattr(self, "tier", "registered")
        network = self.client_network() if tier == "anonymous" else ""
        user_limit = 1 if tier == "anonymous" else MAX_ACTIVE_PER_USER
        with active_lock:
            if active.get(user, 0) >= user_limit or (
                purpose != "interactive" and page_active.get(user, 0) >= max(1, user_limit - 1)
            ):
                raise HttpError(
                    429,
                    "同時に訳せる数を超えています。前の翻訳が終わってから送ってください",
                )
            job = pool.submit(user, purpose, tier, network)
            if job is None:
                raise HttpError(503, "翻訳の待ち行列が一杯です", "busy")
            job.group = group
            try:
                counter = (
                    self.server.character_limits_anonymous
                    if tier == "anonymous"
                    else self.server.character_limits_registered
                )
                counter.charge(user, len(req["text"]), 100000 if tier == "anonymous" else 600000)
            except HttpError:
                with pool.condition:
                    pool._remove(job)
                raise
            active[user] = active.get(user, 0) + 1
            if purpose != "interactive":
                page_active[user] = page_active.get(user, 0) + 1
        started = time.monotonic()
        events: queue.Queue = queue.Queue()
        cancel = UpstreamCancellation()
        assigned = []
        translated = []
        if volunteer:
            worker = threading.Thread(
                target=pool.translate, args=(job, req, events, cancel, assigned), daemon=True
            )
        else:
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
                    if volunteer:
                        translated.append(value)
                    next_ping = time.monotonic() + HEARTBEAT_SECONDS
                elif kind == "done":
                    finish = value
                    self.sse({"done": True, "finish": value})
                    if volunteer and assigned and value == "stop":
                        pool.after_delivery(assigned[-1], req, "".join(translated), job.user)
                    break
                else:
                    self.sse(
                        {
                            "error": value,
                            **(
                                {"code": kind} if kind in ("queue_timeout", "worker_failed") else {}
                            ),
                        }
                    )
                    break
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            finish = "canceled"
        finally:
            cancel.set()
            if volunteer:
                pool.record_finish(job, finish)
            else:
                # 原文と訳は残さない (長さと言語だけ)
                log.info(
                    "user=%s %s->%s chars=%d out=%d glossary=%d history=%d finish=%s seconds=%.1f",
                    "anonymous" if tier == "anonymous" else user,
                    req["source"] or "auto",
                    req["target"],
                    len(req["text"]),
                    out_chars,
                    len(req["glossary"]),
                    len(req["history"]),
                    finish,
                    time.monotonic() - started,
                )


WORKER_TTL = 7 * 86400
WORKER_TERMS_VERSION = 1


class WorkerRegistry:
    """ワーカーの資格のハッシュと参加同意だけを保存する。"""

    def __init__(self, path, per_owner=2):
        if per_owner <= 0:
            raise ValueError("TR_WORKERS_PER_OWNER は正の整数にしてください")
        path = Path(path)
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        if os.name != "nt" and (path.parent.stat().st_mode & 0o077):
            raise ValueError("状態ファイルの親ディレクトリを0700にしてください")
        flags = os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0)
        fd = os.open(path, flags, 0o600)
        try:
            if not stat.S_ISREG(os.fstat(fd).st_mode):
                raise ValueError("状態ファイルは通常ファイルにしてください")
            if os.name != "nt" and os.fstat(fd).st_uid != os.getuid():
                raise ValueError("状態ファイルの所有者が違います")
            os.fchmod(fd, 0o600)
        finally:
            os.close(fd)
        self.db = sqlite3.connect(path, check_same_thread=False)
        self.lock = threading.RLock()
        self.per_owner = per_owner
        self.revocation_callback = None
        with self.db:
            self.db.execute(
                "CREATE TABLE IF NOT EXISTS owners (name TEXT PRIMARY KEY, terms INTEGER, agreed TEXT, revoked INTEGER NOT NULL DEFAULT 0)"
            )
            self.db.execute(
                "CREATE TABLE IF NOT EXISTS workers (id TEXT PRIMARY KEY, owner TEXT NOT NULL, digest TEXT UNIQUE NOT NULL, expires INTEGER NOT NULL, state TEXT NOT NULL)"
            )
            if "quarantined" not in {
                row[1] for row in self.db.execute("PRAGMA table_info(owners)")
            }:
                self.db.execute(
                    "ALTER TABLE owners ADD COLUMN quarantined INTEGER NOT NULL DEFAULT 0"
                )

            if "generation" not in {row[1] for row in self.db.execute("PRAGMA table_info(owners)")}:
                self.db.execute(
                    "ALTER TABLE owners ADD COLUMN generation INTEGER NOT NULL DEFAULT 0"
                )

    def register(self, owner, terms):
        if type(terms) is not int or terms != WORKER_TERMS_VERSION:
            raise HttpError(400, "参加の規約への同意が必要です")
        with self.lock, self.db:
            self.check_owner(owner)
            self.db.execute(
                "DELETE FROM workers WHERE owner=? AND expires<=? AND state!='revoked'",
                (owner, time.time()),
            )
            if (
                self.db.execute("SELECT count(*) FROM workers WHERE owner=?", (owner,)).fetchone()[
                    0
                ]
                >= self.per_owner
            ):
                raise HttpError(429, "登録できる端末の数を超えています")
            held = self.db.execute(
                "SELECT quarantined FROM owners WHERE name=?", (owner,)
            ).fetchone()
            worker_id, secret = secrets.token_hex(16), "ptw_" + secrets.token_hex(32)
            self.db.execute(
                "INSERT INTO owners(name, terms, agreed) VALUES(?,?,?) ON CONFLICT(name) DO UPDATE SET terms=excluded.terms, agreed=excluded.agreed",
                (owner, terms, time.strftime("%Y-%m-%d", time.gmtime())),
            )
            self.db.execute(
                "INSERT INTO workers VALUES(?,?,?,?,?)",
                (
                    worker_id,
                    owner,
                    hashlib.sha256(secret.encode()).hexdigest(),
                    int(time.time()) + WORKER_TTL,
                    "quarantine" if held and held[0] else "probation",
                ),
            )
            return {"worker_id": worker_id, "secret": secret, "expires_in": WORKER_TTL}

    def check_owner(self, owner):
        row = self.db.execute("SELECT revoked FROM owners WHERE name=?", (owner,)).fetchone()
        if row and row[0]:
            raise HttpError(403, "この所有者は失効しています")

    def authenticate(self, secret):
        if not re.fullmatch(r"ptw_[0-9a-f]{64}", secret):
            raise HttpError(401, "ワーカーの資格が無効です")
        with self.lock:
            row = self.db.execute(
                "SELECT w.id,w.owner,w.expires,w.state,o.quarantined,o.revoked,o.generation FROM workers w JOIN owners o ON w.owner=o.name WHERE w.digest=?",
                (hashlib.sha256(secret.encode()).hexdigest(),),
            ).fetchone()
            if not row or row[2] <= time.time() or row[3] == "revoked" or row[5]:
                raise HttpError(401, "ワーカーの資格が無効です")
            return {
                "id": row[0],
                "owner": row[1],
                "expires": row[2],
                "state": row[3],
                "owner_quarantined": bool(row[4]),
                "owner_generation": row[6],
            }

    def info(self, worker_id):
        with self.lock:
            row = self.db.execute(
                "SELECT w.id,w.owner,w.expires,w.state,o.quarantined,o.revoked,o.generation FROM workers w JOIN owners o ON w.owner=o.name WHERE w.id=?",
                (worker_id,),
            ).fetchone()
            if not row or row[2] <= time.time() or row[3] == "revoked" or row[5]:
                raise HttpError(401, "ワーカーの資格が無効です")
            return {
                "id": row[0],
                "owner": row[1],
                "expires": row[2],
                "state": row[3],
                "owner_quarantined": bool(row[4]),
                "owner_generation": row[6],
            }

    def quarantine_owner(self, owner, generation=None):
        with self.lock, self.db:
            changed = self.db.execute(
                "UPDATE owners SET quarantined=1 WHERE name=? AND revoked=0 AND (? IS NULL OR generation=?)",
                (owner, generation, generation),
            ).rowcount
            if changed:
                self.db.execute(
                    "UPDATE workers SET state='quarantine' WHERE owner=? AND state!='revoked'",
                    (owner,),
                )

    def release_owner(self, owner):
        with self.lock, self.db:
            self.check_owner(owner)
            self.db.execute(
                "UPDATE owners SET quarantined=0,generation=generation+1 WHERE name=? AND revoked=0",
                (owner,),
            )
            self.db.execute(
                "UPDATE workers SET state='probation' WHERE owner=? AND state='quarantine'",
                (owner,),
            )

    def owned(self, owner, worker_id):
        self.check_owner(owner)
        row = self.db.execute(
            "SELECT state FROM workers WHERE id=? AND owner=?", (worker_id, owner)
        ).fetchone()
        if not row:
            raise HttpError(404, "ワーカーが見つかりません")
        return row[0]

    def renew(self, owner, worker_id):
        with self.lock, self.db:
            if self.owned(owner, worker_id) == "revoked":
                raise HttpError(403, "このワーカーは失効しています")
            self.db.execute(
                "UPDATE workers SET expires=? WHERE id=?",
                (int(time.time()) + WORKER_TTL, worker_id),
            )
        return {"expires_in": WORKER_TTL}

    def mine(self, owner):
        with self.lock:
            self.check_owner(owner)
            return [
                {"worker_id": row[0], "state": row[1], "expires_at": row[2]}
                for row in self.db.execute(
                    "SELECT id,state,expires FROM workers WHERE owner=? AND expires>? ORDER BY id",
                    (owner, time.time()),
                )
            ]

    def delete(self, owner, worker_id):
        with self.lock, self.db:
            self.owned(owner, worker_id)
            self.db.execute("DELETE FROM workers WHERE id=?", (worker_id,))
            if not self.db.execute("SELECT 1 FROM workers WHERE owner=?", (owner,)).fetchone():
                self.db.execute("DELETE FROM owners WHERE name=? AND revoked=0", (owner,))
        if self.revocation_callback:
            self.revocation_callback(worker_id=worker_id)

    def set_state(self, worker_id, state, generation=None):
        if state not in ("probation", "quarantine", "revoked"):
            raise ValueError("ワーカーの状態が不正です")
        with self.lock, self.db:
            changed = (
                self.db.execute(
                    "UPDATE workers SET state=? WHERE id=? AND (state!='revoked' OR ?='revoked') AND (?!='probation' OR EXISTS (SELECT 1 FROM owners o WHERE o.name=workers.owner AND o.revoked=0 AND o.quarantined=0)) AND (? IS NULL OR EXISTS (SELECT 1 FROM owners o WHERE o.name=workers.owner AND o.generation=?))",
                    (state, worker_id, state, state, generation, generation),
                ).rowcount
                > 0
            )
        # DBのロックを解放してからHubへ通知し、ロックの順序を逆転させない。
        if state == "revoked" and self.revocation_callback:
            self.revocation_callback(worker_id=worker_id)
        return changed

    def revoke_owner(self, owner):
        with self.lock, self.db:
            self.db.execute(
                "INSERT INTO owners(name,revoked) VALUES(?,1) ON CONFLICT(name) DO UPDATE SET terms=NULL,agreed=NULL,revoked=1",
                (owner,),
            )
            self.db.execute("DELETE FROM workers WHERE owner=?", (owner,))
        if self.revocation_callback:
            self.revocation_callback(owner=owner)

    def close(self):
        with self.lock:
            self.db.close()


def worker_input(req):
    return {
        "text": req["text"],
        "target_lang": req["target"],
        "source_lang": req["source"],
        "form": req["form"],
        "context": req["context"],
        "glossary": [{"source": source, "target": target} for source, target in req["glossary"]],
        "history": [{"source": source, "target": target} for source, target in req["history"]],
    }


class WorkerHub:
    """通信プロトコルの土台。利用者のジョブとの接続は検査の実装後に行う。"""

    def __init__(self, registry, model, minimum_version="0.0.0"):
        if not re.fullmatch(r"[0-9a-f]{64}", model):
            raise ValueError("TR_WORKER_MODEL_SHA256 にモデルのSHA-256を指定してください")
        try:
            self.minimum_version = self.version(minimum_version or "0.0.0")
        except HttpError:
            raise ValueError("TR_MIN_AGENT_VERSION は数値3つの版にしてください")
        self.registry, self.model = registry, model
        self.condition = threading.Condition()
        self.polling = set()
        self.ready = {}
        self.deliveries = {}
        self.assignments = {}
        self.poll_seconds, self.first_seconds, self.deadline_seconds = 25, 20, 120
        self.closed = False
        self.checks = None
        self.revoked_workers = set()
        self.revoked_owners = set()
        self.registry.revocation_callback = self.notify_revoked

    def notify_revoked(self, worker_id=None, owner=None):
        with self.condition:
            if worker_id is not None:
                self.revoked_workers.add(worker_id)
            if owner is not None:
                self.revoked_owners.add(owner)
            if self.checks is not None:
                self.checks.health.invalidate(worker_id, owner)
            for wid, (worker, _) in list(self.ready.items()):
                if wid in self.revoked_workers or worker["owner"] in self.revoked_owners:
                    del self.ready[wid]
            for aid, assignment in list(self.assignments.items()):
                if (
                    assignment["worker"] in self.revoked_workers
                    or assignment["owner"] in self.revoked_owners
                ):
                    self._fail(aid)
            self.condition.notify_all()

    @staticmethod
    def version(value):
        if not isinstance(value, str) or not re.fullmatch(
            r"[0-9]{1,9}\.[0-9]{1,9}\.[0-9]{1,9}", value
        ):
            raise HttpError(400, "エージェントの版が不正です")
        return tuple(map(int, value.split(".")))

    def valid(self, secret):
        return self.registry.authenticate(secret)

    def poll(self, secret, body, network, disconnected):
        if set(body) != {"agent_version", "model"}:
            raise HttpError(400, "ポーリングの項目が不正です")
        if (
            self.version(body["agent_version"]) < self.minimum_version
            or body["model"] != self.model
        ):
            raise HttpError(403, "エージェントの版かモデルが対応していません")
        worker = self.valid(secret)
        wid = worker["id"]
        with self.condition:
            if wid in self.polling or any(a["worker"] == wid for a in self.assignments.values()):
                raise HttpError(409, "このワーカーは既に処理中です")
            self.polling.add(wid)
            self.ready[wid] = (worker, network)
            self.condition.notify_all()
        deadline = time.monotonic() + self.poll_seconds
        next_validation = time.monotonic() + 5
        validated_delivery = None
        try:
            while True:
                with self.condition:
                    if wid in self.revoked_workers or worker["owner"] in self.revoked_owners:
                        raise HttpError(401, "ワーカーの資格が無効です")
                    now = time.monotonic()
                    if self.closed or now >= deadline or disconnected():
                        return None
                    if now < next_validation and time.time() < worker["expires"]:
                        delivery = self.deliveries.get(wid)
                        if delivery is None:
                            self.condition.wait(min(0.1, next_validation - now, deadline - now))
                            continue
                        aid = delivery["assignment_id"]
                        assignment = self.assignments.get(aid)
                        if assignment is None:
                            return None
                        if not assignment["user_job"]:
                            return self.deliveries.pop(wid)
                        if validated_delivery == aid:
                            if self.checks.health.eligible(worker):
                                return self.deliveries.pop(wid)
                            self._fail(aid)
                            return None
                        validated_delivery = aid
                # SQLiteへはHubのロックを持たずに問い合わせる。
                # 期限前の更新も取り込むため、資格の期限に達した場合も再確認する。
                worker = self.valid(secret)
                with self.condition:
                    if wid in self.ready:
                        self.ready[wid] = (worker, network)
                next_validation = time.monotonic() + 5
        finally:
            with self.condition:
                self.polling.discard(wid)
                self.ready.pop(wid, None)
                delivery = self.deliveries.pop(wid, None)
                if delivery:
                    self._fail(delivery["assignment_id"])
                self.condition.notify_all()

    def _live(self, assignment):
        now = time.monotonic()
        return (
            not self.closed
            and now < assignment["deadline"]
            and now - assignment["last"] < self.first_seconds
        )

    def _fail(self, assignment_id, reason="worker_failed"):
        assignment = self.assignments.pop(assignment_id, None)
        if assignment:
            assignment["failure"] = reason
            assignment["events"].put(("error", "worker_failed"))
            self.condition.notify_all()

    def execute_probe(
        self,
        req,
        events,
        cancel,
        timeout=120,
        target_worker=None,
        attempts=2,
        select_worker=None,
        wait_started=None,
        on_failure=None,
    ):
        """確認用ジョブと利用者ジョブで同じ割り当て形式を使う。"""
        req = parse_request(worker_input(req))
        excluded = set()
        excluded_networks = set()
        for attempt in range(attempts):
            started = (
                wait_started if attempt == 0 and wait_started is not None else time.monotonic()
            )
            assignment_id = None
            with self.condition:
                while (
                    not cancel.is_set() and not self.closed and time.monotonic() - started < timeout
                ):
                    candidates = [
                        (wid, worker, network)
                        for wid, (worker, network) in self.ready.items()
                        if worker["owner"] not in excluded
                        and wid not in self.revoked_workers
                        and worker["owner"] not in self.revoked_owners
                        and (target_worker is None or wid == target_worker)
                        and (not network or network not in excluded_networks)
                    ]
                    selected = (
                        select_worker(candidates)
                        if select_worker
                        else (secrets.choice(candidates) if candidates else None)
                    )
                    if selected:
                        wid, worker, network = selected
                        excluded.add(worker["owner"])
                        # 同じネットワークの別所有者も再割り当て先から除く。
                        if network:
                            excluded_networks.add(network)
                        self.ready.pop(wid)
                        assignment_id = secrets.token_hex(16)
                        now = time.monotonic()
                        self.assignments[assignment_id] = {
                            "worker": wid,
                            "user_job": select_worker is not None,
                            "deadline": now + self.deadline_seconds,
                            "last": now,
                            "seq": 0,
                            "chars": 0,
                            "tail": "",
                            "limit": max_tokens(req["text"]) * 6,
                            "events": queue.Queue(),
                            "owner": worker["owner"],
                            "network": network,
                        }
                        assigned = self.assignments[assignment_id]
                        self.deliveries[wid] = {
                            "assignment_id": assignment_id,
                            "deadline_seconds": self.deadline_seconds,
                            "input": worker_input(req),
                        }
                        self.condition.notify_all()
                        break
                    self.condition.wait(0.1)
            if not assignment_id:
                if not cancel.is_set():
                    kind = "queue_timeout" if select_worker and attempt == 0 else "worker_failed"
                    events.put((kind, "翻訳ワーカーに接続できません"))
                return "cancelled" if cancel.is_set() else "unavailable"
            sent = False
            try:
                while not cancel.is_set():
                    with self.condition:
                        assignment = self.assignments.get(assignment_id)
                        if not assignment:
                            break
                        if not self._live(assignment):
                            self._fail(assignment_id, "timeout")
                            break
                        result_queue = assignment["events"]
                    try:
                        kind, value = result_queue.get(timeout=0.1)
                    except queue.Empty:
                        continue
                    if cancel.is_set():
                        return "cancelled"
                    if kind == "error":
                        break
                    if kind == "text":
                        sent = True
                    events.put((kind, value))
                    if kind == "done":
                        return "success"
            finally:
                with self.condition:
                    self.assignments.pop(assignment_id, None)
                    self.deliveries.pop(wid, None)
                    self.condition.notify_all()
            if cancel.is_set():
                return "cancelled"
            if on_failure is not None:
                on_failure(worker, assigned.get("failure", "worker_failed"))
            if sent or attempt == attempts - 1:
                events.put(("worker_failed", "翻訳ワーカーとの接続が途中で切れました"))
                return assigned.get("failure", "worker_failed")

    def result(self, secret, body):
        worker = self.valid(secret)
        aid = body.get("assignment_id")
        if not isinstance(aid, str):
            raise HttpError(400, "割り当てが不正です")
        with self.condition:
            assignment = self.assignments.get(aid)
            if not assignment or assignment["worker"] != worker["id"]:
                return {"stop": True}
            if assignment.get("finished"):
                return {"stop": True}
            if not self._live(assignment):
                self._fail(aid, "timeout")
                return {"stop": True}
            seq = body.get("seq")
            if type(seq) is not int or seq < 0:
                self._fail(aid)
                raise HttpError(400, "連番が不正です")
            if seq < assignment["seq"]:
                return {"stop": False}
            if seq != assignment["seq"] or set(body) - {
                "assignment_id",
                "seq",
                "text",
                "done",
                "finish",
                "error",
            }:
                self._fail(aid)
                raise HttpError(400, "結果の項目か連番が不正です")
            text = body.get("text", "")
            done = body.get("done", False)
            if (
                not isinstance(text, str)
                or type(done) is not bool
                or RESERVED in assignment["tail"] + text
                or any(0xD800 <= ord(c) <= 0xDFFF for c in text)
                or assignment["chars"] + len(text) > assignment["limit"]
                or (done and body.get("finish") not in ("stop", "length"))
                or ("error" in body and (body["error"] != "engine" or done or text))
            ):
                self._fail(aid)
                raise HttpError(400, "結果の形式が不正です")
            assignment["seq"] += 1
            assignment["last"] = time.monotonic()
            assignment["chars"] += len(text)
            text = "".join(
                c
                for c in text
                if (c in "\n\t" or ord(c) >= 32)
                and not 127 <= ord(c) <= 159
                and not 0x202A <= ord(c) <= 0x202E
                and not 0x2066 <= ord(c) <= 0x2069
            )
            if RESERVED in assignment["tail"] + text:
                self._fail(aid)
                raise HttpError(400, "結果の形式が不正です")
            assignment["tail"] = (assignment["tail"] + text)[-len(RESERVED) :]
            if text:
                assignment["events"].put(("text", text))
            if done:
                assignment["finished"] = True
                assignment["events"].put(("done", body["finish"]))
            elif "error" in body:
                self._fail(aid)
            return {"stop": False}

    def ping(self, secret, body):
        worker = self.valid(secret)
        if (
            set(body) != {"active"}
            or not isinstance(body["active"], list)
            or len(body["active"]) > 1
            or any(not isinstance(a, str) or len(a) != 32 for a in body["active"])
        ):
            raise HttpError(400, "実行中の割り当てが不正です")
        with self.condition:
            stop = []
            for aid in body["active"]:
                assignment = self.assignments.get(aid)
                if (
                    not assignment
                    or assignment["worker"] != worker["id"]
                    or not self._live(assignment)
                ):
                    stop.append(aid)
                    if assignment and assignment["worker"] == worker["id"]:
                        self._fail(aid, "timeout")
            return {"stop": stop}

    def close(self):
        if self.checks is not None:
            self.checks.close()
        with self.condition:
            self.closed = True
            for aid in list(self.assignments):
                self._fail(aid)
            self.condition.notify_all()
        self.registry.close()


def normalized_translation(text):
    return " ".join(unicodedata.normalize("NFKC", text).casefold().split())


def translation_links(text):
    urls = re.findall(
        r"(?:[A-Za-z][A-Za-z0-9+.-]{1,31}://|www\.)[^\s<>\"'\u3000\u3001\u3002]+",
        text,
        re.IGNORECASE,
    )
    emails = re.findall(r"[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+", text)
    return {value.rstrip(".,;:!?)]}") for value in urls + emails}


def translation_numbers(text):
    text = unicodedata.normalize("NFKC", text)
    text = re.sub(r"(?<=\d),(?=\d{3}(?:\D|$))", "", text)
    return set(re.findall(r"\d+(?:\.\d+)?", text))


def matches_target_script(text, target):
    letters = [c for c in text if unicodedata.category(c).startswith("L")]
    if len(letters) < 20:
        return True  # 短い固有名詞や記号だけでは判定しない。
    groups = {
        "LATIN": "en az ca ceb de es et eu fr hr hu id it jv lt lv ms nl pl sk sl sw tl uz vi",
        "CYRILLIC": "be bg kk ru",
        "ARABIC": "ar fa ur",
        "BENGALI": "bn",
        "GUJARATI": "gu",
        "HEBREW": "he",
        "DEVANAGARI": "hi mr ne",
        "TAMIL": "ta",
        "TELUGU": "te",
        "MALAYALAM": "ml",
        "SINHALA": "si",
        "THAI": "th",
        "KHMER": "km",
        "MYANMAR": "my",
        "GEORGIAN": "ka",
        "ARMENIAN": "hy",
        "GURMUKHI": "pa",
        "HANGUL": "ko",
    }
    scripts = next(([script] for script, codes in groups.items() if target in codes.split()), [])
    if target == "sr":
        scripts = ["LATIN", "CYRILLIC"]
    elif target == "ja":
        scripts = ["HIRAGANA", "KATAKANA", "CJK"]
    elif target in ("zh-hans", "zh-hant"):
        scripts = ["CJK"]
    if not scripts:
        return False
    matching = sum(any(script in unicodedata.name(c, "") for script in scripts) for c in letters)
    return matching / len(letters) >= 0.2


def check_translation(req, translated, reference):
    """内容を返さず、判定理由の固定コードだけを返す。"""
    output, expected = normalized_translation(translated), normalized_translation(reference)
    reasons = []
    a = {output[n : n + 2] for n in range(max(0, len(output) - 1))}
    b = {expected[n : n + 2] for n in range(max(0, len(expected) - 1))}
    similarity = (
        1.0 if output == expected else (2 * len(a & b) / (len(a) + len(b)) if a or b else 0)
    )
    if not output or similarity < 0.6:
        reasons.append("similarity")
    allowed = (
        req["text"]
        + "\n"
        + "\n".join(source + "\n" + target for source, target in req["glossary"])
        + "\n"
        + reference
    )
    if not translation_links(translated) <= translation_links(allowed):
        reasons.append("links")
    if not translation_numbers(translated) <= translation_numbers(reference):
        reasons.append("numbers")
    if matches_target_script(reference, req["target"]) and not matches_target_script(
        translated, req["target"]
    ):
        reasons.append("language")
    return tuple(reasons)


class WorkerHealth:
    """成績は数と時刻だけ。内容や基準の訳を保持しない。"""

    def __init__(self, registry, probation_jobs=20):
        if probation_jobs <= 0:
            raise ValueError("TR_PROBATION_JOBS は正の整数にしてください")
        self.registry, self.probation_jobs = registry, probation_jobs
        self.lock = threading.RLock()
        self.workers = {}
        self.owners = {}
        self.qualified = set()

    def invalidate(self, worker_id=None, owner=None):
        with self.lock:
            if worker_id is not None:
                self.qualified.discard(worker_id)
            if owner is not None:
                self.qualified.difference_update(
                    wid for wid, stats in self.workers.items() if stats["owner"] == owner
                )

    def eligible(self, worker):
        with self.lock:
            return (
                worker["id"] in self.qualified
                and self.workers.get(worker["id"], {}).get("generation", 0)
                == worker.get("owner_generation", 0)
                and worker["state"] == "probation"
                and not worker.get("owner_quarantined", False)
            )

    def record(self, worker, passed=None, timed_out=False, canary=True):
        wid, owner = worker["id"], worker["owner"]
        with self.lock:
            stats = self.workers.setdefault(
                wid,
                {
                    "owner": owner,
                    "passed": 0,
                    "failed": 0,
                    "errors": 0,
                    "timeouts": 0,
                    "streak": 0,
                    "recent": deque(maxlen=20),
                },
            )
            generation = worker.get("owner_generation", 0)
            if generation < max(
                stats.get("generation", 0), self.owners.get(owner, {}).get("generation", 0)
            ):
                return
            if stats.get("generation", 0) != generation:
                stats["streak"] = 0
                stats["recent"].clear()
                self.qualified.discard(wid)
            stats["generation"] = generation
            if owner in self.owners and self.owners[owner].get("generation", 0) != generation:
                self.owners.pop(owner)
            owner_stats = self.owners.setdefault(
                owner,
                {"passed": 0, "failed": 0, "errors": 0, "timeouts": 0, "failures": deque(maxlen=3)},
            )
            owner_stats["generation"] = generation
            for bucket in (stats, owner_stats):
                bucket[
                    "passed" if passed is True else "failed" if passed is False else "errors"
                ] += 1
                bucket["timeouts"] += int(timed_out)
            stats["recent"].append(passed is None)
            if passed is not True:
                stats["streak"] = 0
            elif canary:
                stats["streak"] += 1
            if passed is False:
                now = time.monotonic()
                failures = owner_stats["failures"]
                while failures and failures[0] <= now - 86400:
                    failures.popleft()
                failures.append(now)
                self.qualified.discard(wid)
                self.registry.set_state(wid, "quarantine", generation)
                if len(failures) >= 3:
                    self.registry.quarantine_owner(owner, generation)
                    self.qualified.difference_update(
                        worker_id
                        for worker_id, record in self.workers.items()
                        if record["owner"] == owner
                    )
            elif (
                passed is None
                and len(stats["recent"]) >= 10
                and sum(stats["recent"]) / len(stats["recent"]) >= 0.5
            ):
                self.qualified.discard(wid)
                self.registry.set_state(wid, "quarantine", generation)
            elif (
                passed is True
                and canary
                and stats["streak"] >= self.probation_jobs
                and not worker.get("owner_quarantined", False)
            ):
                if self.registry.set_state(wid, "probation", generation):
                    self.qualified.add(wid)
                else:
                    self.qualified.discard(wid)

    def release_owner(self, owner):
        self.registry.release_owner(owner)
        with self.lock:
            self.owners.pop(owner, None)
            for wid, stats in self.workers.items():
                if stats["owner"] == owner:
                    stats["streak"] = 0
                    stats["recent"].clear()
                    self.qualified.discard(wid)


class CanaryChecks:
    """検証用エンジンの枠まで並列に照合し、内容はメモリだけに置く。"""

    @staticmethod
    def file_signature(metadata):
        return (
            metadata.st_dev,
            metadata.st_ino,
            metadata.st_mtime_ns,
            metadata.st_ctime_ns,
            metadata.st_size,
        )

    @staticmethod
    def load_samples(canary_file):
        fd = os.open(
            canary_file, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
        )
        try:
            metadata = os.fstat(fd)
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > 16 * 1024 * 1024:
                raise ValueError("確認用ジョブは16MiB以下の通常ファイルにしてください")
            if os.name != "nt" and (metadata.st_uid != os.getuid() or metadata.st_mode & 0o077):
                raise ValueError("確認用ジョブのファイルを所有者本人の0600にしてください")
            with os.fdopen(fd, "rb") as source:
                fd = None
                try:
                    raw = source.read(16 * 1024 * 1024 + 1)
                    if len(raw) > 16 * 1024 * 1024:
                        raise ValueError("確認用ジョブが大きすぎます")
                    data = json.loads(raw.decode("utf-8"))
                    final = os.fstat(source.fileno())
                    if CanaryChecks.file_signature(metadata) != CanaryChecks.file_signature(final):
                        raise ValueError("確認用ジョブの読み取り中に更新されました")
                except (ValueError, RecursionError):
                    raise ValueError("確認用ジョブのJSONが不正です") from None
        finally:
            if fd is not None:
                os.close(fd)
        if not isinstance(data, list) or not data or len(data) > 10000:
            raise ValueError("確認用ジョブは1〜10000件のJSON配列にしてください")
        samples = []
        seen = set()
        allowed = {"text", "target_lang", "source_lang", "form", "context", "glossary", "history"}
        for body in data:
            if not isinstance(body, dict) or set(body) - allowed:
                raise ValueError("確認用ジョブの項目が不正です")
            try:
                req = parse_request(body)
            except HttpError:
                raise ValueError("確認用ジョブの入力が不正です") from None
            digest = hashlib.sha256(req["text"].encode()).hexdigest()
            if digest not in seen:
                samples.append((digest, req))
                seen.add(digest)
        return samples, CanaryChecks.file_signature(metadata)

    def __init__(self, hub, upstreams, canary_file, probation_jobs=20):
        self.samples, self.signature = self.load_samples(canary_file)
        self.sample_ids = {digest for digest, _ in self.samples}
        self.canary_file = Path(canary_file)
        self.failed_signature = None
        self.lock = threading.RLock()
        self.hub = hub
        self.pool = DirectPool(upstreams)
        self.health = WorkerHealth(hub.registry, probation_jobs)
        self.used = {}
        self.reserved = {}
        self.busy_workers = set()
        self.cancel = UpstreamCancellation()
        self.thread = None
        self.executor = ThreadPoolExecutor(
            max_workers=self.pool.total_slots, thread_name_prefix="canary"
        )
        self.canary_rate = float(CANARY_RATE)
        self.audit_rate = float(AUDIT_RATE)
        backlog = int(AUDIT_BACKLOG)
        if not (0 < self.canary_rate <= 1 and 0 < self.audit_rate <= 1 and backlog > 0):
            raise ValueError("確認用ジョブと抜き取りの率・待ち行列の設定が不正です")
        self.audit_queue = queue.Queue(maxsize=backlog)
        self.suspicious_audits = WindowCounter(3600)
        self.audit_thread = None
        self.due_canaries = {}
        self.reference_timeout = 120
        self.reload_interval = 1
        self.stats_interval = 60

    def start(self):
        thread = threading.Thread(target=self.run, daemon=True)
        thread.start()
        self.thread = thread
        audit_thread = threading.Thread(target=self.run_audits, daemon=True)
        audit_thread.start()
        self.audit_thread = audit_thread

    @staticmethod
    def sampled(rate):
        return secrets.randbits(53) / (1 << 53) < rate

    def after_delivery(self, worker, req, translated, user=None):
        """配信完了後に呼ぶ。検査の完了や待ち行列の空きを待たない。"""
        if self.cancel.is_set() or not self.health.eligible(worker):
            return False
        wid = worker["id"]
        generation = worker["owner_generation"]
        suspicious = False
        if user is not None:
            allowed = "\n".join(
                [req["text"], *(value for pair in req["glossary"] for value in pair)]
            )
            suspicious = (
                len(translated) > max_tokens(req["text"]) * 6
                or not translation_links(translated) <= translation_links(allowed)
                or not matches_target_script(translated, req["target"])
            )
        with self.lock:
            if self.cancel.is_set():
                return False
            if self.sampled(self.canary_rate):
                self.due_canaries[wid] = generation
            if not suspicious and not self.sampled(self.audit_rate):
                return False
            if self.audit_queue.full():
                return False
            if suspicious:
                try:
                    self.suspicious_audits.charge(user, 1, 5)
                except HttpError:
                    return False
            self.audit_queue.put_nowait((wid, generation, req, translated))
            return True

    def run_audits(self):
        while not self.cancel.is_set():
            try:
                item = self.audit_queue.get(timeout=0.1)
            except queue.Empty:
                continue
            try:
                self.audit(*item)
            except (HttpError, OSError, ValueError, sqlite3.Error):
                pass
            except Exception as error:
                log.error("worker audit failed: %s", type(error).__name__)
            finally:
                self.audit_queue.task_done()
                item = None

    def audit(self, wid, generation, req, translated):
        if self.hub.registry.info(wid)["owner_generation"] != generation:
            return
        reference = self.reference(req)
        if reference is None or self.cancel.is_set():
            return
        current = self.hub.registry.info(wid)
        if current["owner_generation"] == generation:
            passed = not check_translation(req, translated, reference)
            self.health.record(current, passed=passed, canary=False)

    def needs_canary(self, worker):
        if not self.health.eligible(worker):
            return True
        return self.due_canaries.get(worker["id"]) == worker["owner_generation"]

    def reload_if_changed(self):
        signature = None
        try:
            signature = self.file_signature(self.canary_file.stat())
            with self.lock:
                if signature == self.signature or signature == self.failed_signature:
                    return False
            samples, loaded_signature = self.load_samples(self.canary_file)
            with self.lock:
                self.samples = samples
                self.sample_ids = {digest for digest, _ in samples}
                self.signature = loaded_signature
                self.failed_signature = None
            return True
        except (OSError, ValueError) as error:
            # 壊れた更新は採用せず、同じ失敗を毎秒ログに出さない。
            failure = signature if signature is not None else ("missing",)
            with self.lock:
                report = failure != self.failed_signature
                self.failed_signature = failure
            if report:
                log.warning("canary reload failed: %s", type(error).__name__)
            return False

    def report_exhaustion(self):
        with self.lock:
            count = sum(self.sample_ids <= digests for digests in self.used.values())
        log.info("canary exhausted_owners=%d", count)
        return count

    def reference(self, req):
        events = queue.Queue()
        cancel = UpstreamCancellation()
        job = self.pool.submit("canary", "interactive")
        index = self.pool.acquire(job, self.cancel)
        if index is None:
            return None

        def translate():
            try:
                upstream_stream(
                    build_prompt(req),
                    max_tokens(req["text"]),
                    events,
                    cancel,
                    self.pool.urls[index],
                )
            finally:
                self.pool.release(job, index)

        thread = threading.Thread(target=translate, daemon=True)
        try:
            thread.start()
        except Exception:
            self.pool.release(job, index)
            raise
        result, size = [], 0
        deadline = time.monotonic() + self.reference_timeout
        try:
            while not self.cancel.is_set() and time.monotonic() < deadline:
                try:
                    kind, value = events.get(timeout=0.1)
                except queue.Empty:
                    continue
                if kind == "text":
                    size += len(value)
                    if size > max_tokens(req["text"]) * 6:
                        return None
                    result.append(value)
                elif kind == "done":
                    return "".join(result).strip() if value == "stop" and result else None
                else:
                    return None
            return None
        finally:
            cancel.set()
            thread.join(timeout=5.2)

    def run(self):
        next_reload = next_stats = 0
        while not self.cancel.is_set():
            now = time.monotonic()
            if now >= next_reload:
                self.reload_if_changed()
                next_reload = now + self.reload_interval
            if now >= next_stats:
                self.report_exhaustion()
                next_stats = now + self.stats_interval
            with self.hub.condition:
                ready = [worker for worker, _ in self.hub.ready.values()]
            selected = None
            with self.lock:
                if len(self.busy_workers) < self.pool.total_slots:
                    candidates = [
                        worker
                        for worker in ready
                        if worker["id"] not in self.busy_workers
                        and self.needs_canary(worker)
                        and not worker.get("owner_quarantined", False)
                        and self.sample_ids
                        - self.used.get(worker["owner"], set())
                        - self.reserved.get(worker["owner"], set())
                    ]
                    if candidates:
                        worker = secrets.choice(candidates)
                        available = (
                            self.sample_ids
                            - self.used.get(worker["owner"], set())
                            - self.reserved.get(worker["owner"], set())
                        )
                        digest = secrets.choice(tuple(available))
                        req = next(req for key, req in self.samples if key == digest)
                        self.busy_workers.add(worker["id"])
                        self.reserved.setdefault(worker["owner"], set()).add(digest)
                        selected = (worker, digest, req)
            if selected is not None:
                try:
                    self.executor.submit(self.check, *selected)
                except RuntimeError:
                    self.unreserve(selected[0], selected[1])
                    if not self.cancel.is_set():
                        raise
            else:
                with self.hub.condition:
                    self.hub.condition.wait(0.2)

    def unreserve(self, worker, digest):
        with self.lock:
            self.busy_workers.discard(worker["id"])
            reserved = self.reserved.get(worker["owner"], set())
            reserved.discard(digest)
            if not reserved:
                self.reserved.pop(worker["owner"], None)
        with self.hub.condition:
            self.hub.condition.notify_all()

    def check(self, worker, digest, req):
        try:
            worker = self.hub.registry.info(worker["id"])
            with self.hub.condition:
                if worker["id"] in self.hub.ready:
                    _, network = self.hub.ready[worker["id"]]
                    self.hub.ready[worker["id"]] = (worker, network)
            with self.lock:
                if not self.needs_canary(worker) or worker.get("owner_quarantined", False):
                    return
            reference = self.reference(req)
            if reference is None or self.cancel.is_set():
                self.cancel.wait(1)
                return
            with self.lock:
                self.used.setdefault(worker["owner"], set()).add(digest)
            events = queue.Queue()
            outcome = self.hub.execute_probe(
                req, events, self.cancel, timeout=25, target_worker=worker["id"], attempts=1
            )
            if outcome not in ("cancelled", "unavailable"):
                with self.lock:
                    if self.due_canaries.get(worker["id"]) == worker["owner_generation"]:
                        self.due_canaries.pop(worker["id"], None)
            text, finished = [], False
            while not events.empty():
                kind, value = events.get_nowait()
                if kind == "text":
                    text.append(value)
                elif kind == "done":
                    finished = value == "stop"
            if outcome not in ("cancelled", "unavailable") and not self.cancel.is_set():
                current = self.hub.registry.info(worker["id"])
                if current["owner_generation"] != worker["owner_generation"]:
                    return
                verdict = not check_translation(req, "".join(text), reference) if finished else None
                self.health.record(current, passed=verdict, timed_out=outcome == "timeout")
        except (HttpError, OSError, ValueError, sqlite3.Error):
            self.cancel.wait(1)
        except Exception as error:
            log.error("canary check failed: %s", type(error).__name__)
            self.cancel.wait(1)
        finally:
            reference = req = text = None
            self.unreserve(worker, digest)

    def close(self):
        self.cancel.set()
        with self.hub.condition:
            self.hub.condition.notify_all()
        if self.thread is not None:
            self.thread.join(timeout=6)
        self.executor.shutdown(wait=True, cancel_futures=True)
        if self.audit_thread is not None:
            self.audit_thread.join()
        with self.lock:
            while not self.audit_queue.empty():
                self.audit_queue.get_nowait()
                self.audit_queue.task_done()
            self.due_canaries.clear()
            self.busy_workers.clear()
            self.reserved.clear()


class VolunteerPool(DirectPool):
    """待ち行列を共用し、参加端末への割り当てだけを仲介する。"""

    def __init__(self, hub, timeout, aging, max_queue):
        super().__init__([], timeout, aging, max_queue)
        self.hub = hub
        self.condition = hub.condition
        self.window = int(SPREAD_WINDOW)
        self.owner_share = float(OWNER_SHARE_MAX)
        if not (1 <= self.window <= 100 and 0 < self.owner_share <= 1):
            raise ValueError("分散の設定が不正です")
        self.assignment_minutes = {}
        self.user_history = {}
        self.running = set()
        self.held = {}
        self.stats = {}
        self.degraded = False
        self.closed = threading.Event()
        self.reporter = threading.Thread(target=self.report_loop, daemon=True)
        self.reporter.start()

    def available(self, worker):
        checks = self.hub.checks
        with checks.lock:
            return (
                worker["expires"] > time.time()
                and worker["id"] not in checks.busy_workers
                and worker["id"] not in self.held
                and not checks.needs_canary(worker)
            )

    def expire_assignment_minutes(self, minute):
        for bucket in list(self.assignment_minutes):
            if bucket <= minute - 10:
                del self.assignment_minutes[bucket]

    def spread(self, job, candidates, online, now, previous_workers=()):
        minute = int(now // 60)
        self.expire_assignment_minutes(minute)
        assignments = Counter()
        for counts in self.assignment_minutes.values():
            assignments.update(counts)
        parents = {}

        def root(key):
            while key in parents:
                parent = parents[key]
                if parent in parents:
                    parents[key] = parents[parent]
                key = parents.get(key, parent)
            return key

        def connect(owner, network):
            if network:
                left = root(("owner", owner))
                right = root(("network", network))
                if left != right:
                    parents[right] = left

        for owner, network in assignments:
            connect(owner, network)
        for owner, network in online:
            connect(owner, network)

        def owner_group(owner):
            return root(("owner", owner))

        owners = {owner_group(owner) for owner, _ in online}
        count = len(owners)
        limit = max(self.owner_share, min(1, 1.5 / max(1, count)))
        maximum = math.ceil((assignments.total() + 1) * limit)
        counts = Counter()
        for (owner, _), amount in assignments.items():
            counts[owner_group(owner)] += amount
        excluded = {owner_group(worker["owner"]) for worker in previous_workers}
        candidates = [
            item
            for item in candidates
            if owner_group(item[1]["owner"]) not in excluded
            and counts[owner_group(item[1]["owner"])] < maximum
        ]
        if not candidates:
            return None

        recent = self.user_history.setdefault(job.user, deque(maxlen=self.window))
        while recent and recent[0][0] <= now - 600:
            recent.popleft()
        avoid = {owner_group(recent[-1][2])} if recent else set()
        if job.group is not None:
            avoid.update(owner_group(owner) for _, group, owner in recent if group == job.group)
        preferred = [item for item in candidates if owner_group(item[1]["owner"]) not in avoid]
        selected = secrets.choice(preferred or candidates)
        _, worker, network = selected
        owner = worker["owner"]
        self.assignment_minutes.setdefault(minute, Counter())[owner, network] += 1
        recent.append((now, job.group, owner))
        self.degraded |= count < 5
        return selected

    def choose(self, job, candidates, assigned):
        # WorkerHub が condition を保持し、この選択と端末の確保を一緒に行う。
        candidates = [item for item in candidates if self.available(item[1])]
        if not candidates:
            return None
        now = time.monotonic()
        ready = [
            (worker["owner"], network)
            for worker, network in self.hub.ready.values()
            if self.available(worker)
        ]
        self.total_slots = len(ready) + len(self.running)
        first = job not in self.running
        if first and self._next(now) is not job:
            return None
        online = ready + [
            (assignment["owner"], assignment["network"])
            for assignment in self.hub.assignments.values()
            if assignment["user_job"]
        ]
        selected = self.spread(job, candidates, online, now, assigned)
        if selected is None:
            return None
        wid, worker, network = selected
        if first:
            self._begin(job, now)
            self.running.add(job)
            stats = self.stats.setdefault((job.tier, job.purpose), Counter())
            stats["assigned"] += 1
            stats["wait_ms"] += int((now - job.created) * 1000)
        self.held[wid] = job
        assigned.append(worker)
        return selected

    def translate(self, job, req, events, cancel, assigned):
        try:
            self.hub.execute_probe(
                req,
                events,
                cancel,
                timeout=self.timeout,
                wait_started=job.created,
                select_worker=lambda candidates: self.choose(job, candidates, assigned),
                on_failure=self.note_worker_failure,
            )
        except Exception as error:
            log.error("volunteer worker failed: %s", type(error).__name__)
            events.put(("worker_failed", "翻訳ワーカーに接続できません"))
        finally:
            with self.condition:
                if job in self.running:
                    self.running.remove(job)
                    self._end(job)
                elif job in self.waiting:
                    self._remove(job)
            release_active(job.user, job.purpose)

    def note_worker_failure(self, worker, reason):
        try:
            current = self.hub.registry.info(worker["id"])
            if current["owner_generation"] == worker["owner_generation"]:
                self.hub.checks.health.record(current, timed_out=reason == "timeout", canary=False)
        except (HttpError, sqlite3.Error):
            pass

    def after_delivery(self, worker, req, translated, user):
        try:
            self.hub.checks.after_delivery(worker, req, translated, user=user)
        except Exception as error:
            log.error("volunteer delivery check failed: %s", type(error).__name__)

    def record_finish(self, job, finish):
        with self.condition:
            self.held = {wid: entry for wid, entry in self.held.items() if entry is not job}
            stats = self.stats.setdefault((job.tier, job.purpose), Counter())
            stats["count"] += 1
            stats["errors"] += int(finish not in ("stop", "length", "canceled"))
            stats["canceled"] += int(finish == "canceled")
            self.condition.notify_all()

    def report_loop(self):
        while not self.closed.wait(60):
            self.report()

    def report(self):
        with self.condition:
            now = time.monotonic()
            self.expire_assignment_minutes(int(now // 60))
            for user, recent in list(self.user_history.items()):
                while recent and recent[0][0] <= now - 600:
                    recent.popleft()
                if not recent:
                    del self.user_history[user]
            stats, self.stats = self.stats, {}
            degraded, self.degraded = self.degraded, False
        for (tier, purpose), counts in sorted(stats.items()):
            log.info(
                "volunteer tier=%s purpose=%s count=%d assigned=%d wait_ms_sum=%d errors=%d canceled=%d spread=%s",
                tier,
                purpose,
                counts["count"],
                counts["assigned"],
                counts["wait_ms"],
                counts["errors"],
                counts["canceled"],
                "degraded" if degraded else "normal",
            )

    def close(self):
        self.closed.set()
        self.reporter.join()
        self.report()
        with self.condition:
            self.assignment_minutes.clear()
            self.user_history.clear()


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
    if POOL_KIND == "direct" and not UPSTREAM_URL and not UPSTREAMS:
        log.error("TR_UPSTREAM_URL is not set")
        sys.exit(2)
    if not API_KEY:
        log.warning("TR_API_KEY is empty; requests are sent without Authorization")
    server = TranslationHTTPServer((LISTEN_HOST, LISTEN_PORT), Handler)
    try:
        pool = server.get_pool()
        if STATE_FILE:
            server.get_workers()
    except (ValueError, TypeError, OSError, sqlite3.Error) as error:
        log.error("プールの設定が不正です: %s", error)
        server.server_close()
        sys.exit(2)
    server.daemon_threads = True
    signal.signal(
        signal.SIGTERM,
        lambda *_: threading.Thread(target=server.shutdown, daemon=True).start(),
    )
    if POOL_KIND == "volunteer":
        log.info("volunteer listening port=%d", server.server_port)
    else:
        log.info(
            "listening on %s:%d engines=%d slots=%d startup_seconds=%.3f",
            LISTEN_HOST,
            server.server_port,
            len(pool.urls),
            sum(pool.free),
            time.monotonic() - started,
        )
    try:
        server.serve_forever()
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
