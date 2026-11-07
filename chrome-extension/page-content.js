// ページ全体の翻訳 (ページ側)。background が chrome.scripting で注入する。
// 段落 (ブロック要素) ごとに文を集めて background (page-task.mjs) に送り、届いた訳を元の位置に書き込む。
// 段落の中のリンクや強調などの区切りは保てないので、訳は段落の最初の文字のノードにまとめて入れる。
// 元の文は覚えておき、「原文に戻す」で戻す
(() => {
  "use strict";
  if (globalThis.__ptranslatePage) {
    globalThis.__ptranslatePage.toggle();
    return;
  }

  // 訳さない要素 (コード・入力欄・埋め込みなど)
  const SKIP_TAGS = new Set([
    "SCRIPT", "STYLE", "NOSCRIPT", "TEMPLATE", "TEXTAREA", "INPUT", "SELECT", "OPTION", "CODE", "PRE", "KBD",
    "SAMP", "VAR", "SVG", "MATH", "CANVAS", "IFRAME", "OBJECT", "EMBED", "VIDEO", "AUDIO",
  ]);
  const displayCache = new Map();
  const originals = new Map(); // 文字のノード → 元の文
  let blocks = []; // { id, nodes, text }
  let port = null;
  let state = "idle"; // idle・running・done・stopped・error・restored

  function isBlock(el) {
    let d = displayCache.get(el);
    if (d === undefined) {
      d = getComputedStyle(el).display;
      displayCache.set(el, d);
    }
    return !(d.startsWith("inline") || d === "contents" || d === "ruby" || d === "ruby-text");
  }

  function skipped(el) {
    return SKIP_TAGS.has(el.tagName) || el.isContentEditable || el.getAttribute("translate") === "no" ||
      el.classList.contains("notranslate") || el.id === "ptranslate-extension-overlay" ||
      el.id === "ptranslate-page-bar";
  }

  function collect() {
    const groups = new Map(); // ブロック要素 → 文字のノード
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT, {
      acceptNode(node) {
        if (node.nodeType === Node.ELEMENT_NODE) {
          return skipped(node) ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_SKIP;
        }
        return node.data.trim() ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_SKIP;
      },
    });
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      let block = node.parentElement;
      while (block && block !== document.body && !isBlock(block)) block = block.parentElement;
      block ||= document.body;
      if (!groups.has(block)) groups.set(block, []);
      groups.get(block).push(node);
    }
    const list = [];
    for (const [el, nodes] of groups) {
      if (el.checkVisibility && !el.checkVisibility({ visibilityProperty: true })) continue;
      const text = nodes.map((n) => n.data).join("").replace(/\s+/g, " ").trim();
      if (!text) continue;
      const rect = el.getBoundingClientRect();
      const onScreen = rect.bottom > 0 && rect.top < innerHeight;
      list.push({ id: list.length, nodes, text, onScreen });
    }
    return list;
  }

  function write(block, text) {
    const [first, ...rest] = block.nodes;
    for (const n of block.nodes) if (!originals.has(n)) originals.set(n, n.data);
    const lead = originals.get(first).match(/^\s*/)[0];
    const last = block.nodes[block.nodes.length - 1];
    const trail = originals.get(last).match(/\s*$/)[0];
    first.data = lead + text + (rest.length ? "" : trail);
    for (const n of rest) n.data = n === last ? trail : "";
  }

  function restore() {
    for (const [node, data] of originals) node.data = data;
    originals.clear();
  }

  // 進み具合のバー (ページの CSS が混ざらないよう shadow DOM に入れる)
  const host = document.createElement("div");
  host.id = "ptranslate-page-bar";
  const shadow = host.attachShadow({ mode: "closed" });
  shadow.innerHTML = `<style>
    :host { all: initial; }
    .bar { position: fixed; top: 12px; right: 12px; z-index: 2147483647; display: flex; align-items: center; gap: 8px;
      max-width: calc(100vw - 24px); box-sizing: border-box; padding: 8px 10px; border-radius: 10px;
      font: 13px/1.5 system-ui, -apple-system, "Segoe UI", "Hiragino Sans", "Noto Sans JP", sans-serif;
      background: #ffffff; color: #1d2330; border: 1px solid #dde1e8; box-shadow: 0 6px 24px rgba(0,0,0,.18); }
    .msg { flex: 1; min-width: 0; }
    .err { color: #c43c3c; } .ok { color: #1f8a4c; } .warn { color: #b26a00; }
    button { font: inherit; color: inherit; background: transparent; border: 1px solid #dde1e8; border-radius: 6px;
      padding: 2px 8px; cursor: pointer; white-space: nowrap; }
    button.primary { background: #2f6fde; color: #fff; border-color: #2f6fde; }
    button[hidden] { display: none; }
    @media (prefers-color-scheme: dark) {
      .bar { background: #1d2128; color: #e7eaf0; border-color: #2e343e; }
      button { border-color: #2e343e; } button.primary { background: #5b8ff0; color: #0d1117; border-color: #5b8ff0; }
      .err { color: #ef6b6b; } .ok { color: #4cc283; } .warn { color: #e3a33c; }
    }
  </style>
  <div class="bar" role="status" aria-live="polite">
    <strong>PTranslate</strong>
    <span class="msg"></span>
    <button data-act="stop">中止</button>
    <button data-act="restore">原文に戻す</button>
    <button data-act="again" class="primary">訳す</button>
    <button data-act="login">Web版を開いてログイン</button>
    <button data-act="options">拡張の設定</button>
    <button data-act="close" aria-label="閉じる">×</button>
  </div>`;
  const $ = (sel) => shadow.querySelector(sel);
  const buttons = Object.fromEntries([...shadow.querySelectorAll("button")].map((b) => [b.dataset.act, b]));

  function show(text, cls = "", visible = []) {
    $(".msg").textContent = text;
    $(".msg").className = "msg " + cls;
    for (const [act, b] of Object.entries(buttons)) b.hidden = act !== "close" && !visible.includes(act);
    if (!host.isConnected) document.documentElement.append(host);
  }

  function stopPort() {
    if (!port) return;
    try {
      port.disconnect();
    } catch {
      /* もう閉じている */
    }
    port = null;
  }

  function start() {
    stopPort();
    restore();
    blocks = collect();
    if (!blocks.length) {
      state = "error";
      show("訳す文が見つかりませんでした", "err", []);
      return;
    }
    state = "running";
    show("訳す準備をしています", "", ["stop"]);
    const byId = new Map(blocks.map((b) => [b.id, b]));
    // 画面に見えている段落から訳す
    const ordered = [...blocks.filter((b) => b.onScreen), ...blocks.filter((b) => !b.onScreen)];
    const mine = chrome.runtime.connect({ name: "page-translate" });
    port = mine;
    let total = 0;
    let target = "";
    mine.onMessage.addListener((msg) => {
      if (port !== mine) return;
      if (msg.type === "start") {
        total = msg.total;
        target = msg.targetName;
        show(total ? `${target}に訳しています (0 / ${total})` : "訳す段落がありませんでした", total ? "" : "ok",
          total ? ["stop"] : []);
      } else if (msg.type === "partial") {
        const b = byId.get(msg.id);
        if (b) write(b, msg.text);
      } else if (msg.type === "block") {
        const b = byId.get(msg.id);
        if (b) write(b, msg.text);
        show(`${target}に訳しています (${msg.done} / ${msg.total})`, "", ["stop", "restore"]);
      } else if (msg.type === "done") {
        state = "done";
        stopPort();
        if (msg.truncated) {
          show(`訳し終わりました (${msg.truncated} か所は長すぎて途中で切れました)`, "warn", ["restore"]);
        } else show("訳し終わりました", "ok", ["restore"]);
      } else if (msg.type === "error") {
        state = "error";
        stopPort();
        const fix = msg.code === "login" ? ["login"] : msg.remedy === "options" ? ["options"] : [];
        show(msg.message, "err", [...fix, ...(originals.size ? ["restore"] : []), "again"]);
      }
    });
    mine.onDisconnect.addListener(() => {
      if (port !== mine) return;
      port = null;
      state = "error";
      show("翻訳サーバーに接続できません", "err", [...(originals.size ? ["restore"] : []), "again"]);
    });
    mine.postMessage({ blocks: ordered.map(({ id, text }) => ({ id, text })) });
  }

  buttons.stop.addEventListener("click", () => {
    stopPort();
    state = "stopped";
    show("中止しました", "", [...(originals.size ? ["restore"] : []), "again"]);
  });
  buttons.restore.addEventListener("click", () => {
    stopPort();
    restore();
    state = "restored";
    show("原文に戻しました", "", ["again"]);
  });
  buttons.again.addEventListener("click", start);
  buttons.close.addEventListener("click", () => host.remove());
  for (const what of ["login", "options"]) {
    buttons[what].addEventListener("click", () => {
      void chrome.runtime.sendMessage({ type: "open", what });
    });
  }

  globalThis.__ptranslatePage = {
    // もう一度呼ばれたら、訳している途中・訳した後はバーを出し直し、それ以外は訳し直す
    toggle() {
      if (state === "running" || state === "done") {
        if (!host.isConnected) document.documentElement.append(host);
      } else start();
    },
  };
  start();
})();
