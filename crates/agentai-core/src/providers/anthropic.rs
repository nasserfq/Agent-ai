//! Claude, through Anthropic's Messages API.

use std::io::BufRead;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};

use super::{ModelInfo, Provider, Request, parse_tool_input, post_stream};
use crate::sse::read_events;
use crate::types::{Message, Part, RawMessage, Reply, Role, Stop, StreamEvent, Usage};
use crate::{Error, Result};

const VERSION: &str = "2026-10-21";
/// Server-side refusal fallback, routed by refusal category (`fallbacks: "default"`).
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";

/// The current models, used when the model list can't be fetched, with their prices.
pub fn known_models() -> Vec<ModelInfo> {
    [
        ("claude-opus-5-5", "Claude Opus 5.5", 4.0, 20.0),
        ("claude-sonnet-5-5", "Claude Sonnet 5.5", 2.0, 10.0),
        ("claude-haiku-5-5", "Claude Haiku 5.5", 0.10, 0.50),
        ("claude-fable-5-1", "Claude Fable 5.1", 10.0, 50.0),
    ]
    .into_iter()
    .map(|(id, name, i, o)| ModelInfo { id: id.into(), name: name.into(), free: false, price: Some((i, o)) })
    .collect()
}

fn price(id: &str) -> Option<(f64, f64)> {
    let table = [
        ("claude-fable-5", (10.0, 50.0)),
        ("claude-opus-5-5", (4.0, 20.0)),
        ("claude-opus-5", (5.0, 25.0)),
        ("claude-opus-4", (5.0, 25.0)),
        ("claude-sonnet-5", (2.0, 10.0)),
        ("claude-sonnet-4", (3.0, 15.0)),
        ("claude-haiku-5-5", (0.10, 0.50)),
        ("claude-haiku-4", (1.0, 5.0)),
    ];
    table.iter().find(|(p, _)| id.starts_with(p)).map(|(_, v)| *v)
}

/// Models that take adaptive thinking and `output_config.effort` (4.6 and later).
fn modern(id: &str) -> bool {
    id.starts_with("claude-") && !["claude-3", "haiku-4", "-4-0", "-4-1", "-4-2", "-4-5", "sonnet-4-2", "opus-4-2"].iter().any(|old| id.contains(old))
}

/// Models that accept `fallbacks: "default"`.
fn takes_fallback(id: &str) -> bool {
    ["claude-fable-5-1", "claude-opus-5-5", "claude-opus-5", "claude-sonnet-5-5"].contains(&id)
}

pub fn list_models(p: &Provider, key: &str) -> Result<Vec<ModelInfo>> {
    let url = format!("{}/models?limit=1000", p.base_url.trim_end_matches('/'));
    let v = super::get_json(&p.name, &url, &headers(key))?;
    let mut out: Vec<ModelInfo> = v
        .get("data")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    let id = m.get("id")?.as_str()?.to_string();
                    let name = m.get("display_name").and_then(Value::as_str).unwrap_or(&id).to_string();
                    Some(ModelInfo { price: price(&id), id, name, free: false })
                })
                .collect()
        })
        .unwrap_or_default();
    if out.is_empty() {
        out = known_models();
    }
    Ok(out)
}

fn headers(key: &str) -> Vec<(&'static str, String)> {
    vec![("x-api-key", key.trim().to_string()), ("anthropic-version", VERSION.to_string())]
}

/// The request body for `req`.
pub fn body(req: &Request) -> Value {
    let mut b = json!({
        "model": req.model,
        "max_tokens": 64000,
        "stream": true,
        "messages": messages(req.messages),
        // Caches the conversation so far: later turns cost less.
        "cache_control": {"type": "ephemeral"},
    });
    if !req.system.trim().is_empty() {
        b["system"] = json!(req.system);
    }
    if modern(req.model) {
        b["thinking"] = json!({"type": "adaptive", "display": "summarized"});
        if matches!(req.effort, "low" | "medium" | "high") {
            b["output_config"] = json!({"effort": req.effort});
        }
    }
    if takes_fallback(req.model) {
        b["fallbacks"] = json!("default");
    }
    if !req.tools.is_empty() {
        b["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.schema, "eager_input_streaming": true}))
                .collect(),
        );
    }
    b
}

