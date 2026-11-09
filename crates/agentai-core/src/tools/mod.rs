//! What agents can do: the built-in tools (files in one chosen folder, reading web pages) and the
//! tools of MCP servers (كلمة, جدول, عرض, وثيقة or any other).

pub mod builtin;
pub mod mcp;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::clip;
use crate::types::ToolSpec;
use crate::{Error, Result};

/// The field a tool input carries when the model's JSON didn't parse (the tool then reports it).
pub const INVALID_INPUT: &str = "__invalid_input";

/// The most text one tool result sends back to the model.
pub const MAX_RESULT: usize = 100_000;

/// An MCP server the app can start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServer {
    /// Short id, used as the prefix of its tool names.
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Run its tools without asking first.
    #[serde(default)]
    pub trusted: bool,
}

/// The مكتب apps, each with the MCP server it ships: (id, Arabic name, English name, CLI name, macOS app).
pub const MOKTEB_APPS: &[(&str, &str, &str, &str, &str)] = &[
    ("kalima", "كلمة", "Kalima (documents)", "kalima-cli", "كلمة"),
    ("jadwal", "جدول", "Jadwal (spreadsheets)", "jadwal-cli", "جدول"),
    ("ard", "عرض", "Ard (presentations)", "ard-cli", "عرض"),
    ("wathiqa", "وثيقة", "Wathiqa (PDF)", "wathiqa-cli", "وثيقة"),
];

/// The مكتب apps installed on this computer, as MCP servers.
pub fn find_mokteb_apps(arabic: bool) -> Vec<McpServer> {
    let mut out = Vec::new();
    for (id, ar, en, cli, mac) in MOKTEB_APPS {
        if let Some(path) = find_cli(cli, mac) {
            out.push(McpServer {
                id: (*id).into(),
                name: if arabic { (*ar).into() } else { (*en).into() },
                command: path.to_string_lossy().into_owned(),
                args: vec!["mcp".into()],
                trusted: false,
            });
        }
    }
    out
}

fn find_cli(cli: &str, mac_app: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) { format!("{cli}.exe") } else { cli.to_string() };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        candidates.push(PathBuf::from(format!("/Applications/{mac_app}.app/Contents/MacOS/{cli}")));
        if let Some(h) = dirs::home_dir() {
            candidates.push(h.join(format!("Applications/{mac_app}.app/Contents/MacOS/{cli}")));
        }
    }
    // The portable downloads unzip to a folder per app: look beside Agent AI's own folder.
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().and_then(|p| p.parent()).map(PathBuf::from)) {
        for folder in ["Kalima", "Jadwal", "Ard", "Wathiqa"] {
            candidates.push(dir.join(folder).join(&exe));
        }
    }
    if let Some(h) = dirs::home_dir() {
        for d in ["Downloads", "Desktop", "Applications", "Apps"] {
            for folder in ["Kalima", "Jadwal", "Ard", "Wathiqa"] {
                candidates.push(h.join(d).join(folder).join(&exe));
            }
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|d| d.join(&exe)));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// The tools one agent run can use.
pub struct Toolbox {
    /// The folder the file tools work in; `None` turns them off.
    pub folder: Option<PathBuf>,
    pub web: bool,
    servers: Vec<(McpServer, mcp::Client, Vec<ToolSpec>)>,
    /// Servers that failed to start, with why.
    pub failures: Vec<(String, String)>,
}

impl Toolbox {
    pub fn new(folder: Option<PathBuf>, web: bool) -> Self {
        Toolbox { folder, web, servers: Vec::new(), failures: Vec::new() }
    }

    /// Start `s` and add its tools; a server that fails is recorded in `failures`, not fatal.
    pub fn add_server(&mut self, s: McpServer) {
        match mcp::Client::start(&s.command, &s.args).and_then(|mut c| c.list_tools().map(|t| (c, t))) {
            Ok((c, tools)) => {
                let specs = tools
                    .into_iter()
                    .take(200)
                    .map(|t| ToolSpec { name: tool_name(&s.id, &t.name), description: clip(&t.description, 2000), schema: t.schema })
                    .collect();
                self.servers.push((s, c, specs));
            }
            Err(e) => self.failures.push((s.name.clone(), e.to_string())),
        }
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        let mut v = builtin::specs(self.folder.is_some(), self.web);
        for (_, _, specs) in &self.servers {
            v.extend(specs.iter().cloned());
        }
        v
    }

    /// Whether running `name` needs the person's OK first.
    pub fn needs_approval(&self, name: &str) -> bool {
        if let Some(b) = builtin::needs_approval(name) {
            return b;
        }
        !self.server_for(name).is_some_and(|(s, _)| s.trusted)
    }

    /// Who runs `name`, for the approval card: the app's name, or "" for a built-in tool.
    pub fn owner(&self, name: &str) -> String {
        self.server_for(name).map(|(s, _)| s.name.clone()).unwrap_or_default()
    }

    fn server_for(&self, name: &str) -> Option<(&McpServer, String)> {
        self.servers.iter().find_map(|(s, _, specs)| {
            specs.iter().any(|t| t.name == name).then(|| (s, name.strip_prefix(&format!("{}__", s.id)).unwrap_or(name).to_string()))
        })
    }

    /// Run tool `name` with `input`; the text goes back to the model.
    pub fn call(&mut self, name: &str, input: &Value) -> Result<String> {
        if let Some(bad) = input.get(INVALID_INPUT) {
            return Err(Error::Tool(format!("the tool input was not valid JSON: {bad}")));
        }
        if builtin::needs_approval(name).is_some() {
            return builtin::call(name, input, self.folder.as_deref(), self.web).map(|s| clip(&s, MAX_RESULT));
        }
        for (s, c, specs) in &mut self.servers {
            if specs.iter().any(|t| t.name == name) {
                let tool = name.strip_prefix(&format!("{}__", s.id)).unwrap_or(name).to_string();
                return c.call(&tool, input).map(|s| clip(&s, MAX_RESULT));
            }
        }
        Err(Error::Tool(format!("there is no tool named {name}")))
    }
}

/// A tool name the providers accept (letters, digits, `_` and `-`, at most 64), prefixed by its server.
fn tool_name(server: &str, tool: &str) -> String {
    let s: String = format!("{server}__{tool}").chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    s.chars().take(64).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_are_safe() {
        assert_eq!(tool_name("kalima", "text.insert"), "kalima__text_insert");
        assert_eq!(tool_name("x", &"a".repeat(100)).len(), 64);
    }

    #[test]
    fn invalid_input_is_reported_not_run() {
        let mut t = Toolbox::new(None, false);
        let e = t.call("fetch_url", &serde_json::json!({INVALID_INPUT: "{"})).unwrap_err();
        assert!(e.to_string().contains("not valid JSON"));
        assert!(t.call("nope", &serde_json::json!({})).is_err());
    }
}
