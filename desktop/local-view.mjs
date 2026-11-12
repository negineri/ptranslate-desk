import { settingsPage, header, stageSettings } from "./settings-view.mjs";
const { invoke, Channel } = window.__TAURI__.core;
let resolveMode;
export const modeReady = new Promise((resolve) => { resolveMode = resolve; });
document.addEventListener("click", async (event) => {
  const link = event.target.closest("a[data-plamo-license]");
  if (!link) return;
  event.preventDefault();
  const status = document.querySelector("#setup-wizard #wizard-status")
    ?? document.querySelector("#local-download-confirm[open] #local-model-information-status")
    ?? document.getElementById("local-status");
  try { await invoke("open_plamo_license", { registration: link.dataset.plamoLicense === "registration" }); }
  catch (error) {
    if (status) {
      status.textContent = String(error);
      status.className = "hint err";
    }
  }
});
const main = document.querySelector("main");
const panel = document.createElement("details");
panel.id = "local-settings";
panel.innerHTML = `
  <summary id="local-settings-title">ローカル翻訳の設定</summary>
  <p id="local-worker-notice" class="hint" hidden>翻訳の処理を提供している間は、推論エンジンが動き続けます</p>
  <div id="local-options">
    <p class="hint">PLaMo 3 Translate 2Bをこの端末で動かします。<a data-plamo-license href="https://plamo.preferredai.jp/info/plamo-community-license-ja" target="_blank" rel="noreferrer">PLaMoコミュニティライセンス</a></p>
    <div class="setup-group" id="local-model-controls">
      <h4>モデル</h4>
      <div id="local-download-area"><button id="local-download" class="primary">モデルをダウンロード</button><p class="hint">モデルと推論エンジンの合計：約1.85 GB</p></div>
      <div id="local-progress-area" hidden><progress id="local-progress" max="1" value="0"></progress><p id="local-progress-label" role="status"></p><button id="local-cancel-download">ダウンロードを中止</button></div>
    </div>
    <div id="local-setup" hidden>
      <div class="setup-group" id="local-device-controls">
        <h4>翻訳に使うデバイス</h4>
        <div class="local-actions"><button id="local-measure">計測して候補を確認</button><button id="local-cancel-measure" hidden>計測を中止</button></div>
        <progress id="local-measure-progress" aria-label="計測の進捗" hidden></progress>
        <p id="local-measure-status" class="hint" role="status"></p><ol id="local-candidates"></ol>
        <label for="local-device">使用するデバイス</label><select id="local-device"><option value="none">CPU</option></select>
      </div>
      <div class="setup-group" id="local-parallel-controls">
        <h4>同時に訳す数</h4>
        <label for="local-parallel-limit">推論の並列数</label><input id="local-parallel-limit" type="number" min="1" max="64" value="1" required style="width:6em">
        <p class="hint">1〜64件を指定できます。計測すると推奨値が入ります。適用すると反映されます。</p>
      </div>
      <div class="setup-group">
        <h4>メモリ</h4>
        <label class="local-check"><input id="local-auto-release" type="checkbox"> 使っていないときにモデルのメモリを自動解放する</label>
        <p class="hint">無効の場合は、ローカルモードの起動直後にモデルを読み込みます。</p>
        <p id="local-idle-setting"><label for="local-idle-minutes">解放するまでの未使用時間</label> <select id="local-idle-minutes"><option value="1">1分</option><option value="5">5分</option><option value="10">10分</option><option value="30">30分</option><option value="60">60分</option></select></p>
      </div>
    </div>
    <div class="setup-group">
      <h4>Chrome連携</h4>
      <p class="hint">拡張の設定で「接続」を押すと、IDを入力せずに連携できます。</p>
      <details><summary>拡張IDを手動で登録</summary>
      <label for="chrome-extension-id">拡張ID</label><input id="chrome-extension-id" type="text" maxlength="32" autocomplete="off" spellcheck="false">
      <button id="chrome-register">Chrome連携を登録</button></details><p id="chrome-status" class="hint" role="status"></p>
    </div>
  </div>
  <dialog id="local-download-confirm" aria-labelledby="local-download-title">
    <h2 id="local-download-title">翻訳モデルをダウンロードしますか？</h2>
    <p>PLaMo 3 Translate 2B（Q4_K_M、約1.81 GB）と、推論エンジン（約37 MB）をこのPCに保存します。</p>
    <p>Hugging FaceとGitHubから取得します。通信量は約1.85 GB、空き容量は少なくとも約2 GB必要です。</p>
    <p><a id="local-model-information" href="https://huggingface.co/pfnet/plamo-3-translate-2b-GGUF" target="_blank" rel="noreferrer">モデルの情報</a></p>
    <p><a data-plamo-license href="https://plamo.preferredai.jp/info/plamo-community-license-ja" target="_blank" rel="noreferrer">PLaMoコミュニティライセンス</a>をご確認ください。</p>
    <p id="local-model-information-status" class="hint" role="status"></p>
    <div class="local-actions"><button id="local-confirm-download" class="primary">ダウンロードする</button><button id="local-decline-download" autofocus>キャンセル</button></div>
  </dialog>`;
