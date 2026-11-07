# Windowsローカル翻訳の計測（2026-11-07・08）

tools/bench.pyをtools/run_local_bench.pyから実行した結果。RTX 4090は11月7日、CPUと内蔵GPUは11月8日に計測。

- Windows、Ryzen 7 9800X3D、RTX 4090（Vulkan0）、AMD Radeon(TM) Graphics（9800X3D内蔵、Vulkan1）
- ドライバー: NVIDIA 32.0.16.2101、AMD 32.0.11024.2。ドライバーやSDKの変更は行っていない
- llama.cpp b11804、PLaMo 3 Translate 2B Q4_K_M（試作版と同じ取得済みモデル）
- Flash Attention指定はon、総コンテキスト32768。GPUレイヤー99、CPUはdevice=none・GPUレイヤー0
- CPUスレッド数はllama.cppの既定（固定指定なし）
- RTX 4090とCPUはnp=1 / 4、同時要求数1 / 2 / 4の18条件
- 内蔵GPUは低速のため、利用者の指示で全条件の計測を中断し、np=1・同時要求数1の3条件に短縮した
- 各条件3回、bench.pyが選ぶ完了時間の中央値の回を掲載。速度の値もその回のもの
- 原文はja-short=140文字、ja-long=1497文字、en-long=1409文字
- cache_prompt=false、temperature=0、stream=false。モデル読み込み時間は含めない
- 同時要求数とnpは別。np=1では複数要求が待ち行列になる
- 総コンテキストを固定しているため、np変更でスロットあたりの上限も変わる
- 試作アプリを起動したまま、別の推論プロセスで順番に計測。専用の無負荷環境ではない

## 単独要求の比較（np=1）

| デバイス | 日→英140文字（秒） | 日→英1497文字（秒） | 英→日1409文字（秒） |
| --- | ---: | ---: | ---: |
| RTX 4090（Vulkan0） | 0.2 | 2.2 | 0.7 |
| Ryzen 7 9800X3DのCPU | 2.2 | 21.6 | 7.2 |
| AMD Radeon Graphics（9800X3D内蔵、Vulkan1） | 10.6 | 103.3 | 35.1 |

## 全条件の結果

### RTX 4090（Vulkan0）

[生の結果（JSON Lines）](benchmarks/2026-11-07-rtx4090-fa-on.jsonl)

| 原文 | np | 同時要求数 | 全要求の完了（秒） | 入力処理（token/s・1本平均） | 生成（token/s・1本平均） | 生成（token/s・合計） | 終了理由 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| ja-short | 1 | 1 | 0.2 | 3273.8 | 346.3 | 290.3 | stop |
| ja-short | 1 | 2 | 0.4 | 3235.6 | 343.9 | 311.6 | stop |
| ja-short | 1 | 4 | 0.9 | 3242.5 | 342.8 | 311.3 | stop |
| ja-long | 1 | 1 | 2.2 | 12001.2 | 334.2 | 316.2 | stop |
| ja-long | 1 | 2 | 4.2 | 11638.8 | 332.0 | 323.8 | stop |
| ja-long | 1 | 4 | 8.4 | 11520.3 | 332.7 | 324.5 | stop |
| en-long | 1 | 1 | 0.7 | 9414.7 | 338.3 | 315.0 | stop |
| en-long | 1 | 2 | 1.4 | 9434.9 | 342.8 | 331.7 | stop |
| en-long | 1 | 4 | 2.8 | 10398.7 | 344.6 | 334.8 | stop |
| ja-short | 4 | 1 | 0.2 | 3157.1 | 335.3 | 287.5 | stop |
| ja-short | 4 | 2 | 0.9 | 1739.6 | 195.6 | 146.8 | stop |
| ja-short | 4 | 4 | 0.3 | 2521.5 | 220.2 | 780.0 | stop |
| ja-long | 4 | 1 | 2.1 | 12355.1 | 329.5 | 317.5 | stop |
| ja-long | 4 | 2 | 2.4 | 7741.0 | 298.6 | 563.8 | stop |
| ja-long | 4 | 4 | 3.3 | 4170.3 | 214.9 | 816.8 | stop |
| en-long | 4 | 1 | 0.8 | 9050.8 | 321.1 | 300.5 | stop |
| en-long | 4 | 2 | 0.9 | 5730.9 | 286.9 | 537.3 | stop |
| en-long | 4 | 4 | 1.1 | 3522.3 | 229.7 | 858.0 | stop |

