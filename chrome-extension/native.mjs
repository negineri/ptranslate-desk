// 1要求につき1接続。切断は中継と推論の中止まで伝える。
function localError(message) { const error = new Error(message); error.code = "local"; return error; }

export function nativeFetch(route, body, abort, connect = () => chrome.runtime.connectNative("com.witenp.ptranslate")) {
  return new Promise((resolve, reject) => {
    abort.signal.throwIfAborted();
    let port;
    try { port = connect(); } catch { reject(localError("Chrome連携に接続できません。設定で「接続」を押してください")); return; }
    let timer;
    let controller, finished = false, started = false;
    const failure = () => localError("ローカル翻訳に接続できません。デスクトップの起動とローカルモードを確認し、設定で「接続」を押してください");
    const close = () => { clearTimeout(timer); abort.signal.removeEventListener("abort", onAbort); port.disconnect(); };
    const fail = (error) => {
      if (finished) return;
      finished = true;
      if (!abort.signal.aborted) error = localError(error?.message ?? String(error));
      if (started) controller.error(error); else reject(error);
      close();
    };
    const onAbort = () => fail(abort.signal.reason ?? new DOMException("中止しました", "AbortError"));
    abort.signal.addEventListener("abort", onAbort, { once: true });
    port.onDisconnect.addListener(() => {
      const detail = globalThis.chrome?.runtime?.lastError?.message;
      if (detail?.includes("Specified native messaging host not found")) {
        fail(localError("Chrome連携が見つかりません。デスクトップで連携を登録し、Chromeを完全に終了して起動し直してください。"));
      } else fail(detail ? localError(`ローカル翻訳に接続できません (${detail})`) : failure());
    });
    const arm = () => { clearTimeout(timer); timer = setTimeout(() => fail(new Error("ローカル翻訳から応答がありません")), 180_000); };
    arm();
    port.onMessage.addListener((msg) => {
      if (finished) return;
      arm();
      if (msg.kind === "start" && !started && Number.isInteger(msg.status) && msg.status >= 200 && msg.status <= 599) {
        const stream = new ReadableStream({
          start(c) { controller = c; },
          cancel() { finished = true; close(); },
        });
        started = true;
        resolve({ status: msg.status, body: stream });
      } else if (msg.kind === "bytes" && started && Array.isArray(msg.bytes)) {
        controller.enqueue(Uint8Array.from(msg.bytes));
      } else if (msg.kind === "end" && started) {
        finished = true; controller.close(); close();
      } else fail(failure());
    });
    try { port.postMessage({ version: 1, route, body: body ? JSON.parse(body) : null }); }
    catch (error) { fail(error); }
  });
}
