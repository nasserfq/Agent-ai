//! Settings and conversations, saved as JSON in the app's data folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::agent::{Agent, default_agents};
use crate::providers::{Provider, builtin_providers};
use crate::tools::McpServer;
use crate::types::{Message, Usage};

/// Where everything is saved; `AGENTAI_DATA_DIR` overrides it (tests, portable use).
pub fn data_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("AGENTAI_DATA_DIR") {
        return Some(PathBuf::from(d));
    }
    dirs::data_dir().map(|d| d.join("Agent AI"))
}

/// Write `bytes` to `path` through a temporary file, so a crash never leaves half a file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// Interface language code; empty follows the computer.
    #[serde(default)]
    pub language: String,
    /// "light", "dark" or empty (follow the computer).
    #[serde(default)]
    pub theme: String,
    /// The services added by the person (the built-in ones are always present).
    #[serde(default)]
    pub custom_providers: Vec<Provider>,
    #[serde(default)]
    pub default_provider: String,
    #[serde(default)]
    pub default_model: String,
    #[serde(default)]
    pub agents: Vec<Agent>,
    /// MCP servers added by the person; installed مكتب apps are found automatically.
    #[serde(default)]
    pub servers: Vec<McpServer>,
    /// Apps whose tools run without asking (by server id).
    #[serde(default)]
    pub trusted: Vec<String>,
    #[serde(default)]
    pub onboarded: bool,
}

impl Settings {
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join("settings.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        write_atomic(&dir.join("settings.json"), serde_json::to_string_pretty(self)?.as_bytes())
    }

    /// Fill in the starting agents the first time.
    pub fn ensure_agents(&mut self, arabic: bool) {
        if self.agents.is_empty() {
            self.agents = default_agents(arabic);
        }
    }

    pub fn providers(&self) -> Vec<Provider> {
        let mut v = builtin_providers();
        v.extend(self.custom_providers.iter().cloned().map(|mut p| {
            p.builtin = false;
            p
        }));
        v
    }

    pub fn provider(&self, id: &str) -> Option<Provider> {
        self.providers().into_iter().find(|p| p.id == id)
    }
}

/// One chat.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub agent: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    pub messages: Vec<Message>,
    /// Unix seconds.
    pub created: i64,
    pub updated: i64,
    #[serde(default)]
    pub usage: Usage,
    /// US dollars spent, where prices are known.
    #[serde(default)]
    pub cost: f64,
}

impl Conversation {
    pub fn new(agent: &str) -> Self {
        let now = chrono::Utc::now();
        Conversation {
            id: format!("{}-{:04x}", now.format("%Y%m%d%H%M%S%3f"), std::process::id() & 0xffff),
            title: String::new(),
            agent: agent.into(),
            provider: String::new(),
            model: String::new(),
            messages: Vec::new(),
            created: now.timestamp(),
            updated: now.timestamp(),
            usage: Usage::default(),
            cost: 0.0,
        }
    }

    fn path(dir: &Path, id: &str) -> Option<PathBuf> {
        // Ids are made here, but files are input: only safe characters reach the path.
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return None;
        }
        Some(dir.join("chats").join(format!("{id}.json")))
    }

    pub fn save(&mut self, dir: &Path) -> Result<()> {
        self.updated = chrono::Utc::now().timestamp();
        if let Some(p) = Self::path(dir, &self.id) {
            write_atomic(&p, serde_json::to_string(self)?.as_bytes())?;
        }
        Ok(())
    }

    pub fn delete(dir: &Path, id: &str) {
        if let Some(p) = Self::path(dir, id) {
            let _ = std::fs::remove_file(p);
        }
    }

    /// Every saved chat, newest first.
    pub fn load_all(dir: &Path) -> Vec<Conversation> {
        let Ok(rd) = std::fs::read_dir(dir.join("chats")) else { return Vec::new() };
        let mut v: Vec<Conversation> = rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .take(5000)
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect();
        v.sort_by_key(|c| std::cmp::Reverse(c.updated));
        v
    }

    /// A title from the first thing the person wrote.
    pub fn auto_title(&mut self) {
        if self.title.is_empty()
            && let Some(m) = self.messages.iter().find(|m| m.role == crate::types::Role::User)
        {
            let t = m.text();
            let line = t.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
            self.title = crate::error::clip(line, 48);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_and_chats_round_trip() {
        let dir = std::env::temp_dir().join(format!("agentai-store-{}", std::process::id()));
        let mut s = Settings::default();
        s.ensure_agents(true);
        s.default_provider = "gemini".into();
        s.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir), s);
        let mut c = Conversation::new("assistant");
        c.messages.push(Message::user_text("  \nمرحبا يا وكيل، كيف حالك اليوم؟ هذا سؤال طويل جدا جدا جدا جدا"));
        c.auto_title();
        assert!(c.title.starts_with("مرحبا") && c.title.chars().count() <= 49);
        c.save(&dir).unwrap();
        let all = Conversation::load_all(&dir);
        assert_eq!(all[0].id, c.id);
        Conversation::delete(&dir, &c.id);
        assert!(Conversation::load_all(&dir).is_empty());
        assert!(Conversation::path(&dir, "../x").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
