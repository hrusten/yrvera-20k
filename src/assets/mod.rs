//! Asset loading and parsing for RA2 file formats.
//!
//! This module contains parsers for all Westwood/EA binary formats used in Red Alert 2.
//! Every parser is built from scratch — no third-party RA2-specific crates.
//!
//! ## Supported formats (implemented or planned)
//! - `.pal` — 256-color palettes (768 bytes: 256 RGB triplets in VGA 6-bit range)
//! - `.shp` — 2D sprite frames (buildings, infantry, cameo icons)
//! - `.mix` — Archive containers (Blowfish encrypted header, contains nested files)
//! - `.vxl` — Voxel models (vehicles, aircraft — 3D pixel grids)
//! - `.hva` — Voxel animations (bone transform matrices per frame)
//! - `.tmp` — Isometric terrain tiles (theater-specific tilesets)
//! - `.csf` — String tables (localized text for UI/EVA)
//! - `.aud` — Audio files (IMA ADPCM compressed)
//! - `.bik` — Bink 1 video (BIKi / BIKk revisions) from RA2/YR MOVIES mixes
//!
//! ## Dependency rules
//! - assets/ has NO dependencies on other game modules.
//! - assets/ is a standalone parser library — it only knows about file formats.
//! - Other modules (rules/, map/, render/, audio/) depend on assets/, never the reverse.

pub mod asset_manager;
pub mod aud_file;
pub mod audio_bag;
pub mod bink_audio;
pub mod bink_audio_data;
pub mod bink_bits;
pub mod bink_data;
pub mod bink_decode;
pub mod bink_file;
pub mod csf_file;
pub mod error;
pub mod fnt_file;
pub mod hva_file;
pub mod mix_archive;
pub mod mix_crypto;
#[cfg(test)]
mod mix_diag_tests;
pub mod mix_hash;
#[cfg(test)]
mod mix_tests;
pub mod pal_file;
pub mod shp_decode;
pub mod shp_file;
pub mod tmp_decode;
pub mod tmp_file;
pub mod vpl_file;
pub mod vxl_decode;
pub mod vxl_file;
pub mod xcc_database;
