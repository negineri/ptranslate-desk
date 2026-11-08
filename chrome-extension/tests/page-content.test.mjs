// content script を DOM と通信のモックで動かす。翻訳エンジンには接続しない。
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { JSDOM } from "jsdom";

const script = await readFile(new URL("../dist/page-content.js", import.meta.url), "utf8");

function page(html, setup = () => {}) {
  const dom = new JSDOM(html, { runScripts: "outside-only", pretendToBeVisual: true });
  const w = dom.window;
  // jsdom はレイアウトと checkVisibility を実装しないため、可視性だけを模擬する。
  w.Element.prototype.checkVisibility = function () {
    for (let el = this; el; el = el.parentElement || el.getRootNode().host || null) {
      if (w.getComputedStyle(el).display === "none") return false;
    }
    return !["hidden", "collapse"].includes(w.getComputedStyle(this).visibility);
  };
  w.Range.prototype.getClientRects = function () {
    return [this.startContainer.parentElement.getBoundingClientRect()];
  };
  setup(w);
  let shadow;
  const attach = w.Element.prototype.attachShadow;
  w.Element.prototype.attachShadow = function (options) {
    shadow = attach.call(this, options);
    return shadow;
  };
  let receive;
  let sent;
  const messages = [];
  const connections = [];
  w.chrome = { runtime: { connect: () => {
    const connection = { messages: [], disconnected: false };
    connections.push(connection);
    return {
      onMessage: { addListener: (f) => { receive = f; connection.receive = f; } },
      onDisconnect: { addListener() {} },
      postMessage: (msg) => { sent = msg; messages.push(msg); connection.messages.push(msg); },
      disconnect() { connection.disconnected = true; },
    };
  } } };
  const original = w.document.body.innerHTML;
  w.eval(script);
  return { w, original, blocks: sent?.blocks || [], messages, connections, receive: (msg) => receive(msg), shadow };
}

test("リンク・ボタン・強調の文字と要素を保ち、原文へ戻せる", () => {
  const p = page('<p>Hello <a href="/account"><strong>account</strong></a> today.</p><button>Open</button>');
  const { document } = p.w;
  const link = document.querySelector("a");
  let clicked = false;
  link.addEventListener("click", (e) => { e.preventDefault(); clicked = true; });
  assert.deepEqual(Array.from(p.blocks, (b) => b.text), ["Hello", "account", "today.", "Open"]);
  const translations = ["こんにちは", "アカウント", "今日。", "開く"];
  for (const b of p.blocks) p.receive({ type: "block", id: b.id, text: translations[b.id], done: b.id + 1, total: 4 });
  assert.equal(link.textContent, "アカウント");
  assert.equal(document.querySelector("strong").textContent, "アカウント");
  assert.equal(document.querySelector("a"), link);
  assert.equal(link.getAttribute("href"), "/account");
  link.click();
  assert.ok(clicked);
  assert.equal(document.querySelector("button").textContent, "開く");
  assert.match(document.querySelector("p").textContent, /こんにちは アカウント 今日。/);
  p.shadow.querySelector('[data-act="restore"]').click();
  assert.equal(document.body.innerHTML, p.original);
  p.w.close();
});

test("段落内の非表示の文字を送らず、書き換えない", () => {
  const p = page('<p>Hello <span style="visibility:hidden">Hidden secret</span> world.</p><p style="display:none">Invisible</p><p><span style="display:none">Hidden</span>Visible</p><pre>Code</pre>');
  assert.deepEqual(Array.from(p.blocks, (b) => b.text), ["Hello", "world.", "Visible"]);
  for (const b of p.blocks) p.receive({ type: "block", id: b.id, text: "訳", done: 1, total: 3 });
  assert.equal(p.w.document.querySelector("span").textContent, "Hidden secret");
  assert.equal(p.w.document.querySelector("pre").textContent, "Code");
  p.w.close();
});

