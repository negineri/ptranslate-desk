import { settingsPage } from "./settings-view.mjs";
import { downloadModel, measure, saveSetupDevice, resetLocalFields } from "./local-view.mjs";
import { applySetupConnection, resetConnectionFields } from "./connection.mjs";

const { invoke } = window.__TAURI__.core;
const storage = window.localStorage;
const profiles = {
  local: ["usage", "license", "model", "device", "shortcut", "done"],
  server: ["usage", "license", "connection", "login", "shortcut", "done"],
  both: ["usage", "license", "model", "device", "connection", "login", "shortcut", "done"],
};
const labels = { usage: "使い方", license: "ライセンス", model: "モデル", device: "デバイス", connection: "接続先", login: "ログイン", shortcut: "ショートカット", done: "完了" };

export async function runSetup(local, existingSettings, { manual = false, saveShortcut } = {}) {
  const previous = await invoke("setup_state");
  if (!manual && (previous === "done" || (!previous && (local.configured || local.available || existingSettings)))) return null;

  if (!manual) await invoke("setup_state", { state: "started" });
  const style = document.createElement("style");
  style.textContent = `
    .setup-running > :is(.page-tabs, #settings-page, #translation-page, #local-runtime-status), .setup-running #header-account { display: none; }
    #setup-wizard { max-width: 680px; margin: 20px auto 0; }
    #wizard-steps { display: flex; flex-wrap: wrap; gap: 6px; margin: 16px 0 12px; }
    #wizard-steps span { font-size: .78rem; color: var(--muted); border: 1px solid var(--line); border-radius: 999px; padding: 1px 10px; }
    #wizard-steps [aria-current="step"] { color: var(--accent-text); background: var(--accent); border-color: var(--accent); }
    #wizard-steps .done { color: var(--ok); border-color: var(--ok); }
    #setup-wizard .wizard-card { min-height: 320px; padding: 22px; display: flex; flex-direction: column; }
    #setup-wizard h2 { font-size: 1.15rem; margin: 0 0 4px; }
    #setup-wizard .wizard-step { flex: 1; }
    #setup-wizard .wizard-nav { display: flex; align-items: center; gap: 8px; margin-top: 22px; padding-top: 14px; border-top: 1px solid var(--line); }
    .usage-cards { display: grid; gap: 10px; margin-top: 12px; }
    .usage-cards button { text-align: start; padding: 14px 16px; border: 2px solid var(--line); border-radius: 10px; }
    .usage-cards button strong, .usage-cards button span { display: block; }
    .usage-cards button span { color: var(--muted); font-size: .85rem; }
    .usage-cards button[aria-pressed="true"] { border-color: var(--accent); background: var(--accent-soft); }
    .usage-cards button[aria-pressed="true"] strong::before { content: "✓ "; color: var(--accent); }
    #wizard-summary { display: grid; grid-template-columns: auto 1fr; gap: 6px 16px; }
    #wizard-summary dt { color: var(--muted); } #wizard-summary dd { margin: 0; overflow-wrap: anywhere; }
    #setup-wizard :is(#local-model-controls, #local-device-controls) { border-top: 0; padding-top: 0; }
    #setup-wizard :is(#local-model-controls, #local-device-controls) > h4 { display: none; }
    #setup-wizard .license-points { padding-inline-start: 1.4em; }
    #setup-wizard .license-points li { margin: 8px 0; }
    #setup-wizard .license-points .clause { margin-inline-start: .4em; font-size: .8rem; color: var(--muted); }
    #setup-wizard progress { width: 100%; accent-color: var(--accent); }
  `;
  document.head.append(style);

  const wizard = document.createElement("div");
  wizard.id = "setup-wizard";
  wizard.innerHTML = `
    <div id="wizard-steps" aria-label="設定の進行状態"></div>
    <section class="wizard-card" aria-label="初期設定">
      <div class="wizard-step" data-step="usage">
        <h2>使い方を教えてください</h2>
        <p class="hint">あとから設定で変更できます。</p>
        <div class="usage-cards" role="group" aria-label="使い方">
          <button type="button" data-profile="local"><strong>このPCで翻訳</strong><span>ログイン不要。初回にモデルをダウンロードします。</span></button>
          <button type="button" data-profile="server"><strong>サーバーで翻訳</strong><span>接続先サーバーにログインして翻訳します。</span></button>
          <button type="button" data-profile="both"><strong>両方使う</strong><span>PCとサーバーを切り替えて使えます。</span></button>
        </div>
        <p id="wizard-local-unavailable" class="hint" hidden>この端末ではサーバー翻訳を利用できます。</p>
      </div>
      <div class="wizard-step" data-step="license">
        <h2>PLaMoコミュニティライセンス</h2>
        <p class="hint" id="license-hint">翻訳には株式会社Preferred Networksのモデル「PLaMo」を使います。内容を確認し、同意してモデルの取得に進みます。</p>
        <ul class="license-points">
          <li>18歳未満の場合は保護者・法定代理人の同意が必要であること<span class="clause">第2条</span></li>
          <li>利用はロイヤリティ無料。商用目的の場合は<a data-plamo-license="registration" href="https://forms.gle/mTL8tBLrMYXKNZD56" target="_blank" rel="noreferrer">PFNの公式登録ページ</a>への事前登録が必要であること<span class="clause">第3条</span></li>
          <li>翻訳の生成物は明示なしで公表できる。ただし書籍・論文など独立した長文の公表物として公開する場合は、奥付・謝辞等にPLaMoを利用した旨を記すこと<span class="clause">第5条</span></li>
          <li>モデルと生成物は「現状有姿」で提供され、PFNに保証義務はないこと<span class="clause">第7条</span></li>
        </ul>
        <p class="license-link"><a data-plamo-license href="https://plamo.preferredai.jp/info/plamo-community-license-ja" target="_blank" rel="noreferrer">全文を読む</a>（2025年2月25日制定・2026年11月9日改定）</p>
        <label class="check-row"><input id="wizard-license-agree" type="checkbox"> PLaMoコミュニティライセンスの内容に同意します</label>
      </div>
      <div class="wizard-step" data-step="model">
        <h2>翻訳モデルをダウンロード</h2>
        <p id="wizard-model-description" class="hint"></p>
      </div>
      <div class="wizard-step" data-step="device">
        <h2>翻訳に使うデバイス</h2>
        <p class="hint">速度を計測して候補を選べます。計測せずCPUで始めることもできます。</p>
      </div>
      <div class="wizard-step" data-step="connection"><h2>サーバーの接続先を設定</h2></div>
      <div class="wizard-step" data-step="login">
        <h2>サーバーにログイン</h2>
        <p class="hint">ブラウザが開き、アカウントで認証します。</p>
        <button type="button" id="wizard-login" class="primary">ログインする</button>
        <button type="button" id="wizard-cancel-login" hidden>ログインを中止</button>
        <p id="wizard-session-only" class="hint warn" hidden>OS の資格情報に保存できなかったため、アプリを終了すると再ログインが必要です。</p>
      </div>
      <div class="wizard-step" data-step="shortcut">
        <h2>翻訳のショートカット</h2>
        <p class="hint">他のアプリで選んだ文を翻訳する操作を選びます。あとから設定で変更できます。</p>
      </div>
      <div class="wizard-step" data-step="done">
        <h2>✓ セットアップ完了</h2>
        <dl id="wizard-summary"></dl>
        <p class="hint">設定はいつでも変更できます。</p>
      </div>
      <p id="wizard-status" class="hint" role="status"></p>
      <nav class="wizard-nav" aria-label="初期設定の操作">
        <button type="button" id="wizard-back">戻る</button><span class="spacer"></span>
        <button type="button" id="wizard-close" hidden>閉じる</button>
        <button type="button" class="primary" id="wizard-next">次へ</button>
      </nav>
    </section>`;
  const find = (id) => wizard.querySelector("#" + id);
  const stepElement = (step) => wizard.querySelector(`[data-step="${step}"]`);
  const tabs = document.querySelector(".page-tabs");
  const translation = document.getElementById("translation-page");
  const account = document.getElementById("header-account");
  const runtime = document.getElementById("local-runtime-status");
  const originalChildren = [tabs, translation, settingsPage, account, runtime];
  for (const element of originalChildren) element.inert = true;
  tabs.before(wizard);
  document.querySelector("main").classList.add("setup-running");

  const moves = [];
  function move(id, step) {
    const element = document.getElementById(id);
    const marker = document.createComment(id);
    element.before(marker);
    stepElement(step).append(element);
    moves.push(() => marker.replaceWith(element));
    return element;
  }
  move("local-model-controls", "model");
  move("local-status", "model");
  move("local-device-controls", "device");
  move("local-parallel-controls", "device");
  move("connection-form", "connection");
  move("shortcut-controls", "shortcut");
  const shortcut = document.getElementById("shortcut");
  const shortcutKeys = document.getElementById("shortcut-keys");
  const originalShortcut = shortcut.value;
  const originalKeys = shortcutKeys.value;
  const shortcutValue = () => shortcut.value === "custom" ? shortcutKeys.value.trim() : shortcut.value;

  const download = document.getElementById("local-download");
  const normalDownload = download.onclick;
  const downloadArea = document.getElementById("local-download-area");
  const measureButton = document.getElementById("local-measure");
  const normalMeasure = measureButton.onclick;
  const device = document.getElementById("local-device");
  if (!local.available) device.value = "none";
  const back = find("wizard-back");
  const close = find("wizard-close");
  close.hidden = !manual;
  const next = find("wizard-next");
  const consent = find("wizard-license-agree");
  const login = find("wizard-login");
  const cancelLogin = find("wizard-cancel-login");
  const status = find("wizard-status");
  find("wizard-local-unavailable").hidden = local.supported;
  const choices = [...wizard.querySelectorAll("[data-profile]")];
  let profile = null;
  let index = 0;
  let pending = false;
  let modelAvailable = local.available;
  let session = await invoke("session_status");
  let authenticated = session.server_authenticated;
  let sessionOnly = session.server_session_only;
  async function refreshLogin() {
    session = await invoke("session_status");
    authenticated = session.server_authenticated;
    sessionOnly = session.server_session_only;
  }
  let complete;
  const finished = new Promise((resolve) => { complete = resolve; });

  function render() {
    const steps = profile ? profiles[profile] : ["usage"];
    const step = steps[index];
    for (const node of wizard.querySelectorAll("[data-step]")) node.hidden = node.dataset.step !== step;
    const chips = steps.map((name, position) => {
      const chip = document.createElement("span");
      chip.textContent = `${position + 1}. ${labels[name]}`;
      if (name === step) chip.setAttribute("aria-current", "step");
      if (position < index) chip.className = "done";
      return chip;
    });
    find("wizard-steps").replaceChildren(...chips);
    for (const choice of choices) {
      choice.disabled = !local.supported && choice.dataset.profile !== "server";
      choice.setAttribute("aria-pressed", String(choice.dataset.profile === profile));
    }
    if (step === "license") {
      find("license-hint").textContent = profile === "server"
        ? "翻訳には株式会社Preferred Networksのモデル「PLaMo」を使います。内容を確認し、同意して接続の設定に進みます。"
        : "翻訳には株式会社Preferred Networksのモデル「PLaMo」を使います。内容を確認し、同意してモデルの取得に進みます。";
    }
    if (step === "model") {
      downloadArea.hidden = modelAvailable;
      find("wizard-model-description").textContent = modelAvailable
        ? "PLaMo 3 Translate のモデルと推論エンジンは取得済みです。「次へ」でデバイスの設定に進めます。"
        : "このPCで翻訳するためのモデルと推論エンジンをダウンロードします。";
    }
    back.hidden = index === 0;
    back.disabled = pending || index === 0;
    close.disabled = pending;
    next.disabled = pending || (step === "usage" && !profile) || (step === "license" && !consent.checked) ||
      (step === "model" && !modelAvailable) || (step === "login" && !authenticated);
    next.textContent = step === "done" ? "翻訳を開始" : "次へ";
    login.disabled = pending || authenticated;
    login.textContent = authenticated ? "ログイン済み" : "ログインする";
    find("wizard-session-only").hidden = !sessionOnly;
    if (step === "done") {
      const rows = [["ライセンス", "同意済み"], ["ショートカット", shortcut.value === "custom" ? shortcutValue() : shortcut.selectedOptions[0]?.textContent ?? ""]];
      if (profile !== "server") rows.push(
        ["モデル", "準備済み"],
        ["デバイス", device.selectedOptions[0]?.textContent ?? ""],
        ["同時に訳す数", document.getElementById("local-parallel-limit").value + "件"],
      );
      if (profile !== "local") rows.push(
        ["接続先", document.getElementById("server-url").value],
        ["アカウント", sessionOnly ? "今回のみログイン済み（終了後は再ログインが必要）" : "ログイン済み"],
      );
      find("wizard-summary").replaceChildren(...rows.flatMap(([label, value]) => {
        const term = document.createElement("dt");
        const description = document.createElement("dd");
        term.textContent = label;
        description.textContent = value;
        return [term, description];
      }));
    }
  }

  function report(text, failed = false) {
    status.textContent = text;
    status.className = failed ? "hint err" : "hint";
  }
  async function perform(action) {
    if (pending) return;
    pending = true;
    report("");
    render();
    try { await action(); }
    catch (error) { report(String(error), true); }
    finally { pending = false; render(); }
  }

  for (const choice of choices) choice.onclick = () => { profile = choice.dataset.profile; render(); };
  consent.onchange = render;
  close.onclick = () => complete(null);
  back.onclick = () => { index--; report(""); render(); };
  download.onclick = () => void perform(async () => {
    if (!consent.checked) return;
    await downloadModel(false);
    modelAvailable = (await invoke("local_status")).available;
  });
  measureButton.onclick = () => void perform(measure);
  login.onclick = () => void perform(async () => {
    cancelLogin.hidden = false;
    try {
      await invoke("login");
      await refreshLogin();
    } finally { cancelLogin.hidden = true; }
  });
  cancelLogin.onclick = () => void invoke("cancel_login").catch((error) => report(String(error), true));
  next.onclick = () => void perform(async () => {
    const step = profiles[profile][index];
    if (step === "license" && profile !== "server") {
      modelAvailable = (await invoke("local_status")).available;
    }
    if (step === "device" && !await saveSetupDevice()) return;
    if (step === "connection") {
      if (!await applySetupConnection()) return;
      await refreshLogin();
    }
    if (step === "shortcut" && shortcut.value === "custom" && !shortcutValue().includes("+")) {
      shortcutKeys.focus();
      report("CommandOrControl+Alt+J のように、修飾キーと文字を指定してください。", true);
      return;
    }
    if (step === "done") {
      if (shortcutValue() !== (originalShortcut === "custom" ? originalKeys : originalShortcut)) {
        await saveShortcut(shortcutValue());
      }
      const useLocal = profile !== "server";
      if (useLocal) {
        await invoke("set_local_active", { active: true });
        storage.setItem("ptranslate-desktop-local-active", "true");
      }
      storage.setItem("ptranslate-desktop-destination", useLocal ? "local" : "server");
      await invoke("setup_state", { state: "done" });
      complete(useLocal);
      return;
    }
    index++;
  });
  render();
  const result = await finished;
  if (result === null) {
    resetConnectionFields();
    resetLocalFields();
    shortcut.value = originalShortcut;
    shortcutKeys.value = originalKeys;
    document.getElementById("custom-shortcut").hidden = originalShortcut !== "custom";
  }
  download.onclick = normalDownload;
  measureButton.onclick = normalMeasure;
  for (const restore of moves.reverse()) restore();
  for (const element of originalChildren) element.inert = false;
  document.querySelector("main").classList.remove("setup-running");
  wizard.remove();
  style.remove();
  return result;
}
