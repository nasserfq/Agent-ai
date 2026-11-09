//! A small MCP client over stdio: starts a server (such as `kalima-cli mcp`), lists its tools and
//! calls them.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::error::clip;
use crate::{Error, Result};

const PROTOCOL: &str = "2025-06-18";
const START_TIMEOUT: Duration = Duration::from_secs(30);
const CALL_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_LINE: usize = 64 << 20;

#[derive(Debug, Clone, PartialEq)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

pub struct Client {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Client {
    /// Start `command args…` and complete the MCP handshake.
    pub fn start(command: &str, args: &[String]) -> Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn().map_err(|e| Error::Tool(format!("couldn't start {command}: {e}")))?;
        let stdin = child.stdin.take().ok_or_else(|| Error::Tool("no stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| Error::Tool("no stdout".into()))?;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match r.by_ref().take(MAX_LINE as u64).read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let mut c = Client { child, stdin, lines: rx, next_id: 0 };
        c.request(
            "initialize",
            json!({"protocolVersion": PROTOCOL, "capabilities": {}, "clientInfo": {"name": "agent-ai", "version": env!("CARGO_PKG_VERSION")}}),
            START_TIMEOUT,
        )?;
        c.notify("notifications/initialized")?;
        Ok(c)
    }

    pub fn list_tools(&mut self) -> Result<Vec<McpTool>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..50 {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let r = self.request("tools/list", params, START_TIMEOUT)?;
            for t in r.get("tools").and_then(Value::as_array).into_iter().flatten() {
                let Some(name) = t.get("name").and_then(Value::as_str) else { continue };
                out.push(McpTool {
                    name: name.to_string(),
                    description: t.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                    schema: t.get("inputSchema").cloned().filter(Value::is_object).unwrap_or(json!({"type": "object"})),
                });
            }
            cursor = r.get("nextCursor").and_then(Value::as_str).map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(out)
    }

    /// Call `name`; the text of its result, or an error when the tool reports one.
    pub fn call(&mut self, name: &str, args: &Value) -> Result<String> {
        let r = self.request("tools/call", json!({"name": name, "arguments": args}), CALL_TIMEOUT)?;
        let mut text = Vec::new();
        for c in r.get("content").and_then(Value::as_array).into_iter().flatten() {
            match c.get("type").and_then(Value::as_str) {
                Some("text") => text.push(c.get("text").and_then(Value::as_str).unwrap_or("").to_string()),
                Some("resource") => {
                    if let Some(t) = c.get("resource").and_then(|r| r.get("text")).and_then(Value::as_str) {
                        text.push(t.to_string());
                    }
                }
                Some(other) => text.push(format!("[{other} content]")),
                None => {}
            }
        }
        if text.is_empty()
            && let Some(s) = r.get("structuredContent")
        {
            text.push(s.to_string());
        }
        let out = text.join("\n");
        if r.get("isError").and_then(Value::as_bool).unwrap_or(false) {
            return Err(Error::Tool(clip(&out, 4000)));
        }
        Ok(out)
    }

    fn notify(&mut self, method: &str) -> Result<()> {
        self.send(&json!({"jsonrpc": "2.0", "method": method}))
    }

    fn send(&mut self, v: &Value) -> Result<()> {
        let mut line = serde_json::to_string(v)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        self.next_id = self.next_id.saturating_add(1);
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(left) {
                Ok(l) => l,
                Err(RecvTimeoutError::Timeout) => return Err(Error::Tool(format!("{method} took too long"))),
                Err(RecvTimeoutError::Disconnected) => return Err(Error::Tool("the server stopped".into())),
            };
            let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { continue };
            if v.get("id").and_then(Value::as_u64) != Some(id) {
                // A request from the server (none are supported) gets an error back so it doesn't wait.
                if let (Some(rid), Some(_)) = (v.get("id"), v.get("method")) {
                    let _ = self.send(&json!({"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": "not supported"}}));
                }
                continue;
            }
            if let Some(e) = v.get("error") {
                let m = e.get("message").and_then(Value::as_str).unwrap_or("error");
                return Err(Error::Tool(clip(m, 2000)));
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A fake MCP server written in shell: answers by request id.
    const FAKE: &str = r#"
while IFS= read -r line; do
  case "$line" in
    *'"initialize"'*) echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"f","version":"1"}}}' ;;
    *'"tools/list"'*) echo 'garbage'; echo '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}}' ;;
    *'"tools/call"'*) echo '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"تم"}]}}' ;;
  esac
done
"#;

    #[test]
    fn talks_to_a_server() {
        let mut c = Client::start("sh", &["-c".into(), FAKE.into()]).unwrap();
        let tools = c.list_tools().unwrap();
        assert_eq!(tools[0].name, "echo");
        assert_eq!(c.call("echo", &json!({})).unwrap(), "تم");
    }

    #[test]
    fn missing_server_is_an_error() {
        assert!(Client::start("/nonexistent/agentai-mcp", &[]).is_err());
    }
}
