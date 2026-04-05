//! Zone-aware pathfinding — uses zone connectivity for fast unreachability
//! detection and hierarchical corridor-based search space reduction.
//!
//! Current approximation:
//! 1. Look up zone IDs for start and goal.
//! 2. If they are in disconnected zones, return `None` immediately (no A*).
//! 3. Run Dijkstra on the zone adjacency graph to find a coarse corridor.
//! 4. Run cell-level A* restricted to the corridor zones.
//! 5. On failure, retry with zone exclusions (up to 5 retries).
//! 6. Final fallback: run A* without corridor restriction.
//!
//! TODO(RE): RA2/YR has distinct regular vs hierarchical entrypoints and a separate
//! allowHS gate. The recovered entrypoint behavior is precise enough to prove those
//! modes exist, but not yet enough to replace this corridor-Dijkstra approximation.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/zone_map, sim/pathfinding, sim/locomotor.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap};

use super::terrain_cost::TerrainCostGrid;
use super::zone_map::{ZONE_INVALID, ZoneAdjacency, ZoneGrid, ZoneId, ZoneMap};
use super::{
    LayeredPathStep, PathGrid, find_layered_path, find_path_with_costs,
    find_path_with_costs_corridor,
};
use crate::map::resolved_terrain::ResolvedTerrainGrid;
use crate::rules::locomotor_type::MovementZone;
use crate::sim::movement::locomotor::MovementLayer;

/// Maximum corridor Dijkstra retries with zone exclusions before falling back
/// to unrestricted A*. The recovered path entry contract uses a default retry cap of 5.
const MAX_CORRIDOR_RETRIES: u8 = 5;

fn can_use_reduced_zone_precheck(movement_zone: Option<MovementZone>) -> bool {
    match movement_zone {
        None => true,
        Some(
            MovementZone::Normal
            | MovementZone::Amphibious
            | MovementZone::Infantry
            | MovementZone::Fly,
        ) => true,
        // TODO(RE): naval water/beach surface legality in the current terrain-aware zone
        // builder is still coarser than the runtime water-surface predicate, so do not
        // hard-gate those movers on reduced-zone reachability yet.
        Some(_) => false,
    }
}

