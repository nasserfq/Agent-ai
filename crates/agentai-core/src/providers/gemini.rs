//! Gemini, through Google's Generative Language API (which has a free tier).

use std::io::BufRead;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};

use super::{ModelInfo, Provider, Request, post_stream};
use crate::sse::read_events;
use crate::types::{Message, Part, RawMessage, Reply, Role, Stop, StreamEvent, Usage};
use crate::{Error, Result};

/// Always points at Google's newest Flash model, which the free tier covers.
pub const DEFAULT_MODEL: &str = "gemini-flash-latest";

pub fn list_models(p: &Provider, key: &str) -> Result<Vec<ModelInfo>> {
    let url = format!("{}/models?pageSize=1000", p.base_url.trim_end_matches('/'));
    let v = super::get_json(&p.name, &url, &[("x-goog-api-key", key.trim().to_string())])?;
    Ok(v.get("models")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|m| {
                    m.get("supportedGenerationMethods")
                        .and_then(Value::as_array)
                        .is_some_and(|g| g.iter().any(|x| x.as_str() == Some("generateContent")))
                })
                .filter_map(|m| {
                    let id = m.get("name")?.as_str()?.trim_start_matches("models/").to_string();
                    if ["embedding", "tts", "image", "aqa", "imagen", "veo", "live", "audio"].iter().any(|x| id.contains(x)) {
                        return None;
                    }
                    let name = m.get("displayName").and_then(Value::as_str).unwrap_or(&id).to_string();
                    // The free tier covers the Flash and Gemma models.
                    let free = id.contains("flash") || id.contains("gemma");
                    Some(ModelInfo { id, name, free, price: None })
                })
                .collect()
        })
        .unwrap_or_default())
}

pub fn body(req: &Request) -> Value {
    let mut b = json!({
        "contents": contents(req.messages),
        "generationConfig": {"thinkingConfig": {"includeThoughts": true}},
    });
    if !req.system.trim().is_empty() {
        b["systemInstruction"] = json!({"parts": [{"text": req.system}]});
    }
    if !req.tools.is_empty() {
        let decls: Vec<Value> =
            req.tools.iter().map(|t| json!({"name": t.name, "description": t.description, "parametersJsonSchema": t.schema})).collect();
        b["tools"] = json!([{"functionDeclarations": decls}]);
    }
    b
}

/// The conversation in Gemini's format. Gemini's own earlier replies go back exactly as they came
/// (with their thought signatures).
pub fn contents(msgs: &[Message]) -> Value {
    let mut out: Vec<Value> = Vec::new();
    for m in msgs {
        let parts = match &m.raw {
            Some(RawMessage { provider, content, .. }) if provider == "gemini" && m.role == Role::Assistant => content.clone(),
            _ => Value::Array(m.parts.iter().filter_map(part).collect()),
        };
        if parts.as_array().is_some_and(Vec::is_empty) {
            continue;
        }
        let role = if m.role == Role::User { "user" } else { "model" };
        if let Some(last) = out.last_mut()
            && last.get("role").and_then(Value::as_str) == Some(role)
            && let (Some(a), Some(b)) = (last.get_mut("parts").and_then(Value::as_array_mut), parts.as_array())
        {
            a.extend(b.iter().cloned());
            continue;
        }
        out.push(json!({"role": role, "parts": parts}));
    }
    Value::Array(out)
}

fn part(p: &Part) -> Option<Value> {
    Some(match p {
        Part::Text { text } if text.is_empty() => return None,
        Part::Text { text } => json!({"text": text}),
        Part::File { name, mime, data } => {
            if mime.starts_with("image/") || mime == "application/pdf" || mime.starts_with("audio/") {
                json!({"inlineData": {"mimeType": mime, "data": data}})
            } else {
                json!({"text": super::attachment_text(name, data)})
            }
        }
        Part::ToolCall { name, input, .. } => json!({"functionCall": {"name": name, "args": input}}),
        Part::ToolResult { id, name, content, is_error } => {
            let key = if *is_error { "error" } else { "content" };
            let mut r = json!({"name": name, "response": {key: content}});
            // Ids we made up (`call_N`) were never Gemini's, so only Gemini's own go back.
            if !id.starts_with("call_") {
                r["id"] = json!(id);
            }
            json!({"functionResponse": r})
        }
    })
}

pub fn send(p: &Provider, key: &str, req: &Request, cancel: &Arc<AtomicBool>, on_event: &mut dyn FnMut(StreamEvent)) -> Result<Reply> {
    let model = req.model.trim_start_matches("models/");
    let url = format!("{}/models/{model}:streamGenerateContent?alt=sse", p.base_url.trim_end_matches('/'));
    let r = post_stream(&p.name, &url, &[("x-goog-api-key", key.trim().to_string())], &body(req))?;
    parse_stream(r, model, cancel, on_event)
}

