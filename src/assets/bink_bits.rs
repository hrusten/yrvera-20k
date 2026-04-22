//! Little-endian bitstream reader + Huffman VLC builder for Bink.
//!
//! Bit packing is LSB-first within each byte (FFmpeg's
//! `BITSTREAM_READER_LE`). VLC tables follow FFmpeg's flat 8-bit lookup
//! layout so `decode_vlc` does a single indexed read per call for typical
//! short codes.
//!
//! ## Dependency rules
//! - Part of assets/ — no dependencies on game modules.

// (empty — implementation in Tasks 3-4)
