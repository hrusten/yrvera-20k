//! Zone map construction: flood-fill, adjacency extraction, zone info computation.
//!
//! Extracted from zone_map.rs to keep each file under ~400 lines.
//! This module is private to sim/ — public API lives in zone_map.rs.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/pathfinding, sim/terrain_cost, sim/locomotor.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use std::collections::VecDeque;

use super::PathGrid;
use super::passability;
use super::terrain_cost::TerrainCostGrid;
use super::zone_map::{ZONE_INVALID, ZoneAdjacency, ZoneId, ZoneInfo, ZoneMap};
use crate::map::resolved_terrain::{ResolvedTerrainGrid, zone_class};
use crate::rules::locomotor_type::MovementZone;
use crate::sim::movement::locomotor::MovementLayer;

/// 8-directional neighbor offsets: (dx, dy, is_diagonal).
pub(crate) const NEIGHBORS: [(i32, i32, bool); 8] = [
    (0, -1, false), // N
    (1, -1, true),  // NE
    (1, 0, false),  // E
    (1, 1, true),   // SE
    (0, 1, false),  // S
    (-1, 1, true),  // SW
    (-1, 0, false), // W
    (-1, -1, true), // NW
];

/// RE-backed MovementClass8 passability rows used by zone/node rebuilding.
///
/// This is distinct from the terrain `LandType` matrix used elsewhere. RA2/YR
/// connectivity is keyed by a derived per-cell movement class, then filtered by
/// the mover's `MovementZone` row.
const MOVEMENT_CLASS_PASSABILITY: [[u8; 8]; 13] = [
    [1, 2, 2, 2, 2, 2, 2, 3], // Normal
    [1, 1, 2, 2, 2, 2, 2, 3], // Crusher
    [1, 1, 1, 2, 2, 2, 2, 3], // Destroyer
    [1, 1, 1, 1, 1, 1, 2, 3], // AmphibiousDestroyer
    [1, 1, 2, 1, 1, 2, 2, 3], // AmphibiousCrusher
    [1, 2, 2, 1, 1, 2, 2, 3], // Amphibious
    [1, 1, 1, 2, 2, 2, 1, 3], // Subterranean
    [1, 2, 2, 2, 2, 1, 2, 3], // Infantry
    [1, 1, 1, 2, 2, 1, 2, 3], // InfantryDestroyer
    [1, 1, 1, 1, 1, 1, 1, 3], // Fly
    [2, 2, 2, 2, 1, 2, 2, 3], // Water
    [2, 2, 2, 1, 1, 2, 2, 3], // WaterBeach
    [1, 1, 1, 2, 2, 2, 2, 3], // CrusherAll
];


/// Build a zone map and adjacency graph for one MovementZone.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn build_zone_map(
    path_grid: &PathGrid,
    cost_grid: Option<&TerrainCostGrid>,
    mz: MovementZone,
    width: u16,
    height: u16,
) -> (ZoneMap, ZoneAdjacency) {
    build_zone_map_with_terrain(path_grid, cost_grid, None, mz, width, height)
}

