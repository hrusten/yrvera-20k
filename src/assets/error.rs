//! Error types for asset parsing.
//!
//! Uses thiserror for structured, descriptive errors. Each asset parser
//! returns AssetError so callers get clear context about what went wrong.
//!
//! ## Dependency rules
//! - Part of assets/ — no dependencies on game modules.

use thiserror::Error;

/// Errors that can occur when parsing RA2 asset files.
///
/// Each variant includes enough context for an LLM or developer to understand
/// exactly what went wrong and where to look for a fix.
#[derive(Debug, Error)]
pub enum AssetError {
    /// File I/O failed (file not found, permission denied, etc.)
    #[error("IO error reading asset file: {0}")]
    Io(#[from] std::io::Error),

    /// PAL file has wrong size. Must be exactly 768 bytes (256 colors * 3 bytes RGB).
    #[error("Invalid PAL file: expected {expected} bytes, got {actual}")]
    InvalidPalSize { expected: usize, actual: usize },

    /// SHP file header is malformed or unrecognized.
    #[error("Invalid SHP file header: {reason}")]
    InvalidShpHeader { reason: String },

    /// Requested a frame index that doesn't exist in the SHP file.
    #[error("SHP frame index {index} out of range (file has {count} frames)")]
    ShpFrameOutOfRange { index: u16, count: u16 },

    /// Binary parsing failed (nom parse error, truncated data, etc.)
    #[error("Parse error in {format} file: {detail}")]
    ParseError { format: String, detail: String },

    /// MIX archive header is invalid or corrupt.
    #[error("Invalid MIX archive: {reason}")]
    InvalidMixHeader { reason: String },

    /// RSA or Blowfish decryption failed during MIX loading.
    #[error("MIX decryption failed: {reason}")]
    MixDecryptionError { reason: String },

    /// Requested file not found in any loaded MIX archive.
    #[error("Asset not found: {name}")]
    AssetNotFound { name: String },

    /// TMP terrain tile file is malformed or unrecognized.
    #[error("Invalid TMP file: {reason}")]
    InvalidTmpFile { reason: String },

    /// Requested a tile index that doesn't exist in the TMP template.
    #[error("TMP tile index {index} out of range (template has {count} cells)")]
    TmpTileOutOfRange { index: usize, count: usize },

    /// VXL voxel model file is malformed or truncated.
    #[error("Invalid VXL file: {reason}")]
    InvalidVxlFile { reason: String },

    /// HVA animation file is malformed or truncated.
    #[error("Invalid HVA file: {reason}")]
    InvalidHvaFile { reason: String },

    /// Bink container or video decoder failed.
    #[error("Bink error: {reason}")]
    BinkError { reason: String },

    /// Requested video packet index out of range in a Bink file.
    #[error("Bink frame index {index} out of range (file has {count} frames)")]
    BinkFrameOutOfRange { index: usize, count: usize },

    /// Bink audio decoder failed (truncated packet, invalid quantizer, etc.)
    #[error("Bink audio error: {reason}")]
    BinkAudioError { reason: String },
}
