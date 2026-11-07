# ローカル翻訳 (Windows x64)

翻訳サーバーなしで、この端末のPLaMo 3 Translate 2Bを使えます。
デスクトップアプリがllama.cppの`llama-server`を子プロセスとして管理します。
サービスの登録、CUDA/Vulkan SDKの導入は行いません。通常のサーバーモードも選べます。

## 初回の設定

初回起動時に「このPCで翻訳」と「サーバーで翻訳」を選びます。
既に接続先やローカルモードを設定済みの場合は、その設定を引き継ぎます。

1. 「このPCで翻訳」を選ぶ。
2. モデルがない場合は「モデルをダウンロード」を押し、容量と取得元を確認して「ダウンロードする」を押す。
   モデル約1.81 GBと推論エンジン約37 MBを取得する。空き容量の目安は約2 GB。
3. ダウンロード中はファイルごとの取得量と割合を表示する。中止でき、失敗時はやり直せる。
4. 取得後は短い文章でGPUとCPUを順番に計測し、速い候補を「おすすめ」として表示する。
   モデル読み込み時間を除く短文翻訳の時間を比べる。各デバイスは最大45秒。
5. 候補からデバイスを選び、「保存して再起動」を押す。ログインや接続先の設定は不要。

「手動で選ぶ」からGPUまたはCPUを指定することもできます。計測を中止しても手動選択はできます。
計測結果だけで設定は保存しません。おすすめは短文での目安で、長文やほかのアプリの負荷によって速度は変わります。

「サーバーで翻訳」を選んだ場合は接続先設定を開き、通常のログインへ進みます。
設定タブの「翻訳する場所」から、あとでモードを切り替えられます。

## モデルとメモリ

「使っていないときにモデルのメモリを自動解放する」は既定で無効です。
無効の場合は、ローカルモードでの起動直後にバックグラウンドでモデルを読み込み、そのまま保持します。
読み込み中の状態はメインウィンドウにも表示します。

自動解放を有効にすると、翻訳するときに読み込み、指定した未使用時間が過ぎると解放します。
既定は5分で、1〜60分の範囲で設定できます。解放の確認は5秒間隔です。
翻訳中や計測中は自動解放しません。中止した場合も、その要求の終了時点から未使用時間を数えます。

「モデルを読み込む」「モデルを解放」でも手動操作できます。アプリの終了時も解放します。
WindowsではJob Objectを使い、アプリが強制終了した場合にも推論プロセスを終了させます。

## 取得するファイル

llama.cpp b11804のWindows Vulkan版と、PLaMo 3 Translate 2BのQ4_K_M版を、固定URLから取得してSHA-256を検証します。
保存先は`%LOCALAPPDATA%/com.witenp.ptranslate-desk/inference/`です。
検証が完了するまで新しいファイルを有効にせず、取得や展開の途中で失敗しても既存ファイルは維持します。
やり直す場合は、取得・検証済みのファイルを再利用します。中止した取得途中のファイルは破棄します。
別のアプリから同時に取得しないよう、保存先をロックします。

開発用には、Python 3.11以降でリポジトリのルートから準備する方法も残しています。

Python 3.11以降でリポジトリのルートから実行します。

```powershell
python tools/prepare_local_engine.py
```

llama.cpp b11804のWindows Vulkan版 (zip約37MB) と、2BモデルのQ4_K_M版 (約1.81GB) を
固定URLから取得し、SHA-256を検証します。保存先は
`%LOCALAPPDATA%/com.witenp.ptranslate-desk/inference/`です。

## 開発用のビルド

既存の配布版とは別の設定を使う試作版を作ります。必要な開発環境は[desktop/README.md](../desktop/README.md)を参照してください。

```powershell
cd desktop
npm ci
npm run tauri -- build --debug --config src-tauri/tauri.local-prototype.conf.json -- --locked
```

インストーラーは`desktop/src-tauri/target/debug/bundle/nsis/`に生成します。
試作版のアプリ名は「PTranslate Desk Local Prototype」です。既存版と設定を分離し、自動更新の配布先も設定しません。
同時起動すると共通のショートカットが競合するため、普段使う版を終了してから試してください。

GPUの起動に失敗する場合は設定でCPUを選んでください。自動のCPU切り替えはまだ行いません。

## 構成

