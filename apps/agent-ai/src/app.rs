//! The window: chats on the side, the conversation in the middle, settings and the first-run
//! welcome. Network work runs on background threads that report back through channels.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use agentai_core::agent::{Agent, system_prompt};
use agentai_core::i18n::{self, tr};
use agentai_core::providers::{self, ModelInfo, anthropic, gemini};
use agentai_core::run::{Approval, RunConfig, RunEvent, run};
use agentai_core::store::{Conversation, Settings, data_dir};
use agentai_core::tools::{McpServer, Toolbox, find_mokteb_apps};
use agentai_core::types::{Message, Part, Role, Stop};
use agentai_core::{Error, secrets};
use egui::{Align, Color32, FontId, Key, Layout, Modifiers, RichText, Ui};

use crate::markdown;
use crate::theme::{self, Tokens, bold};

pub enum Models {
    Loading,
    Ready(Vec<ModelInfo>),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Services,
    Agents,
    Apps,
    General,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Welcome,
    Chat,
    Settings(Tab),
}

enum Msg {
    Event(RunEvent),
    Failures(Vec<(String, String)>),
    Done { result: Result<(), String>, allowed: HashSet<String> },
}

struct Pending {
    name: String,
    owner: String,
    input: serde_json::Value,
    reply: Sender<Approval>,
}

struct Running {
    chat: String,
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    text: String,
    thinking: String,
    tools: Vec<(String, String, Option<bool>)>,
    approval: Option<Pending>,
}

pub struct ModelPicker {
    pub search: String,
    pub free_only: bool,
    /// Picking for the default model in Settings (otherwise for the open chat).
    pub for_default: bool,
}

pub struct App {
    pub dir: PathBuf,
    pub settings: Settings,
    pub lang: &'static str,
    pub dark: bool,
    pub chats: Vec<Conversation>,
    pub current: Option<usize>,
    input: String,
    attachments: Vec<Part>,
    run: Option<Running>,
    pub models: HashMap<String, Models>,
    models_tx: Sender<(String, Result<Vec<ModelInfo>, String>)>,
    models_rx: Receiver<(String, Result<Vec<ModelInfo>, String>)>,
    pub view: View,
    /// Key fields being edited, by service id.
    pub keys: HashMap<String, String>,
    pub notice: Option<(String, bool)>,
    allowed: HashMap<String, HashSet<String>>,
    pub picker: Option<ModelPicker>,
    pub apps: Vec<McpServer>,
    pub editing_agent: Option<usize>,
    pub welcome_choice: String,
    ctx: egui::Context,
}

impl App {
    pub fn new(ctx: &egui::Context) -> Self {
        Self::with_dir(ctx, data_dir().unwrap_or_else(|| std::env::temp_dir().join("Agent AI")))
    }

    /// The app with its settings and chats in `dir`.
    pub fn with_dir(ctx: &egui::Context, dir: PathBuf) -> Self {
        let mut settings = Settings::load(&dir);
        let lang = if settings.language.is_empty() {
            let locales: Vec<String> = sys_locale::get_locales().collect();
            i18n::pick(&locales)
        } else {
            i18n::lang(&settings.language).code
        };
        settings.ensure_agents(lang == "ar" || lang == "ur");
        let (models_tx, models_rx) = channel();
        let keys: HashMap<String, String> = settings.providers().iter().map(|p| (p.id.clone(), secrets::get(&p.id))).collect();
        let any_key = settings.providers().iter().any(|p| keys.get(&p.id).is_some_and(|k| !k.is_empty()));
        let view = if settings.onboarded || any_key { View::Chat } else { View::Welcome };
        let dark = match settings.theme.as_str() {
            "dark" => true,
            "light" => false,
            _ => ctx.global_style().visuals.dark_mode,
        };
        ctx.set_fonts(theme::fonts());
        let mut app = App {
            dir: dir.clone(),
            chats: Conversation::load_all(&dir),
            settings,
            lang,
            dark,
            current: None,
            input: String::new(),
            attachments: Vec::new(),
            run: None,
            models: HashMap::new(),
            models_tx,
            models_rx,
            view,
            keys,
            notice: None,
            allowed: HashMap::new(),
            picker: None,
            apps: find_mokteb_apps(lang == "ar" || lang == "ur"),
            editing_agent: None,
            welcome_choice: "gemini".into(),
            ctx: ctx.clone(),
        };
        app.apply_language();
        for p in app.settings.providers() {
            // Services with a key load their model lists in the background.
            let keyed = p.needs_key && app.keys.get(&p.id).is_some_and(|k| !k.is_empty());
            // A service added by the person without a key (a local server) loads too.
            if keyed || (!p.builtin && !p.needs_key) {
                app.load_models(&p.id);
            }
        }
        app
    }

