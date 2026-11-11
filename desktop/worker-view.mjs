import { settingsPage } from "./settings-view.mjs";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const style = document.createElement("style");
style.textContent = `#worker-registration form { display: grid; gap: 8px; margin-top: 12px; }
  #worker-registration input[name] { width: 100%; min-width: 0; }
  #worker-registration form p { margin: 0; }`;
document.head.append(style);
const panel = document.createElement("details");
panel.id = "worker-registration";
panel.innerHTML = `<summary>翻訳の処理を提供する</summary>
  <p class="hint">登録すると、この端末でほかの人の文を処理するための資格を取得します。処理する内容は選べず、推論中はGPUとメモリを使います。</p>
  <form>
    <label for="worker-server-url">提供先のサーバーURL</label>
    <input id="worker-server-url" name="server_url" type="url" required autocomplete="off" spellcheck="false">
    <label for="worker-discovery-url">提供先のOIDC discovery URL</label>
    <input id="worker-discovery-url" name="discovery_url" type="url" required autocomplete="off" spellcheck="false">
    <label for="worker-client-id">提供先の公開client ID</label>
    <input id="worker-client-id" name="client_id" required maxlength="256" autocomplete="off" spellcheck="false">
    <p class="hint">日本国内から参加し、処理した内容を記録・目的外利用しないでください。エージェントを改変して内容を取り出すことも禁止します。違反時には資格を失効します。</p>
    <label><input id="worker-terms" type="checkbox" required> 参加の規約に同意する</label>
    <div>
      <button type="submit">登録する</button>
      <button type="button" id="worker-renew" hidden>資格を更新</button>
      <button type="button" id="worker-revoke" hidden>登録を解除</button>
      <button type="button" id="worker-forget" hidden>この端末からだけ削除する</button>
    </div>
    <fieldset id="worker-forget-confirmation" hidden>
      <legend>端末の資格を削除</legend>
      <p class="hint">提供先の登録は期限 (最長 7 日) が切れるまで残ります。</p>
      <label><input type="checkbox" id="worker-forget-agree"> 確認して、この端末からだけ削除する</label>
      <button type="button" id="worker-forget-commit">削除する</button>
      <button type="button" id="worker-forget-cancel">キャンセル</button>
    </fieldset>
  </form>
  <p class="hint" role="status"></p>
  <label id="worker-participation" hidden><input type="checkbox"> 翻訳の処理を提供する</label>
  <p id="worker-activity" class="hint" hidden></p>`;
settingsPage.append(panel);
const form = panel.querySelector("form");
const terms = form.querySelector("#worker-terms");
const register = form.querySelector("button[type=submit]");
const renew = form.querySelector("#worker-renew");
const revoke = form.querySelector("#worker-revoke");
const status = panel.querySelector("[role=status]");
const forget = form.querySelector("#worker-forget");
const confirmation = form.querySelector("#worker-forget-confirmation");
const agree = form.querySelector("#worker-forget-agree");
const fields = [...form.querySelectorAll("input[name]")];
const activityLine = panel.querySelector("#worker-activity");
const participation = panel.querySelector("#worker-participation");
const participationToggle = participation.querySelector("input");
const phaseLabels = {
  idle: "停止中", starting: "準備中", waiting: "待機中",
  working: "処理中", paused: "一時停止中", quarantined: "検疫中", error: "処理を続けられませんでした",
};
let activity;

function renderParticipation() {
  participation.hidden = state?.expires_at == null || !state?.can_participate;
  participationToggle.checked = !!activity?.enabled;
  participationToggle.disabled = busy || !activity || (!activity.enabled && state?.expires_at <= Date.now() / 1000);
}

function renderActivity(next) {
  activity = next;
  activityLine.hidden = !next.enabled && next.phase === "idle" && next.completed === 0;
  const phase = next.phase === "error" && next.enabled
    ? "処理を続けられませんでした。しばらくして再開します"
    : phaseLabels[next.phase];
  activityLine.textContent = `${phase} · 処理した区切り ${next.completed.toLocaleString("ja-JP")} 件`;
  const notice = document.getElementById("local-worker-notice");
  if (notice) notice.hidden = !next.enabled;
  renderParticipation();
}

let state;
let busy = false;
void listen("worker-activity", ({ payload }) => renderActivity(payload)).then(async () => {
  const snapshot = await invoke("worker_activity");
  if (!activity) renderActivity(snapshot);
}).catch(() => {});

function render() {
  const registered = state?.expires_at != null;
  for (const field of fields) {
    field.disabled = busy || registered || !state?.supported;
  }
  terms.disabled = busy || registered || !state?.supported;
  register.disabled = busy || registered || !state?.supported;
  register.hidden = registered;
  renew.hidden = revoke.hidden = !registered;
  renew.disabled = revoke.disabled = busy;
  forget.hidden = !registered || !state?.can_forget;
  forget.disabled = busy;
  confirmation.disabled = busy;
  renderParticipation();
  if (!registered) {
    confirmation.hidden = true;
    agree.checked = false;
  }
  if (!state?.supported) {
    status.textContent = "この端末では翻訳の処理を提供できません。";
  } else if (registered) {
    const expiry = new Date(state.expires_at * 1000);
    status.textContent = `登録済み。資格の有効期限 ${expiry.toLocaleString("ja-JP")}`;
  } else {
    status.textContent = "未登録";
  }
  if (state?.message) status.textContent = state.message;
}

async function operate(command, args) {
  if (busy) return;
  busy = true;
  render();
  status.textContent = "処理中…";
  let error;
  try {
    state = await invoke(command, args);
    for (const field of fields) field.value = state.connection[field.name];
    terms.checked = false;
  } catch (failure) {
    error = String(failure);
    if (command === "revoke_worker" && state) state.can_forget = true;
  } finally {
    busy = false;
    render();
    if (error) status.textContent = error;
  }
}

form.addEventListener("submit", (event) => {
  event.preventDefault();
  if (!state?.supported || !terms.checked || !form.reportValidity()) return;
  const connection = Object.fromEntries(fields.map((field) => [field.name, field.value.trim()]));
  void operate("register_worker", { connection, termsVersion: state.terms_version });
});
renew.onclick = () => operate("renew_worker");
participationToggle.onchange = () => operate("set_worker_participation", { enabled: participationToggle.checked });
revoke.onclick = () => operate("revoke_worker");
forget.onclick = () => { agree.checked = false; confirmation.hidden = false; };
form.querySelector("#worker-forget-cancel").onclick = () => {
  confirmation.hidden = true;
  agree.checked = false;
};
form.querySelector("#worker-forget-commit").onclick = () => {
  if (agree.checked) void operate("forget_worker_registration", { confirmed: true });
};
render();
// 資格情報ストアを開くのは、利用者が参加設定を開いたときだけ。
panel.addEventListener("toggle", () => {
  if (panel.open && !state && !busy) void operate("worker_registration_status");
});
