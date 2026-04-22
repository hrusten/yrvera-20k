// Ported from FFmpeg's libavformat/bink.c.
// Copyright (c) 2008-2010 Peter Ross (pross@xvid.org)
// Copyright (c) 2009 Daniel Verkamp (daniel@drv.nu)
// Licensed LGPL-2.1-or-later. See LICENSES/LGPL-2.1-or-later.txt in the repo root.

//! Bink 1 container demuxer.
//!
//! Parses the fixed header, audio track descriptors, per-frame offset table,
//! and splits each frame packet into its audio blocks + video bitstream.
//!
//! Only BIKi and BIKk revisions are supported — the only variants that ship
//! in RA2 / Yuri's Revenge cutscenes.
//!
//! ## Dependency rules
//! - Part of assets/ — no dependencies on game modules.
//! - Uses util/read_helpers for binary reading.

use crate::assets::error::AssetError;
use crate::util::read_helpers::read_u32_le;

/// Bink file revision byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinkVersion {
    /// BIKi — standard Bink 1 used by nearly all RA2/YR cutscenes.
    BikI,
    /// BIKk — minor revision: extra 4-byte header field, 0xBB block-type XOR,
    /// whole-plane fill shortcut, JPEG color range. One file in movmd03.mix.
    BikK,
}

impl BinkVersion {
    fn from_tag(tag: u32) -> Result<Self, AssetError> {
        // The 4-byte tag is "BIKi" / "BIKk" stored little-endian.
        match tag {
            0x694B4942 => Ok(Self::BikI), // "BIKi" = 0x42 0x49 0x4B 0x69 little-endian
            0x6B4B4942 => Ok(Self::BikK), // "BIKk" = 0x42 0x49 0x4B 0x6B little-endian
            other => Err(AssetError::BinkError {
                reason: format!(
                    "unsupported Bink signature 0x{:08X} (not BIKi or BIKk)",
                    other
                ),
            }),
        }
    }

    #[inline]
    pub fn revision_byte(self) -> u8 {
        match self {
            Self::BikI => b'i',
            Self::BikK => b'k',
        }
    }
}

/// Flags bits stored in the `video_flags` field (bytes 0x24..0x28).
pub const BINK_FLAG_ALPHA: u32 = 0x00100000;
pub const BINK_FLAG_GRAY: u32 = 0x00020000;

/// Fixed-size part of the Bink file header.
#[derive(Debug, Clone)]
pub struct BinkHeader {
    pub version: BinkVersion,
    /// Real file size in bytes. (On-disk field is `file_size - 8`; we store the real size.)
    pub file_size: u64,
    pub num_frames: u32,
    pub largest_frame: u32,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub video_flags: u32,
    pub num_audio_tracks: u32,
    pub audio_tracks: Vec<AudioTrack>,
    /// Byte offset in the source buffer where the frame index table starts.
    pub frame_index_offset: usize,
}

impl BinkHeader {
    #[inline]
    pub fn has_alpha(&self) -> bool {
        self.video_flags & BINK_FLAG_ALPHA != 0
    }
    #[inline]
    pub fn is_gray(&self) -> bool {
        self.video_flags & BINK_FLAG_GRAY != 0
    }
    #[inline]
    pub fn fps(&self) -> f64 {
        if self.fps_den == 0 {
            0.0
        } else {
            self.fps_num as f64 / self.fps_den as f64
        }
    }
}

/// One audio track descriptor from the header.
#[derive(Debug, Clone, Copy)]
pub struct AudioTrack {
    pub sample_rate: u16,
    pub flags: u16,
    pub track_id: u32,
}

impl AudioTrack {
    /// Bit 0x4000: 16-bit output preferred.
    #[inline]
    pub fn is_16bit(self) -> bool {
        self.flags & 0x4000 != 0
    }
    /// Bit 0x2000: stereo (vs mono).
    #[inline]
    pub fn is_stereo(self) -> bool {
        self.flags & 0x2000 != 0
    }
    /// Bit 0x1000: use DCT variant (vs RDFT).
    #[inline]
    pub fn uses_dct(self) -> bool {
        self.flags & 0x1000 != 0
    }
}

/// Maximum plausible dimensions (from FFmpeg's demuxer).
const MAX_WIDTH: u32 = 7680;
const MAX_HEIGHT: u32 = 4800;
const MAX_FRAMES: u32 = 1_000_000;
const MAX_AUDIO_TRACKS: u32 = 256;

