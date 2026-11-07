// 翻訳の状態を保ったまま、端末の設定へ切り替える。
const main = document.querySelector("main");
// Webと共通の見出しを、デスクトップ用のヘッダーへまとめる。
export const header = document.createElement("header");
header.id = "app-header";
const title = main.querySelector("h1");
const description = main.querySelector("p.sub");
const who = document.getElementById("who");
export const updateSlot = document.createElement("div");
updateSlot.id = "header-updates";
export const accountSlot = document.createElement("div");
accountSlot.id = "header-account";
export const accountMenu = document.createElement("details");
accountMenu.id = "account-menu";
accountMenu.hidden = true;
const accountSummary = document.createElement("summary");
who.textContent = "アカウント";
accountSummary.append(who);
accountMenu.append(accountSummary);
accountSlot.append(accountMenu);
title.before(header);
header.append(title, updateSlot, description, accountSlot);
description.textContent = "PLaMo 3 Translate · 原文と訳はサーバーに残りません";
for (const eventName of ["click", "keydown"]) {
  document.addEventListener(eventName, (event) => {
    for (const menu of header.querySelectorAll("details[open]")) {
      if (eventName === "keydown" && event.key === "Escape") {
        menu.open = false;
        menu.querySelector("summary").focus();
      } else if (eventName === "click" && !menu.contains(event.target)) menu.open = false;
    }
  });
}

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
// 認証状態の復元中も、接続先を確認できる設定タブを初期表示する。
showPage(true);
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
  [data-web-only] { display: none !important; }
  main { padding-top: 16px; padding-bottom: 16px; }
  #app-header { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; }
  #app-header h1 { margin: 0; font-size: 1.25rem; white-space: nowrap; }
  #app-header .sub { flex: 1; margin: 0; font-size: .85rem; }
  #header-account { display: flex; align-items: center; gap: 8px; margin-left: auto; }
  #app-header details { position: relative; margin: 0; }
  #app-header summary { list-style: none; cursor: pointer; }
  #app-header summary::-webkit-details-marker { display: none; }
  #account-menu > summary { display: flex; align-items: center; gap: 8px; border: 1px solid var(--line); border-radius: 8px; padding: 5px 10px; background: var(--panel); }
  #account-menu > summary::after { content: "▾"; font-size: .75rem; color: var(--muted); }
  #who { max-width: 180px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .header-popover { position: absolute; top: calc(100% + 10px); z-index: 10; padding: 14px; background: var(--panel); border: 1px solid var(--line); border-radius: 10px; box-shadow: 0 8px 28px #0003; }
  #account-menu .header-popover { right: 0; min-width: 150px; }
  #account-menu .header-popover button { width: 100%; text-align: left; }
  #update-menu > summary { border: 1px solid var(--line); border-radius: 999px; padding: 3px 12px; color: var(--muted); background: var(--panel); font-size: .8rem; white-space: nowrap; }
  #update-menu .header-popover { left: 0; width: min(340px, calc(100vw - 48px)); display: flex; flex-wrap: wrap; gap: 8px; }
  #update-status { flex-basis: 100%; }
  #update-confirm { display: flex; flex-wrap: wrap; gap: 8px; }
  #app-header summary:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
  @media (max-width: 760px) {
    #app-header { gap: 12px; position: relative; }
    #app-header .sub { order: 4; flex-basis: 100%; }
    #app-header #update-menu { position: static; }
    #update-menu .header-popover { left: 0; right: auto; }
  }

  .page-tabs { display: flex; gap: 24px; border-bottom: 1px solid var(--line); margin: 16px 0 12px; }
  .page-tabs button { padding: 6px 4px 8px; border: 0; border-bottom: 2px solid transparent; border-radius: 0; background: none; color: var(--muted); }
  .page-tabs button[aria-selected="true"] { color: var(--accent); border-bottom-color: var(--accent); font-weight: 600; }
  .settings-heading h2 { margin: 0; font-size: 1.2rem; }
  .settings-heading p { margin: 4px 0 20px; }
  #settings-page { max-width: 760px; margin: 0 auto; }
  #translation-page section { padding: 12px; margin-bottom: 12px; }
  #translation-page > .hint { margin: 8px 0 0; }
  #settings-page > details, #settings-page > aside { padding: 16px; margin: 0 0 12px; background: var(--panel); border: 1px solid var(--line); border-radius: 10px; }
  #settings-page summary { color: var(--text); font-weight: 600; }
  #desktop-settings > label { display: block; margin: 16px 0 8px; }
  #custom-shortcut { margin-top: 12px; }
  #shortcut-keys { max-width: 100%; }
`;
document.head.append(style);
