# PTranslate Desk

[![CI](https://github.com/negineri/ptranslate-desk/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/negineri/ptranslate-desk/actions/workflows/ci.yml)

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/logo/png/horizontal-white@2x.png">
  <img src="docs/logo/png/horizontal@2x.png" width="390" alt="PTranslate Desk">
</picture>

PLaMo 3 Translate (llama.cpp の llama-server) を裏側にした、Web とデスクトップの翻訳アプリ。
文章を貼って訳す画面と、選んだ文をショートカットキーで訳す翻訳ポップアップが含まれます。

利用するには、翻訳サーバーと接続先のAuthentikアカウントが必要です。
サーバーを自分で用意する場合は[構築ガイド](docs/SELF_HOSTING.md)、
コードの変更や試験を行う場合は[開発ガイド](docs/DEVELOPMENT.md)を参照してください。

## 対応環境

| 利用方法                          | 対応環境                                                        | 備考                                                                       |
| --------------------------------- | --------------------------------------------------------------- | -------------------------------------------------------------------------- |
| デスクトップ版（PTranslate Desk） | macOS 13以降、Apple Silicon・Intel                              | 両CPU共通のuniversal版。選択した文章の取得にはアクセシビリティの許可が必要 |
| Windowsデスクトップ版 | Windows 10 / 11（x64） | NSISインストーラー。コード署名なし。Ctrl+Cの連続コピーに対応 |
| Web版（PTranslate）               | JavaScriptとストリーミング受信（Fetch API）に対応したブラウザー | インストール不要。構築したサーバーのURLをブラウザーで開く                  |

Linux向けデスクトップ版は未配布で、ショートカットによる選択文の取得も未実装です。
サーバー管理者がAuthentikで利用者と利用権限を管理します。

## インストール（Windows）

1. [最新のRelease](https://github.com/negineri/ptranslate-desk/releases/latest)で、Assetsから
   名前が `_x64-setup.exe` で終わるファイルをダウンロードして実行します。
   Windows版を含むReleaseから利用できます。
2. **Windowsコード署名は未対応です。** SmartScreenの警告が表示される場合があります。
   配布元が上記リポジトリであることと、取得したバージョンを確認してください。
   組織の端末で実行が制限される場合は管理者へ確認してください。
3. インストール後、PTranslate Deskを起動し、「接続先の設定」を保存・再起動します。
   「ログイン」から既定のブラウザで接続先のAuthentikにログインします。
4. ほかのアプリで文を選び、**Ctrl+Cを0.5秒以内に2回**押すと翻訳ポップアップを表示します。
   設定で**Ctrl+Shift+T**などの組み合わせにも切り替えられます。

ログインを維持する認証情報はWindowsの資格情報マネージャーに保存します。
メインウィンドウを閉じても通知領域に常駐します。終了時はトレイの「終了」を使ってください。
アンインストールはWindowsの「設定 → アプリ」から行います。端末を手放す場合は、先にアプリでログアウトしてください。

配布版では更新通知から内容を確認し、承認すると更新します。
更新ファイルはWindowsコード署名とは別のTauri更新署名で検証します。
Windowsではインストール時にアプリを終了し、完了後に起動し直します。

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

## 接続先を設定する

デスクトップの「接続先の設定」はログイン前から操作できます。管理者から次の3項目を受け取って入力し、
「保存して再起動」を押してください。再起動後、設定したサーバーにログインします。

| 項目               | 設定例                                                                                       |
| ------------------ | -------------------------------------------------------------------------------------------- |
| サーバーURL        | `https://translate.example.com`（パスなし）                                                  |
| OIDC discovery URL | `https://auth.example.com/application/o/translator-desktop/.well-known/openid-configuration` |
| client ID          | `translator-desktop`（公開クライアント、client secretなし）                                  |

接続先にはHTTPSが必要です。更新用の資格情報は接続先ごとにOSの資格情報の保管へ保存します。

## Web版を使う

管理者から受け取ったサーバーURLをブラウザーで開き、Authentikにログインします。
原文を入力し、言語や文体を選んで翻訳します。アプリのインストールは不要です。

## サーバーを自分で用意する

[構築ガイド](docs/SELF_HOSTING.md)に、翻訳エンジンとサーバーの導入、認証・HTTPSの設定、
デスクトップの接続、動作確認と保守までの手順をまとめています。
Macとは別のLinuxサーバーに構築することもできます。

## 開発者向けドキュメント

- [開発ガイド](docs/DEVELOPMENT.md): リポジトリの構成、モック起動、試験、ビルド、CI
- [デスクトップの開発手順](desktop/README.md)
- [仕様](docs/SPEC.md)
- [Releaseの作成と更新署名](docs/RELEASING.md)

## ライセンス

MIT ([LICENSE](LICENSE))。翻訳モデルには別途[PLaMo Community License](https://huggingface.co/pfnet/plamo-3-translate-2b-GGUF)が適用されます。
