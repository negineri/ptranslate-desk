#!/usr/bin/env python3
"""翻訳エンジン (llama-server) の速さを測る。

PTranslate のサーバーと同じプロンプト (PFML3) と同じ引数で `/v1/completions` を呼び、
llama-server が返す `timings` から、原文を読む速さ (prompt) と訳を書く速さ (generation) を出す。
`--parallel` で同じ原文を同時に流し、1 本ごとと合計の速さを見る。原文はこのファイルに入れた
架空の文章で、Web の画面が 1 回に送る目安 (約 1,500 文字) に合わせ、同じ文を繰り返さない。

    python3 tools/bench.py --url http://127.0.0.1:8080/v1/completions [--parallel 1,2,4]
        [--text ja-long] [--repeat 3]   # 鍵が要るエンジンは TR_API_KEY に入れる

本物の翻訳エンジンにつなぐので、自動試験 (server/tests) では使わない。
"""

from __future__ import annotations

import argparse
import json
import os
import statistics
import sys
import threading
import time
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "server"))
import translator  # noqa: E402

DESCRIPTION = "翻訳エンジン (llama-server) の速さを測る。"

JA_PARAGRAPHS = [
    "自宅のサーバーを長く動かしていると、最初に決めた構成が少しずつ合わなくなってくる。"
    "はじめは一台の小さな機械に、ファイルの共有と写真の保管と家計簿を詰め込んでいた。"
    "使う人が家族に広がると、夜中の更新で誰かの作業が止まるようになり、"
    "止めてよい時間を事前に知らせる仕組みが必要になった。",
    "そこで、サービスごとに仮想マシンを分け、設定はすべて手順書ではなく構成管理の道具で書くことにした。"
    "手で直した箇所は次の適用で元に戻るので、直したいことがあれば必ず設定の側を変える。"
    "この決まりを守るだけで、壊れたときにどこまで戻せばよいかが分かりやすくなった。",
    "バックアップは、取ることよりも戻せることを確かめるほうが難しい。"
    "月に一度、空の仮想マシンを作ってバックアップから戻し、"
    "画面が開いて最近の写真が見えるところまでを確かめている。"
    "この確認で、暗号化の鍵を別の場所に置き忘れていたことに気付いたこともある。",
    "監視は、通知が多すぎると誰も見なくなる。"
    "すぐに手を動かす必要があるものだけを通知にし、それ以外は朝に一覧で読む形に分けた。"
    "通知が鳴ったときに何をすればよいかを、通知の本文に短く書いておくことも効果があった。",
    "外から使うサービスは、認証の仕組みを一つにまとめた。"
    "家族はそれぞれ一つのアカウントでログインし、使えるサービスはグループで決める。"
    "パスワードの代わりにパスキーを使うようにしてからは、"
    "パスワードを忘れたという相談がほとんど無くなった。",
    "電気代も無視できない。"
    "常に動かしておく機械は消費電力の小さいものにし、"
    "重い処理をする機械は必要なときだけ起動するようにした。"
    "翻訳や文字起こしのような処理は、待てるものはまとめて夜に流している。",
    "最後に、全体の構成を一枚の図にして、家族が見られる場所に置いた。"
    "どの機械が止まると何が使えなくなるのかが分かるだけで、"
    "障害のときに聞かれる質問がかなり減った。"
    "仕組みを作るよりも、仕組みを説明することのほうが大事だと感じている。",
    "ディスクは突然壊れるものだと考えて、同じデータを二台に分けて置いている。"
    "片方が壊れたときに慌てないよう、交換の手順と予備のディスクの置き場所を紙に書いて棚に貼った。"
    "去年の夏に実際に一台が壊れたが、手順どおりに替えるだけで半日のうちに元に戻った。",
    "家のネットワークは、仕事用の機械と家電と来客用の機器とで分けた。"
    "古いテレビやプリンターは更新が止まっていることが多く、同じ場所に置くと怖い。"
    "分けたことで、来客に回線を貸すときもパスワードを気にせず渡せるようになった。",
    "使わなくなったサービスは、思い切って止めるようにしている。"
    "動いているだけで更新の手間がかかり、知らないうちに古い部品が残り続ける。"
    "止める前に一か月ほど通知だけを出し、誰からも声が掛からなければ消す。",
    "子どもが使う機械には、時間を区切る仕組みを入れた。"
    "はじめは一律に止めていたが、宿題のための調べものまで止まってしまい評判が悪かった。"
    "今は、遊びのためのサービスだけを夜に止め、学校の連絡や辞書はいつでも使えるようにしている。",
    "新しい道具を試すときは、本番とは別の仮想マシンを作ってそこで動かす。"
    "うまく動くことを確かめたら、手順を構成管理の側に書き写してから本番に入れる。"
    "試しに入れたものがそのまま本番に残り、誰も中身を知らないという状態を避けたい。",
    "外出先から家の機械を触るときは、仮想的な専用回線を通すようにした。"
    "家の外に開けている入口は無く、回線に入れる端末も家族の分だけに絞っている。"
    "おかげで、旅行先で写真を整理するときにも不安を感じなくなった。",
]
# 英語の原文は JA_PARAGRAPHS とは別の内容 (訳文と同じだと、訳す速さが測れない)
EN_PARAGRAPHS = [
    "Running a server at home for a long time slowly reveals the limits of the first design. "
    "At the beginning, a single small machine handled file sharing, photo storage, and the household budget. "
    "Once the whole family started using it, a midnight update could interrupt someone's work, "
    "so we needed a way to announce maintenance windows in advance.",
    "We split each service into its own virtual machine and described every setting with a "
    "configuration management tool instead of a written checklist. "
    "Any manual fix is reverted by the next run, so changes always go into the configuration. "
    "This single rule made it much clearer how far to roll back when something breaks.",
    "With backups, proving that you can restore is harder than taking them. "
    "Once a month we create an empty virtual machine, restore it from the backup, "
    "and check that the page opens and the latest photos are visible. "
    "This check once revealed that we had forgotten to store the encryption key somewhere else.",
    "Alerts are useless when there are too many of them, because nobody reads them anymore. "
    "We kept notifications only for problems that need someone to act right away "
    "and moved everything else into a short report that we read over breakfast.",
    "The household now shares a single login for every service, and groups decide who can use what. "
    "After we switched from passwords to passkeys, "
    "requests to reset a forgotten password almost disappeared.",
    "Electricity costs matter as well. "
    "The machines that run all day are small and quiet, "
    "while the powerful one only starts when a heavy job such as transcription is waiting in the queue.",
    "Old televisions and printers rarely receive updates, so they live on a separate network. "
    "Guests get their own network too, which means we can share the password without worrying.",
    "Finally, we drew the whole setup on a single page and put it where everyone can see it. "
    "Knowing which machine affects which service reduced the number of questions during outages "
    "more than any technical change we made.",
]


