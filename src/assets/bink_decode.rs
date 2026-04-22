// Ported from FFmpeg's libavcodec/bink.c and libavcodec/binkdsp.c.
// Copyright (c) 2009 Konstantin Shishkov
// Copyright (c) 2011 Peter Ross <pross@xvid.org>
// Licensed LGPL-2.1-or-later. See LICENSES/LGPL-2.1-or-later.txt in the repo root.

//! Bink 1 video decoder.
//!
//! Decodes one video packet at a time into a YUV420P frame. Supports BIKi and
//! BIKk variants. B-frames (BIKb), Bink 2 (KB2), alpha, and grayscale paths
//! are not implemented — not used by RA2 / YR cutscenes.
//!
//! ## Dependency rules
//! - Part of assets/ — no dependencies on game modules.

// IDCT constants from FFmpeg's binkdsp.c.
const IDCT_A1: i32 = 2896;
const IDCT_A2: i32 = 2217;
const IDCT_A3: i32 = 3784;
const IDCT_A4: i32 = -5352;

#[inline]
fn idct_mul(x: i32, y: i32) -> i32 {
    // Use wrapping multiply to match FFmpeg's unsigned-cast intermediate.
    x.wrapping_mul(y) >> 11
}

/// Core 8-point IDCT used by both column and row passes.
#[inline]
fn idct8(
    src: &[i32],
    dst: &mut [i32],
    s_idx: [usize; 8],
    d_idx: [usize; 8],
    munge: impl Fn(i32) -> i32,
) {
    let a0 = src[s_idx[0]] + src[s_idx[4]];
    let a1 = src[s_idx[0]] - src[s_idx[4]];
    let a2 = src[s_idx[2]] + src[s_idx[6]];
    let a3 = idct_mul(IDCT_A1, src[s_idx[2]] - src[s_idx[6]]);
    let a4 = src[s_idx[5]] + src[s_idx[3]];
    let a5 = src[s_idx[5]] - src[s_idx[3]];
    let a6 = src[s_idx[1]] + src[s_idx[7]];
    let a7 = src[s_idx[1]] - src[s_idx[7]];

    let b0 = a4 + a6;
    let b1 = idct_mul(IDCT_A3, a5 + a7);
    let b2 = idct_mul(IDCT_A4, a5) - b0 + b1;
    let b3 = idct_mul(IDCT_A1, a6 - a4) - b2;
    let b4 = idct_mul(IDCT_A2, a7) + b3 - b1;

    dst[d_idx[0]] = munge(a0 + a2 + b0);
    dst[d_idx[1]] = munge(a1 + a3 - a2 + b2);
    dst[d_idx[2]] = munge(a1 - a3 + a2 + b3);
    dst[d_idx[3]] = munge(a0 - a2 - b4);
    dst[d_idx[4]] = munge(a0 - a2 + b4);
    dst[d_idx[5]] = munge(a1 - a3 + a2 - b3);
    dst[d_idx[6]] = munge(a1 + a3 - a2 - b2);
    dst[d_idx[7]] = munge(a0 + a2 - b0);
}

/// Column IDCT: 8-point transform on each column of a 64-entry block.
fn idct_col(scratch: &mut [i32; 64], src: &[i32; 64], col: usize) {
    let s_idx = [
        col,
        col + 8,
        col + 16,
        col + 24,
        col + 32,
        col + 40,
        col + 48,
        col + 56,
    ];
    // Fast path: if all non-DC entries in this column are zero, broadcast DC.
    if src[s_idx[1]]
        | src[s_idx[2]]
        | src[s_idx[3]]
        | src[s_idx[4]]
        | src[s_idx[5]]
        | src[s_idx[6]]
        | src[s_idx[7]]
        == 0
    {
        let v = src[s_idx[0]];
        for &i in &s_idx {
            scratch[i] = v;
        }
        return;
    }
    idct8(src, scratch, s_idx, s_idx, |x| x);
}