test("clipで隠した見出し・リンクとその子を、画面内の本文より先に送らない", () => {
  const p = page('<main><h1 style="position:absolute;clip:rect(0px,0px,0px,0px)">Hidden heading</h1><div style="position:absolute;clip:rect(0px,0px,0px,0px)"><a><span>Hidden link</span></a></div><h2>Visible headline</h2><p>Visible paragraph</p></main>', (w) => {
    w.Range.prototype.getClientRects = () => [rect(0, 10, 200, 20)];
  });
  assert.deepEqual(Array.from(p.blocks, (b) => b.text), ["Visible headline", "Visible paragraph"]);
  assert.deepEqual(Array.from(p.messages[0].visibleIds), [0, 1]);
  p.w.close();
});

test("clip-pathで全体を隠した文字を省き、一部が残る文字と通常の画面外の文は収集する", () => {
  const p = page('<span style="clip-path:inset(50%)">Clipped label</span><span style="clip-path:inset(0px 50% 0px 50%)">Clipped horizontally</span><span style="clip-path:inset(1px)">Partial clipping</span><span style="position:absolute;clip:rect(0px,100px,20px,0px)">Partial rectangle</span><span style="position:relative;clip:rect(0px,0px,0px,0px)">Inactive clip</span><p>Outside viewport</p>', (w) => {
    w.Element.prototype.getBoundingClientRect = () => rect(0, 2000, 100, 20);
  });
  assert.deepEqual(Array.from(p.blocks, (b) => b.text), ["Partial clipping", "Partial rectangle", "Inactive clip", "Outside viewport"]);
  assert.deepEqual(Array.from(p.messages[0].visibleIds), []);
  p.w.close();
});

test("切り抜きが解除されて表示された文字は、変更後に翻訳対象へ追加する", async () => {
  const p = page('<main><div id="clipped" style="position:absolute;clip:rect(0px,0px,0px,0px)"><h2>Newly visible headline</h2></div><p>Visible paragraph</p></main>', (w) => {
    w.Range.prototype.getClientRects = () => [rect(0, 10, 200, 20)];
  });
  assert.deepEqual(Array.from(p.blocks, (b) => b.text), ["Visible paragraph"]);
  p.w.document.querySelector("#clipped").style.clip = "auto";
  await new Promise((resolve) => setTimeout(resolve, 220));
  const update = p.messages.findLast((message) => message.type === "update");
  assert.ok(Array.from(update.blocks).some((block) => block.text === "Newly visible headline"));
  p.w.close();
});

test("画面外の文章も収集し、上限による未翻訳を完了時に表示する", () => {
  const p = page('<p>Hello</p><p style="margin-top:20000px">Outside viewport</p>');
  assert.equal(p.blocks.length, 2);
  p.receive({ type: "done", total: 3000, truncated: 2, omitted: 5 });
  const message = p.shadow.querySelector(".msg");
  assert.match(message.textContent, /5 か所は訳していません/);
  assert.match(message.textContent, /2 か所は長すぎて途中で切れました/);
  assert.ok(message.classList.contains("warn"));
  p.w.close();
});

function rect(left, top, width, height) {
  return { left, top, width, height, right: left + width, bottom: top + height };
}

test("縦横のスクロール領域で隠れた文より、実際に見えている文を先に送る", () => {
  const p = page('<div id="clip" style="overflow-y:auto;overflow-x:hidden"><p id="above">Clipped above</p><p id="right">Clipped right</p><p id="visible">Visible message</p></div><p id="outside">Below viewport</p>', (w) => {
    const clip = w.document.querySelector("#clip");
    clip.getBoundingClientRect = () => rect(100, 100, 300, 200);
    Object.defineProperties(clip, { clientWidth: { value: 300 }, clientHeight: { value: 200 } });
    for (const [id, r] of Object.entries({ above: rect(110, 50, 100, 20), right: rect(410, 110, 100, 20), visible: rect(110, 110, 100, 20), outside: rect(0, 2000, 100, 20) })) {
      w.document.getElementById(id).getBoundingClientRect = () => r;
    }
  });
  assert.deepEqual(Array.from(p.blocks, (b) => b.text), ["Visible message", "Clipped above", "Clipped right", "Below viewport"]);
  assert.deepEqual(Array.from(p.messages[0].visibleIds), [2]);
  p.w.close();
});

