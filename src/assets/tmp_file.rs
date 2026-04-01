//! Parser for RA2 .tmp isometric terrain tile files.
//!
//! TMP files define terrain tile templates for RA2's isometric map grid.
//! Each template is a grid of cells containing diamond-shaped tiles (60×30 for RA2).
//! See `tmp_decode` for low-level diamond unpacking and extra-data overlay logic.
//!
//! ## Dependency rules
//! - Part of assets/ — no dependencies on game modules.

use crate::assets::error::AssetError;
use crate::assets::pal_file::Palette;
use crate::assets::tmp_decode;
use crate::util::read_helpers::read_u32_le;

/// Size of the TMP file header in bytes.
const TMP_HEADER_SIZE: usize = 16;

/// A parsed TMP terrain template containing one or more isometric tiles.
#[derive(Debug)]
pub struct TmpFile {
    pub template_width: u32,
    pub template_height: u32,
    pub tile_width: u32,
    pub tile_height: u32,
    /// Tile cells; None = empty cell in the template grid.
    pub tiles: Vec<Option<TmpTile>>,
}

/// A single tile cell from a TMP file.
///
/// Diamond pixels are unpacked into a rectangular buffer (index 0 = transparent
/// outside the diamond shape).
#[derive(Debug)]
pub struct TmpTile {
    pub height: u8,
    pub terrain_type: u8,
    pub ramp_type: u8,
    pub radar_left: [u8; 3],
    pub radar_right: [u8; 3],
    /// Palette indices, pixel_width × pixel_height. Index 0 = transparent.
    pub pixels: Vec<u8>,
    /// Depth buffer, same layout as pixels.
    pub depth: Vec<u8>,
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// Offset from tile diamond origin (non-zero when extra data extends beyond).
    pub offset_x: i32,
    pub offset_y: i32,
    /// True if tile variants are deterministic damaged states (bridges), not random
    /// visual diversity picks. From HasDamagedData flag (bit 2) in TileCellHeader.
    pub has_damaged_data: bool,
}

impl TmpFile {
    /// Parse a TMP file from raw bytes.
    pub fn from_bytes(data: &[u8]) -> Result<Self, AssetError> {
        if data.len() < TMP_HEADER_SIZE {
            return Err(AssetError::InvalidTmpFile {
                reason: format!(
                    "File too small for header: {} bytes (need {})",
                    data.len(),
                    TMP_HEADER_SIZE
                ),
            });
        }

        let template_width: u32 = read_u32_le(data, 0);
        let template_height: u32 = read_u32_le(data, 4);
        let tile_width: u32 = read_u32_le(data, 8);
        let tile_height: u32 = read_u32_le(data, 12);

        if template_width == 0 || template_height == 0 {
            return Err(AssetError::InvalidTmpFile {
                reason: format!(
                    "Template dimensions are zero: {}x{}",
                    template_width, template_height
                ),
            });
        }
        if tile_width < tmp_decode::DIAMOND_INITIAL_WIDTH || tile_height < 2 {
            return Err(AssetError::InvalidTmpFile {
                reason: format!(
                    "Tile dimensions too small: {}x{} (min {}x2)",
                    tile_width,
                    tile_height,
                    tmp_decode::DIAMOND_INITIAL_WIDTH
                ),
            });
        }

        let cell_count: usize = (template_width * template_height) as usize;
        let offsets_end: usize = TMP_HEADER_SIZE + cell_count * 4;
        if data.len() < offsets_end {
            return Err(AssetError::InvalidTmpFile {
                reason: format!(
                    "File too small for offset table: {} bytes (need {})",
                    data.len(),
                    offsets_end
                ),
            });
        }

        let mut offsets: Vec<u32> = Vec::with_capacity(cell_count);
        for i in 0..cell_count {
            offsets.push(read_u32_le(data, TMP_HEADER_SIZE + i * 4));
        }

        let mut tiles: Vec<Option<TmpTile>> = Vec::with_capacity(cell_count);
        for (i, &offset) in offsets.iter().enumerate() {
            if offset == 0 {
                tiles.push(None);
                continue;
            }
            let col: u32 = (i as u32) % template_width;
            let row: u32 = (i as u32) / template_width;
            let tile: TmpTile = tmp_decode::parse_tile_cell(
                data,
                offset as usize,
                tile_width,
                tile_height,
                col,
                row,
            )?;
            tiles.push(Some(tile));
        }

        Ok(TmpFile {
            template_width,
            template_height,
            tile_width,
            tile_height,
            tiles,
        })
    }