/// Build a zone map using recovered movement-class zoning when resolved terrain is available.
///
/// When `resolved_terrain` is provided, rebuilds a coarse `MovementClass8` grid, derives
/// `nodeIndex` components, then remaps those nodes through the representative movement-zone
/// row. Falls back to the older direct passable-cell flood-fill when resolved terrain is not
/// available (primarily tests and non-terrain-aware call sites).
pub(crate) fn build_zone_map_with_terrain(
    path_grid: &PathGrid,
    cost_grid: Option<&TerrainCostGrid>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
    mz: MovementZone,
    width: u16,
    height: u16,
) -> (ZoneMap, ZoneAdjacency) {
    if let Some(terrain) = resolved_terrain {
        return build_zone_map_with_movement_classes(path_grid, terrain, mz, width, height);
    }

    let total = width as usize * height as usize;

    // -- Ground layer flood-fill --
    let mut zone_ids = vec![ZONE_INVALID; total];
    let mut next_zone: ZoneId = 1;

    // Row-major scan for deterministic zone assignment.
    for ry in 0..height {
        for rx in 0..width {
            let idx = ry as usize * width as usize + rx as usize;
            if zone_ids[idx] != ZONE_INVALID {
                continue;
            }
            if !is_passable(
                rx,
                ry,
                mz,
                path_grid,
                cost_grid,
                resolved_terrain,
                MovementLayer::Ground,
            ) {
                continue;
            }
            // BFS flood-fill from this cell.
            flood_fill(
                rx,
                ry,
                next_zone,
                &mut zone_ids,
                width,
                height,
                mz,
                path_grid,
                cost_grid,
                resolved_terrain,
                MovementLayer::Ground,
            );
            next_zone += 1;
        }
    }

    let zone_count = next_zone - 1;

    // -- Extract adjacency (ground only; bridge edges injected by caller) --
    let adj = extract_adjacency(
        &zone_ids,
        width,
        height,
        zone_count,
    );

    let zone_info = compute_zone_info(
        &zone_ids,
        width,
        height,
        zone_count,
    );

    let zone_map = ZoneMap::new(
        zone_ids,
        None, // bridge_redirect set by caller
        width,
        height,
        zone_count,
        zone_info,
    );

    (zone_map, adj)
}

fn build_zone_map_with_movement_classes(
    path_grid: &PathGrid,
    resolved_terrain: &ResolvedTerrainGrid,
    mz: MovementZone,
    width: u16,
    height: u16,
) -> (ZoneMap, ZoneAdjacency) {
    let total = width as usize * height as usize;
    let movement_classes: Vec<u8> = (0..height)
        .flat_map(|ry| {
            (0..width).map(move |rx| movement_class_for_cell(path_grid, resolved_terrain, rx, ry))
        })
        .collect();

    let (node_indices, node_count) =
        rebuild_node_indices(&movement_classes, path_grid, width, height);
    let node_adj = build_node_adjacency(&node_indices, width, height);
    let movement_zone = mz;
    let raw_zone_ids = rebuild_zone_ids_for_movement_zone(
        &movement_classes,
        &node_indices,
        node_count,
        &node_adj,
        movement_zone,
    );
    let (zone_ids, zone_count) = compact_raw_zone_ids(&node_indices, &raw_zone_ids, total);

    let adj = extract_adjacency(
        &zone_ids,
        width,
        height,
        zone_count,
    );
    let zone_info = compute_zone_info(
        &zone_ids,
        width,
        height,
        zone_count,
    );
    let zone_map = ZoneMap::new(
        zone_ids,
        None, // bridge_redirect set by caller
        width,
        height,
        zone_count,
        zone_info,
    );

    (zone_map, adj)
}

fn movement_class_for_cell(
    path_grid: &PathGrid,
    resolved_terrain: &ResolvedTerrainGrid,
    x: u16,
    y: u16,
) -> u8 {
    let Some(cell) = resolved_terrain.cell(x, y) else {
        return zone_class::OUTSIDE;
    };

    // Buildings are entity-based, not stored on ResolvedTerrainCell.
    // Check PathGrid for building footprints → class 5 (Building).
    // Only override if the cached zone_type isn't already a stronger blocker.
    if cell.zone_type < zone_class::BUILDING && !path_grid.is_walkable(x, y) && !cell.is_water {
        return zone_class::BUILDING;
    }

    cell.zone_type
}

fn rebuild_node_indices(
    movement_classes: &[u8],
    path_grid: &PathGrid,
    width: u16,
    height: u16,
) -> (Vec<u16>, u16) {
    let mut node_indices = vec![0u16; movement_classes.len()];
    let mut next_node: u16 = 1;

    for ry in 0..height {
        for rx in 0..width {
            let idx = ry as usize * width as usize + rx as usize;
            if movement_classes[idx] == zone_class::OUTSIDE || node_indices[idx] != 0 {
                continue;
            }
            flood_fill_node_index(
                rx,
                ry,
                next_node,
                movement_classes,
                &mut node_indices,
                path_grid,
                width,
                height,
            );
            next_node += 1;
        }
    }

    (node_indices, next_node.saturating_sub(1))
}

