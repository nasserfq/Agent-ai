//! OpenAI-compatible chat completions: OpenRouter (hundreds of models, many free), Groq, Ollama on
//! this computer, and any other service with the same API.

use std::io::BufRead;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};

use super::{ModelInfo, Provider, Request, parse_tool_input, post_stream};
use crate::sse::read_events;
use crate::types::{Message, Part, RawMessage, Reply, Role, Stop, StreamEvent, Usage};
use crate::{Error, Result};

fn headers(p: &Provider, key: &str) -> Vec<(&'static str, String)> {
    let mut h = Vec::new();
    if !key.trim().is_empty() {
        h.push(("authorization", format!("Bearer {}", key.trim())));
    }
    if p.base_url.contains("openrouter.ai") {
        // OpenRouter's app attribution.
        h.push(("http-referer", "https://mokteb.com".to_string()));
        h.push(("x-title", "Agent AI".to_string()));
    }
    h
}

pub fn list_models(p: &Provider, key: &str) -> Result<Vec<ModelInfo>> {
    let url = format!("{}/models", p.base_url.trim_end_matches('/'));
    let v = super::get_json(&p.name, &url, &headers(p, key))?;
    let local = p.base_url.contains("localhost") || p.base_url.contains("127.0.0.1");
    let groq = p.base_url.contains("groq.com");
    Ok(v.get("data")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    let id = m.get("id")?.as_str()?.to_string();
                    let name = m.get("name").and_then(Value::as_str).unwrap_or(&id).to_string();
                    let num = |k: &str| {
                        m.get("pricing")
                            .and_then(|p| p.get(k))
                            .and_then(|x| x.as_str().map(str::to_string).or_else(|| x.as_f64().map(|f| f.to_string())))
                    };
                    // OpenRouter prices per token, as strings.
                    let price = match (num("prompt").and_then(|s| s.parse::<f64>().ok()), num("completion").and_then(|s| s.parse::<f64>().ok())) {
                        (Some(i), Some(o)) if i >= 0.0 && o >= 0.0 => Some((i * 1_000_000.0, o * 1_000_000.0)),
                        _ => None,
                    };
                    let free = local || groq || id.ends_with(":free") || price.is_some_and(|(i, o)| i == 0.0 && o == 0.0);
                    Some(ModelInfo { id, name, free, price })
                })
                .collect()
        })
        .unwrap_or_default())
}

