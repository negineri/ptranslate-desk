import { mountOverlay } from "./overlay.mjs";
import { runPageTask } from "./page-task.mjs";
import { apiFetch, isExtensionPage, pinConnection, relay } from "./relay.mjs";
import { connectionKey, loadSettings, originPattern } from "./settings.mjs";

const MENU_ID = "translate-selection";
const PAGE_MENU_ID = "translate-page";
const SETTINGS_MENU_ID = "settings";
const EXTENSION_ROOT = chrome.runtime.getURL("");

chrome.runtime.onInstalled.addListener(async ({ reason }) => {
  await chrome.contextMenus.removeAll();
  chrome.contextMenus.create({ id: MENU_ID, title: "PTranslate で翻訳", contexts: ["selection"] });
  chrome.contextMenus.create({ id: PAGE_MENU_ID, title: "PTranslate でこのページを翻訳", contexts: ["page"] });
  chrome.contextMenus.create({ id: SETTINGS_MENU_ID, title: "設定", contexts: ["action"] });
  await chrome.storage.session.setAccessLevel?.({ accessLevel: "TRUSTED_CONTEXTS" });
  if (reason === "install") await chrome.runtime.openOptionsPage();
});

const apiDeps = {
  fetch: (...args) => fetch(...args),
  server: async () => (await loadSettings()).server,
  hasPermission: (server) => chrome.permissions.contains({ origins: [originPattern(server)] }),
  settings: loadSettings,
};

chrome.runtime.onConnect.addListener((port) => {
  const sender = port.sender;
  // 拡張の画面からの API 呼び出し (transport.mjs)
  if (port.name === "api" && isExtensionPage(sender, EXTENSION_ROOT)) return relay(port, apiDeps);
  // ページ全体の翻訳 (page-content.js)。拡張が注入した、タブの一番上の枠の content script だけ受ける
  if (port.name === "page-translate" && sender?.id === chrome.runtime.id && sender.tab && sender.frameId === 0) {
    return runPageTask(port, { ...apiDeps, incognito: sender.tab.incognito || chrome.extension?.inIncognitoContext });
  }
  port.disconnect();
});

chrome.runtime.onMessage.addListener((msg, sender) => {
  if (sender.id !== chrome.runtime.id) return;
  // ページのバーの「Web版を開いてログイン」「設定」
  if (msg?.type === "open" && sender.tab) {
    if (msg.what === "options") void chrome.runtime.openOptionsPage();
    if (msg.what === "login") {
      void loadSettings().then(({ server }) => server && chrome.tabs.create({ url: `${server}/`, index: sender.tab.index + 1 }));
    }
  }
});

chrome.contextMenus.onClicked.addListener((info, tab) => {
  if (info.menuItemId === SETTINGS_MENU_ID) void chrome.runtime.openOptionsPage();
  if (info.menuItemId === MENU_ID) void translateSelection(tab, info.selectionText || "", info.frameId);
  if (info.menuItemId === PAGE_MENU_ID && tab?.id !== undefined) void translatePage(tab.id);
});

// ツールバーは入力画面を開かず、そのページを翻訳する。
chrome.action.onClicked.addListener(async (tab) => {
  if (tab?.id === undefined) return;
  const settings = await loadSettings();
  if (settings.mode !== "local" && !settings.server) return chrome.runtime.openOptionsPage();
  const abort = new AbortController();
  const timer = setTimeout(() => abort.abort(), 15000);
  try {
    const key = connectionKey(settings);
    const connected = await chrome.storage.session.get("connectedTo");
    if (connected.connectedTo !== key) {
      const deps = await pinConnection(apiDeps, settings);
      const res = await apiFetch("api/config", null, deps, abort);
      if (res.status !== 200) { await res.body?.cancel(); throw new Error("接続してください"); }
      await new Response(res.body).json();
      await chrome.storage.session.set({ connectedTo: key });
    }
  } catch {
    await chrome.storage.session.remove("connectedTo");
    await chrome.runtime.openOptionsPage();
    return;
  } finally { clearTimeout(timer); abort.abort(); }
  const result = await translatePage(tab.id);
  await chrome.action.setBadgeText({ tabId: tab.id, text: result.ok ? "" : "!" });
  await chrome.action.setTitle({ tabId: tab.id, title: result.ok ? "PTranslate でこのページを翻訳" : result.message });
});