    pub fn t(&self, s: &'static str) -> &'static str {
        tr(self.lang, s)
    }

    pub fn rtl(&self) -> bool {
        i18n::lang(self.lang).rtl
    }

    pub fn tokens(&self) -> Tokens {
        theme::tokens(self.dark)
    }

    pub fn apply_language(&mut self) {
        theme::apply(&self.ctx, self.dark, self.rtl());
        self.ctx.set_language(Some(self.lang.to_string()));
    }

    pub fn save_settings(&mut self) {
        if let Err(e) = self.settings.save(&self.dir) {
            self.notice = Some((format!("{}: {e}", self.t("Couldn't save settings")), true));
        }
    }

    // ---- models ----

    pub fn load_models(&mut self, provider: &str) {
        let Some(p) = self.settings.provider(provider) else { return };
        let key = self.keys.get(provider).cloned().unwrap_or_default();
        self.models.insert(provider.to_string(), Models::Loading);
        let tx = self.models_tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let r = providers::list_models(&p, &key).map_err(|e| e.to_string());
            let _ = tx.send((p.id.clone(), r));
            ctx.request_repaint();
        });
    }

    fn poll_models(&mut self) {
        while let Ok((id, r)) = self.models_rx.try_recv() {
            let m = match r {
                Ok(v) => Models::Ready(v),
                Err(e) if id == "anthropic" => {
                    // The model list needs a working key; the current models are known anyway.
                    self.notice = Some((format!("Claude: {e}"), true));
                    Models::Ready(anthropic::known_models())
                }
                Err(e) => Models::Failed(e),
            };
            self.models.insert(id, m);
        }
    }

    pub fn model_list(&self, provider: &str) -> &[ModelInfo] {
        match self.models.get(provider) {
            Some(Models::Ready(v)) => v,
            _ => &[],
        }
    }

    /// The provider and model a chat uses: the chat's own choice, its agent's, or the default.
    pub fn effective(&self, chat: Option<&Conversation>) -> (String, String) {
        let agent = chat.and_then(|c| self.agent(&c.agent));
        let pick = |p: &str, m: &str| (!p.is_empty()).then(|| (p.to_string(), m.to_string()));
        let (provider, model) = chat
            .and_then(|c| pick(&c.provider, &c.model))
            .or_else(|| agent.and_then(|a| pick(&a.provider, &a.model)))
            .or_else(|| pick(&self.settings.default_provider, &self.settings.default_model))
            .or_else(|| {
                ["gemini", "openrouter", "anthropic", "groq"]
                    .iter()
                    .find(|p| self.keys.get(**p).is_some_and(|k| !k.is_empty()))
                    .map(|p| ((*p).to_string(), String::new()))
            })
            .unwrap_or_default();
        let model = if model.is_empty() { self.default_model(&provider) } else { model };
        (provider, model)
    }

    fn default_model(&self, provider: &str) -> String {
        match provider {
            "anthropic" => anthropic::DEFAULT_MODEL.into(),
            "gemini" => gemini::DEFAULT_MODEL.into(),
            _ => {
                let list = self.model_list(provider);
                list.iter().find(|m| m.free).or_else(|| list.first()).map(|m| m.id.clone()).unwrap_or_default()
            }
        }
    }

    pub fn agent(&self, id: &str) -> Option<&Agent> {
        self.settings.agents.iter().find(|a| a.id == id)
    }

    fn price(&self, provider: &str, model: &str) -> Option<(f64, f64)> {
        self.model_list(provider).iter().find(|m| m.id == model).and_then(|m| m.price).or_else(|| {
            (provider == "anthropic").then(|| anthropic::known_models().into_iter().find(|m| m.id == model).and_then(|m| m.price)).flatten()
        })
    }

    pub fn model_label(&self, provider: &str, model: &str) -> String {
        let pname = self.settings.provider(provider).map(|p| p.name).unwrap_or_default();
        let mname = self.model_list(provider).iter().find(|m| m.id == model).map(|m| m.name.clone()).unwrap_or_else(|| model.to_string());
        if pname.is_empty() { self.t("Choose a model").to_string() } else { format!("{pname} · {mname}") }
    }

    // ---- chats ----

    pub fn new_chat(&mut self, agent: &str) {
        let c = Conversation::new(agent);
        self.chats.insert(0, c);
        self.current = Some(0);
        self.view = View::Chat;
    }

    fn current_chat(&self) -> Option<&Conversation> {
        self.current.and_then(|i| self.chats.get(i))
    }

    fn chat_index(&self, id: &str) -> Option<usize> {
        self.chats.iter().position(|c| c.id == id)
    }

    fn send(&mut self) {
        let text = self.input.trim().to_string();
        if (text.is_empty() && self.attachments.is_empty()) || self.run.is_some() {
            return;
        }
        if self.current.is_none() {
            let agent = self.settings.agents.first().map(|a| a.id.clone()).unwrap_or_default();
            self.new_chat(&agent);
        }
        let Some(idx) = self.current else { return };
        let (provider_id, model) = self.effective(self.chats.get(idx));
        let Some(provider) = self.settings.provider(&provider_id) else {
            self.notice = Some((self.t("Add an API key in Settings first.").into(), true));
            self.view = View::Settings(Tab::Services);
            return;
        };
        if model.is_empty() {
            self.notice = Some((self.t("Choose a model first.").into(), true));
            return;
        }
        let key = self.keys.get(&provider.id).cloned().unwrap_or_default();
        let mut parts = std::mem::take(&mut self.attachments);
        if !text.is_empty() {
            parts.push(Part::Text { text });
        }
        self.input.clear();
        let Some(chat) = self.chats.get_mut(idx) else { return };
        chat.messages.push(Message { role: Role::User, parts, raw: None, model: None });
        chat.auto_title();
        let _ = chat.save(&self.dir);
        let chat_id = chat.id.clone();
        let history = chat.messages.clone();
        let agent_id = chat.agent.clone();
        let agent = self.agent(&agent_id).cloned().or_else(|| self.settings.agents.first().cloned());
        let Some(agent) = agent else { return };
        let servers: Vec<McpServer> = self
            .apps
            .iter()
            .chain(self.settings.servers.iter())
            .filter(|s| agent.apps.contains(&s.id))
            .cloned()
            .map(|mut s| {
                s.trusted = self.settings.trusted.contains(&s.id);
                s
            })
            .collect();
        let app_names: Vec<String> = servers.iter().map(|s| s.name.clone()).collect();
        let cfg = RunConfig {
            price: self.price(&provider.id, &model),
            system: system_prompt(&agent, &chrono::Local::now().format("%A %Y-%m-%d").to_string(), i18n::lang(self.lang).english, &app_names),
            effort: agent.effort.clone(),
            provider,
            key,
            model,
        };
        let folder = (!agent.folder.trim().is_empty()).then(|| PathBuf::from(agent.folder.trim()));
        let web = agent.web;
        let mut allowed = self.allowed.get(&chat_id).cloned().unwrap_or_default();
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let mut tools = Toolbox::new(folder, web);
            for s in servers {
                tools.add_server(s);
            }
            if !tools.failures.is_empty() {
                let _ = tx.send(Msg::Failures(tools.failures.clone()));
            }
            let tx2 = tx.clone();
            let ctx2 = ctx.clone();
            let r = run(&cfg, &mut tools, history, &mut allowed, &c2, &mut |e| {
                let _ = tx2.send(Msg::Event(e));
                ctx2.request_repaint();
            });
            let result = match r {
                Ok(()) | Err(Error::Cancelled) => Ok(()),
                Err(e) => Err(e.to_string()),
            };
            let _ = tx.send(Msg::Done { result, allowed });
            ctx.request_repaint();
        });
        self.run = Some(Running { chat: chat_id, rx, cancel, text: String::new(), thinking: String::new(), tools: Vec::new(), approval: None });
    }

    fn poll_run(&mut self) {
        let Some(mut r) = self.run.take() else { return };
        let mut done = false;
        while let Ok(m) = r.rx.try_recv() {
            let idx = self.chat_index(&r.chat);
            match m {
                Msg::Event(RunEvent::Text(t)) => r.text.push_str(&t),
                Msg::Event(RunEvent::Thinking(t)) => {
                    if r.thinking.len() < 20_000 {
                        r.thinking.push_str(&t);
                    }
                }
                Msg::Event(RunEvent::Assistant(m)) | Msg::Event(RunEvent::ToolResults(m)) => {
                    r.text.clear();
                    r.thinking.clear();
                    if let Some(c) = idx.and_then(|i| self.chats.get_mut(i)) {
                        c.messages.push(m);
                    }
                }
                Msg::Event(RunEvent::ToolStarted { id, name }) => r.tools.push((id, name, None)),
                Msg::Event(RunEvent::ToolFinished { id, ok, .. }) => {
                    if let Some(t) = r.tools.iter_mut().find(|t| t.0 == id) {
                        t.2 = Some(ok);
                    }
                }
                Msg::Event(RunEvent::Approve { name, owner, input, reply }) => r.approval = Some(Pending { name, owner, input, reply }),
                Msg::Event(RunEvent::Usage { usage, cost }) => {
                    if let Some(c) = idx.and_then(|i| self.chats.get_mut(i)) {
                        c.usage.add(usage);
                        c.cost += cost.unwrap_or(0.0);
                    }
                }
                Msg::Event(RunEvent::Finished(stop)) => match stop {
                    Stop::Refused(why) => self.notice = Some((format!("{} {why}", self.t("The model declined to answer.")), true)),
                    Stop::MaxTokens => self.notice = Some((self.t("The reply was cut off because it got too long.").into(), true)),
                    Stop::Other(s) if s == "steps" => {
                        self.notice = Some((self.t("The agent stopped after many steps. Say “continue” to let it go on.").into(), false))
                    }
                    _ => {}
                },
                Msg::Failures(f) => {
                    let list: Vec<String> = f.iter().map(|(n, e)| format!("{n}: {e}")).collect();
                    self.notice = Some((format!("{}\n{}", self.t("Some apps couldn't start:"), list.join("\n")), true));
                }
                Msg::Done { result, allowed } => {
                    if let Err(e) = result {
                        self.notice = Some((friendly_error(self.lang, &e), true));
                    }
                    self.allowed.insert(r.chat.clone(), allowed);
                    if let Some(c) = idx.and_then(|i| self.chats.get_mut(i)) {
                        let _ = c.save(&self.dir);
                    }
                    done = true;
                }
            }
        }
        if !done {
            self.run = Some(r);
        }
    }

    fn attach(&mut self, paths: Vec<PathBuf>) {
        use base64::Engine;
        for p in paths {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            let mime = match ext.as_str() {
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "gif" => "image/gif",
                "webp" => "image/webp",
                "pdf" => "application/pdf",
                "txt" | "md" | "csv" | "tsv" | "json" | "xml" | "html" | "htm" | "rs" | "py" | "js" | "ts" | "css" | "yaml" | "yml" | "toml"
                | "log" | "srt" => "text/plain",
                _ => {
                    self.notice = Some((format!("{}: {name}", self.t("This kind of file can't be attached")), true));
                    continue;
                }
            };
            match std::fs::metadata(&p) {
                Ok(m) if m.len() > 20 << 20 => {
                    self.notice = Some((format!("{}: {name}", self.t("The file is larger than 20 MB")), true));
                    continue;
                }
                Err(e) => {
                    self.notice = Some((format!("{name}: {e}"), true));
                    continue;
                }
                _ => {}
            }
            match std::fs::read(&p) {
                Ok(bytes) => {
                    self.attachments.push(Part::File { name, mime: mime.into(), data: base64::engine::general_purpose::STANDARD.encode(bytes) })
                }
                Err(e) => self.notice = Some((format!("{name}: {e}"), true)),
            }
        }
    }

    // ---- drawing ----

    fn sidebar(&mut self, ui: &mut Ui) {
        let t = self.tokens();
        ui.add_space(10.0);
        row(self.rtl(), ui, |ui| {
            logo(ui, 30.0);
            ui.label(RichText::new(self.t("Agent AI")).font(FontId::new(19.0, bold())).color(t.text));
        });
        ui.add_space(8.0);
        let w = ui.available_width();
        if ui
            .add_sized([w, 34.0], egui::Button::new(RichText::new(format!("+  {}", self.t("New chat"))).color(t.accent_text)).fill(t.accent))
            .clicked()
        {
            let a = self.settings.agents.first().map(|a| a.id.clone()).unwrap_or_default();
            self.new_chat(&a);
        }
        ui.add_space(6.0);
        column(self.rtl(), ui, |ui| ui.label(RichText::new(self.t("Agents")).small().color(t.muted)));
        let agents: Vec<(String, String, String)> = self.settings.agents.iter().map(|a| (a.id.clone(), a.name.clone(), a.badge.clone())).collect();
        for (id, name, badge) in agents {
            let resp = row(self.rtl(), ui, |ui| {
                badge_icon(ui, &badge, &t);
                ui.add(egui::Button::new(RichText::new(&name).color(t.text)).frame(false))
            });
            if resp.clicked() {
                self.new_chat(&id);
            }
        }
        ui.add_space(6.0);
        ui.separator();
        column(self.rtl(), ui, |ui| ui.label(RichText::new(self.t("Chats")).small().color(t.muted)));
        let mut open = None;
        let mut delete = None;
        egui::ScrollArea::vertical().id_salt("chats").auto_shrink([false, false]).max_height(ui.available_height() - 44.0).show(ui, |ui| {
            for (i, c) in self.chats.iter().enumerate() {
                if c.messages.is_empty() && self.current != Some(i) {
                    continue;
                }
                let title = if c.title.is_empty() { self.t("New chat").to_string() } else { c.title.clone() };
                let selected = self.current == Some(i) && self.view == View::Chat;
                let w = ui.available_width();
                // The empty growing atom pushes the title to the start of the row.
                let text = RichText::new(title).color(t.text);
                let button = if self.rtl() {
                    egui::Button::selectable(selected, (egui::Atom::grow(), text))
                } else {
                    egui::Button::selectable(selected, (text, egui::Atom::grow()))
                };
                let r = ui.add(button.truncate().min_size(egui::vec2(w, 28.0)));
                if r.clicked() {
                    open = Some(i);
                }
                r.context_menu(|ui| {
                    if ui.button(self.t("Delete chat")).clicked() {
                        delete = Some(i);
                        ui.close();
                    }
                });
            }
        });
        if let Some(i) = open {
            self.current = Some(i);
            self.view = View::Chat;
        }
        if let Some(i) = delete {
            let busy = self.run.as_ref().is_some_and(|r| self.chats.get(i).is_some_and(|c| c.id == r.chat));
            if !busy && i < self.chats.len() {
                let c = self.chats.remove(i);
                Conversation::delete(&self.dir, &c.id);
                self.current = None;
            }
        }
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            let w = ui.available_width();
            if ui.add_sized([w, 30.0], egui::Button::new(format!("⚙  {}", self.t("Settings")))).clicked() {
                self.view = View::Settings(Tab::Services);
            }
        });
    }

    fn chat_view(&mut self, ui: &mut Ui) {
        let t = self.tokens();
        let chat = self.current_chat().cloned();
        let (provider, model) = self.effective(chat.as_ref());
        // Top bar: agent and model.
        row(self.rtl(), ui, |ui| {
            let agent_name = chat.as_ref().and_then(|c| self.agent(&c.agent)).map(|a| a.name.clone()).unwrap_or_default();
            ui.label(RichText::new(agent_name).font(FontId::new(17.0, bold())).color(t.text));
            if ui.button(format!("{}  ⏷", self.model_label(&provider, &model))).on_hover_text(self.t("Choose a model")).clicked() {
                self.picker = Some(ModelPicker { search: String::new(), free_only: false, for_default: false });
            }
            if let Some(c) = &chat
                && c.usage.input + c.usage.output > 0
            {
                let cost = if c.cost > 0.0 { format!(" · ${:.4}", c.cost) } else { String::new() };
                ui.label(RichText::new(format!("{} {}{cost}", c.usage.input + c.usage.output, self.t("tokens"))).small().color(t.muted));
            }
        });
        ui.separator();

        let input_height = 120.0 + if self.run.as_ref().is_some_and(|r| r.approval.is_some()) { 120.0 } else { 0.0 };
        let avail = (ui.available_height() - input_height).max(80.0);
        egui::ScrollArea::vertical().id_salt("messages").stick_to_bottom(true).auto_shrink([false, false]).max_height(avail).show(ui, |ui| {
            ui.add_space(8.0);
            match &chat {
                Some(c) if !c.messages.is_empty() => self.messages(ui, c),
                _ => self.empty_state(ui),
            }
            if let Some(r) = &self.run
                && chat.as_ref().is_some_and(|c| c.id == r.chat)
            {
                for (_, name, state) in &r.tools {
                    if state.is_none() {
                        row(self.rtl(), ui, |ui| {
                            ui.spinner();
                            ui.label(RichText::new(format!("{} {name}", self.t("Running"))).small().color(t.muted));
                        });
                    }
                }
                if !r.thinking.is_empty() && r.text.is_empty() {
                    let tail: String = r.thinking.chars().rev().take(300).collect::<Vec<_>>().into_iter().rev().collect();
                    column(self.rtl(), ui, |ui| ui.label(RichText::new(format!("💭 {tail}")).small().italics().color(t.muted)));
                }
                if !r.text.is_empty() {
                    markdown::show(ui, &r.text, &t, self.t("Copy"));
                } else if r.approval.is_none() {
                    row(self.rtl(), ui, |ui| {
                        ui.spinner();
                        ui.label(RichText::new(self.t("Thinking…")).color(t.muted));
                    });
                }
            }
            ui.add_space(8.0);
        });
        self.approval_card(ui);
        self.input_area(ui);
    }

    fn empty_state(&mut self, ui: &mut Ui) {
        let t = self.tokens();
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            logo(ui, 64.0);
            ui.add_space(8.0);
            ui.label(RichText::new(self.t("How can I help?")).font(FontId::new(24.0, bold())).color(t.text));
            ui.label(RichText::new(self.t("Pick an agent on the side, or just start typing.")).color(t.muted));
            if self.apps.is_empty() {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(self.t("Tip: install Kalima, Jadwal, Ard or Wathiqa from mokteb.com and the agents can work in them."))
                        .small()
                        .color(t.muted),
                );
            }
        });
    }

    fn messages(&self, ui: &mut Ui, c: &Conversation) {
        let t = self.tokens();
        let rtl = self.rtl();
        let width = ui.available_width();
        for m in &c.messages {
            match m.role {
                Role::User => {
                    let text = m.text();
                    let files: Vec<String> =
                        m.parts.iter().filter_map(|p| if let Part::File { name, .. } = p { Some(name.clone()) } else { None }).collect();
                    if text.is_empty() && files.is_empty() {
                        continue; // tool results: shown as chips under the call
                    }
                    // The person's messages sit at the reading end.
                    let l = if rtl { Layout::top_down(Align::Min) } else { Layout::top_down(Align::Max) };
                    ui.with_layout(l, |ui| {
                        egui::Frame::new().fill(t.user_bubble).corner_radius(14).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                            ui.set_max_width(width * 0.75);
                            for f in &files {
                                ui.label(RichText::new(format!("📎 {f}")).small().color(t.muted));
                            }
                            if !text.is_empty() {
                                ui.label(RichText::new(text).color(t.text));
                            }
                        });
                    });
                    ui.add_space(6.0);
                }
                Role::Assistant => {
                    let text = m.text();
                    if !text.is_empty() {
                        markdown::show(ui, &text, &t, self.t("Copy"));
                    }
                    for (_, name, _) in m.tool_calls() {
                        let result = c.messages.iter().flat_map(|x| x.parts.iter()).find_map(|p| match p {
                            Part::ToolResult { name: n, is_error, .. } if n == &name => Some(*is_error),
                            _ => None,
                        });
                        let (mark, color) = match result {
                            Some(false) => ("✓", t.ok),
                            Some(true) => ("✗", t.danger),
                            None => ("…", t.muted),
                        };
                        row(self.rtl(), ui, |ui| {
                            egui::Frame::new().fill(t.panel).corner_radius(10).inner_margin(egui::Margin::symmetric(8, 3)).show(ui, |ui| {
                                ui.label(RichText::new(format!("🔧 {}  {mark}", pretty_tool(&name))).small().color(color));
                            });
                        });
                    }
                    if !text.is_empty() {
                        row(self.rtl(), ui, |ui| {
                            if ui.small_button(self.t("Copy")).clicked() {
                                ui.ctx().copy_text(text.clone());
                            }
                            if let Some(model) = &m.model {
                                ui.label(RichText::new(model).small().color(t.muted));
                            }
                        });
                    }
                    ui.add_space(10.0);
                }
            }
        }
    }

    fn approval_card(&mut self, ui: &mut Ui) {
        let t = self.tokens();
        let Some(r) = &mut self.run else { return };
        let Some(p) = &r.approval else { return };
        let mut answer = None;
        let lang = self.lang;
        egui::Frame::new().fill(t.card).stroke((1.5, t.gold)).corner_radius(12).inner_margin(12).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let who = if p.owner.is_empty() { tr(lang, "the agent") } else { p.owner.as_str() };
            ui.label(RichText::new(format!("{} · {who}: {}", tr(lang, "Permission needed"), pretty_tool(&p.name))).font(FontId::new(15.0, bold())));
            let preview = crate::app::input_preview(&p.input);
            egui::ScrollArea::vertical().max_height(60.0).show(ui, |ui| {
                ui.label(RichText::new(preview).monospace().small().color(t.muted));
            });
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new(tr(lang, "Allow")).color(t.accent_text)).fill(t.accent)).clicked() {
                    answer = Some(Approval::Yes);
                }
                if ui.button(tr(lang, "Always allow in this chat")).clicked() {
                    answer = Some(Approval::Always);
                }
                if ui.button(tr(lang, "Don't allow")).clicked() {
                    answer = Some(Approval::No);
                }
            });
        });
        if let Some(a) = answer
            && let Some(p) = r.approval.take()
        {
            let _ = p.reply.send(a);
        }
    }

    fn input_area(&mut self, ui: &mut Ui) {
        let t = self.tokens();
        if !self.attachments.is_empty() {
            let mut remove = None;
            row(self.rtl(), ui, |ui| {
                for (i, a) in self.attachments.iter().enumerate() {
                    if let Part::File { name, .. } = a
                        && ui.small_button(format!("📎 {name}  ×")).clicked()
                    {
                        remove = Some(i);
                    }
                }
            });
            if let Some(i) = remove
                && i < self.attachments.len()
            {
                self.attachments.remove(i);
            }
        }
        let running = self.run.is_some();
        egui::Frame::new().fill(t.card).stroke((1.0, t.border)).corner_radius(14).inner_margin(10).show(ui, |ui| {
            let send_now = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) && !running;
            // The text field comes first in reading order and Send at the end. The field's width is
            // worked out up front so no layout has to grow to fit it.
            // Measured outside the row: a horizontal row has unbounded room along its direction.
            let gap = ui.spacing().item_spacing.x;
            let field_w = (ui.available_width() - 86.0 - 34.0 - 3.0 * gap).max(80.0);
            row(self.rtl(), ui, |ui| {
                let hint = self.t("Write a message… (Shift+Enter for a new line)");
                let edit = egui::TextEdit::multiline(&mut self.input).hint_text(hint).desired_rows(2).frame(egui::Frame::NONE).desired_width(field_w);
                let r = ui.add(edit);
                if send_now && (r.has_focus() || r.lost_focus()) {
                    self.send();
                }
                if !running && self.input.is_empty() && !r.has_focus() && self.picker.is_none() {
                    r.request_focus();
                }
                if ui.add_sized([34.0, 34.0], egui::Button::new("📎")).on_hover_text(self.t("Attach files")).clicked()
                    && let Some(files) = rfd::FileDialog::new().pick_files()
                {
                    self.attach(files);
                }
                let label = if running { format!("■  {}", self.t("Stop")) } else { self.t("Send").to_string() };
                let send = ui.add_sized(
                    [86.0, 34.0],
                    egui::Button::new(RichText::new(label).color(t.accent_text)).fill(if running { t.danger } else { t.accent }),
                );
                if send.clicked() {
                    if let Some(r) = &self.run {
                        r.cancel.store(true, Ordering::Relaxed);
                        if let Some(r) = &mut self.run {
                            r.approval = None;
                        }
                    } else {
                        self.send();
                    }
                }
            });
        });
    }

    fn notice_bar(&mut self, ui: &mut Ui) {
        let t = self.tokens();
        let Some((msg, err)) = self.notice.clone() else { return };
        let mut close = false;
        egui::Frame::new().fill(if err { t.danger.gamma_multiply(0.15) } else { t.gold.gamma_multiply(0.2) }).corner_radius(8).inner_margin(8).show(
            ui,
            |ui| {
                ui.set_width(ui.available_width());
                row(self.rtl(), ui, |ui| {
                    if ui.small_button("×").clicked() {
                        close = true;
                    }
                    ui.label(RichText::new(msg).color(t.text));
                });
            },
        );
        if close {
            self.notice = None;
        }
    }

    fn model_picker(&mut self, ctx: &egui::Context) {
        let Some(mut picker) = self.picker.take() else { return };
        let mut open = true;
        let mut chosen: Option<(String, String)> = None;
        let t = self.tokens();
        let rtl = self.rtl();
        egui::Window::new(self.t("Choose a model")).open(&mut open).collapsible(false).default_size([460.0, 520.0]).show(ctx, |ui| {
            row(self.rtl(), ui, |ui| {
                ui.checkbox(&mut picker.free_only, self.t("Free only"));
                ui.add(egui::TextEdit::singleline(&mut picker.search).hint_text(self.t("Search")).desired_width(220.0));
            });
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let needle = picker.search.to_lowercase();
                for p in self.settings.providers() {
                    let has_key = !p.needs_key || self.keys.get(&p.id).is_some_and(|k| !k.is_empty());
                    if !has_key {
                        continue;
                    }
                    column(self.rtl(), ui, |ui| ui.label(RichText::new(&p.name).font(FontId::new(15.0, bold())).color(t.accent)));
                    match self.models.get(&p.id) {
                        None => {
                            if ui.button(self.t("Load models")).clicked() {
                                self.load_models(&p.id);
                            }
                        }
                        Some(Models::Loading) => {
                            ui.spinner();
                        }
                        Some(Models::Failed(e)) => {
                            ui.label(RichText::new(e).small().color(t.danger));
                            if ui.button(self.t("Try again")).clicked() {
                                self.load_models(&p.id);
                            }
                        }
                        Some(Models::Ready(list)) => {
                            for m in list
                                .iter()
                                .filter(|m| {
                                    (!picker.free_only || m.free)
                                        && (needle.is_empty() || m.name.to_lowercase().contains(&needle) || m.id.to_lowercase().contains(&needle))
                                })
                                .take(400)
                            {
                                let mut label = m.name.clone();
                                if m.free {
                                    label.push_str(&format!("   · {}", self.t("free")));
                                } else if let Some((i, o)) = m.price {
                                    label.push_str(&format!("   · ${i:.2} / ${o:.2}"));
                                }
                                let w = ui.available_width();
                                let atoms =
                                    if rtl { (egui::Atom::grow(), egui::Atom::from(label)) } else { (egui::Atom::from(label), egui::Atom::grow()) };
                                if ui.add(egui::Button::new(atoms).frame(false).min_size(egui::vec2(w, 24.0))).on_hover_text(&m.id).clicked() {
                                    chosen = Some((p.id.clone(), m.id.clone()));
                                }
                            }
                        }
                    }
                    ui.add_space(6.0);
                }
            });
        });
        if let Some((p, m)) = chosen {
            if picker.for_default {
                self.settings.default_provider = p;
                self.settings.default_model = m;
                self.save_settings();
            } else {
                if self.current.is_none() {
                    let a = self.settings.agents.first().map(|a| a.id.clone()).unwrap_or_default();
                    self.new_chat(&a);
                }
                if let Some(c) = self.current.and_then(|i| self.chats.get_mut(i)) {
                    c.provider = p.clone();
                    c.model = m.clone();
                    let _ = c.save(&self.dir);
                }
                // The latest choice also becomes the default for new chats.
                self.settings.default_provider = p;
                self.settings.default_model = m;
                self.save_settings();
            }
            return;
        }
        if open {
            self.picker = Some(picker);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

impl App {
    /// One frame of the whole window.
    pub fn draw(&mut self, ui: &mut Ui) {
        self.poll_models();
        self.poll_run();
        let dropped: Vec<PathBuf> = ui.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            self.attach(dropped);
        }
        let t = self.tokens();
        let ctx = ui.ctx().clone();
        if self.view != View::Welcome {
            let panel = if self.rtl() { egui::Panel::right("side") } else { egui::Panel::left("side") };
            panel
                .resizable(true)
                .default_size(250.0)
                .size_range(200.0..=380.0)
                .frame(egui::Frame::new().fill(t.panel).inner_margin(10))
                .show(ui, |ui| self.sidebar(ui));
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(t.bg).inner_margin(egui::Margin::symmetric(18, 12))).show(ui, |ui| {
            self.notice_bar(ui);
            match self.view {
                View::Welcome => crate::settings_ui::welcome(self, ui),
                View::Chat => self.chat_view(ui),
                View::Settings(tab) => crate::settings_ui::settings(self, ui, tab),
            }
        });
        self.model_picker(&ctx);
    }
}

