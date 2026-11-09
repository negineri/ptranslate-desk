import { settingsPage, showPage, accountSlot, stageSettings, hasPendingSettings } from "./settings-view.mjs";
import { desktopFetch as request } from "./ipc.mjs";
import "./updates.mjs";
import "./connection.mjs";
import { modeReady } from "./local-view.mjs";
import { runSetup } from "./setup-wizard.mjs";
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

const serverAccess = document.createElement("div");
serverAccess.id = "server-access";
serverAccess.className = "translation-settings-switch";
serverAccess.innerHTML = `<input id="server-active" type="checkbox" role="switch" aria-label="サーバー翻訳" disabled>
  <span class="hint" role="status"></span>`;
const serverPanel = document.getElementById("connection-settings");
serverPanel.after(serverAccess);
const loginSwitch = serverAccess.querySelector("input");
const serverStatus = serverAccess.querySelector("span");
serverAccess.prepend(serverStatus);
const cancelLogin = document.createElement("button");
cancelLogin.textContent = "ログインを中止";
cancelLogin.hidden = true;
serverAccess.after(cancelLogin);
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
const mode = document.createElement("div");
mode.id = "translation-destinations";
mode.setAttribute("role", "group");
mode.setAttribute("aria-label", "一時的な翻訳先");
mode.hidden = true;
mode.innerHTML = `<button id="destination-server" type="button" aria-pressed="false">サーバー <small>無効</small></button>
  <button id="destination-local" type="button" aria-pressed="false">ローカル <small>無効</small></button>`;
accountSlot.append(mode);
const serverButton = mode.querySelector("#destination-server");
const localButton = mode.querySelector("#destination-local");
let defaultLocal = false;
const destinationKey = "ptranslate-desktop-destination";
const rememberedDestination = window.localStorage.getItem(destinationKey);
let temporaryLocal = rememberedDestination === "local" ? true : rememberedDestination === "server" ? false : null;
let serverAuthenticated = false;
let serverConfigured = false;
let serverSwitching = false;

export function translationMode() {
  return temporaryLocal ?? defaultLocal;
}

function updateDestinations() {
  const useLocal = translationMode();
  serverButton.setAttribute("aria-pressed", String(!useLocal));
  localButton.setAttribute("aria-pressed", String(useLocal));
  const serverAvailable = serverConfigured && serverAuthenticated;
  serverButton.dataset.available = String(serverAvailable);
  serverButton.querySelector("small").textContent = serverAvailable ? "有効" : "無効";
  serverButton.title = serverAvailable ? "サーバーで翻訳" : "サーバー翻訳の設定を開く";
}

async function selectDestination(useLocal) {
  if (translationMode() === useLocal) return;
  document.getElementById("stop").click();
  temporaryLocal = useLocal;
  window.localStorage.setItem(destinationKey, useLocal ? "local" : "server");
  updateDestinations();
  document.dispatchEvent(new CustomEvent("translation-destination-changed", { detail: useLocal }));
  await restoreSession();
}

serverButton.addEventListener("click", () => {
  if (serverConfigured && serverAuthenticated) {
    void selectDestination(false).catch(showError);
    return;
  }
  showPage(true);
  const connection = document.getElementById("connection-settings");
  connection.open = true;
  if (serverConfigured) {
    loginSwitch.focus();
    if (defaultLocal && !serverAuthenticated) void invoke("restore_server_session").catch(showError);
  } else {
    document.getElementById("server-url").focus();
  }
});
localButton.addEventListener("click", () => {
  if (localButton.dataset.available === "true") {
    void selectDestination(true).catch(showError);
    return;
  }
  showPage(true);
  document.getElementById("local-settings").open = true;
  document.getElementById("local-settings-title").focus();
});

async function restoreSession() {
  const status = await invoke("session_status");
  defaultLocal = status.local;
  serverAuthenticated = status.server_authenticated;
  serverConfigured = status.mock || Boolean((await invoke("load_connection").catch(showError))?.server_url);
  const useLocal = translationMode();
  loginSwitch.disabled = !serverConfigured || status.logging_in || serverSwitching;
  loginSwitch.checked = serverAuthenticated;
  serverStatus.textContent = status.logging_in ? "ログイン中" : status.server_session_only ? "今回のみログイン中" : serverAuthenticated ? "ログイン済み" :
    serverConfigured ? "未接続" : "接続先未設定";
  cancelLogin.hidden = !status.logging_in;
  if (status.logged_in || useLocal) resolveLogin();
  updateDestinations();
  if (status.auth_error && !useLocal) showError(status.auth_error);
  return status;
}

