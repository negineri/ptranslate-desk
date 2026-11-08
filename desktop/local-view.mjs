import { settingsPage, showPage, accountSlot, header } from "./settings-view.mjs";
const { invoke, Channel } = window.__TAURI__.core;
let resolveMode;
export const modeReady = new Promise((resolve) => { resolveMode = resolve; });
const main = document.querySelector("main");
const welcome = document.createElement("div");
welcome.id = "mode-welcome";
welcome.hidden = true;
welcome.innerHTML = `<h2>翻訳する場所を選んでください</h2>
  <p class="hint">あとから設定で切り替えられます。</p>
  <div class="mode-cards">
    <button id="welcome-local"><strong>このPCで翻訳</strong><span>ログインせず、この端末のモデルで翻訳します。初回に約1.85 GBをダウンロードします。</span></button>
    <button id="welcome-server"><strong>サーバーで翻訳</strong><span>接続先を設定してログインします。モデルのダウンロードは不要です。</span></button>
  </div><p id="welcome-status" class="hint" role="status"></p>`;
main.append(welcome);
const panel = document.createElement("aside");
panel.id = "local-settings";
panel.innerHTML = `<h3>翻訳する場所</h3>
  <div class="mode-switch"><button id="mode-local">このPCで翻訳</button><button id="mode-server">サーバーで翻訳</button></div>
  <div id="local-options" hidden>
    <p class="hint">PLaMo 3 Translate 2Bをこの端末で動かします。</p>
    <div class="setup-group">
      <h4>モデル</h4>
      <div id="local-download-area"><button id="local-download" class="primary">モデルをダウンロード</button><p class="hint">モデルと推論エンジンの合計：約1.85 GB</p></div>
      <div id="local-progress-area" hidden><progress id="local-progress" max="1" value="0"></progress><p id="local-progress-label" role="status"></p><button id="local-cancel-download">ダウンロードを中止</button></div>
      <p id="local-status" class="hint" role="status"></p>
    </div>
    <div id="local-setup" hidden>
      <div class="setup-group">
        <h4>翻訳に使うデバイス</h4>
        <div class="local-actions"><button id="local-measure">計測して候補を確認</button><button id="local-cancel-measure" hidden>計測を中止</button></div>
        <p id="local-measure-status" class="hint" role="status"></p><ol id="local-candidates"></ol>
        <label for="local-device">使用するデバイス</label><select id="local-device"><option value="none">CPU</option></select>
      </div>
      <div class="setup-group">
        <h4>同時に訳す数</h4>
        <label for="local-parallel-limit">推論の並列数</label><input id="local-parallel-limit" type="number" min="1" max="64" value="1" style="width:6em">
        <p class="hint">1〜64件を指定できます。計測すると推奨値が入ります。保存して再起動すると反映されます。</p>
      </div>
      <div class="setup-group">
        <h4>メモリ</h4>
        <label class="local-check"><input id="local-auto-release" type="checkbox"> 使っていないときにモデルのメモリを自動解放する</label>
        <p class="hint">無効の場合は、ローカルモードの起動直後にモデルを読み込みます。</p>
        <p id="local-idle-setting"><label for="local-idle-minutes">解放するまでの未使用時間</label> <select id="local-idle-minutes"><option value="1">1分</option><option value="5">5分</option><option value="10">10分</option><option value="30">30分</option><option value="60">60分</option></select></p>
      </div>
      <div class="local-actions save-row"><button id="local-start">モデルを読み込む</button><span class="spacer"></span><button id="local-save" class="primary">保存して再起動</button></div>
    </div>
    <div class="setup-group">
      <h4>Chrome連携</h4>
      <p class="hint">拡張の設定で「接続」を押すと、IDを入力せずに連携できます。</p>
      <details><summary>拡張IDを手動で登録</summary>
      <label for="chrome-extension-id">拡張ID</label><input id="chrome-extension-id" type="text" maxlength="32" autocomplete="off" spellcheck="false">
      <button id="chrome-register">Chrome連携を登録</button></details><p id="chrome-status" class="hint" role="status"></p>
    </div>
  </div><p id="mode-status" class="hint" role="status"></p>
  <dialog id="local-download-confirm" aria-labelledby="local-download-title">
    <h2 id="local-download-title">翻訳モデルをダウンロードしますか？</h2>
    <p>PLaMo 3 Translate 2B（Q4_K_M、約1.81 GB）と、推論エンジン（約37 MB）をこのPCに保存します。</p>
    <p>Hugging FaceとGitHubから取得します。通信量は約1.85 GB、空き容量は少なくとも約2 GB必要です。</p>
    <p><a id="local-model-information" href="https://huggingface.co/pfnet/plamo-3-translate-2b-GGUF" target="_blank" rel="noreferrer">モデルの情報</a></p>
    <p id="local-model-information-status" class="hint" role="status"></p>
    <div class="local-actions"><button id="local-confirm-download" class="primary">ダウンロードする</button><button id="local-decline-download" autofocus>キャンセル</button></div>
  </dialog>`;
