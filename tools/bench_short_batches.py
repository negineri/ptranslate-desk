#!/usr/bin/env python3
"""短文の個別翻訳と番号付きリストを、同じ並列数で比較する。"""

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import json
import os
import re
from statistics import median
import time
from urllib.request import Request, urlopen

from bench import translator

LABELS = (
    "Site Rules",
    "Recent Changes",
    "Random Page",
    "Incident Reports",
    "File an Appeal",
    "Research Notes",
    "Restricted Access",
    "Browse by Series",
    "Revision History",
    "Containment Procedures",
    "Staff Picks",
    "Search by Tag",
    "Request Access",
    "New Forum Posts",
    "Field Manual",
    "Report a Problem",
)


def unpack(text, count):
    lines = text.strip().splitlines()
    if len(lines) != count:
        return None
    result = []
    for number, line in enumerate(lines, 1):
        match = re.fullmatch(rf"\s*{number}[.．、]\s*(\S.*?)\s*", line)
        if not match:
            return None
        result.append(match[1])
    return result


def translate(url, text, context=""):
    prompt = translator.build_prompt(
        dict(
            text=text, source="en", target="ja", form=None, context=context, glossary=[], history=[]
        )
    )
    body = dict(
        model=translator.MODEL,
        prompt=prompt,
        max_tokens=translator.max_tokens(text),
        temperature=0,
        stop=translator.STOP,
        add_special_tokens=True,
        stream=False,
    )
    headers = {"Content-Type": "application/json"}
    if key := os.environ.get("TR_API_KEY"):
        headers["Authorization"] = f"Bearer {key}"
    with urlopen(Request(url, json.dumps(body).encode(), headers), timeout=45) as response:
        choice = json.load(response)["choices"][0]
    text = choice["text"].strip()
    return text if choice["finish_reason"] == "stop" and text else None


def translate_group(url, labels, with_context):
    if len(labels) == 1:
        return [translate(url, labels[0])], 1, 0
    text = "\n".join(f"{number}. {label}" for number, label in enumerate(labels, 1))
    context = ""
    if with_context:
        actions = {
            "File an Appeal",
            "Browse by Series",
            "Search by Tag",
            "Request Access",
            "Report a Problem",
        }
        roles = " ".join(
            f"Item {number} is {'a button that starts an action' if label in actions else 'a link to a section or resource'}."
            for number, label in enumerate(labels, 1)
        )
        context = (
            "The page is an English collaborative fiction wiki about a fictional organization "
            "that researches and contains anomalous objects. These numbered items are short "
            f"navigation labels from the website. {roles}"
        )
    if translated := translate(url, text, context):
        if parsed := unpack(translated, len(labels)):
            return parsed, 1, 0
    return [translate(url, label) for label in labels], 1 + len(labels), 1


def run(url, labels, size, parallel, with_context):
    first = None
    requests = fallbacks = 0
    pairs = {}
    with ThreadPoolExecutor(max_workers=parallel) as pool:
        start = time.monotonic()
        jobs = {
            pool.submit(translate_group, url, labels[i : i + size], with_context): labels[
                i : i + size
            ]
            for i in range(0, len(labels), size)
        }
        for job in as_completed(jobs):
            texts, calls, fallback = job.result()
            if first is None and any(texts):
                first = time.monotonic() - start
            requests += calls
            fallbacks += fallback
            pairs.update(zip(jobs[job], texts, strict=True))
        elapsed = time.monotonic() - start
    if any(text is None for text in pairs.values()):
        raise RuntimeError("個別翻訳で未完了の訳がありました")
    return dict(first=first, elapsed=elapsed, requests=requests, fallbacks=fallbacks, pairs=pairs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default="http://127.0.0.1:8080/v1/completions")
    parser.add_argument("--parallel", type=int, choices=[1, 2, 4, 8], default=4)
    args = parser.parse_args()
    if translate(args.url, "Please close the window.") is None:
        raise RuntimeError("ウォームアップの訳を取得できませんでした")
    conditions = ((1, False), (4, False), (8, False), (4, True), (8, True))
    results = {condition: [] for condition in conditions}
    for order, labels in [
        (conditions, LABELS),
        (conditions[::-1], LABELS[::-1]),
        (conditions[2:] + conditions[:2], LABELS[4:] + LABELS[:4]),
    ]:
        for size, context in order:
            result = run(args.url, labels, size, args.parallel, context)
            results[size, context].append(result)
            print(
                json.dumps(dict(size=size, context=context, **result), ensure_ascii=False),
                flush=True,
            )
    for (size, context), rounds in results.items():
        print(
            json.dumps(
                dict(
                    size=size,
                    context=context,
                    median_first=median(r["first"] for r in rounds),
                    median_elapsed=median(r["elapsed"] for r in rounds),
                    requests=[r["requests"] for r in rounds],
                    fallbacks=sum(r["fallbacks"] for r in rounds),
                )
            )
        )


if __name__ == "__main__":
    main()
