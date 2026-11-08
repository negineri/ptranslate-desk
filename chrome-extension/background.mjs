import { isExtensionPage, relay } from "./relay.mjs";
import { loadSettings, originPattern } from "./settings.mjs";

const MENU_ID = "translate-selection";
const EXTENSION_ROOT = chrome.runtime.getURL("");

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({ id: MENU_ID, title: "PTranslate で翻訳", contexts: ["selection"] });
  chrome.storage.session.setAccessLevel?.({ accessLevel: "TRUSTED_CONTEXTS" });
});

// 拡張の画面からの API 呼び出し (transport.mjs)
chrome.runtime.onConnect.addListener((port) => {
  if (port.name !== "api" || !isExtensionPage(port.sender, EXTENSION_ROOT)) {
    port.disconnect();
    return;
  }
  relay(port, {
    fetch: (...args) => fetch(...args),
    server: async () => (await loadSettings()).server,
    hasPermission: (server) => chrome.permissions.contains({ origins: [originPattern(server)] }),
  });
});

chrome.contextMenus.onClicked.addListener((info, tab) => {
  if (info.menuItemId !== MENU_ID) return;
  void translateSelection(tab, info.selectionText || "", info.frameId);
});

chrome.commands.onCommand.addListener((command, tab) => {
  if (command === MENU_ID) void translateSelection(tab, "");
});

// ページで選ばれている文を読む (改行を保つため、メニューの selectionText より優先する)
function readSelection() {
  const selection = getSelection();
  const text = selection ? selection.toString() : "";
  let rect = null;
  if (text && selection.rangeCount) {
    const r = selection.getRangeAt(selection.rangeCount - 1).getBoundingClientRect();
    if (r.width || r.height) rect = { top: r.top, bottom: r.bottom, left: r.left, right: r.right };
  }
  return { text, rect };
}

async function pickSelection(tabId, frameId) {
  try {
    const target = frameId === undefined ? { tabId, allFrames: true } : { tabId, frameIds: [frameId] };
    const results = await chrome.scripting.executeScript({ target, func: readSelection });
    const hit = results.find((r) => r.result?.text?.trim());
    if (!hit) return null;
    // 枠 (iframe) の中の選択の位置はページの座標と違うので使わない
    return { text: hit.result.text, rect: hit.frameId === 0 ? hit.result.rect : null };
  } catch {
    return null; // chrome:// や PDF のビューアーなどは読めない
  }
}

// ページの上に訳の枠を出す。ページの CSS が混ざらないように shadow DOM に入れる
function mountOverlay(src, rect) {
  const ID = "ptranslate-extension-overlay";
  document.getElementById(ID)?.remove();
  const host = document.createElement("div");
  host.id = ID;
  const shadow = host.attachShadow({ mode: "closed" });
  const frame = document.createElement("iframe");
  frame.src = src;
  frame.allow = "clipboard-write";
  frame.title = "PTranslate";
  const width = Math.min(480, innerWidth - 16);
  const maxHeight = Math.max(160, Math.round(innerHeight * 0.6));
  let height = 180;
  let top = 8;
  let left = innerWidth - width - 8;
  if (rect) {
    left = Math.min(Math.max(8, rect.left), innerWidth - width - 8);
    top = rect.bottom + 8 + height <= innerHeight ? rect.bottom + 8 : Math.max(8, rect.top - 8 - height);
  }
  const place = () => {
    frame.style.cssText = [
      "position:fixed", `top:${top}px`, `left:${left}px`, `width:${width}px`, `height:${height}px`,
      "border:0", "border-radius:10px", "box-shadow:0 6px 24px rgba(0,0,0,.25)", "z-index:2147483647",
      "background:transparent", "color-scheme:normal", "margin:0", "padding:0",
    ].join(";");
  };
  place();
  shadow.append(frame);
  document.documentElement.append(host);
  const close = () => {
    removeEventListener("message", onMessage, true);
    removeEventListener("mousedown", onOutside, true);
    removeEventListener("keydown", onKey, true);
    host.remove();
  };
  const onMessage = (e) => {
    if (e.source !== frame.contentWindow || typeof e.data !== "object" || !e.data) return;
    if (e.data.ptranslate === "close") close();
    if (e.data.ptranslate === "resize" && Number.isFinite(e.data.height)) {
      height = Math.min(maxHeight, Math.max(120, Math.round(e.data.height)));
      if (top + height > innerHeight - 8) top = Math.max(8, innerHeight - 8 - height);
      place();
    }
  };
  const onOutside = (e) => { if (!e.composedPath().includes(host)) close(); };
  const onKey = (e) => { if (e.key === "Escape") close(); };
  addEventListener("message", onMessage, true);
  addEventListener("mousedown", onOutside, true);
  addEventListener("keydown", onKey, true);
  frame.addEventListener("load", () => frame.focus(), { once: true });
}

async function translateSelection(tab, fallbackText, frameId) {
  const picked = tab?.id === undefined ? null : await pickSelection(tab.id, frameId);
  const text = picked?.text || fallbackText;
  if (!text.trim()) return;
  // 原文は session の保存場所に一度だけ置き、訳の画面が読んだら消す (ディスクに残さない)
  const id = crypto.randomUUID();
  await chrome.storage.session.set({ [`selection:${id}`]: text });
  const src = chrome.runtime.getURL(`popup.html#${id}`);
  if (tab?.id !== undefined) {
    try {
      await chrome.scripting.executeScript({
        target: { tabId: tab.id, frameIds: [0] },
        func: mountOverlay,
        args: [src, picked?.rect ?? null],
      });
      return;
    } catch {
      /* ページに出せないときは別ウィンドウで開く */
    }
  }
  await chrome.windows.create({ url: src, type: "popup", width: 520, height: 360, focused: true });
}
