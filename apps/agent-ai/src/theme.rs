//! Colours (the مكتب palette: blue, ink and gold), fonts and the egui style.

use std::sync::Arc;

use egui::{Color32, FontData, FontDefinitions, FontFamily, Visuals};

/// The colours every panel reads.
#[derive(Clone, Copy)]
pub struct Tokens {
    pub bg: Color32,
    pub panel: Color32,
    pub card: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub accent_text: Color32,
    pub gold: Color32,
    pub user_bubble: Color32,
    pub danger: Color32,
    pub ok: Color32,
}

pub const BLUE: Color32 = Color32::from_rgb(0x3B, 0x5B, 0xDB);
pub const INK: Color32 = Color32::from_rgb(0x1F, 0x2A, 0x5C);
pub const GOLD: Color32 = Color32::from_rgb(0xF5, 0xB8, 0x41);

pub fn tokens(dark: bool) -> Tokens {
    if dark {
        Tokens {
            bg: Color32::from_rgb(0x10, 0x15, 0x2B),
            panel: Color32::from_rgb(0x17, 0x1E, 0x3D),
            card: Color32::from_rgb(0x1F, 0x28, 0x4E),
            border: Color32::from_rgb(0x2E, 0x39, 0x66),
            text: Color32::from_rgb(0xE8, 0xEB, 0xF7),
            muted: Color32::from_rgb(0x9A, 0xA3, 0xC7),
            accent: Color32::from_rgb(0x5C, 0x7C, 0xFA),
            accent_text: Color32::WHITE,
            gold: GOLD,
            user_bubble: Color32::from_rgb(0x26, 0x35, 0x75),
            danger: Color32::from_rgb(0xFF, 0x6B, 0x6B),
            ok: Color32::from_rgb(0x51, 0xCF, 0x66),
        }
    } else {
        Tokens {
            bg: Color32::from_rgb(0xF6, 0xF7, 0xFC),
            panel: Color32::from_rgb(0xEC, 0xEF, 0xFA),
            card: Color32::WHITE,
            border: Color32::from_rgb(0xD5, 0xDA, 0xEE),
            text: INK,
            muted: Color32::from_rgb(0x5E, 0x67, 0x8F),
            accent: BLUE,
            accent_text: Color32::WHITE,
            gold: GOLD,
            user_bubble: Color32::from_rgb(0xE3, 0xE9, 0xFF),
            danger: Color32::from_rgb(0xC9, 0x2A, 0x2A),
            ok: Color32::from_rgb(0x2B, 0x8A, 0x3E),
        }
    }
}

/// Inter for Latin text and Noto Sans Arabic for Arabic and Urdu, before egui's own fonts
/// (which supply symbols and emoji).
pub fn fonts() -> FontDefinitions {
    let mut f = FontDefinitions::default();
    f.font_data.insert("Inter".into(), Arc::new(FontData::from_static(include_bytes!("../../../assets/fonts/Inter-Regular.ttf"))));
    f.font_data.insert("InterSemiBold".into(), Arc::new(FontData::from_static(include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf"))));
    for (name, bytes) in [
        ("Arabic", &include_bytes!("../../../assets/fonts/NotoSansArabic-Regular.ttf")[..]),
        ("ArabicSemiBold", &include_bytes!("../../../assets/fonts/NotoSansArabic-SemiBold.ttf")[..]),
    ] {
        // Arabic letters look small beside Latin ones at the same size.
        let mut d = FontData::from_static(bytes);
        d.tweak.scale = 1.12;
        f.font_data.insert(name.into(), Arc::new(d));
    }
    if let Some(p) = f.families.get_mut(&FontFamily::Proportional) {
        p.insert(0, "Inter".into());
        p.insert(1, "Arabic".into());
    }
    let mut bold = vec!["InterSemiBold".to_string(), "ArabicSemiBold".to_string()];
    if let Some(p) = f.families.get(&FontFamily::Proportional) {
        bold.extend(p.iter().cloned());
    }
    f.families.insert(FontFamily::Name("bold".into()), bold);
    if let Some(m) = f.families.get_mut(&FontFamily::Monospace) {
        m.push("Arabic".into());
    }
    f
}

pub fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

/// Apply the colours and spacing to `ctx`.
pub fn apply(ctx: &egui::Context, dark: bool, rtl: bool) {
    let t = tokens(dark);
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = t.bg;
    v.window_fill = t.card;
    v.extreme_bg_color = t.bg; // text fields stand out from the cards they sit on
    v.faint_bg_color = t.panel;
    v.override_text_color = Some(t.text);
    v.hyperlink_color = t.accent;
    v.selection.bg_fill = t.accent.gamma_multiply(0.35);
    v.selection.stroke.color = t.accent;
    v.widgets.noninteractive.bg_stroke.color = t.border;
    v.widgets.inactive.weak_bg_fill = t.panel;
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, t.border);
    v.widgets.inactive.bg_fill = t.panel;
    v.widgets.hovered.weak_bg_fill = t.border;
    v.widgets.active.weak_bg_fill = t.accent.gamma_multiply(0.4);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = 8.into();
    }
    v.window_corner_radius = 12.into();
    ctx.set_visuals(v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 8.0);
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
        s.spacing.interact_size.y = 30.0;
        s.set_right_to_left(rtl);
        for (style, size) in [
            (egui::TextStyle::Body, 15.0),
            (egui::TextStyle::Button, 14.5),
            (egui::TextStyle::Small, 12.0),
            (egui::TextStyle::Heading, 22.0),
            (egui::TextStyle::Monospace, 13.5),
        ] {
            if let Some(f) = s.text_styles.get_mut(&style) {
                f.size = size;
            }
        }
    });
}