const localPanel = document.createElement("div");
localPanel.className = "translation-settings-panel";
const localAccess = document.createElement("div");
localAccess.className = "translation-settings-switch";
localAccess.innerHTML = `<span id="local-active-status" class="hint" role="status"></span><input id="local-active" type="checkbox" role="switch" aria-label="ローカル翻訳" disabled>`;
const localStatus = document.createElement("p");
localStatus.id = "local-status";
localStatus.className = "hint";
localStatus.setAttribute("role", "status");
localPanel.append(panel, localAccess, localStatus);
settingsPage.querySelector(".settings-heading").after(localPanel);
const chromeDialog = document.createElement("dialog");
chromeDialog.id = "chrome-registration-confirm";
chromeDialog.setAttribute("aria-labelledby", "chrome-registration-title");
chromeDialog.innerHTML = `<h2 id="chrome-registration-title">Chrome拡張との連携を登録しますか？</h2>
  <p>この拡張が、デスクトップのローカル翻訳を利用できるようになります。</p>
  <p>拡張の設定で「接続」を押して開いた場合だけ登録してください。</p>
  <p>拡張ID：<code id="chrome-registration-id"></code></p>
  <p id="chrome-registration-status" class="hint" role="status"></p>
  <div class="local-actions"><button id="chrome-registration-approve" class="primary">登録する</button><button id="chrome-registration-cancel" autofocus>キャンセル</button></div>`;
main.append(chromeDialog);
const chromeNotice = document.createElement("p");
chromeNotice.id = "chrome-registration-result"; chromeNotice.className = "hint"; chromeNotice.setAttribute("role", "status");
main.append(chromeNotice);

