//! Draws text with readings above the kanji. egui has no ruby text of its own, so every piece is
//! laid out and painted here. Adapted from screen-translator, without its word meanings.

use crate::ruby::Ruby;
use eframe::egui::{self, Color32, FontId, Galley, Pos2, Sense, Vec2};
use std::sync::Arc;

const PART_GAP: f32 = 1.0;
/// Japanese typography forbids starting a line with these.
const NO_LINE_START: &str = "。、！？）」』】〕・ー…,.!?)";

pub struct TextStyle {
    pub base: FontId,
    pub ruby: FontId,
    pub base_color: Color32,
    pub ruby_color: Color32,
    pub row_gap: f32,
}

struct Part {
    base: Arc<Galley>,
    ruby: Option<Arc<Galley>>,
    width: f32,
    baseline: f32,
}

/// The smallest unit of wrapping: one annotated word, or one character of plain CJK text.
struct Cell {
    part: Part,
    glue_to_previous: bool,
}

struct Row {
    cells: Vec<Cell>,
    ruby_height: f32,
    baseline: f32,
    height: f32,
}

/// Plain text breaks per character in CJK and per word elsewhere.
fn split_plain(text: &str) -> Vec<String> {
    let mut pieces: Vec<String> = Vec::new();
    let mut in_word = false;
    for c in text.chars() {
        let is_cjk = c >= '\u{2E80}';
        if !is_cjk && in_word {
            pieces.last_mut().unwrap().push(c);
        } else {
            pieces.push(c.to_string());
        }
        in_word = !is_cjk && !c.is_whitespace();
    }
    pieces
}

/// Where the baseline of a single-line galley lies below its top, so pieces from different fonts
/// share one baseline.
fn baseline_of(galley: &Galley) -> f32 {
    galley
        .rows
        .first()
        .and_then(|row| row.row.glyphs.first().map(|glyph| row.pos.y + glyph.pos.y))
        .unwrap_or_default()
}

fn build_part(painter: &egui::Painter, style: &TextStyle, text: &str, reading: &str) -> Part {
    let base = painter.layout_no_wrap(text.to_owned(), style.base.clone(), style.base_color);
    let ruby = (!reading.is_empty()).then(|| painter.layout_no_wrap(reading.to_owned(), style.ruby.clone(), style.ruby_color));
    let width = match &ruby {
        Some(ruby) => base.size().x.max(ruby.size().x) + PART_GAP * 2.0,
        None => base.size().x,
    };
    let baseline = baseline_of(&base);
    Part { base, ruby, width, baseline }
}

fn build_cells(painter: &egui::Painter, style: &TextStyle, parts: &[Ruby]) -> Vec<Cell> {
    let mut cells = Vec::new();
    for part in parts {
        if !part.reading.is_empty() {
            cells.push(Cell { part: build_part(painter, style, &part.text, &part.reading), glue_to_previous: false });
            continue;
        }
        for piece in split_plain(&part.text) {
            let glue = piece.chars().all(|c| NO_LINE_START.contains(c));
            cells.push(Cell { part: build_part(painter, style, &piece, ""), glue_to_previous: glue });
        }
    }
    cells
}

fn wrap(cells: Vec<Cell>, max_width: f32) -> Vec<Vec<Cell>> {
    let mut rows: Vec<Vec<Cell>> = vec![Vec::new()];
    let mut used = 0.0;
    for cell in cells {
        let overflows = used + cell.part.width > max_width;
        if overflows && !cell.glue_to_previous && !rows.last().unwrap().is_empty() {
            rows.push(Vec::new());
            used = 0.0;
        }
        used += cell.part.width;
        rows.last_mut().unwrap().push(cell);
    }
    rows
}

fn line_height(painter: &egui::Painter, font: &FontId) -> f32 {
    painter.layout_no_wrap("あ".to_owned(), font.clone(), Color32::PLACEHOLDER).size().y
}

/// Every row reserves room for readings, so text doesn't jump when furigana arrives a moment
/// after the translation.
fn measure_row(cells: Vec<Cell>, style: &TextStyle, ruby_height: f32) -> Row {
    let baseline = cells.iter().map(|c| c.part.baseline).fold(0.0, f32::max);
    let below = cells.iter().map(|c| c.part.base.size().y - c.part.baseline).fold(0.0, f32::max);
    let height = ruby_height + baseline + below + style.row_gap;
    Row { cells, ruby_height, baseline, height }
}

/// Lets a reading that is wider than its character hang over its neighbours instead of spreading
/// the characters apart, as Japanese and Chinese typesetting do: up to half a ruby character on
/// each side, and only into room the neighbour's own reading leaves free, so readings never collide.
fn allow_overhang(cells: &mut [Cell], style: &TextStyle) {
    let per_side = style.ruby.size * 0.5;
    // Free room above each cell, on each of its sides.
    let spare: Vec<f32> = cells
        .iter()
        .map(|c| match &c.part.ruby {
            Some(ruby) => ((c.part.base.size().x - ruby.size().x - PART_GAP * 2.0) / 2.0).clamp(0.0, per_side),
            None => per_side,
        })
        .collect();
    for i in 0..cells.len() {
        let Some(ruby) = &cells[i].part.ruby else { continue };
        let base = cells[i].part.base.size().x;
        let excess = ruby.size().x + PART_GAP * 2.0 - base;
        if excess <= 0.0 {
            continue;
        }
        let left = if i > 0 { spare[i - 1] } else { 0.0 };
        let right = if i + 1 < cells.len() { spare[i + 1] } else { 0.0 };
        cells[i].part.width = base + (excess - left - right).max(0.0);
    }
}

pub fn show(ui: &mut egui::Ui, parts: &[Ruby], style: &TextStyle) {
    let max_width = ui.available_width();
    let painter = ui.painter().clone();
    let ruby_height = line_height(&painter, &style.ruby);
    let mut cells = build_cells(&painter, style, parts);
    allow_overhang(&mut cells, style);
    let rows: Vec<Row> = wrap(cells, max_width)
        .into_iter()
        .map(|cells| measure_row(cells, style, ruby_height))
        .collect();
    let total = Vec2::new(max_width, rows.iter().map(|row| row.height).sum());
    let (rect, _) = ui.allocate_exact_size(total, Sense::hover());

    let mut y = rect.top();
    for row in rows {
        let mut x = rect.left();
        for cell in row.cells {
            let part = cell.part;
            let center = x + part.width / 2.0;
            if let Some(ruby) = part.ruby {
                painter.galley(Pos2::new(center - ruby.size().x / 2.0, y), ruby, style.ruby_color);
            }
            let position = Pos2::new(center - part.base.size().x / 2.0, y + row.ruby_height + row.baseline - part.baseline);
            painter.galley(position, part.base, style.base_color);
            x += part.width;
        }
        y += row.height;
    }
}

#[cfg(test)]
mod tests {
    use super::split_plain;

    #[test]
    fn splits_cjk_by_character_and_latin_by_word() {
        assert_eq!(split_plain("はい"), vec!["は", "い"]);
        assert_eq!(split_plain("VR Chat"), vec!["VR ", "Chat"]);
        assert_eq!(split_plain("OKです"), vec!["OK", "で", "す"]);
    }
}