chrome.commands.onCommand.addListener((command, tab) => {
  if (command === MENU_ID) void translateSelection(tab, "");
  if (command === PAGE_MENU_ID && tab?.id !== undefined) void translatePage(tab.id);
});

// ページ全体を訳す。もう一度呼ぶと、訳した後なら進み具合のバーを出し直す
async function translatePage(tabId) {
  try {
    await chrome.scripting.executeScript({ target: { tabId, frameIds: [0] }, files: ["page-content.js"] });
    return { ok: true };
  } catch {
    return { ok: false, message: "このページは翻訳できません (ブラウザーの画面・ストア・PDF など)" };
  }
}

// ページで選ばれている文を読む (改行を保つため、メニューの selectionText より優先する)
function readSelection() {
  const selection = getSelection();
  const text = selection ? selection.toString() : "";
  let rect = null;
  if (text && selection.rangeCount) {
    const r = selection.getRangeAt(selection.rangeCount - 1).getBoundingClientRect();
    if (r.width || r.height) rect = { top: r.top, bottom: r.bottom, left: r.left, right: r.right };
  }
  const editable = "input, textarea, select, [contenteditable]";
  const range = selection?.rangeCount === 1 ? selection.getRangeAt(0) : null;
  const node = range?.commonAncestorContainer;
  const insideEditor = (start) => {
    for (let current = start; current; current = current.assignedSlot || current.parentElement || current.getRootNode().host) {
      if (current.matches?.(editable)) return true;
    }
    return false;
  };
  let safe = Boolean(range && document.designMode !== "on" && node.getRootNode() === document &&
    range.toString() === text && !insideEditor(node));
  if (safe) {
    // 選択範囲が編集欄を横切る場合や、選んだ文が編集欄にスロットで入る場合も除く。
    const walker = document.createTreeWalker(node, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT, {
      acceptNode(current) {
        if (!range.intersectsNode(current)) return NodeFilter.FILTER_REJECT;
        return current.matches?.(editable) || insideEditor(current.assignedSlot)
          ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_SKIP;
      },
    });
    safe = !walker.nextNode();
  }
  return { text, rect, safe };
}

async function pickSelection(tabId, frameId) {
  try {
    const target = frameId === undefined ? { tabId, allFrames: true } : { tabId, frameIds: [frameId] };
    const results = await chrome.scripting.executeScript({ target, func: readSelection });
    const hit = results.find((r) => r.result?.text?.trim());
    if (!hit) return null;
    // 枠 (iframe) の中の選択の位置はページの座標と違うので使わない
    return { text: hit.result.text, safe: hit.result.safe === true, rect: hit.frameId === 0 ? hit.result.rect : null };
  } catch {
    return null; // chrome:// や PDF のビューアーなどは読めない
  }
}

async function translateSelection(tab, fallbackText, frameId) {
  const picked = tab?.id === undefined ? null : await pickSelection(tab.id, frameId);
  const text = picked?.text || fallbackText;
  if (!text.trim()) return;
  // 原文は session の保存場所に一度だけ置き、訳の画面が読んだら消す (ディスクに残さない)
  const id = crypto.randomUUID();
  await chrome.storage.session.set({ [`selection:${id}`]: {
    text, safe: picked?.safe === true, incognito: Boolean(tab?.incognito || chrome.extension?.inIncognitoContext),
  } });
  const src = chrome.runtime.getURL(`popup.html#${id}`);
  if (tab?.id !== undefined) {
    try {
      const results = await chrome.scripting.executeScript({
        target: { tabId: tab.id, frameIds: [0] },
        func: mountOverlay,
        args: [src, picked?.rect ?? null],
      });
      if (results[0]?.result !== "blocked") return;
    } catch {
      /* ページに出せないときは別ウィンドウで開く */
    }
  }
  await chrome.windows.create({ url: src, type: "popup", width: 520, height: 360, focused: true });
}