#[inline]
fn idct_row_munge(v: i32) -> i32 {
    (v + 0x7F) >> 8
}

fn idct_row_to_u8(scratch: &[i32; 64], dst: &mut [u8], row: usize, stride: usize) {
    let s_idx = [
        row * 8,
        row * 8 + 1,
        row * 8 + 2,
        row * 8 + 3,
        row * 8 + 4,
        row * 8 + 5,
        row * 8 + 6,
        row * 8 + 7,
    ];
    let mut tmp = [0i32; 8];
    let d_idx = [0, 1, 2, 3, 4, 5, 6, 7];
    idct8(scratch, &mut tmp, s_idx, d_idx, idct_row_munge);
    let base = row * stride;
    for k in 0..8 {
        dst[base + k] = tmp[k].clamp(0, 255) as u8;
    }
}

fn idct_row_add_u8(scratch: &[i32; 64], dst: &mut [u8], row: usize, stride: usize) {
    let s_idx = [
        row * 8,
        row * 8 + 1,
        row * 8 + 2,
        row * 8 + 3,
        row * 8 + 4,
        row * 8 + 5,
        row * 8 + 6,
        row * 8 + 7,
    ];
    let mut tmp = [0i32; 8];
    let d_idx = [0, 1, 2, 3, 4, 5, 6, 7];
    idct8(scratch, &mut tmp, s_idx, d_idx, idct_row_munge);
    let base = row * stride;
    for k in 0..8 {
        let v = dst[base + k] as i32 + tmp[k];
        dst[base + k] = v.clamp(0, 255) as u8;
    }
}

/// Full 2D IDCT: column-first then row-first. Writes 8x8 result into `dst`.
pub(crate) fn bink_idct_put(dst: &mut [u8], stride: usize, block: &[i32; 64]) {
    let mut scratch = [0i32; 64];
    for col in 0..8 {
        idct_col(&mut scratch, block, col);
    }
    for row in 0..8 {
        idct_row_to_u8(&scratch, dst, row, stride);
    }
}

/// Full 2D IDCT and add to existing pixels.
pub(crate) fn bink_idct_add(dst: &mut [u8], stride: usize, block: &mut [i32; 64]) {
    let mut scratch = [0i32; 64];
    for col in 0..8 {
        idct_col(&mut scratch, block, col);
    }
    for row in 0..8 {
        idct_row_add_u8(&scratch, dst, row, stride);
    }
    let _ = block;
}

/// Add an 8x8 i16 residue block to a dst area, clipping to u8.
/// Port of `add_pixels8_c` in binkdsp.c.
pub(crate) fn add_pixels8(dst: &mut [u8], block: &[i16; 64], stride: usize) {
    for row in 0..8 {
        for col in 0..8 {
            let v = dst[row * stride + col] as i32 + block[row * 8 + col] as i32;
            dst[row * stride + col] = v.clamp(0, 255) as u8;
        }
    }
}

/// Copy an 8x8 block from `src` to `dst` at the given strides.
///
/// Caller must ensure the src and dst rectangles do not overlap.
pub(crate) fn copy_block8(
    dst: &mut [u8],
    src: &[u8],
    dst_stride: usize,
    src_stride: usize,
) {
    for row in 0..8 {
        dst[row * dst_stride..row * dst_stride + 8]
            .copy_from_slice(&src[row * src_stride..row * src_stride + 8]);
    }
}

/// Copy an 8x8 block where source and destination live in the same buffer and
/// may overlap. Stages through a stack scratch array.
/// Port of `put_pixels8x8_overlapped` in FFmpeg bink.c.
pub(crate) fn copy_block8_overlapped(dst: &mut [u8], src: &[u8], stride: usize) {
    let mut tmp = [0u8; 64];
    for row in 0..8 {
        tmp[row * 8..row * 8 + 8].copy_from_slice(&src[row * stride..row * stride + 8]);
    }
    for row in 0..8 {
        dst[row * stride..row * stride + 8].copy_from_slice(&tmp[row * 8..row * 8 + 8]);
    }
}

