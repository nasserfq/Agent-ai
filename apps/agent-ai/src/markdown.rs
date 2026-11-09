//! A small Markdown view for replies: headings, paragraphs with bold, italic and code, lists,
//! quotes, code blocks with a copy button, and tables. Each paragraph sits on the side its own
//! language starts on (Arabic on the right, English on the left).

use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Color32, FontId, Layout, RichText, Ui};

use crate::theme::{Tokens, bold};

/// Whether `s` starts (at its first strong letter) right-to-left.
pub fn is_rtl(s: &str) -> bool {
    for c in s.chars() {
        let u = c as u32;
        if c == '\u{200F}' || (0x0590..=0x08FF).contains(&u) || (0xFB1D..=0xFDFF).contains(&u) || (0xFE70..=0xFEFF).contains(&u) {
            return true;
        }
        if c.is_alphabetic() {
            return false;
        }
    }
    false
}

enum Block {
    Heading(usize, String),
    Para(String),
    Item { marker: String, text: String, depth: usize },
    Quote(String),
    Code { lang: String, text: String },
    Table(Vec<Vec<String>>),
    Rule,
}

fn parse(src: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let lines: Vec<&str> = src.lines().collect();
    let mut i = 0;
    let mut para = String::new();
    let flush = |para: &mut String, out: &mut Vec<Block>| {
        if !para.trim().is_empty() {
            out.push(Block::Para(std::mem::take(para).trim().to_string()));
        }
        para.clear();
    };
    while let Some(line) = lines.get(i) {
        let t = line.trim_start();
        if let Some(lang) = t.strip_prefix("```") {
            flush(&mut para, &mut out);
            let mut code = Vec::new();
            i += 1;
            while let Some(l) = lines.get(i) {
                if l.trim_start().starts_with("```") {
                    break;
                }
                code.push(*l);
                i += 1;
            }
            out.push(Block::Code { lang: lang.trim().to_string(), text: code.join("\n") });
        } else if t.is_empty() {
            flush(&mut para, &mut out);
        } else if t.starts_with('#') && t.trim_start_matches('#').starts_with(' ') {
            flush(&mut para, &mut out);
            let level = t.chars().take_while(|c| *c == '#').count().min(6);
            out.push(Block::Heading(level, t.trim_start_matches('#').trim().to_string()));
        } else if t.starts_with('|') {
            flush(&mut para, &mut out);
            let mut rows = Vec::new();
            while let Some(l) = lines.get(i).map(|l| l.trim()) {
                if !l.starts_with('|') {
                    break;
                }
                let cells: Vec<String> = l.trim_matches('|').split('|').map(|c| c.trim().to_string()).collect();
                if !cells.iter().all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' '))) {
                    rows.push(cells);
                }
                i += 1;
            }
            out.push(Block::Table(rows));
            continue;
        } else if t == "---" || t == "***" || t == "___" {
            flush(&mut para, &mut out);
            out.push(Block::Rule);
        } else if let Some(q) = t.strip_prefix("> ").or_else(|| t.strip_prefix('>')) {
            flush(&mut para, &mut out);
            out.push(Block::Quote(q.to_string()));
        } else if let Some((marker, text)) = list_item(t) {
            flush(&mut para, &mut out);
            let depth = (line.len() - t.len()) / 2;
            out.push(Block::Item { marker, text, depth: depth.min(6) });
        } else {
            if !para.is_empty() {
                para.push('\n');
            }
            para.push_str(line.trim());
        }
        i += 1;
    }
    flush(&mut para, &mut out);
    out
}

fn list_item(t: &str) -> Option<(String, String)> {
    for b in ["- ", "* ", "+ ", "• "] {
        if let Some(r) = t.strip_prefix(b) {
            return Some(("•".into(), r.to_string()));
        }
    }
    let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
    if !digits.is_empty() && digits.len() < 4 {
        let rest = t.get(digits.len()..)?;
        if let Some(r) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some((format!("{digits}."), r.to_string()));
        }
    }
    None
}