/// A horizontal row that runs from the reading start (right in Arabic).
pub fn row<R>(rtl: bool, ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let l = if rtl { Layout::right_to_left(Align::Center) } else { Layout::left_to_right(Align::Center) };
    // As tall as one widget to start (like `ui.horizontal`), not the whole space left.
    let size = egui::vec2(ui.available_size_before_wrap().x, ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(size, l, add).inner
}

/// A column whose items sit at the reading start.
pub fn column<R>(rtl: bool, ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let l = Layout::top_down(if rtl { Align::Max } else { Align::Min });
    ui.with_layout(l, add).inner
}

/// A readable tool name: `kalima__text_insert` → `kalima › text insert`.
pub fn pretty_tool(name: &str) -> String {
    match name.split_once("__") {
        Some((app, tool)) => format!("{app} › {}", tool.replace('_', " ")),
        None => name.replace('_', " "),
    }
}

/// A short look at a tool's input for the approval card.
pub fn input_preview(v: &serde_json::Value) -> String {
    let s = serde_json::to_string_pretty(v).unwrap_or_default();
    agentai_core::error::clip(&s, 1200)
}

/// An error in words a person can act on.
pub fn friendly_error(lang: &str, e: &str) -> String {
    let hint = if e.contains("401") || e.contains("403") || e.to_lowercase().contains("api key") || e.contains("no API key") {
        tr(lang, "Check the API key in Settings.")
    } else if e.contains("429") {
        tr(lang, "Too many requests or no credit left. Wait a little, or try another model.")
    } else if e.starts_with("network") {
        tr(lang, "Check the internet connection.")
    } else {
        ""
    };
    format!("{e}\n{hint}").trim().to_string()
}

/// The app's mark: a blue rounded square with a gold dot (drawn, no image file).
pub fn logo(ui: &mut Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, size * 0.24, theme::BLUE);
    let c = rect.center();
    // A speech bubble with three dots: the agent at work.
    let bubble = egui::Rect::from_center_size(c - egui::vec2(0.0, size * 0.04), egui::vec2(size * 0.62, size * 0.44));
    p.rect_filled(bubble, size * 0.12, Color32::WHITE);
    let tail = vec![
        bubble.left_bottom() + egui::vec2(size * 0.14, -1.0),
        bubble.left_bottom() + egui::vec2(size * 0.30, -1.0),
        bubble.left_bottom() + egui::vec2(size * 0.10, size * 0.14),
    ];
    p.add(egui::Shape::convex_polygon(tail, Color32::WHITE, egui::Stroke::NONE));
    for (i, col) in [theme::INK, theme::INK, theme::GOLD].into_iter().enumerate() {
        let x = bubble.center().x + (i as f32 - 1.0) * size * 0.15;
        p.circle_filled(egui::pos2(x, bubble.center().y), size * 0.055, col);
    }
}

