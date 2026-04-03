//! Cell occupancy, infantry sub-cell, crush, and scatter logic for ground movement.
//!
//! Extracted from movement.rs to keep that file under 600 lines. Contains:
//! - `CellOccupancy` — tracks what entities occupy each cell (vehicles vs infantry sub-cells)
//! - `OccupancyGrid` — persistent per-cell occupancy (see sim/occupancy.rs)
//! - Sub-cell allocation for infantry (spots 2, 3, 4 — max 3 per cell)
//! - Crush checks: Crusher/CrusherAll movement zones vs crushable/omni_crush_resistant
//! - Scatter: issue movement commands to displace friendly blockers (replaces old teleport "bump")
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/entity_store, sim/game_entity, sim/locomotor,
//!   sim/pathfinding, sim/rng, rules/locomotor_type.

use std::collections::BTreeSet;

use crate::map::entities::EntityCategory;
use crate::rules::locomotor_type::MovementZone;
use crate::sim::entity_store::EntityStore;
use crate::sim::movement::locomotor::MovementLayer;
use crate::sim::occupancy::{CellOccupancy, OccupancyGrid};
use crate::sim::pathfinding::PathGrid;
use crate::sim::rng::SimRng;
use crate::util::fixed_math::{SimFixed, fixed_distance};

/// Functional infantry sub-cell positions. The original engine uses sub-cells
/// 2 (NE), 3 (SW), 4 (SE) — three corners of the isometric diamond. Sub-cells
/// 0 (center) and 1 (NW) are never assigned to infantry by the placement function
/// (FUN_00481180 explicitly skips them: `if (uVar11 != 0 && uVar11 != 1)`).
pub const FUNCTIONAL_SUB_CELLS: [u8; 3] = [2, 3, 4];

/// Maximum infantry that can share one cell (one per functional sub-cell spot).
pub const MAX_INFANTRY_PER_CELL: usize = 3;

/// Preference order tables for infantry sub-cell placement.
/// Indexed by quadrant result (0-4). Each entry lists 4 sub-cell indices to try.
/// The placement loop skips indices 0 and 1, so effective choices are from {2, 3, 4}.
const SUBCELL_PREFERENCE: [[u8; 4]; 5] = [
    [1, 2, 3, 4], // quadrant 0 (center/NW) — not used directly, random table instead
    [0, 2, 3, 4], // quadrant 1 (dead — GetSubCell never returns 1)
    [0, 1, 4, 3], // quadrant 2 (NE) — effective: 4, then 3
    [0, 1, 4, 2], // quadrant 3 (SW) — effective: 4, then 2
    [0, 2, 3, 1], // quadrant 4 (SE) — effective: 2, then 3
];

/// Random rotation tables for sub-cell placement.
/// When quadrant is 0 (center/NW), one of these 4 rotations is picked randomly.
const SUBCELL_RANDOM_ROTATIONS: [[u8; 4]; 4] =
    [[1, 2, 3, 4], [2, 3, 4, 1], [3, 4, 1, 2], [4, 1, 2, 3]];

/// Determine which sub-cell quadrant a lepton position falls in.
///
/// Returns: 0 (center/NW), 2 (NE), 3 (SW), 4 (SE). Never returns 1.
fn get_subcell_quadrant(sub_x: SimFixed, sub_y: SimFixed) -> u8 {
    let center: SimFixed = SimFixed::from_num(128);
    let cx: SimFixed = sub_x - center;
    let cy: SimFixed = sub_y - center;
    let dist: SimFixed = fixed_distance(cx, cy);
    if dist < SimFixed::from_num(60) {
        return 0;
    }
    let mut bits: u8 = if sub_x > center { 1 } else { 0 };
    if sub_y > center {
        bits |= 2;
    }
    if bits == 0 {
        return 0; // NW quadrant → merged with center
    }
    bits + 1
}

/// The 8 directional offsets in isometric cell coordinates (dx, dy).
const NEIGHBOR_OFFSETS: [(i32, i32); 8] = [
    (0, -1),  // N
    (1, -1),  // NE
    (1, 0),   // E
    (1, 1),   // SE
    (0, 1),   // S
    (-1, 1),  // SW
    (-1, 0),  // W
    (-1, -1), // NW
];