/// `text` with `**bold**`, `*italic*` / `_italic_` and `` `code` `` as one layout job.
pub fn inline(text: &str, size: f32, color: Color32, t: &Tokens) -> LayoutJob {
    let mut job = LayoutJob::default();
    let normal = TextFormat { font_id: FontId::proportional(size), color, ..Default::default() };
    let mut fmt_bold = normal.clone();
    fmt_bold.font_id = FontId::new(size, bold());
    let mut fmt_italic = normal.clone();
    fmt_italic.italics = true;
    let mut fmt_code = normal.clone();
    fmt_code.font_id = FontId::monospace(size * 0.92);
    fmt_code.background = t.panel;
    let mut rest = text;
    while !rest.is_empty() {
        let next = ["**", "`", "*", "_"].iter().filter_map(|m| rest.find(m).map(|i| (i, *m))).min_by_key(|(i, m)| (*i, usize::MAX - m.len()));
        let Some((start, mark)) = next else {
            job.append(rest, 0.0, normal.clone());
            break;
        };
        let after = rest.get(start + mark.len()..).unwrap_or("");
        // `_` inside a word (snake_case) is not emphasis.
        let word_inside = mark == "_" && rest.get(..start).and_then(|b| b.chars().last()).is_some_and(char::is_alphanumeric);
        match after.find(mark).filter(|e| *e > 0 && !word_inside) {
            Some(end) => {
                job.append(rest.get(..start).unwrap_or(""), 0.0, normal.clone());
                let inner = after.get(..end).unwrap_or("");
                let f = match mark {
                    "**" => fmt_bold.clone(),
                    "`" => fmt_code.clone(),
                    _ => fmt_italic.clone(),
                };
                job.append(inner, 0.0, f);
                rest = after.get(end + mark.len()..).unwrap_or("");
            }
            None => {
                job.append(rest.get(..start + mark.len()).unwrap_or(rest), 0.0, normal.clone());
                rest = after;
            }
        }
    }
    job
}

/// Lay `ui`'s next paragraph out on the side its language starts on.
fn side<R>(ui: &mut Ui, text: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    let align = if is_rtl(text) { Align::Max } else { Align::Min };
    ui.with_layout(Layout::top_down(align), add).inner
}

