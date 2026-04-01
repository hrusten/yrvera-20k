//! Westwood .aud audio file parser and IMA ADPCM decoder.
//!
//! .aud files store compressed audio used for music and sound effects in RA2/YR.
//! Two compression formats are supported:
//! - Format 99: IMA ADPCM (4-bit per sample, used by most music tracks)
//! - Format 1: Westwood Compressed (2/4-bit adaptive, older format)
//!
//! The file is divided into chunks, each with a small header and compressed payload.
//! Decoding produces 16-bit signed PCM samples.
//!
//! ## Dependency rules
//! - Part of assets/ — standalone parser, no game dependencies.

/// Magic value at the start of each audio chunk.
const CHUNK_MAGIC: u32 = 0x0000DEAF;

/// IMA ADPCM step index adjustment table.
/// Indexed by the lower 3 bits of each encoded nibble.
const INDEX_ADJUST: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

/// IMA ADPCM step size table (89 entries).
const STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

/// Parsed .aud file header.
#[derive(Debug, Clone)]
pub struct AudHeader {
    /// Sample rate in Hz (e.g. 22050).
    pub sample_rate: u16,
    /// Size of the compressed data in bytes.
    pub data_size: u32,
    /// Size of the decompressed output in bytes.
    pub output_size: u32,
    /// Flags: bit 0 = stereo, bit 1 = 16-bit samples.
    pub flags: u8,
    /// Compression format: 1 = Westwood Compressed, 99 = IMA ADPCM.
    pub format: u8,
}

impl AudHeader {
    /// Whether the audio is stereo (2 channels).
    pub fn is_stereo(&self) -> bool {
        self.flags & 0x01 != 0
    }

    /// Whether samples are 16-bit (vs 8-bit).
    pub fn is_16bit(&self) -> bool {
        self.flags & 0x02 != 0
    }

    /// Number of audio channels.
    pub fn channels(&self) -> u16 {
        if self.is_stereo() { 2 } else { 1 }
    }
}

/// AUD header size in bytes.
const HEADER_SIZE: usize = 12;

/// Chunk header size in bytes (compressed_size u16 + output_size u16 + magic u32 = 8).
const CHUNK_HEADER_SIZE: usize = 8;

/// Parse an .aud file header from raw bytes.
/// Returns None if the data is too short.
pub fn parse_header(data: &[u8]) -> Option<AudHeader> {
    if data.len() < HEADER_SIZE {
        return None;
    }
    let sample_rate: u16 = u16::from_le_bytes([data[0], data[1]]);
    let data_size: u32 = u32::from_le_bytes([data[2], data[3], data[4], data[5]]);
    let output_size: u32 = u32::from_le_bytes([data[6], data[7], data[8], data[9]]);
    let flags: u8 = data[10];
    let format: u8 = data[11];
    Some(AudHeader {
        sample_rate,
        data_size,
        output_size,
        flags,
        format,
    })
}

/// Decode an entire .aud file into 16-bit signed PCM samples.
///
/// Returns `(header, samples)` on success.
/// Returns None if the file is malformed or uses an unsupported format.
pub fn decode_aud(data: &[u8]) -> Option<(AudHeader, Vec<i16>)> {
    let header: AudHeader = parse_header(data)?;
    if header.format != 99 && header.format != 1 {
        log::warn!("Unsupported .aud format: {}", header.format);
        return None;
    }

    let estimated_samples: usize = header.output_size as usize / 2;
    let mut samples: Vec<i16> = Vec::with_capacity(estimated_samples);
    let mut offset: usize = HEADER_SIZE;
    let mut state = ImaAdpcmState::new();

    while offset + CHUNK_HEADER_SIZE <= data.len() {
        let compressed_size: u16 = u16::from_le_bytes([data[offset], data[offset + 1]]);
        let _chunk_output_size: u16 = u16::from_le_bytes([data[offset + 2], data[offset + 3]]);
        let magic: u32 = u32::from_le_bytes([
            data[offset + 4],
            data[offset + 5],
            data[offset + 6],
            data[offset + 7],
        ]);

        if magic != CHUNK_MAGIC {
            log::warn!(
                "Invalid chunk magic at offset {}: 0x{:08X} (expected 0x{:08X})",
                offset,
                magic,
                CHUNK_MAGIC
            );
            break;
        }

        offset += CHUNK_HEADER_SIZE;
        let chunk_end: usize = (offset + compressed_size as usize).min(data.len());
        let chunk_data: &[u8] = &data[offset..chunk_end];

        if header.format == 99 {
            decode_ima_adpcm_chunk(chunk_data, &mut state, &mut samples);
        } else {
            decode_ws_compressed_chunk(chunk_data, &mut samples);
        }

        offset = chunk_end;
    }

    Some((header, samples))
}

