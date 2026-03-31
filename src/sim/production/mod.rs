//! Minimal production system: credits, build queue, and unit spawning.
//!
//! This is a first playable loop implementation. Split into sub-modules:
//! - `production_types`: shared types, constants, state containers
//! - `production_queue`: queue management, enqueue, tick, cancel
//! - `production_economy`: resource harvesting and credit delivery
//! - `production_placement`: building placement, sell, repair
//! - `production_tech`: tech tree, build options, factory matching, spawn cells

mod production_economy;
mod production_placement;
mod production_queue;
mod production_refinery;
mod production_sell;
mod production_spawn;
mod production_tech;
mod production_types;

// Re-export everything so external code can still use `production::X`.
pub use self::production_economy::is_harvester_type;
pub(crate) use self::production_placement::structure_occupies_cell;
pub use self::production_placement::{
    active_producer_for_owner_category, cycle_active_producer_for_owner_category,
    place_ready_building, placement_preview_for_owner, toggle_pause_for_owner_category,
};
pub use self::production_queue::{
    build_options_for_owner, cancel_by_type_for_owner, cancel_last_for_owner, credits_for_owner,
    enqueue_by_type, enqueue_default_unit_for_owner, has_strict_build_option_for_owner,
    power_balance_for_owner, queue_view_for_owner, rally_point_for_owner,
    ready_buildings_for_owner, seed_resource_nodes_from_overlays, set_rally_point_for_owner,
    theoretical_power_for_owner, tick_production,
};
pub use self::production_sell::{
    eject_destruction_survivors, sell_building, tick_repairs, toggle_repair,
};
pub use self::production_spawn::find_spawn_cell_for_owner;
pub use self::production_tech::{
    foundation_dimensions, is_matching_factory, producer_candidates_for_owner_category,
    structure_satisfies_prerequisite,
};
pub use self::production_types::*;

// Re-exports for external consumers (files outside production/ that previously
// imported private submodules directly).
pub(crate) use self::production_economy::pick_best_resource_node;
pub(in crate::sim) use self::production_queue::credits_entry_for_owner;

// Re-exports used by test sub-modules (via `super::` in test files).
#[cfg(test)]
pub(in crate::sim) use self::production_tech::{
    build_time_base_frames, effective_progress_rate_ppm_for_type,
    effective_time_to_build_frames_for_type,
};

#[cfg(test)]
#[path = "production_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "production_queue_tests.rs"]
mod queue_tests;

#[cfg(test)]
#[path = "production_placement_tests.rs"]
mod placement_tests;
