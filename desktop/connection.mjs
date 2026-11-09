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
  <p class="hint">自分で用意した翻訳サーバーを使う場合は、サーバーと認証の設定を入力してください。</p>
  <form id="connection-form">
    <label for="server-url">サーバーURL</label>
    <input id="server-url" type="url" required placeholder="https://translate.example.com" autocomplete="off" spellcheck="false">
    <label for="discovery-url">OIDC discovery URL</label>
    <input id="discovery-url" type="url" required placeholder="https://auth.example.com/application/o/translator-desktop/.well-known/openid-configuration" autocomplete="off" spellcheck="false">
    <label for="client-id">client ID（公開クライアント）</label>
    <input id="client-id" type="text" required maxlength="256" autocomplete="off" spellcheck="false">
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
const fields = {
  server_url: form.querySelector("#server-url"),
  discovery_url: form.querySelector("#discovery-url"),
  client_id: form.querySelector("#client-id"),
};
let saved;
function fill(connection) {
  for (const [key, field] of Object.entries(fields)) field.value = connection[key];
}
try {
  saved = await invoke("load_connection");
  fill(saved);
} catch (error) {
  status.textContent = String(error);
  showPage(true);
  panel.open = true;
}
export function resetConnectionFields() {
  if (saved) fill(saved);
}
export async function applySetupConnection() {
  if (!form.reportValidity()) return false;
  const connection = Object.fromEntries(Object.entries(fields).map(([key, field]) => [key, field.value.trim()]));
  if (!saved || Object.keys(fields).some((key) => connection[key] !== saved[key])) {
    saved = await invoke("save_connection", { connection });
    fill(saved);
  }
  stageSettings("connection", null);
  return true;
}
form.addEventListener("submit", (event) => event.preventDefault());
form.addEventListener("input", () => {
  if (!saved || form.closest("#setup-wizard")) return;
  const connection = Object.fromEntries(Object.entries(fields).map(([key, field]) => [key, field.value.trim()]));
  const changed = Object.keys(fields).some((key) => connection[key] !== saved[key]);
  stageSettings("connection", changed ? {
    validate() { panel.open = true; return form.reportValidity(); },
    async apply() {
      saved = await invoke("save_connection", { connection });
      fill(saved);
      document.dispatchEvent(new Event("translation-settings-applied"));
    },
    cancel: resetConnectionFields,
  } : null);
});