/// The conversation in Anthropic's format. Claude's own earlier replies go back exactly as they
/// came (thinking blocks included).
pub fn messages(msgs: &[Message]) -> Value {
    let mut out: Vec<Value> = Vec::new();
    for m in msgs {
        let content = match &m.raw {
            Some(RawMessage { provider, content, .. }) if provider == "anthropic" && m.role == Role::Assistant => content.clone(),
            _ => Value::Array(m.parts.iter().filter_map(part).collect()),
        };
        if content.as_array().is_some_and(Vec::is_empty) {
            continue;
        }
        let role = if m.role == Role::User { "user" } else { "assistant" };
        // Two messages in a row from the same side are joined (the API wants them alternating).
        if let Some(last) = out.last_mut()
            && last.get("role").and_then(Value::as_str) == Some(role)
            && let (Some(a), Some(b)) = (last.get_mut("content").and_then(Value::as_array_mut), content.as_array())
        {
            a.extend(b.iter().cloned());
            continue;
        }
        out.push(json!({"role": role, "content": content}));
    }
    Value::Array(out)
}

fn part(p: &Part) -> Option<Value> {
    Some(match p {
        Part::Text { text } if text.is_empty() => return None,
        Part::Text { text } => json!({"type": "text", "text": text}),
        Part::File { name, mime, data } => {
            if mime.starts_with("image/") {
                json!({"type": "image", "source": {"type": "base64", "media_type": mime, "data": data}})
            } else if mime == "application/pdf" {
                json!({"type": "document", "source": {"type": "base64", "media_type": mime, "data": data}, "title": name})
            } else {
                json!({"type": "text", "text": super::attachment_text(name, data)})
            }
        }
        Part::ToolCall { id, name, input } => json!({"type": "tool_use", "id": id, "name": name, "input": input}),
        Part::ToolResult { id, content, is_error, .. } => {
            json!({"type": "tool_result", "tool_use_id": id, "content": content, "is_error": is_error})
        }
    })
}

pub fn send(p: &Provider, key: &str, req: &Request, cancel: &Arc<AtomicBool>, on_event: &mut dyn FnMut(StreamEvent)) -> Result<Reply> {
    let url = format!("{}/messages", p.base_url.trim_end_matches('/'));
    let mut h = headers(key);
    if takes_fallback(req.model) {
        h.push(("anthropic-beta", FALLBACK_BETA.to_string()));
    }
    let r = post_stream(&p.name, &url, &h, &body(req))?;
    parse_stream(r, req.model, cancel, on_event)
}

