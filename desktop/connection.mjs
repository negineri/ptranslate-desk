const { invoke } = window.__TAURI__.core;
const style = document.createElement("style");
style.textContent = `#connection-settings { margin-bottom: 16px; }
  #connection-form { display: grid; gap: 8px; margin-top: 12px; }
  #connection-form input { width: 100%; min-width: 0; }
  #connection-form button { justify-self: start; }
  #connection-form p { margin: 0; }`;
document.head.append(style);
const panel = document.createElement("details");
panel.id = "connection-settings";
panel.innerHTML = `<summary>接続先の設定</summary>
  <p class="hint">自分で用意した翻訳サーバーを使う場合は、サーバーと認証の設定を入力してください。</p>
  <form id="connection-form">
    <label for="server-url">サーバーURL</label>
    <input id="server-url" type="url" required placeholder="https://translate.example.com" autocomplete="off" spellcheck="false">
    <label for="discovery-url">OIDC discovery URL</label>
    <input id="discovery-url" type="url" required placeholder="https://auth.example.com/application/o/translator-desktop/.well-known/openid-configuration" autocomplete="off" spellcheck="false">
    <label for="client-id">client ID（公開クライアント）</label>
    <input id="client-id" type="text" required maxlength="256" autocomplete="off" spellcheck="false">
    <p class="hint">保存すると現在の接続先からログアウトし、アプリを再起動します。新しい接続先でログインし直してください。</p>
    <button id="save-connection" type="submit">保存して再起動</button>
    <p id="connection-status" class="hint" role="status"></p>
  </form>`;
document.querySelector("section").before(panel);
const status = panel.querySelector("#connection-status");
try {
  const connection = await invoke("load_connection");
  panel.querySelector("#server-url").value = connection.server_url;
  panel.querySelector("#discovery-url").value = connection.discovery_url;
  panel.querySelector("#client-id").value = connection.client_id;
} catch (error) {
  status.textContent = String(error);
  panel.open = true;
}
panel.querySelector("form").addEventListener("submit", async (event) => {
  event.preventDefault();
  if (!window.confirm("現在の接続先からログアウトして接続先を変更します。保存後にアプリを再起動してよろしいですか？")) return;
  const button = panel.querySelector("#save-connection");
  button.disabled = true;
  status.textContent = "接続先を保存しています…";
  try {
    await invoke("save_connection", { connection: {
      server_url: panel.querySelector("#server-url").value.trim(),
      discovery_url: panel.querySelector("#discovery-url").value.trim(),
      client_id: panel.querySelector("#client-id").value.trim(),
    } });
  } catch (error) {
    status.textContent = String(error);
    status.className = "hint err";
  } finally { button.disabled = false; }
});