fn flood_fill_node_index(
    start_x: u16,
    start_y: u16,
    node_id: u16,
    movement_classes: &[u8],
    node_indices: &mut [u16],
    path_grid: &PathGrid,
    width: u16,
    height: u16,
) {
    let mut queue = VecDeque::new();
    let start_idx = start_y as usize * width as usize + start_x as usize;
    let movement_class = movement_classes[start_idx];
    node_indices[start_idx] = node_id;
    queue.push_back((start_x, start_y));

    // TODO(RE): The recovered nodeIndex flood-fill is 8-neighbor and class/height based,
    // not the same as final movement-time diagonal corner legality. Keep this RE-shaped
    // connectivity here for now and let the actual A* legality checks remain tighter.
    // If later runtime evidence proves an additional corner constraint at the node layer,
    // update this together with the zone fast-reject assumptions.
    while let Some((cx, cy)) = queue.pop_front() {
        for &(dx, dy, _) in &NEIGHBORS {
            let nx = cx as i32 + dx;
            let ny = cy as i32 + dy;
            if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                continue;
            }
            let nx = nx as u16;
            let ny = ny as u16;
            let n_idx = ny as usize * width as usize + nx as usize;
            if node_indices[n_idx] != 0 || movement_classes[n_idx] != movement_class {
                continue;
            }

            if movement_class != zone_class::IMPASSABLE {
                let Some(cur) = path_grid.cell(cx, cy) else {
                    continue;
                };
                let Some(nbr) = path_grid.cell(nx, ny) else {
                    continue;
                };
                if (cur.ground_level as i16 - nbr.ground_level as i16).abs() >= 2 {
                    continue;
                }
            }

            node_indices[n_idx] = node_id;
            queue.push_back((nx, ny));
        }
    }
}

fn build_node_adjacency(node_indices: &[u16], width: u16, height: u16) -> Vec<Vec<u16>> {
    let node_count = node_indices.iter().copied().max().unwrap_or(0) as usize;
    let mut adj = vec![Vec::new(); node_count + 1];

    for y in 0..height {
        for x in 0..width {
            let idx = y as usize * width as usize + x as usize;
            let a = node_indices[idx];
            if a == 0 {
                continue;
            }
            for &(dx, dy, is_diagonal) in &NEIGHBORS {
                let nx = x as i32 + dx;
                let ny = y as i32 + dy;
                if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                    continue;
                }
                let nx = nx as u16;
                let ny = ny as u16;
                let n_idx = ny as usize * width as usize + nx as usize;
                let b = node_indices[n_idx];
                if b == 0 || a == b {
                    continue;
                }

                if is_diagonal {
                    let o = node_indices[y as usize * width as usize + nx as usize];
                    let p = node_indices[ny as usize * width as usize + x as usize];
                    if o == 0 && p == 0 {
                        continue;
                    }
                }

                adj[a as usize].push(b);
            }
        }
    }

    for neighbors in &mut adj {
        neighbors.sort_unstable();
        neighbors.dedup();
    }

    adj
}

fn rebuild_zone_ids_for_movement_zone(
    movement_classes: &[u8],
    node_indices: &[u16],
    node_count: u16,
    node_adj: &[Vec<u16>],
    movement_zone: MovementZone,
) -> Vec<u16> {
    let mut node_movement_classes = vec![zone_class::OUTSIDE; node_count as usize + 1];
    for (&node, &movement_class) in node_indices.iter().zip(movement_classes.iter()) {
        if node != 0 && node_movement_classes[node as usize] == zone_class::OUTSIDE {
            node_movement_classes[node as usize] = movement_class;
        }
    }

    let row = MOVEMENT_CLASS_PASSABILITY[movement_zone as usize];
    let mut zone_id_by_node = vec![1u16; node_count as usize + 1];
    for node in 1..=node_count {
        let movement_class = node_movement_classes[node as usize] as usize;
        if row[movement_class] == 1 {
            zone_id_by_node[node as usize] = 0;
        }
    }

    let mut next_label: u16 = 2;
    for start_node in 1..=node_count {
        if zone_id_by_node[start_node as usize] != 0 {
            continue;
        }
        let mut queue = VecDeque::new();
        zone_id_by_node[start_node as usize] = next_label;
        queue.push_back(start_node);

        while let Some(cur) = queue.pop_front() {
            for &neighbor in &node_adj[cur as usize] {
                if zone_id_by_node[neighbor as usize] != 0 {
                    continue;
                }
                zone_id_by_node[neighbor as usize] = next_label;
                queue.push_back(neighbor);
            }
        }

        next_label += 1;
    }

    zone_id_by_node[0] = u16::MAX;
    zone_id_by_node
}