- ローカルモードではOIDC認証・翻訳サーバーを使わず、RustでPFML3プロンプトを組み立てる。
- サーバーモードのAPIとPythonでのプロンプト生成は維持する。
- 共通の[入力・期待値](../shared/local-translation-fixtures.json)をPythonとRustのテストから読み、一致を確認する。
- 入力の上限、用語集、文体、過去の訳、ストリーミング表示は既存の画面と同じ規則を使う。
- 推論プロセスは127.0.0.1の一時ポートで待ち受け、起動ごとに生成する認証キーを使う。
  キーはコマンドラインに出さず、認証なしの要求が拒否されることを起動時に確認する。
- 原文・訳文・プロンプトをファイルや子プロセスのログに保存しない。
- Vulkan版でNVIDIAとAMDの両方を扱う。CPUではGPUレイヤーを0にする。
- コンテキストは32768トークン、同時推論は1件。Flash Attentionは有効にする。

## 確認

通常のテストはモックを使います。

```powershell
python tools/run_desktop_mock.py --smoke-test
cargo test --locked --manifest-path desktop/src-tauri/Cargo.toml
```

実モデルの試験は明示的な手動操作としてのみ行います。取得済みモデルが必要です。
一時ディレクトリに設定を作り、既存の接続先や設定を変更せずに、WebViewから翻訳・中止・モデル解放を確認します。
原文と訳文は出力せず、時間と文字数だけを出力します。

```powershell
node desktop/build.mjs
cargo build --locked --features smoke-test --manifest-path desktop/src-tauri/Cargo.toml
python tools/run_local_probe.py --device Vulkan0
python tools/run_local_probe.py --device Vulkan1
python tools/run_local_probe.py --device none
python tools/run_local_probe.py --device Vulkan0 --measure
python tools/run_local_probe.py --device Vulkan0 --auto-release
```

`bench.py`の全原文（日→英の短文・長文、英→日の長文）を同時要求数1・2・4本で測るには、
次を実行します。アプリとは別の認証付き推論プロセスを一時的に起動し、終了時に停止します。
Flash Attention有効、コンテキスト32768、`np=1 / 4`、各条件3回で、`bench.py`の中央値を保存します。

```powershell
python tools/run_local_bench.py --device Vulkan0 --output desktop/src-tauri/target/bench-fa-on-vulkan0.jsonl
```

`--device Vulkan1`で内蔵GPU、`--device none`でCPUを選べます。
`--slots 1`でスロット数を固定できます。同時要求数とスロット数は別の条件です。
`--parallel 1`で単独要求に絞れます。内蔵GPUの計測は次の3条件に短縮しました。

```powershell
python tools/run_local_bench.py --device Vulkan1 --slots 1 --parallel 1 --output desktop/src-tauri/target/bench-fa-on-vulkan1-single.jsonl
```

`np=1`に4本を送る場合は待ち行列になり、4本が終わるまでの時間を測ります。
非ストリーミングの計測なので、最初の文字の表示時間や画面上の区切り間の待ち時間は直接測りません。
RTX 4090・CPU・内蔵GPUの結果は[計測記録](LOCAL_BENCHMARK.md)に掲載しています。

このPCではRTX 4090とAMD Radeon GraphicsをVulkanデバイスとして認識し、両GPUとCPUで日本語への翻訳を確認しました。
2026-11-07、Ryzen 7 9800X3D、32768トークン、Flash Attention無効の設定で、同じ短い英文を日本語42文字に訳した際の実測です。
翻訳時間はモデル読み込み後、WebViewから要求して完了するまでを測っています。

| 使用デバイス | モデル読み込み | 翻訳1回目 | 翻訳2回目 |
| --- | ---: | ---: | ---: |
| RTX 4090 (Vulkan0) | 2.56秒 | 0.55秒 | 1.04秒 |
| AMD Radeon Graphics (Vulkan1) | 3.41秒 | 2.82秒 | 3.01秒 |
| CPU | 2.37秒 | 0.48秒 | 0.44秒 |

各デバイスで画面からの翻訳、要求の中止、モデルの解放も確認しました。
その後、RTX 4090でFlash Attentionを有効にし、同じ短文で最初の応答まで15〜23ms、
翻訳完了まで53〜107msを確認しました。区切り間の待ち時間や長文については別途測定が必要です。
短文ではGPUの処理準備の影響も大きいため、モデルや文章の長さを変えた比較は今後の課題です。

## 今後の拡張

- モデルの削除とバージョン更新を画面から行えるようにする。
- 長文、用語集の多い入力、GPUメモリ不足時の挙動と翻訳品質を評価する。
- GPUが使えない端末での案内とCPUへの切り替えを改善する。
- macOS向けのMetal版など、他OSの導入経路を用意する。ローカルモードはWindows x64向け。
