import assert from "node:assert/strict";
import { test } from "node:test";
import { nativeFetch } from "../dist/native.mjs";
import { apiFetch } from "../dist/relay.mjs";
import { Scheduler } from "../dist/scheduler.mjs";
import { concurrencyLimit } from "../dist/page-task.mjs";
import { portPair } from "./helpers.mjs";

const tick = () => new Promise((resolve) => setTimeout(resolve, 10));

test("通知の上限に従い、旧接続先と不正な値は安全に扱う", () => {
  assert.equal(concurrencyLimit({}), 1);
  for (const value of [0, -1, 1.5, "2", Infinity, null]) assert.equal(concurrencyLimit({ concurrency: { max_in_flight: value } }), 1);
  assert.equal(concurrencyLimit({ concurrency: { max_in_flight: 4 } }), 4);
  assert.equal(concurrencyLimit({ concurrency: { max_in_flight: 2 } }), 2);
  assert.equal(concurrencyLimit({ concurrency: { max_in_flight: 1000 } }), 8);
});

test("Native Messagingのバイト列から分割された日本語も復元する", async () => {
  const [client, host] = portPair();
  host.onMessage.addListener((request) => {
    assert.deepEqual(request, { version: 1, route: "api/translate", body: { text: "Hello" } });
    host.postMessage({ kind: "start", status: 200 });
    const bytes = [...new TextEncoder().encode("こんにちは")];
    host.postMessage({ kind: "bytes", bytes: bytes.slice(0, 2) });
    host.postMessage({ kind: "bytes", bytes: bytes.slice(2) });
    host.postMessage({ kind: "end" });
  });
  const res = await nativeFetch("api/translate", JSON.stringify({ text: "Hello" }), new AbortController(), () => client);
  assert.equal(await new Response(res.body).text(), "こんにちは");
});

test("Native Messagingの未接続・途中切断・中止を通知する", async () => {
  const [client, host] = portPair();
  const abort = new AbortController();
  const pending = nativeFetch("api/config", null, abort, () => client);
  host.disconnect();
  await assert.rejects(pending, /ローカル翻訳/);
  const [c2, h2] = portPair();
  h2.onMessage.addListener(() => h2.postMessage({ kind: "start", status: 200 }));
  const res = await nativeFetch("api/translate", null, abort, () => c2);
  const text = new Response(res.body).text();
  abort.abort(new DOMException("中止", "AbortError"));
  await assert.rejects(text, { name: "AbortError" });
  let disconnected = false;
  h2.onDisconnect.addListener(() => { disconnected = true; });
  await tick();
  assert.ok(disconnected);
});

test("ローカルモードはHTTP・Cookie・接続権限を使わない", async () => {
  let calls = 0;
  const deps = {
    settings: async () => ({ mode: "local" }),
    server: () => assert.fail("サーバーは参照しない"),
    fetch: () => assert.fail("HTTPへ転送しない"),
    hasPermission: () => assert.fail("HTTP権限は要求しない"),
    nativeFetch: async () => { calls++; return { status: 200, body: Response.json({ concurrency: { max_in_flight: 4 } }).body }; },
  };
  const res = await apiFetch("api/config", null, deps, new AbortController());
  assert.equal((await new Response(res.body).json()).concurrency.max_in_flight, 4);
  assert.equal(calls, 1);
});

for (const limit of [1, 2, 4]) {
  test(`共有枠${limit}件で応答を読み終えるまで保持し、待機中の中止は送信しない`, async () => {
    const scheduler = new Scheduler();
    const abort = new AbortController();
    let calls = 0;
    const call = async () => { calls++; return { status: 200, body: new Response("translation").body }; };
    const active = await Promise.all(Array.from({ length: limit }, () => scheduler.run("local", limit, abort.signal, call)));
    const waitingAbort = new AbortController();
    const waiting = scheduler.run("local", limit, waitingAbort.signal, call);
    await tick(); assert.equal(calls, limit);
    waitingAbort.abort(new DOMException("中止", "AbortError"));
    await assert.rejects(waiting, { name: "AbortError" });
    const next = scheduler.run("local", limit, abort.signal, call);
    await new Response(active[0].body).text();
    const res = await next;
    assert.equal(calls, limit + 1);
    await res.body.cancel();
    for (const res of active.slice(1)) await res.body.cancel();
    assert.equal(scheduler.groups.get("local").active, 0);
  });
}

test("共有枠は接続先ごとに分かれ、実行中の失敗でも解放する", async () => {
  const scheduler = new Scheduler(); const abort = new AbortController();
  await assert.rejects(scheduler.run("a", 1, abort.signal, async () => { throw new Error("network"); }));
  const call = async () => ({ status: 200, body: new Response("ok").body });
  const a = await scheduler.run("a", 1, abort.signal, call);
  const b = await scheduler.run("b", 1, abort.signal, call);
  await a.body.cancel(); await b.body.cancel();
});

test("Chromeのホスト未検出を、登録と完全再起動の案内にする", async () => {
  const previous = globalThis.chrome;
  globalThis.chrome = { runtime: { lastError: { message: "Specified native messaging host not found." } } };
  try {
    const [client, host] = portPair();
    const pending = nativeFetch("api/config", null, new AbortController(), () => client);
    host.disconnect();
    await assert.rejects(pending, (error) => error.code === "local" && error.message.includes("Chromeを完全に終了"));
  } finally { globalThis.chrome = previous; }
});
