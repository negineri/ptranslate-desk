# PTranslate Desk

[![CI](https://github.com/negineri/ptranslate-desk/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/negineri/ptranslate-desk/actions/workflows/ci.yml)

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/logo/png/horizontal-white@2x.png">
  <img src="docs/logo/png/horizontal@2x.png" width="390" alt="PTranslate Desk">
</picture>

PLaMo 3 Translate (llama.cpp の llama-server) を裏側にした、Web とデスクトップの翻訳アプリ。
文章を貼って訳す画面と、選んだ文をショートカットキーで訳す翻訳ポップアップが含まれます。

| 部分                               | 場所                                               | 状態                                                                                                                         |
| ---------------------------------- | -------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| サーバー (API と Web の画面を返す) | `server/translator.py`、`server/static/index.html` | Pythonの標準ライブラリで動作。認証プロキシを前段に置いて利用                                                                    |
| デスクトップのクライアント         | `desktop/`                                         | macOS版を配布。メインウィンドウ・ショートカット・翻訳ポップアップ・メニューバー常駐に対応: [起動とビルド](desktop/README.md) |
| 翻訳エンジンのモック               | `tools/mock_plamo.py`                              | 試験と手元の開発用                                                                                                           |

macOSクライアントは `v*` タグでGitHub Releasesにuniversal dmgを配布します。
アプリの更新は利用者が承認してから適用します。[配布・更新の手順](docs/RELEASING.md) を参照してください。

## 対応環境

| 利用方法                          | 対応環境                                                        | 備考                                                                       |
| --------------------------------- | --------------------------------------------------------------- | -------------------------------------------------------------------------- |
| デスクトップ版（PTranslate Desk） | macOS 13以降、Apple Silicon・Intel                              | 両CPU共通のuniversal版。選択した文章の取得にはアクセシビリティの許可が必要 |
| Web版（PTranslate）               | JavaScriptとストリーミング受信（Fetch API）に対応したブラウザー | インストール不要。構築したサーバーのURLをブラウザーで開く             |

Windows・Linux向けデスクトップ版は未配布で、ショートカットによる選択文の取得も未実装です。
利用には、接続先のAuthentikアカウントとサーバーへのネットワーク接続が必要です。
サーバー管理者がAuthentikで利用者と利用権限を管理します。

## 自分でセットアップする

サーバーも自分で用意できます。[最初から構築する手順](docs/SELF_HOSTING.md)に、次をまとめています。

1. ソースの取得とローカルの翻訳モックでの確認
2. PLaMo・llama-serverの導入と、翻訳API/Webの起動・常駐化
3. Authentikの導入、利用者・グループ・Web/デスクトップ用OIDCクライアントの作成
4. oauth2-proxyによるCookie/Bearer認証と、nginx・証明書によるHTTPS公開
5. デスクトップの接続先設定、動作確認、トラブルシュート、保守

実運用例はUbuntu 24.04以降のLinuxサーバーです。DNS、HTTPS証明書、Python 3.13以降、
Docker/Compose v2、nginx、翻訳モデルを動かすRAM/VRAMとディスクを用意します。
クライアントのMacとサーバーは別の環境でも構いません。
ローカルのモック確認では、PythonだけでAPIと翻訳の通信を試せます。

デスクトップの「接続先の設定」はログイン前から操作できます。管理者から次の3項目を受け取って入力し、
「保存して再起動」を押してください。再起動後、設定したサーバーにログインします。

| 項目 | 自分で構築する場合の例 |
| --- | --- |
| サーバーURL | `https://translate.example.com`（パスなし） |
| OIDC discovery URL | `https://auth.example.com/application/o/translator-desktop/.well-known/openid-configuration` |
| client ID | `translator-desktop`（公開クライアント、client secretなし） |

接続先にはHTTPSが必要です。更新用の資格情報は接続先ごとにOSの資格情報の保管へ保存します。
**公開中のv0.1.0には接続先設定がありません。** 対応版のRelease公開までは、
[ソースからビルド](desktop/README.md#mac-のアプリをビルドする)して利用してください。

## インストール（macOS）

1. [最新のRelease](https://github.com/negineri/ptranslate-desk/releases/latest)を開き、
   Assetsから名前が `_universal.dmg` で終わるファイルをダウンロードします。
2. dmgを開き、**PTranslate Desk**を**アプリケーション**フォルダへドラッグします。
3. アプリケーションフォルダのPTranslate Deskを起動します。AppleのDeveloper ID署名・公証は未対応です。
   起動がブロックされた場合は、「システム設定 → プライバシーとセキュリティ」で
   PTranslate Deskの「このまま開く」を選びます。
4. 「接続先の設定」に利用するサーバーと認証の設定を入力して、保存・再起動します。
   「ログイン」を押し、接続先のAuthentikにログインします。初回は同意画面で許可します。
5. キーチェーンの確認が出たら、アプリ名が「PTranslate Desk」、保存先が
   `com.witenp.ptranslate-desk` であることを確認します。キーチェーン「ログイン」のパスワード
   （通常はMacのログインパスワード）をmacOSの確認画面に入力し、**「常に許可」** を押します。
   ログイン状態を保つ認証情報を使うための確認です。更新・差し替え後に再び表示される場合があります。
6. ショートカットを使う場合は、アプリの「デスクトップの設定 → アクセシビリティの設定を開く」から
   PTranslate Deskを許可します。別のアプリで文章を選択し、**⌘Cを0.5秒以内に2回**押すと翻訳ポップアップが開きます。
   設定で**⌘⇧T**にも切り替えられます。

メインウィンドウを閉じてもメニューバーに常駐します。終了するにはメニューバーの「終了」か⌘Qを使います。
更新通知から内容を確認し、承認すると新しい版をインストールできます。
ソースから実行する場合は[デスクトップ版の起動・ビルド手順](desktop/README.md)を参照してください。

## サーバーを動かす

```bash
# 手元で (翻訳エンジンの代わりにモックを使う)
python3 tools/mock_plamo.py --port 18082 &
TR_UPSTREAM_URL=http://127.0.0.1:18082/v1/completions python3 -I server/translator.py
# 認証プロキシの代わりに X-Forwarded-User を付けて試す
curl -s -H 'X-Forwarded-User: alice' http://127.0.0.1:8002/api/config

# コンテナで (v* のタグで ghcr.io/negineri/ptranslate-desk/server に出る)
docker run --rm -p 127.0.0.1:8002:8002 -e TR_UPSTREAM_URL=https://llama.example/v1/completions \
  -e TR_API_KEY=... 'ghcr.io/negineri/ptranslate-desk/server:<version>'
```

サーバーは認証を持たない。前段に認証プロキシ (oauth2-proxy など) を置き、ログインした利用者名を
`X-Forwarded-User` で渡すこと。待ち受けは前段からしか届かないようにする。`/api/translate` は
Server-Sent Events なので、前段で応答を溜めない (nginx なら `proxy_buffering off`)。

| 環境変数                            | 既定                                          | 意味                                             |
| ----------------------------------- | --------------------------------------------- | ------------------------------------------------ |
| `TR_UPSTREAM_URL`                   | (必須)                                        | 翻訳エンジン (llama-server) の `/v1/completions` |
| `TR_API_KEY`                        | 空                                            | 翻訳エンジンに付ける `Authorization: Bearer`     |
| `TR_MODEL` | `plamo-3-translate` | 翻訳エンジンに渡すモデル名 |
| `TR_LISTEN_HOST` / `TR_LISTEN_PORT` | `127.0.0.1` / `8002` (コンテナでは `0.0.0.0`) | 待ち受け                                         |
| `TR_MAX_CHARS`                      | 4000                                          | 1 回の原文の上限 (文字)                          |
| `TR_MAX_PROMPT_CHARS`               | 12000                                         | 文脈・前の訳・用語集を合わせた上限 (文字)        |
| `TR_MAX_GLOSSARY` | 200 | 用語集の組数の上限 |
| `TR_MAX_ACTIVE_PER_USER`            | 2                                             | 1 人が同時に流せる翻訳の数                       |
| `TR_UPSTREAM_TIMEOUT` | 600 | 翻訳エンジンとの通信タイムアウト（秒） |
| `TR_HEARTBEAT_SECONDS` | 15 | SSEのハートビート間隔（秒） |
| `TR_STATIC_DIR` | `server/static/`（コンテナでは `/app/static`） | Webの静的ファイルの配置先 |

公開用の認証・HTTPS構成は[構築手順](docs/SELF_HOSTING.md#4-authentikとhttpsを用意する)を参照してください。

## 翻訳エンジン

[pfnet/plamo-3-translate-2b-GGUF](https://huggingface.co/pfnet/plamo-3-translate-2b-GGUF) などを llama-server で
動かす。1 本あたりのコンテキストは 16K を勧める (原文と訳の合計。8K では長めの段落が切れ、約 6,000 トークンを
超える原文は繰り返しに入る)。プロンプトはサーバーが PFML3 (モデルの素の書式) で組むので、`--jinja` は要らない。
モデルのライセンスは PLaMo Community License (このリポジトリのコードは MIT)。

## 開発

```bash
python3 -m unittest discover -s server/tests -v
node --test server/tests/test_translation.mjs
ruff format --check server tools   # CI と同じ版 (.github/workflows/ci.yml)
```

## ライセンス

MIT ([LICENSE](LICENSE))。
