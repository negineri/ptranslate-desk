# デスクトップ版の試作

Tauri 2 の主窓をローカルの翻訳モックで試せます。Rust、Node.js、各 OS の
[Tauri の開発に必要なツール](https://v2.tauri.app/start/prerequisites/)を用意してください。Ubuntu では追加で `libdbus-1-dev` が必要です。

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
   npx --yes @tauri-apps/cli@2 dev
   ```

4. 「モックにログイン」を押し、原文を入力します。「ログアウト」でモックのセッションを終了します。

接続先は `127.0.0.1:8002` 固定です。認証プロキシの代わりに Rust 側でモック利用者のヘッダーを付けます。
主窓は Web 版の画面と翻訳モジュールから生成します。言語・文体・用語集・自動翻訳の設定は、
OS 標準のアプリ設定ディレクトリにある `settings.json` に保存します。

開発用の実行ファイルを作る場合は、`desktop/` で次を実行します。

```sh
node build.mjs
cargo build --locked --manifest-path src-tauri/Cargo.toml
```
