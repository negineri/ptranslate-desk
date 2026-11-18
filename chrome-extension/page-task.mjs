// ページ全体の翻訳 (background 側)。ページの content script (page-content.js) から段落の一覧を受け取り、
// 接続先が通知する並列数に合わせて訳し、portで返す
import { MAX_IN_FLIGHT } from "./scheduler.mjs";
import { splitText, translateChunk } from "./translation.mjs";
import { apiFetch, pinConnection, RelayError } from "./relay.mjs";
import { requestSettings, volunteerFields } from "./settings.mjs";
import { ensureOk, LoginRequired } from "./translate.mjs";

export const MAX_BLOCKS = 3000;
export const PAGE_CONCURRENCY = MAX_IN_FLIGHT;
// 古い接続先や不正な通知は1件。通知値をそのままワーカー数に使わない。
export function concurrencyLimit(config) {
  const c = config?.concurrency;
  if (!Number.isSafeInteger(c?.max_in_flight) || c.max_in_flight < 1) return 1;
  return Math.min(PAGE_CONCURRENCY, c.max_in_flight);
}
const MAIN_TEXT_LENGTH = 300;
const SHORT_TEXT_LENGTH = 60;
const BATCH_SIZE = 4;

function numberedTranslations(text, count) {
  const lines = text.trim().split(/\r?\n/);
  if (lines.length !== count) return null;
  const translations = lines.map((line, i) =>
    line.match(new RegExp(`^\\s*${i + 1}[.．、]\\s*(\\S.*?)\\s*$`))?.[1]);
  return translations.every(Boolean) ? translations : null;
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
  let takeSlot;
  let closeAdmission;
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
      await closeAdmission?.();
    }
  });

  async function call(route, body, id) {
    let deadline = null;
    let attempt = 0;
    try {
      if (takeSlot && route === "api/translate") {
        const input = JSON.parse(body);
        const until = Date.now() + (deps.busyTimeoutMs ?? BUSY_TIMEOUT_MS);
        for (;;) {
          const page_slot = await takeSlot(id);
          input.purpose = priority.has(id) ? "page_visible" : "page_rest";
          const res = await apiFetch(route, JSON.stringify({ ...input, page_slot }), requestDeps, abort, { pageAdmission: true });
          const response = new Response(res.body, { status: res.status });
          if (response.status !== 429) return ensureOk(response);
          const error = await response.json().catch(() => ({}));
          if (error.code !== "page_slot" || Date.now() >= until) {
            throw new RelayError(error.code ?? "busy", error.error ?? "翻訳サーバーが混み合っています");
          }
        }
      }
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
    // ページのURLや本文を使わず、翻訳を開始するたびに新しい値を作る。
    const groupId = btoa(String.fromCharCode(...crypto.getRandomValues(new Uint8Array(16))))
      .replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
    const pageFields = (id) => ({
      group: groupId,
      purpose: priority.has(id) ? "page_visible" : "page_rest",
    });
    // 訳す先の自動はページ全体の文で決める
    const sample = blocks.map((b) => b.text).join("\n");
    const consent = await volunteerFields(settings, config,
      deps.incognito ? "シークレットウィンドウではボランティア枠を使えません" : "");
    const common = { ...requestSettings(sample, settings, config), ...consent };
    const adaptive = config.pool?.page_slots === true && Boolean(consent.consent);
    if (adaptive) {
      const waiting = [];
      let polling = false;
      let requested = false;
      const requestSlots = async (count, controller = abort) => {
        requested = true;
        const res = await apiFetch("api/page-slots", JSON.stringify({ group: groupId, count, ...consent }),
          { ...requestDeps, timeoutMs: 5000 }, controller, { pageAdmission: true });
        const response = await ensureOk(new Response(res.body, { status: res.status }));
        return response.json();
      };
      closeAdmission = async () => {
        if (requested) await requestSlots(0, new AbortController()).catch(() => {});
      };
      async function poll() {
        polling = true;
        let waitingSince = Date.now();
        try {
          while (waiting.length) {
            const grant = await requestSlots(waiting.length);
            abort.signal.throwIfAborted();
            if (!Number.isSafeInteger(grant.count) || grant.count < 0 || grant.count > waiting.length ||
                (grant.count && !/^[A-Za-z0-9_-]{22,128}$/.test(grant.token ?? ""))) {
              throw new RelayError("bad-response", "ページの送信枠を読み取れませんでした");
            }
            if (grant.count) {
              waiting.sort((a, b) => (priority.get(a.id) ?? Infinity) - (priority.get(b.id) ?? Infinity));
              for (const entry of waiting.splice(0, grant.count)) entry.resolve(grant.token);
              waitingSince = Date.now();
            } else {
              if (Date.now() - waitingSince >= (deps.busyTimeoutMs ?? 120_000)) {
                throw new RelayError("busy", "翻訳ワーカーの空きを待ちましたが、翻訳を開始できませんでした");
              }
              post({ type: "waiting", id: -1, waiting: true });
            }
            if (waiting.length) await wait(deps.busyRetryMs ?? 500, abort.signal);
          }
        } catch (error) {
          for (const entry of waiting.splice(0)) entry.reject(error);
        } finally {
          polling = false;
          post({ type: "waiting", id: -1, waiting: false });
        }
      }
      takeSlot = (id) => {
        abort.signal.throwIfAborted();
        return new Promise((resolve, reject) => {
          waiting.push({ id, resolve, reject });
          if (!polling) queueMicrotask(() => { if (!polling && waiting.length) void poll(); });
        });
      };
    }
    if (common.source_lang && common.source_lang === common.target_lang) {
      throw new RelayError("same-lang", "原文と訳す先が同じ言語です");
    }
    const candidates = blocks.filter((b) => shouldTranslate(b.text));
    const todo = candidates.slice(0, MAX_BLOCKS);
    const omitted = candidates.length - todo.length;
    const name = (code) => config.languages?.find((l) => l.code === code)?.name || code;
    post({ type: "start", total: todo.length, target: common.target_lang, targetName: name(common.target_lang), ...(consent.consent ? { volunteer: true } : {}) });
    let previous = null;
    let truncated = 0;
    let done = 0;
    let pending = [...todo];
    const completed = new Set();
    const running = new Set();
    const finishedGroups = [];
    let groupFinished;
    const finishGroup = (result) => {
      finishedGroups.push(result);
      groupFinished?.();
    };
    const runningIds = new Set();
    const batchable = (block) => !priority.has(block.id) &&
      block.text.length <= SHORT_TEXT_LENGTH && !/[\r\n]/.test(block.text) &&
      !/^\s*\d/.test(block.text);
    const batchEnabled = !adaptive && Array.isArray(visibleIds) && common.target_lang === "ja" &&
      (!common.source_lang || common.source_lang === "en");
    let total = todo.length;
    let skipped = omitted;
    for (;;) {
      if (update) {
        const candidates = update.filter((b) => b && Number.isInteger(b.id) && typeof b.text === "string" &&
          shouldTranslate(b.text) && !completed.has(b.id) && !runningIds.has(b.id));
        update = null;
        pending = candidates.slice(0, Math.max(0, MAX_BLOCKS - done - runningIds.size));
        skipped = candidates.length - pending.length;
        total = done + runningIds.size + pending.length;
      }
      while (pending.length && running.size < (adaptive ? MAX_BLOCKS : concurrencyLimit(config)) && !abort.signal.aborted) {
        // 画面内を優先し、それぞれの領域で長文を先に送る。
        let nextIndex = 0;
        let bestPriority = Infinity;
        for (let i = 0; i < pending.length; i++) {
          const block = pending[i];
          const visible = priority.has(block.id);
          const long = block.text.length >= MAIN_TEXT_LENGTH;
          const tier = (visible ? 0 : 2) + (long ? 0 : 1);
          const rank = tier * (priority.size + 1) + (priority.get(block.id) ?? 0);
          if (rank < bestPriority) {
            nextIndex = i;
            bestPriority = rank;
          }
        }
        const [block] = pending.splice(nextIndex, 1);
        const group = [block];
        if (batchEnabled && batchable(block)) {
          const indices = [];
          for (let i = nextIndex; i < pending.length && indices.length < BATCH_SIZE - 1; i++) {
            if (batchable(pending[i])) indices.push(i);
          }
          if (indices.length === BATCH_SIZE - 1) {
            group.push(...indices.map((index) => pending[index]));
            for (const index of indices.reverse()) pending.splice(index, 1);
          }
        }
        for (const member of group) runningIds.add(member.id);
        running.add(block.id);
        void translateGroup(group, previous).then(
          (results) => finishGroup({ group, results }),
          (error) => finishGroup({ group, error }),
        );
      }
      if (!running.size) break;
      if (!finishedGroups.length) {
        await new Promise((resolve) => { groupFinished = resolve; });
        groupFinished = undefined;
      }
      const finished = finishedGroups.shift();
      running.delete(finished.group[0].id);
      if ("error" in finished) throw finished.error;
      if (abort.signal.aborted) return;
      for (const [index, block] of finished.group.entries()) {
        const result = finished.results[index];
        runningIds.delete(block.id);
        previous = result.previous;
        truncated += result.truncated;
        done++;
        completed.add(block.id);
        post({ type: "block", id: block.id, text: result.text, done, total });
      }
    }
    post({ type: "done", total, truncated, omitted: skipped });

    async function translateGroup(group, previous) {
      if (group.length > 1) {
        const text = group.map((block, i) => `${i + 1}. ${block.text}`).join("\n");
        const res = await call("api/translate", JSON.stringify({ ...common, ...pageFields(group[0].id), text, history: [] }), group[0].id);
        let output = "";
        const finish = await translateChunk(res, (part) => { output += part; });
        const translations = finish === "stop" && numberedTranslations(output, group.length);
        if (translations) return translations.map((text, i) => ({
          text, previous: { source: group[i].text, target: text }, truncated: 0,
        }));
      }
      const results = [];
      for (const block of group) {
        const result = await translateBlock(block, previous);
        previous = result.previous;
        results.push(result);
      }
      return results;
    }

    async function translateBlock(block, previous) {
      let out = "";
      let truncated = 0;
      // 同じ箇所の分割は順番を保つ。別の箇所とは、送信時点の完了済みの訳を共有する。
      for (const chunk of splitText(block.text)) {
        if (chunk.text.trim()) {
          const source = chunk.text.trim();
          const lead = out + chunk.text.match(/^\s*/)[0];
          const res = await call("api/translate", JSON.stringify({
            ...common, ...pageFields(block.id), text: source, history: previous ? [previous] : [],
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
  if (e?.code === "consent_required") return { code: e.code, message: e.message, remedy: "options" };
  if (e instanceof LoginRequired) return { code: "login", message: e.message };
  if (e instanceof RelayError) {
    const remedy = e.code === "no-server" || e.code === "no-permission" ? "options" : undefined;
    return { code: e.code, message: e.message, remedy };
  }
  if (e instanceof TypeError || !e?.message) return { code: "network", message: "翻訳サーバーに接続できません" };
  return { code: "server", message: e.message };
}