settingsPage.querySelector(".settings-heading").after(panel);
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
style.textContent = `#mode-welcome { max-width:800px; margin:48px auto; }
  .mode-cards { display:grid; grid-template-columns:repeat(2,minmax(0,1fr)); gap:16px; margin-top:24px; }
  .mode-cards button { text-align:left; padding:24px; white-space:normal; }
  .mode-cards strong,.mode-cards span { display:block; } .mode-cards strong { font-size:1.1rem; margin-bottom:12px; }
  .mode-cards span { color:var(--muted); line-height:1.6; }
  #local-settings h3 { margin-top:0; } .mode-switch,.local-actions { display:flex; flex-wrap:wrap; gap:8px; margin:12px 0; }
  .mode-switch button[aria-pressed=true] { border-color:var(--accent); color:var(--accent); }
  #local-device { display:block; max-width:100%; margin:8px 0 20px; }
  .local-check { display:flex; align-items:center; gap:8px; } #local-options progress { width:100%; }
  #local-candidates li { margin:8px 0; } #local-candidates button { margin-left:8px; }
  #local-download-confirm { max-width:520px; } #local-runtime-status { margin:8px 0; }
  .setup-group { margin-top:16px; padding-top:14px; border-top:1px solid var(--line); } .setup-group h4 { margin:0 0 8px; font-size:.95rem; }
  .setup-group + .setup-group { padding-top:16px; }
  .save-row { margin:20px 0 4px; }
  @media(max-width:600px) { .mode-cards { grid-template-columns:1fr; } #mode-welcome { margin:24px 0; } }`;
