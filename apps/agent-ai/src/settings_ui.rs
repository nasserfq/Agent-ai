//! The first-run welcome and the Settings screens: services and keys, agents, apps, general.

use agentai_core::agent::Agent;
use agentai_core::i18n::{LANGS, tr};
use agentai_core::providers::{Kind, Provider};
use agentai_core::secrets;
use agentai_core::tools::McpServer;
use egui::{FontId, RichText, Ui};

use crate::app::{App, ModelPicker, Models, Tab, View, column, logo, row};
use crate::theme::bold;

/// The four ways to start, with what each costs.
const STARTS: &[(&str, &str)] = &[
    ("gemini", "Free: Google gives a free daily allowance. Get a key in two minutes with a Google account."),
    ("openrouter", "One key for hundreds of models, including many free ones."),
    ("anthropic", "Claude, paid per use. Best for writing, reasoning and agents."),
    ("ollama", "Free and private: models running on this computer (needs Ollama and a fast computer)."),
];

pub fn welcome(app: &mut App, ui: &mut Ui) {
    let t = app.tokens();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.set_max_width(620.0);
            ui.add_space(30.0);
            logo(ui, 72.0);
            ui.add_space(10.0);
            ui.label(RichText::new(app.t("Welcome to Agent AI")).font(FontId::new(26.0, bold())).color(t.text));
            ui.label(
                RichText::new(app.t("Chat with Claude, Gemini and hundreds of free models, and give them agents that work in your files and apps."))
                    .color(t.muted),
            );
            ui.add_space(12.0);
            language_choice(app, ui);
            ui.add_space(12.0);
            ui.label(RichText::new(app.t("How do you want to start?")).font(FontId::new(17.0, bold())).color(t.text));
            ui.add_space(4.0);
        });
        let providers = app.settings.providers();
        for (id, about) in STARTS {
            let Some(p) = providers.iter().find(|p| p.id == *id) else { continue };
            let selected = app.welcome_choice == *id;
            let stroke = if selected { (2.0, t.accent) } else { (1.0, t.border) };
            let r = egui::Frame::new().fill(t.card).stroke(stroke).corner_radius(12).inner_margin(14).show(ui, |ui| {
                ui.set_width(ui.available_width());
                column(app.rtl(), ui, |ui| {
                    ui.label(RichText::new(&p.name).font(FontId::new(16.0, bold())).color(t.text));
                    ui.label(RichText::new(app.t(about)).color(t.muted));
                    if selected {
                        ui.add_space(6.0);
                        key_row(app, ui, p);
                        ui.add_space(6.0);
                        let ready = !p.needs_key || app.keys.get(&p.id).is_some_and(|k| !k.trim().is_empty());
                        if ui.add_enabled(ready, egui::Button::new(RichText::new(app.t("Start")).color(t.accent_text)).fill(t.accent)).clicked() {
                            app.settings.default_provider = p.id.clone();
                            app.settings.default_model.clear();
                            app.settings.onboarded = true;
                            app.save_settings();
                            app.load_models(&p.id);
                            app.view = View::Chat;
                        }
                    }
                });
            });
            if r.response.interact(egui::Sense::click()).clicked() {
                app.welcome_choice = (*id).to_string();
            }
            ui.add_space(8.0);
        }
        ui.vertical_centered(|ui| {
            if ui.link(app.t("Skip for now")).clicked() {
                app.settings.onboarded = true;
                app.save_settings();
                app.view = View::Chat;
            }
        });
    });
}

fn language_choice(app: &mut App, ui: &mut Ui) {
    let mut chosen = None;
    row(app.rtl(), ui, |ui| {
        ui.label(app.t("Language"));
        let current = LANGS.iter().find(|l| l.code == app.lang).map(|l| l.native).unwrap_or("English");
        egui::ComboBox::from_id_salt("lang").selected_text(current).show_ui(ui, |ui| {
            for l in LANGS {
                if ui.selectable_label(l.code == app.lang, l.native).clicked() {
                    chosen = Some(l.code);
                }
            }
        });
    });
    if let Some(code) = chosen {
        app.lang = code;
        app.settings.language = code.to_string();
        app.apply_language();
        app.save_settings();
    }
}

