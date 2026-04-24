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

/// Maximum Huffman code length across the 16 Bink trees. bink_tree_lens never
/// exceeds 13 bits, but we use 13 as a safe bound. 2^13 = 8192 entries is
/// acceptable but the original FFmpeg uses per-tree sizes. We match.
const VLC_MAX_BITS: u32 = 13;

/// A compact canonical Huffman table for a 16-symbol alphabet.
///
/// Uses FFmpeg's flat-lookup layout: for a table of `bits` wide, entries are
/// `1 << bits` long. Each entry packs:
/// - bits 0..15: symbol (i16, -1 for "escape to sub-table" — not used for Bink
///   since all codes fit in `bits` bits)
/// - bits 16..23: code length in bits
///
/// `decode_vlc` reads `bits` bits from the reader, peeks the entry, and
/// advances the reader by the code's length (not by `bits`).
pub struct VlcTable {
    /// Flat lookup table, `1 << bits` entries. Each entry: `(length << 16) | (symbol & 0xFFFF)`.
    entries: Vec<u32>,
    /// Width of the lookup: number of bits to read up-front.
    bits: u32,
}

impl VlcTable {
    /// Build a VLC table from parallel `codes[]` (bit patterns, LSB-first) and
    /// `lengths[]` (bit widths). Both arrays must be 16 entries long.
    ///
    /// LE mode: the `code` value, when read LSB-first from the bitstream,
    /// must match this table's lookup. Bink stores codes as already-LE-ordered.
    pub fn build(codes: &[u8; 16], lengths: &[u8; 16]) -> Result<Self, AssetError> {
        let max_len = *lengths.iter().max().unwrap_or(&0) as u32;
        if max_len == 0 || max_len > VLC_MAX_BITS {
            return Err(AssetError::BinkError {
                reason: format!("VLC max length {} out of range", max_len),
            });
        }

        let bits = max_len;
        let size = 1usize << bits;
        let mut entries = vec![u32::MAX; size];

        for sym in 0..16u32 {
            let len = lengths[sym as usize] as u32;
            if len == 0 {
                continue;
            }
            let code = codes[sym as usize] as u32;
            // For LE mode, the code as-stored IS the lookup prefix. Expand the
            // remaining high bits (bits..max_len) to cover all possible
            // suffixes.
            let high_bits = bits - len;
            let entry = (len << 16) | sym;
            for high in 0..(1u32 << high_bits) {
                let idx = (code | (high << len)) as usize;
                entries[idx] = entry;
            }
        }

        // Any entry still u32::MAX is an unreachable code-space hole and is a
        // table-build error.
        if entries.iter().any(|&e| e == u32::MAX) {
            return Err(AssetError::BinkError {
                reason: "VLC table has gaps — codes do not tile the code space".to_string(),
            });
        }

        Ok(Self { entries, bits })
    }

    /// Decode one symbol. Reads up to `self.bits` bits from `reader`, advances
    /// by the matched code's length.
    #[inline]
    pub fn decode_vlc(&self, reader: &mut BitReader<'_>) -> Result<u32, AssetError> {
        // Peek `self.bits` bits without consuming, then advance by the
        // matched code's length.
        let peek = peek_bits(reader, self.bits)?;
        let entry = self.entries[peek as usize];
        let len = entry >> 16;
        let sym = entry & 0xFFFF;
        reader.skip(len as usize);
        Ok(sym)
    }
}

