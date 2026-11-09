//! The AI services: Claude (Anthropic), Gemini (Google), and every OpenAI-compatible service
//! (OpenRouter, Groq, Ollama on this computer, or any other address).

pub mod anthropic;
pub mod gemini;
pub mod openai;

use std::io::BufReader;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::clip;
use crate::types::{Message, Reply, StreamEvent, ToolSpec};
use crate::{Error, Result};

/// The wire protocol a service speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Anthropic,
    Gemini,
    OpenAi,
}

/// A service the app can talk to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Provider {
    /// Stable id, also the key's name in the keychain.
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub base_url: String,
    /// Whether requests need an API key (Ollama on this computer doesn't).
    pub needs_key: bool,
    /// Where to get a key, shown in Settings.
    #[serde(default)]
    pub key_url: String,
    /// Built in (can't be removed).
    #[serde(default)]
    pub builtin: bool,
}

impl Provider {
    fn new(id: &str, name: &str, kind: Kind, base_url: &str, needs_key: bool, key_url: &str) -> Self {
        Provider { id: id.into(), name: name.into(), kind, base_url: base_url.into(), needs_key, key_url: key_url.into(), builtin: true }
    }
}

/// The services offered out of the box.
pub fn builtin_providers() -> Vec<Provider> {
    vec![
        Provider::new("anthropic", "Claude", Kind::Anthropic, "https://api.anthropic.com/v1", true, "https://console.anthropic.com/settings/keys"),
        Provider::new(
            "gemini",
            "Gemini",
            Kind::Gemini,
            "https://generativelanguage.googleapis.com/v1beta",
            true,
            "https://aistudio.google.com/apikey",
        ),
        Provider::new("openrouter", "OpenRouter", Kind::OpenAi, "https://openrouter.ai/api/v1", true, "https://openrouter.ai/keys"),
        Provider::new("groq", "Groq", Kind::OpenAi, "https://api.groq.com/openai/v1", true, "https://console.groq.com/keys"),
        Provider::new("ollama", "Ollama", Kind::OpenAi, "http://localhost:11434/v1", false, "https://ollama.com/download"),
    ]
}

/// A model one service offers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    /// Free to use (a free model, a free tier, or a model running on this computer).
    pub free: bool,
    /// US dollars per million input and output tokens, when known.
    pub price: Option<(f64, f64)>,
}

/// One request to a model.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    pub model: &'a str,
    pub system: &'a str,
    pub messages: &'a [Message],
    pub tools: &'a [ToolSpec],
    /// "low", "medium" or "high" (Claude's effort; ignored where not supported).
    pub effort: &'a str,
}

/// Send `req` and stream the reply to `on_event`.
pub fn send(p: &Provider, key: &str, req: &Request, cancel: &Arc<AtomicBool>, on_event: &mut dyn FnMut(StreamEvent)) -> Result<Reply> {
    if p.needs_key && key.trim().is_empty() {
        return Err(Error::NoKey(p.name.clone()));
    }
    match p.kind {
        Kind::Anthropic => anthropic::send(p, key, req, cancel, on_event),
        Kind::Gemini => gemini::send(p, key, req, cancel, on_event),
        Kind::OpenAi => openai::send(p, key, req, cancel, on_event),
    }
}

/// The models `p` offers.
pub fn list_models(p: &Provider, key: &str) -> Result<Vec<ModelInfo>> {
    if p.needs_key && key.trim().is_empty() {
        return Err(Error::NoKey(p.name.clone()));
    }
    let mut v = match p.kind {
        Kind::Anthropic => anthropic::list_models(p, key)?,
        Kind::Gemini => gemini::list_models(p, key)?,
        Kind::OpenAi => openai::list_models(p, key)?,
    };
    v.sort_by(|a, b| b.free.cmp(&a.free).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(v)
}

/// The cost in US dollars of `input` and `output` tokens at `price`.
pub fn cost(price: (f64, f64), input: u64, output: u64) -> f64 {
    (input as f64 * price.0 + output as f64 * price.1) / 1_000_000.0
}

// ---- HTTP ----

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(300)))
        .http_status_as_error(false)
        .user_agent(concat!("AgentAI/", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent()
}

/// POST `body` as JSON and return the open streaming response, or the service's error.
pub(crate) fn post_stream(service: &str, url: &str, headers: &[(&str, String)], body: &Value) -> Result<BufReader<ureq::BodyReader<'static>>> {
    let mut req = agent().post(url);
    for (k, v) in headers {
        req = req.header(*k, v.as_str());
    }
    let resp = req.header("content-type", "application/json").send(serde_json::to_vec(body)?)?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let text = resp.into_body().read_to_string().unwrap_or_default();
        return Err(Error::Api { service: service.into(), status, body: error_message(&text) });
    }
    Ok(BufReader::new(resp.into_body().into_reader()))
}

/// GET `url` and parse the JSON answer.
pub(crate) fn get_json(service: &str, url: &str, headers: &[(&str, String)]) -> Result<Value> {
    let mut req = agent().get(url);
    for (k, v) in headers {
        req = req.header(*k, v.as_str());
    }
    let resp = req.call()?;
    let status = resp.status().as_u16();
    let text = resp.into_body().with_config().limit(64 << 20).read_to_string()?;
    if !(200..300).contains(&status) {
        return Err(Error::Api { service: service.into(), status, body: error_message(&text) });
    }
    Ok(serde_json::from_str(&text)?)
}

/// A readable message from an error body: the `error.message` the services send, or the start of it.
pub(crate) fn error_message(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        let e = v.get("error").unwrap_or(&v);
        let e = e.as_array().and_then(|a| a.first()).and_then(|x| x.get("error")).unwrap_or(e);
        if let Some(m) = e.get("message").and_then(Value::as_str) {
            return clip(m, 600);
        }
        if let Some(m) = e.as_str() {
            return clip(m, 600);
        }
    }
    clip(body.trim(), 600)
}

/// The JSON object in `s`, or an error naming what was wrong (a tool input that didn't parse).
pub(crate) fn parse_tool_input(s: &str) -> Value {
    if s.trim().is_empty() {
        return Value::Object(Default::default());
    }
    match serde_json::from_str::<Value>(s) {
        Ok(v @ Value::Object(_)) => v,
        _ => serde_json::json!({ crate::tools::INVALID_INPUT: clip(s, 2000) }),
    }
}

/// The bytes of a base64 attachment as text (for text files sent inline).
pub(crate) fn attachment_text(name: &str, data: &str) -> String {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data).unwrap_or_default();
    format!("<file name=\"{name}\">\n{}\n</file>", String::from_utf8_lossy(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_messages_are_readable() {
        assert_eq!(error_message(r#"{"type":"error","error":{"type":"x","message":"bad key"}}"#), "bad key");
        assert_eq!(error_message(r#"[{"error":{"code":400,"message":"API key not valid"}}]"#), "API key not valid");
        assert_eq!(error_message(r#"{"error":"nope"}"#), "nope");
        assert_eq!(error_message("plain"), "plain");
    }

    #[test]
    fn tool_input_parsing_never_fails() {
        assert_eq!(parse_tool_input(""), serde_json::json!({}));
        assert_eq!(parse_tool_input("{\"a\":1}"), serde_json::json!({"a":1}));
        assert!(parse_tool_input("{\"a\":").get(crate::tools::INVALID_INPUT).is_some());
        assert!(parse_tool_input("[1]").get(crate::tools::INVALID_INPUT).is_some());
    }

    #[test]
    fn cost_is_per_million() {
        assert!((cost((4.0, 20.0), 1_000_000, 500_000) - 14.0).abs() < 1e-9);
    }
}