/// Build the set of cells blocked by entities for pathfinding purposes.
///
/// RA2 key optimization: **moving friendly units are treated as passable terrain**
/// during path calculation. Only stationary units/buildings and enemy units block.
/// This prevents convoy deadlocks and constant repath thrashing in group movement.
///
/// `mover_owner` is the owner of the unit requesting the path.
/// `alliances` is the house alliance graph for friendship checks.
/// Build layer-separated sets of cells blocked by entities for pathfinding.
///
/// Returns `(ground_blocks, bridge_blocks)`. Units on the bridge layer only
/// block bridge pathfinding, and ground units only block ground pathfinding.
/// This enables units to coexist above and below a bridge simultaneously,
/// matching the original engine's `FirstObject`/`AltObject` dual-layer system.
///
/// RA2 cooperative pathfinding: moving friendly units' path cells get a 4x cost
/// penalty instead of being fully blocked. Stationary units/buildings and enemies
/// hard-block. Verified from gamemd.exe AStar_compute_edge_cost (0x00429830).
///
/// Returns `(ground_blocks, bridge_blocks, penalty_cells)`.
pub fn build_entity_block_sets(
    entities: &EntityStore,
    mover_owner: &str,
    alliances: &crate::map::houses::HouseAllianceMap,
    interner: &crate::sim::intern::StringInterner,
) -> (BTreeSet<(u16, u16)>, BTreeSet<(u16, u16)>, BTreeSet<(u16, u16)>) {
    /// Max path steps to include in penalty set per friendly mover.
    /// Matches gamemd.exe PathfinderClass::UpdateBridgePassability loop limit (24).
    const COOPERATIVE_PATH_LOOKAHEAD: usize = 24;

    let mut ground_blocked: BTreeSet<(u16, u16)> = BTreeSet::new();
    let mut bridge_blocked: BTreeSet<(u16, u16)> = BTreeSet::new();
    let mut penalty_cells: BTreeSet<(u16, u16)> = BTreeSet::new();
    for entity in entities.values() {
        // Entities inside transports don't occupy cells.
        if entity.passenger_role.is_inside_transport() {
            continue;
        }
        let layer = entity.movement_layer_or_ground();
        // Air and underground entities never block ground/bridge pathfinding.
        if matches!(layer, MovementLayer::Air | MovementLayer::Underground) {
            continue;
        }
        let pos = (entity.position.rx, entity.position.ry);
        let target_set = match layer {
            MovementLayer::Bridge => &mut bridge_blocked,
            _ => &mut ground_blocked,
        };
        // Buildings always block (they never move). Always ground layer.
        if entity.category == EntityCategory::Structure {
            ground_blocked.insert(pos);
            continue;
        }
        // Enemy units always block (stationary or not).
        let entity_owner_str = interner.resolve(entity.owner);
        let is_friendly =
            crate::map::houses::are_houses_friendly(alliances, mover_owner, entity_owner_str);
        if !is_friendly {
            target_set.insert(pos);
            continue;
        }
        // Friendly moving units: collect their upcoming path cells as penalty cells
        // (4x cost in A*) instead of hard-blocking.
        if let Some(ref mt) = entity.movement_target {
            penalty_cells.insert(pos);
            for &cell in mt.path[mt.next_index..].iter().take(COOPERATIVE_PATH_LOOKAHEAD) {
                penalty_cells.insert(cell);
            }
            continue;
        }
        // Stationary friendly unit — blocks on its layer.
        target_set.insert(pos);
    }
    (ground_blocked, bridge_blocked, penalty_cells)
}

/// Build a combined block set (both layers merged) for the flat A* pathfinder
/// which doesn't distinguish layers. Returns `(blocks, penalty_cells)`.
pub fn build_entity_block_set(
    entities: &EntityStore,
    mover_owner: &str,
    alliances: &crate::map::houses::HouseAllianceMap,
    interner: &crate::sim::intern::StringInterner,
) -> (BTreeSet<(u16, u16)>, BTreeSet<(u16, u16)>) {
    let (ground, bridge, penalty) =
        build_entity_block_sets(entities, mover_owner, alliances, interner);
    (ground.union(&bridge).copied().collect(), penalty)
}

// ---------------------------------------------------------------------------
// Sub-cell allocation
// ---------------------------------------------------------------------------

/// Find the first available sub-cell in a cell. Returns `None` if the cell is
/// full (3 infantry) or contains a vehicle/structure.
pub fn allocate_sub_cell(occ: Option<&CellOccupancy>, layer: MovementLayer) -> Option<u8> {
    let Some(o) = occ else {
        // Empty cell — first infantry gets sub-cell 2 (NE corner).
        return Some(FUNCTIONAL_SUB_CELLS[0]);
    };
    // Vehicle/structure in cell blocks all sub-cells.
    if o.has_blockers_on(layer) {
        return None;
    }
    let infantry: Vec<(u64, u8)> = o.infantry(layer).collect();
    if infantry.len() >= MAX_INFANTRY_PER_CELL {
        return None;
    }
    // Find first sub-cell not already occupied.
    FUNCTIONAL_SUB_CELLS
        .iter()
        .copied()
        .find(|&spot| !infantry.iter().any(|&(_, s)| s == spot))
}

/// Can infantry enter this cell? True if there's an available sub-cell and no
/// vehicles/structures blocking.
pub fn cell_passable_for_infantry(occ: Option<&CellOccupancy>, layer: MovementLayer) -> bool {
    allocate_sub_cell(occ, layer).is_some()
}

