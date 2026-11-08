import { sessionFetch } from "./transport.mjs";
import { loadSettings, requestSettings, saveSettings } from "./settings.mjs";
import { doneMessage, errorMessage, loadConfig, remedy, translateInto } from "./translate.mjs";

const $ = (id) => document.getElementById(id);
let settings = null;
let config = { languages: [] };
let controller = null;

function setStatus({ text, cls = "" }) {
  $("status").textContent = text;
  $("status").className = "hint " + cls;
}

function showRemedy(error) {
  const fix = remedy(error);
  $("login").hidden = fix !== "login";
  if (fix === "options") $("setup").hidden = false;
}

async function run() {
  const text = $("src").value;
  if (!text.trim()) { $("src").focus(); return; }
  controller?.abort();
  const mine = new AbortController();
  controller = mine;
  $("stop").hidden = false;
  $("login").hidden = true;
  $("copy").disabled = true;
  setStatus({ text: "訳しています" });
  try {
    const fetch = sessionFetch(settings);
    const config = await loadConfig({ signal: mine.signal, fetch });
    const common = requestSettings(text, settings, config);
    if (common.source_lang && common.source_lang === common.target_lang) throw new Error("原文と訳す先が同じ言語です");
    const { truncated } = await translateInto($("out"), text, common, { signal: mine.signal, fetch });
    setStatus(doneMessage(truncated));
  } catch (error) {
    if (mine.signal.reason === "superseded") return;
    setStatus(errorMessage(mine.signal.aborted ? new DOMException("", "AbortError") : error));
    showRemedy(error);
  } finally {
    if (controller === mine) { controller = null; $("stop").hidden = true; }
    $("copy").disabled = !$("out").textContent.trim();
  }
}

async function init() {
  settings = await loadSettings();
  if (settings.mode !== "local" && !settings.server) { $("setup").hidden = false; return; }
  $("translator").hidden = false;
  $("open-web").hidden = settings.mode === "local";
  $("src").focus();
  const [command] = (await chrome.commands.getAll()).filter((c) => c.name === "translate-selection");
  $("shortcut").textContent = command?.shortcut
    ? `ショートカットは ${command.shortcut} です。`
    : "ショートカットは拡張の設定から割り当てられます。";
  try {
    config = await loadConfig({ fetch: sessionFetch(settings) });
    for (const l of config.languages || []) $("target").append(new Option(l.name, l.code));
    if ([...$("target").options].some((o) => o.value === settings.target)) $("target").value = settings.target;
  } catch (error) {
    setStatus(errorMessage(error));
    showRemedy(error);
  }
}

$("translate-page").addEventListener("click", async () => {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  const res = tab?.id === undefined ? null : await chrome.runtime.sendMessage({ type: "translate-page", tabId: tab.id });
  if (res?.ok) window.close();
  else {
    $("page-status").textContent = res?.message || "このページは翻訳できません";
    $("page-status").className = "hint err";
  }
});
$("go").addEventListener("click", () => void run());
$("stop").addEventListener("click", () => controller?.abort());
$("src").addEventListener("keydown", (e) => {
  if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); void run(); }
});
$("target").addEventListener("change", async () => {
  settings = await saveSettings({ target: $("target").value });
});
$("copy").addEventListener("click", async () => {
  try {
    await navigator.clipboard.writeText($("out").textContent);
    setStatus({ text: "訳をコピーしました", cls: "ok" });
  } catch {
    setStatus({ text: "訳をコピーできませんでした", cls: "err" });
  }
});
const openWeb = () => { void chrome.tabs.create({ url: `${settings.server}/` }); window.close(); };
$("open-web").addEventListener("click", openWeb);
$("login").addEventListener("click", openWeb);
for (const id of ["open-options", "setup-options"]) {
  $(id).addEventListener("click", () => { void chrome.runtime.openOptionsPage(); window.close(); });
}
init().catch((e) => setStatus(errorMessage(e)));
