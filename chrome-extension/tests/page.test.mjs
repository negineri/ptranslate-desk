// ページ全体の翻訳 (background 側)。先に node chrome-extension/build.mjs で dist/ を作ってから実行する
import assert from "node:assert/strict";
import { test } from "node:test";
import { runPageTask, shouldTranslate } from "../dist/page-task.mjs";
import { portPair, SERVER, sse } from "./helpers.mjs";

const CONFIG = { user: "test", max_glossary: 200, languages: [{ code: "ja", name: "日本語" }, { code: "en", name: "英語" }] };
const SETTINGS = { server: SERVER, source: "", target: "auto", form: "", glossary: "" };

// 訳を "[訳す先] 原文" で返すサーバー。受けた要求を requests に残す
function fakeServer({ config = () => Response.json(CONFIG), translate } = {}) {
  const requests = [];
  const fetch = async (url, init) => {
    if (url.endsWith("/api/config")) return config();
    const body = JSON.parse(init.body);
    requests.push({ body, signal: init.signal });
    if (translate) return translate(body, init);
    return sse([{ t: `[${body.target_lang}] ` }, { t: body.text }, { done: true, finish: "stop" }]);
  };
  return { fetch, requests };
}

// ページの側の port を作って blocks を送り、done か error まで受けたメッセージを返す
function runPage(blocks, server, deps = {}) {
  const [page, background] = portPair();
  runPageTask(background, {
    fetch: server.fetch, server: async () => SERVER, hasPermission: async () => true, settings: async () => SETTINGS,
    ...deps,
  });
  const messages = [];
  const finished = new Promise((resolve) => {
    page.onMessage.addListener((msg) => {
      messages.push(msg);
      if (msg.type === "done" || msg.type === "error") resolve(messages);
    });
  });
  page.postMessage({ blocks });
  return { page, messages, finished };
}

test("訳す先の言語ですでに書かれた段落は送らない", () => {
  assert.ok(shouldTranslate("Hello world", "ja"));
  assert.ok(!shouldTranslate("これは日本語の文です", "ja"));
  assert.ok(shouldTranslate("これは日本語の文です", "en"));
  assert.ok(!shouldTranslate("Already English.", "en"));
  assert.ok(shouldTranslate("Déjà vu", "en"));
  assert.ok(!shouldTranslate("2026-11-08 12:00", "ja"));
  assert.ok(shouldTranslate("Bonjour", "fr"));
});

test("段落を順に訳し、前の段落の訳を手がかりに送る", async () => {
  const server = fakeServer();
  const blocks = [
    { id: 0, text: "First paragraph of an English page about translation." },
    { id: 1, text: "日本語の段落" },
    { id: 2, text: "Second paragraph." },
  ];
  const { finished } = runPage(blocks, server);
  const messages = await finished;
  const start = messages.find((m) => m.type === "start");
  assert.deepEqual(start, { type: "start", total: 2, target: "ja", targetName: "日本語" });
  const done = messages.filter((m) => m.type === "block");
  assert.deepEqual(done.map((m) => [m.id, m.text, m.done]), [
    [0, "[ja] First paragraph of an English page about translation.", 1],
    [2, "[ja] Second paragraph.", 2],
  ]);
  assert.ok(messages.some((m) => m.type === "partial" && m.id === 0));
  assert.deepEqual(messages.at(-1), { type: "done", total: 2, truncated: 0 });
  assert.deepEqual(server.requests.map((r) => r.body.history), [
    [], [{
      source: "First paragraph of an English page about translation.",
      target: "[ja] First paragraph of an English page about translation.",
    }],
  ]);
  assert.equal(server.requests[0].body.target_lang, "ja");
});

test("訳す先の自動はページ全体の文で決める", async () => {
  const server = fakeServer();
  const { finished } = runPage([
    { id: 0, text: "これは日本語のページです。説明が続きます。" },
    { id: 1, text: "English menu" },
  ], server);
  const messages = await finished;
  assert.equal(messages.find((m) => m.type === "start").target, "en");
  assert.deepEqual(messages.filter((m) => m.type === "block").map((m) => m.id), [0]);
});

test("長い段落は区切って訳し、つなげて返す", async () => {
  const server = fakeServer();
  const long = ("A sentence. ").repeat(200).trim();
  const messages = await runPage([{ id: 0, text: long }], server).finished;
  assert.ok(server.requests.length >= 2);
  assert.ok(server.requests.every((r) => r.body.text.length <= 1500));
  const block = messages.find((m) => m.type === "block");
  assert.equal(block.text.split("[ja]").length - 1, server.requests.length);
});

test("ログインしていないときは login を返す", async () => {
  const server = fakeServer({ config: () => new Response("<html>", { status: 403 }) });
  const messages = await runPage([{ id: 0, text: "Hello" }], server).finished;
  assert.equal(messages.at(-1).type, "error");
  assert.equal(messages.at(-1).code, "login");
});

test("接続先が未設定なら設定を促す", async () => {
  const server = fakeServer();
  const messages = await runPage([{ id: 0, text: "Hello" }], server, { server: async () => "" }).finished;
  assert.deepEqual(messages.at(-1), {
    type: "error", code: "no-server", remedy: "options",
    message: "接続先が未設定です。拡張の設定でサーバーURLを入れてください",
  });
});

test("ページが port を閉じたら訳している要求も止める", async () => {
  const server = fakeServer({
    translate: (body, init) => new Response(new ReadableStream({
      start(c) {
        c.enqueue(new TextEncoder().encode('data: {"t":"a"}\n\n'));
        init.signal.addEventListener("abort", () => c.error(init.signal.reason));
      },
    })),
  });
  const { page, messages } = runPage([{ id: 0, text: "One" }, { id: 1, text: "Two" }], server);
  while (!messages.some((m) => m.type === "partial")) await new Promise((r) => setTimeout(r, 5));
  page.disconnect();
  await new Promise((r) => setTimeout(r, 30));
  assert.equal(server.requests.length, 1);
  assert.ok(server.requests[0].signal.aborted);
});
