// ページ全体の翻訳 (background 側)。先に node chrome-extension/build.mjs で dist/ を作ってから実行する
import assert from "node:assert/strict";
import { test } from "node:test";
import { concurrencyLimit, runPageTask, shouldTranslate } from "../dist/page-task.mjs";
import { portPair, SERVER, sse } from "./helpers.mjs";

const CONFIG = { concurrency: { max_in_flight: 2, scope: "user" }, user: "test", max_glossary: 200, languages: [{ code: "ja", name: "日本語" }, { code: "en", name: "英語" }] };
const SETTINGS = { server: SERVER, source: "", target: "auto", form: "", glossary: "" };

// 訳を "[訳す先] 原文" で返すサーバー。受けた要求を requests に残す
function fakeServer({ config = () => Response.json(CONFIG), translate } = {}) {
  const requests = [];
  const fetch = async (url, init) => {
    if (url.endsWith("/api/config")) return config();
    const body = JSON.parse(init.body);
    requests.push({ body, signal: init.signal });
    if (translate) return translate(body, init);
    return sse([{ t: `[${body.target_lang}] ` }, { t: body.text }, { done: true, finish: "stop" }]);
  };
  return { fetch, requests };
}

// ページの側の port を作って blocks を送り、done か error まで受けたメッセージを返す
function runPage(blocks, server, deps = {}, visibleIds) {
  const [page, background] = portPair();
  runPageTask(background, {
    fetch: server.fetch, server: async () => SERVER, hasPermission: async () => true, settings: async () => SETTINGS,
    ...deps,
  });
  const messages = [];
  const finished = new Promise((resolve) => {
    page.onMessage.addListener((msg) => {
      messages.push(msg);
      if (msg.type === "done" || msg.type === "error") resolve(messages);
    });
  });
  page.postMessage({ blocks, visibleIds });
  return { page, messages, finished };
}

test("文字のある文章は言語を推測して省かず、数字や記号だけなら送らない", () => {
  for (const text of ["Hello world", "これは日本語の文です", "日本語", "该域名仅用于文档示例，无需获得许可。", "此網域僅用於文件範例。", "Hola mundo", "Bonjour", "Déjà vu"]) {
    assert.ok(shouldTranslate(text), text);
  }
  for (const text of ["", "2026-11-08 12:00", "123 + 456", "🎉！"]) {
    assert.ok(!shouldTranslate(text), text);
  }
});

test("中国語を日本語へ、ASCII のスペイン語を英語へ送る", async () => {
  for (const [target, text] of [["ja", "该域名仅用于文档示例，无需获得许可。"], ["ja", "此網域僅用於文件範例。"], ["en", "Hola mundo"]]) {
    const server = fakeServer();
    const messages = await runPage([{ id: 0, text }], server, {
      settings: async () => ({ ...SETTINGS, target }),
    }).finished;
    assert.equal(server.requests.length, 1);
    assert.equal(server.requests[0].body.text, text);
    assert.equal(server.requests[0].body.target_lang, target);
    assert.equal(messages.at(-1).total, 1);
  }
});

test("ページ内で同じグループを使い、画面内外を区別する", async () => {
  const blocks = [{ id: 1, text: "Outside" }, { id: 2, text: "Inside" }];
  const first = fakeServer();
  const second = fakeServer();
  for (const server of [first, second]) {
    await runPage(blocks, server, {}, [2]).finished;
    const [visible, rest] = server.requests.map(({ body }) => body);
    assert.equal(visible.purpose, "page_visible");
    assert.equal(rest.purpose, "page_rest");
    assert.match(visible.group, /^[A-Za-z0-9_-]{22}$/);
    assert.equal(rest.group, visible.group);
  }
  assert.notEqual(first.requests[0].body.group, second.requests[0].body.group);
});

test("別の段落を並列に訳して結果をそれぞれ返す", async () => {
  const server = fakeServer();
  const blocks = [
    { id: 0, text: "First paragraph of an English page about translation." },
    { id: 1, text: "12345" },
    { id: 2, text: "Second paragraph." },
  ];
  const { finished } = runPage(blocks, server);
  const messages = await finished;
  const start = messages.find((m) => m.type === "start");
  assert.deepEqual(start, { type: "start", total: 2, target: "ja", targetName: "日本語" });
  const done = messages.filter((m) => m.type === "block");
  assert.deepEqual(done.map((m) => [m.id, m.text, m.done]), [
    [0, "[ja] First paragraph of an English page about translation.", 1],
    [2, "[ja] Second paragraph.", 2],
  ]);
  assert.ok(messages.some((m) => m.type === "partial" && m.id === 0));
  assert.deepEqual(messages.at(-1), { type: "done", total: 2, truncated: 0, omitted: 0 });
  assert.deepEqual(server.requests.map((r) => r.body.history), [[], []]);
  assert.equal(server.requests[0].body.target_lang, "ja");
});