/// Zone-aware path search for flat (ground-only) paths.
///
/// Uses zone reachability plus a corridor-Dijkstra approximation, then runs A*
/// restricted to that corridor. Falls back to unrestricted A* if corridor
/// search fails.
///
/// TODO(RE): terrain-aware nodeIndex connectivity can still be a little looser than
/// final movement legality because the recovered node flood-fill is 8-neighbor while
/// the actual step predicate also applies tighter per-move checks. Treat zone gating
/// here as a best-effort reject, not closed parity.
pub fn find_path_zoned(
    grid: &PathGrid,
    start: (u16, u16),
    goal: (u16, u16),
    costs: Option<&TerrainCostGrid>,
    entity_blocks: Option<&BTreeSet<(u16, u16)>>,
    zone_grid: Option<&ZoneGrid>,
    mz: MovementZone,
    movement_zone: Option<MovementZone>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
    penalty_cells: Option<&BTreeSet<(u16, u16)>>,
) -> Option<Vec<(u16, u16)>> {
    if !can_use_reduced_zone_precheck(movement_zone) {
        return find_path_with_costs(
            grid,
            start,
            goal,
            costs,
            entity_blocks,
            movement_zone,
            resolved_terrain,
            penalty_cells,
        );
    }

    let Some(zg) = zone_grid else {
        return find_path_with_costs(
            grid,
            start,
            goal,
            costs,
            entity_blocks,
            movement_zone,
            resolved_terrain,
            penalty_cells,
        );
    };

    // Zone pre-check: instant unreachability detection.
    if !zg.can_reach(
        mz,
        start,
        MovementLayer::Ground,
        goal,
        MovementLayer::Ground,
    ) {
        log::trace!(
            "zone_search: unreachable {:?} ({:?}→{:?}), skipping A*",
            mz,
            start,
            goal,
        );
        return None;
    }

    let Some(zone_map) = zg.map_for(mz) else {
        return find_path_with_costs(
            grid,
            start,
            goal,
            costs,
            entity_blocks,
            movement_zone,
            resolved_terrain,
            penalty_cells,
        );
    };
    let Some(adjacency) = zg.adjacency_for(mz) else {
        return find_path_with_costs(
            grid,
            start,
            goal,
            costs,
            entity_blocks,
            movement_zone,
            resolved_terrain,
            penalty_cells,
        );
    };

    let start_zone = zone_map.zone_at(start.0, start.1, MovementLayer::Ground);
    let goal_zone = zone_map.zone_at(goal.0, goal.1, MovementLayer::Ground);

    // Same zone — no corridor needed, run A* directly.
    if start_zone == goal_zone && start_zone != ZONE_INVALID {
        return find_path_with_costs(
            grid,
            start,
            goal,
            costs,
            entity_blocks,
            movement_zone,
            resolved_terrain,
            penalty_cells,
        );
    }

    // Try corridor-restricted A* with retry on failure.
    let mut excluded: BTreeSet<ZoneId> = BTreeSet::new();
    for attempt in 0..MAX_CORRIDOR_RETRIES {
        if let Some(corridor_zones) =
            find_zone_corridor(zone_map, adjacency, start_zone, goal_zone, &excluded)
        {
            // Expand corridor by one ring of neighbor zones for flexibility.
            let allowed = expand_corridor(&corridor_zones, adjacency);
            if let Some(path) = find_path_with_costs_corridor(
                grid,
                start,
                goal,
                costs,
                entity_blocks,
                zone_map,
                &allowed,
                movement_zone,
                resolved_terrain,
                penalty_cells,
            ) {
                return Some(path);
            }
            // Corridor A* failed — exclude all corridor zones and retry.
            log::trace!(
                "zone_search: corridor A* failed attempt {} ({} zones), retrying with exclusions",
                attempt + 1,
                corridor_zones.len(),
            );
            excluded.extend(corridor_zones.iter().copied());
        } else {
            break; // Dijkstra couldn't find alternative route
        }
    }

    // Final fallback: unrestricted A*.
    find_path_with_costs(
        grid,
        start,
        goal,
        costs,
        entity_blocks,
        movement_zone,
        resolved_terrain,
        penalty_cells,
    )
}

/// Zone-aware path search for layered (bridge-capable) paths.
///
/// Checks zone connectivity before invoking the layered A* pathfinder.
/// Bridge cells redirect to ground endpoint zones via `zone_at(Bridge)`,
/// so a single ground-layer reachability check covers cross-bridge paths.
pub fn find_layered_path_zoned(
    grid: &PathGrid,
    ground_blocks: Option<&BTreeSet<(u16, u16)>>,
    bridge_blocks: Option<&BTreeSet<(u16, u16)>>,
    start: (u16, u16),
    start_layer: MovementLayer,
    goal: (u16, u16),
    zone_grid: Option<&ZoneGrid>,
    mz: MovementZone,
    terrain_costs: Option<&TerrainCostGrid>,
    movement_zone: Option<MovementZone>,
    penalty_cells: Option<&BTreeSet<(u16, u16)>>,
) -> Option<Vec<LayeredPathStep>> {
    if !can_use_reduced_zone_precheck(movement_zone) {
        return find_layered_path(
            grid,
            ground_blocks,
            bridge_blocks,
            start,
            start_layer,
            goal,
            terrain_costs,
            penalty_cells,
        );
    }

    // Zone pre-check: bridge cells redirect to ground endpoint zones,
    // so a single ground-layer check covers cross-bridge reachability.
    if let Some(zg) = zone_grid {
        if !zg.can_reach(mz, start, start_layer, goal, MovementLayer::Ground) {
            log::trace!(
                "zone_search: layered unreachable {:?} ({:?} layer={:?} -> {:?}), skipping A*",
                mz,
                start,
                start_layer,
                goal,
            );
            return None;
        }
    }

    find_layered_path(
        grid,
        ground_blocks,
        bridge_blocks,
        start,
        start_layer,
        goal,
        terrain_costs,
        penalty_cells,
    )
}

