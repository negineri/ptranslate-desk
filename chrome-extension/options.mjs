import { chromeRegistrationUrl, connectionKey, consentKey, loadSettings, normalizeServerUrl, originPattern, saveSettings } from "./settings.mjs";
import { errorMessage, loadConfig, remedy } from "./translate.mjs";
import { extensionFetch } from "./transport.mjs";

export function mountOptions({ document = globalThis.document, chrome = globalThis.chrome,
  fetch = extensionFetch, openDesktop = (url) => { location.href = url; },
  retryMs = 1000, registrationTimeoutMs = 120_000, checkTimeoutMs = 15000 } = {}) {
  const $ = (id) => document.getElementById(id);
  let settings = null, operation = null, localReady = false, offer = null;
  const clearOffer = () => { offer = null; $("volunteer-consent").hidden = true; };
  const setStatus = (id, { text, cls = "" }) => {
    $(id).textContent = text; $(id).className = "hint " + cls;
  };
  const hideAnonymous = () => {
    $("anonymous-start").hidden = true;
    $("anonymous-forget").hidden = true;
  };
  const begin = () => {
    operation?.abort(); operation = new AbortController();
    $("login").hidden = true;
    clearOffer();
    hideAnonymous();
    return operation;
  };
  const cancel = () => operation?.abort();
  const failed = async (error, mine) => {
    if (mine.signal.aborted) return;
    await chrome.storage.session.remove("connectedTo");
    if (mine.signal.aborted) return;
    setStatus("status", errorMessage(error));
    $("login").hidden = remedy(error) !== "login";
    if (settings?.mode === "server" && settings.server && $("server").value === settings.server) {
      $("anonymous-start").hidden = remedy(error) !== "login" && error.code !== "anonymous";
      $("anonymous-forget").hidden = error.code !== "anonymous";
    }
  };
  async function read(snapshot, signal) {
    const abort = new AbortController();
    const onAbort = () => abort.abort(signal.reason);
    signal.addEventListener("abort", onAbort, { once: true });
    if (signal.aborted) onAbort();
    const timer = setTimeout(() => abort.abort(new Error("接続先から応答がありません")), checkTimeoutMs);
    try {
      return await loadConfig({ signal: abort.signal, fetch: (route, options) => fetch(route, {
        ...options, connection: { mode: snapshot.mode, server: snapshot.server },
      }) });
    } finally { clearTimeout(timer); signal.removeEventListener("abort", onAbort); }
  }
  async function connected(snapshot, config, mine) {
    if (mine.signal.aborted) return;
    for (const id of ["source", "target"]) {
      const select = $(id);
      while (select.options.length > 1) select.remove(1);
      for (const language of config.languages || []) select.append(new document.defaultView.Option(language.name, language.code));
      if ([...select.options].some((option) => option.value === settings[id])) select.value = settings[id];
    }
    localReady = snapshot.mode === "local";
    $("anonymous-forget").hidden = config.tier !== "anonymous" || localReady;
    if (!localReady && config.pool?.kind === "volunteer") {
      const version = config.pool.consent_version;
      if (!Number.isSafeInteger(version) || version < 1) throw new Error("ボランティア枠の同意の版を確認できませんでした");
      const key = consentKey(snapshot.server);
      const stored = await chrome.storage.local.get(key);
      if (mine.signal.aborted) return;
      if (stored[key] !== version) {
        offer = { key, version };
        $("volunteer-agree").disabled = false;
        $("volunteer-consent").hidden = false;
        setStatus("status", { text: "ボランティア枠への送信には同意が必要です" });
        return;
      }
    }
    await chrome.storage.session.set({ connectedTo: connectionKey(snapshot) });
    if (mine.signal.aborted) return;
    setStatus("status", { text: snapshot.mode === "local" ? "このPCのローカル翻訳に接続できました" : config.tier === "anonymous" ? "匿名で接続できました" : config.pool?.kind === "volunteer" ? "ボランティア枠に接続できました" : `接続できました (${config.user} でログイン中)`, cls: "ok" });
  }
  function showMode() {
    cancel();
    clearOffer();
    hideAnonymous();
    $("server-fields").hidden = $("mode").value === "local";
    $("local-help").hidden = $("mode").value !== "local";
    $("login").hidden = true;
    setStatus("status", { text: "" });
  }
  function pause(signal) {
    return new Promise((resolve, reject) => {
      const onAbort = () => { clearTimeout(timer); signal.removeEventListener("abort", onAbort); reject(signal.reason); };
      const timer = setTimeout(() => { signal.removeEventListener("abort", onAbort); resolve(); }, retryMs);
      signal.addEventListener("abort", onAbort, { once: true });
      if (signal.aborted) onAbort();
    });
  }
  async function connectLocal(mine, launched) {
    settings = await saveSettings({ mode: "local" });
    const snapshot = { ...settings };
    const deadline = Date.now() + registrationTimeoutMs;
    for (;;) {
      mine.signal.throwIfAborted();
      let config;
      try { config = await read(snapshot, mine.signal); }
      catch (error) {
        localReady = false;
        if (!launched || Date.now() >= deadline || mine.signal.aborted) throw error;
        await pause(mine.signal);
        continue;
      }
      await connected(snapshot, config, mine);
      return;
    }
  }
  $("server").addEventListener("input", () => { cancel(); clearOffer(); hideAnonymous(); });
  $("volunteer-agree").addEventListener("click", async () => {
    if (!offer) return;
    const current = offer;
    $("volunteer-agree").disabled = true;
    try {
      await chrome.storage.local.set({ [current.key]: current.version });
      if (offer !== current) return;
      clearOffer();
      setStatus("status", { text: "ボランティア枠への送信に同意しました", cls: "ok" });
    } catch {
      if (offer !== current) return;
      $("volunteer-agree").disabled = false;
      setStatus("status", { text: "同意を保存できませんでした", cls: "err" });
    }
  });
  $("mode").addEventListener("change", showMode);
  $("extension-id").textContent = chrome.runtime.id;
  $("save").addEventListener("click", () => {
    const mine = begin();
    if ($("mode").value === "local") {
      const launched = !localReady;
      // 専用URLはクリック直後に開き、承認後の接続はこの操作の中で待つ。
      try { if (launched) openDesktop(chromeRegistrationUrl(chrome.runtime.id)); }
      catch (error) { void failed(error, mine); return; }
      setStatus("status", { text: launched ? "デスクトップで登録を承認してください。接続を待っています" : "接続を確かめています" });
      void connectLocal(mine, launched).catch((error) => failed(error, mine));
      return;
    }
    let server;
    try { server = normalizeServerUrl($("server").value); }
    catch (error) { void failed(error, mine); return; }
    $("server").value = server;
    // 権限の要求もクリック直後、awaitの前に行う。
    const permission = chrome.permissions.request({ origins: [originPattern(server)] });
    setStatus("status", { text: "接続を確かめています" });
    void permission.then(async (granted) => {
      mine.signal.throwIfAborted();
      if (!granted) throw new Error("サーバーへの接続が許可されませんでした");
      settings = await saveSettings({ server, mode: "server" });
      const snapshot = { ...settings };
      await connected(snapshot, await read(snapshot, mine.signal), mine);
    }).catch((error) => failed(error, mine));
  });
  async function changeAnonymous(route) {
    const snapshot = settings;
    if (snapshot?.mode !== "server" || !snapshot.server || $("server").value !== snapshot.server) return;
    const mine = begin();
    try {
      const result = await fetch(route, {
        signal: mine.signal, connection: { mode: snapshot.mode, server: snapshot.server },
      });
      if (!result.ok) throw new Error("匿名利用の設定を変更できませんでした");
      await connected(snapshot, await read(snapshot, mine.signal), mine);
    } catch (error) { await failed(error, mine); }
  }
  $("anonymous-start").addEventListener("click", () => { void changeAnonymous("api/anonymous"); });
  $("anonymous-forget").addEventListener("click", () => { void changeAnonymous("anonymous/forget"); });
  $("login").addEventListener("click", () => { void chrome.tabs.create({ url: `${settings.server}/` }); });
  $("save-prefs").addEventListener("click", async () => {
    settings = await saveSettings({ source: $("source").value, target: $("target").value, form: $("form").value, glossary: $("glossary").value });
    setStatus("prefs-status", { text: "保存しました", cls: "ok" });
  });
  $("shortcuts").addEventListener("click", () => { void chrome.tabs.create({ url: "chrome://extensions/shortcuts" }); });
  const ready = (async () => {
    settings = await loadSettings();
    $("mode").value = settings.mode; showMode();
    for (const id of ["source", "target"]) {
      const select = $(id);
      if (![...select.options].some((option) => option.value === settings[id])) {
        select.append(new document.defaultView.Option(settings[id], settings[id]));
      }
      select.value = settings[id];
    }
    $("server").value = settings.server; $("form").value = settings.form; $("glossary").value = settings.glossary;
    if (settings.mode === "local" || settings.server) {
      const mine = begin(), snapshot = { ...settings };
      setStatus("status", { text: "接続を確かめています" });
      try { await connected(snapshot, await read(snapshot, mine.signal), mine); }
      catch (error) { localReady = false; await failed(error, mine); }
    }
  })().catch((error) => setStatus("status", errorMessage(error)));
  document.defaultView.addEventListener("pagehide", cancel, { once: true });
  return { ready, destroy: cancel };
}

if (typeof document !== "undefined") mountOptions();
