//! Cell scatter system — displaces idle mobile units away from a target cell.
//!
//! When a cell becomes blocked (building placed, terrain change, etc.), idle
//! mobile units near that cell must scatter to nearby passable cells. This
//! reproduces the behavior of `HouseClass::ScatterUnitsFromCell`
//! from the original engine.
//!
//! ## Original engine behavior
//! The original walks a global foundation offset table (350 entries of `(dx, dy)`
//! short pairs) that spiral outward from the origin. For each
//! idle foot-class unit (vehicles and infantry) owned by the house, it:
//! 1. Picks the next spiral offset as a scatter destination
//! 2. Checks if the destination cell is passable
//! 3. Assigns a movement order to send the unit there
//! 4. Tries up to 351 (`0x15F`) directions before giving up
//!
//! We reproduce this with a programmatically generated spiral pattern and the
//! existing A* pathfinding infrastructure.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/entity_store, sim/movement, sim/pathfinding,
//!   sim/bump_crush.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use std::collections::{BTreeMap, BTreeSet};

use crate::map::entities::EntityCategory;
use crate::rules::locomotor_type::SpeedType;
use crate::rules::ruleset::RuleSet;
use crate::sim::entity_store::EntityStore;
use crate::sim::movement;
use crate::sim::movement::bump_crush::{self, OccupancyMap};
use crate::sim::pathfinding::PathGrid;
use crate::sim::pathfinding::terrain_cost::TerrainCostGrid;
use crate::sim::rng::SimRng;
use crate::util::fixed_math::{SimFixed, ra2_speed_to_leptons_per_second};

/// Maximum number of spiral directions to try per scatter operation.
/// The original engine uses `0x15F` (351).
const MAX_SCATTER_DIRECTIONS: usize = 351;

/// Maximum spiral search radius in cells.
/// 351 offsets ≈ a square shell spiral out to radius ~10.
const MAX_SPIRAL_RADIUS: i32 = 10;

/// How often idle scatter checks run (in simulation ticks).
/// Original engine: `frame_counter % rules+0x1808 == 0`.
/// ~10 seconds at 15 fps game speed.
const IDLE_SCATTER_INTERVAL: u64 = 150;

/// The 8 directional neighbor offsets in isometric cell coordinates.
const NEIGHBOR_OFFSETS: [(i32, i32); 8] = [
    (0, -1),
    (1, -1),
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
];

