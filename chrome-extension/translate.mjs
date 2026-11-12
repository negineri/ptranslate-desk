// 拡張の画面で共通の翻訳の流れ。区切り・SSE の読み取りは Web と同じ translation.mjs を使う
import { splitText, translateChunk } from "./translation.mjs";
import { extensionFetch } from "./transport.mjs";

export class LoginRequired extends Error {}

// 応答の状態を画面の言葉にする (SPEC 5.2)。本番の 403 は HTML なので状態だけを見る
export async function ensureOk(res) {
  if (res.ok) return res;
  if (res.status === 403) {
    const error = await res.json().catch(() => null);
    if (error?.code === "consent_required") {
      throw Object.assign(new Error("設定でボランティア枠への送信に同意してください"), { code: error.code });
    }
  }
  if (res.status === 401 || res.status === 403) {
    throw new LoginRequired("ログインしていません。Web版でログインしてから、もう一度試してください");
  }
  if (res.status >= 500) throw new Error("翻訳サーバーに接続できません");
  const e = await res.json().catch(() => ({}));
  throw new Error(e.error || `エラー (${res.status})`);
}

export async function loadConfig({ signal, fetch = extensionFetch } = {}) {
  const res = await ensureOk(await fetch("api/config", { signal }));
  return res.json();
}

// text を区切りごとに訳して el に流し込む。区切りの間の空白はそのまま残す
export async function translateInto(el, text, common, { signal, fetch = extensionFetch, onText } = {}) {
  let previous = null;
  let truncated = 0;
  el.textContent = "";
  for (const chunk of splitText(text)) {
    signal?.throwIfAborted();
    if (chunk.text.trim()) {
      const source = chunk.text.trim();
      el.append(chunk.text.match(/^\s*/)[0]);
      const node = document.createTextNode("");
      el.append(node);
      const res = await ensureOk(
        await fetch("api/translate", {
          body: JSON.stringify({ ...common, purpose: "interactive", text: source, history: previous ? [previous] : [] }),
          signal,
        }),
      );
      let buf = "";
      const finish = await translateChunk(res, (part) => {
        if (signal?.aborted) return;
        buf += part;
        node.data = buf.trimStart();
        onText?.();
      });
      if (finish === "length") truncated++;
      node.data = buf.trim();
      previous = { source, target: buf.trim() };
      el.append(chunk.text.match(/\s*$/)[0]);
    }
    el.append(chunk.tail);
  }
  return { truncated };
}

export function doneMessage(truncated) {
  return truncated
    ? { text: `${truncated} か所が長すぎて途中で切れました。段落を分けて訳し直してください`, cls: "warn" }
    : { text: "訳し終わりました", cls: "ok" };
}

export function errorMessage(error) {
  if (error?.name === "AbortError") return { text: "中止しました", cls: "" };
  return { text: error?.message || String(error), cls: "err" };
}

// ログインや設定が要るときの手当てのボタン
export function remedy(error) {
  if (error instanceof LoginRequired) return "login";
  if (error?.code === "consent_required" || error?.code === "local" || error?.code === "no-server" || error?.code === "no-permission") return "options";
  return null;
}
