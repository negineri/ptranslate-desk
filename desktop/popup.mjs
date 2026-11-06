import { desktopFetch } from "./ipc.mjs";
import { guessTarget, parseGlossary, splitText, translateChunk } from "./translation.mjs";
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (id) => document.getElementById(id);
let controller = null, revision = null, sequence = 0;

function stop() {
  sequence++;
  controller?.abort();
  controller = null;
  revision = null;
  $("out").textContent = "";
  $("status").textContent = "";
  $("copy").disabled = true;
}

async function show() {
  const mine = ++sequence;
  const content = await invoke("popup_content");
  if (mine !== sequence || !content || content.revision === revision) return;
  controller?.abort();
  revision = content.revision;
  const abort = new AbortController();
  controller = abort;
  $("out").textContent = "";
  $("copy").disabled = true;
  $("status").textContent = "訳しています";
  $("status").className = "hint";
  const fetch = (route, body) => desktopFetch(route, {
    body: body ? JSON.stringify(body) : undefined, signal: abort.signal, popupRevision: content.revision,
  });
  try {
    const settings = await invoke("load_settings") || {};
    const response = await fetch("api/config");
    if (!response.ok) throw new Error("翻訳サーバーに接続できません");
    const config = await response.json();
    abort.signal.throwIfAborted();
    const target = !settings.target || settings.target === "auto" ? guessTarget(content.text) : settings.target;
    const name = (code) => config.languages.find((lang) => lang.code === code)?.name || code;
    $("direction").textContent = `${settings.source ? name(settings.source) : "自動で判定"} → ${name(target)}`;
    if (settings.source === target) throw new Error("原文と訳す先が同じ言語です");
    const common = {
      target_lang: target, source_lang: settings.source || null,
      form: target === "ja" ? settings.form || null : null, context: "",
      glossary: parseGlossary(settings.glossary || "", config.max_glossary),
    };
    let previous = null, truncated = 0;
    for (const chunk of splitText(content.text)) {
      abort.signal.throwIfAborted();
      if (chunk.text.trim()) {
        const source = chunk.text.trim();
        $("out").append(chunk.text.match(/^\s*/)[0]);
        const node = document.createTextNode("");
        $("out").append(node);
        const response = await fetch("api/translate", { ...common, text: source, history: previous ? [previous] : [] });
        if (!response.ok) {
          if (response.status >= 500) throw new Error("翻訳サーバーに接続できません");
          const error = await response.json();
          throw new Error(error.error || `HTTP ${response.status}`);
        }
        let text = "";
        const finish = await translateChunk(response, (part) => {
          if (abort.signal.aborted) return;
          text += part;
          node.data = text.trimStart();
          $("copy").disabled = !$("out").textContent.trim();
        });
        if (finish === "length") truncated++;
        node.data = text.trim();
        previous = { source, target: text.trim() };
        $("out").append(chunk.text.match(/\s*$/)[0]);
      }
      $("out").append(chunk.tail);
    }
    $("status").textContent = truncated ? `${truncated} か所が長すぎて途中で切れました。主窓で段落を分けて訳し直してください` : "訳し終わりました";
    $("status").className = truncated ? "hint warn" : "hint ok";
  } catch (error) {
    if (abort.signal.aborted || mine !== sequence) return;
    $("status").textContent = error.name === "AbortError" ? "中止しました" : error.message;
    $("status").className = error.name === "AbortError" ? "hint" : "hint err";
  } finally {
    if (controller === abort) controller = null;
  }
}

$("close").addEventListener("click", () => { void invoke("desktop_action", { action: "close-popup" }); });
$("open-main").addEventListener("click", () => { void invoke("desktop_action", { action: "open-main" }); });
$("copy").addEventListener("click", async () => {
  try {
    await window.__TAURI__.clipboardManager.writeText($("out").textContent);
    $("status").textContent = "訳をコピーしました";
  } catch { $("status").textContent = "訳をコピーできませんでした"; }
});
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") { event.preventDefault(); $("close").click(); }
});
await listen("popup-content", () => { void show(); });
await listen("popup-hidden", stop);
await listen("session-changed", async () => {
  if (!(await invoke("session_status")).logged_in) stop();
});
await show();
