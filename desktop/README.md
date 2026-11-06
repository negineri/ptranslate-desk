# デスクトップ版 (macOS の主窓)

Tauri 2 の主窓をローカルの翻訳モックで試せます。Rust、Node.js、各 OS の
[Tauri の開発に必要なツール](https://v2.tauri.app/start/prerequisites/)を用意してください。Ubuntu では追加で `libdbus-1-dev` が必要です。

macOS は 13 以降に対応します。Mac では Xcode Command Line Tools (`xcode-select --install`) を使います。

## モックで起動する

リポジトリのルートで、次の 1 コマンドを実行します。

```sh
python3 tools/run_desktop_mock.py
```

画面をビルドし、翻訳エンジンのモックと翻訳サーバー、主窓を起動します。
「モックにログイン」を押し、原文を入力すると、1 秒後にモックの訳が届き始めます。
モックの訳は `[ja] Hello` のように、言語の印と原文を返します。
主窓を閉じると、起動したサーバーとモックも終了します。8002 番ポートが使用中なら終了します。

主窓では、自動翻訳、⌘Enter での手動翻訳、中止、空にする、訳のコピー、言語の入れ替え、
文体、文脈、用語集を使えます。

## 個別に起動する

1. リポジトリのルートで翻訳モックを起動します。

   ```sh
   python3 tools/mock_plamo.py --port 18082
   ```

2. 別の端末で、リポジトリのルートからサーバーを起動します。

   ```sh
   TR_LISTEN_HOST=127.0.0.1 TR_LISTEN_PORT=8002 \
     TR_UPSTREAM_URL=http://127.0.0.1:18082/v1/completions python3 -I server/translator.py
   ```

3. さらに別の端末で主窓を起動します。

   ```sh
   cd desktop
   npm ci
   npm run dev
   ```

4. 「モックにログイン」を押し、原文を入力します。「ログアウト」でモックのセッションを終了します。

接続先は `127.0.0.1:8002` 固定です。認証プロキシの代わりに Rust 側でモック利用者のヘッダーを付けます。
主窓は Web 版の画面と翻訳モジュールから生成します。言語・文体・用語集・自動翻訳の設定は、
OS 標準のアプリ設定ディレクトリにある `settings.json` に保存します。

## Mac のアプリをビルドする

`desktop/` で次を実行します。

```sh
npm ci
npm run build
```

`src-tauri/target/release/bundle/macos/翻訳.app` と
`src-tauri/target/release/bundle/dmg/` 内の dmg が生成されます。
現段階は開発用で、Developer ID の署名と公証はしていません。
アプリを単独で起動する場合も、上の手順のモックと翻訳サーバーを先に起動してください。

ビルド済みのアプリをモックとまとめて起動するには、リポジトリのルートで実行します。

```sh
python3 tools/run_desktop_mock.py --app desktop/src-tauri/target/release/bundle/macos/翻訳.app
```

開発用の実行ファイルだけを作る場合は、次を実行します。

```sh
node build.mjs
cargo build --locked --manifest-path src-tauri/Cargo.toml
```

## Mac の主窓を自動で試す

リポジトリのルートで実行します。Mac の GUI セッションが必要です。

```sh
python3 tools/run_desktop_mock.py --smoke-test
```

実際の WebView と Rust の通信を使って、モックへのログイン、53 言語の読み込み、自動翻訳、
ストリーミング、訳のコピー、中止、空にする、ログアウトを確認します。
設定は試験専用の一時ディレクトリに保存し、原文と訳を保存しないことも確認します。
試験後に元のクリップボードの文字列を戻します。
試験用の操作は `smoke-test` フィーチャーを有効にしたビルドだけに入ります。

## これから実装するもの

本番の Authentik 認証とキーチェーンへの保存は未実装です。サーバー側の準備が済むまではモックを使います。
選んだ文を訳すショートカット、小窓、メニューバー常駐、自動起動、履歴、自動更新も未実装です。
仕様は [SPEC](../docs/SPEC.md) を参照してください。
