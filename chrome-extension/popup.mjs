import { sessionFetch } from "./transport.mjs";
import { loadSettings, requestSettings, volunteerFields } from "./settings.mjs";
import { doneMessage, errorMessage, loadConfig, remedy, translateInto } from "./translate.mjs";

const $ = (id) => document.getElementById(id);
// ページの上の枠 (iframe) で開いたか、別ウィンドウで開いたか
const embedded = window.parent !== window;
const abort = new AbortController();
let server = "";

function setStatus({ text, cls = "" }) {
  $("status").textContent = text;
  $("status").className = "hint " + cls;
}

function close() {
  abort.abort();
  if (embedded) window.parent.postMessage({ ptranslate: "close" }, "*");
  else window.close();
}

// 枠の高さを訳の長さに合わせる (上限はページ側で決める)。訳の中身は送らない
let lastHeight = 0;
function reportHeight() {
  if (!embedded) return;
  requestAnimationFrame(() => {
    const main = document.querySelector("main").getBoundingClientRect().height;
    const out = $("out");
    const style = getComputedStyle(out);
    const padding = [style.paddingTop, style.paddingBottom, style.borderTopWidth, style.borderBottomWidth]
      .reduce((sum, v) => sum + parseFloat(v), 0);
    const height = Math.ceil(main - out.getBoundingClientRect().height + Math.max(48, $("translation").getBoundingClientRect().height) + padding);
    if (height === lastHeight) return;
    lastHeight = height;
    window.parent.postMessage({ ptranslate: "resize", height }, "*");
  });
}
new ResizeObserver(reportHeight).observe($("translation"));
new ResizeObserver(reportHeight).observe($("status"));

async function takeText() {
  const id = location.hash.slice(1);
  if (!/^[0-9a-f-]{36}$/.test(id)) return "";
  const key = `selection:${id}`;
  const stored = await chrome.storage.session.get(key);
  await chrome.storage.session.remove(key);
  return stored[key];
}

async function main() {
  const selection = await takeText();
  const text = typeof selection === "string" ? selection : selection?.text || "";
  if (!text.trim()) {
    setStatus({ text: "訳す文が見つかりませんでした。文を選んでからもう一度試してください", cls: "err" });
    return;
  }
  const settings = await loadSettings();
  server = settings.server;
  const fetch = sessionFetch(settings);
  setStatus({ text: "訳しています" });
  try {
    const config = await loadConfig({ signal: abort.signal, fetch });
    const blocked = selection?.incognito || chrome.extension?.inIncognitoContext
      ? "シークレットウィンドウではボランティア枠を使えません"
      : selection?.safe !== true ? "編集欄の外で選んだ文だけをボランティア枠へ送れます" : "";
    const consent = await volunteerFields(settings, config, blocked);
    const common = { ...requestSettings(text, settings, config), ...consent };
    const name = (code) => config.languages?.find((l) => l.code === code)?.name || code;
    $("direction").textContent = `${consent.consent ? "ボランティア枠 · " : ""}${common.source_lang ? name(common.source_lang) : "自動で判定"} → ${name(common.target_lang)}`;
    if (common.source_lang && common.source_lang === common.target_lang) throw new Error("原文と訳す先が同じ言語です");
    const { truncated } = await translateInto($("translation"), text, common, {
      signal: abort.signal, fetch,
      onText: () => { $("copy").disabled = !$("translation").textContent.trim(); },
    });
    $("copy").disabled = !$("translation").textContent.trim();
    setStatus(doneMessage(truncated));
  } catch (error) {
    if (abort.signal.aborted && error?.name !== "AbortError") return;
    setStatus(errorMessage(error));
    const fix = remedy(error);
    $("login").hidden = !(fix === "login" && server);
    $("options").hidden = fix !== "options";
  }
}

$("close").addEventListener("click", close);
$("copy").addEventListener("click", async () => {
  try {
    await navigator.clipboard.writeText($("translation").textContent);
    setStatus({ text: "訳をコピーしました", cls: "ok" });
  } catch {
    setStatus({ text: "訳をコピーできませんでした", cls: "err" });
  }
});
$("login").addEventListener("click", () => { void chrome.tabs.create({ url: `${server}/` }); close(); });
$("options").addEventListener("click", () => { void chrome.runtime.openOptionsPage(); close(); });
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") { e.preventDefault(); close(); }
});
if (embedded) window.parent.postMessage({ ptranslate: "ready" }, "*");
main().catch((e) => setStatus(errorMessage(e)));
