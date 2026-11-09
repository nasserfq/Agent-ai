//! The conversation model shared by every provider.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// One piece of a message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text {
        text: String,
    },
    /// An attached file (image, PDF or text), base64 encoded.
    File {
        name: String,
        mime: String,
        data: String,
    },
    /// The model asks to run a tool.
    ToolCall {
        id: String,
        name: String,
        input: Value,
    },
    /// What a tool returned.
    ToolResult {
        id: String,
        name: String,
        content: String,
        is_error: bool,
    },
}

/// The message exactly as one provider sent it, kept so the next request to the same provider and
/// model can send it back unchanged (Claude's thinking blocks and Gemini's thought signatures
/// must be returned as they came).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawMessage {
    pub provider: String,
    pub model: String,
    pub content: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub parts: Vec<Part>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<RawMessage>,
    /// Which model wrote an assistant message (shown under it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Message { role: Role::User, parts: vec![Part::Text { text: text.into() }], raw: None, model: None }
    }

    /// The message's text parts joined.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for p in &self.parts {
            if let Part::Text { text } = p {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        out
    }

    pub fn tool_calls(&self) -> Vec<(String, String, Value)> {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::ToolCall { id, name, input } => Some((id.clone(), name.clone(), input.clone())),
                _ => None,
            })
            .collect()
    }
}

/// A tool the model may call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the input object.
    pub schema: Value,
}

/// Tokens used by one request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
}

impl Usage {
    pub fn add(&mut self, o: Usage) {
        self.input = self.input.saturating_add(o.input);
        self.output = self.output.saturating_add(o.output);
    }
}

/// What a request streams back while it runs.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Text(String),
    /// A short reasoning summary or progress note, shown dimmed.
    Thinking(String),
}

/// Why the model stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    Done,
    ToolUse,
    MaxTokens,
    Refused(String),
    Other(String),
}

/// A finished reply.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub message: Message,
    pub usage: Usage,
    pub stop: Stop,
}