pub fn parse_stream(r: impl BufRead, model: &str, cancel: &Arc<AtomicBool>, on_event: &mut dyn FnMut(StreamEvent)) -> Result<Reply> {
    let mut raw: Vec<Value> = Vec::new();
    let mut usage = Usage::default();
    let mut finish = String::new();
    read_events(r, cancel, |ev| {
        let Ok(v) = serde_json::from_str::<Value>(&ev.data) else { return Ok(true) };
        if let Some(e) = v.get("error") {
            let m = e.get("message").and_then(Value::as_str).unwrap_or("stream error");
            return Err(Error::Api { service: "Gemini".into(), status: 0, body: m.to_string() });
        }
        if let Some(u) = v.get("usageMetadata") {
            let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
            usage = Usage { input: n("promptTokenCount"), output: n("candidatesTokenCount").saturating_add(n("thoughtsTokenCount")) };
        }
        if let Some(r) = v.get("promptFeedback").and_then(|f| f.get("blockReason")).and_then(Value::as_str) {
            finish = format!("blocked:{r}");
        }
        let cand = v.get("candidates").and_then(|c| c.get(0));
        if let Some(f) = cand.and_then(|c| c.get("finishReason")).and_then(Value::as_str) {
            finish = f.to_string();
        }
        for p in cand.and_then(|c| c.get("content")).and_then(|c| c.get("parts")).and_then(Value::as_array).into_iter().flatten() {
            let text = p.get("text").and_then(Value::as_str);
            let thought = p.get("thought").and_then(Value::as_bool).unwrap_or(false);
            if let Some(t) = text {
                on_event(if thought { StreamEvent::Thinking(t.to_string()) } else { StreamEvent::Text(t.to_string()) });
            }
            // Plain text chunks are joined; anything with a signature or a call stays as it came.
            let plain =
                |x: &Value| x.as_object().is_some_and(|o| o.len() == 1 || (o.len() == 2 && o.contains_key("thought"))) && x.get("text").is_some();
            if let Some(last) = raw.last_mut()
                && plain(p)
                && plain(last)
                && last.get("thought").and_then(Value::as_bool).unwrap_or(false) == thought
                && let (Some(a), Some(t)) = (last.get("text").and_then(Value::as_str).map(str::to_string), text)
            {
                last["text"] = Value::String(a + t);
                continue;
            }
            if raw.len() < 100_000 {
                raw.push(p.clone());
            }
        }
        Ok(true)
    })?;
    let mut parts = Vec::new();
    let mut n = 0usize;
    for p in &raw {
        if p.get("thought").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        if let Some(t) = p.get("text").and_then(Value::as_str) {
            match parts.last_mut() {
                Some(Part::Text { text }) => text.push_str(t),
                _ => parts.push(Part::Text { text: t.to_string() }),
            }
        } else if let Some(fc) = p.get("functionCall") {
            n = n.saturating_add(1);
            let id = fc.get("id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("call_{n}"));
            parts.push(Part::ToolCall {
                id,
                name: fc.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                input: fc.get("args").cloned().filter(Value::is_object).unwrap_or(json!({})),
            });
        }
    }
    let has_calls = parts.iter().any(|p| matches!(p, Part::ToolCall { .. }));
    let stop = match finish.as_str() {
        _ if has_calls => Stop::ToolUse,
        "STOP" | "" => Stop::Done,
        "MAX_TOKENS" => Stop::MaxTokens,
        f if f.starts_with("blocked:") || f == "SAFETY" || f == "PROHIBITED_CONTENT" || f == "BLOCKLIST" => Stop::Refused(f.to_string()),
        f => Stop::Other(f.to_string()),
    };
    let message = Message {
        role: Role::Assistant,
        parts,
        raw: Some(RawMessage { provider: "gemini".into(), model: model.into(), content: Value::Array(raw) }),
        model: Some(model.into()),
    };
    Ok(Reply { message, usage, stop })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_thoughts_and_calls() {
        let s = r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"think","thought":true}]}}]}

data: {"candidates":[{"content":{"role":"model","parts":[{"text":"أهلا "}]}}]}

data: {"candidates":[{"content":{"role":"model","parts":[{"text":"بك"}]}}]}

data: {"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"list_folder","args":{"path":"."}},"thoughtSignature":"abc"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":4,"thoughtsTokenCount":2}}

"#;
        let mut ev = Vec::new();
        let r = parse_stream(s.as_bytes(), "gemini-flash-latest", &Arc::new(AtomicBool::new(false)), &mut |e| ev.push(e)).unwrap();
        assert_eq!(r.message.text(), "أهلا بك");
        assert_eq!(r.stop, Stop::ToolUse);
        assert_eq!(r.usage, Usage { input: 3, output: 6 });
        let raw = r.message.raw.unwrap().content;
        assert_eq!(raw.as_array().unwrap().len(), 3, "thought, joined text, call");
        assert_eq!(raw[2]["thoughtSignature"], "abc");
        assert_eq!(ev[0], StreamEvent::Thinking("think".into()));
    }

    #[test]
    fn tool_results_name_the_function() {
        let m = Message {
            role: Role::User,
            parts: vec![Part::ToolResult { id: "call_1".into(), name: "f".into(), content: "ok".into(), is_error: false }],
            raw: None,
            model: None,
        };
        let c = contents(&[m]);
        assert_eq!(c[0]["parts"][0]["functionResponse"]["name"], "f");
    }
}
