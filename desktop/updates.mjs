const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// ログイン前も更新できるよう、翻訳用の section の外に置く。
const panel = document.createElement("aside");
panel.className = "row";
panel.style.marginBottom = "16px";
panel.innerHTML = `<span id="update-status" class="hint" role="status"></span>
  <button id="check-update">更新を確認</button>
  <button id="offer-update" hidden>更新する</button>
  <span id="update-confirm" hidden>
    <span id="update-question"></span>
    <button id="install-update">インストールする</button>
    <button id="decline-update">やめる</button>
  </span>
  <button id="restart-update" hidden>再起動する</button>`;
document.querySelector("section").before(panel);
const $ = (id) => document.getElementById(id);
let state = null;

function render(status) {
  state = status;
  $("update-status").textContent = `v${status.current_version} · ${status.message}`;
  const busy = ["checking", "installing"].includes(status.phase);
  $("check-update").disabled = busy || status.phase === "restart_required";
  $("offer-update").hidden = status.phase !== "available";
  $("restart-update").hidden = status.phase !== "restart_required";
  if (status.phase !== "available") $("update-confirm").hidden = true;
  if (status.phase === "installing" && status.total) {
    $("update-status").textContent += ` (${Math.min(100, Math.round(100 * status.downloaded / status.total))}%)`;
  }
}
function showError(error) { $("update-status").textContent = String(error); }
$("check-update").addEventListener("click", () => { void invoke("check_updates").catch(showError); });
$("offer-update").addEventListener("click", () => {
  $("update-question").textContent = `バージョン ${state.version} をダウンロードしてインストールしますか？`;
  $("update-confirm").hidden = false;
  $("offer-update").hidden = true;
});
$("decline-update").addEventListener("click", () => { $("update-confirm").hidden = true; render(state); });
$("install-update").addEventListener("click", () => {
  const version = state.version;
  $("update-confirm").hidden = true;
  void invoke("install_update", { version, approved: true }).catch(showError);
});
$("restart-update").addEventListener("click", () => { void invoke("restart_after_update").catch(showError); });
await listen("update-status", ({ payload }) => render(payload));
render(await invoke("update_status"));
