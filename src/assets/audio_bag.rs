//! audio.idx / audio.bag parser for RA2/YR sound data.
//!
//! RA2 stores most sound effects (unit voices, EVA announcements, weapon sounds)
//! in `audio.bag` — a flat concatenation of raw audio data — indexed by `audio.idx`.
//! YR adds `audiomd.idx` / `audiomd.bag` with additional sounds.
//! Both files live inside `AUDIO.MIX` or `AUDIOMD.MIX` (nested MIX archives).
//!
//! ## Format (version 1)
//!
//! **IDX header** (12 bytes):
//! - `+0x00` u32: magic (unused, not validated by engine)
//! - `+0x04` u32: version (must be 1)
//! - `+0x08` u32: entry_count
//!
//! **IDX entry** (32 bytes on disk for v1):
//! - `+0x00` char[16]: name — null-terminated ASCII, max 15 chars
//! - `+0x10` u32: offset — byte position in .bag file
//! - `+0x14` u32: size — byte count in .bag file
//! - `+0x18` u32: sample_rate — Hz (e.g., 22050)
//! - `+0x1C` u32: flags — bit 0=stereo, bit 2=16-bit, bit 3=IMA ADPCM
//!
//! **BAG file**: raw concatenated audio data. No per-entry headers.
//! Each entry's data starts at `offset` and spans `size` bytes.
//!
//! ## Lookup
//! Entries are sorted by name (uppercase) for case-insensitive binary search.
//! The original engine uses `_stricmp` for comparison.
//!
//! ## Dependency rules
//! - Part of assets/ — standalone parser, no game dependencies.
//! - Uses IMA ADPCM decoder from `aud_file.rs`.

const IDX_HEADER_SIZE: usize = 12;
/// V1 entries are 32 bytes, V2 entries are 36 bytes (extra chunk_size field).
const IDX_ENTRY_SIZE_V1: usize = 32;
const IDX_ENTRY_SIZE_V2: usize = 36;

const FLAG_STEREO: u32 = 0x01;
const FLAG_16BIT: u32 = 0x04;
const FLAG_IMA_ADPCM: u32 = 0x08;

/// Metadata for one sound entry from audio.idx.
#[derive(Debug, Clone)]
pub struct AudioBagEntry {
    /// Sound name (uppercase, max 15 chars).
    pub name: String,
    /// Byte offset into the .bag file.
    pub offset: u32,
    /// Byte count in the .bag file.
    pub size: u32,
    /// Sample rate in Hz (e.g., 22050).
    pub sample_rate: u32,
    /// Raw flags bitfield.
    pub flags: u32,
    /// IMA ADPCM block alignment in bytes (nBlockAlign from WAV fmt).
    /// Zero for non-ADPCM entries or v1 IDX files. When nonzero, the data
    /// is structured as blocks of this size, each starting with a 4-byte
    /// preamble (int16 predictor, u8 step_index, u8 reserved).
    pub chunk_size: u32,
}

impl AudioBagEntry {
    pub fn is_stereo(&self) -> bool {
        self.flags & FLAG_STEREO != 0
    }
    pub fn is_16bit(&self) -> bool {
        self.flags & FLAG_16BIT != 0
    }
    pub fn is_ima_adpcm(&self) -> bool {
        self.flags & FLAG_IMA_ADPCM != 0
    }
    pub fn channels(&self) -> u16 {
        if self.is_stereo() { 2 } else { 1 }
    }
}

/// Decoded audio from a bag entry, ready for conversion to playback format.
pub struct BagAudio {
    /// Signed 16-bit PCM samples (mono or stereo interleaved).
    pub samples_i16: Vec<i16>,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Number of channels (1=mono, 2=stereo).
    pub channels: u16,
}

/// Parsed audio.idx index with the .bag data, providing name-based lookup.
pub struct AudioIndex {
    /// Entries sorted by name (uppercase) for binary search.
    entries: Vec<AudioBagEntry>,
    /// The raw .bag file data.
    bag_data: Vec<u8>,
}