test("訳す先の自動はページ全体の文で決める", async () => {
  const server = fakeServer();
  const { finished } = runPage([
    { id: 0, text: "これは日本語のページです。説明が続きます。" },
    { id: 1, text: "English menu" },
  ], server);
  const messages = await finished;
  assert.equal(messages.find((m) => m.type === "start").target, "en");
  assert.deepEqual(messages.filter((m) => m.type === "block").map((m) => m.id), [0, 1]);
});

test("長い段落は区切って訳し、つなげて返す", async () => {
  const server = fakeServer();
  const long = ("A sentence. ").repeat(200).trim();
  const messages = await runPage([{ id: 0, text: long }], server).finished;
  assert.ok(server.requests.length >= 2);
  assert.ok(server.requests.every((r) => r.body.text.length <= 1500));
  const block = messages.find((m) => m.type === "block");
  assert.equal(block.text.split("[ja]").length - 1, server.requests.length);
  for (let i = 1; i < server.requests.length; i++) {
    const previous = server.requests[i - 1].body;
    assert.deepEqual(server.requests[i].body.history, [{ source: previous.text, target: "[ja] " + previous.text }]);
  }
});

test("ログインしていないときは login を返す", async () => {
  const server = fakeServer({ config: () => new Response("<html>", { status: 403 }) });
  const messages = await runPage([{ id: 0, text: "Hello" }], server).finished;
  assert.equal(messages.at(-1).type, "error");
  assert.equal(messages.at(-1).code, "login");
});

test("接続先が未設定なら設定を促す", async () => {
  const server = fakeServer();
  const messages = await runPage([{ id: 0, text: "Hello" }], server, { settings: async () => ({ ...SETTINGS, server: "" }) }).finished;
  assert.deepEqual(messages.at(-1), {
    type: "error", code: "no-server", remedy: "options",
    message: "接続先が未設定です。設定でサーバーURLを入れ、「接続」を押してください",
  });
});

test("ページが port を閉じたら訳している要求も止める", async () => {
  const server = fakeServer({
    translate: (body, init) => new Response(new ReadableStream({
      start(c) {
        c.enqueue(new TextEncoder().encode('data: {"t":"a"}\n\n'));
        init.signal.addEventListener("abort", () => c.error(init.signal.reason));
      },
    })),
  });
  const { page, messages } = runPage([{ id: 0, text: "One" }, { id: 1, text: "Two" }], server);
  while (!messages.some((m) => m.type === "partial")) await new Promise((r) => setTimeout(r, 5));
  page.disconnect();
  await new Promise((r) => setTimeout(r, 30));
  assert.equal(server.requests.length, 2);
  assert.ok(server.requests.every((r) => r.signal.aborted));
});

test("上限を超えた翻訳対象の数を通知し、対象外の段落で枠を使わない", async () => {
  const server = fakeServer();
  const blocks = [
    ...Array.from({ length: 3000 }, (_,id) => ({ id, text: "12345" })),
    ...Array.from({ length: 3001 }, (_,id) => ({ id: id + 3000, text: "Hello" })),
  ];
  const messages = await runPage(blocks, server, { settings: async () => ({ ...SETTINGS, target: "ja" }) }).finished;
  assert.equal(server.requests.length, 3000);
  assert.equal(messages.find((m) => m.type === "start").total, 3000);
  assert.deepEqual(messages.at(-1), { type: "done", total: 3000, truncated: 0, omitted: 1 });
});


async function waitUntil(predicate) {
  for (let i = 0; i < 1000; i++) {
    if (predicate()) return;
    await new Promise((r) => setTimeout(r, 1));
  }
  assert.fail("翻訳要求が進みませんでした");
}

// 最初の2本を止め、更新が送られた後に1枠ずつ空ける。
function gatedServer() {
  const release = new Map();
  const server = fakeServer({ translate: async (body) => {
    if (body.text === "First" || body.text === "Second") {
      await new Promise((r) => release.set(body.text, r));
    }
    return sse([{ t: body.text }, { done: true, finish: "stop" }]);
  } });
  return { server, release };
}

