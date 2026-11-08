// デザイナーが直接編集できる、通信不要のHTMLを現在の画面から作る。
import { readFile, mkdir, writeFile } from "node:fs/promises";
import { desktopTranslationLayout } from "../desktop/translation-layout.mjs";
const root = new URL("../", import.meta.url);
const read = (path) => readFile(new URL(path, root), "utf8");
const [web, settings, local, connection, transport] = await Promise.all([
  read("server/static/index.html"), read("desktop/settings-view.mjs"), read("desktop/local-view.mjs"),
  read("desktop/connection.mjs"), read("desktop/transport.mjs"),
]);
function template(source, name) {
  const match = source.match(new RegExp(`${name}\\.innerHTML = ` + "`([\\s\\S]*?)`;"));
  if (!match) throw new Error(`HTMLを見つけられません: ${name}`);
  // このリポジトリの固定テンプレートだけを評価する。利用者の入力は渡さない。
  return Function("copyKey", "isMac", "return `" + match[1] + "`;")("Ctrl+C", false);
}
function css(source) { return [...source.matchAll(/style\.textContent = `([\s\S]*?)`;/g)].map((m) => m[1]).join("\n"); }
const logo = (await readFile(new URL("server/static/logo.png", root))).toString("base64");
const baseStyle = web.match(/<style>([\s\S]*?)<\/style>/)[1];
const section = desktopTranslationLayout(web.match(/<section>[\s\S]*?<\/section>/)[0]);
const heading = template(settings, "settingsPage");
const html = `<!doctype html>
<html lang="ja" data-theme="auto">
<head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>PTranslate Desk — デザイン確認</title>
<!-- アプリのHTML/CSSを直接編集できます。実際の通信やモデル取得は行いません。 -->
<style>
${baseStyle}
${css(settings)}
${css(connection)}
${css(local)}
/* ここから下は確認用のツールバー。アプリ本体のデザインとは独立しています。 */
html[data-theme="light"] { --bg:#f6f7f9;--panel:#fff;--text:#1d2330;--muted:#5d6576;--line:#dde1e8;--accent:#2f6fde;--accent-text:#fff;--ok:#1f8a4c;--warn:#b26a00;--err:#c43c3c; }
html[data-theme="dark"] { --bg:#14171c;--panel:#1d2128;--text:#e7eaf0;--muted:#9aa3b2;--line:#2e343e;--accent:#5b8ff0;--accent-text:#0d1117;--ok:#4cc283;--warn:#e3a33c;--err:#ef6b6b; }
.preview-toolbar { display:flex; flex-wrap:wrap; align-items:center; gap:12px; padding:12px 20px; border-bottom:1px solid var(--line); background:var(--panel); }
.preview-toolbar label { display:flex; align-items:center; gap:6px; font-size:13px; }
.preview-toolbar p { margin:0; font-size:12px; color:var(--muted); }
#preview-frame { width:1120px; max-width:100%; min-height:760px; margin:16px auto; border:1px solid var(--line); background:var(--bg); }
#preview-frame main { min-height:758px; }
#preview-hint { padding:0 20px; color:var(--muted); font-size:12px; }
</style>
</head>
<body>
<nav class="preview-toolbar" aria-label="デザイン確認ツール">
  <strong>デザイン確認</strong>
  <label>状態<select id="preview-scene">
    <option value="welcome">初回のモード選択</option><option value="server">サーバー設定</option>
    <option value="missing">モデル未取得</option><option value="confirm">ダウンロード確認</option>
    <option value="downloading">ダウンロード中</option><option value="measuring">デバイス計測中</option>
    <option value="candidates">デバイス候補・手動選択</option><option value="memory">メモリ自動解放</option>
    <option value="loading">モデル読み込み中</option><option value="translation">翻訳画面</option><option value="error">取得エラー</option>
  </select></label>
  <label>配色<select id="preview-theme"><option value="auto">OSに合わせる</option><option value="light">ライト</option><option value="dark">ダーク</option></select></label>
  <label>幅<select id="preview-width"><option value="1120">1120px（既定）</option><option value="960">960px</option><option value="760">760px</option><option value="full">全幅</option></select></label>
  <p>表示・操作のサンプルです。翻訳・通信・ダウンロードは行いません。</p>
</nav>
<p id="preview-hint">HTML・CSSを直接編集して保存し、ブラウザを更新してください。ロゴを含む単一ファイルです。</p>
<div id="preview-frame"><main>
  <header id="app-header">
    <h1><img class="brand-mark" src="data:image/png;base64,${logo}" width="32" height="32" alt="">PTranslate Desk</h1>
    <div id="header-updates"><details id="update-menu"><summary id="version-badge">v0.0.0-dev · 開発用</summary><div class="header-popover"><span class="hint">デザイン確認用</span><button type="button">更新を確認</button></div></details></div>
    <p class="sub">PLaMo 3 Translate</p>
    <div id="header-account"><details id="account-menu"><summary><span id="who">この端末</span></summary><div class="header-popover"><button id="preview-server-switch">サーバーモードに切り替え</button></div></details></div>
  </header>
  <p id="local-runtime-status" class="hint" role="status" hidden></p>
  <div class="page-tabs" role="tablist" aria-label="画面の切り替え"><button id="translation-tab" role="tab">翻訳</button><button id="settings-tab" role="tab">設定</button></div>
  <div id="translation-page" role="tabpanel">${section}<p class="hint">長い文章は段落ごとに区切って順に訳します。</p></div>
  <div id="settings-page" role="tabpanel">${heading}
    <aside id="local-settings">${template(local, "panel")}</aside>
    <details id="connection-settings">${template(connection, "panel")}</details>
    <details id="desktop-settings">${template(transport, "settingsPanel")}</details>
  </div>
  <div id="mode-welcome">${template(local, "welcome")}</div>
</main></div>
<script>
// 確認用の状態遷移だけを再現する。アプリのネイティブAPIは使用しない。
const $ = (id) => document.getElementById(id);
let animation;
function show(id, visible) { $(id).hidden = !visible; }
function scene(name) {
  clearInterval(animation);
  document.querySelectorAll('dialog[open]').forEach((d) => d.close());
  $('preview-scene').value = name;
  const welcome = name === 'welcome';
  const translate = name === 'translation' || name === 'loading';
  const server = name === 'server';
  const missing = ['missing','confirm','downloading','error'].includes(name);
  show('mode-welcome', welcome); show('translation-page', !welcome && translate); show('settings-page', !welcome && !translate);
  document.querySelector('.page-tabs').hidden = welcome;
  show('header-account', !welcome && !missing && !server);
  show('local-options', !server); show('connection-settings', server); $('connection-settings').open = server;
  show('local-setup', !missing && !server);
  show('local-download-area', missing && name !== 'downloading'); show('local-progress-area', name === 'downloading');
  show('local-cancel-measure', name === 'measuring'); show('local-runtime-status', name === 'loading');
  $('local-runtime-status').textContent = '翻訳モデルを読み込んでいます…';
  $('local-auto-release').checked = name === 'memory'; show('local-idle-setting', name === 'memory'); $('local-idle-minutes').value = '5';
  $('mode-local').setAttribute('aria-pressed',String(!server)); $('mode-server').setAttribute('aria-pressed',String(server));
  $('translation-tab').setAttribute('aria-selected',String(translate)); $('settings-tab').setAttribute('aria-selected',String(!translate));
  $('local-save').disabled = missing || name === 'measuring'; $('local-measure').disabled = missing || name === 'measuring';
  $('local-start').disabled = missing || name === 'measuring';
  $('local-status').className = name === 'error' ? 'hint err' : 'hint';
  $('local-status').textContent = name === 'error' ? 'ダウンロードが途切れました。やり直してください' : name === 'downloading' ? '' : missing ? 'モデルをダウンロードすると、このPCで翻訳できます。' : 'モデルの準備ができました。';
  $('local-measure-status').textContent = name === 'measuring' ? 'AMD Radeon Graphicsを計測しています…' : ['candidates','memory'].includes(name) ? '結果を参考に使用するデバイスを選んでください。文章の長さによって速度は変わります。' : '';
  $('local-candidates').replaceChildren();
  if (['candidates','memory'].includes(name)) {
    for (const [label,id] of [['NVIDIA GeForce RTX 4090：短文の翻訳 約0.06秒（おすすめ）','Vulkan0'],['CPU：短文の翻訳 約0.45秒','none'],['AMD Radeon Graphics：短文の翻訳 約3.20秒','Vulkan1']]) {
      const li = document.createElement('li'); li.append(document.createTextNode(label));
      const button = document.createElement('button'); button.textContent = '選択'; button.onclick = () => $('local-device').value = id; li.append(button); $('local-candidates').append(li);
    }
  }
  $('local-device').innerHTML = '<option value="Vulkan0">NVIDIA GeForce RTX 4090</option><option value="Vulkan1">AMD Radeon Graphics</option><option value="none">CPU</option>';
  $('local-progress').value = .42; $('local-progress-label').textContent = '翻訳モデル：723.9 / 1723.5 MiB（42%）';
  $('client-id').value = 'translator-desktop'; $('shortcut-status').textContent = 'ショートカットを使えます';
  $('header-account').querySelector('summary').title = 'アカウント';
  $('count').textContent = String($('src').value.length);
  $('out').textContent = name === 'loading' ? '' : 'このアプリケーションは、お使いのコンピューターで文章を翻訳します。';
  $('copy').disabled = name === 'loading';
  document.querySelector('#app-header .sub').textContent = welcome ? 'PLaMo 3 Translate' : server ? 'PLaMo 3 Translate · 原文と訳はサーバーに残りません' : 'PLaMo 3 Translate · このPCで翻訳';
  if (name === 'confirm') { $('local-download-confirm').showModal(); $('local-decline-download').focus(); }
}
$('preview-scene').onchange = (e) => scene(e.target.value);
$('preview-theme').onchange = (e) => document.documentElement.dataset.theme = e.target.value;
$('preview-width').onchange = (e) => $('preview-frame').style.width = e.target.value === 'full' ? '100%' : e.target.value + 'px';
$('welcome-local').onclick = $('mode-local').onclick = () => scene('missing');
$('welcome-server').onclick = $('mode-server').onclick = $('preview-server-switch').onclick = () => scene('server');
$('local-download').onclick = () => scene('confirm'); $('local-decline-download').onclick = () => scene('missing');
$('local-confirm-download').onclick = () => {
  scene('downloading'); let progress = .42;
  animation = setInterval(() => { progress = Math.min(1,progress+.025); $('local-progress').value = progress; $('local-progress-label').textContent = '翻訳モデル：' + Math.floor(progress*100) + '%'; if(progress === 1) scene('candidates'); },200);
};
$('local-cancel-download').onclick = () => scene('missing');
$('local-measure').onclick = () => { scene('measuring'); animation = setInterval(() => scene('candidates'),1800); };
$('local-cancel-measure').onclick = () => scene('candidates'); $('local-manual').onclick = () => $('local-device').focus();
$('local-auto-release').onchange = () => show('local-idle-setting',$('local-auto-release').checked);
$('local-save').onclick = () => scene('translation'); $('local-start').onclick = () => $('local-status').textContent = 'モデルの準備ができました。';
$('translation-tab').onclick = () => scene('translation'); $('settings-tab').onclick = () => scene('candidates');
$('connection-form').onsubmit = (e) => { e.preventDefault(); $('connection-confirm').showModal(); };
$('cancel-connection').onclick = () => $('connection-confirm').close(); $('confirm-connection').onclick = () => { scene('translation'); $('who').textContent = 'サンプルユーザー'; };
for (const id of ['source','target']) $(id).innerHTML = (id === 'source' ? '<option value="">自動で判定</option>' : '<option value="auto">日本語 ⇄ 英語（自動）</option>') + '<option value="ja">日本語</option><option value="en">英語</option>';
$('src').value = 'This application translates text on your computer.';
$('out').textContent = 'このアプリケーションは、お使いのコンピューターで文章を翻訳します。';
$('go').onclick = () => $('status').textContent = '訳し終わりました（表示サンプル）';
$('clear').onclick = () => { $('src').value = ''; $('out').textContent = ''; };
$('shortcut').onchange = () => show('custom-shortcut',$('shortcut').value === 'custom');
document.querySelectorAll('a[href^="http"]').forEach((link) => { link.onclick = (e) => { e.preventDefault(); $('local-status').textContent = 'モデル情報のリンク（表示サンプル）'; }; });
scene(new URLSearchParams(location.search).get('scene') || 'welcome');
</script>
</body></html>`;
await mkdir(new URL("design/", root), { recursive: true });
await writeFile(new URL("design/desktop-preview.html", root), html);
console.log("design/desktop-preview.html を出力しました");
