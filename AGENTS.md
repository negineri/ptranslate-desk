# AGENTS.md

このリポジトリで作業するコーディングエージェント向けの指示。人向けの説明は [README.md](README.md)。
Claude Code は [CLAUDE.md](CLAUDE.md) からこのファイルを読む。

## リポジトリの中身

- `server/translator.py`: サーバー。Python の標準ライブラリだけ (依存を足さない)。API は
  [docs/SPEC.md](docs/SPEC.md) の「5. サーバーの API」が正。変えるときは SPEC も同じ PR で直す
- `server/static/index.html`: Web の画面 (HTML 1 枚、ビルドなし)。区切りと自動で翻訳の規則は SPEC の 4 章
- `tools/mock_plamo.py`: 翻訳エンジンのモック。試験はこれを相手にする。本物の翻訳エンジンには試験でつながない
- `docs/SPEC.md`: デスクトップのクライアントの仕様
- 本番のデプロイの設定 (nginx、oauth2-proxy、Authentik、VM) は別の非公開のリポジトリにある。ここには置かない

## 変更を出す前に

- `ruff format --check server tools` (版は `.github/workflows/ci.yml` と同じ) と
  `python3 -m unittest discover -s server/tests -v` を通す
- コンテナを変えたら `docker build .` が通ること
- 公開リポジトリなので、秘密 (鍵、トークン)、内部のホスト名、本番の利用者名をコード・試験・コミットに入れない
- 文章は日本語。画面の言葉は今の `index.html` に合わせる

## 次にやること (順に)

1. **画面の共通化の準備:** `index.html` の区切り (`splitText`)、訳す先の自動 (`guessTarget`)、用語集の読み取り
   (`parseGlossary`)、SSE の読み取り (`translateChunk`) を、Web とデスクトップで同じものを使えるよう別の
   モジュール (`shared/` など) に分ける。Web はビルドなしで読めること。区切りの試験 (同じ原文で同じ区切りに
   なる、SPEC 10 章の 4) を足す
2. **デスクトップのクライアント:** `desktop/` に Tauri 2 で作る。SPEC の 3〜8 章を満たす。最初は主窓と
   認証 (SPEC 6 章) から。試験はモックを相手にする (SPEC 11 章)。認証の試験もローカルの OIDC モックで行う。
   本番の確認は利用者に明示された範囲で行う
3. **CI:** デスクトップのビルド (Windows・macOS・Linux) と、`v*` のタグでの GitHub Releases への配布を
   `.github/workflows/` に足す。署名は SPEC 9 章が決まってから
4. **リリース:** サーバーのイメージは `v*` のタグで `ghcr.io/negineri/ptranslate-desk/server` に出る
   (`ci.yml`)。本番はそのイメージをバージョンと digest で固定して使う

## サーバー側の準備 (非公開のリポジトリで実施済み)

デスクトップから Bearer の JWT で API を呼べるようにするため、次の準備は済んでいる。
接続先と認証の規則は SPEC の 6 章を参照。scope に `email` を入れない。

- Authentik に公開クライアント `translator-desktop` (PKCE、ループバックのリダイレクト、refresh token)
- 認証プロキシ (oauth2-proxy) で Bearer の JWT を通す
- Bearer で通ったときにサーバーへ利用者名 (`X-Forwarded-User`) が渡ることの確認
