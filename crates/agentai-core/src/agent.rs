//! Agents: saved helpers, each with instructions, a model and the tools it may use.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    /// One or two letters shown in its badge.
    #[serde(default)]
    pub badge: String,
    pub instructions: String,
    /// The service and model; empty uses the default model from Settings.
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    /// "low", "medium" or "high".
    #[serde(default = "default_effort")]
    pub effort: String,
    /// The folder its file tools work in; empty turns them off.
    #[serde(default)]
    pub folder: String,
    #[serde(default)]
    pub web: bool,
    /// The MCP servers (by id) it may use: the مكتب apps and any added server.
    #[serde(default)]
    pub apps: Vec<String>,
}

fn default_effort() -> String {
    "medium".into()
}

impl Agent {
    fn preset(id: &str, name: &str, badge: &str, instructions: &str, web: bool, apps: &[&str]) -> Self {
        Agent {
            id: id.into(),
            name: name.into(),
            badge: badge.into(),
            instructions: instructions.into(),
            provider: String::new(),
            model: String::new(),
            effort: default_effort(),
            folder: String::new(),
            web,
            apps: apps.iter().map(|a| (*a).to_string()).collect(),
        }
    }
}

/// The agents a new install starts with, named in the interface language.
pub fn default_agents(arabic: bool) -> Vec<Agent> {
    let n = |ar: &'static str, en: &'static str| if arabic { ar } else { en };
    vec![
        Agent::preset(
            "assistant",
            n("مساعد", "Assistant"),
            n("م", "A"),
            "You are a helpful, friendly assistant. Answer clearly and briefly, in the language the person writes in.",
            true,
            &[],
        ),
        Agent::preset(
            "writer",
            n("كاتب", "Writer"),
            n("ك", "W"),
            "You are a writer and editor. Draft letters, articles, reports and messages in a clear, natural style in the \
             person's language. When the Kalima tools are available and the person wants a document, write it in Kalima \
             and save it where they ask.",
            false,
            &["kalima"],
        ),
        Agent::preset(
            "analyst",
            n("محلل بيانات", "Data analyst"),
            n("ح", "D"),
            "You are a careful data analyst. Work out numbers step by step and show the result in a table. When the Jadwal \
             tools are available, build the spreadsheet there with formulas the person can check.",
            false,
            &["jadwal"],
        ),
        Agent::preset(
            "presenter",
            n("مصمم عروض", "Presenter"),
            n("ع", "P"),
            "You plan and build presentations: a clear story, one idea per slide, short text. When the Ard tools are \
             available, build the slides there.",
            false,
            &["ard"],
        ),
        Agent::preset(
            "researcher",
            n("باحث", "Researcher"),
            n("ب", "R"),
            "You research questions by reading web pages with the fetch_url tool. Say where each fact came from, with \
             its address, and say plainly when sources disagree or you could not check something.",
            true,
            &[],
        ),
    ]
}

/// The system prompt for one run of `agent`.
pub fn system_prompt(agent: &Agent, today: &str, interface_language: &str, apps: &[String]) -> String {
    let mut s = agent.instructions.trim().to_string();
    s.push_str(&format!(
        "\n\nToday is {today}. The app's interface language is {interface_language}; reply in the language the person writes in. \
         Use Markdown for lists and tables."
    ));
    if !agent.folder.trim().is_empty() {
        s.push_str(
            "\nYou can list, read and write files in the person's chosen working folder with the file tools; the person approves every write.",
        );
    }
    if !apps.is_empty() {
        s.push_str(&format!("\nYou can use these apps through their tools: {}.", apps.join(", ")));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_have_unique_ids() {
        let a = default_agents(true);
        let mut ids: Vec<_> = a.iter().map(|x| x.id.clone()).collect();
        ids.dedup();
        assert_eq!(ids.len(), a.len());
        assert_eq!(a[0].name, "مساعد");
        assert_eq!(default_agents(false)[0].name, "Assistant");
    }

    #[test]
    fn prompt_mentions_tools() {
        let mut a = default_agents(false).remove(1);
        a.folder = "/tmp".into();
        let p = system_prompt(&a, "2026-11-09", "Arabic", &["Kalima".into()]);
        assert!(p.contains("2026-11-09") && p.contains("Kalima") && p.contains("working folder"));
    }
}
