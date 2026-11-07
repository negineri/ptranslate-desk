# 開発ガイド

利用方法と対応環境は[README](../README.md)、サーバーの導入・運用は[構築ガイド](SELF_HOSTING.md)を参照してください。
この文書ではコードの構成、ローカル開発、試験、CIを説明します。

## リポジトリの構成

| 部分 | 場所 | 内容 |
| --- | --- | --- |
| サーバー | `server/translator.py` | Pythonの標準ライブラリだけで動くAPI |
| Web画面 | `server/static/` | ビルド不要のHTMLと、区切り・SSEなどの翻訳モジュール |
| デスクトップ | `desktop/` | Tauri 2。Web画面と翻訳モジュールを共有 |
| Chrome拡張 | `chrome-extension/` | Manifest V3。Web版のログインを使い、翻訳モジュールを共有（[説明](../chrome-extension/README.md)） |
| 翻訳エンジンのモック | `tools/mock_plamo.py` | 実エンジンに接続せず、試験と開発に使う |
| 翻訳エンジンの計測 | `tools/bench.py` | 実エンジンの速さを測る（[かんたん構築ガイド](SELF_HOSTING_COMPOSE.md#必要な性能)）。試験では使わない |
| かんたん構築 | `deploy/compose/` | Tailscale + Pocket IDのcompose.ymlと、Pocket IDの初期設定（`setup.py`） |
| デスクトップのモック起動 | `tools/run_desktop_mock.py` | モック・サーバー・アプリをまとめて起動 |
| Chrome拡張のモック起動 | `tools/run_extension_mock.py` | モック・サーバー・認証プロキシの代わり（開発用の利用者名を付ける）をまとめて起動 |
| 仕様 | [SPEC.md](SPEC.md) | API、画面、認証などの規則 |

## 開発環境

Python 3.13（サーバーCIと同じ版）、Node.js 22、Ruff 0.17.3を使います。
サーバーに追加のPython依存パッケージはありません。
デスクトップの変更にはRustのstableと各OSのTauri開発環境が必要です。
macOSの準備とビルドは[デスクトップの開発手順](../desktop/README.md)を参照してください。

## サーバーをモックで起動する

リポジトリのルートから実行します。

```sh
python3 tools/mock_plamo.py --port 18082
```

別の端末で同じリポジトリに移動してサーバーを起動します。

```sh
TR_UPSTREAM_URL=http://127.0.0.1:18082/v1/completions python3 -I server/translator.py
```

さらに別の端末から、認証プロキシの代わりに開発用の利用者名を付けてAPIを呼びます。

```sh
curl --fail -H 'X-Forwarded-User: local-test' http://127.0.0.1:8002/api/config
```

両プロセスはCtrl+Cで終了します。デスクトップも含めて起動する場合は次を使います。

```sh
python3 tools/run_desktop_mock.py
```

## サーバーの設定とコンテナ

環境変数とコンテナの起動例は[構築ガイド](SELF_HOSTING.md#3-翻訳apiwebを起動する)、
環境変数の一覧は[サーバーの環境変数](SELF_HOSTING.md#サーバーの環境変数)を参照してください。

サーバー自体は認証しません。公開時は前段の認証プロキシが検証した利用者名を
`X-Forwarded-User` で渡し、APIの待ち受けを外部に直接公開しないでください。
`TR_USERINFO_URL` を設定した場合だけ、`X-Forwarded-User` が無いBearerのリクエストの利用者名を
IdPのuserinfoから引きます（[SPEC 5章](SPEC.md#5-サーバーの-api)）。
`/api/translate` はSSEで応答するため、nginxなどのプロキシでbufferingを無効にします。

Dockerfileを変更した場合は、リポジトリのルートからビルドします。

```sh
docker build -t ptranslate-server:dev .
```

## 翻訳エンジンとの通信

[pfnet/plamo-3-translate-2b-GGUF](https://huggingface.co/pfnet/plamo-3-translate-2b-GGUF) などを llama-server で
動かす。1 本あたりのコンテキストは 16K を勧める (原文と訳の合計。8K では長めの段落が切れ、約 6,000 トークンを
超える原文は繰り返しに入る)。プロンプトはサーバーが PFML3 (モデルの素の書式) で組むので、`--jinja` は要らない。
モデルのライセンスは PLaMo Community License (このリポジトリのコードは MIT)。

## 試験と整形

リポジトリのルートから実行します。試験はローカルのモックだけを使い、実エンジンには接続しません。
認証の試験もローカルのOIDCモックとメモリ上の資格情報の保管を使います。

```sh
ruff format --check server tools
python3 -m unittest discover -s server/tests -v
node --test server/tests/test_translation.mjs
node chrome-extension/build.mjs
node --test chrome-extension/tests/
node desktop/build.mjs
cargo fmt --manifest-path desktop/src-tauri/Cargo.toml --check
cargo test --locked --manifest-path desktop/src-tauri/Cargo.toml
```

macOSのGUIセッションでは、実際のWebViewを使うスモーク試験も実行できます。

```sh
python3 tools/run_desktop_mock.py --smoke-test
```

スモーク試験はOSの実際のキー入力を監視する部分を通さないため、
アクセシビリティの許可と他のアプリでのショートカットは手動でも確認します。
原文・訳・トークン・秘密鍵・実際の利用者名を試験データやログに入れないでください。

## CIとRelease

[ci.yml](../.github/workflows/ci.yml)でバージョンの整合性、サーバーとデスクトップの試験、
macOS universal版とコンテナのビルドを行います。mainとPRではアプリをActionsの成果物に保存します。
`v*` タグではサーバーイメージとmacOSのdmg・署名付き更新ファイルをGitHub Releases/GHCRに公開します。
タグとアプリのバージョンを一致させてください。
手順とsecretsの設定は[Releaseの作成と更新署名](RELEASING.md)を参照してください。