// ---------------------------------------------------------------------------
// Hierarchical zone Dijkstra
// ---------------------------------------------------------------------------

/// Find the cheapest coarse route through the zone adjacency graph.
/// Returns an ordered sequence of zone IDs from start to goal.
/// Edge cost = Manhattan distance between zone centers.
fn find_zone_corridor(
    zone_map: &ZoneMap,
    adjacency: &ZoneAdjacency,
    start_zone: ZoneId,
    goal_zone: ZoneId,
    excluded: &BTreeSet<ZoneId>,
) -> Option<Vec<ZoneId>> {
    if start_zone == ZONE_INVALID || goal_zone == ZONE_INVALID {
        return None;
    }
    if start_zone == goal_zone {
        return Some(vec![start_zone]);
    }

    let goal_center = zone_map.info_for(goal_zone)?.center;

    // Dijkstra on zone graph: (cost, zone_id)
    let zone_count = zone_map.zone_count as usize;
    let mut dist: Vec<i32> = vec![i32::MAX; zone_count + 1]; // 1-indexed
    let mut prev: Vec<ZoneId> = vec![ZONE_INVALID; zone_count + 1];
    let mut heap: BinaryHeap<Reverse<(i32, ZoneId)>> = BinaryHeap::new();

    dist[start_zone as usize] = 0;
    heap.push(Reverse((0, start_zone)));

    while let Some(Reverse((cost, zone))) = heap.pop() {
        if zone == goal_zone {
            // Reconstruct path.
            let mut path = Vec::new();
            let mut cur = goal_zone;
            while cur != ZONE_INVALID {
                path.push(cur);
                cur = prev[cur as usize];
            }
            path.reverse();
            return Some(path);
        }
        if cost > dist[zone as usize] {
            continue; // stale entry
        }
        for &neighbor in adjacency.neighbors_of(zone) {
            if excluded.contains(&neighbor) {
                continue;
            }
            let Some(n_info) = zone_map.info_for(neighbor) else {
                continue;
            };
            // Edge cost: Manhattan distance between zone centers.
            let edge_cost = manhattan(
                zone_map.info_for(zone).map(|i| i.center).unwrap_or((0, 0)),
                n_info.center,
            );
            let new_cost = cost + edge_cost;
            if new_cost < dist[neighbor as usize] {
                dist[neighbor as usize] = new_cost;
                prev[neighbor as usize] = zone;
                // f = g + h (A* on zone graph for speed)
                let h = manhattan(n_info.center, goal_center);
                heap.push(Reverse((new_cost + h, neighbor)));
            }
        }
    }

    None // No route through zone graph
}

/// Manhattan distance between two cell coordinates.
fn manhattan(a: (u16, u16), b: (u16, u16)) -> i32 {
    (a.0 as i32 - b.0 as i32).abs() + (a.1 as i32 - b.1 as i32).abs()
}

/// Expand a corridor by adding all 1-hop neighbor zones.
/// This gives A* flexibility to route through cells near corridor boundaries.
fn expand_corridor(corridor: &[ZoneId], adjacency: &ZoneAdjacency) -> BTreeSet<ZoneId> {
    let mut allowed: BTreeSet<ZoneId> = corridor.iter().copied().collect();
    for &zone in corridor {
        for &neighbor in adjacency.neighbors_of(zone) {
            allowed.insert(neighbor);
        }
    }
    allowed
}

// Tests are declared in zone/mod.rs (zone_search_tests.rs).
