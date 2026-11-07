# かんたん構築（Docker Compose + Tailscale + Pocket ID）

IdPもリバースプロキシも無い環境で、Linuxのサーバー1台にPTranslateを立てる手順です。
`deploy/compose/` のファイルだけで、翻訳エンジン、API/Web、ログイン（Pocket ID）、HTTPSがそろいます。
ドメインを公開する構成や、既にあるIdP・リバースプロキシにつなぐ構成は[構築ガイド](SELF_HOSTING.md)を参照してください。

```text
ブラウザー / PTranslate Desk（tailnetの端末）
    ── HTTPS ──▶ tailscale（証明書は *.ts.net）
                   https://<PT_DOMAIN>/       ─▶ oauth2-proxy ─▶ ptranslate ─▶ llama-server
                   https://<PT_DOMAIN>:8443/  ─▶ pocket-id（ログイン、パスキー）
```

- 外に開けるポートはありません。使えるのは、自分のtailnetに入っている端末だけです
- ログインはPocket IDのパスキーです。パスワードはありません
- 利用者はPocket IDのグループ `translate-users` で絞ります。グループに入っていない人はログインできません
- 原文と訳はどこにも保存せず、ログにも出しません

## 用意するもの

| もの | 条件 |
| --- | --- |
| サーバー | Linux（x86_64）。CPU 4コア・メモリ4 GB以上を目安に（[必要な性能](#必要な性能)） |
| Docker | Docker EngineとCompose v2.23.1以降（`configs` の `content` を使うため） |
| ディスク | 約4 GB（翻訳モデル1.7 GB、コンテナイメージ約1.3 GB、Pocket IDのデータ） |
| Tailscale | アカウントと、管理画面のDNSのページでMagicDNSとHTTPS Certificatesを有効にしたtailnet |
| 利用する端末 | Tailscaleを入れてtailnetに参加させる（Mac、Windows、iPhone、Androidなど） |

翻訳モデルの[PLaMo Community License](https://huggingface.co/pfnet/plamo-3-translate-2b-GGUF)を確認してください。
個人・非商用は登録なしで使えます。初回の起動時にHugging Faceから自動でダウンロードします。

## 1. ファイルを置く

```sh
git clone https://github.com/negineri/ptranslate-desk.git
cd ptranslate-desk/deploy/compose
cp .env.example .env
chmod 600 .env
```

`.env` を次のように埋めます。

| 変数 | 値 |
| --- | --- |
| `TS_HOSTNAME` | tailnetでのマシン名（既定は `ptranslate`） |
| `PT_DOMAIN` | `<TS_HOSTNAME>.<tailnetの名前>.ts.net`。tailnetの名前は管理画面のDNSのページにあります |
| `TS_AUTHKEY` | 管理画面のSettings → Keysで作ったauth key（下の注意） |
| `POCKET_ID_ENCRYPTION_KEY`、`COOKIE_SECRET` | それぞれ `openssl rand -base64 32` の結果（別々の値） |
| `PT_ADMIN`、`PT_ADMIN_EMAIL` | Pocket IDの最初の管理者のユーザー名とメールアドレス |

- auth keyは1回だけ使えるもの（Reusableを無効）で十分です。tailnetに入った後は `tailscale-state` のボリュームに
  状態が残るので、次からは要りません。Ephemeralにすると、サーバーが長く止まったときにマシンが消えます
- auth keyを空にすると、ログに出るログインURLで承認する方法になります。ただしコンテナは60秒ごとに
  ログインをやり直し、そのたびにURLが変わるので、auth keyを勧めます
- ユーザー名は英小文字・数字・`_` `.` `-` にしてください（PTranslateの利用者名の規則）
- 同じ名前のマシンが既にtailnetにあると、`ptranslate-1` のように番号が付きます。
  `docker compose exec tailscale tailscale status` で実際の名前を確かめ、`PT_DOMAIN` を合わせます

## 2. Tailscaleとログインを起動する

```sh
docker compose up -d tailscale pocket-id
docker compose exec tailscale tailscale status
```

`ptranslate` がtailnetに出ていれば成功です。tailnetの端末のブラウザーで
`https://<PT_DOMAIN>:8443/` が開けることを確かめます（初回は証明書の発行で数十秒かかります）。

## 3. Pocket IDを設定する

```sh
docker compose run --rm setup
```

`deploy/compose/setup.py` が次を行います。

1. 最初の管理者（`PT_ADMIN`）を作る
2. グループ `translate-users` を作り、管理者を入れる
3. OIDCクライアントを2つ作る。どちらもグループ `translate-users` だけに許可する
   - `ptranslate-web`: Web用（oauth2-proxy）。client secretを作り、`.env` の `WEB_CLIENT_SECRET` に書く
   - `ptranslate-desktop`: PTranslate Desk用。公開クライアント（PKCE）、リダイレクトは `http://127.0.0.1/callback`
     （ポートを書かないループバックは任意のポートに一致します）
4. 管理者がパスキーを登録するためのURL（1時間有効）を出す

出たURLをtailnetの端末のブラウザーで開き、パスキーを登録します。
URLの期限が切れたら、`docker compose exec pocket-id /app/pocket-id one-time-access-token <ユーザー名>` で出し直せます。

管理者を作った後にもう一度流すと、APIキーを求められます。Pocket IDの管理画面でAPIキーを作り、
`POCKET_ID_API_KEY=<キー> docker compose run --rm -e POCKET_ID_API_KEY setup` で流します。
クライアントの設定が正しい値に戻ります。

## 4. 全体を起動する

```sh
docker compose up -d
docker compose logs -f llama-server   # 初回はモデルのダウンロード（1.7 GB）を待つ
```

`listening on http://0.0.0.0:8080` が出たら、tailnetの端末のブラウザーで `https://<PT_DOMAIN>/` を開きます。
Pocket IDのパスキーでログインし、初回は「サインイン」で許可します。原文を入れて訳せれば完了です。

## 5. 利用者を増やす

Pocket IDの管理画面（`https://<PT_DOMAIN>:8443/`）で利用者を作り、グループ `translate-users` に入れます。
その利用者のワンタイムのログインコード（ログイン用のURL）を作って本人に渡すと、本人がそのURLからパスキーを登録します。
コマンドで作るときは `docker compose exec pocket-id /app/pocket-id one-time-access-token <ユーザー名>` です。
利用者の端末もtailnetに入れてください（Tailscaleの管理画面からtailnetに招待するか、
マシンの共有を使います）。

グループに入っていない利用者がログインすると、Pocket IDの画面に「不明なエラーが発生しました」と出ます
（Pocket ID 2.19の表示）。その場合は、グループへの所属を確かめてください。

## 6. PTranslate Deskをつなぐ

[READMEのmacOSインストール手順](../README.md#インストールmacos)でアプリを入れ、「接続先の設定」に次を入れます。
`revocation_endpoint` を持たないIdP（Pocket ID）に対応したv0.2.6以降が必要です。

| 項目 | 値 |
| --- | --- |
| サーバーURL | `https://<PT_DOMAIN>` |
| OIDC discovery URL | `https://<PT_DOMAIN>:8443/.well-known/openid-configuration` |
| client ID | `ptranslate-desktop` |

Macもtailnetに入っている必要があります。ログインするとブラウザーでPocket IDが開くので、パスキーでログインします。

Pocket IDの違いで、次の点がAuthentikの構成と変わります。

- Pocket IDのaccess tokenにはユーザー名が入らないので、サーバーがIdPのuserinfoから利用者名を引きます
  （compose.ymlの `TR_USERINFO_URL`）
- Pocket IDにはrefresh tokenを無効にするURL（revoke）がありません。ログアウトは端末から消すだけで、
  サーバー側のrefresh tokenは有効期限（30日）まで残ります。端末をなくしたときは、本人がPocket IDの
  アカウントの画面で認可済みのアプリを取り消すか、管理者がその利用者をグループ `translate-users` から外します
  （Pocket IDはrefreshのたびにグループを確かめるので、次の更新で使えなくなります）

## 必要な性能

`tools/bench.py` で、PTranslateと同じプロンプトを翻訳エンジンに直接流して測りました（2026-11-07、
llama.cpp b11804、PLaMo 3 Translate 2BのQ4_K_M、`-c 65536 -np 4`）。
原文は約1,500文字（Webの画面が1回に送る目安）で、各条件2回の中央値です。

| 機械 | 日→英 1,500文字（1本） | 同時に4本（1本あたり） | 英→日 1,400文字（1本） | 訳を書く速さ（1本） |
| --- | --- | --- | --- | --- |
| Proxmox上の4 vCPUのVM（CPU種別EPYC-Rome-v4、AVX2あり） | 50秒 | 152秒 | 16秒 | 17〜21 トークン/秒 |
| Ryzen AI Max+ 395のCPU（16スレッド） | 13秒 | 24秒 | 4秒 | 57〜62 トークン/秒 |
| Ryzen AI Max+ 395のGPU（Radeon 8060S、Vulkan） | 7秒 | 9秒 | 2秒 | 100〜110 トークン/秒 |

- 訳は届いた分から画面に出ます。最初の文字が出るまでの時間は原文を読む速さで決まり、4 vCPUでは
  1,500文字（約600トークン）に約10秒、GPUでは1秒未満でした
- CPUでは、同時に流す本数を増やしても合計の速さはあまり伸びません（4 vCPUで1本14、4本18 トークン/秒）。
  1人で使うなら `LLAMA_PARALLEL=1` でも足ります。GPUでは4本で合計約300 トークン/秒まで伸びました
- 翻訳エンジンのメモリは待機時に1.3 GiB（`-np 1 -c 16384`）、1.5 GiB（既定の `-np 2 -c 32768`）、
  2.2 GiB（`-np 4 -c 65536`）でした。既定の設定で全体を動かすと、ほかの4つのコンテナは合わせて約90 MiBでした

自分の機械で測るときは、翻訳エンジンのポートに届く場所で次を流します。

```sh
python3 tools/bench.py --url http://127.0.0.1:8080/v1/completions --parallel 1,2,4
```

## GPUを使う

llama-serverのイメージをGPU向けに替え、デバイスを渡して `-ngl 99` を足します。
`compose.override.yml` に書くと、元のファイルを変えずに済みます。Vulkan（AMD・Intel）の例です。
上の表のGPUの値は、同じイメージと引数（podmanで `/dev/dri` を渡した）で測りました。

```yaml
services:
  llama-server:
    image: ghcr.io/ggml-org/llama.cpp:server-vulkan-b11804
    devices:
      - /dev/dri
    command: ["-hf", "pfnet/plamo-3-translate-2b-GGUF:Q4_K_M", "--host", "0.0.0.0", "--port", "8080",
              "-c", "65536", "-np", "4", "-ngl", "99", "-fa", "on"]
```

Strix Halo（Radeon 8060S）のVulkanでは、上流のllama.cppだと、同じ入力でも前のリクエストの状態で訳が少し変わります。
訳の質には影響しませんが、毎回同じ訳がほしいときは[halo-box/strix-llama.cpp](https://github.com/halo-box/strix-llama.cpp)を
使います（[調査の記録](REPEAT_INVESTIGATION.md)）。

NVIDIAは `server-cuda-b11804` とNVIDIA Container Toolkitを使います。
設定は[llama.cppのDockerの説明](https://github.com/ggml-org/llama.cpp/blob/master/docs/docker.md)を参照してください。

## tailnetの外から使う

Tailscale Funnelで、tailnetに入っていない端末からも使えるようにできます。
ログインの画面もインターネットに出るので、必要なときだけにしてください。
compose.ymlの `tailscale-serve` に `"AllowFunnel": {"$${TS_CERT_DOMAIN}:443": true, "$${TS_CERT_DOMAIN}:8443": true}`
を足し、tailnetのポリシーでこのマシンにFunnelを許可します（この構成でFunnelは試していません）。

## 保守

| 作業 | 方法 |
| --- | --- |
| バックアップ | `.env` と、ボリューム `ptranslate_pocket-id-data`（利用者・パスキー・クライアント）と `ptranslate_tailscale-state` |
| 更新 | compose.ymlのイメージのタグを上げて `docker compose up -d`。Pocket IDはメジャー版を上げる前にリリースノートを読む |
| 設定を変えた | `docker compose up -d --force-recreate <サービス>`。`configs` の中身を変えただけでは作り直されないことがあります |
| ログ | `docker compose logs <サービス>`。PTranslateは原文と訳を出しません |

compose.ymlの翻訳モデルは `-hf` でHugging Faceの最新を取ります。モデルの版を固定したいときは、
GGUFを手でダウンロードしてボリュームに置き、`-m` で指定してください。

## うまくいかないとき

| 症状 | 確かめること |
| --- | --- |
| `https://<PT_DOMAIN>/` が開けない | 端末がtailnetに入っているか、`PT_DOMAIN` が `tailscale status` の名前と同じか、HTTPS Certificatesが有効か |
| ログイン後に「不明なエラーが発生しました」 | 利用者がグループ `translate-users` に入っているか |
| oauth2-proxyが再起動を繰り返す | `.env` の `WEB_CLIENT_SECRET` が空でないか（`docker compose run --rm setup` を先に流す） |
| デスクトップで「ログインしていません」 | サーバーのイメージがv0.2.6以降か（`TR_USERINFO_URL` に対応）、利用者名が英小文字か |
| 訳が遅い・止まる | `docker compose logs llama-server`、メモリの空き、`LLAMA_PARALLEL` を下げる |

## 選ばなかった方法

| 方法 | 選ばなかった理由 |
| --- | --- |
| Authentik | 機能は多いが、PostgreSQLとworkerが要り、1台に詰めるには重い。既にある人は[構築ガイド](SELF_HOSTING.md) |
| Authelia、Keycloak | Autheliaは利用者をファイルかLDAPで持ち、画面から増やせない。Keycloakはメモリが大きい |
| CaddyやTraefikのforward auth | もう1つ入口のコンテナが要る。oauth2-proxyをリバースプロキシとして使えば足りる |
| Let's EncryptとCloudflare Tunnel | ドメインが要り、ログイン画面もインターネットに出る。Tailscaleならドメインもポート開放も要らない |
| 自己署名の証明書 | PTranslate DeskはOSの証明書ストアではなく組み込みのルート証明書で検証するので、つながらない |