fn compact_raw_zone_ids(
    node_indices: &[u16],
    raw_zone_ids: &[u16],
    total: usize,
) -> (Vec<ZoneId>, ZoneId) {
    let mut remap = std::collections::BTreeMap::<u16, ZoneId>::new();
    let mut next_zone: ZoneId = 1;
    let mut zone_ids = vec![ZONE_INVALID; total];

    for (idx, &node) in node_indices.iter().enumerate() {
        if node == 0 {
            continue;
        }
        let raw_zone = raw_zone_ids[node as usize];
        if raw_zone <= 1 {
            continue;
        }
        let zone = *remap.entry(raw_zone).or_insert_with(|| {
            let assigned = next_zone;
            next_zone += 1;
            assigned
        });
        zone_ids[idx] = zone;
    }

    (zone_ids, next_zone.saturating_sub(1))
}

/// Check if a cell is passable for a given MovementZone.
///
/// This helper is still the direct passable-cell check used by the fallback and legacy
/// incremental paths. Terrain-aware full rebuilds now go through `MovementClass8` +
/// `nodeIndex` reconstruction instead.
pub(crate) fn is_passable(
    x: u16,
    y: u16,
    mz: MovementZone,
    path_grid: &PathGrid,
    cost_grid: Option<&TerrainCostGrid>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
    _layer: MovementLayer,
) -> bool {
    // Buildings and static obstacles block all ground movement regardless of
    // land type. PathGrid encodes this (set_blocked for building footprints).
    // Water zones skip this check since water cells are typically blocked in PathGrid.
    if !mz.is_water_mover() && !path_grid.is_walkable(x, y) {
        return false;
    }

    // Primary check: passability matrix using land_type from resolved terrain.
    // Uses MovementZone (not SpeedType) for the passability lookup — this matches
    // the original engine's Can_Enter_Cell logic where MovementZone determines
    // which cells are passable, while SpeedType only affects movement speed.
    // Critical: SpeedType::Float maps to zone 9 (hover — everything passable),
    // but MovementZone::Water maps to zone 10 (water cells only).
    if let Some(terrain) = resolved_terrain {
        if let Some(cell) = terrain.cell(x, y) {
            if mz.is_water_mover() {
                return super::is_water_surface_cell_passable(cell, mz);
            }
            return passability::is_passable_for_zone(cell.land_type, mz);
        }
    }

    // Fallback: TerrainCostGrid-based check (pre-matrix behavior).
    if mz.is_water_mover() {
        if let Some(cg) = cost_grid {
            cg.cost_at(x, y) > 0
        } else {
            false
        }
    } else {
        if let Some(cg) = cost_grid {
            cg.cost_at(x, y) > 0
        } else {
            true
        }
    }
}

