// 先に node chrome-extension/build.mjs で dist/ を作ってから実行する
import assert from "node:assert/strict";
import { test } from "node:test";
import { chromeRegistrationUrl, normalizeServerUrl, originPattern, requestSettings, sanitize } from "../dist/settings.mjs";
import { isExtensionPage, relay } from "../dist/relay.mjs";
import { extensionFetch, sessionFetch } from "../dist/transport.mjs";
import { ensureOk, LoginRequired, remedy } from "../dist/translate.mjs";
import { translateChunk } from "../dist/translation.mjs";
import { portPair, SERVER, sse } from "./helpers.mjs";

function connectTo(deps) {
  return () => {
    const [page, background] = portPair();
    relay(background, { server: async () => SERVER, hasPermission: async () => true, ...deps });
    return page;
  };
}

test("サーバーURLは origin にそろえ、HTTPS かループバックだけを受ける", () => {
  assert.equal(normalizeServerUrl(" https://translate.example.com/ "), SERVER);
  assert.equal(normalizeServerUrl("http://127.0.0.1:8003"), "http://127.0.0.1:8003");
  assert.equal(normalizeServerUrl("http://localhost:8003/"), "http://localhost:8003");
  for (const bad of ["translate.example.com", "http://translate.example.com", "https://translate.example.com/app",
    "https://user:pw@translate.example.com", "https://translate.example.com/?a=1", "ftp://example.com"]) {
    assert.throws(() => normalizeServerUrl(bad), Error, bad);
  }
  assert.equal(originPattern("http://127.0.0.1:8003"), "http://127.0.0.1/*");
  assert.equal(originPattern(SERVER), "https://translate.example.com/*");
});

test("保存した設定は決まった項目の文字列だけを読む", () => {
  assert.deepEqual(sanitize({ server: SERVER, target: 3, text: "原文", glossary: "a => b" }),
    { server: SERVER, mode: "server", source: "", target: "ja", form: "", glossary: "a => b" });
});

test("送る設定: 自動の訳す先、文体は日本語のときだけ、用語集", () => {
  const settings = { source: "", target: "auto", form: "polite", glossary: "管理画面 => dashboard\n壊れた行" };
  assert.deepEqual(requestSettings("Hello world", settings), {
    target_lang: "ja", source_lang: null, form: "polite", context: "",
    glossary: [{ source: "管理画面", target: "dashboard" }],
  });
  assert.equal(requestSettings("こんにちは", settings).target_lang, "en");
  assert.equal(requestSettings("こんにちは", settings).form, null);
  assert.equal(requestSettings("Hi", { ...settings, target: "fr" }).target_lang, "fr");
});

test("拡張自身の画面からの接続だけを受ける", () => {
  const root = "chrome-extension://abcdefghijklmnop/";
  assert.ok(isExtensionPage({ url: `${root}popup.html#x` }, root));
  assert.ok(!isExtensionPage({ url: "https://evil.example/" }, root));
  assert.ok(!isExtensionPage({ url: "chrome-extension://abcdefghijklmnopq/x" }, root));
  assert.ok(!isExtensionPage({}, root));
});

test("background 経由で SSE を流し、Web と同じ読み取りで訳を得る", async () => {
  let seen = null;
  const fetch = async (url, init) => {
    seen = { url, init };
    return sse([{ t: "こん" }, { t: "にちは" }, { done: true, finish: "stop" }]);
  };
  const res = await extensionFetch("api/translate", { body: JSON.stringify({ text: "Hello" }) }, connectTo({ fetch }));
  assert.equal(res.status, 200);
  let text = "";
  assert.equal(await translateChunk(res, (t) => { text += t; }), "stop");
  assert.equal(text, "こんにちは");
  assert.equal(seen.url, `${SERVER}/api/translate`);
  assert.equal(seen.init.method, "POST");
  assert.equal(seen.init.credentials, "include");
  assert.equal(seen.init.redirect, "manual");
  assert.equal(seen.init.headers["Content-Type"], "application/json");
  assert.equal(seen.init.body, JSON.stringify({ text: "Hello" }));
});

test("中止すると background の fetch も止まる", async () => {
  let fetchSignal = null;
  const fetch = async (url, init) => {
    fetchSignal = init.signal;
    // 本物の fetch と違い、中止しても自分では止まらない流れ (relay が読み取りを止めること)
    const stream = new ReadableStream({ start(c) { c.enqueue(new TextEncoder().encode("data: {\"t\":\"a\"}\n\n")); } });
    return new Response(stream, { status: 200 });
  };
  const abort = new AbortController();
  const res = await extensionFetch("api/translate", { body: "{}", signal: abort.signal }, connectTo({ fetch }));
  const reading = translateChunk(res, () => abort.abort());
  await assert.rejects(reading);
  await new Promise((r) => setTimeout(r, 20));
  assert.ok(fetchSignal.aborted);
});

test("ログイン画面への転送はログインしていないとして扱う", async () => {
  const fetch = async () => ({ type: "opaqueredirect", status: 0, body: null });
  const res = await extensionFetch("api/config", {}, connectTo({ fetch }));
  assert.equal(res.status, 401);
  await assert.rejects(ensureOk(res), LoginRequired);
});

test("接続先が未設定・権限がない・知らない API は理由を返す", async () => {
  const fetch = async () => assert.fail("つないではいけない");
  await assert.rejects(extensionFetch("api/config", {}, connectTo({ fetch, server: async () => "" })),
    (e) => e.code === "no-server" && remedy(e) === "options");
  await assert.rejects(extensionFetch("api/config", {}, connectTo({ fetch, hasPermission: async () => false })),
    (e) => e.code === "no-permission");
  await assert.rejects(extensionFetch("api/other", {}, connectTo({ fetch })), (e) => e.code === "route");
});