/// Find the first available sub-cell, accounting for both the (stale) occupancy
/// map and sub-cells reserved by earlier movers this tick.
///
/// This prevents duplicate sub-cell assignment when multiple infantry enter
/// the same cell within one simulation tick. Without this, the stale occupancy
/// map shows the cell as empty for all movers, causing overlapping sub-cells
/// and subsequent blocking/repath oscillation.
pub fn allocate_sub_cell_with_reserved(
    occ: Option<&CellOccupancy>,
    layer: MovementLayer,
    reserved: Option<&[u8]>,
) -> Option<u8> {
    // Vehicle/structure in cell blocks all sub-cells.
    if let Some(o) = occ {
        if o.has_blockers_on(layer) {
            return None;
        }
    }
    let infantry: Vec<(u64, u8)> = occ.map_or_else(Vec::new, |o| o.infantry(layer).collect());
    let stale_count: usize = infantry.len();
    let reserved_count: usize = reserved.map_or(0, |v| v.len());
    if stale_count + reserved_count >= MAX_INFANTRY_PER_CELL {
        return None;
    }
    FUNCTIONAL_SUB_CELLS.iter().copied().find(|&spot| {
        let in_stale: bool = infantry.iter().any(|&(_, s)| s == spot);
        let in_reserved: bool = reserved.is_some_and(|v| v.contains(&spot));
        !in_stale && !in_reserved
    })
}

