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
      const code = e instanceof RelayError ? e.code : "network";
      post({ type: "error", code, message: code === "network" ? "翻訳サーバーに接続できません" : e.message });
    } finally {
      // port は画面の側が閉じる (先に閉じると送ったメッセージが届かないことがある)
      done = true;
    }
  });

  async function run({ route, body } = {}) {
    const method = ROUTES[route];
    if (!method) throw new RelayError("route", "不明な API です");
    const server = await deps.server();
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
    arm();
    try {
      const res = await deps.fetch(`${server}/${route}`, {
        method,
        headers: method === "POST" ? { "Content-Type": "application/json" } : {},
        body: method === "POST" ? body : undefined,
        credentials: "include",
        // 認証プロキシのログイン画面への転送は追わず、ログインしていないとして扱う
        redirect: "manual",
        cache: "no-store",
        signal: abort.signal,
      });
      // 転送 (opaqueredirect) はログインが要る印
      const status = res.type === "opaqueredirect" ? 401 : res.status;
      post({ type: "head", status });
      if (!res.body || res.type === "opaqueredirect") return post({ type: "end" });
      const reader = res.body.getReader();
      abort.signal.addEventListener("abort", () => reader.cancel(abort.signal.reason).catch(() => {}), { once: true });
      const decoder = new TextDecoder();
      for (;;) {
        arm();
        const { value, done: finished } = await reader.read();
        if (finished) break;
        const text = decoder.decode(value, { stream: true });
        if (text) post({ type: "chunk", text });
      }
      const rest = decoder.decode();
      if (rest) post({ type: "chunk", text: rest });
      post({ type: "end" });
    } catch (e) {
      if (abort.signal.reason instanceof RelayError) throw abort.signal.reason;
      throw e;
    } finally {
      clearTimeout(timer);
    }
  }
}
