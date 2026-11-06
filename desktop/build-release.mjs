// 明示的な配布ビルドだけが、HEADのgitタグをバージョンとして使う。
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { run } from "@tauri-apps/cli";

process.chdir(fileURLToPath(new URL(".", import.meta.url)));
const args = process.argv.slice(2);
if (!args.includes("--help") && !args.includes("-h")) {
  const result = spawnSync("python3", ["../tools/check_release_version.py", "--write-config", "src-tauri/gen/release-version.json"], { stdio: "inherit" });
  if (result.error) console.error(result.error.message);
  if (result.status !== 0) process.exit(result.status ?? 1);
  args.push("--config", "src-tauri/gen/release-version.json");
}
try { await run(["build", ...args]); }
catch (error) { console.error(error.message); process.exitCode = 1; }