test("ページ途中から開始しても、画面全体へ適用されるbodyのoverflowで表示中の段落を除外しない", async () => {
  let visibleId = 200;
  const p = page(Array.from({ length: 250 }, (_, id) => `<p>Paragraph ${id}</p>`).join(""), (w) => {
    const root = w.document.documentElement, body = w.document.body;
    root.style.overflow = "visible";
    body.style.overflowX = "hidden"; body.style.overflowY = "auto";
    Object.defineProperty(w.document, "scrollingElement", { value: root });
    for (const el of [root, body]) {
      el.getBoundingClientRect = () => rect(0, -5000, 1024, w.innerHeight);
      Object.defineProperties(el, { clientWidth: { value: 1024 }, clientHeight: { value: w.innerHeight } });
    }
    w.Range.prototype.getClientRects = function () {
      const id = Number(this.startContainer.data.split(" ")[1]);
      return [rect(0, (id - visibleId) * 1000 + 10, 100, 20)];
    };
  });
  assert.equal(p.blocks[0].text, "Paragraph 200");
  assert.deepEqual(Array.from(p.messages[0].visibleIds), [200]);
  visibleId = 220;
  p.w.dispatchEvent(new p.w.Event("scroll"));
  await new Promise((resolve) => p.w.requestAnimationFrame(resolve));
  assert.deepEqual(Array.from(p.messages.at(-1).ids), [220]);
  p.w.close();
});

test("rootにoverflowがあるページでもroot自身は画面の矩形で判定する", () => {
  const p = page('<p>Visible paragraph</p><p>Outside paragraph</p>', (w) => {
    const root = w.document.documentElement;
    root.style.overflow = "auto";
    root.getBoundingClientRect = () => rect(0, -5000, 1024, w.innerHeight);
    Object.defineProperty(w.document, "scrollingElement", { value: root });
    Object.defineProperties(root, { clientWidth: { value: 1024 }, clientHeight: { value: w.innerHeight } });
    w.Range.prototype.getClientRects = function () {
      return [rect(0, this.startContainer.data.startsWith("Visible") ? 10 : -1000, 100, 20)];
    };
  });
  assert.deepEqual(Array.from(p.messages[0].visibleIds), [0]);
  p.w.close();
});

test("body自体が独立したスクロール枠の場合は、その枠の外の文字を優先しない", () => {
  const p = page('<p>Clipped above</p><p>Visible paragraph</p>', (w) => {
    w.document.documentElement.style.overflowX = "hidden";
    w.document.documentElement.style.overflowY = "hidden";
    const body = w.document.body;
    body.style.overflowX = "hidden"; body.style.overflowY = "auto";
    body.getBoundingClientRect = () => rect(100, 100, 300, 200);
    Object.defineProperties(body, { clientWidth: { value: 300 }, clientHeight: { value: 200 } });
    w.Range.prototype.getClientRects = function () {
      return [rect(110, this.startContainer.data.startsWith("Visible") ? 110 : 50, 100, 20)];
    };
  });
  assert.equal(p.blocks[0].text, "Visible paragraph");
  assert.deepEqual(Array.from(p.messages[0].visibleIds), [1]);
  p.w.close();
});

test("親要素が画面内でも、文字自体が画面外なら優先しない", () => {
  const p = page('<div id="large">Outside text<span>Visible text</span></div>', (w) => {
    w.document.querySelector("#large").getBoundingClientRect = () => rect(0, 0, 300, 3000);
    w.Range.prototype.getClientRects = function () {
      return [this.startContainer.data === "Outside text" ? rect(0, 2000, 100, 20) : rect(0, 10, 100, 20)];
    };
  });
  assert.equal(p.blocks[0].text, "Visible text");
  p.w.close();
});