test("つながらないときは翻訳サーバーに接続できないと返す", async () => {
  const fetch = async () => { throw new TypeError("Failed to fetch"); };
  await assert.rejects(extensionFetch("api/config", {}, connectTo({ fetch })),
    (e) => e.code === "network" && e.message === "翻訳サーバーに接続できません");
});

test("応答が止まったら時間切れにする", async () => {
  const fetch = async (url, init) => new Response(new ReadableStream({
    start(c) { init.signal.addEventListener("abort", () => c.error(init.signal.reason)); },
  }), { status: 200 });
  const res = await extensionFetch("api/translate", { body: "{}" }, connectTo({ fetch, timeoutMs: 30 }));
  await assert.rejects(translateChunk(res, () => {}), (e) => e.code === "timeout");
});

test("エラーの状態を画面の言葉にする", async () => {
  await assert.rejects(ensureOk(new Response("<html>", { status: 403 })), LoginRequired);
  await assert.rejects(ensureOk(new Response("", { status: 502 })), /翻訳サーバーに接続できません/);
  await assert.rejects(ensureOk(new Response(JSON.stringify({ error: "同時に訳せるのは 2 つまでです" }), { status: 429 })),
    /同時に訳せるのは 2 つまでです/);
});

for (const initial of [{ mode: "local", server: SERVER }, { mode: "server", server: SERVER }]) {
  test(`区切りごとのportをまたいで開始時の接続先を保つ (${initial.mode})`, async () => {
    let settings = { ...initial };
    const calls = [];
    const connect = connectTo({
      settings: async () => settings,
      server: async () => settings.server,
      nativeFetch: async (route) => { calls.push(`local/${route}`); return { status: 200, body: new Response("ok").body }; },
      fetch: async (url) => { calls.push(url); return new Response("ok"); },
    });
    const fetch = sessionFetch(settings, connect);
    await (await fetch("api/config")).text();
    settings.mode = "server";
    settings.server = "https://other.example.com";
    for (let i = 0; i < 2; i++) await (await fetch("api/translate", { body: "{}" })).text();
    const prefix = initial.mode === "local" ? "local/" : `${SERVER}/`;
    assert.deepEqual(calls, ["api/config", "api/translate", "api/translate"].map((route) => prefix + route));
    // 新しく開始した翻訳だけが新しい接続先を使う。
    await (await sessionFetch(settings, connect)("api/config")).text();
    assert.equal(calls.at(-1), "https://other.example.com/api/config");
  });
}

test("連携URLには拡張自身の正しい形式のIDだけを入れる", () => {
  const id = "abcdefghijklmnopabcdefghijklmnop";
  assert.equal(chromeRegistrationUrl(id), `ptranslate://register-chrome?extension_id=${id}`);
  for (const invalid of ["", "a".repeat(31), "q".repeat(32), "A".repeat(32), `${id}&extra=1`]) {
    assert.throws(() => chromeRegistrationUrl(invalid));
  }
});

test("新しい設定の翻訳先は日本語にし、保存済みの自動設定は維持する", () => {
  assert.equal(sanitize({}).target, "ja");
  assert.equal(sanitize({ target: "auto" }).target, "auto");
});

test("匿名資格を画面に返さず、接続先を分け、401 では資格だけ更新する", async () => {
  const credential = (n) => `pta_${String(n).padStart(32, "0")}.9999999999.${"a".repeat(64)}`;
  const values = {};
  const calls = [];
  let issued = 0;
  const deps = {
    anonymousStorage: {
      get: async (key) => ({ [key]: values[key] }),
      set: async (entries) => Object.assign(values, entries),
      remove: async (key) => { delete values[key]; },
    },
    fetch: async (url, init) => {
      calls.push({ url, init });
      if (url.endsWith("/api/anonymous")) return Response.json({ credential: credential(++issued) });
      if (url.endsWith("/api/translate")) return new Response(null, { status: 401 });
      return Response.json({ tier: init.headers.Authorization ? "anonymous" : "registered" });
    },
  };
  await assert.rejects(extensionFetch("api/anonymous", {}, connectTo(deps)), (error) => error.code === "route");
  assert.equal(issued, 0);
  const fetch = sessionFetch({ mode: "server", server: SERVER }, connectTo({ ...deps, anonymousActions: true }));
  const result = await fetch("api/anonymous");
  assert.equal(result.status, 204);
  assert.equal(await result.text(), "");
  assert.equal(calls[0].init.body, "{}");
  assert.equal(calls[0].init.credentials, "omit");
  assert.equal(calls[0].init.redirect, "manual");
  const config = await fetch("api/config");
  assert.equal((await config.json()).tier, "anonymous");
  assert.equal(calls[1].init.headers.Authorization, `Bearer ${credential(1)}`);
  assert.equal(calls[1].init.credentials, "omit");

  const other = sessionFetch({ mode: "server", server: "https://other.example" }, connectTo(deps));
  assert.equal((await (await other("api/config")).json()).tier, "registered");
  assert.equal(calls[2].init.headers.Authorization, undefined);
  await assert.rejects(fetch("api/translate", { body: '{"text":"A sentence"}' }), /資格を更新しました/);
  assert.equal(issued, 2);
  assert.equal(calls.filter(({ url }) => url.endsWith("/api/translate")).length, 1);
  assert.equal((await (await fetch("api/config")).json()).tier, "anonymous");
  assert.equal(calls.at(-1).init.headers.Authorization, `Bearer ${credential(2)}`);
  assert.equal((await fetch("anonymous/forget")).status, 204);
  assert.deepEqual(values, {});
  assert.equal((await (await fetch("api/config")).json()).tier, "registered");
});
