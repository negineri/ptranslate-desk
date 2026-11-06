# PTranslate Desk の macOS 配布

## タグとバージョン

`v*` のタグをpushすると、サーバーイメージとmacOSクライアントを同じタグで配布します。
タグは `v` と `desktop/src-tauri/tauri.conf.json` の `version` をつないだ値にします。
CIは公開前に次のバージョンが一致することを確認し、不一致なら全ジョブを止めます。

- `desktop/src-tauri/tauri.conf.json`
- `desktop/src-tauri/Cargo.toml` と `Cargo.lock` のアプリの項目
- `desktop/package.json` と `package-lock.json` のアプリの項目

バージョンを更新したら、リポジトリのルートで次を確認します。

```sh
python3 tools/check_release_version.py --tag v0.1.0
```

mainへ反映後、一致するタグを作成してpushします。

```sh
git tag v0.1.0
git push origin v0.1.0
```

`main` とPRではuniversalの `.app` をzipにまとめ、Actionsの成果物として保存します。
タグでは、試験とサーバーイメージの公開が成功してから `desktop-release` がReleaseを作成します。
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
`plugins.updater.pubkey` に置き、秘密鍵とパスワードはリポジトリのActions secretsで管理します。

| secret | 内容 |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | `tauri signer generate` で作った、暗号化された秘密鍵の内容 |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | 秘密鍵のパスワード |

秘密を使うのはタグ用のReleaseビルドだけです。PRやmainの成果物ビルドには渡しません。
秘密鍵をファイル・ログ・コミット・Actionsの成果物に入れないでください。
既存の鍵を失うと、既に配ったアプリが新しい鍵の更新を受け取れなくなります。
鍵の変更は、既存の公開鍵を持つアプリからの移行手順を用意して行います。

Release用設定 `desktop/src-tauri/tauri.release.conf.json` でだけ `createUpdaterArtifacts` を有効にします。
通常の開発ビルドには秘密鍵は不要です。

## アプリでの更新

配布用アプリは起動時と24時間ごとに、GitHub Releasesの `latest.json` を確認します。
開発ビルド・モック・GUIスモーク試験は自動ではGitHubに接続しません。
主窓の「更新を確認」からも手動で確認できます。ログイン前でも操作できます。

「更新する」で内容を確認し、「インストールする」を押した後にだけダウンロードと適用を行います。
署名と署名内のバージョンを検証し、失敗した場合は適用しません。
完了後は「再起動する」を押すと新しい版で起動します。

## Apple の署名・公証

現段階ではDeveloper ID署名と公証は行いません。Releaseの説明に次の案内を入れます。

> 署名していないので、初回は右クリックから開く

Developer IDを用意した段階で、証明書・パスワード・公証用の資格情報をActions secretsに置き、
ReleaseビルドのTauri署名・公証を設定します。対応後にReleaseの案内も更新します。
