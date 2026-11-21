use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontTweak};
use std::sync::Arc;

const MEIRYO_BASELINE_SHIFT: f32 = 0.1;

pub const SEMIBOLD: &str = "semibold";
pub const JAPANESE_DISPLAY: &str = "japanese-display";

/// egui's built-in fonts have no kanji, so Japanese comes from the fonts shipped with Windows.
const FACES: &[(&str, &str)] = &[
    ("segoe", r"C:\Windows\Fonts\segoeui.ttf"),
    ("segoe-semibold", r"C:\Windows\Fonts\seguisb.ttf"),
    ("meiryo", r"C:\Windows\Fonts\meiryo.ttc"),
    ("yu-gothic", r"C:\Windows\Fonts\YuGothM.ttc"),
    ("yu-mincho", r"C:\Windows\Fonts\yumindb.ttf"),
    ("ms-gothic", r"C:\Windows\Fonts\msgothic.ttc"),
];

const BODY_STACK: &[&str] = &["segoe", "meiryo", "yu-gothic", "ms-gothic"];
const SEMIBOLD_STACK: &[&str] = &["segoe-semibold", "segoe", "meiryo", "yu-gothic", "ms-gothic"];
const DISPLAY_STACK: &[&str] = &["yu-mincho", "meiryo", "yu-gothic", "ms-gothic", "segoe"];

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    for (name, path) in FACES {
        if let Ok(bytes) = std::fs::read(path) {
            let mut data = FontData::from_owned(bytes);
            if *name == "meiryo" {
                // Meiryo sits higher than Segoe UI, which shows in mixed Cyrillic and kana lines.
                data = data.tweak(FontTweak {
                    y_offset_factor: MEIRYO_BASELINE_SHIFT,
                    ..Default::default()
                });
            }
            fonts.font_data.insert((*name).to_owned(), Arc::new(data));
        }
    }

    // The defaults stay at the end of each stack for the icon glyphs they carry.
    let defaults = fonts.families[&FontFamily::Proportional].clone();
    let stack = |faces: &[&str]| -> Vec<String> {
        faces
            .iter()
            .filter(|face| fonts.font_data.contains_key(**face))
            .map(|face| (*face).to_owned())
            .chain(defaults.iter().cloned())
            .collect()
    };
    let families = [
        (FontFamily::Proportional, stack(BODY_STACK)),
        (FontFamily::Monospace, stack(BODY_STACK)),
        (FontFamily::Name(SEMIBOLD.into()), stack(SEMIBOLD_STACK)),
        (FontFamily::Name(JAPANESE_DISPLAY.into()), stack(DISPLAY_STACK)),
    ];
    fonts.families.extend(families);
    ctx.set_fonts(fonts);
}
