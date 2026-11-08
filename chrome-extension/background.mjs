import { mountOverlay } from "./overlay.mjs";
import { runPageTask } from "./page-task.mjs";
import { isExtensionPage, relay } from "./relay.mjs";
import { loadSettings, originPattern } from "./settings.mjs";

const MENU_ID = "translate-selection";
const PAGE_MENU_ID = "translate-page";
const EXTENSION_ROOT = chrome.runtime.getURL("");

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({ id: MENU_ID, title: "PTranslate で翻訳", contexts: ["selection"] });
  chrome.contextMenus.create({ id: PAGE_MENU_ID, title: "PTranslate でこのページを翻訳", contexts: ["page"] });
  chrome.storage.session.setAccessLevel?.({ accessLevel: "TRUSTED_CONTEXTS" });
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
    return runPageTask(port, apiDeps);
  }
  port.disconnect();
});

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (sender.id !== chrome.runtime.id) return;
  // ツールバーの画面の「このページを翻訳」
  if (msg?.type === "translate-page" && isExtensionPage(sender, EXTENSION_ROOT) && Number.isInteger(msg.tabId)) {
    translatePage(msg.tabId).then(sendResponse);
    return true;
  }
  // ページのバーの「Web版を開いてログイン」「拡張の設定」
  if (msg?.type === "open" && sender.tab) {
    if (msg.what === "options") void chrome.runtime.openOptionsPage();
    if (msg.what === "login") {
      void loadSettings().then(({ server }) => server && chrome.tabs.create({ url: `${server}/`, index: sender.tab.index + 1 }));
    }
  }
});

chrome.contextMenus.onClicked.addListener((info, tab) => {
  if (info.menuItemId === MENU_ID) void translateSelection(tab, info.selectionText || "", info.frameId);
  if (info.menuItemId === PAGE_MENU_ID && tab?.id !== undefined) void translatePage(tab.id);
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