for (const ids of [[3], [3, 2]]) {
  test(`並列中の優先順の更新を空いた枠に反映する (${ids})`, async () => {
    const { server, release } = gatedServer();
    const { page, finished } = runPage([
      { id: 0, text: "First" }, { id: 1, text: "Second" },
      { id: 2, text: "Offscreen" }, { id: 3, text: "Now visible" },
    ], server);
    await waitUntil(() => release.size === 2);
    page.postMessage({ type: "priority", ids });
    await new Promise((r) => setTimeout(r, 0));
    release.get("Second")();
    await waitUntil(() => server.requests.length === 4);
    release.get("First")();
    const messages = await finished;
    assert.deepEqual(server.requests.map((r) => r.body.text), ["First", "Second", "Now visible", "Offscreen"]);
    assert.deepEqual(server.requests[2].body.history, [{ source: "Second", target: "Second" }]);
    assert.equal(messages.at(-1).total, 4);
  });
}

test("並列中の追加を反映し、実行中と完了済みを重複して訳さない", async () => {
  const { server, release } = gatedServer();
  const blocks = [{ id: 0, text: "First" }, { id: 1, text: "Second" }, { id: 2, text: "Old pending" }];
  const { page, finished } = runPage(blocks, server);
  await waitUntil(() => release.size === 2);
  page.postMessage({ type: "update", blocks: [...blocks, { id: 3, text: "New visible" }], ids: [3, 2] });
  await new Promise((r) => setTimeout(r, 0));
  release.get("Second")();
  await waitUntil(() => server.requests.length === 4);
  release.get("First")();
  const messages = await finished;
  assert.deepEqual(server.requests.map((r) => r.body.text), ["First", "Second", "New visible", "Old pending"]);
  assert.deepEqual(messages.at(-1), { type: "done", total: 4, omitted: 0, truncated: 0 });
});

test("2本まで同時に実行し、1本が終わると次を送る", async () => {
  const release = new Map();
  let active = 0;
  let maxActive = 0;
  const server = fakeServer({ translate: async (body) => {
    active++;
    maxActive = Math.max(maxActive, active);
    await new Promise((r) => release.set(body.text, r));
    active--;
    return sse([{ t: body.text }, { done: true, finish: "stop" }]);
  } });
  const { finished } = runPage(["One", "Two", "Three", "Four"].map((text, id) => ({ id, text })), server);
  await waitUntil(() => release.size === 2);
  assert.equal(server.requests.length, 2);
  release.get("Two")();
  await waitUntil(() => release.has("Three"));
  assert.equal(server.requests.length, 3);
  release.get("Three")();
  await waitUntil(() => release.has("Four"));
  release.get("One")();
  release.get("Four")();
  const messages = await finished;
  assert.equal(maxActive, 2);
  const blocks = messages.filter((m) => m.type === "block");
  assert.deepEqual(blocks.map((m) => m.done), [1, 2, 3, 4]);
  assert.equal(new Set(blocks.map((m) => m.id)).size, 4);
});

test("並列の一方が失敗したら他方も中断し、完了扱いにしない", async () => {
  let fail;
  const server = fakeServer({ translate: async (body, init) => {
    if (body.text === "One") {
      await new Promise((r) => { fail = r; });
      return new Response("failed", { status: 500 });
    }
    return new Response(new ReadableStream({ start(c) {
      c.enqueue(new TextEncoder().encode('data: {"t":"partial"}\n\n'));
      init.signal.addEventListener("abort", () => c.error(init.signal.reason));
    } }));
  } });
  const { finished, messages } = runPage([{ id: 0, text: "One" }, { id: 1, text: "Two" }], server);
  await waitUntil(() => server.requests.length === 2 && fail);
  fail();
  await finished;
  assert.ok(server.requests.every((r) => r.signal.aborted));
  assert.equal(messages.filter((m) => m.type === "error").length, 1);
  assert.ok(!messages.some((m) => m.type === "done"));
});

test("429 では枠が空くまで待ち、同じ原文で再試行して翻訳を完了する", async () => {
  let attempts = 0;
  const server = fakeServer({ translate: (body) => ++attempts <= 2
    ? Response.json({ error: "busy" }, { status: 429 })
    : sse([{ t: "translated" }, { done: true, finish: "stop" }]) });
  const { messages, finished } = runPage([{ id: 0, text: "New channel" }], server, { busyRetryMs: 1 });
  await finished;
  assert.equal(server.requests.length, 3);
  assert.ok(server.requests.every((r) => r.body.text === "New channel"));
  assert.ok(messages.some((m) => m.type === "waiting" && m.waiting));
  assert.ok(messages.some((m) => m.type === "waiting" && !m.waiting));
  assert.equal(messages.at(-1).type, "done");
  assert.equal(messages.filter((m) => m.type === "block").length, 1);
});