### Ryzen 7 9800X3DのCPU

[生の結果（JSON Lines）](benchmarks/2026-11-08-cpu-fa-on.jsonl)

| 原文 | np | 同時要求数 | 全要求の完了（秒） | 入力処理（token/s・1本平均） | 生成（token/s・1本平均） | 生成（token/s・合計） | 終了理由 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| ja-short | 1 | 1 | 2.2 | 385.7 | 32.5 | 29.8 | stop |
| ja-short | 1 | 2 | 4.0 | 473.3 | 35.7 | 33.3 | stop |
| ja-short | 1 | 4 | 8.0 | 472.6 | 35.7 | 33.5 | stop |
| ja-long | 1 | 1 | 21.6 | 522.6 | 32.8 | 31.1 | stop |
| ja-long | 1 | 2 | 43.4 | 559.3 | 32.6 | 31.0 | stop |
| ja-long | 1 | 4 | 86.7 | 579.6 | 32.6 | 31.0 | stop |
| en-long | 1 | 1 | 7.2 | 557.0 | 34.5 | 32.7 | stop |
| en-long | 1 | 2 | 14.2 | 557.2 | 35.0 | 33.1 | stop |
| en-long | 1 | 4 | 28.6 | 559.6 | 34.7 | 32.8 | stop |
| ja-short | 4 | 1 | 2.0 | 377.3 | 35.8 | 32.9 | stop |
| ja-short | 4 | 2 | 2.3 | 302.0 | 32.5 | 57.2 | stop |
| ja-short | 4 | 4 | 2.6 | 150.6 | 30.8 | 102.1 | stop |
| ja-long | 4 | 1 | 21.4 | 582.8 | 33.0 | 31.4 | stop |
| ja-long | 4 | 2 | 26.1 | 299.1 | 28.0 | 51.6 | stop |
| ja-long | 4 | 4 | 31.8 | 151.4 | 24.1 | 84.1 | stop |
| en-long | 4 | 1 | 7.1 | 564.8 | 35.0 | 33.0 | stop |
| en-long | 4 | 2 | 8.4 | 386.4 | 31.1 | 56.3 | stop |
| en-long | 4 | 4 | 9.7 | 154.1 | 28.9 | 97.8 | stop |

### AMD Radeon Graphics（9800X3D内蔵、Vulkan1）

[生の結果（JSON Lines）](benchmarks/2026-11-08-igpu-fa-on-single.jsonl)

| 原文 | np | 同時要求数 | 全要求の完了（秒） | 入力処理（token/s・1本平均） | 生成（token/s・1本平均） | 生成（token/s・合計） | 終了理由 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| ja-short | 1 | 1 | 10.6 | 50.9 | 7.2 | 6.3 | stop |
| ja-long | 1 | 1 | 103.3 | 92.8 | 7.0 | 6.5 | stop |
| en-long | 1 | 1 | 35.1 | 85.5 | 7.2 | 6.6 | stop |

## 観察

掲載した条件はすべてfinish=stopで、上限打ち切りはなかった。翻訳品質や訳の同一性は評価していない。

単独要求の完了時間は、RTX 4090・CPUともnp=1とnp=4でほぼ同じだった。4本同時の日→英長文では、RTX 4090が8.4秒から3.3秒、CPUが86.7秒から31.8秒に短縮した。

同じ9800X3DのCPUと内蔵GPUを比較すると、CPUの単独要求の完了時間は内蔵GPUの約1/5だった。

RTX 4090の短文・np=4・2本条件は0.9秒で、4本条件の0.3秒より長かった。完了時間は0.1秒単位に丸められており、実行時の揺れの内訳はこの集計では分からない。

この計測は最初の文字までの時間や、前の訳を文脈として渡したときの区切り間の停止を直接測らない。

## 再実行

```powershell
python tools/run_local_bench.py --device Vulkan0 --output desktop/src-tauri/target/bench-fa-on-vulkan0.jsonl
python tools/run_local_bench.py --device none --output desktop/src-tauri/target/bench-fa-on-cpu.jsonl
python tools/run_local_bench.py --device Vulkan1 --slots 1 --parallel 1 --output desktop/src-tauri/target/bench-fa-on-vulkan1-single.jsonl
```