/// Periodic idle scatter: every `IDLE_SCATTER_INTERVAL` ticks, scan for idle
/// mobile units sharing a cell with other occupants and scatter them to a
/// random adjacent passable cell.
///
/// Reproduces `TechnoClass::AI` periodic scatter check:
/// "Every N frames, idle infantry with no destination auto-scatter."
pub fn tick_idle_scatter(
    entities: &mut EntityStore,
    rules: Option<&RuleSet>,
    path_grid: Option<&PathGrid>,
    terrain_costs: &BTreeMap<SpeedType, TerrainCostGrid>,
    rng: &mut SimRng,
    frame_counter: u64,
    interner: &crate::sim::intern::StringInterner,
) {
    if frame_counter == 0 || !frame_counter.is_multiple_of(IDLE_SCATTER_INTERVAL) {
        return;
    }
    let Some(grid) = path_grid else { return };

    // Snapshot occupancy for cell-sharing checks.
    let (ground_occ, _bridge_occ) = bump_crush::build_occupancy_maps(entities);

    // Collect idle mobile units sharing a cell with others. Deterministic order
    // via keys_sorted() is critical for lockstep correctness.
    let keys = entities.keys_sorted();
    let mut scatter_commands: Vec<(u64, (u16, u16))> = Vec::new();

    for &id in &keys {
        let Some(entity) = entities.get(id) else {
            continue;
        };
        // Must be idle mobile unit (no movement, no attack, no standing order).
        if entity.movement_target.is_some()
            || entity.attack_target.is_some()
            || entity.order_intent.is_some()
            || entity.dying
            || entity.locomotor.is_none()
        {
            continue;
        }
        match entity.category {
            EntityCategory::Unit | EntityCategory::Infantry => {}
            _ => continue,
        }

        let pos = (entity.position.rx, entity.position.ry);

        // Only scatter if sharing the cell with at least one other entity.
        let sharing = ground_occ.get(&pos).is_some_and(|occ| {
            let total = occ.blockers.len() + occ.infantry.len();
            total > 1
        });
        if !sharing {
            continue;
        }

        // Pick a random adjacent passable cell. Try up to 8 neighbors starting
        // from a random direction, matching the original engine's usage.
        let start_dir = rng.next_range_u32(8) as usize;
        let mut dest: Option<(u16, u16)> = None;
        for i in 0..8 {
            let dir = (start_dir + i) % 8;
            let (dx, dy) = NEIGHBOR_OFFSETS[dir];
            let nx = pos.0 as i32 + dx;
            let ny = pos.1 as i32 + dy;
            if nx < 0 || ny < 0 {
                continue;
            }
            let (rx, ry) = (nx as u16, ny as u16);
            if rx >= grid.width() || ry >= grid.height() || !grid.is_walkable(rx, ry) {
                continue;
            }
            // For infantry: must have a free sub-cell. For vehicles: must be empty.
            let available = match entity.category {
                EntityCategory::Infantry => {
                    bump_crush::cell_passable_for_infantry(ground_occ.get(&(rx, ry)))
                }
                _ => ground_occ
                    .get(&(rx, ry))
                    .map_or(true, |o| o.blockers.is_empty()),
            };
            if available {
                dest = Some((rx, ry));
                break;
            }
        }

        if let Some(target) = dest {
            if let Some(occ) = ground_occ.get(&pos) {
                log::info!(
                    "IDLE_SCATTER entity={} {:?} at ({},{}) → ({},{}) blockers={:?} infantry={:?}",
                    id,
                    entity.category,
                    pos.0,
                    pos.1,
                    target.0,
                    target.1,
                    occ.blockers,
                    occ.infantry,
                );
            }
            scatter_commands.push((id, target));
        }
    }

    // Apply scatter commands outside the immutable entity iteration.
    for (entity_id, dest) in scatter_commands {
        let speed = resolve_entity_speed(entities, rules, entity_id, interner);
        if speed <= SimFixed::from_num(0) {
            continue;
        }
        let cost_grid = entities
            .get(entity_id)
            .and_then(|e| e.locomotor.as_ref())
            .and_then(|l| terrain_costs.get(&l.speed_type));

        movement::issue_move_command_with_layered(
            entities, grid, entity_id, dest, speed, false, cost_grid,
            None, // no entity blocks for 1-cell scatter
            None, // resolved_terrain
        );
    }
}