/// The key field of one service, with its "get a key" link and a save button.
fn key_row(app: &mut App, ui: &mut Ui, p: &Provider) {
    let t = app.tokens();
    if !p.needs_key {
        row(app.rtl(), ui, |ui| {
            ui.label(RichText::new(app.t("No key needed.")).color(t.muted));
            if !p.key_url.is_empty() {
                ui.hyperlink_to(app.t("Download"), &p.key_url);
            }
        });
        return;
    }
    let mut save = false;
    row(app.rtl(), ui, |ui| {
        let key = app.keys.entry(p.id.clone()).or_default();
        let r = ui.add(egui::TextEdit::singleline(key).password(true).hint_text(tr(app.lang, "Paste the API key")).desired_width(260.0));
        if r.lost_focus() {
            save = true;
        }
        if ui.button(app.t("Save")).clicked() {
            save = true;
        }
        if !p.key_url.is_empty() {
            ui.hyperlink_to(app.t("Get a key"), &p.key_url);
        }
    });
    if save {
        let key = app.keys.get(&p.id).cloned().unwrap_or_default();
        if key.trim() != secrets::get(&p.id) {
            match secrets::set(&p.id, &key) {
                Ok(()) if key.trim().is_empty() => {
                    app.models.remove(&p.id);
                }
                Ok(()) => {
                    app.notice = Some((format!("{} {}", p.name, app.t("key saved.")), false));
                    app.load_models(&p.id);
                }
                Err(e) => app.notice = Some((format!("{}: {e}", app.t("Couldn't save the key")), true)),
            }
        }
    }
}

pub fn settings(app: &mut App, ui: &mut Ui, tab: Tab) {
    let t = app.tokens();
    row(app.rtl(), ui, |ui| {
        if ui.button(format!("{} {}", if app.rtl() { "›" } else { "‹" }, app.t("Back"))).clicked() {
            app.view = View::Chat;
        }
        ui.label(RichText::new(app.t("Settings")).font(FontId::new(20.0, bold())).color(t.text));
    });
    row(app.rtl(), ui, |ui| {
        for (tb, name) in [(Tab::Services, "AI services"), (Tab::Agents, "Agents"), (Tab::Apps, "Apps"), (Tab::General, "General")] {
            if ui.selectable_label(tab == tb, app.t(name)).clicked() {
                app.view = View::Settings(tb);
                app.editing_agent = None;
            }
        }
    });
    ui.separator();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match tab {
        Tab::Services => services(app, ui),
        Tab::Agents => agents(app, ui),
        Tab::Apps => apps(app, ui),
        Tab::General => general(app, ui),
    });
}