loginSwitch.addEventListener("change", async () => {
  const active = loginSwitch.checked;
  serverSwitching = true;
  loginSwitch.disabled = true;
  try {
    await pendingSave;
    await invoke(active ? "login" : "logout");
  } catch (error) { showError(error); }
  finally {
    serverSwitching = false;
    await restoreSession().catch(showError);
  }
});

await modeReady;
const localStatus = await invoke("local_status");
defaultLocal = localStatus.enabled;
localButton.dataset.available = String(localStatus.available && localStatus.supported && localStatus.active);
localButton.querySelector("small").textContent = localButton.dataset.available === "true" ? "有効" : "無効";

mode.hidden = false;
await listen("session-changed", () => { void restoreSession().catch(showError); });
await listen("login-started", () => { loginSwitch.disabled = true; cancelLogin.hidden = false; serverStatus.textContent = "ログイン中"; });
await invoke("restore_session").catch(showError);
if (defaultLocal && !translationMode()) await invoke("restore_server_session").catch(showError);
document.dispatchEvent(new CustomEvent("translation-destination-changed", { detail: translationMode() }));
let initialSession = await restoreSession().catch(showError);
const settings = await invoke("load_settings").catch(showError);
savedSettings = settings ? JSON.stringify(settings) : null;
async function restoreAfterSetup(destination) {
  temporaryLocal = destination;
  await invoke("restore_session").catch(showError);
  const status = await restoreSession().catch(showError);
  document.dispatchEvent(new CustomEvent("translation-destination-changed", { detail: translationMode() }));
  return status;
}
export async function desktopFetch(route, options = {}) {
  await login;
  return request(route, { ...options, localOverride: options.localOverride ?? translationMode() });
}

const settingsPanel = document.createElement("details");
const isMac = /Mac/.test(navigator.platform);
const copyKey = isMac ? "⌘C" : "Ctrl+C";
settingsPanel.id = "desktop-settings";
settingsPanel.innerHTML = `<summary>デスクトップの設定</summary>
  <div id="shortcut-controls">
  <label for="shortcut">選んだ文を翻訳するショートカット</label>
  <select id="shortcut">
    <option value="double-copy">${copyKey} を 0.5 秒以内に 2 回</option>
    <option value="CommandOrControl+Shift+T">${isMac ? "⌘⇧T" : "Ctrl+Shift+T"}</option>
    <option value="custom">自分で指定する</option>
  </select>
  <div id="custom-shortcut" hidden>
    <label for="shortcut-keys">組み合わせ（例: CommandOrControl+Alt+J）</label>
    <input id="shortcut-keys" type="text" autocomplete="off" spellcheck="false">
  </div>
  <p class="hint" id="shortcut-status"></p>
  <p class="hint" id="shortcut-diagnostics"></p>
  <button id="accessibility" ${isMac ? "" : "hidden"}>アクセシビリティの設定を開く</button>
  </div>
  <p class="hint">${isMac ? 'macOS のシステム設定で「PTranslate Desk」を許可してください。許可は数秒以内に反映されます。メインウィンドウを閉じても常駐します。終了するにはメニューバーの「終了」か ⌘Q を使います。' : 'メインウィンドウを閉じても通知領域に常駐します。終了するにはトレイの「終了」を使います。組み合わせのショートカットは、キーを離すと選んだ文をコピーします。管理者権限で動くアプリからは取得できない場合があります。'}</p>`;
