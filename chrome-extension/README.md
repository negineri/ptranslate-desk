# PTranslate for Chrome (Chrome 拡張)

ブラウザで読んでいるページの文を、PTranslate のサーバーで訳す Chrome 拡張です。
Chrome 120 以降と、Edge などの Chromium 系のブラウザで動きます。

- ページで文を選んで右クリック →「PTranslate で翻訳」、またはショートカット (既定は Alt+Shift+T) で、
  選んだ文の近くに訳を出します。PDF のビューアーなど、ページに枠を出せないところでは小さいウィンドウで開きます
- ページ全体を訳せます。右クリック →「PTranslate でこのページを翻訳」、ショートカット (既定は Alt+Shift+P)、
  またはツールバーの画面の「このページを翻訳」から使います (下の「ページ全体の翻訳」)
- ツールバーのボタンから、文を入れて訳す小さい画面を開けます
- 区切り (`splitText`)、訳す先の自動 (`guessTarget`)、用語集 (`parseGlossary`)、SSE の読み取り (`translateChunk`) は
  Web と同じ `server/static/translation.mjs` を使います

## 使い方

1. 拡張を読み込みます (下の「開発」)。ストアでの配布はまだしていません
2. 拡張の設定でサーバーURL (例: `https://translate.example.com`、パスなし) を入れ、「保存して接続を確かめる」を押します。
   そのサーバーへの接続の許可を求められるので、許可します
3. 同じブラウザで Web版 (サーバーURLのページ) にログインしておきます。「ログインしていません」と出たら、
   「Web版を開いてログイン」からログインし直します

文体・原文の言語・訳す先・用語集は拡張の設定で決めます。ショートカットは `chrome://extensions/shortcuts` で変えられます。

## ページ全体の翻訳

- 段落 (ブロック要素) ごとに文を集め、訳を元の位置に書き込みます。右上のバーに進み具合が出て、
  「中止」「原文に戻す」ができます。訳し終えた後にもう一度呼ぶとバーを出し直し、原文に戻した後なら訳し直します
- 画面に見えている段落から順に、1 段落ずつ訳します。サーバーの同時の上限 (1 人 2 本) を空けておくため、同時には 1 本だけ送ります。
  前の段落の訳を手がかりに送るのは Web版と同じです
- 訳す先の自動 (日本語 ⇄ 英語) はページ全体の文で決めます。訳す先の言語ですでに書かれた段落
  (日本語へ訳すときの日本語の段落、英語へ訳すときの ASCII だけの段落) と、数字や記号だけの段落は送りません
- コード (`code`・`pre`)、入力欄、編集できる欄、`translate="no"`・`class="notranslate"` の要素、見えていない要素は訳しません
- 段落の中のリンクや強調の区切りは保てないため、訳は段落の最初の文字にまとめて入れます (リンクの要素は残ります)。
  訳した後にページが書き換えた部分は訳しません
- 処理は `page-content.js` (ページに注入する content script) と `page-task.mjs` (background) です。
  content script は `page-translate` の port で段落の一覧を background に送り、訳は background が API を呼んで返します。
  この port はタブの一番上の枠の、この拡張の content script からだけ受けます

## 認証と通信

拡張は自分ではログインせず、Web版のログイン (認証プロキシの Cookie) を使います。
そのため IdP (Authentik・Pocket ID) とサーバーの設定を変える必要はありません。

- API は background (service worker) からだけ呼びます。拡張に許可されたサーバーへの要求には Cookie が付き、
  ページの CORS の制限も受けません。訳の画面 (`popup.html`) はページの中の枠 (iframe) で動くため、
  直接は呼ばずに `chrome.runtime` の port で background に頼みます (`transport.mjs` → `relay.mjs`)
- 呼べるのは `GET /api/config` と `POST /api/translate` だけです (デスクトップの `start_request` と同じ)。
  port は拡張自身の画面からしか受けません
- 認証プロキシのログイン画面への転送は追わず (`redirect: "manual"`)、401・403 と同じくログインしていないとして扱います
- 中止は port を閉じて伝えます。background が接続を閉じ、サーバーが翻訳エンジンを止めます (SPEC 5.4)。
  120 秒なにも届かなければ時間切れにします (SPEC 5.2)
- サーバーURLは HTTPS だけです。開発用に `http://127.0.0.1`・`http://localhost` は通します

## 保存するもの

`chrome.storage.local` にサーバーURL・言語・文体・用語集だけを保存します。ページ全体の翻訳の原文と訳は、そのページの中だけに置きます。
選んだ文は訳の画面へ渡すあいだだけ `chrome.storage.session` (メモリ上、ブラウザを閉じると消える) に置き、
訳の画面が読んだらすぐ消します。原文と訳はサーバー以外には送りません。

## 開発

拡張は `node chrome-extension/build.mjs` で `chrome-extension/dist/` に組み立てます
(Web の `translation.mjs`・見た目・ロゴを写します)。`chrome://extensions` でデベロッパー モードを有効にし、
「パッケージ化されていない拡張機能を読み込む」で `chrome-extension/dist` を選びます。
ソースを変えたら組み立て直し、拡張を再読み込みします。

モックの翻訳エンジンで試すときは、リポジトリのルートで次を実行します。

```sh
python3 tools/run_extension_mock.py
```

モック・翻訳サーバー (127.0.0.1:8002)・認証プロキシの代わり (127.0.0.1:8003) を起動します。
認証プロキシの代わりは開発用の利用者名を `X-Forwarded-User` に付けて中継するだけなので、開発の端末でだけ使ってください。
拡張の設定のサーバーURLには `http://127.0.0.1:8003` を入れます。
`http://127.0.0.1:8002` を入れると、ログインしていないときの表示を試せます。

試験はリポジトリのルートで実行します。

```sh
node chrome-extension/build.mjs
node --test chrome-extension/tests/
```
