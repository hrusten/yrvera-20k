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
