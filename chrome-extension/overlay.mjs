// ページの上に訳の枠を出す。ページの CSS が混ざらないように shadow DOM に入れる
export function mountOverlay(src, rect, timeoutMs = 2000) {
  const ID = "ptranslate-extension-overlay";
  document.getElementById(ID)?.remove();
  const host = document.createElement("div");
  host.id = ID;
  const shadow = host.attachShadow({ mode: "closed" });
  const frame = document.createElement("iframe");

  frame.allow = "clipboard-write";
  frame.title = "PTranslate";
  const width = Math.min(480, innerWidth - 16);
  const maxHeight = Math.max(160, Math.round(innerHeight * 0.6));
  let height = 180;
  let top = 8;
  let left = innerWidth - width - 8;
  if (rect) {
    left = Math.min(Math.max(8, rect.left), innerWidth - width - 8);
    top = rect.bottom + 8 + height <= innerHeight ? rect.bottom + 8 : Math.max(8, rect.top - 8 - height);
  }
  let ready = false;
  let settle;
  const result = new Promise((resolve) => { settle = resolve; });
  const place = () => {
    frame.style.cssText = [
      "position:fixed", `top:${top}px`, `left:${left}px`, `width:${width}px`, `height:${height}px`,
      "border:0", "border-radius:10px", "box-shadow:0 6px 24px rgba(0,0,0,.25)", "z-index:2147483647",
      ready ? "visibility:visible" : "visibility:hidden", "background:transparent", "color-scheme:normal", "margin:0", "padding:0",
    ].join(";");
  };
  place();
  const timer = setTimeout(() => close("blocked"), timeoutMs);
  const close = (reason = "closed") => {
    clearTimeout(timer);
    settle(reason);
    removeEventListener("message", onMessage, true);
    removeEventListener("mousedown", onOutside, true);
    removeEventListener("keydown", onKey, true);
    host.remove();
  };
  const onMessage = (e) => {
    if (e.source !== frame.contentWindow || typeof e.data !== "object" || !e.data) return;
    if (e.data.ptranslate === "ready") {
      ready = true;
      clearTimeout(timer);
      place();
      settle("ready");
      frame.focus();
    }
    if (e.data.ptranslate === "close") close();
    if (e.data.ptranslate === "resize" && Number.isFinite(e.data.height)) {
      height = Math.min(maxHeight, Math.max(120, Math.round(e.data.height)));
      if (top + height > innerHeight - 8) top = Math.max(8, innerHeight - 8 - height);
      place();
    }
  };
  const onOutside = (e) => { if (!e.composedPath().includes(host)) close(); };
  const onKey = (e) => { if (e.key === "Escape") close(); };
  addEventListener("message", onMessage, true);
  addEventListener("mousedown", onOutside, true);
  addEventListener("keydown", onKey, true);
  // 接続後に読み込みを始め、準備完了まで空の枠を見せない。
  shadow.append(frame);
  document.documentElement.append(host);
  frame.src = src;
  return result;
}
