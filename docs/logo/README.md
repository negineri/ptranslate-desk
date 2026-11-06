# PTranslate Desk ロゴ

[ptranslate-desk](https://github.com/negineri/ptranslate-desk) のアプリアイコン / ロゴ一式。
**「訳」と書かれた白い吹き出し** を、Web 画面のアクセントカラー (`#2f6fde`) に寄せた
青のグラデーションタイルに置いたデザインです。

## アプリでの利用

`ptranslate-logo.zip` から展開した原素材を、この `docs/logo/` に保存しています。

- macOSは `png/icon.icns` を `desktop/src-tauri/icons/icon.icns` にコピー (余白付きのmacOS用マーク)
- Windowsは `png/icon-win.ico` を `desktop/src-tauri/icons/icon.ico` にコピー (角丸なしのスクエア)
- `png/icon-ios-1024.png` をマスターに Tauri CLI で `desktop/src-tauri/icons/` の各サイズのPNGを生成
- `svg/ptranslate-mark-mono.svg` を元に、周囲の余白を狭めた `desktop/src-tauri/icons/tray/mark.svg` を作成し、
  同じ CLI で 22 / 44 px を生成 (小さいメニューバーでもマークを読み取りやすくするため)
- `png/icon-64.png` を `server/static/logo.png` にコピーして Web・主窓・小窓で表示
- `png/favicon.ico` を `server/static/favicon.ico` にコピー
- 横型ロゴの PNG はリポジトリの README で使用 (ライト・ダークの配色に追従)

メニューバーは macOS のテンプレートアイコンとして表示し、未ログイン時は薄くします。
Web のロゴとファビコンは、デスクトップのビルド時に `desktop/dist/` にもコピーされます。

## 原素材の構成

各形式と書き出し用のスクリプトをまとめています。

```
docs/logo/
├── svg/     ... 原画 (ベクター)
├── png/     ... 書き出し済み PNG・favicon
├── preview/ ... ブランドシート・GitHub Social preview
└── tools/   ... 再生成スクリプト
```

## ファイル一覧

### svg/ (原画)

| ファイル | 用途 |
| --- | --- |
| `ptranslate-icon.svg` | アプリアイコン (角丸タイル + 訳バブル) |
| `ptranslate-icon-fullbleed.svg` | iOS・汎用マスター (全面を使う正方形) |
| `ptranslate-icon-mac.svg` | macOS用の曲線と余白を持つマーク |
| `ptranslate-horizontal.svg` | 横型ロゴ (淡色背景用) |
| `ptranslate-horizontal-white.svg` | 横型ロゴ (濃色背景用) |
| `ptranslate-mark-mono.svg` | モノクロマーク・1 色ぬき (青) |
| `ptranslate-mark-mono-white.svg` | 同上・白 (濃色背景用) |

### png/ (書き出し済み)

| ファイル | 用途 |
| --- | --- |
| `icon-1024.png` / `-512` / `-256` / `-128` / `-64` / `-48` / `-32` / `-16` | 各サイズ |
| `icon-192.png` / `icon-180.png` | Android 用 / apple-touch-icon |
| `favicon.ico` | ファビコン (16 / 24 / 32 / 48) |
| `horizontal.png` / `horizontal@2x.png` | 横型ロゴ (淡色背景用) |
| `horizontal-white@2x.png` | 横型ロゴ (濃色背景用) |
| `mark-mono.png` | モノクロマーク |
| `icon-ios-1024.png` | iOS・汎用マスター (角丸なし) |
| `icon-mac-1024.png` / `icon.icns` | macOS用 (余白付き) |
| `icon-win.ico` | Windows用 (16〜256px) |

### preview/

| ファイル | 内容 |
| --- | --- |
| `final-sheet.png` | ブランドシート (全バリエーション) |
| `github-social.png` | GitHub Social preview (1280×640) |
| `sheet-platform.png` | Web・iOS・macOS・Windows用の比較 |

## カラー

| 名前 | 値 |
| --- | --- |
| タイル グラデーション | `#57A1FF` → `#1F54D0` (135°) |
| 「訳」グラデーション | `#3D7BE8` → `#2050C0` |
| 見出し文字 | `#1D2330` |
| 補助文字 | `#5D6576` |
| モノクロ 1 色 | `#1D4FC9` |

## フォント

- 欧文 (PTranslate Desk): Noto Sans CJK JP (太字 + 標準)。なければ DejaVu Sans にフォールバック
- 「訳」: Noto Sans CJK JP Bold。モノクロ版はパス化済みでフォント不要

## 再生成

モノクロ版は `tools/gen_mono.py` で各輪郭の包含深度に応じて巻き方向を決め、
「訳」の筆画と文字の内側の抜きを透明部分として生成します。
スクリプトの実行には Noto Sans CJK JP Bold が必要です (`FONT` のパスを参照)。

```bash
cd docs/logo
pip install --user --break-system-packages cairosvg fonttools pillow
python3 tools/gen_mono.py     # モノクロ SVG を再生成
python3 tools/gen_platform.py # OS別のSVG・PNG・ICNS・ICOを再生成
python3 tools/export_all.py   # 全 PNG・シートを再生成
```

## ライセンス

ロゴもリポジトリ本体と同じ MIT ライセンスで利用可。
