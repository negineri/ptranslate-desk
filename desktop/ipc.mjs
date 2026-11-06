const { Channel, invoke } = window.__TAURI__.core;

// メインウィンドウと翻訳ポップアップの両方で使う。所有者は Rust が呼び出し元の窓から決める。
export async function desktopFetch(route, { body, signal, popupRevision = null } = {}) {
  signal?.throwIfAborted();
  const id = crypto.randomUUID();
  const stream = new Channel();
  let reader, settled = false, finished = false;
  const responseBody = new ReadableStream({ start(controller) { reader = controller; } });
  return new Promise((resolve, reject) => {
    function finish(error) {
      if (finished) return;
      finished = true;
      signal?.removeEventListener("abort", abort);
      if (error) {
        if (settled) reader.error(error);
        else { reader.close(); reject(error); }
      } else { reader.close(); }
      void started.then(() => invoke("stop_request", { id })).catch(() => {});
    }
    function abort() { finish(new DOMException("中止しました", "AbortError")); }
    stream.onmessage = (event) => {
      if (finished) return;
      switch (event.kind) {
        case "start":
          settled = true;
          resolve(new Response(responseBody, { status: event.status }));
          break;
        case "bytes": reader.enqueue(Uint8Array.from(event.bytes)); break;
        case "end": finish(); break;
        case "canceled": abort(); break;
        case "login_required":
          finish(new Error(`サーバーが ${event.status} を返しました。ログインし直してください`));
          break;
        case "error":
          finish(new Error("翻訳サーバーに接続できないか、接続が途中で切れました"));
          break;
      }
    };
    const started = invoke("start_request", {
      id, route, body: body ? JSON.parse(body) : null, stream, popupRevision,
    });
    void started.catch((error) => finish(new Error(String(error))));
    signal?.addEventListener("abort", abort, { once: true });
    if (signal?.aborted) abort();
  });
}
