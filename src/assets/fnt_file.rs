//! Parser for RA2/YR `.fnt` bitmap font files (the "fonT" variant).
//!
//! Binary layout:
//!   - 4 bytes: magic `0x546E6F66` ("fonT")
//!   - 6 × u32: header fields
//!   - 128 KB: glyph lookup table (65536 × u16, codepoint → 1-based glyph index)
//!   - N bytes: glyph bitmap data (field[4] × field[5] bytes)
//!
//! Each glyph slot is `glyph_stride` bytes:
//!   - byte 0: glyph pixel width
//!   - remaining: 1-bit-per-pixel bitmap rows, MSB = leftmost pixel
//!
//! See docs/SIDEBAR_READY_TEXT_RENDERING.md for format details.

use std::collections::HashMap;

/// Magic value for the "fonT" variant used by GAME.FNT.
const FONT_MAGIC: u32 = 0x546E_6F66;
/// Size of the glyph lookup table in bytes (65536 entries × 2 bytes).
const LOOKUP_TABLE_BYTES: usize = 65536 * 2;
/// Header size in bytes (magic + 6 fields).
const HEADER_BYTES: usize = 4 + 6 * 4;

/// Parsed bitmap font from a `.fnt` file.
pub struct FntFile {
    /// Cell height for layout purposes (includes 1px line gap).
    pub cell_height: u32,
    /// Number of bitmap scanlines per glyph (cell_height - 1 typically).
    pub bitmap_rows: u32,
    /// Bytes per bitmap row per glyph (each byte = 8 pixels).
    pub bytes_per_row: u32,
    /// Bytes per glyph slot in the data (1 + bytes_per_row × bitmap_rows).
    pub glyph_stride: u32,
    /// Per-glyph data keyed by Unicode codepoint.
    glyphs: HashMap<u16, FntGlyph>,
}

/// A single decoded glyph.
pub struct FntGlyph {
    /// Width in pixels (variable per character).
    pub width: u32,
    /// RGBA bitmap: `width × bitmap_rows × 4` bytes.
    /// White (255,255,255,255) where bits are set, transparent elsewhere.
    pub rgba: Vec<u8>,
}

impl FntFile {
    /// Parse a `.fnt` file from raw bytes.
    pub fn from_bytes(data: &[u8]) -> Result<Self, crate::assets::error::AssetError> {
        let min_len = HEADER_BYTES + LOOKUP_TABLE_BYTES;
        if data.len() < min_len {
            return Err(crate::assets::error::AssetError::ParseError {
                format: "FNT".into(),
                detail: format!("file too small: {} < {}", data.len(), min_len),
            });
        }

        let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        if magic != FONT_MAGIC {
            return Err(crate::assets::error::AssetError::ParseError {
                format: "FNT".into(),
                detail: format!("bad magic: 0x{magic:08X}, expected 0x{FONT_MAGIC:08X}"),
            });
        }

        let field = |i: usize| -> u32 {
            let off = 4 + i * 4;
            u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
        };

        let bytes_per_row = field(1); // 3
        let bitmap_rows = field(2); // 16
        let cell_height = field(3); // 17
        let num_glyph_slots = field(4); // 29655
        let glyph_stride = field(5); // 49

        // Sanity checks.
        if bytes_per_row == 0 || bitmap_rows == 0 || glyph_stride == 0 {
            return Err(crate::assets::error::AssetError::ParseError {
                format: "FNT".into(),
                detail: "zero-valued header field".into(),
            });
        }
        let expected_stride = 1 + bytes_per_row * bitmap_rows;
        if glyph_stride != expected_stride {
            log::warn!(
                "FNT glyph_stride {} != expected {} (1 + {} × {}), using file value",
                glyph_stride,
                expected_stride,
                bytes_per_row,
                bitmap_rows,
            );
        }

        let bitmap_data_size = (num_glyph_slots as usize) * (glyph_stride as usize);
        let bitmap_data_offset = HEADER_BYTES + LOOKUP_TABLE_BYTES;
        if data.len() < bitmap_data_offset + bitmap_data_size {
            return Err(crate::assets::error::AssetError::ParseError {
                format: "FNT".into(),
                detail: format!(
                    "file too small for glyph data: {} < {}",
                    data.len(),
                    bitmap_data_offset + bitmap_data_size
                ),
            });
        }

        // Parse lookup table.
        let lookup_start = HEADER_BYTES;
        let bitmap_data = &data[bitmap_data_offset..];

        let mut glyphs = HashMap::new();
        for codepoint in 0u16..=u16::MAX {
            let lut_off = lookup_start + (codepoint as usize) * 2;
            let index = u16::from_le_bytes([data[lut_off], data[lut_off + 1]]) as u32;
            if index == 0 {
                continue;
            }
            let glyph_off = (glyph_stride as usize) * ((index - 1) as usize);
            if glyph_off + (glyph_stride as usize) > bitmap_data.len() {
                continue;
            }
            let glyph_bytes = &bitmap_data[glyph_off..glyph_off + glyph_stride as usize];
            let width = glyph_bytes[0] as u32;
            if width == 0 {
                continue;
            }

            let rgba = decode_glyph_bitmap(&glyph_bytes[1..], width, bitmap_rows, bytes_per_row);
            glyphs.insert(codepoint, FntGlyph { width, rgba });
        }

        log::info!(
            "Parsed GAME.FNT: {} glyphs, cell_height={}, bitmap_rows={}, bytes_per_row={}",
            glyphs.len(),
            cell_height,
            bitmap_rows,
            bytes_per_row,
        );

        Ok(Self {
            cell_height,
            bitmap_rows,
            bytes_per_row,
            glyph_stride,
            glyphs,
        })
    }