/// Parse the fixed header prefix (no frame index yet).
///
/// Returns the parsed header and the offset in `data` where the audio-track
/// descriptors (if any) start.
pub(crate) fn parse_fixed_header(data: &[u8]) -> Result<(BinkHeader, usize), AssetError> {
    if data.len() < 0x2C {
        return Err(AssetError::BinkError {
            reason: format!(
                "Bink header truncated: {} bytes, need at least 0x2C",
                data.len()
            ),
        });
    }

    let tag = read_u32_le(data, 0x00);
    let version = BinkVersion::from_tag(tag)?;

    let file_size = read_u32_le(data, 0x04) as u64 + 8;
    let num_frames = read_u32_le(data, 0x08);
    let largest_frame = read_u32_le(data, 0x0C);
    // 0x10 is skipped (unused).
    let width = read_u32_le(data, 0x14);
    let height = read_u32_le(data, 0x18);
    let fps_num = read_u32_le(data, 0x1C);
    let fps_den = read_u32_le(data, 0x20);
    let video_flags = read_u32_le(data, 0x24);
    let num_audio_tracks = read_u32_le(data, 0x28);

    // Sanity bounds — matches FFmpeg's probe + header validation.
    if num_frames == 0 || num_frames > MAX_FRAMES {
        return Err(AssetError::BinkError {
            reason: format!("invalid num_frames {}", num_frames),
        });
    }
    if width == 0 || width > MAX_WIDTH {
        return Err(AssetError::BinkError {
            reason: format!("invalid width {}", width),
        });
    }
    if height == 0 || height > MAX_HEIGHT {
        return Err(AssetError::BinkError {
            reason: format!("invalid height {}", height),
        });
    }
    if fps_num == 0 || fps_den == 0 {
        return Err(AssetError::BinkError {
            reason: format!("invalid fps {}/{}", fps_num, fps_den),
        });
    }
    if num_audio_tracks > MAX_AUDIO_TRACKS {
        return Err(AssetError::BinkError {
            reason: format!("too many audio tracks: {}", num_audio_tracks),
        });
    }
    if largest_frame as u64 > file_size {
        return Err(AssetError::BinkError {
            reason: "largest_frame greater than file_size".to_string(),
        });
    }

    let mut next_offset = 0x2C;

    // BIKk has a 4-byte unknown field after num_audio_tracks.
    if version == BinkVersion::BikK {
        if data.len() < next_offset + 4 {
            return Err(AssetError::BinkError {
                reason: "BIKk header truncated: missing extra 4-byte field".to_string(),
            });
        }
        next_offset += 4;
    }

    let header = BinkHeader {
        version,
        file_size,
        num_frames,
        largest_frame,
        width,
        height,
        fps_num,
        fps_den,
        video_flags,
        num_audio_tracks,
        audio_tracks: Vec::new(),
        frame_index_offset: 0,
    };

    Ok((header, next_offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimum valid BIKi header, 0 audio tracks, 1 frame, 320x240 @ 30fps.
    pub(super) fn make_biki_header() -> Vec<u8> {
        let mut h = Vec::with_capacity(0x2C);
        h.extend_from_slice(b"BIKi"); // 0x00
        h.extend_from_slice(&1234u32.to_le_bytes()); // 0x04 file_size - 8
        h.extend_from_slice(&1u32.to_le_bytes()); // 0x08 num_frames
        h.extend_from_slice(&500u32.to_le_bytes()); // 0x0C largest_frame
        h.extend_from_slice(&0u32.to_le_bytes()); // 0x10 unused
        h.extend_from_slice(&320u32.to_le_bytes()); // 0x14 width
        h.extend_from_slice(&240u32.to_le_bytes()); // 0x18 height
        h.extend_from_slice(&30u32.to_le_bytes()); // 0x1C fps_num
        h.extend_from_slice(&1u32.to_le_bytes()); // 0x20 fps_den
        h.extend_from_slice(&0u32.to_le_bytes()); // 0x24 video_flags
        h.extend_from_slice(&0u32.to_le_bytes()); // 0x28 num_audio_tracks
        h
    }

    #[test]
    fn parses_minimum_biki_header() {
        let h = make_biki_header();
        let (hdr, next) = parse_fixed_header(&h).unwrap();
        assert_eq!(hdr.version, BinkVersion::BikI);
        assert_eq!(hdr.num_frames, 1);
        assert_eq!(hdr.width, 320);
        assert_eq!(hdr.height, 240);
        assert_eq!(hdr.fps_num, 30);
        assert_eq!(hdr.fps_den, 1);
        assert_eq!(hdr.num_audio_tracks, 0);
        assert_eq!(next, 0x2C);
        assert_eq!(hdr.file_size, 1234 + 8);
    }

    #[test]
    fn parses_bikk_skips_extra_4_bytes() {
        let mut h = make_biki_header();
        h[3] = b'k';
        h.extend_from_slice(&0u32.to_le_bytes()); // extra BIKk field
        let (hdr, next) = parse_fixed_header(&h).unwrap();
        assert_eq!(hdr.version, BinkVersion::BikK);
        assert_eq!(next, 0x30);
    }

    #[test]
    fn rejects_bad_signature() {
        let mut h = make_biki_header();
        h[3] = b'b'; // BIKb, not supported
        assert!(parse_fixed_header(&h).is_err());
    }

    #[test]
    fn rejects_zero_fps() {
        let mut h = make_biki_header();
        h[0x1C..0x20].copy_from_slice(&0u32.to_le_bytes());
        assert!(parse_fixed_header(&h).is_err());
    }

    #[test]
    fn rejects_gigantic_width() {
        let mut h = make_biki_header();
        h[0x14..0x18].copy_from_slice(&99999u32.to_le_bytes());
        assert!(parse_fixed_header(&h).is_err());
    }

    #[test]
    fn has_alpha_flag() {
        let mut h = make_biki_header();
        h[0x24..0x28].copy_from_slice(&BINK_FLAG_ALPHA.to_le_bytes());
        let (hdr, _) = parse_fixed_header(&h).unwrap();
        assert!(hdr.has_alpha());
        assert!(!hdr.is_gray());
    }
}