pub fn body(req: &Request) -> Value {
    let mut msgs = Vec::new();
    if !req.system.trim().is_empty() {
        msgs.push(json!({"role": "system", "content": req.system}));
    }
    msgs.extend(messages(req.messages));
    let mut b = json!({
        "model": req.model,
        "messages": msgs,
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if !req.tools.is_empty() {
        b["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.schema}}))
                .collect(),
        );
    }
    b
}

pub fn messages(msgs: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    for m in msgs {
        match m.role {
            Role::User => {
                let mut content = Vec::new();
                for p in &m.parts {
                    match p {
                        Part::Text { text } if !text.is_empty() => content.push(json!({"type": "text", "text": text})),
                        Part::File { name, mime, data } if mime.starts_with("image/") => {
                            content.push(json!({"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{data}")}}));
                            let _ = name;
                        }
                        Part::File { name, mime, data } if mime == "application/pdf" => {
                            content.push(json!({"type": "file", "file": {"filename": name, "file_data": format!("data:{mime};base64,{data}")}}))
                        }
                        Part::File { name, data, .. } => content.push(json!({"type": "text", "text": super::attachment_text(name, data)})),
                        Part::ToolResult { id, content: c, is_error, .. } => {
                            let c = if *is_error { format!("Error: {c}") } else { c.clone() };
                            out.push(json!({"role": "tool", "tool_call_id": id, "content": c}));
                        }
                        _ => {}
                    }
                }
                if !content.is_empty() {
                    // A text-only message goes as a plain string, which every service accepts.
                    let only_text = content.iter().all(|c| c.get("type").and_then(Value::as_str) == Some("text"));
                    let c = if only_text {
                        Value::String(content.iter().filter_map(|c| c.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"))
                    } else {
                        Value::Array(content)
                    };
                    out.push(json!({"role": "user", "content": c}));
                }
            }
            Role::Assistant => {
                let text = m.text();
                let calls: Vec<Value> = m
                    .tool_calls()
                    .into_iter()
                    .map(|(id, name, input)| json!({"id": id, "type": "function", "function": {"name": name, "arguments": input.to_string()}}))
                    .collect();
                let mut a = json!({"role": "assistant", "content": if text.is_empty() { Value::Null } else { Value::String(text) }});
                if !calls.is_empty() {
                    a["tool_calls"] = Value::Array(calls);
                }
                if a["content"].is_null() && a.get("tool_calls").is_none() {
                    continue;
                }
                out.push(a);
            }
        }
    }
    out
}

pub fn send(p: &Provider, key: &str, req: &Request, cancel: &Arc<AtomicBool>, on_event: &mut dyn FnMut(StreamEvent)) -> Result<Reply> {
    let url = format!("{}/chat/completions", p.base_url.trim_end_matches('/'));
    let r = post_stream(&p.name, &url, &headers(p, key), &body(req))?;
    parse_stream(r, &p.id, &p.name, req.model, cancel, on_event)
}

#[derive(Default)]
struct Call {
    id: String,
    name: String,
    args: String,
}

pub fn parse_stream(
    r: impl BufRead,
    provider_id: &str,
    service: &str,
    model: &str,
    cancel: &Arc<AtomicBool>,
    on_event: &mut dyn FnMut(StreamEvent),
) -> Result<Reply> {
    let mut text = String::new();
    let mut calls: Vec<Call> = Vec::new();
    let mut usage = Usage::default();
    let mut finish = String::new();
    let mut served_by = model.to_string();
    read_events(r, cancel, |ev| {
        if ev.data.trim() == "[DONE]" {
            return Ok(false);
        }
        let Ok(v) = serde_json::from_str::<Value>(&ev.data) else { return Ok(true) };
        if let Some(e) = v.get("error") {
            let m = e.get("message").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| e.to_string());
            return Err(Error::Api { service: service.into(), status: 0, body: m });
        }
        if let Some(m) = v.get("model").and_then(Value::as_str) {
            served_by = m.to_string();
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
            usage = Usage { input: n("prompt_tokens"), output: n("completion_tokens") };
        }
        let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else { return Ok(true) };
        if let Some(f) = choice.get("finish_reason").and_then(Value::as_str) {
            finish = f.to_string();
        }
        let Some(d) = choice.get("delta") else { return Ok(true) };
        for k in ["reasoning", "reasoning_content"] {
            if let Some(t) = d.get(k).and_then(Value::as_str).filter(|t| !t.is_empty()) {
                on_event(StreamEvent::Thinking(t.to_string()));
            }
        }
        if let Some(t) = d.get("content").and_then(Value::as_str).filter(|t| !t.is_empty()) {
            text.push_str(t);
            on_event(StreamEvent::Text(t.to_string()));
        }
        for tc in d.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
            let i = tc.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).unwrap_or(calls.len().saturating_sub(1));
            let i = i.min(255);
            while calls.len() <= i {
                calls.push(Call::default());
            }
            let Some(c) = calls.get_mut(i) else { continue };
            if let Some(id) = tc.get("id").and_then(Value::as_str) {
                c.id = id.to_string();
            }
            let f = tc.get("function");
            if let Some(n) = f.and_then(|f| f.get("name")).and_then(Value::as_str) {
                c.name.push_str(n);
            }
            if let Some(a) = f.and_then(|f| f.get("arguments")).and_then(Value::as_str) {
                c.args.push_str(a);
            }
        }
        Ok(true)
    })?;
    let mut parts = Vec::new();
    if !text.is_empty() {
        parts.push(Part::Text { text });
    }
    for (n, c) in calls.into_iter().enumerate().filter(|(_, c)| !c.name.is_empty()) {
        let id = if c.id.is_empty() { format!("call_{n}") } else { c.id };
        parts.push(Part::ToolCall { id, name: c.name, input: parse_tool_input(&c.args) });
    }
    let has_calls = parts.iter().any(|p| matches!(p, Part::ToolCall { .. }));
    let stop = match finish.as_str() {
        _ if has_calls => Stop::ToolUse,
        "stop" | "" => Stop::Done,
        "length" => Stop::MaxTokens,
        "content_filter" => Stop::Refused(finish.clone()),
        f => Stop::Other(f.to_string()),
    };
    let message = Message {
        role: Role::Assistant,
        parts,
        raw: Some(RawMessage { provider: provider_id.into(), model: served_by.clone(), content: Value::Null }),
        model: Some(served_by),
    };
    Ok(Reply { message, usage, stop })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_streamed_text_and_split_tool_calls() {
        let s = r#"data: {"model":"meta/llama:free","choices":[{"delta":{"content":"Hi"}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"fetch_url","arguments":"{\"url\":"}}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"https://x\"}"}}]},"finish_reason":"tool_calls"}]}

data: {"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":6}}

data: [DONE]

"#;
        let r = parse_stream(s.as_bytes(), "openrouter", "OpenRouter", "m", &Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap();
        assert_eq!(r.message.text(), "Hi");
        assert_eq!(r.message.tool_calls(), vec![("c1".into(), "fetch_url".into(), json!({"url": "https://x"}))]);
        assert_eq!(r.stop, Stop::ToolUse);
        assert_eq!(r.usage, Usage { input: 5, output: 6 });
        assert_eq!(r.message.model.as_deref(), Some("meta/llama:free"));
    }

    #[test]
    fn hostile_tool_indexes_are_capped() {
        let s = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":99999999999,\"function\":{\"name\":\"x\"}}]}}]}\n\n";
        let r = parse_stream(s.as_bytes(), "p", "P", "m", &Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap();
        assert_eq!(r.message.tool_calls().len(), 1);
    }

    #[test]
    fn tool_round_trip_messages() {
        let msgs = vec![
            Message::user_text("q"),
            Message {
                role: Role::Assistant,
                parts: vec![Part::ToolCall { id: "c1".into(), name: "f".into(), input: json!({"a": 1}) }],
                raw: None,
                model: None,
            },
            Message {
                role: Role::User,
                parts: vec![Part::ToolResult { id: "c1".into(), name: "f".into(), content: "done".into(), is_error: false }],
                raw: None,
                model: None,
            },
        ];
        let m = messages(&msgs);
        assert_eq!(m[0]["content"], "q");
        assert_eq!(m[1]["tool_calls"][0]["function"]["arguments"], "{\"a\":1}");
        assert_eq!(m[2]["role"], "tool");
    }
}