    /// Look up a glyph by Unicode codepoint.
    pub fn glyph(&self, codepoint: u16) -> Option<&FntGlyph> {
        self.glyphs.get(&codepoint)
    }

    /// Measure the pixel width of a text string.
    /// Uses char_advance = glyph_width + 1 (1px inter-character spacing).
    pub fn text_width(&self, text: &str) -> u32 {
        let mut width: u32 = 0;
        let mut count: u32 = 0;
        for ch in text.chars() {
            let cp = ch as u32;
            if cp > u16::MAX as u32 {
                continue;
            }
            if let Some(g) = self.glyphs.get(&(cp as u16)) {
                width += g.width;
                count += 1;
            }
        }
        // Inter-char spacing of 1px between each pair of glyphs.
        if count > 1 {
            width += count - 1;
        }
        width
    }
}

/// Decode 1-bit-per-pixel glyph bitmap rows into RGBA.
/// Each byte stores 8 pixels, MSB = leftmost pixel.
/// Output: width × bitmap_rows × 4 bytes (white on transparent).
fn decode_glyph_bitmap(bitmap: &[u8], width: u32, bitmap_rows: u32, bytes_per_row: u32) -> Vec<u8> {
    let mut rgba = vec![0u8; (width * bitmap_rows * 4) as usize];
    for row in 0..bitmap_rows {
        let row_start = (row * bytes_per_row) as usize;
        for px in 0..width {
            let byte_idx = row_start + (px / 8) as usize;
            let bit_idx = 7 - (px % 8); // MSB first
            if byte_idx < bitmap.len() && (bitmap[byte_idx] >> bit_idx) & 1 != 0 {
                let out = ((row * width + px) * 4) as usize;
                rgba[out] = 255;
                rgba[out + 1] = 255;
                rgba[out + 2] = 255;
                rgba[out + 3] = 255;
            }
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // Requires RA2_DIR env var pointing to retail game files
    fn parse_real_game_fnt() {
        use crate::assets::asset_manager::AssetManager;
        use std::path::Path;
        let dir = std::env::var("RA2_DIR").expect("Set RA2_DIR to your RA2/YR install directory");
        let ra2_dir = Path::new(&dir);
        let mut mgr = AssetManager::new(ra2_dir).expect("AssetManager");
        mgr.load_all_disk_mixes().ok();
        for name in [
            "local.mix",
            "localmd.mix",
            "cache.mix",
            "cachemd.mix",
            "conquer.mix",
            "conquermd.mix",
        ] {
            mgr.load_nested(name).ok();
        }
        let data = mgr.get("GAME.FNT").expect("GAME.FNT not found");
        let fnt = FntFile::from_bytes(&data).expect("parse failed");

        assert_eq!(fnt.cell_height, 17);
        assert_eq!(fnt.bitmap_rows, 16);
        assert_eq!(fnt.bytes_per_row, 3);
        assert_eq!(fnt.glyph_stride, 49);

        // 'R' (0x52) should exist and have a reasonable width.
        let r = fnt.glyph(b'R' as u16).expect("glyph 'R' missing");
        assert!(r.width >= 3 && r.width <= 20, "R width = {}", r.width);
        assert_eq!(
            r.rgba.len(),
            (r.width * fnt.bitmap_rows * 4) as usize,
            "RGBA size mismatch"
        );

        // Measure "Ready" — should be a reasonable width.
        let w = fnt.text_width("Ready");
        assert!(w >= 15 && w <= 60, "Ready width = {}", w);
    }

    #[test]
    fn decode_glyph_bitmap_basic() {
        // 2 pixels wide, 2 rows, 1 byte per row.
        // Row 0: 0b11000000 → pixels 0,1 set
        // Row 1: 0b01000000 → pixel 1 set
        let bitmap = [0b1100_0000, 0b0100_0000];
        let rgba = decode_glyph_bitmap(&bitmap, 2, 2, 1);
        assert_eq!(rgba.len(), 2 * 2 * 4);
        // Row 0: px0 = white, px1 = white
        assert_eq!(rgba[0..4], [255, 255, 255, 255]);
        assert_eq!(rgba[4..8], [255, 255, 255, 255]);
        // Row 1: px0 = transparent, px1 = white
        assert_eq!(rgba[8..12], [0, 0, 0, 0]);
        assert_eq!(rgba[12..16], [255, 255, 255, 255]);
    }

    #[test]
    fn text_width_empty() {
        let fnt = FntFile {
            cell_height: 17,
            bitmap_rows: 16,
            bytes_per_row: 3,
            glyph_stride: 49,
            glyphs: HashMap::new(),
        };
        assert_eq!(fnt.text_width(""), 0);
        assert_eq!(fnt.text_width("xyz"), 0); // no glyphs loaded
    }
}
