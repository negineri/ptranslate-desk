import { nativeFetch } from "./native.mjs";
import { Scheduler } from "./scheduler.mjs";
const scheduler = new Scheduler();
const limits = new Map();

// background で API を呼び、応答を拡張の画面へ port で流す。
// 画面 (popup.html など) は Web ページの中の iframe でも動くので、Cookie を確実に付けられる
// background からだけサーバーへつなぐ。デスクトップの start_request と同じく 2 つの API だけ通す
export const ROUTES = Object.freeze({ "api/config": "GET", "api/translate": "POST" });
export const READ_TIMEOUT_MS = 120_000;

export class RelayError extends Error {
  constructor(code, message) {
    super(message);
    this.code = code;
  }
}

// 送り手が拡張自身の画面か (内容スクリプトや Web ページからは受けない)。
// root は chrome.runtime.getURL("") ("chrome-extension://<id>/")
export function isExtensionPage(sender, root) {
  return typeof sender?.url === "string" && root.endsWith("/") && sender.url.startsWith(root);
}

// port: { postMessage, onMessage, onDisconnect } (chrome.runtime.Port と同じ形)
// deps: { fetch, server(): Promise<string>, hasPermission(server): Promise<boolean>, timeoutMs }
export function relay(port, deps) {
  const abort = new AbortController();
  let done = false;
  const post = (msg) => {
    if (done) return;
    try {
      port.postMessage(msg);
    } catch {
      abort.abort();
    }
  };
  port.onDisconnect.addListener(() => {
    done = true;
    abort.abort();
  });
  port.onMessage.addListener(async function onRequest(msg) {
    port.onMessage.removeListener(onRequest);
    try {
      await run(msg);
    } catch (e) {
      if (abort.signal.aborted && !(e instanceof RelayError)) return;
      const code = e instanceof RelayError ? e.code : e?.code === "local" ? "local" : "network";
      post({ type: "error", code, message: code === "network" ? "翻訳サーバーに接続できません" : e.message });
    } finally {
      // port は画面の側が閉じる (先に閉じると送ったメッセージが届かないことがある)
      done = true;
    }
  });

  async function run({ route, body } = {}) {
    const res = await apiFetch(route, body, deps, abort);
    post({ type: "head", status: res.status });
    if (!res.body) return post({ type: "end" });
    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    for (;;) {
      const { value, done: finished } = await reader.read();
      if (finished) break;
      const text = decoder.decode(value, { stream: true });
      if (text) post({ type: "chunk", text });
    }
    const rest = decoder.decode();
    if (rest) post({ type: "chunk", text: rest });
    post({ type: "end" });
  }
}

// background から API を 1 回呼ぶ。返す body は読むたびに時間切れを延ばし、abort で止まる。
// 失敗は RelayError (route・no-server・no-permission・timeout) か、接続の失敗 (そのほかの例外)
export async function apiFetch(route, body, deps, abort) {
  const method = ROUTES[route];
  if (!method) throw new RelayError("route", "不明な API です");
  const settings = await deps.settings?.();
  const local = settings?.mode === "local";
  const server = local ? "local" : await deps.server();
  const send = () => local ? (deps.nativeFetch ?? nativeFetch)(route, body, abort) : httpFetch(route, body, deps, abort, server);
  const key = local ? "local" : server;
  if (route === "api/config") {
    const res = await send();
    if (res.status === 200 && res.body) {
      const response = new Response(res.body, { status: res.status });
      const text = await response.text();
      try {
        const c = JSON.parse(text).concurrency;
        limits.set(key, Number.isSafeInteger(c?.max_in_flight) && c.max_in_flight > 0 ? Math.min(c.max_in_flight, 8) : 1);
      } catch { limits.set(key, 1); }
      return { status: res.status, body: new Response(text).body };
    }
    return res;
  }
  return scheduler.run(key, limits.get(key) ?? 1, abort.signal, send);
}

async function httpFetch(route, body, deps, abort, server) {
  const method = ROUTES[route];
  if (!server) throw new RelayError("no-server", "接続先が未設定です。拡張の設定でサーバーURLを入れてください");
  if (!(await deps.hasPermission(server))) {
    throw new RelayError("no-permission", "サーバーへの接続が許可されていません。拡張の設定で保存し直してください");
  }
  let timer;
  const timeoutMs = deps.timeoutMs ?? READ_TIMEOUT_MS;
  const arm = () => {
    clearTimeout(timer);
    timer = setTimeout(() => abort.abort(new RelayError("timeout", "翻訳サーバーの応答がありません")), timeoutMs);
  };
  const reason = (e) => (abort.signal.reason instanceof RelayError ? abort.signal.reason : e);
  arm();
  let res;
  try {
    res = await deps.fetch(`${server}/${route}`, {
      method,
      headers: method === "POST" ? { "Content-Type": "application/json" } : {},
      body: method === "POST" ? body : undefined,
      credentials: "include",
      // 認証プロキシのログイン画面への転送は追わず、ログインしていないとして扱う
      redirect: "manual",
      cache: "no-store",
      signal: abort.signal,
    });
  } catch (e) {
    clearTimeout(timer);
    throw reason(e);
  }
  // ここからは読み取りを待つあいだだけ時間を測る (読まずに捨てる応答で時計を残さない)
  clearTimeout(timer);
  // 転送 (opaqueredirect) はログインが要る印
  const status = res.type === "opaqueredirect" ? 401 : res.status;
  if (!res.body || res.type === "opaqueredirect") {
    clearTimeout(timer);
    return { status, body: null };
  }
  const reader = res.body.getReader();
  const onAbort = () => reader.cancel(abort.signal.reason).catch(() => {});
  abort.signal.addEventListener("abort", onAbort, { once: true });
  const cleanup = () => {
    clearTimeout(timer);
    abort.signal.removeEventListener("abort", onAbort);
  };
  const stream = new ReadableStream({
    async pull(controller) {
      arm();
      try {
        const { value, done } = await reader.read();
        if (abort.signal.aborted) throw abort.signal.reason;
        if (done) {
          cleanup();
          controller.close();
        } else controller.enqueue(value);
      } catch (e) {
        cleanup();
        controller.error(reason(e));
      }
    },
    cancel(why) {
      cleanup();
      return reader.cancel(why);
    },
  }, { highWaterMark: 0 });
  return { status, body: stream };
}
