//! Shared utilities used across the engine.
//!
//! Contains helpers that don't belong to any specific game module:
//! config loading, fixed-point math wrappers, color conversion, rectangles.
//!
//! ## Dependency rules
//! - util/ has NO dependencies on other game modules.
//! - Any module may depend on util/.

pub mod base64;
pub mod config;
pub mod facing_table;
pub mod fixed_math;
pub mod flh_transform;
pub mod lcw;
pub mod lepton;
pub mod logging;
pub mod lzo;
pub mod read_helpers;
// pub mod rect;
// pub mod color;