impl AudioIndex {
    /// Parse an audio.idx + audio.bag pair.
    ///
    /// Returns `None` if the idx is malformed or has an unsupported version.
    pub fn from_idx_bag(idx_data: &[u8], bag_data: Vec<u8>) -> Option<Self> {
        if idx_data.len() < IDX_HEADER_SIZE {
            log::warn!("AudioIndex: idx too short ({} bytes)", idx_data.len());
            return None;
        }

        let version = u32::from_le_bytes([idx_data[4], idx_data[5], idx_data[6], idx_data[7]]);
        let entry_size = match version {
            1 => IDX_ENTRY_SIZE_V1,
            2 => IDX_ENTRY_SIZE_V2,
            _ => {
                log::warn!(
                    "AudioIndex: unsupported version {} (expected 1 or 2)",
                    version
                );
                return None;
            }
        };

        let entry_count =
            u32::from_le_bytes([idx_data[8], idx_data[9], idx_data[10], idx_data[11]]) as usize;

        let expected_size = IDX_HEADER_SIZE + entry_count * entry_size;
        if idx_data.len() < expected_size {
            log::warn!(
                "AudioIndex: truncated idx ({} bytes, need {} for {} entries)",
                idx_data.len(),
                expected_size,
                entry_count
            );
            return None;
        }

        let mut entries = Vec::with_capacity(entry_count);
        for i in 0..entry_count {
            let base = IDX_HEADER_SIZE + i * entry_size;
            let name_bytes = &idx_data[base..base + 16];
            let name_end = name_bytes.iter().position(|&b| b == 0).unwrap_or(16);
            let name = String::from_utf8_lossy(&name_bytes[..name_end]).to_ascii_uppercase();

            let offset = read_u32_le(idx_data, base + 16);
            let size = read_u32_le(idx_data, base + 20);
            let sample_rate = read_u32_le(idx_data, base + 24);
            let flags = read_u32_le(idx_data, base + 28);
            // V2 entries have chunk_size at +0x20 (IMA ADPCM block alignment).
            let chunk_size = if entry_size >= IDX_ENTRY_SIZE_V2 {
                read_u32_le(idx_data, base + 32)
            } else {
                0
            };

            entries.push(AudioBagEntry {
                name,
                offset,
                size,
                sample_rate,
                flags,
                chunk_size,
            });
        }

        // Sort by name for binary search (matches original engine behavior).
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        log::info!(
            "AudioIndex: loaded {} entries, bag size {} bytes",
            entries.len(),
            bag_data.len()
        );
        Some(Self { entries, bag_data })
    }

    /// Look up a sound by name (case-insensitive).
    ///
    /// Tries exact name first, then with `.aud` extension appended (some idx files
    /// store entries with the extension, some without).
    /// Returns the entry metadata and a slice of the raw audio data from the .bag.
    pub fn get(&self, name: &str) -> Option<(&AudioBagEntry, &[u8])> {
        // Try exact name first.
        if let Some(result) = self.get_exact(name) {
            return Some(result);
        }
        // Try with .aud extension (entries may include the extension).
        if !name.contains('.') {
            if let Some(result) = self.get_exact(&format!("{}.aud", name)) {
                return Some(result);
            }
        }
        // Try stripping .aud extension (caller may include it but index doesn't).
        if let Some(stem) = name
            .strip_suffix(".aud")
            .or_else(|| name.strip_suffix(".AUD"))
        {
            if let Some(result) = self.get_exact(stem) {
                return Some(result);
            }
        }
        None
    }

    /// Exact binary search by uppercase name.
    fn get_exact(&self, name: &str) -> Option<(&AudioBagEntry, &[u8])> {
        let upper = name.to_ascii_uppercase();
        let idx = self
            .entries
            .binary_search_by(|e| e.name.as_str().cmp(&upper))
            .ok()?;
        let entry = &self.entries[idx];
        let start = entry.offset as usize;
        let end = start + entry.size as usize;
        if end > self.bag_data.len() {
            log::warn!(
                "AudioIndex: entry '{}' overflows bag (offset={}, size={}, bag_len={})",
                name,
                entry.offset,
                entry.size,
                self.bag_data.len()
            );
            return None;
        }
        Some((entry, &self.bag_data[start..end]))
    }

