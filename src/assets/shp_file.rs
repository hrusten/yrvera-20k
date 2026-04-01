//! Parser for RA2 .shp(ts) sprite files.
//!
//! SHP files contain multiple frames of 8-bit indexed color data.
//! Used for buildings, infantry, animations, cameo icons, and UI elements.
//! Each pixel is a palette index (0 = transparent).
//!
//! ## Format (Tiberian Sun / Red Alert 2 variant, "SHP(TS)")
//!
//! ### File header (8 bytes):
//! ```text
//! u16: zero (always 0 — distinguishes from older SHP format)
//! u16: width  (max frame width in pixels)
//! u16: height (max frame height in pixels)
//! u16: frame_count (number of frames in this file)
//! ```
//!
//! ### Per-frame header (24 bytes each, frame_count total):
//! ```text
//! u16: frame_x      — X offset within the full sprite bounds
//! u16: frame_y      — Y offset within the full sprite bounds
//! u16: frame_width  — width of this specific frame's pixel data
//! u16: frame_height — height of this specific frame's pixel data
//! u08: flags        — bit 1 = has transparency, bit 2 = uses RLE compression
//! u24: padding/reserved
//! u32: zero/reserved
//! u32: data_offset  — byte offset from file start to this frame's pixel data
//! ```
//!
//! ### Frame pixel data:
//! - If not RLE compressed: raw palette indices, width * height bytes
//! - If RLE compressed: see shp_decode.rs for the RLE-Zero format details.
//!
//! ## Dependency rules
//! - Part of assets/ — no dependencies on game modules.
//! - Uses util/read_helpers for binary reading, assets/shp_decode for RLE decompression.

use crate::assets::error::AssetError;
use crate::assets::pal_file::Palette;
use crate::assets::shp_decode::{decode_length_prefixed_frame, decode_rle_frame};
use crate::util::read_helpers::{read_u16_le, read_u32_le};

/// Frame compression format values (byte 8 in the 24-byte frame header).
/// Combines transparency (bit 0) and RLE (bit 1) flags.
/// Format 0/1: raw uncompressed pixel data (width bytes per row, no length prefix).
/// Format 2: length-prefixed uncompressed scanlines (u16 length + raw bytes).
/// Format 3: length-prefixed RLE-Zero compressed scanlines (u16 length + RLE bytes).
const FORMAT_RAW: u8 = 0;
const FORMAT_RAW_TRANSPARENT: u8 = 1;
const FORMAT_LENGTH_PREFIXED: u8 = 2;
const FORMAT_RLE_ZERO: u8 = 3;

/// A parsed SHP sprite file containing one or more frames.
///
/// Frames are stored as palette indices (u8). Convert to RGBA
/// using `frame_to_rgba()` with a palette for rendering.
#[derive(Debug)]
pub struct ShpFile {
    /// Maximum width across all frames (from file header).
    pub width: u16,
    /// Maximum height across all frames (from file header).
    pub height: u16,
    /// The individual sprite frames.
    pub frames: Vec<ShpFrame>,
}

/// A single frame from an SHP file.
///
/// Each pixel is a palette index. Index 0 = transparent.
/// The frame may be smaller than the file's overall width/height,
/// positioned at (frame_x, frame_y) within the full bounds.
#[derive(Debug)]
pub struct ShpFrame {
    /// X offset of this frame within the full sprite bounds.
    pub frame_x: u16,
    /// Y offset of this frame within the full sprite bounds.
    pub frame_y: u16,
    /// Width of this frame's pixel data.
    pub frame_width: u16,
    /// Height of this frame's pixel data.
    pub frame_height: u16,
    /// Decoded pixel data (palette indices). Length = frame_width * frame_height.
    /// Index 0 means transparent.
    pub pixels: Vec<u8>,
}

