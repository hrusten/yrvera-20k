//! Little-endian bitstream reader + Huffman VLC builder for Bink.
//!
//! Bit packing is LSB-first within each byte (FFmpeg's
//! `BITSTREAM_READER_LE`). VLC tables follow FFmpeg's flat 8-bit lookup
//! layout so `decode_vlc` does a single indexed read per call for typical
//! short codes.
//!
//! ## Dependency rules
//! - Part of assets/ — no dependencies on game modules.

use crate::assets::error::AssetError;

/// Little-endian bitstream reader.
///
/// Bits within each byte are consumed LSB-first: reading 4 bits from a stream
/// starting with the byte `0xA3` yields `0x3` (low nibble), then `0xA`.
/// Multi-byte reads pack later bytes into higher positions.
///
/// Byte index + bit-in-byte are tracked as a single `bit_pos` (counted in bits).
/// `bits_total` caps reads; over-reading returns an `AssetError::BinkError`.
pub struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
    bits_total: usize,
}

impl<'a> BitReader<'a> {
    /// Create a reader over `data` with an explicit bit length (≤ `data.len() * 8`).
    #[inline]
    pub fn new(data: &'a [u8], bits_total: usize) -> Self {
        debug_assert!(bits_total <= data.len() * 8);
        Self {
            data,
            bit_pos: 0,
            bits_total,
        }
    }

    /// Create a reader over the full byte slice.
    #[inline]
    pub fn from_bytes(data: &'a [u8]) -> Self {
        Self::new(data, data.len() * 8)
    }

    /// Bits consumed so far.
    #[inline]
    pub fn pos(&self) -> usize {
        self.bit_pos
    }

    /// Bits remaining. Saturates at 0 so callers can safely compare.
    #[inline]
    pub fn bits_left(&self) -> isize {
        self.bits_total as isize - self.bit_pos as isize
    }

    /// Skip `n` bits. Advances past end-of-stream silently; subsequent reads
    /// will detect EOF.
    #[inline]
    pub fn skip(&mut self, n: usize) {
        self.bit_pos += n;
    }

    /// Rewind by `n` bits. Used by `peek_bits` helper.
    #[inline]
    pub(super) fn rewind(&mut self, n: usize) {
        debug_assert!(n <= self.bit_pos);
        self.bit_pos -= n;
    }

    /// Read one bit as a bool.
    #[inline]
    pub fn read_bit(&mut self) -> Result<bool, AssetError> {
        if self.bits_left() < 1 {
            return Err(AssetError::BinkError {
                reason: "bitstream exhausted".to_string(),
            });
        }
        let byte = self.data[self.bit_pos >> 3];
        let bit = (byte >> (self.bit_pos & 7)) & 1;
        self.bit_pos += 1;
        Ok(bit != 0)
    }

    /// Read `n` bits (1..=32) as a u32. LSB-first.
    #[inline]
    pub fn read_bits(&mut self, n: u32) -> Result<u32, AssetError> {
        debug_assert!(n <= 32, "read_bits called with n > 32");
        if (self.bits_left() as i64) < n as i64 {
            return Err(AssetError::BinkError {
                reason: format!("bitstream exhausted reading {} bits", n),
            });
        }

        let mut result: u64 = 0;
        let mut shift: u32 = 0;
        let mut remaining = n;
        while remaining > 0 {
            let byte_idx = self.bit_pos >> 3;
            let bit_in_byte = (self.bit_pos & 7) as u32;
            let take = (8 - bit_in_byte).min(remaining);
            let byte = self.data[byte_idx] as u64;
            let chunk = (byte >> bit_in_byte) & ((1u64 << take) - 1);
            result |= chunk << shift;
            shift += take;
            self.bit_pos += take as usize;
            remaining -= take;
        }
        Ok(result as u32)
    }

    /// Align `bit_pos` up to the next 32-bit boundary.
    ///
    /// Bink planes and bitstream sections are 32-bit aligned.
    #[inline]
    pub fn align_to_dword(&mut self) {
        let rem = self.bit_pos & 31;
        if rem != 0 {
            self.bit_pos += 32 - rem;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_single_bits_lsb_first() {
        // 0b_1010_0011 = 0xA3. LSB-first reads: 1,1,0,0,0,1,0,1.
        let data = [0xA3u8];
        let mut r = BitReader::from_bytes(&data);
        assert_eq!(r.read_bit().unwrap(), true);
        assert_eq!(r.read_bit().unwrap(), true);
        assert_eq!(r.read_bit().unwrap(), false);
        assert_eq!(r.read_bit().unwrap(), false);
        assert_eq!(r.read_bit().unwrap(), false);
        assert_eq!(r.read_bit().unwrap(), true);
        assert_eq!(r.read_bit().unwrap(), false);
        assert_eq!(r.read_bit().unwrap(), true);
    }

    #[test]
    fn read_bits_4_4_from_one_byte() {
        // 0xA3: low nibble = 0x3, high nibble = 0xA.
        let data = [0xA3u8];
        let mut r = BitReader::from_bytes(&data);
        assert_eq!(r.read_bits(4).unwrap(), 0x3);
        assert_eq!(r.read_bits(4).unwrap(), 0xA);
    }

    #[test]
    fn read_bits_across_byte_boundary() {
        // 0x78 0x56 -> as 16 bits LSB-first = 0x5678.
        let data = [0x78u8, 0x56u8];
        let mut r = BitReader::from_bytes(&data);
        assert_eq!(r.read_bits(16).unwrap(), 0x5678);
    }

    #[test]
    fn read_bits_spanning_three_bytes() {
        // 24 bits: 0x44 0x33 0x22 -> 0x223344
        let data = [0x44u8, 0x33u8, 0x22u8];
        let mut r = BitReader::from_bytes(&data);
        assert_eq!(r.read_bits(24).unwrap(), 0x223344);
    }

    #[test]
    fn read_bits_32() {
        let data = [0x78u8, 0x56, 0x34, 0x12];
        let mut r = BitReader::from_bytes(&data);
        assert_eq!(r.read_bits(32).unwrap(), 0x12345678);
    }

    #[test]
    fn align_to_dword_from_middle() {
        let data = [0xFFu8; 8];
        let mut r = BitReader::from_bytes(&data);
        r.skip(5);
        r.align_to_dword();
        assert_eq!(r.pos(), 32);
        r.skip(1);
        r.align_to_dword();
        assert_eq!(r.pos(), 64);
    }

    #[test]
    fn eof_returns_err() {
        let data = [0x00u8];
        let mut r = BitReader::from_bytes(&data);
        r.skip(8);
        assert!(r.read_bit().is_err());
    }
}