const runtime = document.createElement("p");
runtime.id = "local-runtime-status"; runtime.className = "hint"; runtime.hidden = true; runtime.setAttribute("role", "status");
header.after(runtime);
const style = document.createElement("style");
style.textContent = `#local-settings h3 { margin-top:0; } #local-options { margin-top:20px; }
  #local-measure-progress { width: 100%; accent-color: var(--accent); }
  .local-actions { display:flex; flex-wrap:wrap; gap:8px; margin:12px 0; }
  #local-device { display:block; max-width:100%; margin:8px 0 0; }
  #chrome-extension-id { width:100%; max-width:22em; }
  label[for="local-parallel-limit"] { margin-inline-end:8px; }
  .local-check { display:flex; align-items:center; gap:8px; } #local-options progress { width:100%; }
  #local-candidates { list-style:none; padding-inline-start:0; }
  #local-candidates li { margin:8px 0; }
  .local-benchmark { width:100%; border-collapse:collapse; margin-block:8px; font-size:.9em; }
  .local-benchmark th, .local-benchmark td { padding:5px 6px; text-align:start; border-bottom:1px solid var(--line); }
  #local-candidates label { display:flex; align-items:center; gap:8px; cursor:pointer; }
  #local-download-confirm { max-width:520px; } #local-runtime-status { margin:8px 0; }
  .setup-group { margin-top:16px; padding-top:14px; border-top:1px solid var(--line); } .setup-group h4 { margin:0 0 8px; font-size:.95rem; }
  .setup-group + .setup-group { padding-top:16px; }
  .save-row { margin:20px 0 4px; }
`;
document.head.append(style);
const $ = (id) => document.getElementById(id);
let state, downloading = false, measuring = false, detecting = 0, pendingLocal = null;
let measurementCompleted = false;
const localActiveKey = "ptranslate-desktop-local-active";
let saving = false;
let switchRevision = 0;
function message(id, text, failed = false) {
  $(id).textContent = String(text); $(id).className = failed ? "hint err" : "hint";
  $(id).setAttribute("role", failed ? "alert" : "status");
}
function error(e) { message("local-status", e, true); }
function settings() {
  return { enabled: state.enabled, device: $("local-device").value, auto_release: $("local-auto-release").checked,
    idle_minutes: Number($("local-idle-minutes").value), parallel: Number($("local-parallel-limit").value), measurement_completed: measurementCompleted };
}
function busy() {
  $("local-setup").hidden = !state?.available;
  $("local-active").checked = pendingLocal ?? (state.available && state.active);
  $("local-active-status").textContent = pendingLocal !== null ? pendingLocal ? "読み込み中" : "解放中"
    : !state.available ? "未ダウンロード" : state.loading ? "読み込み中"
      : state.ready ? "読み込み済み" : state.active ? "待機中" : "オフ";
  $("local-active").disabled = !state.supported || !state.available || pendingLocal !== null || state.loading || saving || downloading || measuring;
  $("local-download").disabled = downloading || measuring;
  $("local-measure").disabled = pendingLocal !== null || state.loading || downloading || measuring || detecting > 0 || !state?.available;
  $("local-device").disabled = measuring || downloading;
  $("local-parallel-limit").disabled = measuring || downloading;
  $("local-cancel-measure").hidden = !measuring;
}
async function devices() {
  detecting++; busy();
  const options = [new Option("CPU", "none")];
  try { for (const d of await invoke("local_devices")) options.push(new Option(d.name, d.id)); }
  catch (e) { message("local-measure-status", `${e} CPUは手動で選択できます。`, true); }
  finally { detecting--; busy(); }
  const value = $("local-device").value;
  if (!options.some((o) => o.value === value)) options.push(new Option(value === "auto" ? "自動（エンジンに任せる）" : value, value));
  $("local-device").replaceChildren(...options); $("local-device").value = value;
}
export async function measure() {
  if (measuring || downloading || !state.available) return;
  measuring = true; busy(); $("local-candidates").replaceChildren();
  const meter = $("local-measure-progress");
  meter.hidden = false;
  meter.removeAttribute("value");
  message("local-measure-status", "計測するデバイスを確認しています…");
  const progress = new Channel();
  progress.onmessage = (update) => {
    if (!measuring) return;
    if (update.total > 0) {
      meter.max = update.total;
      meter.value = update.completed;
    } else {
      meter.removeAttribute("value");
    }
    if (update.phase === "devices") {
      meter.setAttribute("aria-label", "デバイス比較の進捗");
      message("local-measure-status", `デバイス比較 ${update.completed + 1}/${update.total} · ${update.name}を計測中`);
    } else {
      meter.setAttribute("aria-label", "並列数候補の進捗");
      const detail = update.total ? `${update.completed}/${update.total}件 完了` : "モデルの準備中";
      message("local-measure-status", `並列数の候補 ${update.candidate}/${update.candidates} · ${update.name}・${update.parallel}並列 · ${detail}`);
    }
  };
  try {
    const results = await invoke("measure_local_devices", { progress });
    const recommended = results.find((r) => r.recommended_parallel) ?? results.find((r) => r.milliseconds !== null);
    for (const r of results) {
      if (![...$("local-device").options].some((o) => o.value === r.id)) $("local-device").add(new Option(r.name, r.id));
      const li = document.createElement("li");
      const seconds = r.milliseconds === null ? r.error : `短文の翻訳 約${(r.milliseconds / 1000).toFixed(2)}秒${r === recommended ? "（おすすめのデバイス）" : ""}`;
      const label = document.createElement("label");
      const radio = document.createElement("input");
      radio.type = "radio";
      radio.name = "local-device-candidate";
      radio.value = r.id;
      radio.disabled = r.milliseconds === null;
      radio.onchange = () => {
        $("local-device").value = r.id;
        $("local-parallel-limit").value = String(r.recommended_parallel ?? 1);
        stageLocalSettings();
      };
      label.append(radio, document.createTextNode(`${r.name}：${seconds}`));
      li.append(label);
      if (r.parallel_samples?.length) {
        const table = document.createElement("table");
        table.className = "local-benchmark";
        table.createCaption().textContent = "長文の計測結果（約1,500字／件）";
        const header = table.createTHead().insertRow();
        for (const title of ["並列数", "処理速度", "見込みメモリ"]) {
          const th = document.createElement("th");
          th.scope = "col";
          th.textContent = title;
          header.append(th);
        }
        const body = table.createTBody();
        for (const { parallel, requests_per_second, memory } of r.parallel_samples) {
          const row = body.insertRow();
          row.insertCell().textContent = parallel === r.recommended_parallel ? `${parallel}（推奨）` : parallel;
          row.insertCell().textContent = `${(requests_per_second * 60).toFixed(1)}件/分`;
          const mib = (size) => `${size.toLocaleString("ja-JP")} MiB`;
          row.insertCell().textContent = memory.shared
            ? `${mib(memory.system_mib)}（共有）`
            : `RAM ${mib(memory.system_mib)} · VRAM ${mib(memory.gpu_mib)}`;
        }
        li.append(table);
      }
      if (r.bulk_error) {
        const note = document.createElement("p"); note.className = "hint";
        note.textContent = r.recommended_parallel ? `完了した候補からの推奨です。${r.bulk_error}` : `長文翻訳の推奨値は取得できませんでした。${r.bulk_error}`;
        li.append(note);
      }
      $("local-candidates").append(li);
    }
    if (recommended) {
      measurementCompleted = true;
      $("local-device").value = recommended.id;
      $("local-parallel-limit").value = String(recommended.recommended_parallel ?? 1);
    }
    syncDeviceCandidates();
    message("local-measure-status", recommended ? "推奨値を設定欄に反映しました。「適用」で反映します。" : "計測できませんでした。設定欄からデバイスと並列数を指定できます。", !recommended);
  } catch (e) { message("local-measure-status", e, true); }
  finally { measuring = false; meter.hidden = true; busy(); stageLocalSettings(); }
}
$("chrome-register").onclick = async () => {
  try {
    const extensionId = $("chrome-extension-id").value.trim();
    if (!/^[a-p]{32}$/.test(extensionId)) throw new Error("拡張IDはa〜pの32文字で入力してください");
    await invoke("register_chrome", { extensionId }); message("chrome-status", "登録しました。拡張の設定から接続を確かめてください。");
  } catch (e) { message("chrome-status", e, true); }
};
$("local-measure").onclick = () => { void measure(); };
$("local-cancel-measure").onclick = () => void invoke("cancel_local_measurement").catch(error);
$("local-auto-release").onchange = () => { $("local-idle-setting").hidden = !$("local-auto-release").checked; };
$("local-download").onclick = () => { $("local-download-confirm").showModal(); $("local-decline-download").focus(); };
$("local-model-information").onclick = async (event) => {
  event.preventDefault();
  message("local-model-information-status", "");
  try { await invoke("open_model_information"); }
  catch (e) { message("local-model-information-status", e, true); }
};
$("local-decline-download").onclick = () => $("local-download-confirm").close();
export async function downloadModel(measureAfter = true) {
  $("local-download-confirm").close(); message("local-status", ""); downloading = true; busy(); $("local-progress-area").hidden = false;
  const progress = new Channel();
  progress.onmessage = (r) => {
    const item = r.item === "engine" ? "推論エンジン" : "翻訳モデル";
    if (r.phase === "download") {
      $("local-progress").value = r.total ? r.received / r.total : 0;
      message("local-progress-label", `${item}：${(r.received / 1024 ** 2).toFixed(1)} / ${(r.total / 1024 ** 2).toFixed(1)} MiB（${Math.floor(r.received / r.total * 100)}%）`);
    } else { $("local-progress").removeAttribute("value"); message("local-progress-label", r.phase === "verify" ? `${item}を検証しています…` : r.phase === "extract" ? "推論エンジンを準備しています…" : "ダウンロードが完了しました"); }
  };
  try {
    await invoke("download_local_resources", { confirmed: true, progress }); state = await invoke("local_status");
    $("local-download-area").hidden = state.available; message("local-status", "モデルの準備ができました。");
  } catch (e) { error(e); }
  finally { downloading = false; $("local-progress-area").hidden = true; busy(); }
  if (state.available) {
    await devices();
    if (measureAfter) await measure();
  }
}
$("local-confirm-download").onclick = () => void downloadModel();
$("local-cancel-download").onclick = () => void invoke("cancel_local_download").catch(error);
export async function saveSetupDevice() {
  if (!$("local-parallel-limit").reportValidity()) return false;
  const draft = { ...settings(), enabled: true };
  await invoke("save_local_settings", { settings: draft });
  state = { ...state, ...draft };
  stageSettings("local", null);
  return true;
}
function syncDeviceCandidates() {
  for (const radio of $("local-candidates").querySelectorAll("input")) radio.checked = radio.value === $("local-device").value;
}
$("local-device").addEventListener("change", syncDeviceCandidates);
export function resetLocalFields() {
  $("local-device").value = state.device;
  $("local-parallel-limit").value = String(state.parallel);
  $("local-auto-release").checked = state.auto_release;
  $("local-idle-minutes").value = String(state.idle_minutes);
  $("local-idle-setting").hidden = !state.auto_release;
  measurementCompleted = state.measurement_completed;
  syncDeviceCandidates();
}
function stageLocalSettings() {
  if (document.getElementById("setup-wizard")) return;
  const draft = settings();
  const changed = state.available &&
    ["device", "parallel", "auto_release", "idle_minutes"].some((key) => draft[key] !== state[key]);
  stageSettings("local", changed ? {
    validate() {
      if (pendingLocal !== null || downloading || measuring || state.loading) {
        error("ローカル翻訳の処理が終わってから適用してください。");
        return false;
      }
      panel.open = true;
      return $("local-parallel-limit").reportValidity() && draft.parallel >= 1;
    },
    async apply() {
      saving = true;
      busy();
      try {
        await invoke("save_local_settings", { settings: draft });
        state = { ...state, ...draft };
        document.dispatchEvent(new Event("translation-settings-applied"));
        message("local-status", "ローカル翻訳の設定を適用しました。");
      } finally {
        saving = false;
        busy();
      }
    },
    cancel: resetLocalFields,
  } : null);
}
panel.addEventListener("input", stageLocalSettings);
$("local-active").onchange = async (event) => {
  const active = event.target.checked;
  pendingLocal = active;
  switchRevision++;
  busy();
  message("local-status", active ? "モデルを読み込んでいます…" : "モデルを解放しています…");
  try {
    await invoke("set_local_active", { active });
    window.localStorage.setItem(localActiveKey, String(active));
    message("local-status", active ? "ローカル翻訳をオンにしました。" : "モデルを解放しました。");
  } catch (e) { error(e); }
  finally {
    switchRevision++;
    try { await refreshRuntime(); }
    catch (e) { error(e); }
    finally {
      pendingLocal = null;
      busy();
    }
  }
};

