# PTranslate Desk

[![CI](https://github.com/negineri/ptranslate-desk/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/negineri/ptranslate-desk/actions/workflows/ci.yml)

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/logo/png/horizontal-white@2x.png">
  <img src="docs/logo/png/horizontal@2x.png" width="390" alt="PTranslate Desk">
</picture>

PLaMo 3 Translate (llama.cpp の llama-server) を裏側にした、Web とデスクトップの翻訳アプリ。
文章を貼って訳す画面と、選んだ文をショートカットキーで訳す翻訳ポップアップが含まれます。

Windowsでは初回に、このPCで翻訳する[ローカルモード](docs/LOCAL_TRANSLATION.md)と、サーバーモードを選べます。
サーバーモードには翻訳サーバーと、そのサーバーがログインに使うOIDCのIdP（Pocket ID、Authentikなど）の
アカウントが必要です。サーバーを自分で用意する場合は[サーバーを自分で用意する](#サーバーを自分で用意する)、
コードの変更や試験を行う場合は[開発ガイド](docs/DEVELOPMENT.md)を参照してください。

## 対応環境

| 利用方法                          | 対応環境                                                        | 備考                                                                       |
| --------------------------------- | --------------------------------------------------------------- | -------------------------------------------------------------------------- |
| デスクトップ版（PTranslate Desk） | macOS 13以降、Apple Silicon・Intel                              | 両CPU共通のuniversal版。選択した文章の取得にはアクセシビリティの許可が必要 |
| Windowsデスクトップ版 | Windows 10 / 11（x64） | NSISインストーラー。コード署名なし。Ctrl+Cの連続コピーに対応 |
| Web版（PTranslate）               | JavaScriptとストリーミング受信（Fetch API）に対応したブラウザー | インストール不要。構築したサーバーのURLをブラウザーで開く                  |
| Chrome拡張（PTranslate for Chrome） | Chrome 120以降、Edgeなど Chromium系のブラウザー | ページで選んだ文やページ全体を訳す。Web版のログインを使う。ストア配布は未対応 |

Linux向けデスクトップ版は未配布で、ショートカットによる選択文の取得も未実装です。
サーバー管理者がIdPで利用者と利用権限を管理します。

## インストール（Windows）

1. [最新のRelease](https://github.com/negineri/ptranslate-desk/releases/latest)で、Assetsから
   名前が `_x64-setup.exe` で終わるファイルをダウンロードして実行します。
   Windows版を含むReleaseから利用できます。
2. **Windowsコード署名は未対応です。** SmartScreenの警告が表示される場合があります。
   配布元が上記リポジトリであることと、取得したバージョンを確認してください。
   組織の端末で実行が制限される場合は管理者へ確認してください。
3. 起動後に「このPCで翻訳」または「サーバーで翻訳」を選びます。
   ローカルモードはモデルの取得とデバイス選択を進め、保存・再起動します。
   サーバーモードは「接続先の設定」を保存・再起動し、「ログイン」から接続先のIdPにログインします。
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
   「ログイン」を押し、接続先のIdPにログインします。初回は同意画面で許可します。
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

Web版の「クライアントの接続設定」ボタンから、デスクトップに入力する3項目を表示・コピーできます。
認証先が未設定と表示された場合は管理者に確認してください。

デスクトップの「接続先の設定」はログイン前から操作できます。管理者から次の3項目を受け取って入力し、
「保存して再起動」を押してください。再起動後、設定したサーバーにログインします。

| 項目               | 設定例（Pocket IDの構成）                                                  | 設定例（Authentikの構成）                                                                    |
| ------------------ | -------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| サーバーURL        | `https://ptranslate.example.ts.net`（パスなし）                            | `https://translate.example.com`（パスなし）                                                  |
| OIDC discovery URL | `https://ptranslate.example.ts.net:8443/.well-known/openid-configuration` | `https://auth.example.com/application/o/translator-desktop/.well-known/openid-configuration` |
| client ID          | `translator-desktop`                                                       | `translator-desktop`                                                                         |

client IDは公開クライアント（client secretなし、PKCE）のものです。

接続先にはHTTPSが必要です。更新用の資格情報は接続先ごとにOSの資格情報の保管へ保存します。

## Web版を使う

管理者から受け取ったサーバーURLをブラウザーで開き、IdPにログインします。
原文を入力し、言語や文体を選んで翻訳します。アプリのインストールは不要です。

## Chrome拡張を使う

ブラウザーで読んでいるページの文を選び、右クリックの「PTranslate で翻訳」か
**Alt+Shift+T** で訳します。ページ全体は右クリックの「PTranslate でこのページを翻訳」か **Alt+Shift+P** で訳し、
「原文に戻す」で戻せます。拡張の設定にサーバーURLを入れ、同じブラウザーでWeb版にログインしておきます。
拡張は自分ではログインせず、Web版のログインを使うので、IdPの設定は変えなくて構いません。
読み込み方と仕組みは[Chrome拡張の説明](chrome-extension/README.md)を参照してください。

## サーバーを自分で用意する

Linuxのサーバー1台で動きます。GPUは要りません。構成は2通りあります。

| 構成 | 向いている場合 | 手順 |
| --- | --- | --- |
| Docker Compose 1つ（Tailscale + Pocket ID） | IdPもリバースプロキシも無い。家族や自分だけで使う | [かんたん構築ガイド](docs/SELF_HOSTING_COMPOSE.md) |
| 個別に組む（nginx + oauth2-proxy + Authentik など） | ドメインを公開して使う。既にIdPやリバースプロキシがある | [構築ガイド](docs/SELF_HOSTING.md) |

かんたん構築では外に開けるポートが無く、tailnetに入った端末だけから使えます。証明書はTailscaleが発行します。

翻訳モデル（PLaMo 3 Translate 2B、Q4_K_M）は1.7 GBで、翻訳エンジンのメモリは1.5 GiBほどです。CPUだけでも動きます。
1,500文字ほどの日本語を英語に訳す時間の目安です（[測定の詳細](docs/SELF_HOSTING_COMPOSE.md#必要な性能)）。

| 機械 | 1本 | 同時に4本（1本あたり） |
| --- | --- | --- |
| 4 vCPUのVM（AVX2） | 50秒 | 152秒 |
| Ryzen AI Max+ 395のCPU | 13秒 | 24秒 |
| Ryzen AI Max+ 395のGPU（Radeon 8060S） | 7秒 | 9秒 |

訳は届いた分から画面に出ます（4 vCPUでも10秒ほどで出始めます）。

## 開発者向けドキュメント

- [開発ガイド](docs/DEVELOPMENT.md): リポジトリの構成、モック起動、試験、ビルド、CI
- [デスクトップの開発手順](desktop/README.md)
- [Chrome拡張](chrome-extension/README.md)
- [仕様](docs/SPEC.md)
- [Releaseの作成と更新署名](docs/RELEASING.md)
- [訳の繰り返しと揺れの調査](docs/REPEAT_INVESTIGATION.md)

## ライセンス

MIT ([LICENSE](LICENSE))。翻訳モデルには別途[PLaMo Community License](https://huggingface.co/pfnet/plamo-3-translate-2b-GGUF)が適用されます。
