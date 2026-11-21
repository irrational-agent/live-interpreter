//! The window front end (eframe/egui), styled like screen-translator.

pub mod app;
pub mod fonts;
pub mod furigana;
pub mod icon;
pub mod theme;
pub mod widgets;

use eframe::egui;

pub fn run() -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Синхронний перекладач")
        .with_inner_size([1000.0, window_height()])
        .with_min_inner_size([720.0, 480.0]);
    if let Some(icon) = icon::app_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("live-interpreter", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}

/// The self-test can ask for a taller window to photograph the whole settings panel.
fn window_height() -> f32 {
    std::env::var("LIVE_INTERPRETER_WINDOW_HEIGHT").ok().and_then(|v| v.parse().ok()).unwrap_or(720.0)
}