state = await invoke("local_status");
if (window.localStorage.getItem(localActiveKey) === "false") {
  try {
    await invoke("set_local_active", { active: false });
    state = await invoke("local_status");
  } catch (e) { error(e); }
}
// 古い版で保存した明示的なデバイス設定も、モード切り替えで上書きしない。
measurementCompleted = state.measurement_completed || (state.configured && state.device !== "auto");
if (state.chrome_error) message("chrome-status", state.chrome_error, true);
$("local-parallel-limit").value = String(state.parallel);
$("local-auto-release").checked = state.auto_release;
if (![...$("local-idle-minutes").options].some((o) => Number(o.value) === state.idle_minutes)) $("local-idle-minutes").add(new Option(`${state.idle_minutes}分`, String(state.idle_minutes)));
$("local-idle-minutes").value = String(state.idle_minutes); $("local-idle-setting").hidden = !state.auto_release;
if (![...$("local-device").options].some((o) => o.value === state.device)) $("local-device").add(new Option(state.device === "auto" ? "自動（エンジンに任せる）" : state.device, state.device));
$("local-device").value = state.device; $("local-download-area").hidden = state.available;
if (!state.supported) message("local-status", "このPCではローカル翻訳を利用できません。");
resolveMode();
busy();
if (state.available && state.supported) void devices();
let temporaryLocal;
async function refreshRuntime() {
  const revision = switchRevision;
  const status = await invoke("local_status");
  if (revision !== switchRevision) return;
  state = status;
  if (state.chrome_error) message("chrome-status", state.chrome_error, true);
  const localButton = $("destination-local");
  if (localButton) {
    const available = state.available && state.supported && state.active;
    localButton.dataset.available = String(available);
    localButton.querySelector("small").textContent = available ? "有効" : "無効";
    localButton.title = available ? "このPCで翻訳" : "ローカル翻訳の設定を開く";
  }
  runtime.hidden = !(temporaryLocal ?? state.enabled);
  header.querySelector(".sub").textContent = "PLaMo 3 Translate";
  runtime.textContent = state.error || (!state.active ? "ローカル翻訳はオフです" : state.loading ? "翻訳モデルを読み込んでいます…" : state.ready ? "ローカル翻訳の準備ができました" : state.auto_release ? "翻訳するときにモデルを読み込みます" : "モデルは解放されています。次の翻訳時に読み込みます");
  runtime.className = state.error ? "hint err" : "hint"; busy();
}
document.addEventListener("translation-destination-changed", ({ detail }) => {
  temporaryLocal = detail;
  runtime.hidden = !detail;
});
void refreshRuntime().catch(error);
setInterval(() => { void refreshRuntime().catch(error); }, 1000);

