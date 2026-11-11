import { copyFile, mkdir, readFile, writeFile } from "node:fs/promises";
import { desktopTranslationLayout } from "./translation-layout.mjs";

const source = new URL("../server/static/", import.meta.url);
const dist = new URL("./dist/", import.meta.url);
const html = desktopTranslationLayout(await readFile(new URL("index.html", source), "utf8"));
const { productName } = JSON.parse(await readFile(new URL("./src-tauri/tauri.conf.json", import.meta.url), "utf8"));
const consentDialog = html.match(/<dialog id="volunteer-consent"[\s\S]*?<\/dialog>/)?.[0]
  .replace("</h2>", '</h2><p id="volunteer-server" class="hint"></p>');
if (!consentDialog) throw new Error("ボランティア枠の同意画面がありません");
const script = '<script type="module">';
if (!html.includes(script)) throw new Error("Web 画面のモジュールを見つけられません");
await mkdir(dist, { recursive: true });
// 言語一覧はサーバーの定義から生成し、ローカル翻訳でも同じものを使う。
const server = await readFile(new URL("../server/translator.py", import.meta.url), "utf8");
const languages = [...server.match(/LANGUAGES = \[([\s\S]*?)\n\]/)[1].matchAll(/\("([^"]+)", "([^"]+)"\)/g)]
  .map(([, code, name]) => ({ code, name }));
if (languages.length < 53) throw new Error("ローカル翻訳の言語一覧を生成できません");
await mkdir(new URL("./src-tauri/gen/", import.meta.url), { recursive: true });
await writeFile(new URL("./src-tauri/gen/local-languages.json", import.meta.url), JSON.stringify(languages));
await Promise.all([
  writeFile(new URL("index.html", dist), html.replace(
    script, script + '\nimport { desktopFetch as fetch, settingsStorage as localStorage, copyText, translationMode } from "./transport.mjs";\nimport { volunteerFields } from "./volunteer-consent.mjs";',
  ).replace("const webClient = true;", "const webClient = false;")
    .replaceAll("--accent-text: #ffffff;", "--accent-text: #ffffff; --accent-soft: #e9f0fd;")
    .replaceAll("--accent-text: #0d1117;", "--accent-text: #0d1117; --accent-soft: #2c3e63;")
    .replace("このブラウザに", "この端末に")
    .replace("els.who.textContent = ` (${config.user} でログイン中)`;", "")
    .replace("<title>PTranslate</title>", `<title>${productName}</title>`)
    .replace(">PTranslate</h1>", `>${productName}</h1>`)
    .replace("await navigator.clipboard.writeText(els.out.textContent);",
      "if (!await copyText(els.out.textContent)) return;")
    .replace("async function requestTranslation(body, onText, signal)", "async function requestTranslation(body, onText, signal, localOverride)")
    .replace("body: JSON.stringify(body), signal });", "body: JSON.stringify(body), signal, localOverride });")
    .replace(/<dialog id="volunteer-consent"[\s\S]*?<\/dialog>/, consentDialog)
    .replace("const runKey = JSON.stringify([text, common]);\n  if (auto && runKey === lastKey && !controller) return;", "const localOverride = translationMode();\n  const wasRunning = controller !== null;")
    .replace('  try {\n    els.out.textContent = "";', `  try {
    const current = await loadDesktopConfig(localOverride, mine.signal);
    mine.signal.throwIfAborted();
    $("volunteer-destination").hidden = current.pool?.kind !== "volunteer";
    Object.assign(common, volunteerFields(current, { auto }));
    const runKey = JSON.stringify([text, common, localOverride]);
    if (auto && runKey === lastKey && !wasRunning) return;
    els.out.textContent = "";`)
    .replace("JSON.stringify([common, source, prev ? prev.source : null])", "JSON.stringify([localOverride, common, source, prev ? prev.source : null])")
    .replace("}, mine.signal);", "}, mine.signal, localOverride);")
    .replace('  if (res.status === 401) { location.reload(); throw', '  if (res.status === 409) invalidateDesktopConfig();\n  if (res.status === 401) { location.reload(); throw')
    .replace('  const res = await fetch("api/config");\n  if (res.status === 401) { location.reload(); return; }\n  if (!res.ok) throw new Error(`HTTP ${res.status}`);\n  config = await res.json();',
      '  config = await loadDesktopConfig(translationMode());')
    .replace('els.go.addEventListener("click", () => run());', `let desktopConfig = null;
let configGeneration = 0;

function invalidateDesktopConfig() {
  desktopConfig = null;
  configGeneration++;
  $("volunteer-destination").hidden = true;
}

async function loadDesktopConfig(localOverride, signal) {
  if (desktopConfig?.localOverride === localOverride) return desktopConfig.value;
  const generation = configGeneration;
  const response = await fetch("api/config", { signal, localOverride });
  if (!response.ok) throw new Error("翻訳先を確認できませんでした");
  const value = await response.json();
  signal?.throwIfAborted();
  if (generation === configGeneration) desktopConfig = { localOverride, value };
  return value;
}

function resetDesktopTranslation() {
  invalidateDesktopConfig();
  els.stop.click();
  cache.clear();
  lastKey = null;
}

document.addEventListener("translation-destination-changed", resetDesktopTranslation);
document.addEventListener("translation-settings-applied", resetDesktopTranslation);
els.go.addEventListener("click", () => run());`)
    .replace("<section>", "<section inert>")
    .replace("  restore();", '  restore();\n  document.querySelector("section").inert = false;\n  document.dispatchEvent(new Event("desktop-ready"));')),
  copyFile(new URL("translation.mjs", source), new URL("translation.mjs", dist)),
  ...["logo.png", "favicon.ico"].map((name) => copyFile(new URL(name, source), new URL(name, dist))),
  copyFile(new URL("./transport.mjs", import.meta.url), new URL("transport.mjs", dist)),
  ...["ipc.mjs", "popup.mjs", "updates.mjs", "connection.mjs", "worker-view.mjs", "volunteer-consent.mjs", "settings-view.mjs", "local-view.mjs", "setup-wizard.mjs"].map((name) => copyFile(new URL(name, import.meta.url), new URL(name, dist))),
  readFile(new URL("popup.html", import.meta.url), "utf8").then((popup) =>
    writeFile(new URL("popup.html", dist), popup.replace("<!-- shared-style -->", html.match(/<style>[\s\S]*?<\/style>/)[0]).replace("<!-- volunteer-consent -->", consentDialog))),
]);
