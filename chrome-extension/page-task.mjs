// ページ全体の翻訳 (background 側)。ページの content script (page-content.js) から段落の一覧を受け取り、
// 接続先が通知する並列数に合わせて訳し、portで返す
import { splitText, translateChunk } from "./translation.mjs";
import { apiFetch, pinConnection, RelayError } from "./relay.mjs";
import { requestSettings } from "./settings.mjs";
import { ensureOk, LoginRequired } from "./translate.mjs";

export const MAX_BLOCKS = 3000;
export const PAGE_CONCURRENCY = 8;
// 古い接続先や不正な通知は1件。通知値をそのままワーカー数に使わない。
export function concurrencyLimit(config) {
  const c = config?.concurrency;
  if (!Number.isSafeInteger(c?.max_in_flight) || c.max_in_flight < 1) return 1;
  return Math.min(PAGE_CONCURRENCY, c.max_in_flight);
}
const BUSY_TIMEOUT_MS = 60_000;
const BUSY_RETRY_MS = 500;

// 同時実行枠が空くまでの待機も、port の切断ですぐ止める。
function wait(ms, signal) {
  signal.throwIfAborted();
  return new Promise((resolve, reject) => {
    const cleanup = () => signal.removeEventListener("abort", onAbort);
    const timer = setTimeout(() => { cleanup(); resolve(); }, ms);
    const onAbort = () => { clearTimeout(timer); cleanup(); reject(signal.reason); };
    signal.addEventListener("abort", onAbort, { once: true });
  });
}

// 文字種から言語を推測して省くと、中国語や ASCII の外国語を取りこぼす。
// 翻訳先と同じ言語かどうかは推測せず、数字や記号だけの箇所を除く。
export function shouldTranslate(text) {
  return /\p{L}/u.test(text);
}