/// IMA ADPCM decoder state (persists across chunks for continuous decoding).
pub(crate) struct ImaAdpcmState {
    index: i32,
    predicted: i32,
}

impl ImaAdpcmState {
    pub(crate) fn new() -> Self {
        Self {
            index: 0,
            predicted: 0,
        }
    }

    /// Initialize state from a block preamble (predictor + step_index).
    pub(crate) fn set_state(&mut self, predicted: i32, index: i32) {
        self.predicted = predicted;
        self.index = index.clamp(0, 88);
    }

    /// Decode a single 4-bit IMA ADPCM nibble into a 16-bit sample.
    pub(crate) fn decode_nibble(&mut self, nibble: u8) -> i16 {
        let step: i32 = STEP_TABLE[self.index as usize];
        let code: u8 = nibble & 0x07;

        // Delta = step * code / 4 + step / 8  (integer arithmetic).
        let mut diff: i32 = step >> 3;
        if code & 0x04 != 0 {
            diff += step;
        }
        if code & 0x02 != 0 {
            diff += step >> 1;
        }
        if code & 0x01 != 0 {
            diff += step >> 2;
        }

        // Sign bit (bit 3 of nibble).
        if nibble & 0x08 != 0 {
            self.predicted -= diff;
        } else {
            self.predicted += diff;
        }

        // Clamp to 16-bit range.
        self.predicted = self.predicted.clamp(-32768, 32767);

        // Update step index.
        self.index += INDEX_ADJUST[code as usize];
        self.index = self.index.clamp(0, 88);

        self.predicted as i16
    }
}

/// Decode a chunk of IMA ADPCM data into PCM samples.
pub(crate) fn decode_ima_adpcm_chunk(data: &[u8], state: &mut ImaAdpcmState, out: &mut Vec<i16>) {
    for &byte in data {
        // Each byte contains two 4-bit samples: low nibble first, then high nibble.
        let lo: u8 = byte & 0x0F;
        let hi: u8 = (byte >> 4) & 0x0F;
        out.push(state.decode_nibble(lo));
        out.push(state.decode_nibble(hi));
    }
}