    /// Convert a tile's palette-indexed pixels to RGBA.
    ///
    /// Pixels outside the diamond shape use palette index 0 → transparent (alpha=0).
    /// Pixels INSIDE the diamond that happen to have palette index 0 are rendered
    /// as opaque — index 0 is a valid color within the diamond (often dark/black).
    /// Without this distinction, diamond-border index-0 pixels become transparent
    /// holes showing the black clear color, producing visible dark grid lines
    /// at every isometric cell boundary.
    pub fn tile_to_rgba(
        &self,
        tile_index: usize,
        palette: &Palette,
    ) -> Result<Vec<u8>, AssetError> {
        let cell_count: usize = (self.template_width * self.template_height) as usize;
        if tile_index >= cell_count {
            return Err(AssetError::TmpTileOutOfRange {
                index: tile_index,
                count: cell_count,
            });
        }
        let tile: &TmpTile =
            self.tiles[tile_index]
                .as_ref()
                .ok_or_else(|| AssetError::InvalidTmpFile {
                    reason: format!("Tile {} is an empty cell", tile_index),
                })?;

        let pixel_count: usize = tile.pixel_width as usize * tile.pixel_height as usize;
        let mut rgba: Vec<u8> = Vec::with_capacity(pixel_count * 4);
        for (i, &idx) in tile.pixels.iter().enumerate() {
            let color = palette.colors[idx as usize];
            rgba.push(color.r);
            rgba.push(color.g);
            rgba.push(color.b);
            // Inside the diamond, palette index 0 is a valid color — render opaque.
            // Outside the diamond, index 0 means transparent background.
            // Chroma key (magenta, idx != 0) stays transparent everywhere.
            if idx == 0
                && color.a == 0
                && is_inside_diamond(
                    (i as u32) % tile.pixel_width,
                    (i as u32) / tile.pixel_width,
                    self.tile_width,
                    self.tile_height,
                    tile.offset_x,
                    tile.offset_y,
                )
            {
                rgba.push(255);
            } else {
                rgba.push(color.a);
            }
        }
        Ok(rgba)
    }
}

/// Compute the diamond row width at row `j` for a tile of given height.
///
/// The diamond expands by DIAMOND_WIDTH_STEP each row from the top, reaching
/// full tile_width at the midpoint, then shrinks symmetrically. The last row
/// (j = tile_height - 1) has width 0.
fn diamond_row_width_at(j: u32, tile_height: u32) -> u32 {
    if j >= tile_height {
        return 0;
    }
    // w(j) = STEP * min(j+1, tile_height-1-j)
    let a: u32 = j + 1;
    let b: u32 = tile_height - 1 - j;
    tmp_decode::DIAMOND_WIDTH_STEP * a.min(b)
}