/// BFS flood-fill on the ground layer.
///
/// Assigns `zone_id` to all passable cells reachable from `(start_x, start_y)`.
/// Height continuity: adjacent cells with ground_level difference > 1 form zone
/// boundaries (matches original engine flood-fill behavior).
pub(crate) fn flood_fill(
    start_x: u16,
    start_y: u16,
    zone_id: ZoneId,
    zone_ids: &mut [ZoneId],
    width: u16,
    height: u16,
    mz: MovementZone,
    path_grid: &PathGrid,
    cost_grid: Option<&TerrainCostGrid>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
    layer: MovementLayer,
) {
    let mut queue = VecDeque::new();
    let start_idx = start_y as usize * width as usize + start_x as usize;
    zone_ids[start_idx] = zone_id;
    queue.push_back((start_x, start_y));

    while let Some((cx, cy)) = queue.pop_front() {
        for &(dx, dy, is_diagonal) in &NEIGHBORS {
            let nx = cx as i32 + dx;
            let ny = cy as i32 + dy;
            if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                continue;
            }
            let nx = nx as u16;
            let ny = ny as u16;
            let n_idx = ny as usize * width as usize + nx as usize;

            if zone_ids[n_idx] != ZONE_INVALID {
                continue;
            }
            if !is_passable(nx, ny, mz, path_grid, cost_grid, resolved_terrain, layer) {
                continue;
            }

            // Diagonal corner-cutting: both adjacent cardinals must be passable.
            if is_diagonal {
                let ax = (cx as i32 + dx) as u16;
                let ay = cy;
                let bx = cx;
                let by = (cy as i32 + dy) as u16;
                if !is_passable(ax, ay, mz, path_grid, cost_grid, resolved_terrain, layer)
                    || !is_passable(bx, by, mz, path_grid, cost_grid, resolved_terrain, layer)
                {
                    continue;
                }
            }

            // Height continuity: original engine enforces abs(h_diff) <= 1 in zone
            // flood-fill. Height jumps > 1 create zone boundaries so the zone system
            // never claims two cells are mutually reachable when A* would fail due to
            // a cliff. Only checked on the ground layer for land-based categories.
            if layer == MovementLayer::Ground {
                if let (Some(cur), Some(nbr)) = (path_grid.cell(cx, cy), path_grid.cell(nx, ny)) {
                    if (cur.ground_level as i16 - nbr.ground_level as i16).abs() > 1 {
                        continue;
                    }
                }
            }

            zone_ids[n_idx] = zone_id;
            queue.push_back((nx, ny));
        }
    }
}

/// Compute per-zone centroid and cell count from the ground zone ID array.
pub(crate) fn compute_zone_info(
    zone_ids: &[ZoneId],
    width: u16,
    _height: u16,
    zone_count: u16,
) -> Vec<ZoneInfo> {
    let mut sums: Vec<(u64, u64, u32)> = vec![(0, 0, 0); zone_count as usize];
    for (idx, &zid) in zone_ids.iter().enumerate() {
        if zid != ZONE_INVALID {
            let x = (idx % width as usize) as u64;
            let y = (idx / width as usize) as u64;
            let entry = &mut sums[zid as usize - 1];
            entry.0 += x;
            entry.1 += y;
            entry.2 += 1;
        }
    }
    sums.iter()
        .map(|&(sx, sy, count)| {
            if count == 0 {
                ZoneInfo::default()
            } else {
                ZoneInfo {
                    center: (
                        u16::try_from(sx / count as u64).unwrap_or(u16::MAX),
                        u16::try_from(sy / count as u64).unwrap_or(u16::MAX),
                    ),
                    cell_count: count,
                }
            }
        })
        .collect()
}

/// Extract adjacency from ground zone ID array (ground-layer only).
///
/// Bridge cross-zone edges are injected separately via `inject_bridge_adjacency`.
pub(crate) fn extract_adjacency(
    ground_zones: &[ZoneId],
    width: u16,
    height: u16,
    zone_count: u16,
) -> ZoneAdjacency {
    let mut adj_sets: Vec<Vec<ZoneId>> = vec![Vec::new(); zone_count as usize + 1];
    let w = width as usize;

    for ry in 0..height {
        for rx in 0..width {
            let idx = ry as usize * w + rx as usize;
            let z = ground_zones[idx];
            if z == ZONE_INVALID {
                continue;
            }
            for &(dx, dy) in &[(1i32, 0i32), (0, 1), (1, 1), (1, -1)] {
                let nx = rx as i32 + dx;
                let ny = ry as i32 + dy;
                if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                    continue;
                }
                let n_idx = ny as usize * w + nx as usize;
                let nz = ground_zones[n_idx];
                if nz != ZONE_INVALID && nz != z {
                    add_adjacency(&mut adj_sets, z, nz);
                }
            }
        }
    }

    for list in &mut adj_sets {
        list.sort_unstable();
        list.dedup();
    }

    ZoneAdjacency::new(adj_sets)
}

