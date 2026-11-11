use serde_json::{json, Value};

fn field(body: &Value, name: &str, limit: usize, required: bool) -> Result<String, String> {
    let value = match body.get(name) {
        None | Some(Value::Null) => "",
        Some(v) => v.as_str().ok_or("文字列の指定が不正です")?,
    }
    .replace("\r\n", "\n");
    if value.chars().count() > limit || value.contains("<|plamo:") {
        return Err("文字数の上限を超えているか、使えない文字列が含まれています".into());
    }
    if required && value.trim().is_empty() {
        return Err("原文が空です".into());
    }
    Ok(value)
}
pub(crate) fn languages() -> Value {
    serde_json::from_str(include_str!("../gen/local-languages.json"))
        .expect("生成した言語一覧が不正です")
}
pub fn prompt(body: &Value) -> Result<(String, usize), String> {
    let text = field(body, "text", 4000, true)?;
    let target = field(body, "target_lang", 32, true)?;
    let source = field(body, "source_lang", 32, false)?;
    let valid_lang = |v: &str| {
        languages()
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["code"] == v)
    };
    if !valid_lang(&target) || (!source.is_empty() && !valid_lang(&source)) {
        return Err("言語の指定が不正です".into());
    }
    let form = field(body, "form", 32, false)?;
    if !form.is_empty() && (target != "ja" || !["plain", "polite"].contains(&form.as_str())) {
        return Err("文体の指定が不正です".into());
    }
    let context = field(body, "context", 12000, false)?;
    let lang = |v: &str| {
        if v.is_empty() {
            String::new()
        } else {
            format!("<|plamo:key|>lang<|plamo:val|>{v}")
        }
    };
    let mut value = "<|plamo:tag|>dataset<|plamo:msg|>translation".to_string();
    if !context.trim().is_empty() {
        value += &format!("<|plamo:tag|>context<|plamo:msg|>{}", context.trim());
    }
    for (name, max, source_limit, target_limit) in
        [("history", 4, 4000, 12000), ("glossary", 200, 200, 200)]
    {
        if let Some(raw) = body.get(name).filter(|v| !v.is_null()) {
            let pairs = raw.as_array().ok_or("文脈または用語集の形式が不正です")?;
            if pairs.len() > max {
                return Err("文脈または用語集の件数が上限を超えています".into());
            }
            let mut glossary = String::new();
            for pair in pairs {
                if !pair.is_object() {
                    return Err("文脈または用語集の形式が不正です".into());
                }
                let src = field(pair, "source", source_limit, false)?;
                let dst = field(pair, "target", target_limit, false)?;
                if src.trim().is_empty() || dst.trim().is_empty() {
                    continue;
                }
                if name == "history" {
                    value += &format!("<|plamo:tag|>input{}<|plamo:msg|>{src}<|plamo:tag|>output{}<|plamo:msg|>{dst}", lang(&source), lang(&target));
                } else {
                    glossary +=
                        &format!("<|plamo:index|>{}<|plamo:item|>{}", src.trim(), dst.trim());
                }
            }
            if !glossary.is_empty() {
                value += &format!("<|plamo:tag|>glossary{glossary}");
            }
        }
    }
    value += &format!(
        "<|plamo:tag|>input{}<|plamo:msg|>{text}<|plamo:tag|>output{}",
        lang(&source),
        lang(&target)
    );
    if !form.is_empty() {
        value += &format!("<|plamo:key|>form<|plamo:val|>{form}");
    }
    value += "<|plamo:msg|>";
    if value.chars().count() > 12000 {
        return Err("文脈・前の訳・用語集を合わせて長すぎます".into());
    }
    Ok((value, (text.chars().count() * 3 / 2 + 64).clamp(256, 8000)))
}

#[derive(Default)]
pub struct Decoder {
    buffer: Vec<u8>,
    pub(crate) done: bool,
}
impl Decoder {
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Value>, String> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > 1024 * 1024 {
            return Err("推論エンジンの応答が長すぎます".into());
        }
        let mut events = vec![];
        while let Some(end) = self.buffer.iter().position(|c| *c == b'\n') {
            let line: Vec<_> = self.buffer.drain(..=end).collect();
            let line = std::str::from_utf8(&line)
                .map_err(|_| "応答の文字コードが不正です")?
                .trim();
            let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                continue;
            };
            if data == "[DONE]" {
                continue;
            }
            let value: Value = serde_json::from_str(data).map_err(|_| "応答の形式が不正です")?;
            if value.get("error").is_some() {
                return Err("ローカル推論エンジンがエラーを返しました".into());
            }
            let choice = &value["choices"][0];
            if let Some(text) = choice["text"].as_str().filter(|s| !s.is_empty()) {
                events.push(json!({"t":text}));
            }
            if let Some(finish) = choice["finish_reason"].as_str() {
                events.push(json!({"done":true,"finish":finish}));
                self.done = true;
            }
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_prompt_fixtures() {
        let cases: Vec<Value> = serde_json::from_str(include_str!(
            "../../../shared/local-translation-fixtures.json"
        ))
        .unwrap();
        for case in cases {
            let (actual, limit) = prompt(&case["body"]).unwrap();
            assert_eq!(actual, case["prompt"].as_str().unwrap(), "{}", case["name"]);
            assert_eq!(limit, case["max_tokens"].as_u64().unwrap() as usize);
        }
    }

    #[test]
    fn reject_invalid_requests() {
        for body in [
            json!({"text":" ","target_lang":"ja"}),
            json!({"text":"<|plamo:tag|>","target_lang":"ja"}),
            json!({"text":"x".repeat(4001),"target_lang":"ja"}),
            json!({"text":"test","target_lang":"invalid"}),
            json!({"text":"test","target_lang":"en","form":"polite"}),
            json!({"text":"test","target_lang":"ja","history":vec![json!({"source":"a","target":"b"}); 5]}),
            json!({"text":"test","target_lang":"ja","glossary":[{"source":"<|plamo:","target":"b"}]}),
        ] {
            assert!(prompt(&body).is_err());
        }
    }

    #[test]
    fn sse_handles_fragmented_unicode_and_finish() {
        let raw = ": keepalive\r\ndata: {\"choices\":[{\"text\":\"こんにちは🌸\",\"finish_reason\":null}]}\r\n\r\ndata: {\"choices\":[{\"text\":\"\",\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        let mut decoder = Decoder::default();
        let mut events = vec![];
        for b in raw.bytes() {
            events.extend(decoder.feed(&[b]).unwrap());
        }
        assert_eq!(
            events,
            vec![
                json!({"t":"こんにちは🌸"}),
                json!({"done":true,"finish":"stop"})
            ]
        );
        assert!(decoder.done);
    }

    #[test]
    fn reject_malformed_engine_response() {
        assert!(Decoder::default().feed(b"data: invalid\n").is_err());
        assert!(Decoder::default()
            .feed(&vec![b'x'; 1024 * 1024 + 1])
            .is_err());
    }
}