settingsPanel.open = Boolean(initialSession?.logged_in);
const desktopActions = document.createElement("div");
desktopActions.innerHTML = `<button id="start-setup" type="button">初期設定を始める</button>`;
settingsPage.querySelector(".settings-heading h2").after(desktopActions);
serverPanel.parentElement.after(settingsPanel);
const startSetup = desktopActions.querySelector("button");
startSetup.addEventListener("click", async () => {
  if (hasPendingSettings()) {
    showError("変更を適用またはキャンセルしてから、初期設定を始めてください。");
    return;
  }
  startSetup.disabled = true;
  try {
    const local = await invoke("local_status");
    const destination = await runSetup(local, null, { manual: true, saveShortcut });
    if (destination !== null) {
      await restoreAfterSetup(destination);
      showPage(false);
    }
  } catch (error) { showError(error); }
  finally {
    startSetup.disabled = false;
    if (!settingsPage.hidden) startSetup.focus();
    void updateShortcutStatus().catch(showError);
  }
});
const shortcut = document.getElementById("shortcut");
let shortcutDraft = false;
const shortcutStatus = document.getElementById("shortcut-status");
async function updateShortcutStatus() {
  const status = await invoke("session_status");
  const custom = !["double-copy", "CommandOrControl+Shift+T"].includes(status.shortcut);
  if (!shortcutDraft && !document.getElementById("setup-wizard") && document.activeElement !== shortcut && document.activeElement?.id !== "shortcut-keys") {
    shortcut.value = custom ? "custom" : status.shortcut;
    document.getElementById("custom-shortcut").hidden = !custom;
    if (custom) document.getElementById("shortcut-keys").value = status.shortcut;
  }
  shortcutStatus.textContent = status.paused ? "ショートカットを一時的に止めています" :
    !status.logged_in ? "ショートカットを使うにはログインしてください" :
    !status.accessibility ? (isMac ? "選んだ文を取得するにはアクセシビリティの許可が必要です" : "このOSでは選んだ文の取得に対応していません") :
    status.shortcut === "double-copy" && !status.copy_monitor ? `${copyKey}の監視を登録できませんでした` : "ショートカットを使えます";
  document.getElementById("shortcut-diagnostics").textContent =
    `${isMac ? `アクセシビリティ: ${status.accessibility ? "許可済み" : "未許可"} / ` : ""}${copyKey}の監視: ${status.copy_monitor ? "登録済み" : "未登録"} / 他のアプリでの${copyKey}検出: ${status.copy_events}回`;
}
setInterval(() => {
  if (!settingsPage.hidden && settingsPanel.open) void updateShortcutStatus().catch(showError);
}, 2000);
const shortcutKeys = document.getElementById("shortcut-keys");
async function saveShortcut(value) {
  await pendingSave;
  const settings = { ...JSON.parse(savedSettings || "{}"), shortcut: value };
  await invoke("save_settings", { settings });
  savedSettings = JSON.stringify(settings);
}
function stageShortcut() {
  if (document.getElementById("setup-wizard")) return;
  const value = shortcut.value === "custom" ? shortcutKeys.value.trim() : shortcut.value;
  shortcutDraft = value !== (JSON.parse(savedSettings || "{}").shortcut || "double-copy");
  stageSettings("shortcut", shortcutDraft ? {
    validate() {
      if (shortcut.value !== "custom" || value.includes("+")) return true;
      settingsPanel.open = true;
      shortcutKeys.focus();
      showError("CommandOrControl+Alt+J のように修飾キーと文字を指定してください");
      return false;
    },
    async apply() {
      await saveShortcut(value);
      shortcutDraft = false;
      await updateShortcutStatus().catch(showError);
    },
    cancel() {
      shortcutDraft = false;
      void updateShortcutStatus().catch(showError);
    },
  } : null);
}
shortcut.addEventListener("change", () => {
  document.getElementById("custom-shortcut").hidden = shortcut.value !== "custom";
  if (shortcut.value === "custom") shortcutKeys.focus();
  stageShortcut();
});
shortcutKeys.addEventListener("input", stageShortcut);
document.getElementById("accessibility").addEventListener("click", () => {
  void invoke("open_accessibility_settings").catch(showError);
});
await updateShortcutStatus().catch(showError);
const setupDestination = await runSetup(localStatus, settings, { saveShortcut });
if (setupDestination !== null) initialSession = await restoreAfterSetup(setupDestination);
if (initialSession) showPage(!initialSession.logged_in);
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
