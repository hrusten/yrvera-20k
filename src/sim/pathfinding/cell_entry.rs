//! Cell entry classification — unified Can_Enter_Cell result codes.
//!
//! The original RA2 engine returns 8 distinct codes when a unit
//! tries to enter a cell. Each code triggers a different movement response.
//! This module centralizes the classification logic that was previously
//! scattered as inline boolean checks in movement.rs.
//!
//! Two-phase design for borrow checker compatibility:
//! - Phase 1 (`check_terrain`): terrain + occupancy presence, no EntityStore needed
//! - Phase 2 (`classify_occupied_cell`): blocker friendship/crush, needs &EntityStore
//!
//! TODO(RE): The stock search-time legality/cost predicate is richer than this runtime
//! movement-side classification. Bridge legality, more terrain/object cases, and the
//! exact cost classes still need to be pulled in from the RE corpus.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/bump_crush, sim/entity_store, sim/locomotor,
//!   sim/pathfinding, map/entities, map/houses, rules/locomotor_type.

use std::collections::BTreeSet;

use super::PathGrid;
use super::terrain_cost::TerrainCostGrid;
use crate::map::entities::EntityCategory;
use crate::map::houses::{self, HouseAllianceMap};
use crate::rules::locomotor_type::{LocomotorKind, MovementZone};
use crate::sim::entity_store::EntityStore;
use crate::sim::movement::bump_crush;
use crate::sim::movement::locomotor::MovementLayer;
use crate::sim::occupancy::OccupancyGrid;

// ---------------------------------------------------------------------------
// Result enums
// ---------------------------------------------------------------------------

/// Result of checking whether a unit can enter a target cell.
///
/// Maps to the original engine's Can_Enter_Cell return codes (0–7). Each variant
/// carries enough context for the movement tick to dispatch the correct
/// response without re-querying the EntityStore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellEntryResult {
    /// Code 0: Cell is passable. Enter freely.
    Clear,
    /// Code 1: Cell contains crushable occupants. Crush and enter.
    Crushable { victims: Vec<u64> },
    /// Code 2: Blocked by a moving friendly unit. Wait, then repath.
    TemporaryBlock { blocker_id: u64 },
    /// Code 3: Bridge ramp transition. Adjust Z, enter with elevation change.
    /// Current implementation still treats this as Clear because bridge-layer state
    /// handling is centralized in movement_bridge.rs.
    BridgeRamp,
    /// Code 4: Friendly stationary unit occupying. Try bump/scatter, or wait.
    OccupiedFriendly { blocker_id: u64 },
    /// Code 5: Enemy unit occupying. Attack blocker while waiting.
    OccupiedEnemy { blocker_id: u64 },
    /// Code 6: Cliff or steep elevation change. Repath or stop.
    Cliff,
    /// Code 7: Terrain impassable (water, building footprint, etc.). Abort.
    Impassable,
}

/// Phase 1 result — terrain and basic occupancy check (no EntityStore needed).
///
/// Computed inside the mutable entity borrow where we cannot also access
/// EntityStore for blocker lookups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerrainCheckResult {
    /// Cell is passable (terrain OK, occupancy clear or sub-cell available).
    Clear,
    /// Terrain impassable for this unit type.
    Impassable,
    /// Reserved by another mover this tick.
    Reserved,
    /// Cell has occupants — needs Phase 2 EntityStore lookup to classify.
    NeedsBlockerCheck,
}

// ---------------------------------------------------------------------------
// Phase 1: terrain + occupancy presence
// ---------------------------------------------------------------------------