def first_paragraphs(paragraphs: list[str], limit: int) -> str:
    """段落を順に足し、limit 文字を超えない所で止める (原文に同じ文を繰り返さない)。"""
    text = paragraphs[0]
    for paragraph in paragraphs[1:]:
        if len(text) + 2 + len(paragraph) > limit:
            break
        text += "\n\n" + paragraph
    return text


TEXTS = {
    "ja-short": JA_PARAGRAPHS[0],
    "ja-long": first_paragraphs(JA_PARAGRAPHS, 1500),
    "en-long": first_paragraphs(EN_PARAGRAPHS, 1500),
}
TARGETS = {"ja-short": "en", "ja-long": "en", "en-long": "ja"}


def run_one(url: str, api_key: str, text: str, target: str, results: list, index: int) -> None:
    req = {
        "text": text,
        "target": target,
        "source": None,
        "form": None,
        "context": "",
        "glossary": [],
        "history": [],
    }
    body = json.dumps(
        {
            "model": translator.MODEL,
            "prompt": translator.build_prompt(req),
            "max_tokens": translator.max_tokens(text),
            "temperature": 0,
            "stop": translator.STOP,
            "add_special_tokens": True,
            "stream": False,
            # 同じ原文を繰り返し流すので、前の回の読み取り結果を使わせない
            "cache_prompt": False,
        },
        ensure_ascii=False,
    ).encode()
    headers = {"Content-Type": "application/json"}
    if api_key:
        headers["Authorization"] = f"Bearer {api_key}"
    started = time.monotonic()
    with urllib.request.urlopen(
        urllib.request.Request(url, data=body, headers=headers), timeout=1800
    ) as resp:
        reply = json.load(resp)
    timings = reply.get("timings") or {}
    results[index] = {
        "seconds": time.monotonic() - started,
        "prompt_n": timings.get("prompt_n", 0),
        "prompt_tps": timings.get("prompt_per_second", 0.0),
        "predicted_n": timings.get("predicted_n", 0),
        "predicted_tps": timings.get("predicted_per_second", 0.0),
        "chars": len(reply["choices"][0]["text"]),
        "finish": reply["choices"][0].get("finish_reason"),
    }


def run(url: str, api_key: str, name: str, parallel: int) -> dict:
    results: list = [None] * parallel
    threads = [
        threading.Thread(
            target=run_one, args=(url, api_key, TEXTS[name], TARGETS[name], results, i)
        )
        for i in range(parallel)
    ]
    started = time.monotonic()
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    wall = time.monotonic() - started
    if any(result is None for result in results):
        raise SystemExit("失敗したリクエストがあります (上のエラーを参照)")
    generated = sum(r["predicted_n"] for r in results)
    return {
        "text": name,
        "parallel": parallel,
        "wall_s": round(wall, 1),
        "prompt_tokens": results[0]["prompt_n"],
        "output_tokens": round(statistics.mean(r["predicted_n"] for r in results)),
        "prompt_tps": round(statistics.mean(r["prompt_tps"] for r in results), 1),
        "gen_tps_each": round(statistics.mean(r["predicted_tps"] for r in results), 1),
        "gen_tps_total": round(generated / wall, 1),
        "finish": sorted({r["finish"] for r in results}),
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=DESCRIPTION)
    parser.add_argument("--url", default="http://127.0.0.1:8080/v1/completions")
    parser.add_argument(
        "--api-key", default=os.environ.get("TR_API_KEY", ""), help="既定は TR_API_KEY"
    )
    parser.add_argument("--parallel", default="1", help="同時に流す本数 (カンマ区切り)")
    parser.add_argument("--text", default="ja-short,ja-long,en-long", help=",".join(TEXTS))
    parser.add_argument("--repeat", type=int, default=2, help="条件ごとの回数 (中央値を出す)")
    args = parser.parse_args()
    for name in args.text.split(","):
        for parallel in (int(n) for n in args.parallel.split(",")):
            runs = [run(args.url, args.api_key, name, parallel) for _ in range(args.repeat)]
            runs.sort(key=lambda r: r["wall_s"])
            print(json.dumps(runs[len(runs) // 2], ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