/// Scatter idle mobile units owned by `owner` away from `target_cell`.
///
/// Iterates all vehicles and infantry owned by the specified house. For each
/// idle (no active movement order) mobile entity, finds a nearby passable cell
/// using a spiral search outward from `target_cell` and assigns a movement
/// target to displace it there.
///
/// Returns the number of units that were successfully scattered.
pub fn scatter_units_from_cell(
    entities: &mut EntityStore,
    rules: Option<&RuleSet>,
    owner: crate::sim::intern::InternedId,
    target_cell: (u16, u16),
    path_grid: &PathGrid,
    terrain_costs: &BTreeMap<SpeedType, TerrainCostGrid>,
    interner: &crate::sim::intern::StringInterner,
) -> u32 {
    let spiral = generate_spiral_offsets(MAX_SPIRAL_RADIUS, MAX_SCATTER_DIRECTIONS);
    let (ground_occ, _bridge_occ) = bump_crush::build_occupancy_maps(entities);

    // Collect candidate entities: idle mobile units owned by this house.
    // keys_sorted() takes &mut self, so .to_vec() releases the borrow before
    // we read entity fields.
    let keys = entities.keys_sorted();
    let candidates: Vec<(u64, EntityCategory)> = keys
        .iter()
        .filter_map(|&id| {
            let entity = entities.get(id)?;
            if entity.owner != owner {
                return None;
            }
            match entity.category {
                EntityCategory::Unit | EntityCategory::Infantry => {}
                _ => return None,
            }
            // Skip units already moving (not idle).
            if entity.movement_target.is_some() {
                return None;
            }
            // Skip immobile entities.
            if entity.locomotor.is_none() {
                return None;
            }
            Some((id, entity.category))
        })
        .collect();

    if candidates.is_empty() {
        return 0;
    }

    let mut direction_idx: usize = 0;
    let mut scattered: u32 = 0;

    for (entity_id, category) in &candidates {
        if direction_idx >= spiral.len() {
            break;
        }

        // Find the next passable scatter cell from the current spiral direction.
        let scatter_cell = find_passable_scatter_cell(
            &spiral,
            &mut direction_idx,
            target_cell,
            *category,
            path_grid,
            &ground_occ,
        );

        let Some(dest) = scatter_cell else {
            // Exhausted all spiral directions — stop.
            break;
        };

        // Resolve movement speed from rules.ini.
        let speed = resolve_entity_speed(entities, rules, *entity_id, interner);
        if speed <= SimFixed::from_num(0) {
            continue;
        }

        // Resolve terrain cost grid for this entity's speed type.
        let cost_grid = entities
            .get(*entity_id)
            .and_then(|e| e.locomotor.as_ref())
            .and_then(|l| terrain_costs.get(&l.speed_type));

        // Build a set of cells occupied by other entities to avoid during pathfinding.
        let entity_blocks = build_entity_block_set(entities, *entity_id);

        let success = movement::issue_move_command_with_layered(
            entities,
            path_grid,
            *entity_id,
            dest,
            speed,
            false, // don't queue — replace any existing movement
            cost_grid,
            Some(&entity_blocks),
            None, // resolved_terrain
        );

        if success {
            scattered += 1;
        }
    }

    scattered
}

/// Walk the spiral offset table from `direction_idx` to find the next cell
/// that is walkable and available for the given entity category.
///
/// Advances `direction_idx` past each tested offset. Returns `None` if
/// the spiral is exhausted without finding a passable cell.
fn find_passable_scatter_cell(
    spiral: &[(i16, i16)],
    direction_idx: &mut usize,
    target_cell: (u16, u16),
    category: EntityCategory,
    path_grid: &PathGrid,
    ground_occ: &OccupancyMap,
) -> Option<(u16, u16)> {
    while *direction_idx < spiral.len() {
        let (dx, dy) = spiral[*direction_idx];
        *direction_idx += 1;

        let cx = target_cell.0 as i32 + dx as i32;
        let cy = target_cell.1 as i32 + dy as i32;
        if cx < 0 || cy < 0 {
            continue;
        }
        let rx = cx as u16;
        let ry = cy as u16;

        // Check grid bounds and walkability.
        if rx >= path_grid.width() || ry >= path_grid.height() {
            continue;
        }
        if !path_grid.is_walkable(rx, ry) {
            continue;
        }

        // Check occupancy: infantry can share cells, vehicles cannot.
        let available = match category {
            EntityCategory::Infantry => {
                bump_crush::cell_passable_for_infantry(ground_occ.get(&(rx, ry)))
            }
            _ => match ground_occ.get(&(rx, ry)) {
                Some(o) => o.blockers.is_empty(),
                None => true,
            },
        };
        if !available {
            continue;
        }

        return Some((rx, ry));
    }
    None
}

/// Resolve a unit's movement speed from rules.ini + locomotor multiplier.
fn resolve_entity_speed(
    entities: &EntityStore,
    rules: Option<&RuleSet>,
    entity_id: u64,
    interner: &crate::sim::intern::StringInterner,
) -> SimFixed {
    let Some(entity) = entities.get(entity_id) else {
        return SimFixed::from_num(0);
    };
    let base_speed = rules
        .and_then(|r| r.object(interner.resolve(entity.type_ref)))
        .map(|obj| ra2_speed_to_leptons_per_second(obj.speed))
        .unwrap_or_else(|| ra2_speed_to_leptons_per_second(4));
    let loco_mult = entity
        .locomotor
        .as_ref()
        .map(|l| l.speed_multiplier)
        .unwrap_or(SimFixed::from_num(1));
    (base_speed * loco_mult).max(SimFixed::from_num(25))
}