fn services(app: &mut App, ui: &mut Ui) {
    let t = app.tokens();
    row(app.rtl(), ui, |ui| {
        ui.label(RichText::new(app.t("Default model:")).color(t.muted));
        let (p, m) = app.effective(None);
        if ui.button(app.model_label(&p, &m)).clicked() {
            app.picker = Some(ModelPicker { search: String::new(), free_only: false, for_default: true });
        }
    });
    column(app.rtl(), ui, |ui| {
        ui.label(RichText::new(app.t("Your keys stay on this computer and are sent only to the service they belong to.")).small().color(t.muted))
    });
    ui.add_space(6.0);
    let mut remove = None;
    for p in app.settings.providers() {
        egui::Frame::new().fill(t.card).stroke((1.0, t.border)).corner_radius(10).inner_margin(12).show(ui, |ui| {
            ui.set_width(ui.available_width());
            column(app.rtl(), ui, |ui| {
                row(app.rtl(), ui, |ui| {
                    ui.label(RichText::new(&p.name).font(FontId::new(16.0, bold())).color(t.text));
                    let status = match app.models.get(&p.id) {
                        Some(Models::Ready(v)) => format!("✓ {} {}", v.len(), app.t("models")),
                        Some(Models::Loading) => app.t("Loading…").to_string(),
                        Some(Models::Failed(e)) => format!("✗ {e}"),
                        None => String::new(),
                    };
                    ui.label(RichText::new(status).small().color(t.muted));
                    if !p.builtin && ui.small_button(app.t("Remove")).clicked() {
                        remove = Some(p.id.clone());
                    }
                });
                ui.label(RichText::new(&p.base_url).small().color(t.muted));
                key_row(app, ui, &p);
                if (!p.needs_key || app.keys.get(&p.id).is_some_and(|k| !k.is_empty())) && ui.small_button(app.t("Load models")).clicked() {
                    app.load_models(&p.id);
                }
            });
        });
        ui.add_space(6.0);
    }
    if let Some(id) = remove {
        app.settings.custom_providers.retain(|p| p.id != id);
        let _ = secrets::set(&id, "");
        app.save_settings();
    }
    ui.add_space(6.0);
    add_service(app, ui);
}

fn add_service(app: &mut App, ui: &mut Ui) {
    let t = app.tokens();
    let id = egui::Id::unique("add_service");
    let (mut name, mut url): (String, String) = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    egui::Frame::new().stroke((1.0, t.border)).corner_radius(10).inner_margin(12).show(ui, |ui| {
        ui.set_width(ui.available_width());
        column(app.rtl(), ui, |ui| {
            ui.label(RichText::new(app.t("Add an OpenAI-compatible service")).font(FontId::new(15.0, bold())));
            ui.label(RichText::new(app.t("For example LM Studio, DeepSeek, Mistral or a company server.")).small().color(t.muted));
            row(app.rtl(), ui, |ui| {
                ui.add(egui::TextEdit::singleline(&mut name).hint_text(app.t("Name")).desired_width(140.0));
                ui.add(egui::TextEdit::singleline(&mut url).hint_text("https://…/v1").desired_width(260.0));
                if ui.add_enabled(!name.trim().is_empty() && url.trim().starts_with("http"), egui::Button::new(app.t("Add"))).clicked() {
                    let pid = format!("custom-{}", app.settings.custom_providers.len() + 1);
                    app.settings.custom_providers.push(Provider {
                        id: pid,
                        name: name.trim().to_string(),
                        kind: Kind::OpenAi,
                        base_url: url.trim().trim_end_matches('/').to_string(),
                        needs_key: !(url.contains("localhost") || url.contains("127.0.0.1")),
                        key_url: String::new(),
                        builtin: false,
                    });
                    app.save_settings();
                    name.clear();
                    url.clear();
                }
            });
        });
    });
    ui.data_mut(|d| d.insert_temp(id, (name, url)));
}

fn agents(app: &mut App, ui: &mut Ui) {
    let t = app.tokens();
    if let Some(i) = app.editing_agent {
        edit_agent(app, ui, i);
        return;
    }
    let mut edit = None;
    let mut delete = None;
    for (i, a) in app.settings.agents.iter().enumerate() {
        egui::Frame::new().fill(t.card).stroke((1.0, t.border)).corner_radius(10).inner_margin(10).show(ui, |ui| {
            ui.set_width(ui.available_width());
            row(app.rtl(), ui, |ui| {
                ui.label(RichText::new(&a.name).font(FontId::new(15.0, bold())));
                let first_line = a.instructions.lines().next().unwrap_or("");
                ui.label(RichText::new(agentai_core::error::clip(first_line, 70)).small().color(t.muted));
                if ui.small_button(app.t("Edit")).clicked() {
                    edit = Some(i);
                }
                if app.settings.agents.len() > 1 && ui.small_button(app.t("Delete")).clicked() {
                    delete = Some(i);
                }
            });
        });
        ui.add_space(4.0);
    }
    if ui.button(format!("+ {}", app.t("New agent"))).clicked() {
        let n = app.settings.agents.len() + 1;
        app.settings.agents.push(Agent {
            id: format!("agent-{}-{n}", chrono::Utc::now().timestamp()),
            name: app.t("New agent").into(),
            badge: "★".into(),
            instructions: String::new(),
            provider: String::new(),
            model: String::new(),
            effort: "medium".into(),
            folder: String::new(),
            web: false,
            apps: Vec::new(),
        });
        edit = Some(app.settings.agents.len() - 1);
    }
    if let Some(i) = delete
        && i < app.settings.agents.len()
    {
        app.settings.agents.remove(i);
        app.save_settings();
    }
    app.editing_agent = edit;
}

