# PTranslate Desk の macOS 配布

## タグとバージョン

`v*` のタグをpushすると、サーバーイメージとmacOSクライアントを同じタグで配布します。
配布バージョンはビルド対象のHEADに付いたgitタグから取得します。例えば `v0.2.2` ならアプリは `0.2.2` です。
設定ファイルやnpm・Cargoのバージョンは `0.0.0-dev` のままで、リリースごとの書き換えは不要です。

mainへ反映後、配布するコミットへタグを付けてpushします。

```sh
git tag v0.2.2
git push origin v0.2.2
```

CIはタグが正しいSemVerでHEADを指すことと、追跡ファイルに変更がないことを確認します。
`tools/check_release_version.py --tag "$RELEASE_TAG" --write-config desktop/src-tauri/gen/release-version.json`
でバージョンと更新署名の設定をまとめたTauri設定を生成し、Tauriの `--config` へ渡します。
生成ファイルはgitで管理せず、既定の設定・npm・Cargoのバージョンも書き換えません。
アプリ内表示・配布ファイル名・更新manifestはいずれもこのバージョンを使います。

ローカルでは `npm run build` がデバッグビルド（`target/debug/`）となり、バージョンは `0.0.0-dev` です。
タグの付いたコミットでも通常のビルドは開発用です。明示的に配布ビルドを行う場合だけ、
HEADへタグを付けた状態で `npm run build:release` を使います。更新署名には後述の鍵が必要です。
`python3` とgitを利用できる環境が必要です。

`main` とPRではuniversalの `.app` をzipにまとめ、Actionsの成果物として保存します。
タグでは、試験とサーバーイメージの公開が成功してから `desktop-release-build` が秘密鍵なしで配布用アプリをビルドします。
別の新しいランナーの `desktop-release` が署名・パッケージ作成・公開を担当します。
両ジョブでタグのコミットがmainに含まれることを確認します。
`-beta.1` などを含むタグはGitHubのprereleaseとして公開し、通常の更新対象にはしません。

## Release の内容

macOS 13以降のApple Silicon・Intelに対応する `universal-apple-darwin` でビルドします。

- `.dmg`: 利用者が最初にインストールするファイル
- `.app.tar.gz` と `.sig`: Tauri updaterが使うアーカイブと署名
- `latest.json`: 同じuniversalアーカイブを `darwin-aarch64` と `darwin-x86_64` に割り当てた更新情報

