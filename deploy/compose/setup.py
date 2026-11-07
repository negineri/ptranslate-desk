#!/usr/bin/env python3
"""Pocket ID に PTranslate 用の設定を作る (docs/SELF_HOSTING_COMPOSE.md の 3)。

compose の setup サービスとして、同じネットワークの中から Pocket ID の API を呼ぶ。

    docker compose run --rm setup

1. 最初の管理者がまだいなければ作る (.env の PT_ADMIN と PT_ADMIN_EMAIL)
2. 利用を許すグループ (既定は translate-users) を作り、管理者を入れる
3. OIDC クライアントを 2 つ作るか、正しい設定に直す
   - ptranslate-web: Web 用 (oauth2-proxy)。client secret あり
   - ptranslate-desktop: PTranslate Desk 用。公開クライアント (PKCE)、ループバックのリダイレクト
4. Web 用の client secret を作り、.env の WEB_CLIENT_SECRET に書く (既に書いてあれば作らない)
5. 管理者を作ったときは、パスキーを登録するためのログイン用 URL (1 時間有効) を出す

管理者を作った後にもう一度流すときは、Pocket ID の管理画面で作った API キーを
POCKET_ID_API_KEY に渡す。Python の標準ライブラリだけで動く。
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

WEB_CLIENT_ID = "ptranslate-web"
DESKTOP_CLIENT_ID = "ptranslate-desktop"


class Api:
    def __init__(self, base: str, api_key: str = ""):
        self.base = base.rstrip("/")
        self.headers = {"Content-Type": "application/json", "Accept": "application/json"}
        if api_key:
            self.headers["X-API-Key"] = api_key

    def call(self, method: str, path: str, body=None, ok=(200, 201, 204)):
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(
            self.base + path, data=data, method=method, headers=self.headers
        )
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                raw = response.read()
                status, headers = response.status, response.headers
        except urllib.error.HTTPError as error:
            raw, status, headers = error.read(), error.code, error.headers
        if status not in ok:
            detail = raw.decode(errors="replace")[:300]
            raise SystemExit(f"{method} {path} が {status} を返しました: {detail}")
        return status, headers, (json.loads(raw) if raw.strip() else None)

    def use_session(self, headers) -> None:
        """最初の管理者を作った応答の Cookie (access token) を、以降は Bearer で送る。"""
        for value in headers.get_all("Set-Cookie") or []:
            name, _, rest = value.partition("=")
            if name.strip().endswith("access_token"):
                self.headers["Authorization"] = "Bearer " + rest.split(";", 1)[0]
                return
        raise SystemExit("管理者のセッションを受け取れませんでした")


def read_env(path: Path) -> dict[str, str]:
    values = {}
    for line in path.read_text().splitlines():
        if "=" in line and not line.lstrip().startswith("#"):
            key, _, value = line.partition("=")
            values[key.strip()] = value.strip()
    return values


def write_env_value(path: Path, key: str, value: str) -> None:
    text = path.read_text()
    line = f"{key}={value}"
    if re.search(rf"^{key}=.*$", text, flags=re.M):
        text = re.sub(rf"^{key}=.*$", lambda _: line, text, count=1, flags=re.M)
    else:
        text = text.rstrip("\n") + "\n" + line + "\n"
    path.write_text(text)


def ensure_group(api: Api, name: str) -> str:
    _, _, page = api.call("GET", "/api/user-groups?search=" + urllib.parse.quote(name))
    for group in page.get("data", []):
        if group["name"] == name:
            return group["id"]
    _, _, group = api.call("POST", "/api/user-groups", {"name": name, "friendlyName": name})
    print(f"グループ {name} を作りました")
    return group["id"]


def ensure_client(api: Api, client_id: str, settings: dict, group_id: str) -> None:
    status, _, _ = api.call("GET", f"/api/oidc/clients/{client_id}", ok=(200, 404))
    if status == 404:
        api.call("POST", "/api/oidc/clients", {"id": client_id, **settings})
        print(f"OIDC クライアント {client_id} を作りました")
    else:
        api.call("PUT", f"/api/oidc/clients/{client_id}", settings)
        print(f"OIDC クライアント {client_id} の設定を確かめました")
    api.call(
        "PUT", f"/api/oidc/clients/{client_id}/allowed-user-groups", {"userGroupIds": [group_id]}
    )


def main() -> None:
    parser = argparse.ArgumentParser(description="Pocket ID に PTranslate 用の設定を作る")
    parser.add_argument(
        "--pocket-id", default=os.environ.get("POCKET_ID_URL", "http://pocket-id:1411")
    )
    parser.add_argument("--env-file", default="/work/.env")
    parser.add_argument("--admin", help="最初の管理者のユーザー名 (既定は .env の PT_ADMIN)")
    parser.add_argument(
        "--email", help="最初の管理者のメールアドレス (既定は .env の PT_ADMIN_EMAIL)"
    )
    parser.add_argument("--group", default=os.environ.get("PT_GROUP", "translate-users"))
    args = parser.parse_args()

    env_path = Path(args.env_file)
    env = read_env(env_path)
    domain = env.get("PT_DOMAIN", "")
    if not domain or domain.endswith("example-tailnet.ts.net"):
        raise SystemExit(".env の PT_DOMAIN を書いてください")
    app_url = f"https://{domain}:8443"
    admin_name = args.admin or env.get("PT_ADMIN") or "admin"
    admin_email = args.email or env.get("PT_ADMIN_EMAIL", "")

    api = Api(args.pocket_id, os.environ.get("POCKET_ID_API_KEY", ""))
    status, _, _ = api.call("GET", "/api/signup/setup", ok=(204, 400, 403, 404, 409))
    admin = None
    if status == 204:
        if not admin_email:
            raise SystemExit(
                "最初の管理者のメールアドレスを .env の PT_ADMIN_EMAIL に書いてください"
            )
        _, headers, admin = api.call(
            "POST",
            "/api/signup/setup",
            {"username": admin_name, "email": admin_email, "firstName": admin_name, "lastName": ""},
        )
        api.use_session(headers)
        print(f"最初の管理者 {admin_name} を作りました")
    elif "X-API-Key" not in api.headers:
        raise SystemExit(
            "管理者は作成済みです。Pocket ID の管理画面で API キーを作り、"
            "POCKET_ID_API_KEY に渡してもう一度流してください "
            "(POCKET_ID_API_KEY=<キー> docker compose run --rm -e POCKET_ID_API_KEY setup)"
        )

    group_id = ensure_group(api, args.group)
    common = {
        "pkceEnabled": True,
        "isGroupRestricted": True,
        "accessTokenDurationMinutes": 10,
    }
    ensure_client(
        api,
        WEB_CLIENT_ID,
        {
            **common,
            "name": "PTranslate (Web)",
            "callbackURLs": [f"https://{domain}/oauth2/callback"],
            "isPublic": False,
            "launchURL": f"https://{domain}/",
        },
        group_id,
    )
    ensure_client(
        api,
        DESKTOP_CLIENT_ID,
        {
            **common,
            "name": "PTranslate Desk",
            # ポートを書かないループバックは、任意のポートに一致する (RFC 8252)
            "callbackURLs": ["http://127.0.0.1/callback"],
            "isPublic": True,
            "refreshTokenDurationMinutes": 30 * 24 * 60,
        },
        group_id,
    )

    if not env.get("WEB_CLIENT_SECRET"):
        _, _, created = api.call("POST", f"/api/oidc/clients/{WEB_CLIENT_ID}/secrets", {})
        write_env_value(env_path, "WEB_CLIENT_SECRET", created["secret"])
        print(".env の WEB_CLIENT_SECRET に Web 用の client secret を書きました")

    if admin:
        api.call("PUT", f"/api/users/{admin['id']}/user-groups", {"userGroupIds": [group_id]})
        _, _, token = api.call(
            "POST", f"/api/users/{admin['id']}/one-time-access-token", {"ttl": "1h"}
        )
        print()
        print(
            "次の URL を tailnet に入っている端末のブラウザーで開き、パスキーを登録してください (1 時間有効):"
        )
        print(f"  {app_url}/lc/{token['token']}")
    print()
    print("終わったら docker compose up -d で全体を起動します。")


if __name__ == "__main__":
    sys.exit(main())