impl ShpFile {
    /// Parse an SHP file from raw bytes.
    ///
    /// Reads the file header, all frame headers, then decodes each frame's
    /// pixel data (handling both raw and RLE-compressed formats).
    pub fn from_bytes(data: &[u8]) -> Result<Self, AssetError> {
        // --- File header: 8 bytes ---
        if data.len() < 8 {
            return Err(AssetError::InvalidShpHeader {
                reason: format!(
                    "File too small for header: {} bytes (need at least 8)",
                    data.len()
                ),
            });
        }

        // Bytes 0-1: should be zero (distinguishes SHP(TS) from older SHP format).
        let zero: u16 = read_u16_le(data, 0);
        if zero != 0 {
            return Err(AssetError::InvalidShpHeader {
                reason: format!(
                    "First two bytes should be 0 for SHP(TS) format, got {}",
                    zero
                ),
            });
        }

        let width: u16 = read_u16_le(data, 2);
        let height: u16 = read_u16_le(data, 4);
        let frame_count: u16 = read_u16_le(data, 6);

        // --- Frame headers: 24 bytes each ---
        let headers_end: usize = 8 + (frame_count as usize) * 24;
        if data.len() < headers_end {
            return Err(AssetError::InvalidShpHeader {
                reason: format!(
                    "File too small for {} frame headers: {} bytes (need {})",
                    frame_count,
                    data.len(),
                    headers_end
                ),
            });
        }

        let mut frames: Vec<ShpFrame> = Vec::with_capacity(frame_count as usize);

        for i in 0..frame_count as usize {
            let hdr_offset: usize = 8 + i * 24;

            let frame_x: u16 = read_u16_le(data, hdr_offset);
            let frame_y: u16 = read_u16_le(data, hdr_offset + 2);
            let frame_width: u16 = read_u16_le(data, hdr_offset + 4);
            let frame_height: u16 = read_u16_le(data, hdr_offset + 6);
            let format: u8 = data[hdr_offset + 8];
            // Bytes 9-11: padding/reserved
            // Bytes 12-15: radar minimap color (RGB packed into u32)
            // Bytes 16-19: reserved (always 0)
            // Bytes 20-23: data_offset (absolute file offset to this frame's pixel data)
            let data_offset: u32 = read_u32_le(data, hdr_offset + 20);

            // A frame with zero dimensions has no pixel data (empty frame).
            if frame_width == 0 || frame_height == 0 {
                frames.push(ShpFrame {
                    frame_x,
                    frame_y,
                    frame_width,
                    frame_height,
                    pixels: Vec::new(),
                });
                continue;
            }

            let pixel_count: usize = frame_width as usize * frame_height as usize;
            let frame_data_start: usize = data_offset as usize;

            // Bounds check: make sure data_offset points inside the file.
            if frame_data_start >= data.len() {
                return Err(AssetError::ParseError {
                    format: "SHP".to_string(),
                    detail: format!(
                        "Frame {} data offset {} is past end of file ({})",
                        i,
                        frame_data_start,
                        data.len()
                    ),
                });
            }

            let frame_slice: &[u8] = &data[frame_data_start..];
            let pixels: Vec<u8> = match format {
                FORMAT_RLE_ZERO => {
                    // Format 3: each scanline has u16 length (includes itself),
                    // followed by (length - 2) bytes of RLE-Zero compressed data.
                    decode_rle_frame(frame_slice, frame_width as usize, frame_height as usize)?
                }
                FORMAT_LENGTH_PREFIXED => {
                    // Format 2: each scanline has u16 length (includes itself),
                    // followed by (length - 2) bytes of uncompressed pixel data.
                    decode_length_prefixed_frame(
                        frame_slice,
                        frame_width as usize,
                        frame_height as usize,
                    )?
                }
                FORMAT_RAW | FORMAT_RAW_TRANSPARENT | _ => {
                    // Format 0/1: raw uncompressed pixel data, width bytes per row.
                    let end: usize = frame_data_start + pixel_count;
                    if end > data.len() {
                        return Err(AssetError::ParseError {
                            format: "SHP".to_string(),
                            detail: format!(
                                "Frame {} raw data extends past end of file ({} > {})",
                                i,
                                end,
                                data.len()
                            ),
                        });
                    }
                    data[frame_data_start..end].to_vec()
                }
            };

            frames.push(ShpFrame {
                frame_x,
                frame_y,
                frame_width,
                frame_height,
                pixels,
            });
        }

        Ok(ShpFile {
            width,
            height,
            frames,
        })
    }