fn badge_icon(ui: &mut Ui, text: &str, t: &Tokens) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 12.0, t.accent.gamma_multiply(0.18));
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, FontId::new(13.0, bold()), t.accent);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_read_well() {
        assert_eq!(pretty_tool("kalima__text_insert"), "kalima › text insert");
        assert_eq!(pretty_tool("read_file"), "read file");
    }

    #[test]
    fn errors_get_hints() {
        assert!(friendly_error("en", "Claude returned 401: invalid x-api-key").contains("API key"));
        assert!(friendly_error("en", "x returned 429: slow down").contains("Too many"));
    }

    /// Every string the interface translates has an Arabic translation.
    #[test]
    fn every_interface_string_is_in_arabic() {
        let ar = i18n::parse(include_str!("../../../locales/ar.tsv"));
        let mut missing = Vec::new();
        for src in [include_str!("app.rs"), include_str!("settings_ui.rs")] {
            for call in [".t(\"", "tr(lang, \"", "tr(app.lang, \"", "tr(self.lang, \""] {
                for piece in src.split(call).skip(1) {
                    let Some(end) = piece.find('"') else { continue };
                    let s = &piece[..end];
                    if !s.is_empty() && !s.contains('\\') && !ar.contains_key(s) && !missing.contains(&s.to_string()) {
                        missing.push(s.to_string());
                    }
                }
            }
        }
        assert!(missing.is_empty(), "missing Arabic: {missing:#?}");
    }

    /// Draws every screen (with a reply that has a list, table and code, a tool call and a pending
    /// approval) in Arabic and English, light and dark, and checks that nothing grows without bound.
    /// egui's own layout checks panic in debug builds, so this catches layout bugs before people do.
    #[test]
    fn every_screen_lays_out() {
        let dir = std::env::temp_dir().join(format!("agent-ai-ui-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let reply = "## أهلًا\n\nنص **غامق** and English.\n\n- بند\n  - فرعي\n1. one\n\n| المدينة | السكان |\n|---|---|\n| الرياض | 7.6 |\n\n```python\nprint('hi')\n```\n> اقتباس\n\n---";
        for (lang, dark) in [("ar", true), ("ar", false), ("en", false), ("en", true), ("ur", false), ("fr", true)] {
            let ctx = egui::Context::default();
            let mut app = App::with_dir(&ctx, dir.clone());
            app.lang = i18n::lang(lang).code;
            app.dark = dark;
            app.apply_language();
            app.new_chat("assistant");
            if let Some(c) = app.current.and_then(|i| app.chats.get_mut(i)) {
                c.messages.push(Message::user_text("مرحبا hello"));
                let mut a = Message { role: Role::Assistant, parts: vec![Part::Text { text: reply.into() }], raw: None, model: Some("m".into()) };
                a.parts.push(Part::ToolCall { id: "1".into(), name: "kalima__text_insert".into(), input: serde_json::json!({"text": "x"}) });
                c.messages.push(a);
                c.messages.push(Message {
                    role: Role::User,
                    parts: vec![Part::ToolResult { id: "1".into(), name: "kalima__text_insert".into(), content: "ok".into(), is_error: false }],
                    raw: None,
                    model: None,
                });
            }
            let (reply_tx, _reply_rx) = channel();
            let (_tx, rx) = channel();
            let chat = app.current.and_then(|i| app.chats.get(i)).map(|c| c.id.clone()).unwrap_or_default();
            app.run = Some(Running {
                chat,
                rx,
                cancel: Arc::new(AtomicBool::new(false)),
                text: reply.into(),
                thinking: String::new(),
                tools: vec![("1".into(), "write file".into(), None)],
                approval: Some(Pending {
                    name: "write_file".into(),
                    owner: String::new(),
                    input: serde_json::json!({"path": "a.txt", "content": "مرحبا"}),
                    reply: reply_tx,
                }),
            });
            let views = [
                View::Welcome,
                View::Chat,
                View::Settings(Tab::Services),
                View::Settings(Tab::Agents),
                View::Settings(Tab::Apps),
                View::Settings(Tab::General),
            ];
            for view in views {
                app.view = view;
                app.editing_agent = if view == View::Settings(Tab::Agents) { Some(0) } else { None };
                app.picker =
                    if view == View::Chat { Some(ModelPicker { search: String::new(), free_only: false, for_default: false }) } else { None };
                for width in [1100.0, 640.0] {
                    let input = egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 700.0))),
                        ..Default::default()
                    };
                    let mut out = ctx.run_ui(input, |ui| {
                        app.draw(ui);
                        assert!(ui.min_rect().is_finite(), "{lang} {view:?} grew without bound");
                    });
                    out.textures_delta.clear();
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
