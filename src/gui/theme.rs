//! The look of the app: colors named after traditional Japanese pigments, and the egui style.

use crate::gui::fonts;
use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle, Theme, vec2,
};
use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DwmSetWindowAttribute,
};
use windows::core::BOOL;

/// Sumi ink: the window background.
pub const BG: Color32 = Color32::from_rgb(0x13, 0x14, 0x1A);
pub const CARD: Color32 = Color32::from_rgb(0x1B, 0x1D, 0x25);
pub const RAISED: Color32 = Color32::from_rgb(0x26, 0x29, 0x33);
pub const RAISED_HOVER: Color32 = Color32::from_rgb(0x31, 0x35, 0x41);
pub const INPUT: Color32 = Color32::from_rgb(0x10, 0x11, 0x16);
pub const BORDER: Color32 = Color32::from_rgb(0x2C, 0x2F, 0x3B);
/// Washi paper: the main text.
pub const TEXT: Color32 = Color32::from_rgb(0xEE, 0xE9, 0xDF);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0xA3, 0xA6, 0xB1);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x6C, 0x70, 0x7D);
/// Shu vermilion: the accent of seals and shrine gates.
pub const ACCENT: Color32 = Color32::from_rgb(0xE8, 0x60, 0x3C);
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0xF2, 0x74, 0x50);
pub const ACCENT_TEXT: Color32 = Color32::from_rgb(0xFF, 0xF6, 0xF0);
pub const RUBY: Color32 = Color32::from_rgb(0xF0, 0xA8, 0x8E);
pub const OK: Color32 = Color32::from_rgb(0x82, 0xC9, 0xA6);
pub const ERROR: Color32 = Color32::from_rgb(0xF2, 0x80, 0x7E);
pub const ERROR_BG: Color32 = Color32::from_rgb(0x34, 0x1E, 0x22);
pub const POPUP: Color32 = Color32::from_rgb(0x2E, 0x31, 0x3D);

pub const RADIUS_CARD: u8 = 14;
pub const RADIUS_CONTROL: u8 = 9;

pub fn accent_soft() -> Color32 {
    Color32::from_rgba_unmultiplied(0xE8, 0x60, 0x3C, 46)
}

/// Blends two opaque colors; `t` of 0 gives `from` and 1 gives `to`.
pub fn mix(from: Color32, to: Color32, t: f32) -> Color32 {
    let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t.clamp(0.0, 1.0)).round() as u8;
    Color32::from_rgb(
        channel(from.r(), to.r()),
        channel(from.g(), to.g()),
        channel(from.b(), to.b()),
    )
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(fonts::SEMIBOLD.into()))
}

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(Theme::Dark);
    ctx.all_styles_mut(|style| {
        for (text_style, font) in [
            (TextStyle::Body, FontId::proportional(14.5)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Heading, semibold(19.0)),
            (TextStyle::Small, FontId::proportional(12.5)),
        ] {
            style.text_styles.insert(text_style, font);
        }

        let spacing = &mut style.spacing;
        spacing.item_spacing = vec2(8.0, 8.0);
        spacing.button_padding = vec2(13.0, 6.0);
        spacing.interact_size.y = 30.0;
        spacing.scroll.floating = true;
        spacing.scroll.bar_width = 8.0;

        let visuals = &mut style.visuals;
        visuals.dark_mode = true;
        visuals.panel_fill = BG;
        visuals.window_fill = POPUP;
        visuals.window_stroke = Stroke::new(1.0, BORDER);
        visuals.window_corner_radius = CornerRadius::same(RADIUS_CARD);
        visuals.menu_corner_radius = CornerRadius::same(RADIUS_CONTROL);
        visuals.extreme_bg_color = INPUT;
        visuals.text_edit_bg_color = Some(INPUT);
        visuals.faint_bg_color = CARD;
        visuals.weak_text_color = Some(TEXT_FAINT);
        visuals.hyperlink_color = RUBY;
        visuals.selection.bg_fill = Color32::from_rgb(0x7C, 0x3D, 0x2C);
        visuals.selection.stroke = Stroke::new(1.0, ACCENT);
        visuals.text_cursor.stroke = Stroke::new(2.0, ACCENT);
        visuals.popup_shadow = Shadow {
            offset: [0, 6],
            blur: 18,
            spread: 0,
            color: Color32::from_black_alpha(110),
        };

        let widgets = &mut visuals.widgets;
        let radius = CornerRadius::same(RADIUS_CONTROL);
        widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_MUTED);
        widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        widgets.noninteractive.bg_fill = CARD;
        widgets.noninteractive.weak_bg_fill = CARD;
        widgets.noninteractive.corner_radius = radius;

        for (state, fill, stroke, text) in [
            (&mut widgets.inactive, RAISED, Stroke::NONE, TEXT),
            (&mut widgets.hovered, RAISED_HOVER, Stroke::NONE, TEXT),
            (&mut widgets.active, RAISED_HOVER, Stroke::new(1.0, ACCENT), TEXT),
            (&mut widgets.open, RAISED, Stroke::NONE, TEXT),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.bg_stroke = stroke;
            state.fg_stroke = Stroke::new(1.0, text);
            state.corner_radius = radius;
            state.expansion = 0.0;
        }
    });
}

pub fn card_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(RADIUS_CARD)
        .inner_margin(Margin::same(16))
}

fn color_ref(color: Color32) -> COLORREF {
    COLORREF(color.r() as u32 | (color.g() as u32) << 8 | (color.b() as u32) << 16)
}

/// Makes the native title bar blend into the window. Keeps Windows' own buttons, snapping and
/// resizing, which a hand-drawn title bar would have to reimplement. Older Windows ignores this.
pub fn style_title_bar(window: isize) {
    let hwnd = HWND(window as *mut _);
    let set_color = |attribute, color: Color32| unsafe {
        let value = color_ref(color);
        let _ = DwmSetWindowAttribute(
            hwnd,
            attribute,
            (&raw const value).cast(),
            size_of::<COLORREF>() as u32,
        );
    };
    unsafe {
        let dark = BOOL::from(true);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&raw const dark).cast(),
            size_of::<BOOL>() as u32,
        );
    }
    set_color(DWMWA_CAPTION_COLOR, BG);
    set_color(DWMWA_BORDER_COLOR, BG);
    set_color(DWMWA_TEXT_COLOR, TEXT_MUTED);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_reaches_both_ends() {
        assert_eq!(mix(BG, TEXT, 0.0), BG);
        assert_eq!(mix(BG, TEXT, 1.0), TEXT);
        assert_eq!(mix(BG, TEXT, 7.0), TEXT);
    }

    #[test]
    fn color_ref_is_blue_green_red() {
        assert_eq!(color_ref(Color32::from_rgb(1, 2, 3)).0, 0x0003_0201);
    }
}
