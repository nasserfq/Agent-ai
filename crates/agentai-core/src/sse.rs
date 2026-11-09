//! Server-sent events, the streaming format every provider uses.

use std::io::BufRead;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{Error, Result};

/// The largest single event accepted (a hostile or broken server can't grow it without bound).
const MAX_EVENT: usize = 32 << 20;

/// One event: its `event:` name (empty when none) and its joined `data:` lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub name: String,
    pub data: String,
}

/// Read events from `r`, calling `f` for each, until the stream ends, `f` returns `false`, or
/// `cancel` is set.
pub fn read_events(r: impl BufRead, cancel: &Arc<AtomicBool>, mut f: impl FnMut(Event) -> Result<bool>) -> Result<()> {
    let mut name = String::new();
    let mut data = String::new();
    for line in r.lines() {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let line = line.map_err(|e| Error::Network(e.to_string()))?;
        let line = line.strip_suffix('\r').unwrap_or(&line);
        if line.is_empty() {
            if !data.is_empty() || !name.is_empty() {
                let ev = Event { name: std::mem::take(&mut name), data: std::mem::take(&mut data) };
                if !f(ev)? {
                    return Ok(());
                }
            }
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => name = value.to_string(),
            "data" => {
                if data.len().saturating_add(value.len()) > MAX_EVENT {
                    return Err(Error::Invalid("streamed event too large".into()));
                }
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value);
            }
            _ => {}
        }
    }
    if !data.is_empty() {
        f(Event { name, data })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(s: &str) -> Vec<Event> {
        let mut v = Vec::new();
        read_events(s.as_bytes(), &Arc::new(AtomicBool::new(false)), |e| {
            v.push(e);
            Ok(true)
        })
        .unwrap();
        v
    }

    #[test]
    fn parses_named_and_unnamed_events() {
        let v = collect("event: a\ndata: {\"x\":1}\n\n: comment\ndata: one\ndata: two\n\ndata: [DONE]");
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], Event { name: "a".into(), data: "{\"x\":1}".into() });
        assert_eq!(v[1].data, "one\ntwo");
        assert_eq!(v[2].data, "[DONE]");
    }

    #[test]
    fn handles_crlf_and_junk() {
        let v = collect("data:x\r\n\r\nnonsense\n\n\n");
        assert_eq!(v[0].data, "x");
    }

    #[test]
    fn stops_when_cancelled() {
        let c = Arc::new(AtomicBool::new(true));
        assert!(matches!(read_events("data: x\n\n".as_bytes(), &c, |_| Ok(true)), Err(Error::Cancelled)));
    }
}