test("領域内のスクロールで優先するIDを更新し、中止後は送らない", async () => {
  let firstVisible = true;
  const p = page('<div id="scroll"><p>First</p><p>Second</p></div>', (w) => {
    w.Range.prototype.getClientRects = function () {
      const visible = (this.startContainer.data === "First") === firstVisible;
      return [rect(0, visible ? 10 : 2000, 100, 20)];
    };
  });
  assert.deepEqual(Array.from(p.messages[0].visibleIds), [0]);
  firstVisible = false;
  p.w.document.querySelector("#scroll").dispatchEvent(new p.w.Event("scroll"));
  await new Promise((r) => p.w.requestAnimationFrame(r));
  assert.equal(p.messages.at(-1).type, "priority");
  assert.deepEqual(Array.from(p.messages.at(-1).ids), [1]);
  p.shadow.querySelector('[data-act="stop"]').click();
  const count = p.messages.length;
  p.w.dispatchEvent(new p.w.Event("resize"));
  await new Promise((r) => p.w.requestAnimationFrame(r));
  assert.equal(p.messages.length, count);
  p.w.close();
});

test("表示中のチャット本文・メイン領域を、先に並んだサイドバーより優先する", async () => {
  const p = page('<nav><span>Server menu</span><span>Channel menu</span></nav><div role="main"><h1>Channel header</h1><div role="article"><p>Visible message</p></div></div><aside>Member list</aside>', (w) => {
    w.Range.prototype.getClientRects = () => [rect(0, 100, 200, 20)];
  });
  assert.deepEqual(Array.from(p.blocks, (b) => b.text), ["Visible message", "Channel header", "Server menu", "Channel menu", "Member list"]);
  p.w.dispatchEvent(new p.w.Event("resize"));
  await new Promise((r) => p.w.requestAnimationFrame(r));
  assert.deepEqual(Array.from(p.messages.at(-1).ids), [3, 2, 0, 1, 4]);
  p.w.close();
});

test("画面外の本文より表示中のナビゲーションを先にし、通常のページも訳せる", () => {
  const p = page('<nav>Visible menu</nav><main><article>Outside article</article></main>', (w) => {
    w.Range.prototype.getClientRects = function () {
      return [rect(0, this.startContainer.data === "Visible menu" ? 100 : 2000, 200, 20)];
    };
  });
  assert.deepEqual(Array.from(p.blocks, (b) => b.text), ["Visible menu", "Outside article"]);
  p.w.close();
});

async function waitUntil(predicate) {
  for (let i = 0; i < 100; i++) {
    if (predicate()) return;
    await new Promise((r) => setTimeout(r, 10));
  }
  assert.fail("DOM の変更が翻訳へ反映されませんでした");
}

function complete(connection) {
  const blocks = connection.messages[0].blocks;
  for (const b of blocks) connection.receive({ type: "block", id: b.id, text: "訳:" + b.text, done: 1, total: blocks.length });
  connection.receive({ type: "done", total: blocks.length, omitted: 0, truncated: 0 });
}

test("翻訳中の新しい文章を追加し、自分が書いた訳は再送しない", async () => {
  const p = page('<main><p>Original message</p></main>');
  const first = p.connections[0];
  p.receive({ type: "partial", id: p.blocks[0].id, text: "途中の訳" });
  const added = p.w.document.createElement("p");
  added.textContent = "New message";
  p.w.document.querySelector("main").append(added);
  await waitUntil(() => p.messages.some((m) => m.type === "update"));
  const update = p.messages.find((m) => m.type === "update");
  assert.deepEqual(Array.from(update.blocks, (b) => b.text), ["Original message", "New message"]);
  assert.equal(p.connections.length, 1);
  for (const b of update.blocks) first.receive({ type: "block", id: b.id, text: "訳:" + b.text, done: 1, total: 2 });
  first.receive({ type: "done", total: 2, omitted: 0, truncated: 0 });
  await new Promise((r) => setTimeout(r, 200));
  assert.equal(p.connections.length, 1);
  p.w.close();
});