Release作成後にも `latest.json` のバージョン、HTTPS、署名、両アーキテクチャの対応を検査します。
公開先は [GitHub Releases](https://github.com/negineri/ptranslate-desk/releases) です。

## 更新署名の鍵

更新の署名はAppleのDeveloper ID署名とは別です。公開鍵を `desktop/src-tauri/tauri.conf.json` の
`plugins.updater.pubkey` に置き、秘密鍵とパスワードは `release-signing` EnvironmentのActions secretsで管理します。
リポジトリSecretsには残しません。

| secret | 内容 |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | `tauri signer generate` で作った、暗号化された秘密鍵の内容 |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | 秘密鍵のパスワード |

秘密を使うのは、承認を受けたタグ用の署名ジョブだけです。ビルドジョブには渡しません。
署名ジョブは共有キャッシュを復元せず、Cargo・アプリ・ビルドフックを実行しません。
npmはロックファイルの整合性を検証し、`--ignore-scripts` で署名用CLIだけを準備します。
更新署名はCLIの `signer sign --app-version` を直接呼び、秘密はこのステップだけに渡します。
秘密鍵をファイル・ログ・コミット・Actionsの成果物に入れないでください。
既存の鍵を失うと、既に配ったアプリが新しい鍵の更新を受け取れなくなります。
鍵の変更は、既存の公開鍵を持つアプリからの移行手順を用意して行います。

Release用設定 `desktop/src-tauri/tauri.release.conf.json` を基にした生成設定でだけ `createUpdaterArtifacts` を有効にします。
通常の開発ビルドには秘密鍵は不要です。

## アプリでの更新

配布用アプリは起動時と24時間ごとに、GitHub Releasesの `latest.json` を確認します。
開発ビルド・モック・GUIスモーク試験は自動ではGitHubに接続しません。
メインウィンドウの「更新を確認」からも手動で確認できます。ログイン前でも操作できます。

「更新する」で内容を確認し、「インストールする」を押した後にだけダウンロードと適用を行います。
署名と署名内のバージョンを検証し、失敗した場合は適用しません。
完了後は「再起動する」を押すと新しい版で起動します。

## Apple の署名・公証

配布アプリは `desktop/signing/certificate.pem` の固定した自己署名証明書で署名します。
アプリのBundle IDと証明書を維持し、更新前後でmacOSの識別条件（Designated Requirement）が変わらないようにします。
旧来のアドホック署名から切り替える最初の1回は、アクセシビリティの許可を再登録してください。
その後は同じ識別条件で許可を引き継ぐ構成です。OSによる実際の許可の継続は、更新前後で確認してください。

| Actions secret | 内容 |
| --- | --- |
| `MACOS_SIGNING_CERTIFICATE` | 固定証明書と秘密鍵を含む `.p12` のbase64 |
| `MACOS_SIGNING_PASSWORD` | `.p12` のパスワード |

GPG秘密鍵はCIへ渡しません。ローカルのGPG暗号化バックアップから復元したコード署名鍵だけを使います。
公開証明書はコミットしますが、秘密鍵・p12・パスワードはコードや成果物へ含めません。
複数アプリで共用する場合も、各アプリのBundle IDは固定します。同じ鍵と証明書を継続利用し、リリースごとに作り直さないでください。

`tools/macos_signing.py import` は一時キーチェーンへ秘密鍵を読み込み、固定した公開証明書との一致を確認します。
証明書の指紋を明示して署名し、ビルド用・利用者のMacのどちらもOSの信頼設定は変更しません。
CIの `sign` は鍵の書き出しを禁止してインポートし、OSのcodesignだけで署名します。
成功・失敗のどちらでもp12と一時キーチェーンを削除し、検索対象を元へ戻します。
`cleanup` も `if: always()` で実行して、秘密を置いた一時ディレクトリごと削除します。
通常のローカルビルドとPR/mainの成果物はアドホック署名のままで、配布用の秘密鍵を使いません。
ローカルで配布ビルドをする場合は、同じ証明書を読み込んだキーチェーンと `APPLE_SIGNING_IDENTITY` が必要です。

CIでは異なる2つの実行ファイルを同じ証明書で署名して識別条件の一致を試験し、アドホック署名への置き換えを拒否することも確認します。
配布zipは展開前にパス・リンク・特殊ファイル・サイズを検査します。
アプリ全体の署名と識別条件、更新manifest、公開するファイル一覧を検証してから下書きのReleaseを公開します。
dmgはOSのhdiutilで作成し、更新用tar.gzの最上位には従来と同じ `.app` を置きます。

自己署名はAppleによる開発元の確認や公証には対応しません。
Releaseの説明に次の案内を入れます。

> AppleのDeveloper ID署名・公証は未対応です。起動がブロックされた場合は、
> 「システム設定 → プライバシーとセキュリティ → このまま開く」を選んでください。

右クリックでも開けない場合は、起動を試した後に「システム設定 → プライバシーとセキュリティ →
このまま開く」を選びます。アクセシビリティを許可してもアプリが未許可と表示する場合は、
完全終了後に `tccutil reset Accessibility com.witenp.ptranslate-desk` でこのアプリの許可だけを
リセットし、インストール先のアプリを再度登録します。他のアプリの許可はリセットしません。

Developer IDを用意した段階で、証明書・パスワード・公証用の資格情報をActions secretsに置き、
ReleaseビルドのTauri署名・公証を設定します。対応後にReleaseの案内も更新します。

## CIの署名鍵の保護と移行

`release-signing` Environmentは、`v*` タグだけを許可し、リポジトリ所有者の承認を必須にします。
管理者の保護ルール迂回も無効にします。所有者1名で運用できるよう、実行者本人による承認は許可します。
承認前にタグのコミット、ワークフロー、署名用スクリプトとnpm依存の変更を確認してください。
タグの更新・削除はルールセットで禁止します。外部Actionは完全なコミットSHAに固定します。

既存の4つのSecretsはGitHubから読み戻せません。保管している**同じ鍵**を手元から登録します。
新しいワークフローをmainへ反映した後、次のコマンドを自分の端末で実行してください。
パスワードは非表示のプロンプトで入力し、コマンド引数・環境変数・会話へ貼り付けません。
`gh` の認証済みアカウントにSecretsの管理権限が必要です。

```sh
python3 tools/register_signing_secrets.py \
  --certificate /path/to/existing-certificate.p12 \
  --updater-key /path/to/existing-updater.key
```

このスクリプトは4つの値を標準入力経由でEnvironmentへ登録し、全件の成功と名前を確認してから
リポジトリSecretsの旧4件を削除します。途中で失敗した場合は再実行できます。
旧Secretsが残っている間は、他のワークフローから利用できるリスクが残ります。
移行が完了するまで新しいリリースタグをpushしないでください。
登録を確認するには、値を表示しない次の一覧を使います。

```sh
gh secret list --repo negineri/ptranslate-desk --env release-signing
gh secret list --repo negineri/ptranslate-desk
```

GitHubホストランナーや署名用CLI自体の侵害までは、この分離で防げません。
署名ジョブで追加のツール・Action・キャッシュ・アプリのコードを実行しないでください。