/// Allocate sub-cell using quadrant-based directional preference tables.
///
/// Infantry approaching from a specific direction prefers the sub-cell on that
/// side of the diamond. If occupied, a directional preference table biases the
/// fallback. For center/NW entries, a random rotation picks which sub-cell to
/// try first.
///
/// Use this when the infantry's lepton position (approach direction) and RNG
/// are available. Falls back to `allocate_sub_cell_with_reserved` semantics
/// at call sites without position data (spawning, terrain checks).
pub fn allocate_sub_cell_with_preference(
    occ: Option<&CellOccupancy>,
    layer: MovementLayer,
    reserved: Option<&[u8]>,
    sub_x: SimFixed,
    sub_y: SimFixed,
    rng: &mut SimRng,
) -> Option<u8> {
    // Vehicle/structure blocks all infantry.
    if let Some(o) = occ {
        if o.has_blockers_on(layer) {
            return None;
        }
    }
    let infantry: Vec<(u64, u8)> = occ.map_or_else(Vec::new, |o| o.infantry(layer).collect());
    let stale_count: usize = infantry.len();
    let reserved_count: usize = reserved.map_or(0, |v| v.len());
    if stale_count + reserved_count >= MAX_INFANTRY_PER_CELL {
        return None;
    }

    let is_occupied = |spot: u8| -> bool {
        let in_stale: bool = infantry.iter().any(|&(_, s)| s == spot);
        let in_reserved: bool = reserved.is_some_and(|v| v.contains(&spot));
        in_stale || in_reserved
    };

    let quadrant: u8 = get_subcell_quadrant(sub_x, sub_y);

    // Fast-path: if the quadrant maps directly to a functional sub-cell and it's free,
    // use it without consulting the preference table.
    if quadrant >= 2 && !is_occupied(quadrant) {
        return Some(quadrant);
    }

    // Select preference list: random rotation for center/NW, fixed table otherwise.
    let pref: &[u8; 4] = if quadrant == 0 {
        let rotation: usize = rng.next_range_u32(4) as usize;
        &SUBCELL_RANDOM_ROTATIONS[rotation]
    } else {
        &SUBCELL_PREFERENCE[quadrant as usize]
    };

    // Search preference list, skipping indices 0 and 1 (matching original engine).
    for &spot in pref {
        if spot >= 2 && !is_occupied(spot) {
            return Some(spot);
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Crush logic
// ---------------------------------------------------------------------------

/// Whether `mover_zone` can crush a target with the given properties.
///
/// Crush hierarchy:
///
/// 1. OmniCrushResistant blocks ALL crush (MCVs, Battle Fortress, Slave Miner, T-Rex)
/// 2. OmniCrusher (per-unit flag, only Battle Fortress) crushes anything not
///    OmniCrushResistant, regardless of Crushable flag
/// 3. CrusherAll (MovementZone) crushes walls — for unit crushing it works
///    like OmniCrusher since only BFRT has it and also has OmniCrusher=yes
/// 4. Standard Crusher zones crush only infantry with Crushable=yes
/// 5. Structures and aircraft are NEVER crushable
pub fn can_crush(
    mover_zone: MovementZone,
    mover_omni_crusher: bool,
    target_category: EntityCategory,
    target_crushable: bool,
    target_omni_crush_resistant: bool,
) -> bool {
    // Structures and aircraft are never crushed.
    if matches!(
        target_category,
        EntityCategory::Structure | EntityCategory::Aircraft
    ) {
        return false;
    }
    // OmniCrushResistant blocks everything.
    if target_omni_crush_resistant {
        return false;
    }
    // OmniCrusher (Battle Fortress) crushes any non-resistant mobile entity.
    if mover_omni_crusher {
        return true;
    }

    match mover_zone {
        // CrusherAll zone handles wall crushing in pathfinding; for unit crush
        // it behaves like OmniCrusher (in vanilla YR only BFRT has both).
        MovementZone::CrusherAll => true,
        // Standard crushers can only crush infantry with Crushable=yes.
        MovementZone::Crusher
        | MovementZone::AmphibiousCrusher
        | MovementZone::Destroyer
        | MovementZone::AmphibiousDestroyer
        | MovementZone::InfantryDestroyer => {
            target_category == EntityCategory::Infantry && target_crushable
        }
        // Non-crusher zones cannot crush anything.
        _ => false,
    }
}

/// Collect entity IDs in a cell that the mover would crush on entry.
///
/// Returns an empty vec if the mover can't crush anything there.
pub fn collect_crush_victims(
    cell: (u16, u16),
    occupancy: &OccupancyGrid,
    layer: MovementLayer,
    mover_zone: MovementZone,
    mover_omni_crusher: bool,
    entities: &EntityStore,
) -> Vec<u64> {
    let Some(occ) = occupancy.get(cell.0, cell.1) else {
        return Vec::new();
    };
    let mut victims: Vec<u64> = Vec::new();

    // Check infantry occupants.
    for (eid, _sub) in occ.infantry(layer) {
        if let Some(e) = entities.get(eid) {
            if can_crush(
                mover_zone,
                mover_omni_crusher,
                e.category,
                e.crushable,
                e.omni_crush_resistant,
            ) {
                victims.push(eid);
            }
        }
    }
    // Check vehicle/structure occupants (only OmniCrusher/CrusherAll can crush vehicles).
    for eid in occ.blockers(layer) {
        if let Some(e) = entities.get(eid) {
            if can_crush(
                mover_zone,
                mover_omni_crusher,
                e.category,
                e.crushable,
                e.omni_crush_resistant,
            ) {
                victims.push(eid);
            }
        }
    }

    victims
}

/// Check whether a mover can enter a cell after crushing all occupants.
///
/// Returns `true` if the mover can crush everything in the cell (i.e. the cell
/// would become empty after crush kills are applied).
pub fn cell_passable_after_crush(
    cell: (u16, u16),
    occupancy: &OccupancyGrid,
    layer: MovementLayer,
    mover_zone: MovementZone,
    mover_omni_crusher: bool,
    entities: &EntityStore,
) -> bool {
    let Some(occ) = occupancy.get(cell.0, cell.1) else {
        return true; // empty cell
    };
    // All blockers must be crushable.
    for eid in occ.blockers(layer) {
        if let Some(e) = entities.get(eid) {
            if !can_crush(
                mover_zone,
                mover_omni_crusher,
                e.category,
                e.crushable,
                e.omni_crush_resistant,
            ) {
                return false;
            }
        }
    }
    // All infantry must be crushable.
    for (eid, _) in occ.infantry(layer) {
        if let Some(e) = entities.get(eid) {
            if !can_crush(
                mover_zone,
                mover_omni_crusher,
                e.category,
                e.crushable,
                e.omni_crush_resistant,
            ) {
                return false;
            }
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Scatter displacement (replaces old "bump" teleport)
// ---------------------------------------------------------------------------
//
// The original engine uses CellClass::Scatter_Objects to tell occupants to
// move out of the way. All 6 locomotor call sites pass force=1 with a
// NullCoord, which triggers UnitClass::Scatter Branch A: random direction,
// Set_Destination only (no mission change). The blocker walks away via its
// normal locomotor — it is never teleported.
//
// Our implementation: find a walkable, unoccupied adjacent cell and issue
// the blocker a 1-cell movement command via `issue_direct_move`.

/// Try to scatter a blocker to an adjacent cell by issuing a movement command.
///
/// Matches the original engine's movement scatter (Branch A — NullCoord):
/// search 8 neighbors starting from a random direction, pick the first
/// walkable + unoccupied + unreserved cell, issue the blocker a movement
/// order to walk there.
///
/// Returns `true` if the blocker was given a scatter movement command.
pub fn scatter_blocker(
    entities: &mut EntityStore,
    blocker_id: u64,
    path_grid: Option<&PathGrid>,
    occupancy: &OccupancyGrid,
    reserved: &BTreeSet<(MovementLayer, u16, u16)>,
    layer: MovementLayer,
    rng: &mut SimRng,
) -> bool {
    // Read blocker properties (immutable borrow).
    let Some(blocker) = entities.get(blocker_id) else {
        return false;
    };
    // Don't scatter a blocker that's already moving.
    if blocker.movement_target.is_some() {
        return false;
    }
    let bpos = (blocker.position.rx, blocker.position.ry);
    let speed = blocker
        .locomotor
        .as_ref()
        .map(|l| l.speed_multiplier * crate::util::fixed_math::SimFixed::from_num(1024))
        .unwrap_or(crate::util::fixed_math::SimFixed::from_num(1024));

    // Find a valid adjacent cell. Random start direction matches Branch A.
    let start_dir = rng.next_range_u32(8) as usize;
    let mut target: Option<(u16, u16)> = None;

    for i in 0..8 {
        let dir = (start_dir + i) % 8;
        let (dx, dy) = NEIGHBOR_OFFSETS[dir];
        let nx = bpos.0 as i32 + dx;
        let ny = bpos.1 as i32 + dy;
        if nx < 0 || ny < 0 {
            continue;
        }
        let (nx, ny) = (nx as u16, ny as u16);

        // Must be walkable terrain.
        if let Some(grid) = path_grid {
            if !grid.is_walkable(nx, ny) {
                continue;
            }
        }
        // Must not be occupied by vehicles/structures. Infantry sub-cells OK.
        if let Some(occ) = occupancy.get(nx, ny) {
            if occ.has_blockers_on(layer) {
                continue;
            }
        }
        // Must not be reserved by another mover this tick.
        if reserved.contains(&(layer, nx, ny)) {
            continue;
        }
        target = Some((nx, ny));
        break;
    }

    let Some(dest) = target else {
        return false;
    };

    // Issue a 1-cell movement command. The blocker walks there via normal
    // locomotor processing — no teleport.
    crate::sim::movement::movement_commands::issue_direct_move(entities, blocker_id, dest, speed)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::game_entity::GameEntity;

    fn infantry(id: u64, rx: u16, ry: u16, sub: u8) -> GameEntity {
        let mut e = GameEntity::test_default(id, "E1", "Allies", rx, ry);
        e.category = EntityCategory::Infantry;
        e.sub_cell = Some(sub);
        e.crushable = true;
        e
    }

    fn vehicle(id: u64, rx: u16, ry: u16) -> GameEntity {
        let mut e = GameEntity::test_default(id, "MTNK", "Allies", rx, ry);
        e.category = EntityCategory::Unit;
        e.crushable = false;
        e
    }

    /// Helper: build an OccupancyGrid from a set of entity descriptions.
    fn make_occ(entries: &[(u16, u16, u64, MovementLayer, Option<u8>)]) -> OccupancyGrid {
        let mut grid = OccupancyGrid::new();
        for &(rx, ry, eid, layer, sub) in entries {
            grid.add(rx, ry, eid, layer, sub);
        }
        grid
    }

    // -- can_crush tests --

    #[test]
    fn test_crusher_crushes_crushable_infantry() {
        assert!(can_crush(
            MovementZone::Crusher,
            false, // omni_crusher
            EntityCategory::Infantry,
            true,
            false,
        ));
    }

    #[test]
    fn test_crusher_cannot_crush_non_crushable_infantry() {
        assert!(!can_crush(
            MovementZone::Crusher,
            false, // omni_crusher
            EntityCategory::Infantry,
            false,
            false,
        ));
    }

    #[test]
    fn test_crusher_all_crushes_non_crushable_infantry() {
        assert!(can_crush(
            MovementZone::CrusherAll,
            false, // omni_crusher
            EntityCategory::Infantry,
            false,
            false,
        ));
    }

    #[test]
    fn test_crusher_all_crushes_vehicles() {
        assert!(can_crush(
            MovementZone::CrusherAll,
            false, // omni_crusher
            EntityCategory::Unit,
            false,
            false,
        ));
    }

    #[test]
    fn test_omni_crush_resistant_blocks_all() {
        assert!(!can_crush(
            MovementZone::CrusherAll,
            true, // omni_crusher
            EntityCategory::Infantry,
            true,
            true, // omni_crush_resistant
        ));
    }

    #[test]
    fn test_structures_never_crushable() {
        assert!(!can_crush(
            MovementZone::CrusherAll,
            true, // omni_crusher
            EntityCategory::Structure,
            true,
            false,
        ));
    }

    #[test]
    fn test_crusher_cannot_crush_vehicles() {
        assert!(!can_crush(
            MovementZone::Crusher,
            false, // omni_crusher
            EntityCategory::Unit,
            false,
            false,
        ));
    }

    #[test]
    fn test_normal_zone_cannot_crush() {
        assert!(!can_crush(
            MovementZone::Normal,
            false, // omni_crusher
            EntityCategory::Infantry,
            true,
            false,
        ));
    }

    // -- sub-cell allocation tests --

    #[test]
    fn test_allocate_sub_cell_empty_cell() {
        // No occupancy entry → first spot (2 = NE corner).
        assert_eq!(allocate_sub_cell(None, MovementLayer::Ground), Some(2));
    }

    #[test]
    fn test_allocate_sub_cell_one_infantry() {
        let grid = make_occ(&[(5, 5, 1, MovementLayer::Ground, Some(2))]);
        let occ = grid.get(5, 5).unwrap();
        assert_eq!(allocate_sub_cell(Some(occ), MovementLayer::Ground), Some(3));
    }

    #[test]
    fn test_allocate_sub_cell_two_infantry() {
        let grid = make_occ(&[
            (5, 5, 1, MovementLayer::Ground, Some(2)),
            (5, 5, 2, MovementLayer::Ground, Some(3)),
        ]);
        let occ = grid.get(5, 5).unwrap();
        assert_eq!(allocate_sub_cell(Some(occ), MovementLayer::Ground), Some(4));
    }

    #[test]
    fn test_allocate_sub_cell_full() {
        let grid = make_occ(&[
            (5, 5, 1, MovementLayer::Ground, Some(2)),
            (5, 5, 2, MovementLayer::Ground, Some(3)),
            (5, 5, 3, MovementLayer::Ground, Some(4)),
        ]);
        let occ = grid.get(5, 5).unwrap();
        assert_eq!(allocate_sub_cell(Some(occ), MovementLayer::Ground), None);
    }

    #[test]
    fn test_vehicle_blocks_all_sub_cells() {
        let grid = make_occ(&[(5, 5, 99, MovementLayer::Ground, None)]);
        let occ = grid.get(5, 5).unwrap();
        assert_eq!(allocate_sub_cell(Some(occ), MovementLayer::Ground), None);
    }

    #[test]
    fn test_cell_passable_for_infantry_empty() {
        assert!(cell_passable_for_infantry(None, MovementLayer::Ground));
    }

    #[test]
    fn test_cell_passable_for_infantry_with_vehicle() {
        let grid = make_occ(&[(5, 5, 1, MovementLayer::Ground, None)]);
        let occ = grid.get(5, 5).unwrap();
        assert!(!cell_passable_for_infantry(Some(occ), MovementLayer::Ground));
    }

    // -- collect_crush_victims tests --

    #[test]
    fn test_collect_crush_victims_infantry() {
        let mut store = EntityStore::new();
        let inf = infantry(1, 5, 5, 2);
        store.insert(inf);

        let grid = make_occ(&[(5, 5, 1, MovementLayer::Ground, Some(2))]);

        let victims = collect_crush_victims(
            (5, 5),
            &grid,
            MovementLayer::Ground,
            MovementZone::Crusher,
            false,
            &store,
        );
        assert_eq!(victims, vec![1]);
    }

    #[test]
    fn test_collect_crush_victims_non_crushable() {
        let mut store = EntityStore::new();
        let mut inf = infantry(1, 5, 5, 2);
        inf.crushable = false;
        store.insert(inf);

        let grid = make_occ(&[(5, 5, 1, MovementLayer::Ground, Some(2))]);

        let victims = collect_crush_victims(
            (5, 5),
            &grid,
            MovementLayer::Ground,
            MovementZone::Crusher,
            false,
            &store,
        );
        assert!(victims.is_empty());
    }

    // -- scatter_blocker tests --

    #[test]
    fn test_scatter_blocker_issues_movement() {
        let grid = PathGrid::new(10, 10);
        let occupancy = OccupancyGrid::new();
        let reserved: BTreeSet<(MovementLayer, u16, u16)> = BTreeSet::new();
        let mut rng = SimRng::new(42);

        let mut store = EntityStore::new();
        let v = vehicle(1, 5, 5);
        store.insert(v);

        let result = scatter_blocker(
            &mut store,
            1,
            Some(&grid),
            &occupancy,
            &reserved,
            MovementLayer::Ground,
            &mut rng,
        );
        assert!(result, "scatter_blocker should succeed with open cells");

        // Blocker should now have a movement_target (walking, not teleported).
        let e = store.get(1).unwrap();
        assert!(
            e.movement_target.is_some(),
            "Blocker should have a movement command"
        );
        // Position should NOT have changed yet — blocker walks on next tick.
        assert_eq!(e.position.rx, 5);
        assert_eq!(e.position.ry, 5);
    }

    #[test]
    fn test_scatter_blocker_all_blocked() {
        let grid = PathGrid::new(3, 3);
        let mut occupancy = OccupancyGrid::new();
        for &(dx, dy) in &NEIGHBOR_OFFSETS {
            let nx = (1 + dx) as u16;
            let ny = (1 + dy) as u16;
            occupancy.add(nx, ny, 100, MovementLayer::Ground, None);
        }
        let reserved: BTreeSet<(MovementLayer, u16, u16)> = BTreeSet::new();
        let mut rng = SimRng::new(42);

        let mut store = EntityStore::new();
        let v = vehicle(1, 1, 1);
        store.insert(v);

        let result = scatter_blocker(
            &mut store,
            1,
            Some(&grid),
            &occupancy,
            &reserved,
            MovementLayer::Ground,
            &mut rng,
        );
        assert!(!result, "scatter_blocker should fail when all blocked");
        assert!(store.get(1).unwrap().movement_target.is_none());
    }

    #[test]
    fn test_scatter_blocker_skips_already_moving() {
        let grid = PathGrid::new(10, 10);
        let occupancy = OccupancyGrid::new();
        let reserved: BTreeSet<(MovementLayer, u16, u16)> = BTreeSet::new();
        let mut rng = SimRng::new(42);

        let mut store = EntityStore::new();
        let mut v = vehicle(1, 5, 5);
        v.movement_target = Some(crate::sim::components::MovementTarget {
            path: vec![(5, 5), (6, 5)],
            path_layers: vec![MovementLayer::Ground; 2],
            next_index: 1,
            speed: crate::util::fixed_math::SimFixed::from_num(1024),
            ..Default::default()
        });
        store.insert(v);

        let result = scatter_blocker(
            &mut store,
            1,
            Some(&grid),
            &occupancy,
            &reserved,
            MovementLayer::Ground,
            &mut rng,
        );
        assert!(
            !result,
            "scatter_blocker should not scatter already-moving unit"
        );
    }

    #[test]
    fn test_scatter_deterministic() {
        let grid = PathGrid::new(10, 10);
        let occupancy = OccupancyGrid::new();
        let reserved: BTreeSet<(MovementLayer, u16, u16)> = BTreeSet::new();

        let mut store1 = EntityStore::new();
        store1.insert(vehicle(1, 5, 5));
        let mut rng1 = SimRng::new(42);
        scatter_blocker(
            &mut store1,
            1,
            Some(&grid),
            &occupancy,
            &reserved,
            MovementLayer::Ground,
            &mut rng1,
        );

        let mut store2 = EntityStore::new();
        store2.insert(vehicle(1, 5, 5));
        let mut rng2 = SimRng::new(42);
        scatter_blocker(
            &mut store2,
            1,
            Some(&grid),
            &occupancy,
            &reserved,
            MovementLayer::Ground,
            &mut rng2,
        );

        let t1 = store1.get(1).unwrap().movement_target.as_ref().unwrap();
        let t2 = store2.get(1).unwrap().movement_target.as_ref().unwrap();
        assert_eq!(t1.path, t2.path, "Scatter must be deterministic");
    }

    // -- allocate_sub_cell_with_reserved tests --

    #[test]
    fn test_allocate_with_reserved_empty_cell_no_reservations() {
        assert_eq!(
            allocate_sub_cell_with_reserved(None, MovementLayer::Ground, None),
            Some(2)
        );
    }

    #[test]
    fn test_allocate_with_reserved_skips_reserved_spot() {
        let reserved: Vec<u8> = vec![2];
        assert_eq!(
            allocate_sub_cell_with_reserved(None, MovementLayer::Ground, Some(&reserved)),
            Some(3)
        );
    }

    #[test]
    fn test_allocate_with_reserved_full_from_reservations() {
        let reserved: Vec<u8> = vec![2, 3, 4];
        assert_eq!(
            allocate_sub_cell_with_reserved(None, MovementLayer::Ground, Some(&reserved)),
            None
        );
    }

    #[test]
    fn test_allocate_with_reserved_full_mixed() {
        let grid = make_occ(&[
            (5, 5, 1, MovementLayer::Ground, Some(2)),
            (5, 5, 2, MovementLayer::Ground, Some(3)),
        ]);
        let occ = grid.get(5, 5).unwrap();
        let reserved: Vec<u8> = vec![4];
        assert_eq!(
            allocate_sub_cell_with_reserved(
                Some(occ),
                MovementLayer::Ground,
                Some(&reserved)
            ),
            None
        );
    }

    #[test]
    fn test_allocate_with_reserved_vehicle_blocks() {
        let grid = make_occ(&[(5, 5, 99, MovementLayer::Ground, None)]);
        let occ = grid.get(5, 5).unwrap();
        assert_eq!(
            allocate_sub_cell_with_reserved(Some(occ), MovementLayer::Ground, None),
            None
        );
    }

    // -- quadrant detection tests --

    #[test]
    fn test_quadrant_center() {
        // Distance from (128,128) is 0 — well within 60-lepton threshold.
        assert_eq!(
            get_subcell_quadrant(SimFixed::from_num(128), SimFixed::from_num(128)),
            0
        );
    }

    #[test]
    fn test_quadrant_near_center() {
        // (150, 140): distance = sqrt(22^2 + 12^2) ≈ 25 — within 60-lepton threshold.
        assert_eq!(
            get_subcell_quadrant(SimFixed::from_num(150), SimFixed::from_num(140)),
            0
        );
    }

    #[test]
    fn test_quadrant_nw_returns_zero() {
        // (40, 40): X<=128, Y<=128 → NW quadrant → returns 0 (merged with center).
        assert_eq!(
            get_subcell_quadrant(SimFixed::from_num(40), SimFixed::from_num(40)),
            0
        );
    }

    #[test]
    fn test_quadrant_ne() {
        // (200, 40): X>128, Y<=128 → bits=1 → returns 2 (NE).
        assert_eq!(
            get_subcell_quadrant(SimFixed::from_num(200), SimFixed::from_num(40)),
            2
        );
    }

    #[test]
    fn test_quadrant_sw() {
        // (40, 200): X<=128, Y>128 → bits=2 → returns 3 (SW).
        assert_eq!(
            get_subcell_quadrant(SimFixed::from_num(40), SimFixed::from_num(200)),
            3
        );
    }

    #[test]
    fn test_quadrant_se() {
        // (200, 200): X>128, Y>128 → bits=3 → returns 4 (SE).
        assert_eq!(
            get_subcell_quadrant(SimFixed::from_num(200), SimFixed::from_num(200)),
            4
        );
    }

    // -- preference-aware allocation tests --

    #[test]
    fn test_preference_ne_entry_fast_path() {
        let mut rng = SimRng::new(42);
        let result = allocate_sub_cell_with_preference(
            None,
            MovementLayer::Ground,
            None,
            SimFixed::from_num(200),
            SimFixed::from_num(40),
            &mut rng,
        );
        assert_eq!(result, Some(2));
    }

    #[test]
    fn test_preference_ne_entry_occupied_fallback() {
        let grid = make_occ(&[(5, 5, 1, MovementLayer::Ground, Some(2))]);
        let occ = grid.get(5, 5).unwrap();
        let mut rng = SimRng::new(42);
        let result = allocate_sub_cell_with_preference(
            Some(occ),
            MovementLayer::Ground,
            None,
            SimFixed::from_num(200),
            SimFixed::from_num(40),
            &mut rng,
        );
        assert_eq!(result, Some(4));
    }

    #[test]
    fn test_preference_sw_entry() {
        let mut rng = SimRng::new(42);
        let result = allocate_sub_cell_with_preference(
            None,
            MovementLayer::Ground,
            None,
            SimFixed::from_num(40),
            SimFixed::from_num(200),
            &mut rng,
        );
        assert_eq!(result, Some(3));
    }

    #[test]
    fn test_preference_sw_entry_occupied_fallback() {
        let grid = make_occ(&[(5, 5, 1, MovementLayer::Ground, Some(3))]);
        let occ = grid.get(5, 5).unwrap();
        let mut rng = SimRng::new(42);
        let result = allocate_sub_cell_with_preference(
            Some(occ),
            MovementLayer::Ground,
            None,
            SimFixed::from_num(40),
            SimFixed::from_num(200),
            &mut rng,
        );
        assert_eq!(result, Some(4));
    }

    #[test]
    fn test_preference_se_entry() {
        let mut rng = SimRng::new(42);
        let result = allocate_sub_cell_with_preference(
            None,
            MovementLayer::Ground,
            None,
            SimFixed::from_num(200),
            SimFixed::from_num(200),
            &mut rng,
        );
        assert_eq!(result, Some(4));
    }

    #[test]
    fn test_preference_se_entry_occupied_fallback() {
        let grid = make_occ(&[(5, 5, 1, MovementLayer::Ground, Some(4))]);
        let occ = grid.get(5, 5).unwrap();
        let mut rng = SimRng::new(42);
        let result = allocate_sub_cell_with_preference(
            Some(occ),
            MovementLayer::Ground,
            None,
            SimFixed::from_num(200),
            SimFixed::from_num(200),
            &mut rng,
        );
        assert_eq!(result, Some(2));
    }

    #[test]
    fn test_preference_center_entry_randomizes() {
        let mut seen: BTreeSet<u8> = BTreeSet::new();
        for seed in 0..20u64 {
            let mut rng = SimRng::new(seed);
            let result = allocate_sub_cell_with_preference(
                None,
                MovementLayer::Ground,
                None,
                SimFixed::from_num(128),
                SimFixed::from_num(128),
                &mut rng,
            );
            assert!(result.is_some());
            seen.insert(result.unwrap());
        }
        assert!(seen.contains(&2), "expected sub-cell 2 from randomization");
        assert!(seen.contains(&3), "expected sub-cell 3 from randomization");
        assert!(seen.contains(&4), "expected sub-cell 4 from randomization");
    }

    #[test]
    fn test_preference_all_occupied() {
        let grid = make_occ(&[
            (5, 5, 1, MovementLayer::Ground, Some(2)),
            (5, 5, 2, MovementLayer::Ground, Some(3)),
            (5, 5, 3, MovementLayer::Ground, Some(4)),
        ]);
        let occ = grid.get(5, 5).unwrap();
        let mut rng = SimRng::new(42);
        let result = allocate_sub_cell_with_preference(
            Some(occ),
            MovementLayer::Ground,
            None,
            SimFixed::from_num(200),
            SimFixed::from_num(40),
            &mut rng,
        );
        assert_eq!(result, None);
    }

    #[test]
    fn test_preference_respects_reserved() {
        let reserved: Vec<u8> = vec![2];
        let mut rng = SimRng::new(42);
        let result = allocate_sub_cell_with_preference(
            None,
            MovementLayer::Ground,
            Some(&reserved),
            SimFixed::from_num(200),
            SimFixed::from_num(40),
            &mut rng,
        );
        assert_eq!(result, Some(4));
    }

    #[test]
    fn test_preference_vehicle_blocks() {
        let grid = make_occ(&[(5, 5, 99, MovementLayer::Ground, None)]);
        let occ = grid.get(5, 5).unwrap();
        let mut rng = SimRng::new(42);
        let result = allocate_sub_cell_with_preference(
            Some(occ),
            MovementLayer::Ground,
            None,
            SimFixed::from_num(200),
            SimFixed::from_num(40),
            &mut rng,
        );
        assert_eq!(result, None);
    }
}
