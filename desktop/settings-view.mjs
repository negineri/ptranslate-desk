// 翻訳の状態を保ったまま、端末の設定へ切り替える。
const main = document.querySelector("main");
const translation = document.createElement("div");
translation.id = "translation-page";
translation.setAttribute("role", "tabpanel");
translation.setAttribute("aria-labelledby", "translation-tab");
const section = main.querySelector("section");
section.before(translation);
translation.append(section);
const help = translation.nextElementSibling;
if (help?.matches("p.hint")) translation.append(help);

export const settingsPage = document.createElement("div");
settingsPage.id = "settings-page";
settingsPage.hidden = true;
settingsPage.setAttribute("role", "tabpanel");
settingsPage.setAttribute("aria-labelledby", "settings-tab");
settingsPage.innerHTML = `<div class="settings-heading"><h2>設定</h2><p class="hint">この端末で使う接続先やショートカットを管理します。</p></div>`;
main.append(settingsPage);

const tabs = document.createElement("div");
tabs.className = "page-tabs";
tabs.setAttribute("role", "tablist");
tabs.setAttribute("aria-label", "画面の切り替え");
tabs.innerHTML = `<button id="translation-tab" role="tab" aria-controls="translation-page" aria-selected="true">翻訳</button>
  <button id="settings-tab" role="tab" aria-controls="settings-page" aria-selected="false" tabindex="-1">設定</button>`;
translation.before(tabs);
const buttons = [...tabs.querySelectorAll("button")];
export function showPage(settings = false) {
  translation.hidden = settings;
  settingsPage.hidden = !settings;
  for (const [index, button] of buttons.entries()) {
    const selected = index === Number(settings);
    button.setAttribute("aria-selected", String(selected));
    button.tabIndex = selected ? 0 : -1;
  }
}
buttons.forEach((button, index) => {
  button.addEventListener("click", () => showPage(index === 1));
  button.addEventListener("keydown", (event) => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    const next = event.key === "Home" ? 0 : event.key === "End" ? 1 : 1 - index;
    showPage(next === 1);
    buttons[next].focus();
  });
});
const style = document.createElement("style");
style.textContent = `
  [hidden] { display: none !important; }
  .page-tabs { display: flex; gap: 24px; border-bottom: 1px solid var(--line); margin: 24px 0 20px; }
  .page-tabs button { padding: 8px 4px 12px; border: 0; border-bottom: 2px solid transparent; border-radius: 0; background: none; color: var(--muted); }
  .page-tabs button[aria-selected="true"] { color: var(--accent); border-bottom-color: var(--accent); font-weight: 600; }
  .settings-heading h2 { margin: 0; font-size: 1.2rem; }
  .settings-heading p { margin: 4px 0 20px; }
  #settings-page { max-width: 760px; margin: 0 auto; }
  #settings-page > details, #settings-page > aside { padding: 20px; margin: 0 0 16px; background: var(--panel); border: 1px solid var(--line); border-radius: 10px; }
  #settings-page summary { color: var(--text); font-weight: 600; }
  #desktop-settings > label { display: block; margin: 16px 0 8px; }
  #custom-shortcut { margin-top: 12px; }
  #shortcut-keys { max-width: 100%; }
`;
document.head.append(style);