/// Read a streamed reply.
pub fn parse_stream(r: impl BufRead, model: &str, cancel: &Arc<AtomicBool>, on_event: &mut dyn FnMut(StreamEvent)) -> Result<Reply> {
    let mut blocks: Vec<Value> = Vec::new();
    let mut partial: Vec<String> = Vec::new();
    let mut usage = Usage::default();
    let mut stop = Stop::Other(String::new());
    let mut served_by = model.to_string();
    read_events(r, cancel, |ev| {
        let Ok(v) = serde_json::from_str::<Value>(&ev.data) else { return Ok(true) };
        let index = v.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok());
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "message_start" => {
                let m = v.get("message");
                if let Some(id) = m.and_then(|m| m.get("model")).and_then(Value::as_str) {
                    served_by = id.to_string();
                }
                let u = m.and_then(|m| m.get("usage"));
                let n = |k: &str| u.and_then(|u| u.get(k)).and_then(Value::as_u64).unwrap_or(0);
                usage.input = n("input_tokens").saturating_add(n("cache_read_input_tokens")).saturating_add(n("cache_creation_input_tokens"));
            }
            "content_block_start" => {
                let block = v.get("content_block").cloned().unwrap_or(Value::Null);
                // Indexes arrive in order; a gap (never expected) is filled so later ones line up.
                if let Some(i) = index {
                    while blocks.len() <= i && blocks.len() < 10_000 {
                        blocks.push(Value::Null);
                        partial.push(String::new());
                    }
                    if let Some(b) = blocks.get_mut(i) {
                        *b = block;
                    }
                }
            }
            "content_block_delta" => {
                let d = v.get("delta").cloned().unwrap_or(Value::Null);
                let (Some(i), Some(b)) = (index, index.and_then(|i| blocks.get_mut(i))) else { return Ok(true) };
                match d.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text_delta" => {
                        let t = d.get("text").and_then(Value::as_str).unwrap_or("");
                        append(b, "text", t);
                        on_event(StreamEvent::Text(t.to_string()));
                    }
                    "thinking_delta" => {
                        let t = d.get("thinking").and_then(Value::as_str).unwrap_or("");
                        append(b, "thinking", t);
                        on_event(StreamEvent::Thinking(t.to_string()));
                    }
                    "signature_delta" => {
                        if let Some(s) = d.get("signature") {
                            b["signature"] = s.clone();
                        }
                    }
                    "input_json_delta" => {
                        if let Some(p) = partial.get_mut(i) {
                            p.push_str(d.get("partial_json").and_then(Value::as_str).unwrap_or(""));
                        }
                    }
                    "citations_delta" => {
                        if let Some(c) = d.get("citation") {
                            if !b.get("citations").is_some_and(Value::is_array) {
                                b["citations"] = json!([]);
                            }
                            if let Some(a) = b.get_mut("citations").and_then(Value::as_array_mut) {
                                a.push(c.clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let (Some(i), Some(b)) = (index, index.and_then(|i| blocks.get_mut(i)))
                    && b.get("type").and_then(Value::as_str) == Some("tool_use")
                    && let Some(p) = partial.get(i)
                    && !p.is_empty()
                {
                    b["input"] = parse_tool_input(p);
                }
            }
            "message_delta" => {
                if let Some(o) = v.get("usage").and_then(|u| u.get("output_tokens")).and_then(Value::as_u64) {
                    usage.output = o;
                }
                let d = v.get("delta");
                stop = match d.and_then(|d| d.get("stop_reason")).and_then(Value::as_str).unwrap_or("") {
                    "end_turn" | "stop_sequence" => Stop::Done,
                    "tool_use" => Stop::ToolUse,
                    "max_tokens" => Stop::MaxTokens,
                    "refusal" => Stop::Refused(
                        d.and_then(|d| d.get("stop_details")).and_then(|s| s.get("category")).and_then(Value::as_str).unwrap_or("").to_string(),
                    ),
                    "pause_turn" => Stop::Other("pause_turn".into()),
                    other => Stop::Other(other.to_string()),
                };
            }
            "error" => {
                let m = v.get("error").and_then(|e| e.get("message")).and_then(Value::as_str).unwrap_or("stream error");
                return Err(Error::Api { service: "Claude".into(), status: 0, body: m.to_string() });
            }
            "message_stop" => return Ok(false),
            _ => {}
        }
        Ok(true)
    })?;
    blocks.retain(|b| !b.is_null());
    let mut parts = Vec::new();
    for b in &blocks {
        match b.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" => parts.push(Part::Text { text: b.get("text").and_then(Value::as_str).unwrap_or("").to_string() }),
            "tool_use" => parts.push(Part::ToolCall {
                id: b.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                name: b.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                input: b.get("input").cloned().unwrap_or(json!({})),
            }),
            _ => {}
        }
    }
    let message = Message {
        role: Role::Assistant,
        parts,
        raw: Some(RawMessage { provider: "anthropic".into(), model: served_by.clone(), content: Value::Array(blocks) }),
        model: Some(served_by),
    };
    Ok(Reply { message, usage, stop })
}

