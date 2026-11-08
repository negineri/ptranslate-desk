import assert from "node:assert/strict";
import { test } from "node:test";

const event = () => {
  const listeners = [];
  return { addListener: (listener) => listeners.push(listener), emit: async (...args) => Promise.all(listeners.map((listener) => listener(...args))) };
};
async function setup(t, { settings = {}, connectedTo, status = 200, blocked = false } = {}) {
  const session = { connectedTo }, menus = [], scripts = [], badges = [], requests = [];
  let opened = 0;
  const chrome = {
    runtime: { id: "a".repeat(32), getURL: (path) => `chrome-extension://${"a".repeat(32)}/${path}`,
      onInstalled: event(), onConnect: event(), onMessage: event(), openOptionsPage: async () => { opened++; } },
    storage: { local: { get: async () => settings }, session: { setAccessLevel: async () => {},
      get: async () => session, set: async (values) => Object.assign(session, values), remove: async (key) => { delete session[key]; } } },
    contextMenus: { onClicked: event(), create: (menu) => menus.push(menu), removeAll: async () => { menus.length = 0; } },
    commands: { onCommand: event() }, permissions: { contains: async () => true },
    action: { onClicked: event(), setBadgeText: async (badge) => badges.push(badge), setTitle: async () => {} },
    scripting: { executeScript: async (request) => { if (blocked) throw new Error("blocked"); scripts.push(request); } },
  };
  const previous = { chrome: globalThis.chrome, fetch: globalThis.fetch };
  globalThis.chrome = chrome;
  globalThis.fetch = async (url) => { requests.push(url); return Response.json({ languages: [] }, { status }); };
  t.after(() => { globalThis.chrome = previous.chrome; globalThis.fetch = previous.fetch; });
  await import(`../dist/background.mjs?test=${crypto.randomUUID()}`);
  return { chrome, menus, scripts, badges, requests, session, opened: () => opened };
}

test("インストール直後に設定へ誘導し、更新では勝手に開かない", async (t) => {
  const p = await setup(t);
  await p.chrome.runtime.onInstalled.emit({ reason: "install" });
  assert.equal(p.opened(), 1);
  assert.ok(p.menus.some((menu) => menu.title === "設定" && menu.contexts.includes("action")));
  await p.chrome.runtime.onInstalled.emit({ reason: "update" });
  assert.equal(p.opened(), 1); assert.equal(p.menus.length, 3);
});

test("接続済みなら左クリックでそのタブを即翻訳し、設定も入力画面も開かない", async (t) => {
  const p = await setup(t, { settings: { mode: "server", server: "https://translate.example.com" }, connectedTo: "server:https://translate.example.com" });
  await p.chrome.action.onClicked.emit({ id: 7 });
  assert.deepEqual(p.scripts, [{ target: { tabId: 7, frameIds: [0] }, files: ["page-content.js"] }]);
  assert.equal(p.requests.length, 0); assert.equal(p.opened(), 0);
});

test("ローカル接続済みの左クリックでもサーバーへ接続せずページを翻訳する", async (t) => {
  const p = await setup(t, { settings: { mode: "local" }, connectedTo: "local" });
  await p.chrome.action.onClicked.emit({ id: 8 });
  assert.equal(p.scripts.length, 1); assert.equal(p.requests.length, 0);
});

test("未設定の左クリックは設定へ誘導する", async (t) => {
  const p = await setup(t);
  await p.chrome.action.onClicked.emit({ id: 7 });
  assert.equal(p.opened(), 1); assert.equal(p.scripts.length, 0); assert.equal(p.requests.length, 0);
});

test("接続状態が未確認なら確かめ、成功した場合だけ翻訳する", async (t) => {
  const p = await setup(t, { settings: { server: "https://translate.example.com" }, connectedTo: "local" });
  await p.chrome.action.onClicked.emit({ id: 7 });
  assert.deepEqual(p.requests, ["https://translate.example.com/api/config"]);
  assert.equal(p.scripts.length, 1); assert.equal(p.session.connectedTo, "server:https://translate.example.com");
});

test("未ログインの左クリックは翻訳を始めず設定へ誘導する", async (t) => {
  const p = await setup(t, { settings: { server: "https://translate.example.com" }, status: 401 });
  await p.chrome.action.onClicked.emit({ id: 7 });
  assert.equal(p.opened(), 1); assert.equal(p.scripts.length, 0); assert.equal(p.session.connectedTo, undefined);
});

test("右クリックの設定を開き、翻訳できないページではアイコンで理由を知らせる", async (t) => {
  const p = await setup(t, { settings: { mode: "local" }, connectedTo: "local", blocked: true });
  await p.chrome.contextMenus.onClicked.emit({ menuItemId: "settings" }, { id: 7 });
  assert.equal(p.opened(), 1);
  await p.chrome.action.onClicked.emit({ id: 7 });
  assert.deepEqual(p.badges.at(-1), { tabId: 7, text: "!" });
});
