import { settingsPage, showPage, stageSettings } from "./settings-view.mjs";
const { invoke } = window.__TAURI__.core;
const style = document.createElement("style");
style.textContent = `#connection-form { display: grid; gap: 8px; margin-top: 12px; }
  #connection-form input { width: 100%; min-width: 0; }
  #connection-form button { justify-self: start; }
  #connection-form p { margin: 0; }`;
document.head.append(style);
const panel = document.createElement("details");
panel.id = "connection-settings";
panel.innerHTML = `<summary>サーバー翻訳の設定</summary>
  <p class="hint">自分で用意した翻訳サーバーを使う場合は、サーバーURLを入力してください。</p>
  <form id="connection-form">
    <label for="server-url">サーバーURL</label>
    <input id="server-url" type="url" required placeholder="https://translate.example.com" autocomplete="off" spellcheck="false">
    <p class="hint">接続先を変更すると現在の接続先からログアウトします。適用後、新しい接続先でログインしてください。</p>
    <p id="connection-status" class="hint" role="status"></p>
  </form>`;
panel.open = true;
const serverPanel = document.createElement("div");
serverPanel.id = "server-settings-panel";
serverPanel.className = "translation-settings-panel";
serverPanel.append(panel);
settingsPage.append(serverPanel);
const status = panel.querySelector("#connection-status");
const form = panel.querySelector("form");
const serverUrlField = form.querySelector("#server-url");
let saved;
try {
  saved = await invoke("load_connection");
  resetConnectionFields();
} catch (error) {
  status.textContent = String(error);
  showPage(true);
  panel.open = true;
}
export function resetConnectionFields() {
  if (saved) serverUrlField.value = saved.server_url;
}
async function saveConnection(serverUrl) {
  saved = await invoke("save_connection", { serverUrl });
  resetConnectionFields();
}
export async function applySetupConnection() {
  if (!form.reportValidity()) return false;
  const serverUrl = serverUrlField.value.trim();
  if (serverUrl !== saved?.server_url) await saveConnection(serverUrl);
  stageSettings("connection", null);
  return true;
}
form.addEventListener("submit", (event) => event.preventDefault());
form.addEventListener("input", () => {
  if (form.closest("#setup-wizard")) return;
  const serverUrl = serverUrlField.value.trim();
  stageSettings("connection", serverUrl !== saved?.server_url ? {
    validate() { panel.open = true; return form.reportValidity(); },
    async apply() {
      await saveConnection(serverUrl);
      document.dispatchEvent(new Event("translation-settings-applied"));
    },
    cancel: resetConnectionFields,
  } : null);
});
