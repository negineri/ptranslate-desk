// ページ全体の翻訳 (ページ側)。background が chrome.scripting で注入する。
// 同じ要素の中で連続する文を background (page-task.mjs) に送り、届いた訳を元の位置に書き込む。
// リンクや強調の内外は別々に訳し、操作と装飾を元の要素に残す。
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
  const originals = new Map(); // 文字のノード → 元の文
  const written = new Map(); // 自分が最後に書いた値。サイト側の変更とは区別する
  const parents = new Map();
  let nextId = 0;
  let scanTimer = null;
  let rootsTimer = null;
  const roots = new Set();
  const observeOptions = { childList: true, characterData: true, subtree: true,
    attributes: true, attributeFilter: ["class", "style", "hidden", "open", "translate", "contenteditable"] };
  let following = false;
  const observer = new MutationObserver((records) => {
    // ストリーミング中に自分が書いた文字で再収集を繰り返さない。
    if (records.some((r) => r.type !== "characterData" || written.get(r.target) !== r.target.data ||
        parents.get(r.target) !== r.target.parentNode)) scheduleScan();
  });
  let blocks = []; // { id, nodes, text }
  let port = null;
  let priorityFrame = null;
  let finishedOnce = false;
  let activityTimer = null;
  let pendingActivity = null;
  let deferActivity = false;
  let state = "idle"; // idle・running・done・stopped・error・restored

  function skipped(el) {
    return SKIP_TAGS.has(el.tagName) || el.isContentEditable || el.getAttribute("translate") === "no" ||
      el.classList.contains("notranslate") || el.id === "ptranslate-extension-overlay" ||
      el.id === "ptranslate-page-bar";
  }

  // open Shadow DOM の境界を越えて、画面上の祖先をたどる。
  function textContainer(node) {
    return node.parentElement || node.getRootNode().host || null;
  }

  function composedParent(el) {
    return el.assignedSlot || el.parentElement || el.getRootNode().host || null;
  }

  function watchRoots(root = document.body) {
    let added = false;
    for (const el of root.querySelectorAll("*")) {
      if (!el.shadowRoot || skipped(el)) continue;
      if (!roots.has(el.shadowRoot)) {
        roots.add(el.shadowRoot);
        observer.observe(el.shadowRoot, observeOptions);
        added = true;
      }
      if (watchRoots(el.shadowRoot)) added = true;
    }
    return added;
  }

  // 読み上げ用の文字はcheckVisibilityでも可視扱いになることがある。
  // clipやclip-pathで面積が0になる祖先を、収集と画面内の優先判定の両方から除く。
  function emptyClip(el, style) {
    const clip = style.clip?.match(/^rect\((.*)\)$/);
    if (clip && ["absolute", "fixed"].includes(style.position)) {
      const edges = clip[1].trim().split(/[,\s]+/).map((value) =>
        /^-?(?:\d+\.?\d*|\.\d+)(?:px)?$/.test(value) ? parseFloat(value) : NaN);
      if (edges.length === 4 && (edges[2] <= edges[0] || edges[1] <= edges[3])) return true;
    }
    const inset = style.clipPath?.match(/^inset\(([^)]*)\)$/);
    if (!inset) return false;
    const values = inset[1].split(/\s+round\s+/)[0].trim().split(/\s+/);
    if (values.length < 1 || values.length > 4) return false;
    const [top, right = top, bottom = top, left = right] = values;
    const rect = el.getBoundingClientRect();
    const pixels = (value, size) => {
      const match = value.match(/^(-?(?:\d+\.?\d*|\.\d+))(px|%)?$/);
      return match ? Number(match[1]) * (match[2] === "%" ? size / 100 : 1) : NaN;
    };
    return pixels(top, rect.height) + pixels(bottom, rect.height) >= rect.height ||
      pixels(left, rect.width) + pixels(right, rect.width) >= rect.width;
  }

  function fullyClipped(el, cache) {
    const path = [];
    let clipped = false;
    for (let current = el; current; current = composedParent(current)) {
      if (cache.has(current)) { clipped = cache.get(current); break; }
      path.push(current);
      if (emptyClip(current, getComputedStyle(current))) { clipped = true; break; }
    }
    for (const current of path) cache.set(current, clipped);
    return clipped;
  }

  function textVisible(el, clipCache) {
    if (!el || !el.isConnected) return false;
    // display:contents の要素自身には箱がなく、checkVisibility() は false を返す。
    // 文字の visibility を調べたうえで、箱のある祖先に表示状態を問い合わせる。
    if (["hidden", "collapse"].includes(getComputedStyle(el).visibility)) return false;
    let box = el;
    while (box && getComputedStyle(box).display === "contents") box = composedParent(box);
    return !!box && (!box.checkVisibility || box.checkVisibility({ opacityProperty: true })) && !fullyClipped(el, clipCache);
  }

  // 文字そのものの位置を、画面と祖先のスクロール領域で切り取って判定する。
  // 大きな親要素の矩形だけを見ると、その中の画面外の文まで見えている扱いになる。
  function onScreen(block, clipCache, viewportElements) {
    return block.nodes.some((node) => {
      const el = node.assignedSlot || textContainer(node);
      if (!node.isConnected || !textVisible(el, clipCache)) return false;
      let left = 0, top = 0, right = innerWidth, bottom = innerHeight;
      for (let parent = el; parent; parent = composedParent(parent)) {
        if (viewportElements.has(parent)) continue;
        const style = getComputedStyle(parent);
        if (style.display === "contents") continue;
        const clipX = /^(auto|scroll|hidden|clip|overlay)$/.test(style.overflowX);
        const clipY = /^(auto|scroll|hidden|clip|overlay)$/.test(style.overflowY);
        if (!clipX && !clipY) continue;
        const rect = parent.getBoundingClientRect();
        if (clipX) {
          left = Math.max(left, rect.left + parent.clientLeft);
          right = Math.min(right, rect.left + parent.clientLeft + parent.clientWidth);
        }
        if (clipY) {
          top = Math.max(top, rect.top + parent.clientTop);
          bottom = Math.min(bottom, rect.top + parent.clientTop + parent.clientHeight);
        }
      }
      if (left >= right || top >= bottom) return false;
      const range = document.createRange();
      range.selectNodeContents(node);
      return [...range.getClientRects()].some((r) =>
        r.width > 0 && r.height > 0 && r.right > left && r.left < right && r.bottom > top && r.top < bottom);
    });
  }

  // チャット本文や記事を、同時に見えているナビゲーションより先に訳す。
  // サイト固有のクラス名には依存せず、HTML と ARIA の本文領域を使う。
  function contentPriority(block) {
    let rank = 2;
    for (let el = textContainer(block.nodes[0]); el; el = composedParent(el)) {
      if (el.matches('article, [role="article"]')) return 0;
      if (el.matches('main, [role="main"]')) rank = 1;
    }
    return rank;
  }

  function visibleBlocks() {
    const clipCache = new WeakMap();
    // rootのoverflow、またrootがvisibleのときのbodyのoverflowは、画面全体へ適用される。
    // スクロール後のbodyの矩形（topが負）で再度切ると、画面内の文字まで消えてしまう。
    const root = document.documentElement;
    const viewportElements = new Set([root, document.scrollingElement]);
    const style = getComputedStyle(root);
    if ((!style.overflowX || style.overflowX === "visible") && (!style.overflowY || style.overflowY === "visible")) {
      viewportElements.add(document.body);
    }
    return blocks.filter((block) => onScreen(block, clipCache, viewportElements)).sort((a, b) => contentPriority(a) - contentPriority(b));
  }

  function schedulePriority() {
    if (!port || priorityFrame !== null) return;
    priorityFrame = requestAnimationFrame(() => {
      priorityFrame = null;
      port?.postMessage({ type: "priority", ids: visibleBlocks().map((b) => b.id) });
    });
  }

  function sourceData(node) {
    if (originals.has(node) && written.get(node) === node.data && parents.get(node) === node.parentNode) {
      return originals.get(node);
    }
    originals.delete(node);
    written.delete(node);
    parents.delete(node);
    return node.data;
  }

  function current(block) {
    return block.nodes.every((n, i) => n.isConnected && n.parentNode === block.parents[i] && n.data === block.expected[i]);
  }

  function scheduleScan() {
    if (!following || scanTimer !== null) return;
    scanTimer = setTimeout(() => {
      scanTimer = null;
      scan();
    }, 150);
  }

  function stopFollowing() {
    following = false;
    observer.disconnect();
    roots.clear();
    clearInterval(rootsTimer);
    rootsTimer = null;
    clearTimeout(scanTimer);
    scanTimer = null;
  }

  function scan() {
    if (!following) return;
    let removedRoot = false;
    for (const root of roots) {
      if (!root.host.isConnected) { roots.delete(root); removedRoot = true; }
    }
    if (removedRoot) {
      observer.disconnect();
      observer.observe(document.body, observeOptions);
      for (const root of roots) observer.observe(root, observeOptions);
    }
    watchRoots();
    for (const node of originals.keys()) {
      if (!node.isConnected) {
        originals.delete(node);
        written.delete(node);
        parents.delete(node);
      }
    }
    const old = new Map(blocks.map((b) => [b.nodes[0], b]));
    const next = collect().map((b) => {
      const previous = old.get(b.nodes[0]);
      if (previous && previous.text === b.text && previous.nodes.length === b.nodes.length &&
          previous.nodes.every((n, i) => n === b.nodes[i]) && current(previous)) return previous;
      return { ...b, skipped: !/\p{L}/u.test(b.text), id: nextId++, parents: b.nodes.map((n) => n.parentNode), expected: b.nodes.map((n) => n.data) };
    });
    const ids = new Set(next.map((b) => b.id));
    const removedPending = blocks.some((b) => !b.done && !b.skipped && !ids.has(b.id));
    const changed = next.length !== blocks.length || next.some((b, i) => b !== blocks[i]);
    blocks = next;
    if (!changed) return;
    const pending = blocks.filter((b) => !b.done && !b.skipped);
    if (removedPending) stopPort(); // チャンネルの古い要求を中断して、新しい本文へ切り替える
    if (!pending.length) {
      if (!port) {
        state = "done";
        show(originals.size ? "完了" : "待機中", originals.size ? "ok" : "", ["stop", ...(originals.size ? ["restore", "again"] : [])]);
      }
      return;
    }
    if (port) {
      port.postMessage({ type: "update", blocks: orderedPending().map(({ id, text }) => ({ id, text })),
        ids: visibleBlocks().map((b) => b.id) });
    } else launch();
  }

  function orderedPending() {
    const visible = visibleBlocks();
    const ids = new Set(visible.map((b) => b.id));
    return [...visible, ...blocks.filter((b) => !ids.has(b.id))].filter((b) => !b.done && !b.skipped);
  }

  function collect() {
    const clipCache = new WeakMap();
    const groups = []; // 同じ親要素を持つ、連続する文字のノード
    function visit(root) {
      const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT, {
        acceptNode(node) {
          if (node.nodeType === Node.ELEMENT_NODE) {
            if (skipped(node)) return NodeFilter.FILTER_REJECT;
            return node.shadowRoot ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_SKIP;
          }
          return sourceData(node).trim() ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_SKIP;
        },
      });
      let previous = null;
      for (let node = walker.nextNode(); node; node = walker.nextNode()) {
        if (node.nodeType === Node.ELEMENT_NODE) {
          visit(node.shadowRoot);
          previous = null;
          continue;
        }
        const el = textContainer(node);
        if (!textVisible(el, clipCache)) {
          previous = null;
          continue;
        }
        // 親が同じでも、間にリンクなどがあれば別のまとまりにする。
        if (!previous || previous.nextSibling !== node || previous.parentNode !== node.parentNode) {
          groups.push([el, []]);
        }
        groups.at(-1)[1].push(node);
        previous = node;
      }
    }
    visit(document.body);
    const list = [];
    for (const [el, nodes] of groups) {
      if (!textVisible(el, clipCache)) continue;
      const text = nodes.map(sourceData).join("").replace(/\s+/g, " ").trim();
      if (!text) continue;
      list.push({ id: list.length, nodes, text });
    }
    return list;
  }

  function write(block, text) {
    if (!current(block)) return;
    const [first, ...rest] = block.nodes;
    for (const n of block.nodes) if (!originals.has(n)) {
      originals.set(n, n.data);
      parents.set(n, n.parentNode);
    }
    const lead = originals.get(first).match(/^\s*/)[0];
    const last = block.nodes[block.nodes.length - 1];
    const trail = originals.get(last).match(/\s*$/)[0];
    first.data = lead + text + (rest.length ? "" : trail);
    for (const n of rest) n.data = n === last ? trail : "";
    block.expected = block.nodes.map((n) => n.data);
    for (const n of block.nodes) written.set(n, n.data);
  }

  function restore() {
    for (const [node, data] of originals) {
      if (node.isConnected && node.parentNode === parents.get(node) && node.data === written.get(node)) node.data = data;
    }
    originals.clear();
    written.clear();
    parents.clear();
  }

  // 進み具合のバー (ページの CSS が混ざらないよう shadow DOM に入れる)
  const host = document.createElement("div");
  host.id = "ptranslate-page-bar";
  const shadow = host.attachShadow({ mode: "closed" });
  shadow.innerHTML = `<style>
    :host { all: initial; }
    .bar { position: fixed; top: 12px; right: 12px; z-index: 2147483647; display: grid; gap: 4px;
      width: 260px; max-width: calc(100vw - 24px); box-sizing: border-box; padding: 6px; border-radius: 10px;
      font: 13px/1.5 system-ui, -apple-system, "Segoe UI", "Hiragino Sans", "Noto Sans JP", sans-serif;
      background: #ffffff; color: #1d2330; border: 1px solid #dde1e8; box-shadow: 0 6px 24px rgba(0,0,0,.18); }
    .heading, .actions { display: flex; align-items: center; gap: 4px; }
    .heading strong { font-size: 12px; }
    .msg { flex: 1; min-width: 0; display: block; height: 20px; line-height: 20px; overflow: hidden; text-overflow: ellipsis;
      white-space: nowrap; font-variant-numeric: tabular-nums; }
    .actions { flex-wrap: wrap; min-height: 26px; }
    .err { color: #c43c3c; } .ok { color: #1f8a4c; } .warn { color: #b26a00; }
    button { font: inherit; color: inherit; background: transparent; border: 1px solid #dde1e8; border-radius: 6px;
      padding: 2px 6px; cursor: pointer; white-space: nowrap; }
    button.primary { background: #2f6fde; color: #fff; border-color: #2f6fde; }
    button:disabled { opacity: .45; cursor: default; }
    button[hidden] { display: none; }
    @media (prefers-color-scheme: dark) {
      .bar { background: #1d2128; color: #e7eaf0; border-color: #2e343e; }
      button { border-color: #2e343e; } button.primary { background: #5b8ff0; color: #0d1117; border-color: #5b8ff0; }
      .err { color: #ef6b6b; } .ok { color: #4cc283; } .warn { color: #e3a33c; }
    }
  </style>
  <div class="bar" role="region" aria-label="ページ翻訳">
    <div class="heading"><strong>PTranslate</strong>
      <span class="msg" role="status" aria-live="polite"></span>
      <button data-act="close" aria-label="閉じる">×</button>
    </div>
    <div class="actions">
      <button data-act="stop">中止</button>
      <button data-act="restore">原文に戻す</button>
      <button data-act="again" class="primary">再翻訳</button>
      <button data-act="login">ログイン</button>
      <button data-act="options">設定</button>
    </div>
  </div>`;
  const $ = (sel) => shadow.querySelector(sel);
  const buttons = Object.fromEntries([...shadow.querySelectorAll("button")].map((b) => [b.dataset.act, b]));

  function show(text, cls = "", visible = [], detail = text) {
    const message = $(".msg");
    if (message.textContent !== text) message.textContent = text;
    if (message.title !== detail) message.title = detail;
    if (message.className !== "msg " + cls) message.className = "msg " + cls;
    // 通常のボタンは表示したままにし、状態によって位置が動かないようにする。
    for (const act of ["stop", "restore", "again"]) buttons[act].disabled = !visible.includes(act);
    for (const act of ["login", "options"]) buttons[act].hidden = !visible.includes(act);
    if (!host.isConnected) document.documentElement.append(host);
  }

  function clearActivity() {
    clearTimeout(activityTimer); activityTimer = null; pendingActivity = null; deferActivity = false;
  }
  function activity(...args) {
    if (!deferActivity) return show(...args);
    pendingActivity = args;
    // 完了後の短い追加翻訳では、完了→準備中→完了の点滅を出さない。
    if (activityTimer === null) activityTimer = setTimeout(() => {
      const latest = pendingActivity;
      clearActivity();
      if (latest && port) show(...latest);
    }, 400);
  }
  function stopPort() {
    clearActivity();
    removeEventListener("scroll", schedulePriority, true);
    removeEventListener("resize", schedulePriority);
    if (priorityFrame !== null) cancelAnimationFrame(priorityFrame);
    priorityFrame = null;
    if (!port) return;
    try {
      port.disconnect();
    } catch {
      /* もう閉じている */
    }
    port = null;
  }

  function start() {
    stopFollowing();
    stopPort();
    restore();
    finishedOnce = false;
    blocks = [];
    following = true;
    state = "idle";
    scan();
    observer.observe(document.body, observeOptions);
    // attachShadow 自体は MutationObserver に通知されないので、遅れて作られた境界も探す。
    rootsTimer = setInterval(() => { if (watchRoots()) scheduleScan(); }, 1000);
    if (!blocks.length) show("待機中", "", ["stop"]);
  }

  function launch() {
    state = "running";
    deferActivity = finishedOnce;
    activity("準備中", "", ["stop", ...(originals.size ? ["restore"] : [])]);
    const visibleIds = visibleBlocks().map((b) => b.id);
    const ordered = orderedPending();
    const mine = chrome.runtime.connect({ name: "page-translate" });
    port = mine;
    let total = 0;
    let done = 0;
    const waiting = new Set();
    const progress = () => activity(waiting.size
      ? `順番待ち ${done} / ${total}`
      : `翻訳中 ${done} / ${total}`, "", ["stop", ...(originals.size ? ["restore"] : [])]);
    mine.onMessage.addListener((msg) => {
      if (port !== mine) return;
      if (msg.type === "start") {
        total = msg.total;
        activity(total ? `翻訳中 0 / ${total}` : "対象なし", total ? "" : "ok",
          total ? ["stop"] : []);
      } else if (msg.type === "waiting") {
        if (msg.waiting) waiting.add(msg.id);
        else waiting.delete(msg.id);
        progress();
      } else if (msg.type === "partial") {
        const b = blocks.find((b) => b.id === msg.id);
        if (b) write(b, msg.text);
      } else if (msg.type === "block") {
        const b = blocks.find((b) => b.id === msg.id);
        if (b) {
          write(b, msg.text);
          if (current(b)) b.done = true;
        }
        done = msg.done;
        total = msg.total;
        progress();
        schedulePriority();
      } else if (msg.type === "done") {
        state = "done";
        stopPort();
        const warnings = [];
        finishedOnce = true;
        if (msg.truncated) warnings.push(`途中まで: ${msg.truncated}件`);
        if (msg.omitted) warnings.push(`未翻訳: ${msg.omitted}件`);
        if (warnings.length) show("一部未翻訳", "warn", ["stop", "restore", "again"], warnings.join(" / "));
        else show("完了", "ok", ["stop", "restore", "again"]);
        if (msg.omitted) {
          for (const b of blocks) if (!b.done) b.skipped = true;
        } else if (following && blocks.some((b) => !b.done && !b.skipped)) {
          // 完了直前に追加された文章が background に届かなかった場合も拾う。
          scan();
          if (!port && blocks.some((b) => !b.done && !b.skipped)) launch();
        }
      } else if (msg.type === "error") {
        state = "error";
        stopFollowing();
        stopPort();
        const fix = msg.code === "login" ? ["login"] : msg.remedy === "options" ? ["options"] : [];
        const text = msg.code === "login" ? "ログインが必要です" : msg.remedy === "options" || msg.code === "local" ? "接続を確認してください" : msg.code === "same-lang" ? "原文と同じ言語です" : "翻訳できません";
        show(text, "err", [...fix, ...(originals.size ? ["restore"] : []), "again"], msg.message);
      }
    });
    mine.onDisconnect.addListener(() => {
      if (port !== mine) return;
      stopPort();
      stopFollowing();
      state = "error";
      show("接続できません", "err", ["options", ...(originals.size ? ["restore"] : []), "again"]);
    });
    mine.postMessage({ blocks: ordered.map(({ id, text }) => ({ id, text })), visibleIds });
    // scroll はバブルしないので capture で、ページ内のスクロール領域も受ける。
    addEventListener("scroll", schedulePriority, true);
    addEventListener("resize", schedulePriority);
  }

  buttons.stop.addEventListener("click", () => {
    stopFollowing();
    stopPort();
    state = "stopped";
    show("中止", "", [...(originals.size ? ["restore"] : []), "again"]);
  });
  buttons.restore.addEventListener("click", () => {
    stopFollowing();
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