test("完了後のチャンネル変更でも新しい本文だけを訳す", async () => {
  const p = page('<nav>Menu</nav><main><p>Channel A</p></main>');
  complete(p.connections[0]);
  const main = p.w.document.querySelector("main");
  main.innerHTML = "<p>Channel B</p>";
  await waitUntil(() => p.connections.length === 2);
  assert.deepEqual(Array.from(p.connections[1].messages[0].blocks, (b) => b.text), ["Channel B"]);
  complete(p.connections[1]);
  p.shadow.querySelector('[data-act="restore"]').click();
  assert.equal(main.textContent, "Channel B");
  assert.equal(p.w.document.querySelector("nav").textContent, "Menu");
  p.w.close();
});

test("翻訳中に再利用された文字ノードへ古い訳を書かず、新しい原文を復元する", async () => {
  const p = page('<main><p>Channel A</p></main>');
  const old = p.connections[0];
  const node = p.w.document.querySelector("p").firstChild;
  p.receive({ type: "partial", id: p.blocks[0].id, text: "古い途中訳" });
  node.data = "Channel B";
  old.receive({ type: "partial", id: p.blocks[0].id, text: "遅れて届いた古い訳" });
  assert.equal(node.data, "Channel B");
  await waitUntil(() => p.connections.length === 2);
  assert.ok(old.disconnected);
  old.receive({ type: "block", id: p.blocks[0].id, text: "古い完了訳", done: 1, total: 1 });
  assert.equal(node.data, "Channel B");
  complete(p.connections[1]);
  p.shadow.querySelector('[data-act="restore"]').click();
  assert.equal(node.data, "Channel B");
  p.w.close();
});

test("中止後と原文へ戻した後は、新しい文章を自動で訳さない", async () => {
  for (const action of ["stop", "restore"]) {
    const p = page("<p>First message</p>");
    p.shadow.querySelector(`[data-act="${action}"]`).click();
    p.w.document.querySelector("p").textContent = "Second message";
    await new Promise((r) => setTimeout(r, 200));
    assert.equal(p.connections.length, 1);
    assert.equal(p.w.document.querySelector("p").textContent, "Second message");
    p.w.close();
  }
});

test("数字だけのページでも監視を続け、文章が来たら一度だけ接続する", async () => {
  const p = page("<p>12345</p>");
  assert.equal(p.connections.length, 0);
  p.w.document.querySelector("p").textContent = "New message";
  await waitUntil(() => p.connections.length === 1);
  complete(p.connections[0]);
  await new Promise((r) => setTimeout(r, 200));
  assert.equal(p.connections.length, 1);
  p.w.close();
});

test("片方が終わっても枠待ちの表示を保ち、再開で通常の表示に戻る", () => {
  const p = page("<p>One</p><p>Two</p>");
  p.receive({ type: "start", total: 2, targetName: "日本語" });
  p.receive({ type: "waiting", id: 0, waiting: true });
  p.receive({ type: "waiting", id: 1, waiting: true });
  p.receive({ type: "waiting", id: 1, waiting: false });
  p.receive({ type: "block", id: 1, text: "訳", done: 1, total: 2 });
  assert.match(p.shadow.querySelector(".msg").textContent, /前の翻訳の終了を待っています \(1 \/ 2\)/);
  p.receive({ type: "waiting", id: 0, waiting: false });
  assert.match(p.shadow.querySelector(".msg").textContent, /日本語に訳しています \(1 \/ 2\)/);
  p.w.close();
});

