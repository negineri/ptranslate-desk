import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { test } from "node:test";
import * as translation from "../dist/translation.mjs";
import { volunteerFields } from "../dist/volunteer-consent.mjs";

// 拡張の試験と同じ jsdom を使い、ビルドで組み立てたメイン画面を動かす。
const require = createRequire(new URL("../../chrome-extension/package.json", import.meta.url));
const { JSDOM } = require("jsdom");
const html = await readFile(new URL("../dist/index.html", import.meta.url), "utf8");

async function waitFor(check) {
  for (let i = 0; i < 200; i++) {
    if (check()) return;
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  assert.fail("翻訳画面が期待した状態になりませんでした");
}

test("翻訳中に原文を元に戻すと、以前の訳と完了表示に戻る", async (t) => {
  const { window } = new JSDOM(html, { url: "https://app.example", runScripts: "outside-only" });
  t.after(() => window.close());
  const el = (id) => window.document.getElementById(id);
  const sent = [];
  let interrupted;
  let configReads = 0;
  Object.assign(window, translation, {
    volunteerFields,
    translationMode: () => false,
    fetch: async (route, { body, signal } = {}) => {
      if (route === "api/config") {
        configReads++;
        return Response.json({ desktop_server: "https://translate.example", languages: [] });
      }
      assert.equal(route, "api/translate");
      const { text } = JSON.parse(body);
      sent.push(text);
      if (text === "Original text") {
        return new Response('data: {"t":"元の訳"}\n\ndata: {"done":true,"finish":"stop"}\n\n');
      }
      interrupted = signal;
      return new Response(new ReadableStream({
        start(stream) {
          stream.enqueue(new TextEncoder().encode('data: {"t":"途中の訳"}\n\n'));
          signal.addEventListener("abort", () => stream.error(new Error("中止")), { once: true });
        },
      }));
    },
  });
  const ready = new Promise((resolve) => window.document.addEventListener("desktop-ready", resolve, { once: true }));
  const script = html.match(/<script type="module">([\s\S]*?)<\/script>/)[1];
  window.eval(script.replace(/^import .*$/gm, ""));
  await ready;

  const input = (text) => {
    el("src").value = text;
    el("src").dispatchEvent(new window.Event("input"));
  };
  input("Original text");
  await waitFor(() => el("status").textContent === "訳し終わりました");
  input("Changed text");
  await waitFor(() => el("out").textContent === "途中の訳");
  assert.equal(el("status").textContent, "訳しています");
  input("Original text");
  await waitFor(() => interrupted.aborted && el("stop").hidden);
  assert.equal(el("out").textContent, "元の訳");
  assert.match(el("status").textContent, /^訳し終わりました/);
  assert.deepEqual(sent, ["Original text", "Changed text"]);
  assert.equal(configReads, 1);
});

test("409 と翻訳先変更の後だけ設定を取り直す", async (t) => {
  const { window } = new JSDOM(html, { url: "https://app.example", runScripts: "outside-only" });
  t.after(() => window.close());
  const el = (id) => window.document.getElementById(id);
  const configReads = [];
  const sent = [];
  let local = false;
  let server = "https://first.example";
  Object.assign(window, translation, {
    volunteerFields,
    translationMode: () => local,
    fetch: async (route, { body, localOverride } = {}) => {
      if (route === "api/config") {
        configReads.push(localOverride);
        return Response.json({ languages: [], ...(localOverride ? {} : { desktop_server: server }) });
      }
      const { desktop_server } = JSON.parse(body);
      sent.push(desktop_server);
      if (!localOverride && desktop_server !== server) {
        return Response.json({ error: "翻訳先が変わりました" }, { status: 409 });
      }
      return new Response('data: {"t":"訳"}\n\ndata: {"done":true,"finish":"stop"}\n\n');
    },
  });
  const ready = new Promise((resolve) => window.document.addEventListener("desktop-ready", resolve, { once: true }));
  const script = html.match(/<script type="module">([\s\S]*?)<\/script>/)[1];
  window.eval(script.replace(/^import .*$/gm, ""));
  await ready;

  server = "https://second.example";
  el("src").value = "Sample text";
  el("go").click();
  await waitFor(() => el("status").textContent === "翻訳先が変わりました");
  assert.deepEqual(configReads, [false]);
  el("go").click();
  await waitFor(() => el("status").textContent === "訳し終わりました");
  assert.deepEqual(configReads, [false, false]);

  local = true;
  window.document.dispatchEvent(new window.Event("translation-destination-changed"));
  el("go").click();
  await waitFor(() => sent.length === 3 && el("stop").hidden);
  assert.deepEqual(configReads, [false, false, true]);
  assert.deepEqual(sent, ["https://first.example", "https://second.example", undefined]);
});
