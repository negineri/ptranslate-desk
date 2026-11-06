import assert from "node:assert/strict";
import { test } from "node:test";
import { splitText } from "../static/translation.mjs";

test("短い段落は空行ごとまとめ、入りきらない区切りの空白を残す", () => {
  const first = "あ".repeat(1495) + "\n \n短文";
  assert.deepEqual(splitText(first + "\n\n次の段落"), [
    { text: first, tail: "\n\n" },
    { text: "次の段落", tail: "" },
  ]);
});

test("長い段落は文末で区切り、長すぎる文は上限で区切る", () => {
  const sentence = "あ".repeat(1000) + "。";
  const long = "い".repeat(1500);
  assert.deepEqual(splitText(sentence + sentence + long + "う"), [
    { text: sentence, tail: "" },
    { text: sentence, tail: "" },
    { text: long, tail: "" },
    { text: "う", tail: "" },
  ]);
});