fn append(b: &mut Value, field: &str, t: &str) {
    let cur = b.get(field).and_then(Value::as_str).unwrap_or("").to_string();
    b[field] = Value::String(cur + t);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolSpec;

    const STREAM: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"m","model":"claude-opus-5-5","usage":{"input_tokens":10,"cache_read_input_tokens":5}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Hmm"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"مرحبا"}}

event: content_block_start
data: {"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"t1","name":"read_file","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}

event: content_block_delta
data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"a.txt\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":2}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}

event: message_stop
data: {"type":"message_stop"}

"#;

    #[test]
    fn parses_a_streamed_tool_call_and_keeps_thinking() {
        let mut seen = Vec::new();
        let r = parse_stream(STREAM.as_bytes(), "claude-opus-5-5", &Arc::new(AtomicBool::new(false)), &mut |e| seen.push(e)).unwrap();
        assert_eq!(r.stop, Stop::ToolUse);
        assert_eq!(r.usage, Usage { input: 15, output: 7 });
        assert_eq!(r.message.text(), "مرحبا");
        assert_eq!(r.message.tool_calls(), vec![("t1".into(), "read_file".into(), json!({"path": "a.txt"}))]);
        let raw = r.message.raw.unwrap().content;
        assert_eq!(raw[0]["signature"], "sig");
        assert_eq!(raw[0]["thinking"], "Hmm");
        assert!(seen.contains(&StreamEvent::Thinking("Hmm".into())));
    }

    #[test]
    fn hostile_streams_do_not_panic() {
        let junk = "data: {\"type\":\"content_block_delta\",\"index\":99999999999,\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n\n\
                    data: {\"type\":\"content_block_start\",\"index\":-1}\n\ndata: not json\n\n\
                    data: {\"type\":\"content_block_stop\",\"index\":0}\n\n";
        let r = parse_stream(junk.as_bytes(), "m", &Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap();
        assert!(r.message.parts.is_empty());
    }

    #[test]
    fn request_body_uses_current_api() {
        let msgs = vec![Message::user_text("hi")];
        let tools = vec![ToolSpec { name: "t".into(), description: "d".into(), schema: json!({"type":"object"}) }];
        let b = body(&Request { model: "claude-opus-5-5", system: "be kind", messages: &msgs, tools: &tools, effort: "low" });
        assert_eq!(b["thinking"]["type"], "adaptive");
        assert_eq!(b["output_config"]["effort"], "low");
        assert_eq!(b["fallbacks"], "default");
        assert_eq!(b["tools"][0]["eager_input_streaming"], true);
        assert_eq!(b["system"], "be kind");
        let old = body(&Request { model: "claude-haiku-4-5", system: "", messages: &msgs, tools: &[], effort: "low" });
        assert!(old.get("thinking").is_none() && old.get("fallbacks").is_none() && old.get("system").is_none());
    }

    #[test]
    fn earlier_claude_replies_go_back_unchanged() {
        let raw = json!([{"type":"thinking","thinking":"x","signature":"s"},{"type":"text","text":"a"}]);
        let msgs = vec![
            Message::user_text("q"),
            Message {
                role: Role::Assistant,
                parts: vec![Part::Text { text: "a".into() }],
                raw: Some(RawMessage { provider: "anthropic".into(), model: "m".into(), content: raw.clone() }),
                model: None,
            },
            Message::user_text("q2"),
            Message::user_text("q3"),
        ];
        let m = messages(&msgs);
        assert_eq!(m[1]["content"], raw);
        assert_eq!(m.as_array().unwrap().len(), 3, "consecutive user messages are joined");
    }
}
