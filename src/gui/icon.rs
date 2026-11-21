//! Draws the app icon: a vermilion seal with the kanji 通 (from 通訳, "interpreter"), for the
//! window and, saved as .ico, for the desktop shortcut. Adapted from screen-translator.

use crate::gui::theme;
use anyhow::{Result, anyhow};
use eframe::egui::IconData;
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, CreateFontW,
    DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW,
    FW_BOLD, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::core::w;

const GLYPH: &str = "通";

/// Renders the kanji in white on black at `size` pixels; one channel of the result is its coverage.
fn glyph_mask(size: i32) -> Option<Vec<u8>> {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    unsafe {
        let dc = CreateCompatibleDC(None);
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        let previous_bitmap = SelectObject(dc, bitmap.into());
        let font = CreateFontW(
            size * 25 / 32,
            0,
            0,
            0,
            FW_BOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            Default::default(),
            Default::default(),
            ANTIALIASED_QUALITY,
            Default::default(),
            w!("Yu Gothic"),
        );
        let previous_font = SelectObject(dc, font.into());
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, COLORREF(0x00FF_FFFF));
        let mut text: Vec<u16> = GLYPH.encode_utf16().collect();
        let mut rect = RECT { left: 0, top: 0, right: size, bottom: size };
        DrawTextW(dc, &mut text, &mut rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE);

        let pixels = std::slice::from_raw_parts(bits.cast::<u8>(), (size * size * 4) as usize);
        let mask = pixels.chunks_exact(4).map(|pixel| pixel[1]).collect();

        SelectObject(dc, previous_font);
        let _ = DeleteObject(font.into());
        SelectObject(dc, previous_bitmap);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(dc);
        Some(mask)
    }
}

/// How much of the pixel at the given center lies inside the rounded square, from 0 to 1.
fn seal_coverage(x: f32, y: f32, size: f32) -> f32 {
    let corner = size * 14.0 / 64.0;
    let half = size / 2.0;
    let inner = half - corner;
    let dx = ((x - half).abs() - inner).max(0.0);
    let dy = ((y - half).abs() - inner).max(0.0);
    (corner - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0)
}

/// RGBA pixels of the seal, top row first.
fn render(size: i32) -> Option<Vec<u8>> {
    let mask = glyph_mask(size)?;
    let mut rgba = Vec::with_capacity(mask.len() * 4);
    for (index, coverage) in mask.iter().enumerate() {
        let x = (index as i32 % size) as f32 + 0.5;
        let y = (index as i32 / size) as f32 + 0.5;
        let color = theme::mix(theme::ACCENT, theme::ACCENT_TEXT, *coverage as f32 / 255.0);
        let alpha = (seal_coverage(x, y, size as f32) * 255.0).round() as u8;
        rgba.extend([color.r(), color.g(), color.b(), alpha]);
    }
    Some(rgba)
}

pub fn app_icon() -> Option<IconData> {
    Some(IconData { rgba: render(64)?, width: 64, height: 64 })
}

/// Writes a Windows .ico with 16-256 px images as 32-bit bitmaps (bottom-up BGRA plus an empty
/// AND mask), which every Windows version reads.
pub fn write_ico(path: &std::path::Path) -> Result<()> {
    const SIZES: [i32; 6] = [16, 24, 32, 48, 64, 256];
    let mut images = Vec::new();
    for size in SIZES {
        let rgba = render(size).ok_or_else(|| anyhow!("не вдалося намалювати іконку {size} px"))?;
        let mut data = Vec::new();
        let mask_row = ((size + 31) / 32 * 4) as usize;
        let header = [
            40u32.to_le_bytes(),
            (size as u32).to_le_bytes(),
            (size as u32 * 2).to_le_bytes(),
        ];
        for h in header {
            data.extend_from_slice(&h);
        }
        data.extend_from_slice(&1u16.to_le_bytes()); // planes
        data.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        data.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
        let image_size = (size * size * 4) as usize + mask_row * size as usize;
        data.extend_from_slice(&(image_size as u32).to_le_bytes());
        data.extend_from_slice(&[0u8; 16]); // resolution and palette fields
        for row in (0..size as usize).rev() {
            for px in rgba[row * size as usize * 4..(row + 1) * size as usize * 4].chunks_exact(4) {
                data.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
        }
        data.extend(std::iter::repeat_n(0u8, mask_row * size as usize));
        images.push((size, data));
    }

    let mut file = Vec::new();
    file.extend_from_slice(&0u16.to_le_bytes());
    file.extend_from_slice(&1u16.to_le_bytes()); // icon
    file.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len();
    for (size, data) in &images {
        let dim = if *size >= 256 { 0 } else { *size as u8 };
        file.extend_from_slice(&[dim, dim, 0, 0]);
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&32u16.to_le_bytes());
        file.extend_from_slice(&(data.len() as u32).to_le_bytes());
        file.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += data.len();
    }
    for (_, data) in &images {
        file.extend_from_slice(data);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, file)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_has_transparent_corners_and_solid_middle() {
        assert_eq!(seal_coverage(0.5, 0.5, 64.0), 0.0);
        assert_eq!(seal_coverage(32.0, 32.0, 64.0), 1.0);
        assert_eq!(seal_coverage(32.0, 0.5, 64.0), 1.0);
    }

    #[test]
    fn ico_has_every_size() {
        let path = std::env::temp_dir().join("li-icon-test.ico");
        write_ico(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..4], &[0, 0, 1, 0]);
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 6);
        std::fs::remove_file(&path).unwrap();
    }
}
