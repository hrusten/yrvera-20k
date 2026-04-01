//! Bridge layer transitions — resolves ground-to-bridge and bridge-to-ground layer changes
//! during cell boundary crossings, and applies bridge render state for smooth visual transitions.
//!
//! Uses **reactive height-based detection**:
//! - `abs(unit_z - cell.ground_level) >= 2` → unit is at bridge level → stay on bridge
//! - `abs(unit_z - cell.ground_level) < 2` → unit is at ground level → pass under
//! - Ramp entry: `src_z == dst_ground + 4` with bridge flag → going UP onto bridge
//! Path layers are NOT used for bridge state decisions; the unit's Z relative to the
//! cell's ground height determines everything at runtime.
//!
//! TODO: Once the underlying bridge Z issues are fixed, this module should switch
//! from height heuristics to using the pathfinder's `next_layer` (path_layers) as the
//! primary bridge/ground decision. The pathfinder already produces correct per-cell
//! Bridge/Ground layer assignments — the `_next_layer` parameter is passed in at every
//! call site but currently ignored. Using it would fix ramp-entry edge cases where
//! the height heuristic fails (unit at ground Z approaching a bridge deck cell with
//! matching ground_level).

use crate::rules::locomotor_type::MovementZone;
use crate::sim::components::{BridgeOccupancy, Position};
use crate::sim::movement::locomotor::{LocomotorState, MovementLayer};
use crate::sim::pathfinding::LayeredPathGrid;
use crate::util::fixed_math::SimFixed;

/// Threshold for ground vs bridge level detection.
/// If `abs(unit_z - cell.ground_level) >= HEIGHT_THRESHOLD`, unit is at bridge level.
const HEIGHT_THRESHOLD: u8 = 2;

/// Height of one ship Z-step in leptons.
/// Computed as `ftol(sin(30 deg) * 256*sqrt(2) * 0.5) = 90`.
#[allow(dead_code)]
pub(super) const SHIP_HEIGHT_STEP: SimFixed = SimFixed::lit("90");

/// Bridge vertical clearance in leptons.
/// Equals `SHIP_HEIGHT_STEP * 4 = 360` -- the Z distance from water surface to bridge deck.
/// Added to braking distance when a ship passes under a bridge cell.
pub(super) const BRIDGE_Z_OFFSET: SimFixed = SimFixed::lit("360");

/// Resolve bridge layer state at a cell boundary crossing using reactive height
/// comparison.
///
/// Compares the unit's current Z to the destination cell's ground height to decide
/// ground vs bridge level. The `_next_layer` parameter from path_layers is available
/// but currently unused — see module-level TODO.
pub(super) fn resolve_cell_transition_bridge_state(
    position: &mut Position,
    layered_grid: Option<&LayeredPathGrid>,
    _next_layer: MovementLayer,
    nx: u16,
    ny: u16,
    _diag_entity_id: u64,
    _diag_source: &str,
) -> (MovementLayer, Option<Option<u8>>) {
    let mut pending_bridge_update: Option<Option<u8>> = None;

    if let Some(grid) = layered_grid {
        if let Some(cell) = grid.cell(nx, ny) {
            if let Some(deck_level) = cell.bridge_deck_level_if_any() {
                // Cell has a bridge deck. Use height comparison to decide layer.
                //   abs(height_param - cell.height_level) < 2 -> ground level
                //   else -> bridge level
                let height_diff = (position.z as i16 - cell.ground_level as i16).unsigned_abs() as u8;
                if height_diff >= HEIGHT_THRESHOLD {
                    // Unit is at bridge level → stay on bridge deck.
                    position.z = deck_level;
                    pending_bridge_update = Some(Some(deck_level));
                    return (MovementLayer::Bridge, pending_bridge_update);
                }
            }
            // No bridge deck, or unit is at ground level → ground layer.
            position.z = cell.ground_level;
            pending_bridge_update = Some(None);
            return (MovementLayer::Ground, pending_bridge_update);
        }
    }

    (_next_layer, pending_bridge_update)
}

pub(super) fn apply_pending_bridge_render_state(
    locomotor: &mut Option<LocomotorState>,
    bridge_occupancy: &mut Option<BridgeOccupancy>,
    on_bridge: &mut bool,
    active_layer: MovementLayer,
    pending_bridge_update: Option<Option<u8>>,
    _diag_entity_id: u64,
) {
    if let Some(loco) = locomotor {
        loco.layer = active_layer;
    }
    *on_bridge = active_layer == MovementLayer::Bridge;
    if let Some(bridge_level) = pending_bridge_update {
        match bridge_level {
            Some(level) => {
                *bridge_occupancy = Some(BridgeOccupancy { deck_level: level });
            }
            None => {
                *bridge_occupancy = None;
            }
        }
    }
}

/// Preemptive bridge detection for units approaching a bridge cell.
///
/// Uses height comparison to decide if the unit should be elevated to bridge
/// deck level. Only fires when bridge_occupancy is not already set and the
/// unit's Z indicates it's at bridge level relative to the next cell.
///
/// The `_next_step_layer` from path_layers is available but currently unused —
/// see module-level TODO.
pub(super) fn apply_bridge_lookahead_if_needed(
    position: &mut Position,
    bridge_occupancy: &mut Option<BridgeOccupancy>,
    on_bridge: &mut bool,
    mover_zone: MovementZone,
    next_step: Option<(u16, u16)>,
    _next_step_layer: MovementLayer,
    layered_grid: Option<&LayeredPathGrid>,
) {
    if mover_zone.is_water_mover() || bridge_occupancy.is_some() {
        return;
    }

    let Some((nx, ny)) = next_step else {
        return;
    };
    if let Some(lg) = layered_grid {
        if let Some(cell) = lg.cell(nx, ny) {
            if let Some(deck) = cell.bridge_deck_level_if_any() {
                // Same height check as resolve: if unit Z is far from ground,
                // it's approaching at bridge level (e.g., coming from a ramp).
                let height_diff =
                    (position.z as i16 - cell.ground_level as i16).unsigned_abs() as u8;
                if height_diff >= HEIGHT_THRESHOLD {
                    *bridge_occupancy = Some(BridgeOccupancy { deck_level: deck });
                    *on_bridge = true;
                    position.z = deck;
                }
            }
        }
    }
}
