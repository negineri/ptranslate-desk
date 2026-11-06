import { desktopFetch as request } from "./ipc.mjs";
import "./updates.mjs";
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

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
    const previous = savedSettings;
    const settings = { ...JSON.parse(savedSettings || "{}"), ...JSON.parse(value) };
    const next = JSON.stringify(settings);
    savedSettings = next;
    pendingSave = pendingSave.then(() => invoke("save_settings", { settings })).catch((error) => {
      if (savedSettings === next) savedSettings = previous;
      showError(error);
    });
    return pendingSave;
  },
};

export async function copyText(text) {
  try { await window.__TAURI__.clipboardManager.writeText(text); return true; }
  catch (error) { showError(error); return false; }
}

const loginButton = document.createElement("button");
loginButton.textContent = "ログイン";
document.getElementById("who").after(" ", loginButton);
const cancelLogin = document.createElement("button");
cancelLogin.textContent = "ログインを中止";
cancelLogin.hidden = true;
loginButton.after(" ", cancelLogin);
cancelLogin.addEventListener("click", () => { void invoke("cancel_login").catch(showError); });
let resolveLogin;
const login = new Promise((resolve) => { resolveLogin = resolve; });

async function restoreSession() {
  const status = await invoke("session_status");
  loginButton.disabled = status.logging_in;
  cancelLogin.hidden = !status.logging_in;
  if (!loginButton.dataset.loggedIn) loginButton.textContent = status.mock ? "モックにログイン" : "ログイン";
  if (status.logged_in && !loginButton.dataset.loggedIn) {
    const settings = await invoke("load_settings").catch(showError);
    savedSettings = settings ? JSON.stringify(settings) : null;
    loginButton.dataset.loggedIn = "true";
    loginButton.textContent = "ログアウト";
    resolveLogin();
  } else if (!status.logged_in && loginButton.dataset.loggedIn) {
    await pendingSave;
    location.reload();
  }
  if (status.auth_error) showError(status.auth_error);
  return status;
}

loginButton.addEventListener("click", async () => {
  loginButton.disabled = true;
  const loggingIn = !loginButton.dataset.loggedIn;
  cancelLogin.hidden = !loggingIn;
  try {
    await pendingSave;
    await invoke(loggingIn ? "login" : "logout");
    await restoreSession();
  } catch (error) { showError(error); }
  finally { loginButton.disabled = false; cancelLogin.hidden = true; }
});

await listen("session-changed", () => { void restoreSession().catch(showError); });
await listen("login-started", () => { loginButton.disabled = true; cancelLogin.hidden = false; });
await invoke("restore_session").catch(showError);
await restoreSession().catch(showError);

export async function desktopFetch(...args) { await login; return request(...args); }

const settingsPanel = document.createElement("details");
settingsPanel.id = "desktop-settings";
settingsPanel.innerHTML = `<summary>デスクトップの設定</summary>
  <label for="shortcut">選んだ文を翻訳するショートカット</label>
  <select id="shortcut">
    <option value="double-copy">⌘C を 0.5 秒以内に 2 回</option>
    <option value="CommandOrControl+Shift+T">⌘⇧T</option>
  </select>
  <p class="hint" id="shortcut-status"></p>
  <button id="accessibility">アクセシビリティの設定を開く</button>
  <p class="hint">macOS のシステム設定で「PTranslate Desk」を許可してください。許可後はアプリを再起動してください。
  主窓を閉じても常駐します。終了するにはメニューバーの「終了」か ⌘Q を使います。</p>`;
document.querySelector("section").append(settingsPanel);
const shortcut = document.getElementById("shortcut");
const shortcutStatus = document.getElementById("shortcut-status");
async function updateShortcutStatus() {
  const status = await invoke("session_status");
  shortcut.value = status.shortcut;
  shortcutStatus.textContent = status.paused ? "ショートカットを一時的に止めています" :
    status.accessibility ? "ショートカットを使えます" : "選んだ文を取得するにはアクセシビリティの許可が必要です";
}
shortcut.addEventListener("change", async () => {
  shortcut.disabled = true;
  await settingsStorage.setItem("translator", JSON.stringify({ shortcut: shortcut.value }));
  await updateShortcutStatus().catch(showError);
  shortcut.disabled = false;
});
document.getElementById("accessibility").addEventListener("click", () => {
  void invoke("open_accessibility_settings").catch(showError);
});
await listen("show-settings", () => { settingsPanel.open = true; settingsPanel.scrollIntoView(); });
await listen("session-changed", () => { void updateShortcutStatus().catch(showError); });
await listen("desktop-error", ({ payload }) => showError(payload));
await listen("translation-interrupted", () => document.getElementById("stop").click());

async function takeMainText() {
  if (document.querySelector("section").inert) return;
  const text = await invoke("take_main_text");
  if (text === null) return;
  const source = document.getElementById("src");
  source.value = text;
  source.dispatchEvent(new Event("input"));
  document.getElementById("go").click();
}
await listen("main-text", () => { void takeMainText().catch(showError); });
document.addEventListener("desktop-ready", () => { void takeMainText().catch(showError); });
void updateShortcutStatus().catch(showError);