// イベントの到着が画面の初期化より早くても、保留中の要求を取得できる。
let chromeRequest = null;
let registeringChrome = false;
async function showChromeRegistration() {
  const pending = await invoke("chrome_registration_request");
  if (!pending || chromeDialog.open || registeringChrome) return;
  chromeRequest = pending;
  $("chrome-registration-id").textContent = pending.extension_id;
  message("chrome-registration-status", "");
  chromeDialog.showModal();
}
async function finishChromeRegistration(approve) {
  if (!chromeRequest || registeringChrome) return;
  registeringChrome = true;
  $("chrome-registration-approve").disabled = $("chrome-registration-cancel").disabled = true;
  try {
    await invoke("finish_chrome_registration", { token: chromeRequest.token, approve });
    if (approve) {
      $("chrome-extension-id").value = chromeRequest.extension_id;
      message("chrome-status", "登録しました。拡張に戻り、接続を確かめてください。");
      message("chrome-registration-result", "Chrome連携を登録しました。拡張に戻り、接続を確かめてください。");
    }
  } catch (e) { message("chrome-registration-result", e, true); }
  finally {
    chromeRequest = null;
    chromeDialog.close();
    registeringChrome = false;
    $("chrome-registration-approve").disabled = $("chrome-registration-cancel").disabled = false;
    void showChromeRegistration().catch(error);
  }
}
$("chrome-registration-approve").onclick = () => { void finishChromeRegistration(true); };
$("chrome-registration-cancel").onclick = () => { void finishChromeRegistration(false); };
chromeDialog.addEventListener("cancel", (event) => { event.preventDefault(); void finishChromeRegistration(false); });
await window.__TAURI__.event.listen("chrome-registration-request", () => { void showChromeRegistration().catch(error); });
await showChromeRegistration();