/// Fill an `n x n` square at `dst` with the constant value `v`.
/// `n` must be 8 or 16.
pub(crate) fn fill_block(dst: &mut [u8], v: u8, stride: usize, n: usize) {
    debug_assert!(n == 8 || n == 16);
    for row in 0..n {
        let base = row * stride;
        dst[base..base + n].fill(v);
    }
}

/// Pixel-double an 8x8 ublock into a 16x16 region at `dst` with `stride`.
/// Each source pixel becomes a 2x2 square. Port of `scale_block_c` in binkdsp.c.
pub(crate) fn scale_block_8x8_to_16x16(src: &[u8; 64], dst: &mut [u8], stride: usize) {
    for sy in 0..8 {
        let row0 = sy * 2;
        let row1 = sy * 2 + 1;
        for sx in 0..8 {
            let v = src[sy * 8 + sx];
            let dx = sx * 2;
            dst[row0 * stride + dx] = v;
            dst[row0 * stride + dx + 1] = v;
            dst[row1 * stride + dx] = v;
            dst[row1 * stride + dx + 1] = v;
        }
    }
}

use crate::assets::bink_bits::{BitReader, VlcTable};
use crate::assets::bink_data::{
    BINK_INTER_QUANT, BINK_INTRA_QUANT, BINK_PATTERNS, BINK_RLELENS, BINK_SCAN,
    BINK_TREE_BITS, BINK_TREE_LENS, DC_START_BITS,
};
use crate::assets::bink_file::{BinkHeader, BinkVersion};
use crate::assets::error::AssetError;

/// Bundle IDs for modern Bink (BIKi/BIKk).
#[allow(dead_code)]
#[derive(Clone, Copy)]
#[repr(usize)]
enum Src {
    BlockTypes = 0,
    SubBlockTypes = 1,
    Colors = 2,
    Pattern = 3,
    XOff = 4,
    YOff = 5,
    IntraDc = 6,
    InterDc = 7,
    Run = 8,
}
const NB_SRC: usize = 9;

/// YUV frame buffer; planes have their own strides (Y stride = rounded-up
/// width, UV stride = rounded-up width/2).
pub struct BinkFrame {
    pub width: u32,
    pub height: u32,
    pub color_range: ColorRange,
    pub y: Box<[u8]>,
    pub stride_y: usize,
    pub u: Box<[u8]>,
    pub stride_uv: usize,
    pub v: Box<[u8]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorRange {
    /// BT.601 limited (MPEG): Y in 16..235, UV in 16..240.
    Mpeg,
    /// BT.601 full (JPEG): Y/U/V in 0..255.
    Jpeg,
}

impl BinkFrame {
    fn alloc(width: u32, height: u32, color_range: ColorRange) -> Self {
        let stride_y = ((width + 7) & !7) as usize;
        let stride_uv = (((width + 15) & !15) / 2) as usize;
        let y_size = stride_y * ((height + 7) & !7) as usize;
        let uv_size = stride_uv * (((height + 15) & !15) / 2) as usize;
        Self {
            width,
            height,
            color_range,
            y: vec![0u8; y_size].into_boxed_slice(),
            stride_y,
            u: vec![0u8; uv_size].into_boxed_slice(),
            stride_uv,
            v: vec![0u8; uv_size].into_boxed_slice(),
        }
    }