pub fn show(ui: &mut Ui, src: &str, t: &Tokens, copy_label: &str) {
    let size = 15.0;
    for b in parse(src) {
        match b {
            Block::Heading(level, text) => {
                let sz = match level {
                    1 => 22.0,
                    2 => 19.0,
                    _ => 16.5,
                };
                side(ui, &text, |ui| ui.label(RichText::new(text.clone()).font(FontId::new(sz, bold())).color(t.text)));
            }
            Block::Para(text) => {
                side(ui, &text, |ui| ui.label(inline(&text, size, t.text, t)));
            }
            Block::Quote(text) => {
                side(ui, &text, |ui| {
                    egui::Frame::new().fill(t.panel).inner_margin(8).corner_radius(6).show(ui, |ui| ui.label(inline(&text, size, t.muted, t)))
                });
            }
            Block::Item { marker, text, depth } => {
                let rtl = is_rtl(&text);
                let layout = if rtl { Layout::right_to_left(Align::TOP) } else { Layout::left_to_right(Align::TOP) };
                // A horizontal row has unbounded room along its direction, so the text gets an explicit
                // width (what is left after the indent and marker) and wraps inside it.
                let full = ui.available_width();
                ui.allocate_ui_with_layout(egui::vec2(full, 0.0), layout, |ui| {
                    let indent = depth as f32 * 18.0 + 4.0;
                    ui.add_space(indent);
                    let m = ui.label(RichText::new(marker).color(t.accent));
                    let w = (full - indent - m.rect.width() - ui.spacing().item_spacing.x * 2.0).max(40.0);
                    ui.allocate_ui_with_layout(egui::vec2(w, 0.0), Layout::top_down(if rtl { Align::Max } else { Align::Min }), |ui| {
                        ui.set_width(w);
                        ui.label(inline(&text, size, t.text, t))
                    });
                });
            }
            Block::Code { lang, text } => {
                egui::Frame::new().fill(t.panel).stroke((1.0, t.border)).inner_margin(10).corner_radius(8).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    // Code reads left to right in every language: the language name at the start, Copy at the end.
                    let h = ui.spacing().interact_size.y;
                    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), h), Layout::left_to_right(Align::Center), |ui| {
                        ui.label(RichText::new(lang).small().color(t.muted));
                        let button = egui::Button::new(RichText::new(copy_label).small());
                        let w = ui.fonts_mut(|f| f.layout_no_wrap(copy_label.to_string(), FontId::proportional(12.0), Color32::WHITE).size().x)
                            + 2.0 * ui.spacing().button_padding.x;
                        ui.add_space((ui.available_width() - w - 2.0).max(0.0));
                        if ui.add(button).clicked() {
                            ui.ctx().copy_text(text.clone());
                        }
                    });
                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                        ui.label(RichText::new(text).monospace().color(t.text));
                    });
                });
            }
            Block::Table(rows) => {
                let rtl = rows.first().and_then(|r| r.first()).is_some_and(|c| is_rtl(c));
                let id = ui.next_auto_id();
                // An Arabic table sits on the right. egui grids only grow from the left, so the table is
                // pushed right by the room it left over last frame (its width is remembered by id).
                let full = ui.available_width();
                let width_id = id.with("width");
                let last: Option<f32> = ui.data(|d| d.get_temp(width_id));
                let h = ui.spacing().interact_size.y;
                ui.allocate_ui_with_layout(egui::vec2(full, h), Layout::left_to_right(Align::TOP), |ui| {
                    if rtl && let Some(w) = last {
                        ui.add_space((full - w).max(0.0));
                    }
                    let r = egui::Frame::new().stroke((1.0, t.border)).inner_margin(6).corner_radius(6).show(ui, |ui| {
                        egui::Grid::new(id).striped(true).spacing([14.0, 6.0]).show(ui, |ui| {
                            for (r, row) in rows.iter().enumerate() {
                                let cells: Vec<&String> = if rtl { row.iter().rev().collect() } else { row.iter().collect() };
                                for c in cells {
                                    let mut job = inline(c, size, t.text, t);
                                    if r == 0 {
                                        for s in &mut job.sections {
                                            s.format.font_id = FontId::new(size, bold());
                                        }
                                    }
                                    ui.label(job);
                                }
                                ui.end_row();
                            }
                        });
                    });
                    let w = r.response.rect.width();
                    if last != Some(w) {
                        ui.data_mut(|d| d.insert_temp(width_id, w));
                        ui.ctx().request_repaint();
                    }
                });
            }
            Block::Rule => {
                ui.separator();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_direction() {
        assert!(is_rtl("  ١٢ مرحبا hello"));
        assert!(!is_rtl("Hello مرحبا"));
        assert!(!is_rtl("123"));
        assert!(is_rtl("اردو"));
    }

    #[test]
    fn parses_blocks() {
        let b = parse("# عنوان\n\nفقرة **غامقة**\nسطر\n\n- أ\n  - ب\n1. one\n\n```rust\nfn x(){}\n```\n| a | b |\n|---|---|\n| 1 | 2 |\n> q\n---");
        let kinds: Vec<&str> = b
            .iter()
            .map(|b| match b {
                Block::Heading(..) => "h",
                Block::Para(_) => "p",
                Block::Item { .. } => "li",
                Block::Quote(_) => "q",
                Block::Code { .. } => "code",
                Block::Table(_) => "table",
                Block::Rule => "hr",
            })
            .collect();
        assert_eq!(kinds, ["h", "p", "li", "li", "li", "code", "table", "q", "hr"]);
        if let Block::Table(rows) = &b[6] {
            assert_eq!(rows.len(), 2, "the separator row is dropped");
        }
    }

    #[test]
    fn inline_never_panics_on_odd_markers() {
        let t = crate::theme::tokens(false);
        for s in ["**", "*a", "`", "a_b_c", "**x**y*z*`c`", "مرحبا **عالم**", "_", "***"] {
            let job = inline(s, 14.0, Color32::BLACK, &t);
            assert!(!job.text.is_empty() || s.is_empty());
        }
    }

    /// Every prefix of a reply (as it streams in) lays out without tripping egui's layout checks.
    #[test]
    fn streaming_reply_lays_out_in_both_directions() {
        let text = "## أهلًا بك!\n\nهذه **إجابة** Here is English too.\n\n- البند الأول\n\n| المدينة | السكان |\n|---|---|\n| الرياض | 7.6 |\n\n```python\nprint('hello')\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```\nx\n```";
        let t = crate::theme::tokens(false);
        for rtl in [false, true] {
            let ctx = egui::Context::default();
            ctx.all_styles_mut(|s| s.set_right_to_left(rtl));
            let ends: Vec<usize> = text.char_indices().map(|(i, _)| i).chain([text.len()]).collect();
            for end in ends {
                let part = &text[..end];
                let input =
                    egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 700.0))), ..Default::default() };
                let mut out = ctx.run_ui(input, |ui| {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        show(ui, part, &t, "Copy");
                        assert!(ui.min_rect().is_finite(), "{part:?} grew without bound: {:?}", ui.min_rect());
                    });
                });
                out.textures_delta.clear();
            }
        }
    }
}