/// Inject bridge adjacency edges into an existing adjacency graph.
///
/// For each active bridge endpoint record, connects the ground zones at
/// endpoint_a and endpoint_b. This mirrors gamemd.exe AddBridgeZoneEdges
/// (0x005851b0) which adds bidirectional edges to the zone graph.
pub(crate) fn inject_bridge_adjacency(
    adj: &mut ZoneAdjacency,
    ground_zones: &[ZoneId],
    bridge_records: &[crate::sim::bridge_state::BridgeEndpointRecord],
    width: u16,
) {
    let w = width as usize;
    for record in bridge_records {
        if !record.active {
            continue;
        }
        let (ax, ay) = record.endpoint_a;
        let (bx, by) = record.endpoint_b;

        let a_idx = ay as usize * w + ax as usize;
        let b_idx = by as usize * w + bx as usize;

        if a_idx >= ground_zones.len() || b_idx >= ground_zones.len() {
            continue;
        }

        let za = ground_zones[a_idx];
        let zb = ground_zones[b_idx];

        if za != ZONE_INVALID && zb != ZONE_INVALID && za != zb {
            if !adj.neighbors[za as usize].contains(&zb) {
                adj.neighbors[za as usize].push(zb);
                adj.neighbors[za as usize].sort_unstable();
            }
            if !adj.neighbors[zb as usize].contains(&za) {
                adj.neighbors[zb as usize].push(za);
                adj.neighbors[zb as usize].sort_unstable();
            }
        }
    }
}

/// Build per-cell bridge redirect table.
///
/// For each bridge cell (walkable on bridge layer), find the nearest active
/// bridge endpoint and store its ground cell coordinates.
pub(crate) fn build_bridge_redirect(
    path_grid: &PathGrid,
    bridge_records: &[crate::sim::bridge_state::BridgeEndpointRecord],
    width: u16,
    height: u16,
) -> Option<Vec<Option<(u16, u16)>>> {
    if bridge_records.is_empty() {
        return None;
    }

    let total = width as usize * height as usize;
    let mut redirect: Vec<Option<(u16, u16)>> = vec![None; total];
    let mut any = false;

    for ry in 0..height {
        for rx in 0..width {
            if !path_grid.is_walkable_on_layer(rx, ry, MovementLayer::Bridge) {
                continue;
            }

            let mut best_endpoint: Option<(u16, u16)> = None;
            let mut best_dist = u32::MAX;

            for record in bridge_records {
                if !record.active {
                    continue;
                }
                let da = (rx as i32 - record.endpoint_a.0 as i32).unsigned_abs()
                    + (ry as i32 - record.endpoint_a.1 as i32).unsigned_abs();
                let db = (rx as i32 - record.endpoint_b.0 as i32).unsigned_abs()
                    + (ry as i32 - record.endpoint_b.1 as i32).unsigned_abs();
                let closer = if da <= db {
                    (record.endpoint_a, da)
                } else {
                    (record.endpoint_b, db)
                };
                if closer.1 < best_dist {
                    best_dist = closer.1;
                    best_endpoint = Some(closer.0);
                }
            }

            if let Some(ep) = best_endpoint {
                let idx = ry as usize * width as usize + rx as usize;
                redirect[idx] = Some(ep);
                any = true;
            }
        }
    }

    if any { Some(redirect) } else { None }
}

/// Add a bidirectional adjacency edge (avoids duplicates via sorted dedup later).
pub(crate) fn add_adjacency(adj: &mut [Vec<ZoneId>], a: ZoneId, b: ZoneId) {
    adj[a as usize].push(b);
    adj[b as usize].push(a);
}
