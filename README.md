# ptranslate-desk

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/logo/png/horizontal-white@2x.png">
  <img src="docs/logo/png/horizontal@2x.png" width="390" alt="PTranslate Desk">
</picture>

PLaMo 3 Translate (llama.cpp の llama-server) を裏側にした、Web とデスクトップの翻訳アプリ。
DeepL のように、文章を貼って訳す画面と、選んだ文をショートカットで訳す小窓を目指す。

| 部分 | 場所 | 状態 |
| --- | --- | --- |
| サーバー (API と Web の画面を返す) | `server/translator.py`、`server/static/index.html` | 動く。本番 (`https://translate.witenp.com/`) で使っている |
| デスクトップのクライアント | `desktop/` | macOS の主窓・ショートカット・小窓・メニューバー常駐をモックで実行できる: [起動とビルド](desktop/README.md) |
| 翻訳エンジンのモック | `tools/mock_plamo.py` | 試験と手元の開発用 |

## サーバーを動かす

```bash
# 手元で (翻訳エンジンの代わりにモックを使う)
python3 tools/mock_plamo.py --port 18082 &
TR_UPSTREAM_URL=http://127.0.0.1:18082/v1/completions python3 -I server/translator.py
# 認証プロキシの代わりに X-Forwarded-User を付けて試す
curl -s -H 'X-Forwarded-User: alice' http://127.0.0.1:8002/api/config

# コンテナで (v* のタグで ghcr.io/negineri/ptranslate-desk/server に出る)
docker run --rm -p 8002:8002 -e TR_UPSTREAM_URL=https://llama.example/v1/completions \
  -e TR_API_KEY=... ghcr.io/negineri/ptranslate-desk/server:<version>
```

サーバーは認証を持たない。前段に認証プロキシ (oauth2-proxy など) を置き、ログインした利用者名を
`X-Forwarded-User` で渡すこと。待ち受けは前段からしか届かないようにする。`/api/translate` は
Server-Sent Events なので、前段で応答を溜めない (nginx なら `proxy_buffering off`)。

| 環境変数 | 既定 | 意味 |
| --- | --- | --- |
| `TR_UPSTREAM_URL` | (必須) | 翻訳エンジン (llama-server) の `/v1/completions` |
| `TR_API_KEY` | 空 | 翻訳エンジンに付ける `Authorization: Bearer` |
| `TR_LISTEN_HOST` / `TR_LISTEN_PORT` | `127.0.0.1` / `8002` (コンテナでは `0.0.0.0`) | 待ち受け |
| `TR_MAX_CHARS` | 4000 | 1 回の原文の上限 (文字) |
| `TR_MAX_PROMPT_CHARS` | 12000 | 文脈・前の訳・用語集を合わせた上限 (文字) |
| `TR_MAX_ACTIVE_PER_USER` | 2 | 1 人が同時に流せる翻訳の数 |

## 翻訳エンジン

[pfnet/plamo-3-translate-2b-GGUF](https://huggingface.co/pfnet/plamo-3-translate-2b-GGUF) などを llama-server で
動かす。1 本あたりのコンテキストは 16K を勧める (原文と訳の合計。8K では長めの段落が切れ、約 6,000 トークンを
超える原文は繰り返しに入る)。プロンプトはサーバーが PFML3 (モデルの素の書式) で組むので、`--jinja` は要らない。
モデルのライセンスは PLaMo Community License (このリポジトリのコードは MIT)。

## 開発

```bash
python3 -m unittest discover -s server/tests -v
node --test server/tests/test_translation.mjs
ruff format --check server tools   # CI と同じ版 (.github/workflows/ci.yml)
```

## ライセンス

MIT ([LICENSE](LICENSE))。