/// Check terrain walkability and basic occupancy for a target cell.
///
/// This is Phase 1 of the two-phase cell entry check. It does NOT access
/// EntityStore, so it can run inside a mutable entity borrow.
///
/// For infantry movers, also checks sub-cell availability.
pub fn check_terrain(
    target: (u16, u16),
    target_layer: MovementLayer,
    mover_category: EntityCategory,
    path_grid: Option<&PathGrid>,
    cost_grid: Option<&TerrainCostGrid>,
    reserved_destinations: &BTreeSet<(MovementLayer, u16, u16)>,
    occupancy: &OccupancyGrid,
    reserved_sub_cells: Option<&[u8]>,
) -> TerrainCheckResult {
    let (nx, ny) = target;

    // --- Terrain walkability ---
    let terrain_walkable = match target_layer {
        MovementLayer::Ground => {
            let grid_ok = path_grid.map_or(true, |g| g.is_walkable(nx, ny));
            let cost_ok = cost_grid.map_or(true, |cg| cg.cost_at(nx, ny) > 0);
            grid_ok && cost_ok
        }
        MovementLayer::Bridge => {
            path_grid.is_some_and(|grid| grid.is_walkable_on_layer(nx, ny, MovementLayer::Bridge))
        }
        MovementLayer::Air | MovementLayer::Underground => {
            true
        }
    };
    if !terrain_walkable {
        return TerrainCheckResult::Impassable;
    }

    // --- Reserved destination ---
    if reserved_destinations.contains(&(target_layer, nx, ny)) {
        return TerrainCheckResult::Reserved;
    }

    // --- Occupancy ---
    let occ = occupancy.get(nx, ny);

    if mover_category == EntityCategory::Infantry {
        // Infantry: check sub-cell availability.
        let sub =
            bump_crush::allocate_sub_cell_with_reserved(occ, target_layer, reserved_sub_cells);
        if sub.is_some() {
            return TerrainCheckResult::Clear;
        }
        // No sub-cell available — needs blocker classification.
        return TerrainCheckResult::NeedsBlockerCheck;
    }

    // Vehicle/aircraft/structure: cell must be unoccupied on this layer.
    match occ {
        None => TerrainCheckResult::Clear,
        Some(o) if o.is_empty_on(target_layer) => TerrainCheckResult::Clear,
        Some(_) => TerrainCheckResult::NeedsBlockerCheck,
    }
}

// ---------------------------------------------------------------------------
// Phase 2: blocker classification (needs EntityStore)
// ---------------------------------------------------------------------------

/// Classify an occupied cell's blockers to determine the Can_Enter_Cell code.
///
/// This is Phase 2 — runs outside the mutable entity borrow so it can read
/// blocker properties from EntityStore.
///
/// Check order (current approximation of original engine priority):
/// 1. Crush: if all occupants are crushable → Crushable
/// 2. Blocker friendship: enemy → OccupiedEnemy, friendly → moving/stationary
/// 3. JumpJet override: codes < 7 treated as Clear
///
/// TODO(RE): The recovered candidate predicate also folds in bridge legality and
/// additional terrain/object state before these occupancy outcomes are chosen.
pub fn classify_occupied_cell(
    target: (u16, u16),
    target_layer: MovementLayer,
    mover_id: u64,
    mover_zone: MovementZone,
    mover_omni_crusher: bool,
    mover_owner: &str,
    mover_locomotor: LocomotorKind,
    occupancy: &OccupancyGrid,
    entities: &EntityStore,
    alliances: &HouseAllianceMap,
    interner: &crate::sim::intern::StringInterner,
) -> CellEntryResult {
    // --- Crush check ---
    let victims = bump_crush::collect_crush_victims(
        target,
        occupancy,
        target_layer,
        mover_zone,
        mover_omni_crusher,
        entities,
    );
    if !victims.is_empty()
        && bump_crush::cell_passable_after_crush(
            target,
            occupancy,
            target_layer,
            mover_zone,
            mover_omni_crusher,
            entities,
        )
    {
        return apply_overrides(CellEntryResult::Crushable { victims }, mover_locomotor);
    }

    // --- Find primary blocker ---
    let blocker_id = find_primary_blocker(target, target_layer, mover_id, occupancy);
    let Some(bid) = blocker_id else {
        // No identifiable blocker (shouldn't happen if Phase 1 said NeedsBlockerCheck).
        return apply_overrides(CellEntryResult::Impassable, mover_locomotor);
    };

    // --- Classify blocker ---
    let result = classify_blocker(bid, mover_owner, entities, alliances, interner);
    apply_overrides(result, mover_locomotor)
}

/// Find the primary blocker entity in a cell (first vehicle/structure, or first
/// non-self infantry).
fn find_primary_blocker(
    target: (u16, u16),
    layer: MovementLayer,
    mover_id: u64,
    occupancy: &OccupancyGrid,
) -> Option<u64> {
    let occ = occupancy.get(target.0, target.1)?;
    // Prefer vehicle/structure blockers over infantry.
    if let Some(bid) = occ.blockers(layer).next() {
        return Some(bid);
    }
    // Fall back to first non-self infantry.
    occ.infantry(layer)
        .find(|&(id, _)| id != mover_id)
        .map(|(id, _)| id)
}

/// Classify a single blocker as enemy, friendly-moving, or friendly-stationary.
fn classify_blocker(
    blocker_id: u64,
    mover_owner: &str,
    entities: &EntityStore,
    alliances: &HouseAllianceMap,
    interner: &crate::sim::intern::StringInterner,
) -> CellEntryResult {
    let Some(blocker) = entities.get(blocker_id) else {
        return CellEntryResult::Impassable;
    };
    let is_friendly =
        houses::are_houses_friendly(alliances, mover_owner, interner.resolve(blocker.owner));
    if !is_friendly {
        return CellEntryResult::OccupiedEnemy { blocker_id };
    }
    // Friendly: moving → temporary block, stationary → occupied.
    if blocker.movement_target.is_some() {
        CellEntryResult::TemporaryBlock { blocker_id }
    } else {
        CellEntryResult::OccupiedFriendly { blocker_id }
    }
}