/// Decode a Westwood Compressed (format 1) chunk.
/// This is a simpler 2/4-bit adaptive scheme used by older .aud files.
fn decode_ws_compressed_chunk(data: &[u8], out: &mut Vec<i16>) {
    // Westwood compressed format: each byte is either a 2-bit or 4-bit encoded delta.
    // For simplicity and because RA2 music primarily uses format 99 (IMA ADPCM),
    // this is a basic implementation.
    let mut sample: i16 = 0;
    let mut i: usize = 0;
    while i < data.len() {
        let byte: u8 = data[i];
        i += 1;

        let count_code: u8 = byte >> 6;
        match count_code {
            // 2-bit delta: 6 values packed in current + next 2 bytes.
            0b00 => {
                // Skip count (byte & 0x3F) samples of silence.
                let skip: usize = (byte & 0x3F) as usize;
                for _ in 0..skip {
                    out.push(sample);
                }
            }
            0b01 => {
                // Low 6 bits = count, next N bytes are raw 8-bit unsigned deltas.
                let count: usize = (byte & 0x3F) as usize;
                for _ in 0..count {
                    if i >= data.len() {
                        break;
                    }
                    let raw: u8 = data[i];
                    i += 1;
                    // Treat as signed offset from 128 (unsigned bias).
                    sample = ((raw as i16) - 128) * 256;
                    out.push(sample);
                }
            }
            0b10 => {
                // 4-bit deltas: low 6 bits = count, each byte has 2 nibbles.
                let count: usize = (byte & 0x3F) as usize;
                for _ in 0..count {
                    if i >= data.len() {
                        break;
                    }
                    let raw: u8 = data[i];
                    i += 1;
                    let lo: i16 = ((raw & 0x0F) as i16) - 8;
                    let hi: i16 = (((raw >> 4) & 0x0F) as i16) - 8;
                    sample = sample.saturating_add(lo * 16);
                    out.push(sample);
                    sample = sample.saturating_add(hi * 16);
                    out.push(sample);
                }
            }
            _ => {
                // 0b11: raw 8-bit sample (single).
                sample = (((byte & 0x3F) as i16) - 32) * 512;
                out.push(sample);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_header_too_short() {
        assert!(parse_header(&[0u8; 5]).is_none());
    }

    #[test]
    fn test_parse_header_valid() {
        // sample_rate=22050 (0x5622), data_size=1000, output_size=4000, flags=2, format=99
        let mut data: Vec<u8> = Vec::new();
        data.extend_from_slice(&22050u16.to_le_bytes()); // sample_rate
        data.extend_from_slice(&1000u32.to_le_bytes()); // data_size
        data.extend_from_slice(&4000u32.to_le_bytes()); // output_size
        data.push(0x02); // flags (16-bit)
        data.push(99); // format (IMA ADPCM)

        let hdr: AudHeader = parse_header(&data).expect("should parse");
        assert_eq!(hdr.sample_rate, 22050);
        assert_eq!(hdr.data_size, 1000);
        assert_eq!(hdr.output_size, 4000);
        assert!(!hdr.is_stereo());
        assert!(hdr.is_16bit());
        assert_eq!(hdr.channels(), 1);
        assert_eq!(hdr.format, 99);
    }

    #[test]
    fn test_ima_adpcm_decode_nibble_zero_is_small_step() {
        let mut state = ImaAdpcmState::new();
        // Nibble 0 with initial state: predicted=0, index=0, step=7.
        // diff = 7/8 = 0 (integer), predicted += 0 = 0.
        let sample: i16 = state.decode_nibble(0);
        assert_eq!(sample, 0);
        // Index should decrease by 1 but clamp to 0.
        assert_eq!(state.index, 0);
    }

    #[test]
    fn test_ima_adpcm_decode_produces_nonzero() {
        let mut state = ImaAdpcmState::new();
        // Nibble 0x07 (code=7, sign=0): diff = 7/8 + 7/2 + 7/4 = 0+3+1 = 4+0 = 4.
        // Actually: diff = step>>3 = 0, +step>>1=3, +step>>2=1 => diff=0+3+1=4?
        // Wait: step=7, diff starts at 7>>3=0.
        // code&4=4 => diff += 7 => diff=7
        // code&2=2 => diff += 3 => diff=10
        // code&1=1 => diff += 1 => diff=11
        // sign=0 => predicted = 0+11 = 11
        let sample: i16 = state.decode_nibble(0x07);
        assert_eq!(sample, 11);
        // Index += INDEX_ADJUST[7] = 8, clamped to 8.
        assert_eq!(state.index, 8);
    }

    #[test]
    fn test_ima_adpcm_negative_nibble() {
        let mut state = ImaAdpcmState::new();
        // Nibble 0x0F (code=7, sign=1): diff=11, predicted = 0-11 = -11.
        let sample: i16 = state.decode_nibble(0x0F);
        assert_eq!(sample, -11);
    }

    #[test]
    fn test_decode_chunk_pairs_nibbles() {
        let mut state = ImaAdpcmState::new();
        let mut out: Vec<i16> = Vec::new();
        // Single byte 0x10: lo=0, hi=1.
        decode_ima_adpcm_chunk(&[0x10], &mut state, &mut out);
        assert_eq!(out.len(), 2);
        // First sample from nibble 0, second from nibble 1.
    }

    #[test]
    fn test_decode_aud_invalid_format() {
        let mut data: Vec<u8> = Vec::new();
        data.extend_from_slice(&22050u16.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.push(0x02);
        data.push(50); // unsupported format
        assert!(decode_aud(&data).is_none());
    }

    #[test]
    fn test_decode_aud_empty_data() {
        let mut data: Vec<u8> = Vec::new();
        data.extend_from_slice(&22050u16.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.push(0x02);
        data.push(99);
        let (hdr, samples) = decode_aud(&data).expect("should decode");
        assert_eq!(hdr.sample_rate, 22050);
        assert!(samples.is_empty());
    }

    #[test]
    fn test_decode_aud_single_chunk() {
        let mut data: Vec<u8> = Vec::new();
        // Header.
        data.extend_from_slice(&22050u16.to_le_bytes()); // sample_rate
        data.extend_from_slice(&10u32.to_le_bytes()); // data_size (approx)
        data.extend_from_slice(&8u32.to_le_bytes()); // output_size
        data.push(0x02); // flags
        data.push(99); // format

        // Chunk header: compressed_size=2, output_size=8, magic=0xDEAF.
        data.extend_from_slice(&2u16.to_le_bytes());
        data.extend_from_slice(&8u16.to_le_bytes());
        data.extend_from_slice(&CHUNK_MAGIC.to_le_bytes());
        // 2 bytes of ADPCM data → 4 samples.
        data.push(0x00);
        data.push(0x00);

        let (_hdr, samples) = decode_aud(&data).expect("should decode chunk");
        assert_eq!(samples.len(), 4);
    }
}
