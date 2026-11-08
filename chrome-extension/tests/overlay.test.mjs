import assert from "node:assert/strict";
import { test } from "node:test";
import { JSDOM } from "jsdom";
import { mountOverlay } from "../dist/overlay.mjs";

function fixture() {
  const dom = new JSDOM("<!doctype html><html><body>Page</body></html>", { url: "https://example.com/", runScripts: "outside-only" });
  const window = dom.window;
  let frame;
  const create = window.document.createElement.bind(window.document);
  window.document.createElement = (name) => {
    const element = create(name);
    if (name === "iframe") {
      frame = element;
      Object.defineProperty(frame, "contentWindow", { value: {} });
    }
    return element;
  };
  const mount = window.eval(`(${mountOverlay.toString()})`);
  return { dom, window, mount, get frame() { return frame; } };
}

test("読み込めない小窓は透明な枠を残さず、別ウィンドウへの切り替えを返す", async () => {
  const f = fixture();
  const result = f.mount("chrome-extension://test/popup.html", null, 10);
  assert.equal(f.frame.style.visibility, "hidden");
  assert.equal(await result, "blocked");
  assert.equal(f.window.document.getElementById("ptranslate-extension-overlay"), null);
  f.dom.window.close();
});

test("準備完了した小窓だけを表示し、原文ページからの偽の通知を無視する", async () => {
  const f = fixture();
  const result = f.mount("chrome-extension://test/popup.html", null, 100);
  f.window.dispatchEvent(new f.window.MessageEvent("message", { source: f.window, data: { ptranslate: "ready" } }));
  assert.equal(f.frame.style.visibility, "hidden");
  f.window.dispatchEvent(new f.window.MessageEvent("message", { source: f.frame.contentWindow, data: { ptranslate: "ready" } }));
  assert.equal(await result, "ready");
  assert.equal(f.frame.style.visibility, "visible");
  f.window.dispatchEvent(new f.window.MessageEvent("message", { source: f.frame.contentWindow, data: { ptranslate: "close" } }));
  assert.equal(f.window.document.getElementById("ptranslate-extension-overlay"), null);
  f.dom.window.close();
});

test("利用者が読み込み中に閉じた場合は別ウィンドウを出さない", async () => {
  const f = fixture();
  const result = f.mount("chrome-extension://test/popup.html", null, 100);
  f.window.dispatchEvent(new f.window.KeyboardEvent("keydown", { key: "Escape" }));
  assert.equal(await result, "closed");
  assert.equal(f.window.document.getElementById("ptranslate-extension-overlay"), null);
  f.dom.window.close();
});