test("箱を持たない display:contents の見出しも収集・優先・復元する", () => {
  const p = page('<p>Outside</p><main><h2 style="display:contents;overflow:hidden"><span style="display:contents">A visible headline</span></h2></main><div style="display:none"><span style="display:contents">Hidden headline</span></div>', (w) => {
    const native = w.Element.prototype.checkVisibility;
    w.Element.prototype.checkVisibility = function (options) {
      if (w.getComputedStyle(this).display === "contents") return false;
      return native.call(this, options);
    };
    w.Range.prototype.getClientRects = function () {
      return [rect(0, this.startContainer.data === "Outside" ? 2000 : 100, 200, 20)];
    };
  });
  assert.deepEqual(Array.from(p.blocks, (b) => b.text), ["A visible headline", "Outside"]);
  complete(p.connections[0]);
  assert.equal(p.w.document.querySelector("h2").textContent, "訳:A visible headline");
  p.shadow.querySelector('[data-act="restore"]').click();
  assert.equal(p.w.document.body.innerHTML, p.original);
  p.w.close();
});

test("文字の追加がなくても、CSSの変更で表示された文章を収集する", async () => {
  const p = page('<p>First</p><h2 style="display:none">Later headline</h2>');
  complete(p.connections[0]);
  p.w.document.querySelector("h2").style.display = "contents";
  await waitUntil(() => p.connections.length === 2);
  assert.deepEqual(Array.from(p.connections[1].messages[0].blocks, (b) => b.text), ["Later headline"]);
  p.w.close();
});

test("open Shadow DOM 内の見出し・リンクを収集し、境界外のmainより優先する", () => {
  const p = page('<main><h1>Main title</h1><news-feed></news-feed><hidden-feed style="display:none"></hidden-feed><ignored-feed translate="no"></ignored-feed></main>', (w) => {
    const root = w.document.querySelector("news-feed").attachShadow({ mode: "open" });
    root.innerHTML = '<article><p><a href="#story">A shadow headline</a></p><nested-feed></nested-feed></article>';
    root.querySelector("nested-feed").attachShadow({ mode: "open" }).append("Nested headline");
    w.document.querySelector("hidden-feed").attachShadow({ mode: "open" }).innerHTML = '<p>Hidden headline</p>';
    w.document.querySelector("ignored-feed").attachShadow({ mode: "open" }).innerHTML = '<p>Ignored headline</p>';
    w.Range.prototype.getClientRects = () => [rect(0, 100, 200, 20)];
  });
  assert.deepEqual(Array.from(p.blocks, (b) => b.text), ["A shadow headline", "Nested headline", "Main title"]);
  const link = p.w.document.querySelector("news-feed").shadowRoot.querySelector("a");
  complete(p.connections[0]);
  assert.equal(link.textContent, "訳:A shadow headline");
  assert.equal(link.getAttribute("href"), "#story");
  p.shadow.querySelector('[data-act="restore"]').click();
  assert.equal(link.textContent, "A shadow headline");
  assert.equal(p.w.document.querySelector("news-feed").shadowRoot.querySelector("nested-feed").shadowRoot.textContent, "Nested headline");
  p.w.close();
});

test("Shadow DOM 内の後から追加された文章を監視する", async () => {
  const p = page("<news-feed></news-feed>", (w) => {
    w.document.querySelector("news-feed").attachShadow({ mode: "open" }).innerHTML = '<p>First headline</p>';
  });
  complete(p.connections[0]);
  p.w.document.querySelector("news-feed").shadowRoot.innerHTML = '<p>Next headline</p>';
  await waitUntil(() => p.connections.length === 2);
  assert.deepEqual(Array.from(p.connections[1].messages[0].blocks, (b) => b.text), ["Next headline"]);
  p.w.close();
});

test("後からattachShadowされた既存要素も見つける", async () => {
  const p = page("<p>First</p><news-feed></news-feed>");
  complete(p.connections[0]);
  p.w.document.querySelector("news-feed").attachShadow({ mode: "open" }).innerHTML = '<p>Late shadow headline</p>';
  // 境界そのものの作成にはMutationObserver通知がないため、定期検出を待つ。
  for (let i = 0; i < 200 && p.connections.length < 2; i++) await new Promise((r) => setTimeout(r, 10));
  assert.equal(p.connections.length, 2);
  assert.deepEqual(Array.from(p.connections[1].messages[0].blocks, (b) => b.text), ["Late shadow headline"]);
  p.w.close();
});