    #[allow(dead_code)]
    fn plane_mut(&mut self, idx: usize) -> (&mut [u8], usize) {
        match idx {
            0 => (&mut self.y, self.stride_y),
            1 => (&mut self.u, self.stride_uv),
            2 => (&mut self.v, self.stride_uv),
            _ => panic!("invalid plane idx"),
        }
    }
}

/// Per-bundle state.
#[allow(dead_code)]
struct Bundle {
    len_bits: u32,
    tree: HuffmanTree,
    buf_start: usize,
    buf_end: usize,
    cur_dec: usize,
    cur_ptr: usize,
}

#[derive(Default, Clone)]
struct HuffmanTree {
    vlc_num: u32,
    syms: [u8; 16],
}

#[allow(dead_code)]
pub struct BinkDecoder {
    version: BinkVersion,
    width: u32,
    height: u32,
    has_alpha: bool,
    color_range: ColorRange,

    vlc_tables: Vec<VlcTable>,
    col_high: [HuffmanTree; 16],
    col_lastval: u8,

    bundles: [Bundle; NB_SRC],
    bundle_data: Vec<u8>,

    frame_num: u32,
    pub cur: BinkFrame,
    pub prev: BinkFrame,
    has_prev: bool,
}

impl BinkDecoder {
    pub fn new(header: &BinkHeader) -> Result<Self, AssetError> {
        if header.is_gray() {
            return Err(AssetError::BinkError {
                reason: "grayscale Bink not supported".to_string(),
            });
        }

        let color_range = match header.version {
            BinkVersion::BikK => ColorRange::Jpeg,
            _ => ColorRange::Mpeg,
        };

        let bw = ((header.width + 7) >> 3) as usize;
        let bh = ((header.height + 7) >> 3) as usize;
        let blocks = bw * bh;
        let total_bytes = blocks.saturating_mul(64 * NB_SRC);
        let bundle_data = vec![0u8; total_bytes];

        let block_per_bundle = blocks * 64;
        let bundles: [Bundle; NB_SRC] = std::array::from_fn(|i| Bundle {
            len_bits: 0,
            tree: HuffmanTree::default(),
            buf_start: i * block_per_bundle,
            buf_end: (i + 1) * block_per_bundle,
            cur_dec: i * block_per_bundle,
            cur_ptr: i * block_per_bundle,
        });

        let mut vlc_tables = Vec::with_capacity(16);
        for t in 0..16 {
            let mut codes = [0u8; 16];
            let mut lens = [0u8; 16];
            for i in 0..16 {
                codes[i] = BINK_TREE_BITS[t][i];
                lens[i] = BINK_TREE_LENS[t][i];
            }
            vlc_tables.push(VlcTable::build(&codes, &lens)?);
        }

        Ok(Self {
            version: header.version,
            width: header.width,
            height: header.height,
            has_alpha: header.has_alpha(),
            color_range,
            vlc_tables,
            col_high: std::array::from_fn(|_| HuffmanTree::default()),
            col_lastval: 0,
            bundles,
            bundle_data,
            frame_num: 0,
            cur: BinkFrame::alloc(header.width, header.height, color_range),
            prev: BinkFrame::alloc(header.width, header.height, color_range),
            has_prev: false,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn color_range(&self) -> ColorRange {
        self.color_range
    }

    /// Reset ping-pong state; caller must call this after seeking.
    pub fn flush(&mut self) {
        self.frame_num = 0;
        self.has_prev = false;
    }

    /// Compute bundle `len_bits` from plane width and block-width.
    /// Port of `init_lengths` at libavcodec/bink.c:155-173.
    fn init_bundle_lengths(&mut self, width: u32, bw: u32) {
        let width = (width + 7) & !7;
        let log2_bw_plus_511 = log2_floor((width >> 3) + 511) + 1;
        let log2_bw16_plus_511 = log2_floor((width >> 4) + 511) + 1;
        let log2_bw_cols_plus_511 = log2_floor(bw * 64 + 511) + 1;
        let log2_pattern_plus_511 = log2_floor((bw << 3) + 511) + 1;
        let log2_run_plus_511 = log2_floor(bw * 48 + 511) + 1;

        self.bundles[Src::BlockTypes as usize].len_bits = log2_bw_plus_511;
        self.bundles[Src::SubBlockTypes as usize].len_bits = log2_bw16_plus_511;
        self.bundles[Src::Colors as usize].len_bits = log2_bw_cols_plus_511;
        self.bundles[Src::IntraDc as usize].len_bits = log2_bw_plus_511;
        self.bundles[Src::InterDc as usize].len_bits = log2_bw_plus_511;
        self.bundles[Src::XOff as usize].len_bits = log2_bw_plus_511;
        self.bundles[Src::YOff as usize].len_bits = log2_bw_plus_511;
        self.bundles[Src::Pattern as usize].len_bits = log2_pattern_plus_511;
        self.bundles[Src::Run as usize].len_bits = log2_run_plus_511;
    }

    /// Reset all bundle cursors to the start of their allocated region.
    #[allow(dead_code)]
    fn reset_bundle_cursors(&mut self) {
        for b in &mut self.bundles {
            b.cur_dec = b.buf_start;
            b.cur_ptr = b.buf_start;
        }
    }

    /// Prepare one bundle for decoding: reads its Huffman tree (or 16
    /// col_high trees for COLORS) and resets the bundle's cursors.
    /// Port of `read_bundle` at libavcodec/bink.c:285-313.
    fn read_bundle(
        &mut self,
        r: &mut BitReader<'_>,
        bundle_num: usize,
    ) -> Result<(), AssetError> {
        if bundle_num == Src::Colors as usize {
            for i in 0..16 {
                self.col_high[i] = HuffmanTree::read(r)?;
            }
            self.col_lastval = 0;
        }
        if bundle_num != Src::IntraDc as usize && bundle_num != Src::InterDc as usize {
            self.bundles[bundle_num].tree = HuffmanTree::read(r)?;
        }
        let b = &mut self.bundles[bundle_num];
        b.cur_dec = b.buf_start;
        b.cur_ptr = b.buf_start;
        Ok(())
    }
}

#[inline]
fn log2_floor(x: u32) -> u32 {
    debug_assert!(x > 0);
    31 - x.leading_zeros()
}

/// Interleave two adjacent halves (size + size) based on bits read. Port of
/// `merge` at libavcodec/bink.c:220-239. Writes `2*size` bytes to `dst`,
/// consuming up to `2*size` bits from `r`.
fn merge_lists(
    r: &mut BitReader<'_>,
    dst: &mut [u8],
    src: &[u8],
    size: usize,
) -> Result<(), AssetError> {
    let mut src1 = 0usize;
    let mut src2 = size;
    let mut size1 = size;
    let mut size2 = size;
    let mut d = 0usize;

    while size1 > 0 && size2 > 0 {
        if !r.read_bit()? {
            dst[d] = src[src1];
            src1 += 1;
            size1 -= 1;
        } else {
            dst[d] = src[src2];
            src2 += 1;
            size2 -= 1;
        }
        d += 1;
    }
    while size1 > 0 {
        dst[d] = src[src1];
        src1 += 1;
        size1 -= 1;
        d += 1;
    }
    while size2 > 0 {
        dst[d] = src[src2];
        src2 += 1;
        size2 -= 1;
        d += 1;
    }
    Ok(())
}

impl HuffmanTree {
    /// Read a Huffman-tree descriptor from the bitstream.
    /// Port of `read_tree` at libavcodec/bink.c:247-283.
    fn read(r: &mut BitReader<'_>) -> Result<Self, AssetError> {
        if r.bits_left() < 4 {
            return Err(AssetError::BinkError {
                reason: "tree descriptor EOF".to_string(),
            });
        }
        let vlc_num = r.read_bits(4)?;
        if vlc_num == 0 {
            let mut syms = [0u8; 16];
            for i in 0..16 {
                syms[i] = i as u8;
            }
            return Ok(Self { vlc_num, syms });
        }

        if r.read_bit()? {
            let mut len = r.read_bits(3)? as usize;
            let mut seen = [false; 16];
            let mut syms = [0u8; 16];
            for i in 0..=len {
                let s = r.read_bits(4)? as u8;
                syms[i] = s;
                seen[s as usize] = true;
            }
            let mut i = 0usize;
            while i < 16 && len < 15 {
                if !seen[i] {
                    len += 1;
                    syms[len] = i as u8;
                }
                i += 1;
            }
            Ok(Self { vlc_num, syms })
        } else {
            let len = r.read_bits(2)? as usize;
            let mut tmp1 = [0u8; 16];
            let mut tmp2 = [0u8; 16];
            let (mut in_arr, mut out_arr) = (&mut tmp1, &mut tmp2);
            for i in 0..16 {
                in_arr[i] = i as u8;
            }
            for i in 0..=len {
                let size = 1usize << i;
                let mut t = 0usize;
                while t < 16 {
                    let mut src_window = [0u8; 16];
                    src_window[..2 * size].copy_from_slice(&in_arr[t..t + 2 * size]);
                    let mut dst_window = [0u8; 16];
                    merge_lists(r, &mut dst_window[..2 * size], &src_window[..2 * size], size)?;
                    out_arr[t..t + 2 * size].copy_from_slice(&dst_window[..2 * size]);
                    t += size << 1;
                }
                std::mem::swap(&mut in_arr, &mut out_arr);
            }
            let mut syms = [0u8; 16];
            syms.copy_from_slice(in_arr);
            Ok(Self { vlc_num, syms })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idct_of_dc_only_produces_flat_block() {
        let mut block = [0i32; 64];
        block[0] = 2048;
        let mut dst = [0u8; 64];
        bink_idct_put(&mut dst, 8, &block);
        let v = dst[0];
        for &p in &dst {
            assert_eq!(p, v, "IDCT of DC-only block should be flat");
        }
    }

    #[test]
    fn idct_add_accumulates() {
        let mut dst = [100u8; 64];
        let mut block = [0i32; 64];
        block[0] = 2048;
        bink_idct_add(&mut dst, 8, &mut block);
        let v = dst[0];
        for &p in &dst {
            assert_eq!(p, v);
        }
    }

    #[test]
    fn idct_all_zeros_produces_all_zeros() {
        let block = [0i32; 64];
        let mut dst = [0u8; 64];
        bink_idct_put(&mut dst, 8, &block);
        for &p in &dst {
            assert_eq!(p, 0);
        }
    }

    #[test]
    fn idct_symmetric_input_produces_smooth_output() {
        // DC + a few AC coefficients. Output should vary smoothly, not jump.
        let mut block = [0i32; 64];
        block[0] = 1024;
        block[1] = 256;
        block[2] = 128;
        let mut dst = [0u8; 64];
        bink_idct_put(&mut dst, 8, &block);
        for r in 0..8 {
            for c in 0..7 {
                let diff = (dst[r * 8 + c] as i32 - dst[r * 8 + c + 1] as i32).abs();
                assert!(
                    diff < 128,
                    "IDCT output has huge jump at row {} col {}",
                    r,
                    c
                );
            }
        }
    }

    #[test]
    fn idct_clips_to_u8_range() {
        let mut block = [0i32; 64];
        block[0] = 1_000_000;
        let mut dst = [0u8; 64];
        bink_idct_put(&mut dst, 8, &block);
        for &p in &dst {
            assert!(p == 255);
        }
    }

    #[test]
    fn add_pixels8_accumulates_and_clips() {
        let mut dst = [100u8; 64];
        let mut block = [0i16; 64];
        block[0] = 50;
        block[63] = -200;
        block[10] = 300;
        add_pixels8(&mut dst, &block, 8);
        assert_eq!(dst[0], 150);
        assert_eq!(dst[63], 0); // clipped
        assert_eq!(dst[10], 255); // clipped
    }

    #[test]
    fn copy_block8_copies_with_stride() {
        let mut buf = [0u8; 16 * 16];
        for r in 0..8 {
            for c in 0..16 {
                buf[r * 16 + c] = (r * 10 + c) as u8;
            }
        }
        let (src, dst) = buf.split_at_mut(8 * 16);
        copy_block8(dst, src, 16, 16);
        for r in 0..8 {
            for c in 0..8 {
                assert_eq!(buf[(8 + r) * 16 + c], (r * 10 + c) as u8);
            }
        }
    }

    #[test]
    fn fill_block_writes_square() {
        let mut dst = [0u8; 32 * 16];
        fill_block(&mut dst, 0xAA, 32, 8);
        for r in 0..8 {
            for c in 0..8 {
                assert_eq!(dst[r * 32 + c], 0xAA);
            }
            for c in 8..32 {
                assert_eq!(dst[r * 32 + c], 0);
            }
        }
        for r in 8..16 {
            for c in 0..32 {
                assert_eq!(dst[r * 32 + c], 0);
            }
        }
    }

    #[test]
    fn new_decoder_allocates_planes_correctly() {
        let mut h = crate::assets::bink_file::BinkHeader {
            version: BinkVersion::BikI,
            file_size: 1000,
            num_frames: 1,
            largest_frame: 100,
            width: 320,
            height: 240,
            fps_num: 30,
            fps_den: 1,
            video_flags: 0,
            num_audio_tracks: 0,
            audio_tracks: vec![],
            frame_index_offset: 0,
        };
        let d = BinkDecoder::new(&h).unwrap();
        assert_eq!(d.width, 320);
        assert_eq!(d.height, 240);
        assert_eq!(d.color_range, ColorRange::Mpeg);
        assert!(d.cur.y.len() >= 320 * 240);
        assert!(d.cur.u.len() >= 160 * 120);
        h.version = BinkVersion::BikK;
        let dk = BinkDecoder::new(&h).unwrap();
        assert_eq!(dk.color_range, ColorRange::Jpeg);
    }

    #[test]
    fn init_bundle_lengths_matches_ffmpeg_formula() {
        let h = crate::assets::bink_file::BinkHeader {
            version: BinkVersion::BikI,
            file_size: 1000,
            num_frames: 1,
            largest_frame: 100,
            width: 320,
            height: 240,
            fps_num: 30,
            fps_den: 1,
            video_flags: 0,
            num_audio_tracks: 0,
            audio_tracks: vec![],
            frame_index_offset: 0,
        };
        let mut d = BinkDecoder::new(&h).unwrap();
        let bw = (320u32 + 7) >> 3;
        d.init_bundle_lengths(320, bw);
        assert_eq!(d.bundles[Src::BlockTypes as usize].len_bits, 10);
        assert_eq!(d.bundles[Src::SubBlockTypes as usize].len_bits, 10);
    }

    #[test]
    fn huffman_tree_vlc_num_zero_gives_identity() {
        let data = [0x00u8; 4];
        let mut r = crate::assets::bink_bits::BitReader::from_bytes(&data);
        let t = HuffmanTree::read(&mut r).unwrap();
        assert_eq!(t.vlc_num, 0);
        for i in 0..16 {
            assert_eq!(t.syms[i], i as u8);
        }
    }

    #[test]
    fn scale_block_doubles_each_pixel() {
        let mut src = [0u8; 64];
        for i in 0..64 {
            src[i] = i as u8;
        }
        let mut dst = [0u8; 16 * 16];
        scale_block_8x8_to_16x16(&src, &mut dst, 16);

        assert_eq!(dst[0 * 16 + 0], 0);
        assert_eq!(dst[0 * 16 + 1], 0);
        assert_eq!(dst[1 * 16 + 0], 0);
        assert_eq!(dst[1 * 16 + 1], 0);

        // src[3,4] = src[3*8+4] = 28. Doubled into dst rows 6..8 cols 8..10.
        assert_eq!(dst[6 * 16 + 8], 28);
        assert_eq!(dst[7 * 16 + 9], 28);
    }
}
