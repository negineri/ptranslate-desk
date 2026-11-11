import { anonymousKey, normalizeServerUrl } from "./settings.mjs";
import { nativeFetch } from "./native.mjs";
import { MAX_IN_FLIGHT, Scheduler } from "./scheduler.mjs";
const scheduler = new Scheduler();
const limits = new Map();
const credentialOperations = new Map();
const ANONYMOUS_TOKEN = /^pta_[0-9a-f]{32}\.([0-9]{1,12})\.[0-9a-f]{64}$/;

// background で API を呼び、応答を拡張の画面へ port で流す。
// 画面 (popup.html など) は Web ページの中の iframe でも動くので、Cookie を確実に付けられる
// background からだけサーバーへつなぐ。匿名資格の発行と削除は設定画面からだけ受ける。
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

  async function run({ route, body, connection } = {}) {
    // 接続先は信頼済みの拡張画面だけが渡す。権限は要求ごとに確かめる。
    const requestDeps = connection ? await pinConnection(deps, connection) : deps;
    const res = route === "api/anonymous" || route === "anonymous/forget"
      ? await anonymousAction(route, requestDeps, abort)
      : await apiFetch(route, body, requestDeps, abort);
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

// 翻訳開始時の接続先を保持し、区切り・再試行・追従中に設定を読み直さない。
export async function pinConnection(deps, settings = undefined) {
  const snapshot = { ...(settings ?? await deps.settings?.()) };
  const server = snapshot.mode === "local" ? "" : snapshot.server ?? await deps.server();
  snapshot.server = server;
  return { ...deps, settings: async () => snapshot, server: async () => server };
}

// 同じ接続先で発行・更新・削除が重なっても、削除後に更新結果が残らないようにする。
async function withCredential(key, operation) {
  const pending = (credentialOperations.get(key) ?? Promise.resolve()).catch(() => {}).then(operation);
  credentialOperations.set(key, pending);
  try { return await pending; }
  finally { if (credentialOperations.get(key) === pending) credentialOperations.delete(key); }
}

async function issueCredential(server, deps, abort) {
  const response = await httpFetch("api/anonymous", "{}", { ...deps, timeoutMs: 15000 }, abort, server);
  if (response.status !== 200 || !response.body) {
    await response.body?.cancel();
    const message = response.status === 404 ? "この接続先では匿名利用を使えません"
      : response.status === 429 || response.status === 503 ? "資格を発行できません。しばらく待ってから試してください"
        : "匿名の資格を発行できませんでした";
    throw new RelayError("anonymous", message);
  }
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let text = "", size = 0;
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > 4096) throw new Error();
      text += decoder.decode(value, { stream: true });
    }
    const token = JSON.parse(text + decoder.decode()).credential;
    if (typeof token === "string" && ANONYMOUS_TOKEN.test(token)) return token;
  } catch {
    abort.signal.throwIfAborted();
  } finally { await reader.cancel().catch(() => {}); }
  throw new RelayError("anonymous", "匿名の資格を受け取れませんでした");
}

async function anonymousAction(route, deps, abort) {
  const settings = await deps.settings?.();
  if (!deps.anonymousActions || !deps.anonymousStorage || settings?.mode === "local") {
    throw new RelayError("route", "不明な API です");
  }
  const server = normalizeServerUrl(settings?.server ?? await deps.server());
  const key = anonymousKey(server);
  await withCredential(key, async () => {
    abort.signal.throwIfAborted();
    if (route === "anonymous/forget") {
      await deps.anonymousStorage.remove(key);
    } else {
      const token = await issueCredential(server, deps, abort);
      abort.signal.throwIfAborted();
      await deps.anonymousStorage.set({ [key]: token });
    }
  });
  return { status: 204, body: null };
}

async function authenticatedFetch(route, body, deps, abort, server) {
  const storage = deps.anonymousStorage;
  if (!storage || !server) return httpFetch(route, body, deps, abort, server);
  const origin = normalizeServerUrl(server);
  const key = anonymousKey(origin);
  let token = (await storage.get(key))[key];
  if (typeof token !== "string" || !ANONYMOUS_TOKEN.test(token)) {
    return httpFetch(route, body, deps, abort, origin);
  }
  const renew = (previous) => withCredential(key, async () => {
    abort.signal.throwIfAborted();
    const current = (await storage.get(key))[key];
    if (current !== previous) {
      if (typeof current === "string" && ANONYMOUS_TOKEN.test(current)) return current;
      throw new RelayError("anonymous", "匿名利用を終了しました");
    }
    const fresh = await issueCredential(origin, deps, abort);
    abort.signal.throwIfAborted();
    await storage.set({ [key]: fresh });
    return fresh;
  });
  if (Number(ANONYMOUS_TOKEN.exec(token)[1]) * 1000 <= Date.now()) token = await renew(token);
  const response = await httpFetch(route, body, deps, abort, origin, token);
  if (response.status !== 401) return response;
  await response.body?.cancel();
  const fresh = await renew(token);
  if (route === "api/config") return httpFetch(route, body, deps, abort, origin, fresh);
  throw new RelayError("anonymous", "匿名の資格を更新しました。もう一度翻訳してください");
}

// background から API を 1 回呼ぶ。返す body は読むたびに時間切れを延ばし、abort で止まる。
// 失敗は RelayError (route・no-server・no-permission・timeout) か、接続の失敗 (そのほかの例外)
export async function apiFetch(route, body, deps, abort) {
  if (!Object.hasOwn(ROUTES, route)) throw new RelayError("route", "不明な API です");
  const settings = await deps.settings?.();
  const local = settings?.mode === "local";
  const server = local ? "local" : settings?.server ?? await deps.server();
  const send = () => local ? (deps.nativeFetch ?? nativeFetch)(route, body, abort) : authenticatedFetch(route, body, deps, abort, server);
  const key = local ? "local" : server;
  if (route === "api/config") {
    const res = await send();
    if (res.status === 200 && res.body) {
      const response = new Response(res.body, { status: res.status });
      const text = await response.text();
      try {
        const c = JSON.parse(text).concurrency;
        limits.set(key, Number.isSafeInteger(c?.max_in_flight) && c.max_in_flight > 0 ? Math.min(c.max_in_flight, MAX_IN_FLIGHT) : 1);
      } catch { limits.set(key, 1); }
      return { status: res.status, body: new Response(text).body };
    }
    return res;
  }
  return scheduler.run(key, limits.get(key) ?? 1, abort.signal, send);
}

async function httpFetch(route, body, deps, abort, server, token) {
  const method = route === "api/anonymous" ? "POST" : ROUTES[route];
  if (!server) throw new RelayError("no-server", "接続先が未設定です。設定でサーバーURLを入れ、「接続」を押してください");
  if (!(await deps.hasPermission(server))) {
    throw new RelayError("no-permission", "サーバーへの接続が許可されていません。設定で「接続」を押してください");
  }
  abort.signal.throwIfAborted();
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
      headers: {
        ...(method === "POST" ? { "Content-Type": "application/json" } : {}),
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      },
      body: method === "POST" ? body : undefined,
      credentials: token || route === "api/anonymous" ? "omit" : "include",
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
