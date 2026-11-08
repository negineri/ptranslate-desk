// 拡張の画面から background (relay.mjs) 経由で API を呼び、fetch と同じ Response を返す。
// 中止 (signal) は port を閉じて伝える。background が接続を閉じ、サーバーが翻訳エンジンを止める (SPEC 5.4)
export class TransportError extends Error {
  constructor(code, message) {
    super(message);
    this.code = code;
  }
}

const defaultConnect = () => chrome.runtime.connect({ name: "api" });

// 別々のportを使う各区切りにも、開始時の接続先を引き継ぐ。
export function sessionFetch(settings, connect = defaultConnect) {
  const connection = Object.freeze({ mode: settings.mode, server: settings.server });
  return (route, options = {}) => extensionFetch(route, { ...options, connection }, connect);
}

export function extensionFetch(route, { body, signal, connection } = {}, connect = defaultConnect) {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) return reject(signal.reason ?? new DOMException("中止しました", "AbortError"));
    const port = connect();
    const encoder = new TextEncoder();
    let controller = null;
    let settled = false;
    let finished = false;

    const fail = (error) => {
      if (finished) return;
      finished = true;
      if (!settled) {
        settled = true;
        reject(error);
      } else {
        try {
          controller.error(error);
        } catch {
          /* もう閉じている */
        }
      }
    };
    const onAbort = () => {
      fail(signal.reason ?? new DOMException("中止しました", "AbortError"));
      try {
        port.disconnect();
      } catch {
        /* もう閉じている */
      }
    };
    signal?.addEventListener("abort", onAbort, { once: true });

    port.onMessage.addListener((msg) => {
      if (finished) return;
      if (msg.type === "head") {
        const stream = new ReadableStream({
          start(c) {
            controller = c;
          },
          cancel() {
            finished = true;
            try {
              port.disconnect();
            } catch {
              /* もう閉じている */
            }
          },
        });
        settled = true;
        // Response は 200〜599 しか作れない。範囲外は接続できなかったとして扱う
        if (!(msg.status >= 200 && msg.status <= 599)) {
          finished = true;
          port.disconnect();
          return reject(new TransportError("network", "翻訳サーバーに接続できません"));
        }
        const empty = [204, 205, 304].includes(msg.status);
        resolve(new Response(empty ? null : stream, { status: msg.status }));
      } else if (msg.type === "chunk") {
        controller?.enqueue(encoder.encode(msg.text));
      } else if (msg.type === "end") {
        finished = true;
        signal?.removeEventListener("abort", onAbort);
        controller?.close();
        port.disconnect();
      } else if (msg.type === "error") {
        signal?.removeEventListener("abort", onAbort);
        fail(new TransportError(msg.code, msg.message));
        port.disconnect();
      }
    });
    port.onDisconnect.addListener(() => {
      signal?.removeEventListener("abort", onAbort);
      fail(new TransportError("network", "翻訳サーバーに接続できません"));
    });
    port.postMessage({ route, body, ...(connection ? { connection } : {}) });
  });
}
