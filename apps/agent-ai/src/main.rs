//! Agent AI (وكيل): a light, native AI app for Claude, Gemini, OpenRouter and free models, with
//! agents that can use files, web pages and the مكتب apps. Runs on Macs from macOS 10.12, Windows
//! and Linux.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod app;
mod markdown;
mod settings_ui;
mod theme;

fn main() -> eframe::Result {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon/agent-ai-256.png")).ok();
    let mut viewport =
        egui::ViewportBuilder::default().with_title("Agent AI — وكيل").with_inner_size([1100.0, 760.0]).with_min_inner_size([640.0, 480.0]);
    if let Some(i) = icon {
        viewport = viewport.with_icon(std::sync::Arc::new(i));
    }
    let options = eframe::NativeOptions {
        viewport,
        // OpenGL works on every Mac back to macOS 10.12, including ones without Metal 2.
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native("Agent AI", options, Box::new(|cc| Ok(Box::new(app::App::new(&cc.egui_ctx)))))
}
