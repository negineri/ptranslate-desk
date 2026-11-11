// 拡張の設定。原文と訳はここに保存しない
import { guessTarget, parseGlossary } from "./translation.mjs";

export const DEFAULTS = Object.freeze({ server: "", mode: "server", source: "", target: "ja", form: "", glossary: "" });
const KEYS = Object.keys(DEFAULTS);
const LOOPBACK = new Set(["127.0.0.1", "localhost", "[::1]"]);

// 入力されたサーバーURLを origin にそろえる。HTTPS だけ (開発用にループバックの HTTP は通す)
export function normalizeServerUrl(input) {
  let url;
  try {
    url = new URL(String(input).trim());
  } catch {
    throw new Error("サーバーURLの形が正しくありません (例: https://translate.example.com)");
  }
  const loopback = url.protocol === "http:" && LOOPBACK.has(url.hostname);
  if (url.protocol !== "https:" && !loopback) throw new Error("サーバーURLは https:// で始めてください");
  if (url.username || url.password) throw new Error("サーバーURLにユーザー名やパスワードは入れないでください");
  if ((url.pathname !== "/" && url.pathname !== "") || url.search || url.hash) {
    throw new Error("サーバーURLにはパスを付けないでください (例: https://translate.example.com)");
  }
  return url.origin;
}

// 権限の要求に使う形 (chrome.permissions の origins)
export function originPattern(server) {
  const url = new URL(server);
  return `${url.protocol}//${url.hostname}/*`;
}

export function sanitize(stored) {
  const out = { ...DEFAULTS };
  for (const key of KEYS) if (typeof stored?.[key] === "string") out[key] = stored[key];
  if (!["server", "local"].includes(out.mode)) out.mode = "server";
  return out;
}

export async function loadSettings() {
  return sanitize(await chrome.storage.local.get(KEYS));
}

export async function saveSettings(values) {
  const clean = sanitize({ ...(await loadSettings()), ...values });
  await chrome.storage.local.set(clean);
  return clean;
}

// /api/translate に毎回送る設定 (原文と history 以外)
export function requestSettings(text, settings, config = {}) {
  const target = !settings.target || settings.target === "auto" ? guessTarget(text) : settings.target;
  return {
    target_lang: target,
    source_lang: settings.source || null,
    form: target === "ja" ? settings.form || null : null,
    context: "",
    glossary: parseGlossary(settings.glossary || "", config.max_glossary || 200),
  };
}

// Native Messagingの初回登録は専用URLでデスクトップへ引き継ぐ。
export function chromeRegistrationUrl(id) {
  if (!/^[a-p]{32}$/.test(id)) throw new Error("拡張IDが正しくありません");
  return `ptranslate://register-chrome?extension_id=${id}`;
}

// 接続済みの判定は翻訳先ごと。ブラウザーを閉じると消えるsessionにだけ保持する。
export function connectionKey(settings) {
  return settings.mode === "local" ? "local" : `server:${settings.server}`;
}

export function consentKey(server) {
  return `volunteer-consent:${normalizeServerUrl(server)}`;
}

export async function volunteerFields(settings, config, blocked = "") {
  if (settings.mode === "local" || config.pool?.kind !== "volunteer") return {};
  const version = config.pool.consent_version;
  if (blocked) throw new Error(blocked);
  if (Number.isSafeInteger(version) && version > 0) {
    const key = consentKey(settings.server);
    if ((await chrome.storage.local.get(key))[key] === version) return { consent: version };
  }
  throw Object.assign(new Error("設定でボランティア枠への送信に同意してください"), { code: "consent_required" });
}
