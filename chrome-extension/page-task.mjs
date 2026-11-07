// ページ全体の翻訳 (background 側)。ページの content script (page-content.js) から段落の一覧を受け取り、
// 1 段落ずつ順に訳して port で返す。サーバーの同時の上限 (1 人 2 本) を空けておくため、同時には 1 本だけ送る
import { guessTarget, splitText, translateChunk } from "./translation.mjs";
import { apiFetch, RelayError } from "./relay.mjs";
import { requestSettings } from "./settings.mjs";
import { ensureOk, LoginRequired } from "./translate.mjs";

export const MAX_BLOCKS = 3000;

// 訳す先の言語ですでに書かれている段落は送らない
export function shouldTranslate(text, target) {
  if (!/\p{L}/u.test(text)) return false; // 数字や記号だけ
  if (target === "ja") return guessTarget(text) !== "en"; // 日本語の段落 (かな・漢字が多い) は訳さない
  if (target === "en") return !/^[\x00-\x7F]*$/.test(text); // ASCII だけの段落は英語とみなす
  return true;
}

// port: chrome.runtime.Port と同じ形。deps: relay と同じもの + settings(): Promise<設定>
export function runPageTask(port, deps) {
  const abort = new AbortController();
  let closed = false;
  const post = (msg) => {
    if (closed) return;
    try {
      port.postMessage(msg);
    } catch {
      abort.abort();
    }
  };
  port.onDisconnect.addListener(() => {
    closed = true;
    abort.abort();
  });
  port.onMessage.addListener(async function onStart(msg) {
    port.onMessage.removeListener(onStart);
    try {
      await run(msg);
    } catch (e) {
      if (abort.signal.aborted && !(e instanceof RelayError)) return;
      post({ type: "error", ...describe(e) });
    } finally {
      // 読まずに終わった応答の接続も閉じる。port はページの側が閉じる
      abort.abort();
    }
  });

  async function call(route, body) {
    const res = await apiFetch(route, body, deps, abort);
    return ensureOk(new Response(res.body, { status: res.status }));
  }

  async function run({ blocks } = {}) {
    if (!Array.isArray(blocks)) throw new RelayError("bad-request", "ページの文を読み取れませんでした");
    blocks = blocks
      .filter((b) => b && Number.isInteger(b.id) && typeof b.text === "string")
      .slice(0, MAX_BLOCKS);
    const settings = await deps.settings();
    const config = await (await call("api/config")).json();
    // 訳す先の自動はページ全体の文で決める
    const sample = blocks.map((b) => b.text).join("\n");
    const common = requestSettings(sample, settings, config);
    if (common.source_lang && common.source_lang === common.target_lang) {
      throw new RelayError("same-lang", "原文と訳す先が同じ言語です");
    }
    const todo = blocks.filter((b) => shouldTranslate(b.text, common.target_lang));
    const name = (code) => config.languages?.find((l) => l.code === code)?.name || code;
    post({ type: "start", total: todo.length, target: common.target_lang, targetName: name(common.target_lang) });
    let previous = null;
    let truncated = 0;
    let done = 0;
    for (const block of todo) {
      let out = "";
      for (const chunk of splitText(block.text)) {
        if (chunk.text.trim()) {
          const source = chunk.text.trim();
          const lead = out + chunk.text.match(/^\s*/)[0];
          const res = await call("api/translate", JSON.stringify({
            ...common, text: source, history: previous ? [previous] : [],
          }));
          let buf = "";
          const finish = await translateChunk(res, (part) => {
            buf += part;
            post({ type: "partial", id: block.id, text: (lead + buf).trim() });
          });
          if (finish === "length") truncated++;
          previous = { source, target: buf.trim() };
          out = lead + buf.trim() + chunk.text.match(/\s*$/)[0];
        }
        out += chunk.tail;
      }
      done++;
      post({ type: "block", id: block.id, text: out.trim(), done, total: todo.length });
    }
    post({ type: "done", total: todo.length, truncated });
  }
}

// 失敗を画面に出す言葉と、手当ての種類にする
export function describe(e) {
  if (e instanceof LoginRequired) return { code: "login", message: e.message };
  if (e instanceof RelayError) {
    const remedy = e.code === "no-server" || e.code === "no-permission" ? "options" : undefined;
    return { code: e.code, message: e.message, remedy };
  }
  if (e instanceof TypeError || !e?.message) return { code: "network", message: "翻訳サーバーに接続できません" };
  return { code: "server", message: e.message };
}