/// Apply locomotor-specific overrides to a cell entry result.
///
/// JumpJet: all codes except Impassable treated as Clear (deep_113 line 861).
fn apply_overrides(result: CellEntryResult, locomotor: LocomotorKind) -> CellEntryResult {
    if locomotor == LocomotorKind::Jumpjet && !matches!(result, CellEntryResult::Impassable) {
        return CellEntryResult::Clear;
    }
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn empty_occ() -> OccupancyGrid {
        OccupancyGrid::new()
    }

    #[test]
    fn test_clear_empty_cell() {
        let result = check_terrain(
            (5, 5),
            MovementLayer::Ground,
            EntityCategory::Unit,
            None,
            None,
            &BTreeSet::new(),
            &empty_occ(),
            None,
        );
        assert_eq!(result, TerrainCheckResult::Clear);
    }

    #[test]
    fn test_impassable_blocked_grid() {
        use crate::sim::pathfinding::PathGrid;
        let mut grid = PathGrid::new(10, 10);
        grid.set_blocked(5, 5, true);
        let result = check_terrain(
            (5, 5),
            MovementLayer::Ground,
            EntityCategory::Unit,
            Some(&grid),
            None,
            &BTreeSet::new(),
            &empty_occ(),
            None,
        );
        assert_eq!(result, TerrainCheckResult::Impassable);
    }

    #[test]
    fn test_reserved_cell() {
        let mut reserved = BTreeSet::new();
        reserved.insert((MovementLayer::Ground, 5, 5));
        let result = check_terrain(
            (5, 5),
            MovementLayer::Ground,
            EntityCategory::Unit,
            None,
            None,
            &reserved,
            &empty_occ(),
            None,
        );
        assert_eq!(result, TerrainCheckResult::Reserved);
    }

    #[test]
    fn test_vehicle_occupied_needs_check() {
        let mut occ = OccupancyGrid::new();
        occ.add(5, 5, 42, MovementLayer::Ground, None);
        let result = check_terrain(
            (5, 5),
            MovementLayer::Ground,
            EntityCategory::Unit,
            None,
            None,
            &BTreeSet::new(),
            &occ,
            None,
        );
        assert_eq!(result, TerrainCheckResult::NeedsBlockerCheck);
    }

    #[test]
    fn test_infantry_subcell_available() {
        let mut occ = OccupancyGrid::new();
        occ.add(5, 5, 10, MovementLayer::Ground, Some(2));
        let result = check_terrain(
            (5, 5),
            MovementLayer::Ground,
            EntityCategory::Infantry,
            None,
            None,
            &BTreeSet::new(),
            &occ,
            None,
        );
        assert_eq!(result, TerrainCheckResult::Clear);
    }

    #[test]
    fn test_infantry_cell_full() {
        let mut occ = OccupancyGrid::new();
        occ.add(5, 5, 10, MovementLayer::Ground, Some(2));
        occ.add(5, 5, 11, MovementLayer::Ground, Some(3));
        occ.add(5, 5, 12, MovementLayer::Ground, Some(4));
        let result = check_terrain(
            (5, 5),
            MovementLayer::Ground,
            EntityCategory::Infantry,
            None,
            None,
            &BTreeSet::new(),
            &occ,
            None,
        );
        assert_eq!(result, TerrainCheckResult::NeedsBlockerCheck);
    }

    #[test]
    fn test_jumpjet_override_clears_non_impassable() {
        let result = apply_overrides(
            CellEntryResult::OccupiedEnemy { blocker_id: 1 },
            LocomotorKind::Jumpjet,
        );
        assert_eq!(result, CellEntryResult::Clear);
    }

    #[test]
    fn test_jumpjet_keeps_impassable() {
        let result = apply_overrides(CellEntryResult::Impassable, LocomotorKind::Jumpjet);
        assert_eq!(result, CellEntryResult::Impassable);
    }

    #[test]
    fn test_non_jumpjet_no_override() {
        let result = apply_overrides(
            CellEntryResult::OccupiedEnemy { blocker_id: 1 },
            LocomotorKind::Drive,
        );
        assert_eq!(result, CellEntryResult::OccupiedEnemy { blocker_id: 1 });
    }
}
