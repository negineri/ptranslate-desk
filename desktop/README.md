# PTranslate Desk (macOS)

Tauri 2 の主窓をローカルの翻訳モックで試せます。Rust、Node.js、各 OS の
[Tauri の開発に必要なツール](https://v2.tauri.app/start/prerequisites/)を用意してください。Ubuntu では追加で `libdbus-1-dev` が必要です。

macOS は 13 以降に対応します。Mac では Xcode Command Line Tools (`xcode-select --install`) を使います。

## 本番にログインする

アプリを起動して「ログイン」を押すか、メニューバーの「ログイン」を選びます。
OS の既定のブラウザで Authentik にログインし、初回の同意の画面で許可してください。
`tamesareru-members` に入っていないアカウントは Authentik の画面で断られます。
ログイン後、主窓に Authentik の利用者名が表示されます。

接続先は `https://translate.witenp.com`、認証は公開クライアント `translator-desktop` の
Authorization Code + PKCE (S256) です。authorize・token・revoke の URL は discovery から取得し、
scope は `openid profile offline_access` に限定します。`email` は要求しません。

access token は Rust のメモリ、refresh token は macOS のキーチェーン (Windows は資格情報マネージャー、
Linux は Secret Service) に保存します。トークンを画面・ログ・設定ファイルへ渡しません。
起動時と有効期限の 1 分前、API の 401 応答時に更新します。更新に失敗したときや 403 を受けたときは、
ログインし直します。403 の HTML は JSON として読みません。
ログアウトすると端末の資格情報を削除し、discovery の revoke URL でも無効化します。

## モックで起動する

リポジトリのルートで、次の 1 コマンドを実行します。

```sh
python3 tools/run_desktop_mock.py
```

画面をビルドし、翻訳エンジンのモックと翻訳サーバー、主窓を起動します。
「モックにログイン」を押し、原文を入力すると、1 秒後にモックの訳が届き始めます。
モックの訳は `[ja] Hello` のように、言語の印と原文を返します。
主窓を閉じてもメニューバーに常駐します。メニューバーの「終了」か ⌘Q でアプリを終了すると、
起動したサーバーとモックも終了します。8002 番ポートが使用中なら終了します。

主窓では、自動翻訳、⌘Enter での手動翻訳、中止、空にする、訳のコピー、言語の入れ替え、
文体、文脈、用語集を使えます。

## 選んだ文を小窓で訳す

1. 「モックにログイン」し、主窓の「デスクトップの設定」を開きます。
2. 「アクセシビリティの設定を開く」を押し、macOS の「プライバシーとセキュリティ →
   アクセシビリティ」で「PTranslate Desk」を許可します。開発用の実行ファイルで試す場合は、実行している
   アプリまたは端末に許可が必要な場合があります。許可後はアプリを再起動します。
3. ほかのアプリで文を選び、⌘C を 0.5 秒以内に 2 回押します。コピーされた文をマウスの近くの
   小窓で訳し、届いた分から表示します。

設定で ⌘⇧T に切り替えることもできます。この場合はアプリが ⌘C を送り、選んだ文を取得した後、
元のクリップボードの文字列を戻します。選択が無いときやコピーできないアプリでは、主窓にエラーを表示します。
⌘C を 2 回押す方式では、自分でコピーした文をそのまま使います。

小窓には「コピー」「主窓で開く」「閉じる」があります。Esc、外のアプリへのフォーカス移動、
閉じるボタンで消え、翻訳サーバーへの接続も切ります。主窓で開くと原文を引き継いで翻訳します。
言語・文体・用語集は主窓と共通です。小窓と主窓の翻訳は同時に流さず、新しい翻訳を優先します。

メニューバーには「主窓を開く」(未ログインなら「ログイン」)、「ショートカットを一時的に止める」、
「設定」「ログアウト」「終了」があります。未ログイン時はアイコンも変わります。
主窓を閉じた後は、メニューバーか Dock のアイコンから開けます。

ショートカットによる選択文の取得は、現在 macOS 向けに実装しています。

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
   PTRANSLATE_MOCK=1 npm run dev
   ```

4. 「モックにログイン」を押し、原文を入力します。「ログアウト」でモックのセッションを終了します。

モック起動では `PTRANSLATE_MOCK=1` を渡します。この場合だけ接続先を `127.0.0.1:8002` に固定し、
認証プロキシの代わりに Rust 側でモック利用者のヘッダーを付けます。モックは OS の資格情報に触れません。
主窓は Web 版の画面と翻訳モジュールから生成します。言語・文体・用語集・自動翻訳の設定は、
OS 標準のアプリ設定ディレクトリにある `settings.json` に保存します。

## Mac のアプリをビルドする

`desktop/` で次を実行します。

```sh
npm ci
npm run build
```

`src-tauri/target/release/bundle/macos/PTranslate Desk.app` と
`src-tauri/target/release/bundle/dmg/` 内の dmg が生成されます。
現段階は開発用で、Developer ID の署名と公証はしていません。
タグからの配布はApple Silicon・Intel共通のuniversal dmgです。
Releaseの作成、更新署名のsecrets、バージョンの合わせ方は [配布手順](../docs/RELEASING.md) を参照してください。
アプリを単独で起動すると、本番にログインできます。

ビルド済みのアプリをモックとまとめて起動するには、リポジトリのルートで実行します。

```sh
python3 tools/run_desktop_mock.py --app 'desktop/src-tauri/target/release/bundle/macos/PTranslate Desk.app'
```

開発用の実行ファイルだけを作る場合は、次を実行します。

```sh
node build.mjs
cargo build --locked --manifest-path src-tauri/Cargo.toml
```

## Mac の主窓・小窓・常駐を自動で試す

リポジトリのルートで実行します。Mac の GUI セッションが必要です。

```sh
python3 tools/run_desktop_mock.py --smoke-test
```

実際の WebView と Rust の通信を使って、モックへのログイン、53 言語の読み込み、自動翻訳、
ストリーミング、訳のコピー、中止、空にする、ログアウトを確認します。
さらにショートカットの登録と変更、クリップボード取得から小窓を出す処理、小窓の逐次翻訳、
Esc とフォーカス移動での中止、主窓への引き継ぎ、主窓を閉じた後の常駐、メニュー操作を試します。
⌘C を 2 回押す判定は Rust の単体試験でも確認します。
OS の実際のキー監視と、ほかのアプリへのコピーキー送信は、アクセシビリティを許可して手動でも確認してください。
設定は試験専用の一時ディレクトリに保存し、原文と訳を保存しないことも確認します。
試験後に元のクリップボードの文字列を戻します。
試験用の操作は `smoke-test` フィーチャーを有効にしたビルドだけに入ります。

## これから実装するもの

自動起動、履歴、Windows・Linux でのショートカットによる選択文の取得は未実装です。
仕様は [SPEC](../docs/SPEC.md) を参照してください。