/// Build a set of cells occupied by entities other than `exclude_id`.
///
/// Used as the `entity_blocks` parameter for A* pathfinding so the scattered
/// unit doesn't try to path through other units.
fn build_entity_block_set(entities: &EntityStore, exclude_id: u64) -> BTreeSet<(u16, u16)> {
    let mut blocks = BTreeSet::new();
    for entity in entities.values() {
        if entity.stable_id == exclude_id {
            continue;
        }
        // Only block cells occupied by vehicles/structures — infantry share cells.
        match entity.category {
            EntityCategory::Unit | EntityCategory::Structure => {
                blocks.insert((entity.position.rx, entity.position.ry));
            }
            _ => {}
        }
    }
    blocks
}

/// Generate a spiral of `(dx, dy)` cell offsets radiating outward from `(0, 0)`.
///
/// Reproduces the search pattern of the original engine's foundation offset table
/// (350 entries spiraling outward). The spiral walks concentric
/// square shells: radius 1 first (8 cells), then radius 2 (16 cells), etc.
///
/// Each shell visits cells in order: top edge (left→right), right edge (top→bottom),
/// bottom edge (right→left), left edge (bottom→top).
fn generate_spiral_offsets(max_radius: i32, max_entries: usize) -> Vec<(i16, i16)> {
    let mut offsets = Vec::with_capacity(max_entries);
    for r in 1..=max_radius {
        // Top edge: y = -r, x from -r to r
        for x in -r..=r {
            offsets.push((x as i16, (-r) as i16));
            if offsets.len() >= max_entries {
                return offsets;
            }
        }
        // Right edge: x = r, y from -r+1 to r
        for y in (-r + 1)..=r {
            offsets.push((r as i16, y as i16));
            if offsets.len() >= max_entries {
                return offsets;
            }
        }
        // Bottom edge: y = r, x from r-1 down to -r
        for x in (-r..r).rev() {
            offsets.push((x as i16, r as i16));
            if offsets.len() >= max_entries {
                return offsets;
            }
        }
        // Left edge: x = -r, y from r-1 down to -r+1
        for y in ((-r + 1)..r).rev() {
            offsets.push(((-r) as i16, y as i16));
            if offsets.len() >= max_entries {
                return offsets;
            }
        }
    }
    offsets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spiral_offsets_starts_at_radius_1() {
        let offsets = generate_spiral_offsets(2, 100);
        // Radius 1 shell: 8 cells.
        // Top edge: (-1,-1), (0,-1), (1,-1)
        assert_eq!(offsets[0], (-1, -1));
        assert_eq!(offsets[1], (0, -1));
        assert_eq!(offsets[2], (1, -1));
        // Right edge: (1, 0), (1, 1)
        assert_eq!(offsets[3], (1, 0));
        assert_eq!(offsets[4], (1, 1));
        // Bottom edge: (0, 1), (-1, 1)
        assert_eq!(offsets[5], (0, 1));
        assert_eq!(offsets[6], (-1, 1));
        // Left edge: (-1, 0)
        assert_eq!(offsets[7], (-1, 0));
        // Radius 1 = 8 cells total.
        assert_eq!(offsets.len(), 8 + 16); // radius 1 (8) + radius 2 (16)
    }

    #[test]
    fn spiral_offsets_capped_at_max_entries() {
        let offsets = generate_spiral_offsets(10, MAX_SCATTER_DIRECTIONS);
        assert!(offsets.len() <= MAX_SCATTER_DIRECTIONS);
    }

    #[test]
    fn spiral_offsets_no_duplicates() {
        let offsets = generate_spiral_offsets(10, MAX_SCATTER_DIRECTIONS);
        let unique: std::collections::BTreeSet<(i16, i16)> = offsets.iter().copied().collect();
        assert_eq!(
            offsets.len(),
            unique.len(),
            "spiral contains duplicate offsets"
        );
    }

    #[test]
    fn spiral_offsets_excludes_origin() {
        let offsets = generate_spiral_offsets(10, MAX_SCATTER_DIRECTIONS);
        assert!(
            !offsets.contains(&(0, 0)),
            "spiral should not contain origin (0,0)"
        );
    }
}
