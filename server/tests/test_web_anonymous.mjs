import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { test } from "node:test";
import * as translation from "../static/translation.mjs";

const require = createRequire(new URL("../../chrome-extension/package.json", import.meta.url));
const { JSDOM } = require("jsdom");
const html = await readFile(new URL("../static/index.html", import.meta.url), "utf8");

async function waitFor(check) {
  for (let i = 0; i < 200; i++) {
    if (check()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  assert.fail("匿名利用の画面が期待した状態になりませんでした");
}

test("Web は明示操作で匿名資格を取得し、同意後に翻訳する。401 では原文を再送せず資格を更新する", async (t) => {
  const { window } = new JSDOM(html.replace('data-web-anonymous="disabled"', 'data-web-anonymous="enabled"'), {
    url: "https://translate.example", runScripts: "outside-only",
  });
  t.after(() => window.close());
  const el = (id) => window.document.getElementById(id);
  const dialog = el("volunteer-consent");
  dialog.showModal = () => { dialog.open = true; };
  dialog.addEventListener("click", (event) => { event.preventDefault(); dialog.open = false; });
  let issued = 0;
  let expired = true;
  const sent = [];
  Object.assign(window, translation, {
    fetch: async (route, options) => {
      assert.equal(options?.headers?.Authorization, undefined);
      if (route === "api/anonymous") {
        assert.equal(options.method, "POST");
        assert.equal(options.credentials, "same-origin");
        assert.equal(options.body, "{}");
        issued++;
        expired = false;
        return Response.json({ expires_in: 2592000 });
      }
      if (route === "api/config") {
        return expired ? new Response(null, { status: 401 }) : Response.json({
          tier: "anonymous", languages: [], pool: { kind: "volunteer", consent_version: 1 },
        });
      }
      assert.equal(route, "api/translate");
      sent.push(JSON.parse(options.body));
      if (expired) return new Response(null, { status: 401 });
      return new Response('data: {"t":"訳"}\n\ndata: {"done":true,"finish":"stop"}\n\n');
    },
  });
  const script = html.match(/<script type="module">([\s\S]*?)<\/script>/)[1];
  window.eval(script.replace(/^import .*$/gm, ""));
  await waitFor(() => !el("anonymous-access").hidden);
  assert.equal(issued, 0);
  el("anonymous-start").click();
  await waitFor(() => el("who").textContent.includes("匿名で利用中"));
  assert.equal(issued, 1);
  el("src").value = "Original text";
  el("go").click();
  assert.equal(dialog.open, true);
  assert.equal(sent.length, 0);
  el("volunteer-accept").click();
  el("go").click();
  await waitFor(() => el("status").textContent === "訳し終わりました");
  assert.equal(sent[0].consent, 1);

  expired = true;
  el("src").value = "Another text";
  el("go").click();
  await waitFor(() => el("status").textContent.includes("資格を更新しました"));
  assert.equal(issued, 2);
  assert.equal(sent.length, 2);
  assert.ok(el("anonymous-access").hidden);
});
