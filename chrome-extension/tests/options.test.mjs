import assert from "node:assert/strict";
import { readFile, access } from "node:fs/promises";
import { test } from "node:test";
import { JSDOM } from "jsdom";
import { mountOptions } from "../dist/options.mjs";

const html = await readFile(new URL("../dist/options.html", import.meta.url), "utf8");
const config = () => Response.json({ user: "test", languages: [{ code: "ja", name: "日本語" }] });
const wait = async (predicate) => {
  for (let i = 0; i < 100; i++) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  assert.fail("接続操作が完了しませんでした");
};
async function setup(t, { stored = {}, fetch = async () => config(), permission = true, ...options } = {}) {
  const dom = new JSDOM(html);
  const local = { ...stored }, session = {}, opened = [], permissions = [], requests = [];
  const area = (values) => ({
    get: async () => ({ ...values }), set: async (data) => { Object.assign(values, data); },
    remove: async (key) => { delete values[key]; },
  });
  const chrome = { runtime: { id: "a".repeat(32) },
    storage: { local: area(local), session: area(session) },
    permissions: { request: (request) => { permissions.push(request); return Promise.resolve(permission); } },
    tabs: { create: async () => {} },
  };
  const previous = globalThis.chrome; globalThis.chrome = chrome;
  const ui = mountOptions({ document: dom.window.document, chrome, openDesktop: (url) => opened.push(url),
    fetch: async (route, request) => { requests.push({ route, ...request }); return fetch(route, request); },
    retryMs: 1, registrationTimeoutMs: 50, ...options });
  t.after(() => { ui.destroy(); dom.window.close(); globalThis.chrome = previous; });
  await ui.ready;
  const $ = (id) => dom.window.document.getElementById(id);
  const mode = (value) => { $("mode").value = value; $("mode").dispatchEvent(new dom.window.Event("change")); };
  return { $, mode, ui, local, session, opened, requests, permissions, dom };
}

test("設定と選択文の小窓には原文の入力翻訳を置かず、ツールバーの入力画面も梱包しない", async () => {
  const document = new JSDOM(html).window.document;
  assert.equal(document.querySelector("h1").textContent, "設定");
  assert.equal(document.querySelector("#save").textContent, "接続");
  assert.equal(document.querySelector("#connect-desktop"), null);
  assert.equal(document.querySelector("#src"), null);
  const popup = new JSDOM(await readFile(new URL("../dist/popup.html", import.meta.url), "utf8"));
  assert.equal(popup.window.document.querySelector("input,textarea,[contenteditable]"), null);
  const manifest = JSON.parse(await readFile(new URL("../dist/manifest.json", import.meta.url), "utf8"));
  assert.equal(manifest.action.default_popup, undefined);
  await assert.rejects(access(new URL("../dist/action.html", import.meta.url)));
  await assert.rejects(access(new URL("../dist/action.mjs", import.meta.url)));
  document.defaultView.close(); popup.window.close();
});

test("ローカルの接続1回でデスクトップを開き、登録後の接続まで待つ", async (t) => {
  let attempts = 0;
  const p = await setup(t, { fetch: async () => { if (++attempts < 3) throw new Error("未登録"); return config(); } });
  p.mode("local"); p.$("save").click();
  assert.deepEqual(p.opened, [`ptranslate://register-chrome?extension_id=${"a".repeat(32)}`]);
  await wait(() => p.$("status").textContent.includes("接続できました"));
  assert.equal(p.local.mode, "local"); assert.equal(p.session.connectedTo, "local");
  assert.ok(p.requests.every((request) => request.route === "api/config" && request.connection.mode === "local"));
});

test("ローカルへ接続済みなら登録確認を開き直さず接続を確かめる", async (t) => {
  const p = await setup(t, { stored: { mode: "local" } });
  p.$("save").click();
  await wait(() => p.requests.length === 2 && p.$("status").textContent.includes("接続できました"));
  assert.equal(p.opened.length, 0);
});

test("接続待ち中に翻訳場所を変えるとローカルへの再試行を止める", async (t) => {
  const p = await setup(t, { fetch: async () => { throw new Error("未登録"); } });
  p.mode("local"); p.$("save").click();
  await wait(() => p.requests.length > 0);
  p.mode("server");
  const count = p.requests.length;
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(p.requests.length, count); assert.equal(p.session.connectedTo, undefined);
});

test("登録されない場合は接続待ちを終えて理由を表示する", async (t) => {
  const p = await setup(t, { registrationTimeoutMs: 10, fetch: async () => { throw new Error("デスクトップに接続できません"); } });
  p.mode("local"); p.$("save").click();
  await wait(() => p.$("status").classList.contains("err"));
  assert.match(p.$("status").textContent, /デスクトップに接続できません/);
  const count = p.requests.length;
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(p.requests.length, count); assert.equal(p.session.connectedTo, undefined);
});

test("サーバーの接続はクリック直後に権限を求め、設定を保存して接続する", async (t) => {
  const p = await setup(t);
  p.$("server").value = " https://translate.example.com/ "; p.$("save").click();
  assert.equal(p.permissions.length, 1);
  await wait(() => p.$("status").textContent.includes("接続できました"));
  assert.equal(p.local.server, "https://translate.example.com");
  assert.equal(p.session.connectedTo, "server:https://translate.example.com");
  assert.equal(p.opened.length, 0);
});

test("権限が拒否された場合は要求を送らず、接続済みにしない", async (t) => {
  const p = await setup(t, { permission: false });
  p.$("server").value = "https://translate.example.com"; p.$("save").click();
  await wait(() => p.$("status").classList.contains("err"));
  assert.equal(p.requests.length, 0); assert.equal(p.session.connectedTo, undefined);
});

test("未ログインなら設定内にログインの案内を表示する", async (t) => {
  const p = await setup(t, { stored: { server: "https://translate.example.com" }, fetch: async () => Response.json({}, { status: 401 }) });
  assert.equal(p.$("login").hidden, false); assert.equal(p.session.connectedTo, undefined);
});
