//! Map loading, terrain, tile system, and theater management.
//!
//! RA2 maps are INI files with binary-encoded sections:
//! - [Map] — metadata (size, theater name)
//! - [IsoMapPack5] — base64-encoded terrain tile data
//! - [OverlayDataPack] — base64-encoded overlay data (ore, gems, walls)
//! - [Structures] / [Units] / [Infantry] — entity placements
//!
//! Each map specifies a theater (temperate, snow, urban, etc.) which determines
//! which terrain tileset (.tmp files) and palette to use.
//!
//! ## Dependency rules
//! - map/ depends on: assets/ (reads .tmp terrain tiles), rules/ (terrain type definitions)
//! - map/ does NOT depend on: sim/, render/, ui/, sidebar/, audio/, net/
//! - trigger_runtime (runtime evaluation) lives in sim/ — map/ only holds static definitions

pub mod actions;
pub mod basic;
pub mod briefing;
pub mod cell_tags;
pub mod entities;
pub mod events;
pub mod houses;
pub mod lat;
pub mod lighting;
pub mod map_file;
pub mod overlay;
pub mod overlay_types;
pub mod preview;
pub mod resolved_terrain;
pub mod tags;
pub mod terrain;
pub mod theater;
pub mod trigger_graph;
pub mod triggers;
pub mod variable_names;
pub mod waypoints;
// Future modules — uncomment as implemented:
// pub mod cell;