test("同時実行枠を待っている間の中止で、再試行も止まる", async () => {
  const server = fakeServer({ translate: () => Response.json({ error: "busy" }, { status: 429 }) });
  const { page, messages } = runPage([{ id: 0, text: "One" }, { id: 1, text: "Two" }], server, { busyRetryMs: 100 });
  await waitUntil(() => messages.filter((m) => m.type === "waiting" && m.waiting).length === 2);
  page.disconnect();
  const count = server.requests.length;
  await new Promise((r) => setTimeout(r, 150));
  assert.equal(server.requests.length, count);
  assert.ok(server.requests.every((r) => r.signal.aborted));
  assert.ok(!messages.some((m) => m.type === "error" || m.type === "done"));
});

test("429 が続いても無限に待たず、待機時間を超えたら案内する", async () => {
  const server = fakeServer({ translate: () => Response.json({ error: "busy" }, { status: 429 }) });
  const { finished } = runPage([{ id: 0, text: "One" }], server, { busyRetryMs: 1, busyTimeoutMs: 10 });
  const messages = await finished;
  assert.equal(messages.at(-1).type, "error");
  assert.equal(messages.at(-1).code, "busy");
  assert.ok(server.requests.length > 1);
  assert.ok(!messages.some((m) => m.type === "done"));
});

test("認証や要求エラーは枠待ちとして再試行しない", async () => {
  for (const status of [400, 401, 403, 500]) {
    const server = fakeServer({ translate: () => Response.json({ error: "failed" }, { status }) });
    const messages = await runPage([{ id: 0, text: "One" }], server).finished;
    assert.equal(server.requests.length, 1);
    assert.equal(messages.at(-1).type, "error");
    assert.ok(!messages.some((m) => m.type === "waiting"));
  }
});

for (const advertised of [undefined, 1, 4, 12]) {
  test(`接続先の通知に応じてページ翻訳を実行する (${advertised ?? "通知なし"})`, async () => {
    let active = 0, maximum = 0;
    const config = { ...CONFIG };
    if (advertised === undefined) delete config.concurrency;
    else config.concurrency = { max_in_flight: advertised };
    const server = fakeServer({
      config: () => Response.json(config),
      translate: async (body) => {
        active++; maximum = Math.max(maximum, active);
        await new Promise((resolve) => setTimeout(resolve, 10));
        active--;
        return sse([{ t: body.text }, { done: true, finish: "stop" }]);
      },
    });
    const messages = await runPage(Array.from({ length: 16 }, (_, id) => ({ id, text: `Paragraph ${id}` })), server).finished;
    assert.equal(messages.at(-1).type, "done");
    assert.equal(maximum, advertised ?? 1);
    assert.equal(server.requests.length, 16);
  });
}

