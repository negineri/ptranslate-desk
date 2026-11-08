import { loadSettings, normalizeServerUrl, originPattern, saveSettings } from "./settings.mjs";
import { errorMessage, loadConfig, remedy } from "./translate.mjs";

const $ = (id) => document.getElementById(id);
let settings = null;

function setStatus(id, { text, cls = "" }) {
  $(id).textContent = text;
  $(id).className = "hint " + cls;
}

function fillLanguages(languages) {
  for (const id of ["source", "target"]) {
    const select = $(id);
    while (select.options.length > 1) select.remove(1);
    for (const l of languages) select.append(new Option(l.name, l.code));
    if ([...select.options].some((o) => o.value === settings[id])) select.value = settings[id];
  }
}

async function check() {
  $("login").hidden = true;
  setStatus("status", { text: "接続を確かめています" });
  try {
    const config = await loadConfig();
    fillLanguages(config.languages || []);
    setStatus("status", { text: settings.mode === "local" ? "このPCのローカル翻訳に接続できました" : `接続できました (${config.user} でログイン中)`, cls: "ok" });
  } catch (error) {
    setStatus("status", errorMessage(error));
    $("login").hidden = remedy(error) !== "login";
  }
}

function showMode() {
  $("server-fields").hidden = $("mode").value === "local";
  $("local-help").hidden = $("mode").value !== "local";
  $("login").hidden = true;
}
$("mode").addEventListener("change", showMode);
$("extension-id").textContent = chrome.runtime.id;
$("save").addEventListener("click", () => {
  if ($("mode").value === "local") {
    void saveSettings({ mode: "local" }).then(async (saved) => { settings = saved; await check(); }).catch((e) => setStatus("status", errorMessage(e)));
    return;
  }
  let server;
  try {
    server = normalizeServerUrl($("server").value);
  } catch (error) {
    setStatus("status", errorMessage(error));
    return;
  }
  $("server").value = server;
  // 権限の確認はボタンを押した直後 (await の前) に呼ぶ必要がある
  chrome.permissions.request({ origins: [originPattern(server)] }).then(async (granted) => {
    if (!granted) {
      setStatus("status", { text: "サーバーへの接続が許可されませんでした", cls: "err" });
      return;
    }
    settings = await saveSettings({ server, mode: "server" });
    await check();
  }, (error) => setStatus("status", errorMessage(error)));
});

$("login").addEventListener("click", () => { void chrome.tabs.create({ url: `${settings.server}/` }); });

$("save-prefs").addEventListener("click", async () => {
  settings = await saveSettings({
    source: $("source").value, target: $("target").value, form: $("form").value, glossary: $("glossary").value,
  });
  setStatus("prefs-status", { text: "保存しました", cls: "ok" });
});

$("shortcuts").addEventListener("click", () => { void chrome.tabs.create({ url: "chrome://extensions/shortcuts" }); });

async function init() {
  settings = await loadSettings();
  $("mode").value = settings.mode; showMode();
  $("server").value = settings.server;
  $("form").value = settings.form;
  $("glossary").value = settings.glossary;
  if (settings.mode === "local" || settings.server) await check();
}
init().catch((e) => setStatus("status", errorMessage(e)));