// port: chrome.runtime.Port と同じ形。deps: relay と同じもの + settings(): Promise<設定>
// busyTimeoutMs・busyRetryMs は枠の解放を待つ時間と再試行間隔 (省略時 60 秒・0.5 秒から)。
export function runPageTask(port, deps) {
  const abort = new AbortController();
  let closed = false;
  let requestDeps = deps;
  let priority = new Map();
  let update = null;
  const onPriority = (msg) => {
    if (msg?.type === "update" && Array.isArray(msg.blocks)) update = msg.blocks;
    if ((msg?.type === "priority" || msg?.type === "update") && Array.isArray(msg.ids)) {
      priority = new Map(msg.ids.filter(Number.isInteger).map((id, index) => [id, index]));
    }
  };
  port.onMessage.addListener(onPriority);
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
      port.onMessage.removeListener(onPriority);
    }
  });

  async function call(route, body, id) {
    let deadline = null;
    let attempt = 0;
    try {
      for (;;) {
        abort.signal.throwIfAborted();
        const res = await apiFetch(route, body, requestDeps, abort);
        if (route !== "api/translate" || res.status !== 429) {
          return ensureOk(new Response(res.body, { status: res.status }));
        }
        // キャンセルした古い要求の枠は、サーバーが切断を検知するまで残ることがある。
        // エラー本文の接続を閉じてから、同じ区切りだけを送り直す。
        await res.body?.cancel();
        deadline ??= Date.now() + (deps.busyTimeoutMs ?? BUSY_TIMEOUT_MS);
        const remaining = deadline - Date.now();
        if (remaining <= 0) {
          throw new RelayError("busy", "翻訳サーバーが混み合っています。しばらく待ってから、もう一度試してください");
        }
        post({ type: "waiting", id, waiting: true });
        await wait(Math.min((deps.busyRetryMs ?? BUSY_RETRY_MS) * 2 ** attempt++, 5000, remaining), abort.signal);
      }
    } finally {
      if (deadline !== null) post({ type: "waiting", id, waiting: false });
    }
  }

  async function run({ blocks, visibleIds } = {}) {
    if (Array.isArray(visibleIds)) priority = new Map(visibleIds.filter(Number.isInteger).map((id, index) => [id, index]));
    if (!Array.isArray(blocks)) throw new RelayError("bad-request", "ページの文を読み取れませんでした");
    blocks = blocks.filter((b) => b && Number.isInteger(b.id) && typeof b.text === "string");
    const settings = await deps.settings();
    requestDeps = await pinConnection(deps, settings);
    const config = await (await call("api/config")).json();
    // 訳す先の自動はページ全体の文で決める
    const sample = blocks.map((b) => b.text).join("\n");
    const common = requestSettings(sample, settings, config);
    if (common.source_lang && common.source_lang === common.target_lang) {
      throw new RelayError("same-lang", "原文と訳す先が同じ言語です");
    }
    const candidates = blocks.filter((b) => shouldTranslate(b.text));
    const todo = candidates.slice(0, MAX_BLOCKS);
    const omitted = candidates.length - todo.length;
    const name = (code) => config.languages?.find((l) => l.code === code)?.name || code;
    post({ type: "start", total: todo.length, target: common.target_lang, targetName: name(common.target_lang) });
    let previous = null;
    let truncated = 0;
    let done = 0;
    let pending = [...todo];
    const completed = new Set();
    const running = new Map();
    let total = todo.length;
    let skipped = omitted;
    for (;;) {
      if (update) {
        const candidates = update.filter((b) => b && Number.isInteger(b.id) && typeof b.text === "string" &&
          shouldTranslate(b.text) && !completed.has(b.id) && !running.has(b.id));
        update = null;
        pending = candidates.slice(0, Math.max(0, MAX_BLOCKS - done - running.size));
        skipped = candidates.length - pending.length;
        total = done + running.size + pending.length;
      }
      while (pending.length && running.size < concurrencyLimit(config) && !abort.signal.aborted) {
        // 空いた枠へ、最新の表示順で次の箇所を送る。
        let nextIndex = 0;
        let bestPriority = Infinity;
        for (let i = 0; i < pending.length; i++) {
          const rank = priority.get(pending[i].id) ?? Infinity;
          if (rank < bestPriority) {
            nextIndex = i;
            bestPriority = rank;
          }
        }
        const [block] = pending.splice(nextIndex, 1);
        running.set(block.id, translateBlock(block, previous).then(
          (result) => ({ id: block.id, result }),
          (error) => ({ id: block.id, error }),
        ));
      }
      if (!running.size) break;
      const finished = await Promise.race(running.values());
      running.delete(finished.id);
      if ("error" in finished) throw finished.error;
      if (abort.signal.aborted) return;
      const result = finished.result;
      previous = result.previous;
      truncated += result.truncated;
      done++;
      completed.add(finished.id);
      post({ type: "block", id: finished.id, text: result.text, done, total });
    }
    post({ type: "done", total, truncated, omitted: skipped });

    async function translateBlock(block, previous) {
      let out = "";
      let truncated = 0;
      // 同じ箇所の分割は順番を保つ。別の箇所とは、送信時点の完了済みの訳を共有する。
      for (const chunk of splitText(block.text)) {
        if (chunk.text.trim()) {
          const source = chunk.text.trim();
          const lead = out + chunk.text.match(/^\s*/)[0];
          const res = await call("api/translate", JSON.stringify({
            ...common, text: source, history: previous ? [previous] : [],
          }), block.id);
          let buf = "";
          const finish = await translateChunk(res, (part) => {
            buf += part;
            if (!abort.signal.aborted) post({ type: "partial", id: block.id, text: (lead + buf).trim() });
          });
          if (finish === "length") truncated++;
          previous = { source, target: buf.trim() };
          out = lead + buf.trim() + chunk.text.match(/\s*$/)[0];
        }
        out += chunk.tail;
      }
      return { text: out.trim(), previous, truncated };
    }
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
