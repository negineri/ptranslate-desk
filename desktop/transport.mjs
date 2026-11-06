const { Channel, invoke } = window.__TAURI__.core;
function showError(error) {
  const status = document.getElementById("status");
  status.textContent = String(error);
  status.className = "hint err";
}

let savedSettings = null;
let pendingSave = Promise.resolve();

export const settingsStorage = {
  getItem() { return savedSettings; },
  setItem(_key, value) {
    savedSettings = value;
    const settings = JSON.parse(value);
    pendingSave = pendingSave.then(() => invoke("save_settings", { settings })).catch(showError);
  },
};

export async function copyText(text) {
  try {
    await window.__TAURI__.clipboardManager.writeText(text);
    return true;
  } catch (error) {
    showError(error);
    return false;
  }
}

const loginButton = document.createElement("button");
loginButton.textContent = "モックにログイン";
document.getElementById("who").after(" ", loginButton);

let resolveLogin;
const login = new Promise((resolve) => { resolveLogin = resolve; });
loginButton.addEventListener("click", async () => {
  loginButton.disabled = true;
  try {
    if (loginButton.dataset.loggedIn) {
      await invoke("set_mock_login", { loggedIn: false });
      await pendingSave;
      location.reload();
      return;
    }
    const settings = await invoke("load_settings").catch(showError);
    savedSettings = settings ? JSON.stringify(settings) : null;
    await invoke("set_mock_login", { loggedIn: true });
    loginButton.dataset.loggedIn = "true";
    loginButton.textContent = "ログアウト";
    resolveLogin();
  } catch (error) {
    showError(error);
  } finally {
    loginButton.disabled = false;
  }
});

let nextId = 0;

export async function desktopFetch(route, { body, signal } = {}) {
  await login;
  signal?.throwIfAborted();
  const id = ++nextId;
  const stream = new Channel();
  let reader;
  let finished = false;
  const responseBody = new ReadableStream({ start(controller) { reader = controller; } });

  return new Promise((resolve, reject) => {
    function finish(error) {
      if (finished) return;
      finished = true;
      signal?.removeEventListener("abort", abort);
      if (error) {
        reader.error(error);
        reject(error);
      } else {
        reader.close();
      }
      void started.then(() => invoke("stop_request", { id })).catch(() => {});
    }

    function abort() {
      finish(new DOMException("中止しました", "AbortError"));
    }

    stream.onmessage = (event) => {
      if (finished) return;
      switch (event.kind) {
        case "start":
          resolve(new Response(responseBody, { status: event.status }));
          break;
        case "bytes":
          reader.enqueue(Uint8Array.from(event.bytes));
          break;
        case "end":
          finish();
          break;
        case "error":
          finish(new Error("ローカルのモックサーバーに接続できないか、接続が途中で切れました"));
          break;
      }
    };
    const started = invoke("start_request", { id, route, body: body ? JSON.parse(body) : null, stream });
    void started.catch((error) => finish(new Error(String(error))));
    signal?.addEventListener("abort", abort, { once: true });
  });
}