fn edit_agent(app: &mut App, ui: &mut Ui, i: usize) {
    let t = app.tokens();
    let lang = app.lang;
    let mut apps_list: Vec<McpServer> = app.apps.clone();
    apps_list.extend(app.settings.servers.iter().cloned());
    let providers = app.settings.providers();
    let model_lists: Vec<(String, Vec<(String, String)>)> =
        providers.iter().map(|p| (p.id.clone(), app.model_list(&p.id).iter().map(|m| (m.id.clone(), m.name.clone())).collect())).collect();
    let rtl = app.rtl();
    let Some(a) = app.settings.agents.get_mut(i) else {
        app.editing_agent = None;
        return;
    };
    let mut done = false;
    let col = egui::Layout::top_down(if rtl { egui::Align::Max } else { egui::Align::Min });
    ui.with_layout(col, |ui| {
        ui.label(tr(lang, "Name"));
        ui.text_edit_singleline(&mut a.name);
        ui.label(tr(lang, "Badge (one letter)"));
        ui.add(egui::TextEdit::singleline(&mut a.badge).char_limit(2).desired_width(40.0));
        ui.label(tr(lang, "Instructions"));
        ui.add(egui::TextEdit::multiline(&mut a.instructions).desired_rows(6).desired_width(f32::INFINITY));
        ui.label(tr(lang, "Model"));
        ui.horizontal(|ui| {
            let pname = providers.iter().find(|p| p.id == a.provider).map(|p| p.name.clone()).unwrap_or_else(|| tr(lang, "Default").to_string());
            egui::ComboBox::from_id_salt("agent_provider").selected_text(pname).show_ui(ui, |ui| {
                if ui.selectable_label(a.provider.is_empty(), tr(lang, "Default")).clicked() {
                    a.provider.clear();
                    a.model.clear();
                }
                for p in &providers {
                    if ui.selectable_label(a.provider == p.id, &p.name).clicked() {
                        a.provider = p.id.clone();
                        a.model.clear();
                    }
                }
            });
            if !a.provider.is_empty() {
                let list = model_lists.iter().find(|(p, _)| *p == a.provider).map(|(_, l)| l.as_slice()).unwrap_or(&[]);
                if list.is_empty() {
                    ui.add(egui::TextEdit::singleline(&mut a.model).hint_text(tr(lang, "model id")).desired_width(220.0));
                } else {
                    let mname = list.iter().find(|(id, _)| *id == a.model).map(|(_, n)| n.clone()).unwrap_or_else(|| a.model.clone());
                    egui::ComboBox::from_id_salt("agent_model").selected_text(mname).height(400.0).show_ui(ui, |ui| {
                        for (id, name) in list.iter().take(500) {
                            if ui.selectable_label(a.model == *id, name).clicked() {
                                a.model = id.clone();
                            }
                        }
                    });
                }
            }
        });
        ui.label(tr(lang, "Effort (Claude)"));
        ui.horizontal(|ui| {
            for (v, l) in [("low", "Quick"), ("medium", "Balanced"), ("high", "Thorough")] {
                ui.radio_value(&mut a.effort, v.to_string(), tr(lang, l));
                ui.add_space(10.0);
            }
        });
        ui.add_space(4.0);
        ui.label(RichText::new(tr(lang, "Tools")).font(FontId::new(15.0, bold())));
        ui.checkbox(&mut a.web, tr(lang, "Read web pages"));
        ui.label(tr(lang, "Read and write files in this folder"));
        ui.horizontal(|ui| {
            let label = if a.folder.is_empty() { tr(lang, "No working folder").to_string() } else { format!("\u{200E}{}", a.folder) };
            ui.label(RichText::new(label).color(t.muted));
            if ui.button(tr(lang, "Choose folder…")).clicked()
                && let Some(f) = rfd::FileDialog::new().pick_folder()
            {
                a.folder = f.to_string_lossy().into_owned();
            }
            if !a.folder.is_empty() && ui.small_button("×").clicked() {
                a.folder.clear();
            }
        });
        if apps_list.is_empty() {
            ui.label(
                RichText::new(tr(lang, "No apps found. Install Kalima, Jadwal, Ard or Wathiqa, or add an MCP server under Apps."))
                    .small()
                    .color(t.muted),
            );
        }
        for s in &apps_list {
            let mut on = a.apps.contains(&s.id);
            if ui.checkbox(&mut on, &s.name).changed() {
                if on {
                    a.apps.push(s.id.clone());
                } else {
                    a.apps.retain(|x| x != &s.id);
                }
            }
        }
        ui.add_space(8.0);
        if ui.button(tr(lang, "Done")).clicked() {
            done = true;
        }
    });
    if done {
        app.editing_agent = None;
        app.save_settings();
    }
}