    /// Number of indexed entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return entry names matching a prefix (case-insensitive). For debugging.
    pub fn names_with_prefix(&self, prefix: &str) -> Vec<&str> {
        let upper = prefix.to_ascii_uppercase();
        self.entries
            .iter()
            .filter(|e| e.name.starts_with(&upper))
            .map(|e| e.name.as_str())
            .collect()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Decode raw audio data from a .bag entry based on its format flags.
///
/// Supports raw PCM (8-bit unsigned, 16-bit signed) and IMA ADPCM with
/// per-block preambles (standard Microsoft IMA ADPCM block format).
pub fn decode_bag_audio(entry: &AudioBagEntry, data: &[u8]) -> Option<BagAudio> {
    if data.is_empty() {
        return None;
    }

    let samples_i16 = if entry.is_ima_adpcm() {
        decode_ima_adpcm_blocks(data, entry.channels(), entry.chunk_size)
    } else if entry.is_16bit() {
        // Raw 16-bit signed PCM (little-endian).
        data.chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect()
    } else {
        // Raw 8-bit unsigned PCM → convert to 16-bit signed.
        data.iter().map(|&b| ((b as i16) - 128) * 256).collect()
    };

    Some(BagAudio {
        samples_i16,
        sample_rate: entry.sample_rate,
        channels: entry.channels(),
    })
}

/// Decode IMA ADPCM data with per-block preambles.
///
/// Each block starts with a 4-byte preamble per channel:
///   int16 predictor (LE), u8 step_index (0-88), u8 reserved (must be 0)
/// Followed by nibble data for the remaining block bytes.
///
/// If `block_size` is 0 (v1 IDX or unknown), treats the entire data as one block.
fn decode_ima_adpcm_blocks(data: &[u8], channels: u16, block_size: u32) -> Vec<i16> {
    use super::aud_file::ImaAdpcmState;

    let ch = channels.max(1) as usize;
    let preamble_size = ch * 4; // 4 bytes per channel preamble

    // If no block size specified, try treating entire data as one block.
    let bs = if block_size > 0 {
        block_size as usize
    } else {
        data.len()
    };

    if bs < preamble_size {
        // Block too small for even the preamble — fall back to raw nibble decode.
        let mut state = ImaAdpcmState::new();
        let mut out = Vec::with_capacity(data.len() * 2);
        super::aud_file::decode_ima_adpcm_chunk(data, &mut state, &mut out);
        return out;
    }

    let mut out = Vec::with_capacity(data.len() * 4);
    let mut pos = 0;

    while pos + preamble_size <= data.len() {
        let block_end = (pos + bs).min(data.len());
        let block = &data[pos..block_end];

        if block.len() < preamble_size {
            break;
        }

        if ch == 1 {
            // Mono: single preamble + nibble data.
            let predictor = i16::from_le_bytes([block[0], block[1]]) as i32;
            let step_index = block[2] as i32;
            let _reserved = block[3];

            let mut state = ImaAdpcmState::new();
            state.set_state(predictor, step_index.clamp(0, 88));

            // First output sample is the predictor value itself.
            out.push(predictor as i16);

            // Decode remaining nibble data.
            let nibble_data = &block[preamble_size..];
            super::aud_file::decode_ima_adpcm_chunk(nibble_data, &mut state, &mut out);
        } else {
            // Stereo: two preambles, then interleaved nibble groups.
            let pred_l = i16::from_le_bytes([block[0], block[1]]) as i32;
            let idx_l = block[2] as i32;
            let pred_r = i16::from_le_bytes([block[4], block[5]]) as i32;
            let idx_r = block[6] as i32;

            let mut state_l = ImaAdpcmState::new();
            let mut state_r = ImaAdpcmState::new();
            state_l.set_state(pred_l, idx_l.clamp(0, 88));
            state_r.set_state(pred_r, idx_r.clamp(0, 88));

            // First output frame is the predictor values.
            out.push(pred_l as i16);
            out.push(pred_r as i16);

            // Stereo IMA ADPCM: nibbles are in 4-byte groups per channel,
            // alternating: 4 bytes L, 4 bytes R, 4 bytes L, 4 bytes R...
            let nibble_data = &block[preamble_size..];
            let mut i = 0;
            while i + 8 <= nibble_data.len() {
                // 4 bytes for left channel (8 nibbles = 8 samples).
                let mut l_samples = Vec::with_capacity(8);
                super::aud_file::decode_ima_adpcm_chunk(
                    &nibble_data[i..i + 4],
                    &mut state_l,
                    &mut l_samples,
                );
                // 4 bytes for right channel (8 nibbles = 8 samples).
                let mut r_samples = Vec::with_capacity(8);
                super::aud_file::decode_ima_adpcm_chunk(
                    &nibble_data[i + 4..i + 8],
                    &mut state_r,
                    &mut r_samples,
                );
                // Interleave L/R output.
                for j in 0..l_samples.len().min(r_samples.len()) {
                    out.push(l_samples[j]);
                    out.push(r_samples[j]);
                }
                i += 8;
            }
        }

        pos += bs;
    }

    out
}

/// Read a little-endian u32 from a byte slice at the given offset.
fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid idx with the given entries.
    fn build_idx(entries: &[(&str, u32, u32, u32, u32)]) -> Vec<u8> {
        let mut idx = Vec::new();
        // Header: magic(0) + version(1) + count
        idx.extend_from_slice(&0u32.to_le_bytes()); // magic
        idx.extend_from_slice(&1u32.to_le_bytes()); // version
        idx.extend_from_slice(&(entries.len() as u32).to_le_bytes()); // count
        for &(name, offset, size, rate, flags) in entries {
            let mut name_buf = [0u8; 16];
            let name_bytes = name.as_bytes();
            let copy_len = name_bytes.len().min(15);
            name_buf[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
            idx.extend_from_slice(&name_buf);
            idx.extend_from_slice(&offset.to_le_bytes());
            idx.extend_from_slice(&size.to_le_bytes());
            idx.extend_from_slice(&rate.to_le_bytes());
            idx.extend_from_slice(&flags.to_le_bytes());
        }
        idx
    }

    #[test]
    fn test_parse_valid_idx() {
        let idx = build_idx(&[
            ("testsound", 0, 4, 22050, 0x04), // 16-bit PCM, offset 0, 4 bytes
        ]);
        let bag = vec![0x00, 0x40, 0x00, 0x40]; // two 16-bit samples: 16384, 16384
        let index = AudioIndex::from_idx_bag(&idx, bag).expect("should parse");
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn test_bad_version() {
        let mut idx = build_idx(&[]);
        // Overwrite version to 99 (unsupported — v1 and v2 are valid).
        idx[4..8].copy_from_slice(&99u32.to_le_bytes());
        assert!(AudioIndex::from_idx_bag(&idx, vec![]).is_none());
    }

    #[test]
    fn test_truncated_idx() {
        let idx = vec![0u8; 8]; // too short for header
        assert!(AudioIndex::from_idx_bag(&idx, vec![]).is_none());
    }

    #[test]
    fn test_case_insensitive_lookup() {
        let idx = build_idx(&[
            ("igisea", 0, 2, 22050, 0x00), // 8-bit PCM
        ]);
        let bag = vec![128, 192]; // two 8-bit samples
        let index = AudioIndex::from_idx_bag(&idx, bag).expect("should parse");

        assert!(index.get("igisea").is_some());
        assert!(index.get("IGISEA").is_some());
        assert!(index.get("IgiSea").is_some());
        assert!(index.get("notfound").is_none());
    }

    #[test]
    fn test_decode_pcm_16bit() {
        let entry = AudioBagEntry {
            name: "TEST".into(),
            offset: 0,
            size: 4,
            sample_rate: 22050,
            flags: FLAG_16BIT,
            chunk_size: 0,
        };
        let data: &[u8] = &[0x00, 0x40, 0x00, 0xC0]; // 16384, -16384
        let audio = decode_bag_audio(&entry, data).expect("should decode");
        assert_eq!(audio.samples_i16.len(), 2);
        assert_eq!(audio.samples_i16[0], 16384);
        assert_eq!(audio.samples_i16[1], -16384);
        assert_eq!(audio.channels, 1);
    }

    #[test]
    fn test_decode_pcm_8bit() {
        let entry = AudioBagEntry {
            name: "TEST".into(),
            offset: 0,
            size: 3,
            sample_rate: 22050,
            flags: 0, // 8-bit mono
            chunk_size: 0,
        };
        let data: &[u8] = &[128, 255, 0]; // center, max, min
        let audio = decode_bag_audio(&entry, data).expect("should decode");
        assert_eq!(audio.samples_i16.len(), 3);
        assert_eq!(audio.samples_i16[0], 0); // 128 - 128 = 0
        assert_eq!(audio.samples_i16[1], 127 * 256); // 255 - 128 = 127
        assert_eq!(audio.samples_i16[2], -128 * 256); // 0 - 128 = -128
    }

    #[test]
    fn test_bag_overflow_returns_none() {
        let idx = build_idx(&[
            ("overflow", 0, 100, 22050, 0x00), // size 100 but bag only has 2 bytes
        ]);
        let bag = vec![0, 0];
        let index = AudioIndex::from_idx_bag(&idx, bag).expect("should parse");
        assert!(index.get("overflow").is_none());
    }

    #[test]
    fn test_empty_data_returns_none() {
        let entry = AudioBagEntry {
            name: "EMPTY".into(),
            offset: 0,
            size: 0,
            sample_rate: 22050,
            flags: 0,
            chunk_size: 0,
        };
        assert!(decode_bag_audio(&entry, &[]).is_none());
    }

    #[test]
    fn test_multiple_entries_sorted() {
        let idx = build_idx(&[
            ("zebra", 0, 2, 22050, 0x00),
            ("alpha", 2, 2, 22050, 0x00),
            ("middle", 4, 2, 22050, 0x00),
        ]);
        let bag = vec![128; 6];
        let index = AudioIndex::from_idx_bag(&idx, bag).expect("should parse");
        assert!(index.get("alpha").is_some());
        assert!(index.get("middle").is_some());
        assert!(index.get("zebra").is_some());
    }
}
