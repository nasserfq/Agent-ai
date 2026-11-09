//! One agent turn: ask the model, run the tools it calls (asking the person first where needed),
//! send the results back, and repeat until it answers.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};

use serde_json::Value;

use crate::error::clip;
use crate::providers::{Provider, Request, cost, send};
use crate::tools::Toolbox;
use crate::types::{Message, Part, Role, Stop, StreamEvent, Usage};
use crate::{Error, Result};

/// The most model calls one turn makes before stopping to check with the person.
pub const MAX_STEPS: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    Yes,
    /// Yes, and don't ask again for this tool in this chat.
    Always,
    No,
}

#[derive(Debug)]
pub enum RunEvent {
    Text(String),
    Thinking(String),
    /// A finished assistant message (append it to the chat).
    Assistant(Message),
    ToolStarted {
        id: String,
        name: String,
    },
    ToolFinished {
        id: String,
        ok: bool,
        summary: String,
    },
    /// The tool results message (append it to the chat).
    ToolResults(Message),
    /// The person must approve a tool call; answer on `reply`.
    Approve {
        name: String,
        owner: String,
        input: Value,
        reply: Sender<Approval>,
    },
    Usage {
        usage: Usage,
        cost: Option<f64>,
    },
    /// The turn ended.
    Finished(Stop),
}

pub struct RunConfig {
    pub provider: Provider,
    pub key: String,
    pub model: String,
    pub system: String,
    pub effort: String,
    pub price: Option<(f64, f64)>,
}

/// Run one turn over `history` (which ends with the person's message). `allowed` holds the tools
/// the person said to stop asking about in this chat.
pub fn run(
    cfg: &RunConfig,
    tools: &mut Toolbox,
    mut history: Vec<Message>,
    allowed: &mut HashSet<String>,
    cancel: &Arc<AtomicBool>,
    emit: &mut dyn FnMut(RunEvent),
) -> Result<()> {
    close_dangling_calls(&mut history);
    let specs = tools.specs();
    for _ in 0..MAX_STEPS {
        let req = Request { model: &cfg.model, system: &cfg.system, messages: &history, tools: &specs, effort: &cfg.effort };
        let reply = send(&cfg.provider, &cfg.key, &req, cancel, &mut |e| match e {
            StreamEvent::Text(t) => emit(RunEvent::Text(t)),
            StreamEvent::Thinking(t) => emit(RunEvent::Thinking(t)),
        })?;
        emit(RunEvent::Usage { usage: reply.usage, cost: cfg.price.map(|p| cost(p, reply.usage.input, reply.usage.output)) });
        let calls = reply.message.tool_calls();
        history.push(reply.message.clone());
        emit(RunEvent::Assistant(reply.message));
        // A cut-off or refused reply never runs its tools.
        if calls.is_empty() || matches!(reply.stop, Stop::MaxTokens | Stop::Refused(_)) {
            emit(RunEvent::Finished(reply.stop));
            return Ok(());
        }
        let mut results = Vec::new();
        for (id, name, input) in calls {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let approved = if tools.needs_approval(&name) && !allowed.contains(&name) {
                let (tx, rx) = channel();
                emit(RunEvent::Approve { name: name.clone(), owner: tools.owner(&name), input: input.clone(), reply: tx });
                match rx.recv().unwrap_or(Approval::No) {
                    Approval::Always => {
                        allowed.insert(name.clone());
                        true
                    }
                    Approval::Yes => true,
                    Approval::No => false,
                }
            } else {
                true
            };
            emit(RunEvent::ToolStarted { id: id.clone(), name: name.clone() });
            let (content, is_error) = if !approved {
                ("The person declined this action. Ask them how to continue.".to_string(), true)
            } else {
                match tools.call(&name, &input) {
                    Ok(s) => (s, false),
                    Err(e) => (e.to_string(), true),
                }
            };
            emit(RunEvent::ToolFinished { id: id.clone(), ok: !is_error, summary: clip(&content, 300) });
            results.push(Part::ToolResult { id, name, content, is_error });
        }
        let m = Message { role: Role::User, parts: results, raw: None, model: None };
        history.push(m.clone());
        emit(RunEvent::ToolResults(m));
    }
    emit(RunEvent::Finished(Stop::Other("steps".into())));
    Ok(())
}

/// Give every tool call in `history` a result: a turn stopped between a call and its result would
/// otherwise make the next request invalid.
pub fn close_dangling_calls(history: &mut Vec<Message>) {
    let mut i = 0;
    while i < history.len() {
        let calls = match history.get(i) {
            Some(m) if m.role == Role::Assistant => m.tool_calls(),
            _ => Vec::new(),
        };
        if !calls.is_empty() {
            let answered: HashSet<String> = history
                .get(i + 1)
                .map(|m| m.parts.iter().filter_map(|p| if let Part::ToolResult { id, .. } = p { Some(id.clone()) } else { None }).collect())
                .unwrap_or_default();
            let missing: Vec<Part> = calls
                .into_iter()
                .filter(|(id, _, _)| !answered.contains(id))
                .map(|(id, name, _)| Part::ToolResult { id, name, content: "Stopped before this ran.".into(), is_error: true })
                .collect();
            if !missing.is_empty() {
                match history.get_mut(i + 1) {
                    Some(next) if next.role == Role::User && !answered.is_empty() => next.parts.extend(missing),
                    _ => history.insert(i + 1, Message { role: Role::User, parts: missing, raw: None, model: None }),
                }
            }
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dangling_calls_get_results() {
        let call = Message {
            role: Role::Assistant,
            parts: vec![Part::ToolCall { id: "a".into(), name: "f".into(), input: json!({}) }],
            raw: None,
            model: None,
        };
        let mut h = vec![Message::user_text("q"), call, Message::user_text("next")];
        close_dangling_calls(&mut h);
        assert_eq!(h.len(), 4);
        assert!(matches!(&h[2].parts[0], Part::ToolResult { id, is_error: true, .. } if id == "a"));
        close_dangling_calls(&mut h);
        assert_eq!(h.len(), 4, "idempotent");
    }
}
