# 自分のサーバーで使う

この手順では、翻訳エンジン、PTranslateのAPI/Web、Authentik、oauth2-proxy、nginxを用意し、
Web版とmacOS版の両方から利用します。例のドメイン・利用者・秘密は自分のものに置き換えてください。
IdPもリバースプロキシも無く、tailnetの中だけで使うなら、Docker Compose 1つで立てる
[かんたん構築ガイド](SELF_HOSTING_COMPOSE.md)のほうが手早く済みます。

## 構成と用意するもの

例はUbuntu 24.04以降の1台のサーバーです。Docker EngineとCompose v2、Python 3.13以降、
Git、CMake、C++コンパイラ、curl、nginx、Certbotを用意します。
Dockerの導入は[公式手順](https://docs.docker.com/engine/install/ubuntu/)、Pythonは利用OSのパッケージなどを使ってください。
llama.cppのCPUビルドには `build-essential cmake libcurl4-openssl-dev` が必要です。
Pythonサーバー自体には追加のpipパッケージはありません。

| 用意するもの | この手順の例 | 公開範囲 |
| --- | --- | --- |
| 翻訳用ドメイン | `translate.example.com` | nginxの80/443番だけ |
| 認証用ドメイン | `auth.example.com` | nginxの80/443番だけ |
| llama-server | `127.0.0.1:8080` | サーバー内だけ |
| 翻訳API/Web | `127.0.0.1:8002` | サーバー内だけ |
| oauth2-proxy | `127.0.0.1:4180` | サーバー内だけ |
| Authentik | `127.0.0.1:9000` | nginx経由で公開 |

両ドメインのDNSをサーバーに向け、80/443番への接続を許可します。証明書は両ドメイン用に発行します。
モデル本体と推論用メモリのほかに、AuthentikとDB用の容量が必要です。
量子化・同時実行数で必要量が変わるため、モデルの配布情報でRAM/VRAMとディスク容量を確認してください。
GPUは任意です。最初はCPUで確認でき、GPUを使う場合はllama.cppの対応ドライバ・ビルド設定を追加します。

```text
ブラウザー / PTranslate Desk
    → HTTPS nginx → oauth2-proxyで認証確認
                  → PTranslate API → llama-server
ブラウザーのログイン → HTTPS nginx → Authentik → PostgreSQL
```

## 1. ソースとローカルのモックを確認する

```sh
mkdir -p ~/src
cd ~/src
git clone https://github.com/negineri/ptranslate-desk.git
cd ptranslate-desk
python3 --version
python3 tools/mock_plamo.py --port 18082
```

別の端末で、同じリポジトリから起動します。

```sh
TR_UPSTREAM_URL=http://127.0.0.1:18082/v1/completions \
  python3 -I server/translator.py
```

サーバー内の端末から設定とSSEの応答を確認します。

```sh
curl --fail -H 'X-Forwarded-User: local-test' http://127.0.0.1:8002/api/config
curl --fail --no-buffer -H 'X-Forwarded-User: local-test' \
  -H 'Content-Type: application/json' \
  --data '{"text":"Hello","target_lang":"ja"}' \
  http://127.0.0.1:8002/api/translate
```

`user` が `local-test` になり、翻訳にはモックの印と原文が返ります。
ここで付けたヘッダーはローカル開発専用です。外部に公開するときは、検証した利用者名を認証プロキシから渡します。
この確認後、両方をCtrl+Cで止めます。デスクトップ全体をモックで試す場合は
[開発手順](../desktop/README.md#モックで起動する)を使います。

## 2. 翻訳エンジンを起動する

[PLaMoのGGUFモデル](https://huggingface.co/pfnet/plamo-3-translate-2b-GGUF)のライセンスを確認し、
[llama.cppのビルド手順](https://github.com/ggml-org/llama.cpp/blob/master/docs/build.md)に沿って導入します。
CPU版の例です。運用時は動作確認したllama.cppのコミットを固定してください。

```sh
cd ~/src
git clone https://github.com/ggml-org/llama.cpp.git
cd llama.cpp
cmake -B build -DGGML_CUDA=OFF
cmake --build build --config Release --target llama-server -j 2
./build/bin/llama-server \
  -hf pfnet/plamo-3-translate-2b-GGUF:Q4_K_M \
  --host 127.0.0.1 --port 8080 --ctx-size 16384 --parallel 1
```

初回はモデルのダウンロードが発生します。`GET http://127.0.0.1:8080/health` が200になるまで待ちます。
このサーバーは `/v1/completions` を使います。PFML3のプロンプトはPTranslate側が作るため、`--jinja` は不要です。
コンテキストは原文と訳を合わせて16K、例では同時推論を1本にしています。
別マシンのエンジンを使う場合は、APIキーとTLSまたは閉じたネットワークで接続を制限します。
起動引数は[llama-serverの公式説明](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md)を参照してください。

## 3. 翻訳API/Webを起動する

リポジトリのルートで、モックの代わりに実エンジンを指定します。

```sh
cd ~/src/ptranslate-desk
TR_LISTEN_HOST=127.0.0.1 TR_LISTEN_PORT=8002 \
  TR_UPSTREAM_URL=http://127.0.0.1:8080/v1/completions \
  python3 -I server/translator.py
```

別の端末から1章のcurlを再実行すると実際の訳が返ります。これは構築時の手動確認であり、自動試験には実エンジンを使いません。
エンジンがAPIキーを要求する場合だけ `TR_API_KEY` を設定します。空の警告はキー不要のローカルエンジンなら正常です。
その他の環境変数は[以下の一覧](#サーバーの環境変数)を参照してください。

再起動後も動かすには、ソースを `/srv/ptranslate-desk`、llama.cppを `/srv/llama.cpp` などに配置し、
専用ユーザーとsystemdサービスを作ります。モデルのキャッシュを保存できるホームディレクトリを用意します。

```sh
sudo useradd --system --create-home --home-dir /var/lib/ptranslate ptranslate
sudo install -d /srv/ptranslate-desk /srv/llama.cpp
sudo cp -a ~/src/ptranslate-desk/. /srv/ptranslate-desk/
sudo cp -a ~/src/llama.cpp/. /srv/llama.cpp/
```

`/etc/systemd/system/ptranslate-engine.service` の例です。

```ini
[Unit]
Description=PTranslate translation engine
After=network-online.target
Wants=network-online.target

[Service]
User=ptranslate
WorkingDirectory=/srv/llama.cpp
ExecStart=/srv/llama.cpp/build/bin/llama-server -hf pfnet/plamo-3-translate-2b-GGUF:Q4_K_M --host 127.0.0.1 --port 8080 --ctx-size 16384 --parallel 1
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

`/etc/systemd/system/ptranslate.service` の例です。Pythonの実際のパスに合わせてください。

```ini
[Unit]
Description=PTranslate API and Web
After=network.target ptranslate-engine.service

[Service]
User=ptranslate
WorkingDirectory=/srv/ptranslate-desk
Environment=TR_LISTEN_HOST=127.0.0.1
Environment=TR_LISTEN_PORT=8002
Environment=TR_UPSTREAM_URL=http://127.0.0.1:8080/v1/completions
ExecStart=/usr/bin/python3 -I /srv/ptranslate-desk/server/translator.py
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

ソースとビルド成果物を専用ユーザーが読めるようにし、手動起動分を止めてから有効化します。

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now ptranslate-engine ptranslate
sudo systemctl status ptranslate-engine ptranslate
```

コンテナを使う場合は、公開イメージのバージョンとdigestを固定します。
Linuxの同一ホストのエンジンにつなぐ例は次のとおりです。ホストネットワークを使うので待ち受けは127.0.0.1にします。

```sh
docker run -d --name ptranslate --restart unless-stopped --network host \
  -e TR_LISTEN_HOST=127.0.0.1 \
  -e TR_UPSTREAM_URL=http://127.0.0.1:8080/v1/completions \
  ghcr.io/negineri/ptranslate-desk/server:0.2.0
```

上のタグは例です。`docker image inspect` で取得したRepoDigestを使い、実運用では `image@sha256:...` に固定してください。
Pythonサービスとコンテナはどちらか一方を使います。

### サーバーの環境変数

| 環境変数                            | 既定                                          | 意味                                             |
| ----------------------------------- | --------------------------------------------- | ------------------------------------------------ |
| `TR_UPSTREAM_URL`                   | (`TR_UPSTREAMS` が空なら必須)                                        | 翻訳エンジン (llama-server) の `/v1/completions` |
| `TR_API_KEY`                        | 空                                            | 翻訳エンジンに付ける `Authorization: Bearer`     |
| `TR_MODEL` | `plamo-3-translate` | 翻訳エンジンに渡すモデル名 |
| `TR_LISTEN_HOST` / `TR_LISTEN_PORT` | `127.0.0.1` / `8002` (コンテナでは `0.0.0.0`) | 待ち受け                                         |
| `TR_MAX_CHARS`                      | 4000                                          | 1 回の原文の上限 (文字)                          |
| `TR_MAX_PROMPT_CHARS`               | 12000                                         | 文脈・前の訳・用語集を合わせた上限 (文字)        |
| `TR_MAX_GLOSSARY` | 200 | 用語集の組数の上限 |
| `TR_MAX_ACTIVE_PER_USER`            | 2                                             | 1 人が同時に流せる翻訳の数                       |
| `TR_UPSTREAMS` | 空 | 複数エンジンのJSON配列。例: `[{"url":"http://127.0.0.1:8080/v1/completions","slots":2}]`。各 `slots` は1〜1000 |
| `TR_UPSTREAM_SLOTS` | 4 | `TR_UPSTREAMS` が空のときの実行枠。1〜1000の整数。エンジンの並列数より多い分はエンジン側で待つ。`TR_UPSTREAMS` 指定時は使用・検証しない |
| `TR_STATE_FILE` | 空 | ワーカー登録用SQLite。空ならワーカーAPIは無効。POSIXでは親を0700にする。利用者ジョブの接続は未実装 |
| `TR_WORKER_MODEL_SHA256` | 空 | ワーカーAPIを有効にする場合に必須。承認するモデルの小文字16進数64桁 |
| `TR_AUDIT_UPSTREAMS` | 空 | 確認用ジョブの基準を作る運営のエンジン。`url` と `slots` のJSON配列。`TR_CANARY_FILE` と両方指定して検査を有効にする |
| `TR_CANARY_FILE` | 空 | 非公開の合成ジョブのJSON配列。16MiB・10,000件まで。所有者だけが読める0600にし、本番の文をリポジトリに置かない |
| `TR_PROBATION_JOBS` | 20 | 試用と端末の検疫から合格するまでに必要な連続合格数。正の整数 |
| `TR_WORKERS_PER_OWNER` | 2 | 所有者あたりの登録上限 |
| `TR_MIN_AGENT_VERSION` | 空 | 受けるエージェントの最小版（数値3つ）。空なら `0.0.0` |
| `TR_ANON_ENABLED` | 0 | 匿名の資格のAPIを有効にするか。前段からだけ仲介に接続できる構成で使う |
| `TR_TOKEN_KEY` | 空 | 匿名を有効にする場合に必須。32バイト以上の秘密の鍵。リポジトリ・ログには入れない |
| `TR_ANON_ISSUE_PER_MINUTE` | 100 | 全体の資格発行の試みの1分の上限 |
| `TR_ANON_SHARE` | 0.5 | 登録済みの待ちがある場合の匿名の実行枠の割合（0より大きく1以下、最低1枠） |
| `TR_CLIENT_IP_HEADER` | 空 | 信頼する前段が上書きするIPヘッダー。空ならIP単位の制限を使わない |
| `TR_COUNTRY_HEADER` / `TR_ALLOWED_COUNTRIES` | 空 / 空 | 信頼する前段の国ヘッダーと許可する国のカンマ区切り一覧（例 `JP`）。一覧が空なら制限しない |
| `TR_MAX_OPEN_REQUESTS` | 1000 | 開いた接続の上限。超過は503 |
| `TR_UPSTREAM_TIMEOUT` | 600 | 翻訳エンジンとの通信タイムアウト（秒） |
| `TR_HEARTBEAT_SECONDS` | 15 | SSEのハートビート間隔（秒） |
| `TR_USERINFO_URL` | 空 | IdPのuserinfo。`X-Forwarded-User` が無く前段が検証したBearerだけがあるとき、ここから `preferred_username` を引く（access tokenにユーザー名が入らないIdP向け。[かんたん構築ガイド](SELF_HOSTING_COMPOSE.md)） |
| `TR_USERINFO_CACHE_SECONDS` | 300 | userinfoの結果を覚えておく秒数の上限（tokenの期限が先ならそこまで） |
| `TR_PUBLIC_SERVER_URL` | 空 | クライアント向けに表示する公開サーバーURL。空ならWebの閲覧中のオリジンを使う |
| `TR_DESKTOP_DISCOVERY_URL` | 空 | クライアント向けに表示する外部から到達できるHTTPSのOIDC discovery URL |
| `TR_DESKTOP_CLIENT_ID` | `translator-desktop` | デスクトップの公開クライアントのID |
| `TR_STATIC_DIR` | `server/static/`（コンテナでは `/app/static`） | Webの静的ファイルの配置先 |


## 4. AuthentikとHTTPSを用意する

[AuthentikのCompose手順](https://docs.goauthentik.io/install-config/install/docker-compose/)で導入します。
リポジトリの外に専用ディレクトリを作り、公式Composeファイルをダウンロードします。
`.env` は権限600で作り、`PG_PASS` と `AUTHENTIK_SECRET_KEY` にそれぞれ生成したランダム値を設定します。
生成値を端末に表示せず保存する例です。

```sh
mkdir -p ~/ptranslate-auth
cd ~/ptranslate-auth
curl --fail --location --output compose.yml https://docs.goauthentik.io/compose.yml
umask 077
python3 - <<'PY'
from pathlib import Path
import secrets
path = Path('.env')
with path.open('x') as file:
    file.write('PG_PASS=' + secrets.token_urlsafe(36) + '\n')
    file.write('AUTHENTIK_SECRET_KEY=' + secrets.token_urlsafe(60) + '\n')
    file.write('COMPOSE_PORT_HTTP=127.0.0.1:9000\n')
    file.write('COMPOSE_PORT_HTTPS=127.0.0.1:9443\n')
PY
docker compose pull
docker compose up -d
```

公式Composeが公開するポートを確認し、9000/9443番を外部に直接公開しないようにします。
この構成ではOutpostを使わないため、workerへのDockerソケットのマウントは削除できます。
DB・アップロード・`.env` はバックアップ対象です。公式Composeに固定されたバージョンを記録します。

nginxとCertbotを導入し、まず `/etc/nginx/sites-available/ptranslate` に次を置き、
`sites-enabled` からリンクしてnginxを再読み込みします。

```nginx
server {
    listen 80;
    server_name auth.example.com;
    location / {
        proxy_pass http://127.0.0.1:9000;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Forwarded-For $remote_addr;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
    }
}
server {
    listen 80;
    server_name translate.example.com;
    location / { return 503; }
}
```

```sh
sudo ln -s /etc/nginx/sites-available/ptranslate /etc/nginx/sites-enabled/ptranslate
sudo nginx -t
sudo systemctl reload nginx
sudo certbot --nginx -d auth.example.com -d translate.example.com
```

Certbotはメールアドレスと規約への同意を尋ねます。HTTPSへの転送を有効にします。
`https://auth.example.com/` を開き、初期管理者のパスワードを設定します。
初期設定が表示されない場合は[公式の初期設定トラブルシュート](https://docs.goauthentik.io/troubleshooting/login/)を参照してください。
初期管理者を日常利用には使わず、翻訳用の一般利用者を作ります。

## 5. Authentikに2つのクライアントを登録する

管理画面の「Applications → Applications → Create with Provider」などからOAuth2/OIDCの組を2つ作ります。
表示名はAuthentikの版により異なります。[公式のプロバイダー設定](https://docs.goauthentik.io/add-secure-apps/providers/oauth2/)も参照してください。

| 設定 | デスクトップ用 | Web用（oauth2-proxy） |
| --- | --- | --- |
| Application slug | `translator-desktop` | `translator-web` |
| Client type | Public | Confidential |
| Client ID | `translator-desktop` | `translator-web` |
| Client secret | なし | 生成してサーバー内だけに保管 |
| Redirect URI | Regex: `^http://127\.0\.0\.1:[0-9]+/callback$` | Strict: `https://translate.example.com/oauth2/callback` |
| Flow | Authorization Code + PKCE S256 | Authorization Code + PKCE S256 |
| Scope mappings | `openid`、`profile`、`offline_access` | `openid`、`profile` |
| Issuer mode | Global | Global |
| Signing key | 同じRSA署名鍵（RS256） | 同じRSA署名鍵（RS256） |
| Access token validity | 10分 | 10分 |
| Refresh token validity | 30日 | この例では使わない |

認証フローと、初回の同意を求める認可フローを選びます。デスクトップにclient secretは入れません。
`offline_access` のscope mappingを追加しないとrefresh tokenが発行されません。
両クライアントとも `email` は要求しません。ログインにメールアドレスを使わず、ユーザー名を使います。
この構成では、ID tokenとaccess tokenに `preferred_username` が必要です。
標準のprofile mappingで不足する場合は、Scope name `profile` の独自mappingを作り、
`return {"preferred_username": request.user.username}` を含めます。
ID tokenにscopeのユーザーclaimを含める設定も有効にします。

利用を許可するグループ（例: `translate-users`）を作り、一般利用者を追加します。
**両アプリケーション**のPolicy/Group/User Bindingsでそのグループだけを許可してください。
Global issuerを共有するため、署名鍵は同じものにそろえます。

それぞれのdiscoveryを取得し、`issuer` が同一であることを確認します。
Global issuerでもdiscoveryのURLはアプリごとのパスです。issuer URLにdiscoveryがあるとは限りません。

```sh
curl --fail https://auth.example.com/application/o/translator-web/.well-known/openid-configuration \
  --output web-discovery.json
curl --fail https://auth.example.com/application/o/translator-desktop/.well-known/openid-configuration \
  --output desktop-discovery.json
python3 - <<'PY'
import json
from pathlib import Path
web = json.loads(Path('web-discovery.json').read_text())
desktop = json.loads(Path('desktop-discovery.json').read_text())
assert web['issuer'] == desktop['issuer'], '両方をGlobal issuerにしてください'
for key in ('authorization_endpoint', 'token_endpoint', 'revocation_endpoint'):
    assert key in desktop, key
print('discoveryの確認: OK')
PY
```

`jwks_uri` のURLはアプリごとに異なって構いません。両方が同じ署名鍵を公開していることを確認します。

## 6. oauth2-proxyを用意する

以下はoauth2-proxy v7.14.0を使う例です。[設定の公式説明](https://oauth2-proxy.github.io/oauth2-proxy/configuration/overview/)と
[nginx連携](https://oauth2-proxy.github.io/oauth2-proxy/configuration/integration/#configuring-for-use-with-the-nginx-auth_request-directive)を参照してください。
Global issuerのdiscovery場所がアプリごとにあるため、取得済みdiscoveryから接続先を設定ファイルへ書きます。
前章のディレクトリで実行します。これらは公開情報であり、トークンは扱いません。

```sh
python3 - <<'PY'
from pathlib import Path
import json
config = json.loads(Path('web-discovery.json').read_text())
values = {
    'provider': 'oidc',
    'http_address': '127.0.0.1:4180',
    'upstreams': ['static://202'],
    'client_id': 'translator-web',
    'redirect_url': 'https://translate.example.com/oauth2/callback',
    'scope': 'openid profile',
    'code_challenge_method': 'S256',
    'skip_oidc_discovery': True,
    'oidc_issuer_url': config['issuer'],
    'login_url': config['authorization_endpoint'],
    'redeem_url': config['token_endpoint'],
    'profile_url': config['userinfo_endpoint'],
    'oidc_jwks_url': config['jwks_uri'],
    'oidc_extra_audiences': ['translator-desktop'],
    'oidc_email_claim': 'preferred_username',
    'email_domains': ['*'],
    'skip_jwt_bearer_tokens': True,
    'bearer_token_login_fallback': False,
    'set_xauthrequest': True,
    'reverse_proxy': True,
    'cookie_secure': True,
    'cookie_samesite': 'lax',
    'session_cookie_minimal': True,
    'insecure_oidc_skip_nonce': False,
    'skip_provider_button': True,
    'request_logging': False,
    'auth_logging': False,
}
# この範囲のJSON値（文字列・配列・bool）はTOMLでも有効。
Path('oauth2-proxy.cfg').write_text(''.join(f'{k} = {json.dumps(v)}\n' for k, v in values.items()))
PY
umask 077
python3 - <<'PY'
from pathlib import Path
import base64, secrets, getpass
client_secret = getpass.getpass('AuthentikのWeb用client secret: ')
if not client_secret or '\n' in client_secret:
    raise SystemExit('client secretが不正です')
with Path('oauth2-proxy.env').open('x') as file:
    file.write('OAUTH2_PROXY_CLIENT_SECRET=' + client_secret + '\n')
    file.write('OAUTH2_PROXY_COOKIE_SECRET=' + base64.b64encode(secrets.token_bytes(32)).decode() + '\n')
PY
docker run -d --name ptranslate-auth-proxy --restart unless-stopped --network host \
  --env-file "$PWD/oauth2-proxy.env" \
  --mount "type=bind,src=$PWD/oauth2-proxy.cfg,dst=/etc/oauth2-proxy.cfg,readonly" \
  quay.io/oauth2-proxy/oauth2-proxy:v7.14.0 --config=/etc/oauth2-proxy.cfg
```

`oidc_extra_audiences` はデスクトップのclient IDです。client IDを変えるならここも変えます。
`skip_jwt_bearer_tokens` はJWTの署名・issuer・audience・期限を検証したうえでCookieログインを省略する設定です。
`email_domains = ["*"]` はメールドメインによる制限を使わない指定で、匿名アクセスを許可するものではありません。
許可する利用者は前章のAuthentikの両アプリのグループで制限します。
秘密を保存した `.env`、DB、証明書の秘密鍵はリポジトリに追加しないでください。

## 7. nginxでWebとAPIを公開する

4章でCertbotが作った証明書パスを使い、同じnginx設定ファイルを次の形にします。
認証用ドメインのHTTPS設定はCertbotが作ったものを残し、認証用upstreamには4章のHost・転送ヘッダー設定を保ちます。
下記は**翻訳用ドメインのserverブロック**です。既存の同ドメインのブロックと置き換えます。
HTTPの80番はCertbotのHTTPS転送設定を残します。

```nginx
server {
    listen 443 ssl;
    server_name translate.example.com;
    ssl_certificate /etc/letsencrypt/live/translate.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/translate.example.com/privkey.pem;
    client_max_body_size 1m;

    location /oauth2/ {
        proxy_pass http://127.0.0.1:4180;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Auth-Request-Redirect https://translate.example.com/;
        # 認可コードを含むcallbackのクエリーをアクセスログに残さない。
        access_log off;
    }

    location = /_auth {
        internal;
        proxy_pass http://127.0.0.1:4180/oauth2/auth;
        proxy_pass_request_body off;
        proxy_set_header Content-Length "";
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header Authorization $http_authorization;
        proxy_set_header X-Original-URI $request_uri;
    }

    location /api/ {
        auth_request /_auth;
        error_page 401 = @api_login_required;
        auth_request_set $api_user $upstream_http_x_auth_request_preferred_username;
        proxy_set_header X-Forwarded-User $api_user;
        # 元のBearerは検証済み。翻訳サーバーにはトークンを渡さない。
        proxy_set_header Authorization "";
        proxy_pass http://127.0.0.1:8002;
        proxy_buffering off;
        proxy_cache off;
        proxy_read_timeout 600s;
    }

    location @api_login_required {
        default_type application/json;
        return 401 '{"error":"ログインしていません"}';
    }

    location / {
        auth_request /_auth;
        error_page 401 = /oauth2/start?rd=https://translate.example.com/;
        auth_request_set $web_user $upstream_http_x_auth_request_preferred_username;
        proxy_set_header X-Forwarded-User $web_user;
        proxy_set_header Authorization "";
        proxy_pass http://127.0.0.1:8002;
    }
}
```

証明書が両ドメインをまとめた名前で発行された場合、実際の保存先は
`sudo certbot certificates` で確認して合わせます。
`auth_request` モジュールのあるnginxを使い、`sudo nginx -t` が通ってから再読み込みします。
利用者から送られた `X-Forwarded-User` は必ず検証結果で上書きします。
8002/8080/4180番は外部から到達できないことも確認してください。

### ワーカーAPIを有効にする場合の前段

OIDCを通さず仲介へ渡すワーカーAPIは、`/api/worker/poll`、`/api/worker/result`、
`/api/worker/ping` の3つだけです。パスの完全一致で設定し、`/api/worker/` の前方一致で認証を外さないでください。
nginxでは `location = /api/worker/poll`、`location = /api/worker/result`、
`location = /api/worker/ping` の3つを用意し、それぞれに `proxy_set_header X-Forwarded-User "";` を指定します。
クライアントが送った利用者名を仲介へ通さず、仲介はワーカーのBearer資格を検証します。
これらの場所でも、IP・国のヘッダーを前段が検証済みの値へ上書きする設定と、SSE・キャッシュの要件を維持してください。
nginxの `proxy_set_header` は同じ階層で指定すると上位からの継承が変わるため、必要なヘッダーを各場所で明示します。

`/api/worker/register`、`/api/worker/renew`、`/api/worker/mine`、
`DELETE /api/worker/<id>` はOIDCを通します。認証済みの要求だけに、前段が検証した `X-Forwarded-User` を付けます。
匿名用APIの設定と、ワーカー管理APIの認証除外を混同しないでください。

## 8. デスクトップの接続先を設定する

[READMEのmacOSインストール手順](../README.md#インストールmacos)でアプリを入れます。
接続先設定はv0.2.0以降で利用できます。ソースから実行する場合は
[ビルド手順](../desktop/README.md#mac-のアプリをビルドする)を参照してください。

ログイン前から表示される「接続先の設定」に次を入力します。

| 項目 | この手順の値 |
| --- | --- |
| サーバーURL | `https://translate.example.com` |
| OIDC discovery URL | `https://auth.example.com/application/o/translator-desktop/.well-known/openid-configuration` |
| client ID | `translator-desktop` |

サーバーURLはHTTPSのオリジン（ポート指定可）で、パス・クエリー・ユーザー情報は付けません。
APIはこのオリジンの `/api/config` と `/api/translate` に置きます。
認証のURLにもHTTPSが必要です。ローカルHTTPでの動作確認は1章のモック用手順を使います。
「保存して再起動」を押して設定を反映します。

再起動後にログインし、初回の同意とキーチェーンの「常に許可」を選びます。
自分のAuthentikで作った利用者でログインします。
接続先の公開情報はOS標準のアプリ設定フォルダの `connection.json` に保存します。
macOSでは `~/Library/Application Support/com.witenp.ptranslate-desk/connection.json` です。
refresh tokenはOSの資格情報の保管だけに置き、サーバーURL・discovery URL・client IDの組ごとに分離します。
通常の設定や保存済み用語集は、従来どおり同じフォルダの `settings.json` です。
設定ファイルを手で変更する場合は完全終了してから編集します。不正な設定ファイルでは起動を止めます。

## 9. 完了を確認する

1. ブラウザーで `https://translate.example.com/` を開き、Authentik経由でログインして文章を翻訳します。
2. 同じブラウザーで `https://translate.example.com/api/config` を開き、`user` に利用者名が出ることを確認します。
3. デスクトップでログインします。アプリはBearerで `GET /api/config` を呼び、利用者名を確認します。
   メインウィンドウに同じ利用者名が出て、翻訳できれば接続は完了です。
4. アクセシビリティを許可し、他のアプリで選んだ文を⌘Cの2回押しか⌘⇧Tで翻訳します。
5. アプリを完全終了して再起動し、保存した接続先とログインが復元されることを確認します。
6. ログアウトし、もう一度ログインできることを確認します。

| 症状 | 確認すること |
| --- | --- |
| 起動時に翻訳サーバーが終了する | `TR_UPSTREAM_URL`、Pythonの版、ポートの競合、systemdのパス |
| モックの印が付いた訳になる | `TR_UPSTREAM_URL` が18082番のままになっていないか |
| 502/504・訳が届かない | llama-serverのhealth、モデルのロード、APIからの到達性、nginxのbuffering/timeout |
| discoveryを取得できない | DNS、証明書、discoveryのアプリslug、URLがHTTPSか |
| ブラウザーで同意後に拒否される | グループの所属、両アプリへの許可、redirect URIのRegex/Strictの指定 |
| ログイン直後に401 | access tokenに `preferred_username` があるか、nginxが検証結果のユーザー名を渡すか |
| 403（HTMLの場合もある） | issuer、audience、署名鍵、期限、scopeの `email` 混入、プロキシの許可設定 |
| 再起動でログアウトする | `offline_access` のmapping、refresh tokenの有効期限、キーチェーンの許可 |
| 許可済みでもショートカットが動かない | アプリの署名検証とアクセシビリティの許可。[配布手順](RELEASING.md#apple-の署名公証)を参照 |

トークン、認可コード、Cookie、client secret、実際の利用者名を問い合わせやログに貼らないでください。
JWTを調べる必要がある場合も、手元だけで確認し、共有するのはclaimの名前の一覧までにします。

## 保守

nginxの証明書自動更新、systemdとDockerの自動起動、DBと秘密のバックアップ・復元を確認してください。
PTranslate、llama.cpp、モデル、Authentik、oauth2-proxyの使用バージョンを記録し、更新前にモックと検証環境で確認します。
PTranslate Deskの自動更新先はこのプロジェクトのGitHub Releasesです。接続先を変えても配布元は変わりません。
自分のアプリ配布も行う場合は、[更新署名とReleaseの手順](RELEASING.md)に従って、公開鍵と更新URLを自分のものに変更します。
