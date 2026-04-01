//! Pathfinding system — A* search, zone connectivity, terrain costs, and path smoothing.
//!
//! Combines cell-level A* with zone-aware hierarchical search for fast unreachability
//! detection and corridor-based pruning.
//!
//! TODO(RE): The current split is still only partially aligned with RA2/YR. Terrain-aware
//! zone rebuilds now use recovered nodeIndex -> zoneId semantics, but bridge-layer remap,
//! full hierarchical subzone support, and the exact regular-vs-hierarchical path entry
//! behavior are still pending.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on map/ (terrain, resolved_terrain).
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

// Core A* algorithm, PathGrid, PathCell
mod core;

// Path post-processing
pub mod path_smooth;

// Terrain and movement costs
pub mod cell_entry;
pub mod passability;
pub mod terrain_cost;
pub mod terrain_speed;

// Zone connectivity (flood-fill zones, hierarchy, zone-aware search)
pub(crate) mod zone_build;
pub(crate) mod zone_hierarchy;
pub(crate) mod zone_incremental;
pub mod zone_map;
pub mod zone_search;

// Re-export core types so external code uses crate::sim::pathfinding::PathGrid etc.
pub use self::core::*;

#[cfg(test)]
#[path = "zone_map_tests.rs"]
mod zone_map_tests;