/// Check if a pixel at buffer position (px, py) is inside the diamond shape.
///
/// Accounts for the tile's offset (non-zero when extra data extends the
/// pixel buffer beyond the diamond region, e.g., cliff tiles).
fn is_inside_diamond(
    px: u32,
    py: u32,
    tile_width: u32,
    tile_height: u32,
    offset_x: i32,
    offset_y: i32,
) -> bool {
    // Convert pixel buffer coords to diamond-local coords.
    let dx: i32 = px as i32 + offset_x;
    let dy: i32 = py as i32 + offset_y;

    if dx < 0 || dy < 0 || dx >= tile_width as i32 || dy >= tile_height as i32 {
        return false;
    }

    let j: u32 = dy as u32;
    let row_width: u32 = diamond_row_width_at(j, tile_height);
    if row_width == 0 {
        return false;
    }

    let x_start: u32 = (tile_width - row_width) / 2;
    let x: u32 = dx as u32;
    x >= x_start && x < x_start + row_width
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::tmp_decode::{DIAMOND_INITIAL_WIDTH, DIAMOND_WIDTH_STEP};

    /// Compute total diamond pixel count for the given tile height.
    fn diamond_pixel_count(tile_height: u32) -> usize {
        let mut total: usize = 0;
        let mut w: u32 = DIAMOND_INITIAL_WIDTH;
        let half_minus_one: u32 = tile_height / 2 - 1;
        for j in 0..tile_height {
            total += w as usize;
            if j < half_minus_one {
                w += DIAMOND_WIDTH_STEP;
            } else {
                w = w.saturating_sub(DIAMOND_WIDTH_STEP);
            }
        }
        total
    }

    /// Build a minimal valid TMP file: 1×1 template, 8×4 tile.
    /// Diamond rows for 8×4: widths 4, 8, 4, 0 → 16 pixels total.
    fn make_test_tmp() -> Vec<u8> {
        let (tw, th): (u32, u32) = (8, 4);
        let dpixels: usize = diamond_pixel_count(th);
        let mut data: Vec<u8> = Vec::new();
        // File header (16 bytes).
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&tw.to_le_bytes());
        data.extend_from_slice(&th.to_le_bytes());
        // Offset table: 1 entry.
        data.extend_from_slice(&((TMP_HEADER_SIZE + 4) as u32).to_le_bytes());
        // Tile header (52 bytes).
        data.extend_from_slice(&[0u8; 20]); // bytes 0-19: metadata
        data.extend_from_slice(&0i32.to_le_bytes()); // extra_x
        data.extend_from_slice(&0i32.to_le_bytes()); // extra_y
        data.extend_from_slice(&0u32.to_le_bytes()); // extra_width
        data.extend_from_slice(&0u32.to_le_bytes()); // extra_height
        data.extend_from_slice(&0u32.to_le_bytes()); // flags (no extra)
        data.push(5);
        data.push(1);
        data.push(0); // height, terrain, ramp
        data.extend_from_slice(&[100, 120, 80]); // radar_left
        data.extend_from_slice(&[90, 110, 70]); // radar_right
        data.extend_from_slice(&[0u8; 3]); // padding
        // Diamond pixel data: distinct values 1..=N.
        for i in 0..dpixels {
            data.push((i as u8) + 1);
        }
        data.extend_from_slice(&vec![0u8; dpixels]); // depth
        data
    }

    #[test]
    fn test_parse_tmp_basic() {
        let tmp: TmpFile = TmpFile::from_bytes(&make_test_tmp()).expect("Should parse");
        assert_eq!(tmp.template_width, 1);
        assert_eq!(tmp.template_height, 1);
        assert_eq!(tmp.tile_width, 8);
        assert_eq!(tmp.tile_height, 4);
        assert_eq!(tmp.tiles.len(), 1);
        let tile: &TmpTile = tmp.tiles[0].as_ref().unwrap();
        assert_eq!(tile.height, 5);
        assert_eq!(tile.terrain_type, 1);
        assert_eq!(tile.pixel_width, 8);
        assert_eq!(tile.pixel_height, 4);
    }

    #[test]
    fn test_tmp_tile_diamond_pixels() {
        let tmp: TmpFile = TmpFile::from_bytes(&make_test_tmp()).expect("Should parse");
        let tile: &TmpTile = tmp.tiles[0].as_ref().unwrap();
        assert_eq!(tile.pixels.len(), 32); // 8×4
        // Row 0: 4 pixels centered → positions 2..6, values 1..4.
        assert_eq!(tile.pixels[2], 1);
        assert_eq!(tile.pixels[5], 4);
        // Row 1: 8 pixels full width, values 5..12.
        assert_eq!(tile.pixels[8], 5);
        assert_eq!(tile.pixels[15], 12);
        // Row 2: 4 pixels centered, values 13..16.
        assert_eq!(tile.pixels[18], 13);
        // Outside diamond = 0.
        assert_eq!(tile.pixels[0], 0);
        assert_eq!(tile.pixels[6], 0);
    }

    #[test]
    fn test_tile_to_rgba() {
        let tmp: TmpFile = TmpFile::from_bytes(&make_test_tmp()).expect("Should parse");
        let palette: Palette = Palette::from_bytes(&vec![0u8; 768]).expect("palette");
        let rgba: Vec<u8> = tmp.tile_to_rgba(0, &palette).expect("Should convert");
        assert_eq!(rgba.len(), 128); // 8 × 4 × 4
    }

    #[test]
    fn test_reject_too_small() {
        assert!(TmpFile::from_bytes(&vec![0; 8]).is_err());
    }

    #[test]
    fn test_empty_tile_offset() {
        let (tw, th): (u32, u32) = (8, 4);
        let dpixels: usize = diamond_pixel_count(th);
        let mut data: Vec<u8> = Vec::new();
        // Header: 2×1 template.
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&tw.to_le_bytes());
        data.extend_from_slice(&th.to_le_bytes());
        // Offsets: first valid, second empty (0).
        data.extend_from_slice(&((TMP_HEADER_SIZE + 8) as u32).to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        // Tile header + data.
        data.extend_from_slice(&[0u8; 20]);
        data.extend_from_slice(&0i32.to_le_bytes());
        data.extend_from_slice(&0i32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 3]); // height, terrain, ramp
        data.extend_from_slice(&[0u8; 6]); // radar
        data.extend_from_slice(&[0u8; 3]); // padding
        data.extend_from_slice(&vec![1u8; dpixels]);
        data.extend_from_slice(&vec![0u8; dpixels]);

        let tmp: TmpFile = TmpFile::from_bytes(&data).expect("Should parse");
        assert_eq!(tmp.tiles.len(), 2);
        assert!(tmp.tiles[0].is_some());
        assert!(tmp.tiles[1].is_none());
    }

    #[test]
    fn test_diamond_row_width_at_standard_60x30() {
        // Row 0: width 4 (top tip of diamond).
        assert_eq!(diamond_row_width_at(0, 30), 4);
        // Row 14: width 60 (widest row).
        assert_eq!(diamond_row_width_at(14, 30), 60);
        // Row 15: width 56 (shrinking phase).
        assert_eq!(diamond_row_width_at(15, 30), 56);
        // Row 28: width 4 (bottom tip).
        assert_eq!(diamond_row_width_at(28, 30), 4);
        // Row 29: width 0 (last row, empty).
        assert_eq!(diamond_row_width_at(29, 30), 0);
        // Out of bounds.
        assert_eq!(diamond_row_width_at(30, 30), 0);
    }

    #[test]
    fn test_is_inside_diamond_standard_tile() {
        // Standard 60x30 tile, no offset.
        // Center pixel (30, 14) should be inside.
        assert!(is_inside_diamond(30, 14, 60, 30, 0, 0));
        // Top-left corner (0, 0) should be outside.
        assert!(!is_inside_diamond(0, 0, 60, 30, 0, 0));
        // Top-center pixel (28, 0) should be inside (row 0 starts at x=28).
        assert!(is_inside_diamond(28, 0, 60, 30, 0, 0));
        // Pixel just outside diamond at row 0 (27, 0) should be outside.
        assert!(!is_inside_diamond(27, 0, 60, 30, 0, 0));
        // Last row (29) has width 0 → all outside.
        assert!(!is_inside_diamond(30, 29, 60, 30, 0, 0));
    }

    #[test]
    fn test_index_zero_inside_diamond_is_opaque() {
        // Build a TMP where diamond-interior pixels include palette index 0.
        // Verify tile_to_rgba makes those pixels opaque (alpha=255) instead
        // of transparent.
        let (tw, th): (u32, u32) = (8, 4);
        let dpixels: usize = diamond_pixel_count(th);
        let mut data: Vec<u8> = Vec::new();
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&tw.to_le_bytes());
        data.extend_from_slice(&th.to_le_bytes());
        data.extend_from_slice(&((TMP_HEADER_SIZE + 4) as u32).to_le_bytes());
        data.extend_from_slice(&[0u8; 20]);
        data.extend_from_slice(&0i32.to_le_bytes());
        data.extend_from_slice(&0i32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.push(0);
        data.push(0);
        data.push(0);
        data.extend_from_slice(&[0u8; 6]);
        data.extend_from_slice(&[0u8; 3]);
        // All diamond pixels are index 0 (palette index 0).
        data.extend_from_slice(&vec![0u8; dpixels]);
        data.extend_from_slice(&vec![0u8; dpixels]);

        let tmp: TmpFile = TmpFile::from_bytes(&data).expect("Should parse");
        // Create palette where index 0 is black with alpha=0.
        let palette: Palette = Palette::from_bytes(&vec![0u8; 768]).expect("palette");
        let rgba: Vec<u8> = tmp.tile_to_rgba(0, &palette).expect("Should convert");

        // 8x4 tile. Row 0: 4 pixels at x=2..5 inside diamond.
        // Pixel at (2, 0) = idx 2 in the pixel buffer → RGBA at offset 2*4.
        let inside_alpha: u8 = rgba[2 * 4 + 3];
        assert_eq!(inside_alpha, 255, "index 0 inside diamond should be opaque");

        // Pixel at (0, 0) is outside diamond → should be transparent.
        let outside_alpha: u8 = rgba[0 * 4 + 3];
        assert_eq!(
            outside_alpha, 0,
            "index 0 outside diamond should be transparent"
        );
    }
}