fn apps(app: &mut App, ui: &mut Ui) {
    let t = app.tokens();
    column(app.rtl(), ui, |ui| {
        ui.label(RichText::new(app.t("Agents can work inside these apps. Turn an app on for an agent under Agents.")).color(t.muted));
    });
    ui.add_space(6.0);
    let mut toggle_trust = None;
    let mut remove = None;
    let list: Vec<(McpServer, bool)> =
        app.apps.iter().map(|s| (s.clone(), false)).chain(app.settings.servers.iter().map(|s| (s.clone(), true))).collect();
    if app.apps.is_empty() {
        column(app.rtl(), ui, |ui| {
            ui.label(RichText::new(app.t("None of the مكتب apps were found on this computer.")).color(t.muted));
            ui.hyperlink_to(app.t("Get them free at mokteb.com"), "https://mokteb.com");
        });
    }
    for (s, custom) in &list {
        egui::Frame::new().fill(t.card).stroke((1.0, t.border)).corner_radius(10).inner_margin(10).show(ui, |ui| {
            ui.set_width(ui.available_width());
            column(app.rtl(), ui, |ui| {
                row(app.rtl(), ui, |ui| {
                    ui.label(RichText::new(&s.name).font(FontId::new(15.0, bold())));
                    let mut trusted = app.settings.trusted.contains(&s.id);
                    if ui.checkbox(&mut trusted, app.t("Run without asking")).changed() {
                        toggle_trust = Some((s.id.clone(), trusted));
                    }
                    if *custom && ui.small_button(app.t("Remove")).clicked() {
                        remove = Some(s.id.clone());
                    }
                });
                ui.label(RichText::new(format!("{} {}", s.command, s.args.join(" "))).small().monospace().color(t.muted));
            });
        });
        ui.add_space(4.0);
    }
    if let Some((id, on)) = toggle_trust {
        app.settings.trusted.retain(|x| x != &id);
        if on {
            app.settings.trusted.push(id);
        }
        app.save_settings();
    }
    if let Some(id) = remove {
        app.settings.servers.retain(|s| s.id != id);
        app.save_settings();
    }
    ui.add_space(8.0);
    let id = egui::Id::unique("add_server");
    let (mut name, mut cmd): (String, String) = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    egui::Frame::new().stroke((1.0, t.border)).corner_radius(10).inner_margin(12).show(ui, |ui| {
        ui.set_width(ui.available_width());
        column(app.rtl(), ui, |ui| {
            ui.label(RichText::new(app.t("Add an MCP server")).font(FontId::new(15.0, bold())));
            ui.label(
                RichText::new(app.t("The command that starts it, for example: npx -y @modelcontextprotocol/server-filesystem ~/Documents"))
                    .small()
                    .color(t.muted),
            );
            row(app.rtl(), ui, |ui| {
                ui.add(egui::TextEdit::singleline(&mut name).hint_text(app.t("Name")).desired_width(140.0));
                ui.add(egui::TextEdit::singleline(&mut cmd).hint_text(app.t("Command")).desired_width(300.0));
                if ui.button(app.t("Browse…")).clicked()
                    && let Some(f) = rfd::FileDialog::new().pick_file()
                {
                    cmd = format!("\"{}\"", f.to_string_lossy());
                }
                if ui.add_enabled(!name.trim().is_empty() && !cmd.trim().is_empty(), egui::Button::new(app.t("Add"))).clicked() {
                    let mut words = split_command(&cmd);
                    if !words.is_empty() {
                        let command = words.remove(0);
                        let sid: String = name.trim().to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).take(16).collect();
                        let sid = if sid.is_empty() { format!("mcp{}", app.settings.servers.len() + 1) } else { sid };
                        app.settings.servers.push(McpServer { id: sid, name: name.trim().into(), command, args: words, trusted: false });
                        app.save_settings();
                        name.clear();
                        cmd.clear();
                    }
                }
            });
        });
    });
    ui.data_mut(|d| d.insert_temp(id, (name, cmd)));
    ui.add_space(6.0);
    if ui.button(app.t("Look for apps again")).clicked() {
        app.apps = agentai_core::tools::find_mokteb_apps(app.lang == "ar" || app.lang == "ur");
    }
}