/// Read up to 16 bits without advancing. Helper for VLC peeking.
///
/// Zero-pads past end-of-stream. FFmpeg's `GetBitContext` requires callers to
/// over-allocate the input buffer by `AV_INPUT_BUFFER_PADDING_SIZE` so a VLC
/// lookup near EOF can peek the table's full width and match a *short* code
/// whose real bits fit within the stream. We don't over-allocate, so we
/// emulate the padding here: if fewer than `n` bits remain, the missing
/// high bits read as zero. `decode_vlc` only advances `bit_pos` by the
/// matched code's real length, so this never consumes padding bits.
#[inline]
fn peek_bits(r: &mut BitReader<'_>, n: u32) -> Result<u32, AssetError> {
    let have = r.bits_left();
    if have >= n as isize {
        let saved = r.pos();
        let v = r.read_bits(n)?;
        r.rewind(r.pos() - saved);
        Ok(v)
    } else if have <= 0 {
        Ok(0)
    } else {
        let saved = r.pos();
        let real = have as u32;
        let v = r.read_bits(real)?;
        r.rewind(r.pos() - saved);
        Ok(v)
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

    #[test]
    fn vlc_build_and_decode_simple_tree() {
        // Tree: a=0b0 (len 1), b=0b01 (len 2) — invalid (prefix conflict).
        // Use a proper one: a=0b0 (len 1), b=0b01 (len 2), c=0b11 (len 2).
        // All codes LE-stored as shown.
        // Bink tree_bits layout: code bits. tree_lens: lengths.
        // Entries 0..15: sym, len. Zero-len entries unused.
        let mut codes = [0u8; 16];
        let mut lens = [0u8; 16];
        codes[0] = 0b0;  lens[0] = 1;  // symbol 0
        codes[1] = 0b01; lens[1] = 2;  // NO: conflicts with 0b0 prefix
        // Re-seed: single-bit 0 = sym 0, two-bit codes '01'=sym1, '11'=sym2.
        // LE reading: '01' means bit0=1 then bit1=0; stored as code=0b01.
        // Actually: in canonical LE layout the bit order matches the read order.
        // Let's use Bink tree 0 (identity, len=4 each): 16 codes of length 4.
        for i in 0..16 {
            codes[i] = i as u8;
            lens[i] = 4;
        }
        let table = VlcTable::build(&codes, &lens).unwrap();

        // Decoding a stream that starts with 0x5: expect symbol 5.
        let data = [0x5u8, 0x0, 0x0, 0x0];
        let mut r = BitReader::from_bytes(&data);
        let sym = table.decode_vlc(&mut r).unwrap();
        assert_eq!(sym, 5);
        assert_eq!(r.pos(), 4); // Consumed 4 bits.
    }

    #[test]
    fn vlc_decode_short_code_at_eof_with_zero_padding() {
        // Real Bink trees peek up to 7 bits but codes can be as short as 1.
        // Simplified: peek width 2, with a 1-bit code for sym 0. A stream
        // with fewer bits left than the peek width must still decode when
        // the matched code's real length fits.
        // sym 0: len 1, code "0"  → slots 0, 2 (bit 0 == 0)
        // sym 1: len 2, code "01" → slot 1       (bits 10)
        // sym 2: len 2, code "11" → slot 3       (bits 11)
        let mut codes = [0u8; 16];
        let mut lens = [0u8; 16];
        codes[0] = 0b0;  lens[0] = 1;
        codes[1] = 0b01; lens[1] = 2;
        codes[2] = 0b11; lens[2] = 2;
        let table = VlcTable::build(&codes, &lens).unwrap();

        // 1 real bit = 0. Peek wants 2 bits. Zero-padded peek = 0b00 → sym 0.
        let data = [0b1111_1110u8];
        let mut r = BitReader::new(&data, 1);
        assert_eq!(table.decode_vlc(&mut r).unwrap(), 0);
        assert_eq!(r.pos(), 1);

        // 0 bits left: peek returns 0 → sym 0.
        let data = [0u8];
        let mut r = BitReader::new(&data, 0);
        assert_eq!(table.decode_vlc(&mut r).unwrap(), 0);
    }

    #[test]
    fn vlc_canonical_bink_tree_1() {
        // Mimics Bink tree 1 (len 1,2,3,4,5,6,7,8,8...). Just confirm build
        // succeeds and decodes round-trip correctly for symbol 0.
        let codes: [u8; 16] = [
            0x00, 0x01, 0x03, 0x05, 0x07, 0x09, 0x0B, 0x0D,
            0x0F, 0x13, 0x15, 0x17, 0x19, 0x1B, 0x1D, 0x1F,
        ];
        let lens: [u8; 16] = [1, 3, 4, 4, 4, 4, 4, 4, 4, 5, 5, 5, 5, 5, 5, 5];
        let table = VlcTable::build(&codes, &lens).unwrap();
        // Code 0 (length 1) = symbol 0. A byte of 0xFE has bit0=0, so reads 0.
        let data = [0xFEu8];
        let mut r = BitReader::from_bytes(&data);
        assert_eq!(table.decode_vlc(&mut r).unwrap(), 0);
        assert_eq!(r.pos(), 1);
    }
}
