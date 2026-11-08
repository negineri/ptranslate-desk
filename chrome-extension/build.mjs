// Chrome 拡張を dist/ に組み立てる。Web の翻訳モジュールと画面の見た目をそのまま使う
// 使い方: node chrome-extension/build.mjs → chrome://extensions で chrome-extension/dist を読み込む
import { copyFile, mkdir, readFile, rm, writeFile } from "node:fs/promises";

const here = new URL("./", import.meta.url);
const web = new URL("../server/static/", import.meta.url);
const logo = new URL("../docs/logo/png/", import.meta.url);
const dist = new URL("./dist/", import.meta.url);

const html = await readFile(new URL("index.html", web), "utf8");
const style = html.match(/<style>[\s\S]*?<\/style>/)?.[0];
if (!style) throw new Error("Web 画面の見た目 (<style>) を見つけられません");

await rm(dist, { recursive: true, force: true });
await mkdir(new URL("icons/", dist), { recursive: true });
const pages = ["popup.html", "options.html"];
const scripts = ["background.mjs", "overlay.mjs", "relay.mjs", "native.mjs", "scheduler.mjs", "transport.mjs", "settings.mjs", "translate.mjs",
  "popup.mjs", "options.mjs", "page-task.mjs", "page-content.js"];
await Promise.all([
  copyFile(new URL("manifest.json", here), new URL("manifest.json", dist)),
  ...pages.map(async (name) => {
    const page = await readFile(new URL(name, here), "utf8");
    if (!page.includes("<!-- shared-style -->")) throw new Error(`${name} に <!-- shared-style --> がありません`);
    await writeFile(new URL(name, dist), page.replace("<!-- shared-style -->", style));
  }),
  ...scripts.map((name) => copyFile(new URL(name, here), new URL(name, dist))),
  copyFile(new URL("translation.mjs", web), new URL("translation.mjs", dist)),
  copyFile(new URL("logo.png", web), new URL("logo.png", dist)),
  ...[16, 32, 48, 128].map((size) => copyFile(new URL(`icon-${size}.png`, logo), new URL(`icons/icon-${size}.png`, dist))),
]);
console.log("chrome-extension/dist を作りました");