/// Split a command line into words, keeping "quoted parts" together.
pub fn split_command(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => quote = Some(c),
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn general(app: &mut App, ui: &mut Ui) {
    let t = app.tokens();
    column(app.rtl(), ui, |ui| {
        language_choice(app, ui);
        ui.add_space(6.0);
        let mut theme = app.settings.theme.clone();
        row(app.rtl(), ui, |ui| {
            ui.label(app.t("Appearance"));
            for (v, l) in [("", "Automatic"), ("light", "Light"), ("dark", "Dark")] {
                ui.radio_value(&mut theme, v.to_string(), app.t(l));
                ui.add_space(10.0);
            }
        });
        if theme != app.settings.theme {
            app.settings.theme = theme;
            app.dark = match app.settings.theme.as_str() {
                "dark" => true,
                "light" => false,
                _ => ui.ctx().global_style().visuals.dark_mode,
            };
            app.apply_language();
            app.save_settings();
        }
        ui.add_space(12.0);
        ui.label(RichText::new(format!("Agent AI {}", env!("CARGO_PKG_VERSION"))).font(FontId::new(15.0, bold())));
        ui.label(RichText::new(app.t("Made by Nasser Alqahtani, part of the مكتب family of apps.")).color(t.muted));
        ui.hyperlink_to("mokteb.com", "https://mokteb.com");
        ui.label(RichText::new(format!("{}:", app.t("Your chats are saved in"))).small().color(t.muted));
        // A path reads left to right whatever the interface language.
        ui.label(RichText::new(format!("\u{200E}{}", app.dir.display())).small().monospace().color(t.muted));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_split_like_a_shell() {
        assert_eq!(split_command("npx -y \"a b\" c"), vec!["npx", "-y", "a b", "c"]);
        assert_eq!(split_command("  "), Vec::<String>::new());
        assert_eq!(split_command("'unterminated"), vec!["unterminated"]);
    }
}
