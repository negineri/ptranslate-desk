// 1 回に送る区切りの目安 (文字)。PLaMo は長い原文で繰り返しに入るので小さめにする
const CHUNK_CHARS = 1500;

// 日本語の文字 (かな・漢字) が多ければ英語へ、そうでなければ日本語へ
export function guessTarget(text) {
  const sample = text.slice(0, 2000);
  const ja = (sample.match(/[\u3040-\u30ff\u3400-\u9fff]/g) || []).length;
  const letters = (sample.match(/[\p{L}]/gu) || []).length || 1;
  return ja / letters > 0.2 ? "en" : "ja";
}

export function parseGlossary(text, maxGlossary = 200) {
  return text.split("\n").map((line) => {
    const m = line.split(/\s*(?:=>|⇒|→)\s*/);
    return m.length === 2 && m[0].trim() && m[1].trim() ? { source: m[0].trim(), target: m[1].trim() } : null;
  }).filter(Boolean).slice(0, maxGlossary);
}

// 段落 (空行) で区切り、CHUNK_CHARS までまとめる。長い段落は文の終わりで分ける。
// 区切りの間の空白はそのまま訳に戻すので、段落の形が保たれる
export function splitText(text) {
  const pieces = [];
  const re = /(\n\s*\n)/;
  for (const part of text.split(re)) {
    if (re.test(part) || part === "") { pieces.push({ sep: part }); continue; }
    if (part.length <= CHUNK_CHARS) { pieces.push({ text: part }); continue; }
    let buf = "";
    for (const s of part.match(/[^。．.!?！？\n]+[。．.!?！？\n]*|[。．.!?！？\n]+/g) || [part]) {
      if (buf && buf.length + s.length > CHUNK_CHARS) { pieces.push({ text: buf }); buf = ""; }
      buf += s;
      while (buf.length > CHUNK_CHARS) { pieces.push({ text: buf.slice(0, CHUNK_CHARS) }); buf = buf.slice(CHUNK_CHARS); }
    }
    if (buf) pieces.push({ text: buf });
  }
  // 短い段落は区切りごとまとめて 1 回で送る
  const chunks = [];
  let cur = null;
  for (const p of pieces) {
    if (p.sep !== undefined) { if (cur) cur.tail += p.sep; else chunks.push({ text: "", tail: p.sep }); continue; }
    if (cur && cur.text.length + cur.tail.length + p.text.length <= CHUNK_CHARS) {
      cur.text += cur.tail + p.text; cur.tail = "";
    } else {
      cur = { text: p.text, tail: "" }; chunks.push(cur);
    }
  }
  return chunks;
}

export async function translateChunk(res, onText) {
  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buf = "", finish = null;
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    buf += decoder.decode(value, { stream: true });
    let i;
    while ((i = buf.indexOf("\n\n")) >= 0) {
      const ev = buf.slice(0, i); buf = buf.slice(i + 2);
      if (!ev.startsWith("data:")) continue;
      const data = JSON.parse(ev.slice(5));
      if (data.t) onText(data.t);
      if (data.error) throw new Error(data.error);
      if (data.done) finish = data.finish;
    }
  }
  if (!finish) throw new Error("接続が途中で切れました");
  return finish;
}