document.head.append(style);
const $ = (id) => document.getElementById(id);
let state, downloading = false, measuring = false, detecting = 0, draftLocal = false, modeVersion = 0;
function message(id, text, failed = false) {
  $(id).textContent = String(text); $(id).className = failed ? "hint err" : "hint";
  $(id).setAttribute("role", failed ? "alert" : "status");
}
function error(e) { message("local-status", e, true); }
function settings() {
  return { enabled: true, device: $("local-device").value, auto_release: $("local-auto-release").checked,
    idle_minutes: Number($("local-idle-minutes").value), parallel: Number($("local-parallel-limit").value) };
}
function busy() {
  $("local-setup").hidden = !state?.available;
  $("local-save").disabled = downloading || measuring || detecting > 0 || !state?.available;
  $("local-download").disabled = downloading || measuring;
  $("local-measure").disabled = downloading || measuring || detecting > 0 || !state?.available;
  $("local-device").disabled = measuring || downloading;
  $("local-parallel-limit").disabled = measuring || downloading;
  $("local-cancel-measure").hidden = !measuring;
  $("local-start").disabled = !state?.enabled || downloading || measuring || state?.loading;
  $("mode-server").disabled = downloading || measuring;
}
function selectDraft(local) {
  draftLocal = local; modeVersion++;
  $("local-options").hidden = !local;
  $("mode-local").setAttribute("aria-pressed", String(local));
  $("mode-server").setAttribute("aria-pressed", String(!local));
  $("connection-settings").hidden = local;
}
function hideWelcome() {
  welcome.hidden = true; main.querySelector(".page-tabs").hidden = false;
  accountSlot.hidden = false; showPage(true);
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
async function measure() {
  if (measuring || downloading || !state.available) return;
  measuring = true; busy(); $("local-candidates").replaceChildren();
  message("local-measure-status", "短文でデバイスを比較し、ページ翻訳に近い条件で並列数を計測します…");
  const progress = new Channel();
  progress.onmessage = (r) => message("local-measure-status", r.measuring_parallel
    ? `${r.name}：${r.measuring_parallel}並列でページ相当の短文を計測しています…`
    : r.milliseconds === null && !r.error ? `${r.name}を計測しています…` : `${r.name}の計測が終わりました`);
  try {
    const results = await invoke("measure_local_devices", { progress });
    const fastest = results.find((r) => r.milliseconds !== null);
    for (const r of results) {
      if (![...$("local-device").options].some((o) => o.value === r.id)) $("local-device").add(new Option(r.name, r.id));
      const li = document.createElement("li");
      const seconds = r.milliseconds === null ? r.error : `短文の翻訳 約${(r.milliseconds / 1000).toFixed(2)}秒${r === fastest ? "（おすすめ）" : ""}`;
      li.append(document.createTextNode(`${r.name}：${seconds}`));
      if (r.recommended_parallel) {
        const result = document.createElement("p");
        result.textContent = `推奨${r.recommended_parallel}並列（約${r.bulk_per_second.toFixed(1)}件/秒）`;
        li.append(result);
      }
      if (r.bulk_error) {
        const note = document.createElement("p"); note.className = "hint";
        note.textContent = r.recommended_parallel ? `完了した候補からの推奨です。${r.bulk_error}` : `大量翻訳の推奨値は取得できませんでした。${r.bulk_error}`;
        li.append(note);
      }
      $("local-candidates").append(li);
    }
    if (fastest) {
      $("local-device").value = fastest.id;
      $("local-parallel-limit").value = String(fastest.recommended_parallel ?? 1);
    }
    message("local-measure-status", fastest ? "推奨値を設定欄に反映しました。「保存して再起動」で適用します。" : "計測できませんでした。設定欄からデバイスと並列数を指定できます。", !fastest);
  } catch (e) { message("local-measure-status", e, true); }
  finally { measuring = false; busy(); }
}
async function chooseLocal() {
  hideWelcome(); selectDraft(true);
  const version = modeVersion;
  if (!state.configured) accountSlot.hidden = true;
  if (state.available) { await devices(); if (draftLocal && modeVersion === version) await measure(); }
  else message("local-status", "モデルをダウンロードすると、このPCで翻訳できます。");
}
async function chooseServer() {
  try {
    if (state.enabled) { await invoke("save_local_settings", { settings: { ...settings(), enabled: false } }); return; }
    await invoke("select_server_mode"); hideWelcome(); selectDraft(false);
    $("connection-settings").open = true;
    message("mode-status", "接続先を設定してログインしてください。"); resolveMode();
  } catch (e) { message("mode-status", e, true); }
}
$("welcome-local").onclick = () => void chooseLocal().catch(error);
$("welcome-server").onclick = () => void chooseServer();
$("mode-local").onclick = () => void chooseLocal().catch(error);
$("mode-server").onclick = () => void chooseServer();
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
$("local-confirm-download").onclick = async () => {
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
  if (state.available) { await devices(); await measure(); }
};
$("local-cancel-download").onclick = () => void invoke("cancel_local_download").catch(error);
$("local-save").onclick = async () => {
  $("local-save").disabled = true; message("local-status", "設定を保存して再起動しています…");
  try { await invoke("save_local_settings", { settings: settings() }); } catch (e) { error(e); busy(); }
};
$("local-start").onclick = async () => {
  message("local-status", "モデルを読み込んでいます…");
  try { await invoke("start_local_engine"); message("local-status", "モデルの準備ができました。"); } catch (e) { error(e); }
};

state = await invoke("local_status");
if (state.chrome_error) message("chrome-status", state.chrome_error, true);
$("local-parallel-limit").value = String(state.parallel);
$("local-auto-release").checked = state.auto_release;
if (![...$("local-idle-minutes").options].some((o) => Number(o.value) === state.idle_minutes)) $("local-idle-minutes").add(new Option(`${state.idle_minutes}分`, String(state.idle_minutes)));
$("local-idle-minutes").value = String(state.idle_minutes); $("local-idle-setting").hidden = !state.auto_release;
if (![...$("local-device").options].some((o) => o.value === state.device)) $("local-device").add(new Option(state.device === "auto" ? "自動（エンジンに任せる）" : state.device, state.device));
$("local-device").value = state.device; $("local-download-area").hidden = state.available;
$("welcome-local").disabled = $("mode-local").disabled = !state.supported;
if (!state.supported) message("welcome-status", "このPCでの翻訳はWindows x64とmacOS 13.3以降（Apple Silicon・Intel）に対応しています。");
selectDraft(state.enabled);
if (!state.configured) {
  welcome.hidden = false; showPage(true); settingsPage.hidden = true;
  main.querySelector(".page-tabs").hidden = true; accountSlot.hidden = true;
} else resolveMode();
busy();
if (state.enabled && state.available && state.supported) void devices();
async function refreshRuntime() {
  state = await invoke("local_status");
  if (state.chrome_error) message("chrome-status", state.chrome_error, true);
  runtime.hidden = !state.enabled;
  header.querySelector(".sub").textContent = state.enabled ? "PLaMo 3 Translate · このPCで翻訳" : state.configured ? "PLaMo 3 Translate · 原文と訳はサーバーに残りません" : "PLaMo 3 Translate";
  runtime.textContent = state.error || (state.loading ? "翻訳モデルを読み込んでいます…" : state.ready ? "ローカル翻訳の準備ができました" : state.auto_release ? "翻訳するときにモデルを読み込みます" : "モデルは解放されています。次の翻訳時に読み込みます");
  runtime.className = state.error ? "hint err" : "hint"; busy();
}
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
