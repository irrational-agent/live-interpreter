//! Hand-drawn controls for the parts of the interface egui's stock widgets can't style.
//! Adapted from screen-translator so both apps look alike.

use crate::gui::theme;
use eframe::egui::{
    self, Align2, CornerRadius, CursorIcon, FontId, Pos2, Rect, Response, RichText, Sense, Stroke, Ui, pos2, vec2,
};
use std::time::Duration;

const HOVER_FADE_SECONDS: f32 = 0.12;
const ACTION_HEIGHT: f32 = 40.0;
const SKELETON_FRAME: Duration = Duration::from_millis(33);

/// A small caps-style caption above a block of content.
pub fn section_label(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).font(theme::semibold(11.5)).extra_letter_spacing(1.3).color(theme::TEXT_FAINT));
}

/// Muted explanatory text under a control.
pub fn hint(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(12.5).color(theme::TEXT_FAINT));
}

/// The main start/stop button: filled with the accent when it starts something.
pub fn primary_button(ui: &mut Ui, title: &str, primary: bool, enabled: bool) -> Response {
    let galley = ui.painter().layout_no_wrap(title.to_owned(), theme::semibold(14.5), egui::Color32::PLACEHOLDER);
    let size = vec2(galley.size().x + 44.0, ACTION_HEIGHT);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let hover = ui.ctx().animate_bool_with_time(response.id, response.hovered() && enabled, HOVER_FADE_SECONDS);
    let (fill, text) = if primary {
        (theme::mix(theme::ACCENT, theme::ACCENT_HOVER, hover), theme::ACCENT_TEXT)
    } else {
        (theme::mix(theme::RAISED, theme::RAISED_HOVER, hover), theme::TEXT)
    };
    let (fill, text) = if enabled { (fill, text) } else { (theme::RAISED, theme::TEXT_FAINT) };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same((ACTION_HEIGHT / 2.0) as u8), fill);
    painter.galley(rect.center() - galley.size() / 2.0, galley, text);
    if enabled { response.on_hover_cursor(CursorIcon::PointingHand) } else { response }
}

#[derive(Clone, Copy)]
pub enum Icon {
    Gear,
    Close,
}

/// Drawn by hand because the system fonts in the stack have no gear glyph.
fn paint_icon(painter: &egui::Painter, center: Pos2, icon: Icon) {
    let stroke = Stroke::new(1.8, theme::TEXT);
    match icon {
        Icon::Gear => {
            const TEETH: usize = 8;
            const RING_RADIUS: f32 = 6.2;
            const TOOTH_END: f32 = 9.4;
            painter.circle_stroke(center, RING_RADIUS, stroke);
            painter.circle_stroke(center, 2.2, stroke);
            for tooth in 0..TEETH {
                let angle = tooth as f32 * std::f32::consts::TAU / TEETH as f32;
                let direction = vec2(angle.cos(), angle.sin());
                painter.line_segment(
                    [center + direction * RING_RADIUS, center + direction * TOOTH_END],
                    Stroke::new(2.6, theme::TEXT),
                );
            }
        }
        Icon::Close => {
            const HALF: f32 = 6.0;
            painter.line_segment([center + vec2(-HALF, -HALF), center + vec2(HALF, HALF)], stroke);
            painter.line_segment([center + vec2(-HALF, HALF), center + vec2(HALF, -HALF)], stroke);
        }
    }
}

/// A round button holding a single icon.
pub fn icon_button(ui: &mut Ui, icon: Icon, hint: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ACTION_HEIGHT, ACTION_HEIGHT), Sense::click());
    let hover = ui.ctx().animate_bool_with_time(response.id, response.hovered(), HOVER_FADE_SECONDS);
    let painter = ui.painter();
    painter.circle_filled(rect.center(), ACTION_HEIGHT / 2.0, theme::mix(theme::RAISED, theme::RAISED_HOVER, hover));
    paint_icon(painter, rect.center(), icon);
    response.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(hint)
}

/// An on/off switch with a label. Marks the response as changed when flipped.
pub fn toggle(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    const TRACK: egui::Vec2 = vec2(36.0, 20.0);
    const KNOB_RADIUS: f32 = 7.0;
    const LABEL_GAP: f32 = 10.0;

    let wrap_width = ui.available_width() - TRACK.x - LABEL_GAP;
    let galley = ui.painter().layout(label.to_owned(), FontId::proportional(14.0), theme::TEXT, wrap_width);
    let size = vec2(ui.available_width(), TRACK.y.max(galley.size().y));
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }

    let progress = ui.ctx().animate_bool_with_time(response.id, *on, HOVER_FADE_SECONDS);
    let track = Rect::from_min_size(pos2(rect.left(), rect.top() + (galley.rows.first().map_or(TRACK.y, |r| r.rect().height()) - TRACK.y) / 2.0), TRACK);
    let painter = ui.painter();
    painter.rect_filled(track, CornerRadius::same((TRACK.y / 2.0) as u8), theme::mix(theme::RAISED_HOVER, theme::ACCENT, progress));
    let travel = TRACK.x - TRACK.y;
    painter.circle_filled(pos2(track.left() + TRACK.y / 2.0 + travel * progress, track.center().y), KNOB_RADIUS, theme::TEXT);
    painter.galley(pos2(track.right() + LABEL_GAP, rect.top()), galley, theme::TEXT);
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// Pulsing bars that stand in for text which is still being generated.
pub fn skeleton(ui: &mut Ui, bars: &[(f32, f32)]) {
    let time = ui.input(|input| input.time) as f32;
    let width = ui.available_width();
    for (index, (fraction, height)) in bars.iter().enumerate() {
        let (rect, _) = ui.allocate_exact_size(vec2(width * fraction, *height), Sense::hover());
        let phase = (time * 3.0 - index as f32 * 0.5).sin() * 0.5 + 0.5;
        ui.painter().rect_filled(rect, 6, theme::mix(theme::RAISED, theme::RAISED_HOVER, phase));
    }
    ui.ctx().request_repaint_after(SKELETON_FRAME);
}

/// A dot that pulses while something is live.
pub fn status_dot(ui: &mut Ui, color: egui::Color32, pulsing: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
    let painter = ui.painter();
    if pulsing {
        let time = ui.input(|input| input.time) as f32;
        let phase = (time * 2.2).sin() * 0.5 + 0.5;
        let halo = egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), (60.0 * phase) as u8);
        painter.circle_filled(rect.center(), 6.0, halo);
        ui.ctx().request_repaint_after(SKELETON_FRAME);
    }
    painter.circle_filled(rect.center(), 3.5, color);
}

/// A red strip for problems the user has to act on.
pub fn error_banner(ui: &mut Ui, message: &str) {
    egui::Frame::new()
        .fill(theme::ERROR_BG)
        .corner_radius(theme::RADIUS_CONTROL)
        .inner_margin(egui::Margin::symmetric(14, 9))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new(message).color(theme::ERROR));
        });
}

/// Text placed at the center of the remaining space.
pub fn centered_note(ui: &mut Ui, title: &str, body: &str) {
    let rect = ui.available_rect_before_wrap();
    let painter = ui.painter();
    painter.text(rect.center() - vec2(0.0, 14.0), Align2::CENTER_CENTER, title, theme::semibold(19.0), theme::TEXT_MUTED);
    painter.text(rect.center() + vec2(0.0, 16.0), Align2::CENTER_CENTER, body, FontId::proportional(14.0), theme::TEXT_FAINT);
}
