// Constants ported from FFmpeg's libavcodec/binkaudio.c and libavcodec/wma_freqs.c.
// Copyright (c) 2007-2011 Peter Ross (pross@xvid.org)
// Copyright (c) 2009 Daniel Verkamp (daniel@drv.nu)
// Licensed LGPL-2.1-or-later. See LICENSES/LGPL-2.1-or-later.txt in the repo root.

//! Constant tables for Bink audio decoding.
//!
//! These are pure data — no creative content. Copied verbatim from FFmpeg
//! with attribution.

/// WMA critical-band frequencies (Hz). Used to pick the number of audio
/// bands for a given sample rate.
pub const WMA_CRITICAL_FREQS: [u16; 25] = [
    100, 200, 300, 400, 510, 630, 770, 920, 1080, 1270, 1480, 1720, 2000,
    2320, 2700, 3150, 3700, 4400, 5300, 6400, 7700, 9500, 12000, 15500,
    24500,
];

/// RLE run-length lookup. Indexed by the 4-bit RLE escape value when a
/// coefficient-width run begins with bit 1.
pub const RLE_LENGTH_TAB: [u8; 16] = [
    2, 3, 4, 5, 6, 8, 9, 10, 11, 12, 13, 14, 15, 16, 32, 64,
];