    /// Convert a frame's palette-indexed pixels to RGBA using the given palette.
    ///
    /// Returns a Vec of width * height * 4 bytes (RGBA).
    /// Palette index 0 becomes fully transparent (alpha = 0).
    pub fn frame_to_rgba(
        &self,
        frame_index: usize,
        palette: &Palette,
    ) -> Result<Vec<u8>, AssetError> {
        if frame_index >= self.frames.len() {
            return Err(AssetError::ShpFrameOutOfRange {
                index: frame_index as u16,
                count: self.frames.len() as u16,
            });
        }

        let frame: &ShpFrame = &self.frames[frame_index];
        let pixel_count: usize = frame.frame_width as usize * frame.frame_height as usize;
        let mut rgba: Vec<u8> = Vec::with_capacity(pixel_count * 4);

        for &palette_index in &frame.pixels {
            let color = palette.colors[palette_index as usize];
            rgba.push(color.r);
            rgba.push(color.g);
            rgba.push(color.b);
            rgba.push(color.a);
        }

        Ok(rgba)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid SHP file with one uncompressed 2x2 frame.
    fn make_test_shp_raw() -> Vec<u8> {
        let mut data: Vec<u8> = Vec::new();

        // File header (8 bytes): zero=0, width=2, height=2, frame_count=1
        data.extend_from_slice(&0u16.to_le_bytes()); // zero
        data.extend_from_slice(&2u16.to_le_bytes()); // width
        data.extend_from_slice(&2u16.to_le_bytes()); // height
        data.extend_from_slice(&1u16.to_le_bytes()); // frame_count

        // Frame header (24 bytes):
        // Byte layout: x(2) y(2) w(2) h(2) format(1) padding(11) data_offset(4)
        let data_offset: u32 = 8 + 24; // right after the header
        data.extend_from_slice(&0u16.to_le_bytes()); // +0: frame_x
        data.extend_from_slice(&0u16.to_le_bytes()); // +2: frame_y
        data.extend_from_slice(&2u16.to_le_bytes()); // +4: frame_width
        data.extend_from_slice(&2u16.to_le_bytes()); // +6: frame_height
        data.push(0x00); // +8: format (0 = raw)
        data.extend_from_slice(&[0u8; 11]); // +9: padding (11 bytes)
        data.extend_from_slice(&data_offset.to_le_bytes()); // +20: data_offset

        // Pixel data: 2x2 = 4 bytes (palette indices: 1, 2, 3, 0)
        data.extend_from_slice(&[1, 2, 3, 0]);

        data
    }

    #[test]
    fn test_parse_raw_shp() {
        let data: Vec<u8> = make_test_shp_raw();
        let shp: ShpFile = ShpFile::from_bytes(&data).expect("Should parse valid SHP");

        assert_eq!(shp.width, 2);
        assert_eq!(shp.height, 2);
        assert_eq!(shp.frames.len(), 1);
        assert_eq!(shp.frames[0].pixels, vec![1, 2, 3, 0]);
    }

    #[test]
    fn test_reject_too_small() {
        let data: Vec<u8> = vec![0; 4]; // Way too small
        assert!(ShpFile::from_bytes(&data).is_err());
    }

    #[test]
    fn test_frame_to_rgba() {
        let data: Vec<u8> = make_test_shp_raw();
        let shp: ShpFile = ShpFile::from_bytes(&data).expect("Should parse");

        // Create a palette where index 1 = red, index 2 = green, index 3 = blue
        let mut pal_data: Vec<u8> = vec![0u8; 768];
        pal_data[3] = 63; // Index 1: R=63 (max red)
        pal_data[7] = 63; // Index 2: G=63 (max green)
        pal_data[11] = 63; // Index 3: B=63 (max blue)

        let palette: Palette = Palette::from_bytes(&pal_data).expect("Should parse palette");
        let rgba: Vec<u8> = shp.frame_to_rgba(0, &palette).expect("Should convert");

        // 2x2 * 4 bytes = 16 bytes
        assert_eq!(rgba.len(), 16);
        // Pixel 0 (index 1): red (255, 0, 0, 255)
        assert_eq!(rgba[0], 255); // R
        assert_eq!(rgba[1], 0); // G
        assert_eq!(rgba[3], 255); // A (opaque)
        // Pixel 3 (index 0): transparent
        assert_eq!(rgba[15], 0); // A (transparent)
    }

    #[test]
    fn test_frame_out_of_range() {
        let data: Vec<u8> = make_test_shp_raw();
        let shp: ShpFile = ShpFile::from_bytes(&data).expect("Should parse");
        let pal_data: Vec<u8> = vec![0u8; 768];
        let palette: Palette = Palette::from_bytes(&pal_data).expect("Should parse");

        // Frame index 5 doesn't exist (only 1 frame).
        assert!(shp.frame_to_rgba(5, &palette).is_err());
    }
}