for (const mode of ["local", "server"]) {
  test(`設定変更後もページの再試行・後続段落・更新を元の接続先へ送る (${mode})`, { timeout: 5000 }, async () => {
    const settings = { ...SETTINGS, mode };
    const calls = [];
    let translations = 0;
    let page;
    const respond = async (route) => {
      calls.push(route);
      if (route.endsWith("api/config")) {
        settings.mode = "server";
        settings.server = "https://other.example.com";
        return Response.json({ ...CONFIG, concurrency: { max_in_flight: 1 } });
      }
      translations++;
      if (translations === 1) return Response.json({ error: "busy" }, { status: 429 });
      if (translations === 2) {
        page.postMessage({ type: "update", blocks: [{ id: 2, text: "Updated paragraph" }] });
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      return sse([{ t: "訳" }, { done: true, finish: "stop" }]);
    };
    const task = runPage([{ id: 0, text: "First paragraph" }, { id: 1, text: "Second paragraph" }], {
      fetch: async (url) => respond(url),
    }, {
      settings: async () => settings,
      server: async () => settings.server,
      nativeFetch: async (route) => { const res = await respond(`local/${route}`); return { status: res.status, body: res.body }; },
      busyRetryMs: 1,
    });
    page = task.page;
    const messages = await task.finished;
    assert.equal(messages.at(-1).type, "done");
    assert.ok(translations >= 3);
    const prefix = mode === "local" ? "local/" : `${SERVER}/`;
    assert.ok(calls.every((url) => url.startsWith(prefix)), JSON.stringify(calls));
    assert.ok(messages.some((msg) => msg.type === "block" && msg.id === 2));
  });
}

test("ページ途中の表示IDを初回要求から優先し、画面外の200段落を先に送らない", async () => {
  const server = fakeServer({
    config: () => Response.json({ ...CONFIG, concurrency: { max_in_flight: 1 } }),
    translate: ({ text }) => sse([{ t: text }, { done: true, finish: "stop" }]),
  });
  const blocks = Array.from({ length: 250 }, (_, id) => ({ id, text: `Paragraph ${id}` }));
  const messages = await runPage(blocks, server, {}, [200, 201]).finished;
  assert.deepEqual(server.requests.slice(0, 3).map((request) => request.body.text), ["Paragraph 200", "Paragraph 201", "1. Paragraph 0\n2. Paragraph 1\n3. Paragraph 2\n4. Paragraph 3"]);
  assert.equal(messages.at(-1).type, "done");
  assert.equal(server.requests.length, 64);
});

test("長文を画面内・画面外の各範囲で優先し、画面外の短文を4件まとめる", async () => {
  const visibleLong = "A visible paragraph about the collection. ".repeat(9).trim();
  const hiddenLong = "A paragraph farther down about the archive. ".repeat(9).trim();
  const labels = ["Site Rules", "Recent Changes", "Research Notes", "Request Access"];
  const server = fakeServer({
    config: () => Response.json({ ...CONFIG, concurrency: { max_in_flight: 1 } }),
    translate: ({ text }) => sse([
      { t: text.startsWith("1. ") ? "1. 規則\n2. 変更\n3. ノート\n4. 申請" : "本文の訳" },
      { done: true, finish: "stop" },
    ]),
  });
  const blocks = [
    { id: 0, text: "Visible label" },
    { id: 1, text: visibleLong },
    ...labels.map((text, index) => ({ id: index + 2, text })),
    { id: 6, text: hiddenLong },
  ];
  const messages = await runPage(blocks, server, {}, [0, 1]).finished;
  assert.deepEqual(server.requests.map(({ body }) => body.text), [
    visibleLong, "Visible label", hiddenLong,
    labels.map((label, index) => `${index + 1}. ${label}`).join("\n"),
  ]);
  assert.deepEqual(messages.filter((m) => m.type === "block").slice(-4).map(({ id, text }) => [id, text]),
    [[2, "規則"], [3, "変更"], [4, "ノート"], [5, "申請"]]);
  assert.equal(messages.at(-1).total, 7);
});

test("一括翻訳で番号が重複した場合はすべて個別に訳し直す", async () => {
  const labels = ["Site Rules", "Recent Changes", "Research Notes", "Request Access"];
  const blocks = labels.map((text, id) => ({ id, text }));
  const server = fakeServer({
    translate: ({ text }) => sse([
      { t: text.startsWith("1. ") ? "1. 規則\n2. 変更\n2. ノート\n4. 申請" : `訳 ${text}` },
      { done: true, finish: "stop" },
    ]),
  });
  const messages = await runPage(blocks, server, {}, []).finished;
  assert.equal(server.requests.length, 5);
  assert.deepEqual(server.requests.slice(1).map(({ body }) => body.text), labels);
  assert.deepEqual(messages.filter((m) => m.type === "block").map(({ text }) => text),
    labels.map((label) => `訳 ${label}`));
});

test("ボランティア枠には接続先の現行版に同意した通常ウィンドウからだけ送る", async (t) => {
  const previous = globalThis.chrome;
  t.after(() => { globalThis.chrome = previous; });
  const blocks = [{ id: 1, text: "A paragraph" }];
  for (const [stored, incognito, expected] of [
    [{}, false, "consent_required"],
    [{ [`volunteer-consent:${SERVER}`]: 1 }, false, "consent_required"],
    [{ "volunteer-consent:https://other.example": 2 }, false, "consent_required"],
    [{ [`volunteer-consent:${SERVER}`]: 2 }, true, "server"],
    [{ [`volunteer-consent:${SERVER}`]: 2 }, false, null],
  ]) {
    globalThis.chrome = { storage: { local: { get: async () => stored } } };
    const server = fakeServer({ config: () => Response.json({
      ...CONFIG, pool: { kind: "volunteer", consent_version: 2 },
    }) });
    const messages = await runPage(blocks, server, { incognito }, [1]).finished;
    if (expected) {
      assert.equal(server.requests.length, 0);
      assert.equal(messages.at(-1).code, expected);
    } else {
      assert.equal(server.requests[0].body.consent, 2);
      assert.equal(messages[0].volunteer, true);
      assert.equal(messages.at(-1).type, "done");
    }
  }
});
