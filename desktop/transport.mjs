import { settingsPage, showPage } from "./settings-view.mjs";
import { desktopFetch as request } from "./ipc.mjs";
import "./updates.mjs";
import "./connection.mjs";
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const settingsError = document.createElement("p");
settingsError.id = "settings-error";
settingsError.setAttribute("role", "alert");
settingsPage.prepend(settingsError);

function showError(error) {
  const status = settingsPage.hidden ? document.getElementById("status") : document.getElementById("settings-error");
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
if (/Mac/.test(navigator.platform)) {
  const keychainHelp = document.createElement("details");
  keychainHelp.className = "hint";
  keychainHelp.open = false;
  keychainHelp.innerHTML = `<summary>キーチェーンの確認が表示されたら</summary>
    <p>PTranslate Deskは、ログイン状態を保つための認証情報をmacOSのキーチェーンに保存しています。
    起動時やログインの更新時に、この情報を使う許可を求められることがあります。</p>
    <p>確認画面に「PTranslate Desk」と「com.witenp.ptranslate-desk」が表示されていることを確認し、
    キーチェーン「ログイン」のパスワードを入力して「常に許可」を押してください。
    通常はMacにログインするときのパスワードです。入力する場所はmacOSの確認画面です。</p>
    <p>「常に許可」で、この認証情報へのアクセスが許可され、以後の確認を省けます。
    アプリの更新や差し替え後は、再び確認が表示されることがあります。</p>`;
  settingsPage.append(keychainHelp);
}
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
    <option value="custom">自分で指定する</option>
  </select>
  <div id="custom-shortcut" hidden>
    <label for="shortcut-keys">組み合わせ（例: CommandOrControl+Alt+J）</label>
    <input id="shortcut-keys" type="text" autocomplete="off" spellcheck="false">
    <button id="save-shortcut">この組み合わせを使う</button>
  </div>
  <p class="hint" id="shortcut-status"></p>
  <p class="hint" id="shortcut-diagnostics"></p>
  <button id="accessibility">アクセシビリティの設定を開く</button>
  <p class="hint">macOS のシステム設定で「PTranslate Desk」を許可してください。許可は数秒以内に反映されます。
  メインウィンドウを閉じても常駐します。終了するにはメニューバーの「終了」か ⌘Q を使います。</p>`;
settingsPanel.open = true;
document.getElementById("connection-settings").after(settingsPanel);
const shortcut = document.getElementById("shortcut");
const shortcutStatus = document.getElementById("shortcut-status");
async function updateShortcutStatus() {
  const status = await invoke("session_status");
  const custom = !["double-copy", "CommandOrControl+Shift+T"].includes(status.shortcut);
  if (document.activeElement !== shortcut && document.activeElement?.id !== "shortcut-keys") {
    shortcut.value = custom ? "custom" : status.shortcut;
    document.getElementById("custom-shortcut").hidden = !custom;
    if (custom) document.getElementById("shortcut-keys").value = status.shortcut;
  }
  shortcutStatus.textContent = status.paused ? "ショートカットを一時的に止めています" :
    !status.logged_in ? "ショートカットを使うにはログインしてください" :
    !status.accessibility ? "選んだ文を取得するにはアクセシビリティの許可が必要です" :
    status.shortcut === "double-copy" && !status.copy_monitor ? "⌘Cの監視を登録できませんでした" : "ショートカットを使えます";
  document.getElementById("shortcut-diagnostics").textContent =
    `アクセシビリティ: ${status.accessibility ? "許可済み" : "未許可"} / ⌘Cの監視: ${status.copy_monitor ? "登録済み" : "未登録"} / 他のアプリでの⌘C検出: ${status.copy_events}回`;
}
setInterval(() => {
  if (!settingsPage.hidden && settingsPanel.open) void updateShortcutStatus().catch(showError);
}, 2000);
shortcut.addEventListener("change", async () => {
  document.getElementById("custom-shortcut").hidden = shortcut.value !== "custom";
  if (shortcut.value === "custom") {
    document.getElementById("shortcut-keys").focus();
    return;
  }
  shortcut.disabled = true;
  await settingsStorage.setItem("translator", JSON.stringify({ shortcut: shortcut.value }));
  await updateShortcutStatus().catch(showError);
  shortcut.disabled = false;
});
document.getElementById("save-shortcut").addEventListener("click", async () => {
  const value = document.getElementById("shortcut-keys").value.trim();
  if (!value.includes("+")) {
    showError("CommandOrControl+Alt+J のように修飾キーと文字を指定してください");
    return;
  }
  await settingsStorage.setItem("translator", JSON.stringify({ shortcut: value }));
  await updateShortcutStatus().catch(showError);
});
document.getElementById("accessibility").addEventListener("click", () => {
  void invoke("open_accessibility_settings").catch(showError);
});
await listen("show-settings", () => { showPage(true); settingsPanel.open = true; settingsPanel.scrollIntoView(); document.getElementById("shortcut").focus(); });
await listen("session-changed", () => { void updateShortcutStatus().catch(showError); });
await listen("desktop-error", ({ payload }) => showError(payload));
await listen("translation-interrupted", () => document.getElementById("stop").click());

async function takeMainText() {
  if (document.querySelector("section").inert) return;
  const text = await invoke("take_main_text");
  if (text === null) return;
  const source = document.getElementById("src");
  showPage(false);
  source.value = text;
  source.dispatchEvent(new Event("input"));
  document.getElementById("go").click();
}
await listen("main-text", () => { void takeMainText().catch(showError); });
document.addEventListener("desktop-ready", () => { void takeMainText().catch(showError); });
void updateShortcutStatus().catch(showError);
