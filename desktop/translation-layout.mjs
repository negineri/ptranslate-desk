// Webと共通の画面を、デスクトップ用の操作配置にする。
export function desktopTranslationLayout(html) {
  const copy = /[ \t]*<span class="spacer"><\/span>\r?\n[ \t]*<button id="copy">訳をコピー<\/button>/;
  const output = '<div id="out" aria-live="polite"></div>';
  if (!copy.test(html) || !html.includes(output)) throw new Error("訳のコピー操作を配置できません");
  return html.replace(copy, "").replace(output, `${output}\n        <div class="hint under"><span class="spacer"></span><button id="copy" class="link" title="訳をクリップボードにコピー">訳をコピー</button></div>`);
}
