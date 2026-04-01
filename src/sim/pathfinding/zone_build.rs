//! Zone map construction: flood-fill, adjacency extraction, zone info computation.
//!
//! Extracted from zone_map.rs to keep each file under ~400 lines.
//! This module is private to sim/ — public API lives in zone_map.rs.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/pathfinding, sim/terrain_cost, sim/locomotor.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use std::collections::VecDeque;

use super::passability;
use super::terrain_cost::TerrainCostGrid;
use super::zone_map::{ZONE_INVALID, ZoneAdjacency, ZoneCategory, ZoneId, ZoneInfo, ZoneMap};
use super::{LayeredPathGrid, PathGrid};
use crate::map::resolved_terrain::ResolvedTerrainGrid;
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

/// Build a zone map and adjacency graph for one ZoneCategory.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn build_zone_map(
    path_grid: &PathGrid,
    layered_grid: Option<&LayeredPathGrid>,
    cost_grid: Option<&TerrainCostGrid>,
    cat: ZoneCategory,
    width: u16,
    height: u16,
) -> (ZoneMap, ZoneAdjacency) {
    build_zone_map_with_terrain(path_grid, layered_grid, cost_grid, None, cat, width, height)
}

/// Build a zone map using the passability matrix when resolved terrain is available.
///
/// When `resolved_terrain` is provided, uses the original engine's passability matrix
/// lookups on the cell's `land_type` field — this is the authoritative check
/// matching the original engine. Falls back to PathGrid + TerrainCostGrid
/// checks when resolved terrain is not available.
pub(crate) fn build_zone_map_with_terrain(
    path_grid: &PathGrid,
    layered_grid: Option<&LayeredPathGrid>,
    cost_grid: Option<&TerrainCostGrid>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
    cat: ZoneCategory,
    width: u16,
    height: u16,
) -> (ZoneMap, ZoneAdjacency) {
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
                cat,
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
                cat,
                path_grid,
                cost_grid,
                resolved_terrain,
                layered_grid,
                MovementLayer::Ground,
            );
            next_zone += 1;
        }
    }

    let _ground_zone_count = next_zone - 1;

    // -- Bridge layer flood-fill (if applicable) --
    let bridge_zone_ids = if matches!(
        cat,
        ZoneCategory::Land | ZoneCategory::Infantry | ZoneCategory::Amphibious
    ) {
        if let Some(lg) = layered_grid {
            let mut bz = vec![ZONE_INVALID; total];
            for ry in 0..height {
                for rx in 0..width {
                    let idx = ry as usize * width as usize + rx as usize;
                    if bz[idx] != ZONE_INVALID {
                        continue;
                    }
                    if !lg.is_walkable(rx, ry, MovementLayer::Bridge) {
                        continue;
                    }
                    flood_fill_bridge(rx, ry, next_zone, &mut bz, width, height, lg);
                    next_zone += 1;
                }
            }
            Some(bz)
        } else {
            None
        }
    } else {
        None
    };

    let zone_count = next_zone - 1;

    // -- Extract adjacency --
    let adj = extract_adjacency(
        &zone_ids,
        bridge_zone_ids.as_deref(),
        layered_grid,
        width,
        height,
        zone_count,
    );

    // Compute zone centroids from cell assignments.
    let zone_info = compute_zone_info(
        &zone_ids,
        bridge_zone_ids.as_deref(),
        width,
        height,
        zone_count,
    );

    let zone_map = ZoneMap::new(
        zone_ids,
        bridge_zone_ids,
        width,
        height,
        zone_count,
        zone_info,
    );

    (zone_map, adj)
}

/// Check if a cell is passable for a given ZoneCategory.
///
/// When `resolved_terrain` is available, uses the original engine's passability
/// matrix (land_type → zone layer → passable/blocked/impassable). This is the
/// authoritative check matching the original engine's zone flood-fill.
///
/// Falls back to PathGrid + TerrainCostGrid checks when resolved terrain data
/// is not available (e.g. in unit tests).
pub(crate) fn is_passable(
    x: u16,
    y: u16,
    cat: ZoneCategory,
    path_grid: &PathGrid,
    cost_grid: Option<&TerrainCostGrid>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
    _layer: MovementLayer,
) -> bool {
    // Buildings and static obstacles block all ground movement regardless of
    // land type. PathGrid encodes this (set_blocked for building footprints).
    // Water zones skip this check since water cells are typically blocked in PathGrid.
    let is_water_zone = matches!(cat, ZoneCategory::Water | ZoneCategory::WaterBeach);
    if !is_water_zone && !path_grid.is_walkable(x, y) {
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
            let mz = cat.representative_movement_zone();
            if mz.is_water_mover() {
                return super::is_water_surface_cell_passable(cell, mz);
            }
            return passability::is_passable_for_zone(cell.land_type, mz);
        }
    }

    // Fallback: TerrainCostGrid-based check (pre-matrix behavior).
    match cat {
        ZoneCategory::Water | ZoneCategory::WaterBeach => {
            if let Some(cg) = cost_grid {
                cg.cost_at(x, y) > 0
            } else {
                false
            }
        }
        _ => {
            if let Some(cg) = cost_grid {
                cg.cost_at(x, y) > 0
            } else {
                true
            }
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
    cat: ZoneCategory,
    path_grid: &PathGrid,
    cost_grid: Option<&TerrainCostGrid>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
    layered_grid: Option<&LayeredPathGrid>,
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
            if !is_passable(nx, ny, cat, path_grid, cost_grid, resolved_terrain, layer) {
                continue;
            }

            // Diagonal corner-cutting: both adjacent cardinals must be passable.
            if is_diagonal {
                let ax = (cx as i32 + dx) as u16;
                let ay = cy;
                let bx = cx;
                let by = (cy as i32 + dy) as u16;
                if !is_passable(ax, ay, cat, path_grid, cost_grid, resolved_terrain, layer)
                    || !is_passable(bx, by, cat, path_grid, cost_grid, resolved_terrain, layer)
                {
                    continue;
                }
            }

            // Height continuity: original engine enforces abs(h_diff) <= 1 in zone
            // flood-fill. Height jumps > 1 create zone boundaries so the zone system
            // never claims two cells are mutually reachable when A* would fail due to
            // a cliff. Only checked on the ground layer for land-based categories.
            if layer == MovementLayer::Ground {
                if let Some(lg) = layered_grid {
                    if let (Some(cur), Some(nbr)) = (lg.cell(cx, cy), lg.cell(nx, ny)) {
                        if (cur.ground_level as i16 - nbr.ground_level as i16).abs() > 1 {
                            continue;
                        }
                    }
                }
            }

            zone_ids[n_idx] = zone_id;
            queue.push_back((nx, ny));
        }
    }
}

/// BFS flood-fill on the bridge layer.
pub(crate) fn flood_fill_bridge(
    start_x: u16,
    start_y: u16,
    zone_id: ZoneId,
    zone_ids: &mut [ZoneId],
    width: u16,
    height: u16,
    lg: &LayeredPathGrid,
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
            if !lg.is_walkable(nx, ny, MovementLayer::Bridge) {
                continue;
            }

            // Diagonal corner-cutting on bridge layer.
            if is_diagonal {
                let ax = (cx as i32 + dx) as u16;
                let bx = cx;
                let by = (cy as i32 + dy) as u16;
                if !lg.is_walkable(ax, cy, MovementLayer::Bridge)
                    || !lg.is_walkable(bx, by, MovementLayer::Bridge)
                {
                    continue;
                }
            }

            zone_ids[n_idx] = zone_id;
            queue.push_back((nx, ny));
        }
    }
}

/// Compute per-zone centroid and cell count from the zone ID arrays.
pub(crate) fn compute_zone_info(
    zone_ids: &[ZoneId],
    bridge_zone_ids: Option<&[ZoneId]>,
    width: u16,
    _height: u16,
    zone_count: u16,
) -> Vec<ZoneInfo> {
    let mut sums: Vec<(u64, u64, u32)> = vec![(0, 0, 0); zone_count as usize];
    // Accumulate from ground layer.
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
    // Accumulate from bridge layer.
    if let Some(bz) = bridge_zone_ids {
        for (idx, &zid) in bz.iter().enumerate() {
            if zid != ZONE_INVALID {
                let x = (idx % width as usize) as u64;
                let y = (idx / width as usize) as u64;
                let entry = &mut sums[zid as usize - 1];
                entry.0 += x;
                entry.1 += y;
                entry.2 += 1;
            }
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

/// Extract adjacency from zone ID arrays. Also creates edges between ground
/// and bridge zones at transition cells.
pub(crate) fn extract_adjacency(
    ground_zones: &[ZoneId],
    bridge_zones: Option<&[ZoneId]>,
    layered_grid: Option<&LayeredPathGrid>,
    width: u16,
    height: u16,
    zone_count: u16,
) -> ZoneAdjacency {
    // neighbors[z] = set of adjacent zone IDs for zone z.
    let mut adj_sets: Vec<Vec<ZoneId>> = vec![Vec::new(); zone_count as usize + 1];

    let w = width as usize;

    // Scan all cells for ground-layer adjacency.
    for ry in 0..height {
        for rx in 0..width {
            let idx = ry as usize * w + rx as usize;
            let z = ground_zones[idx];
            if z == ZONE_INVALID {
                continue;
            }

            // Check right and down neighbors (avoids double-counting).
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

    // Bridge-layer adjacency.
    if let Some(bz) = bridge_zones {
        for ry in 0..height {
            for rx in 0..width {
                let idx = ry as usize * w + rx as usize;
                let z = bz[idx];
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
                    let nz = bz[n_idx];
                    if nz != ZONE_INVALID && nz != z {
                        add_adjacency(&mut adj_sets, z, nz);
                    }
                }

                // Transition edge: bridge zone <-> ground zone at transition cells.
                if let Some(lg) = layered_grid {
                    if lg.is_transition(rx, ry) {
                        let gz = ground_zones[idx];
                        if gz != ZONE_INVALID && z != gz {
                            add_adjacency(&mut adj_sets, z, gz);
                        }
                    }
                }
            }
        }
    }

    // Sort and dedup all neighbor lists for determinism and binary search.
    for list in &mut adj_sets {
        list.sort_unstable();
        list.dedup();
    }

    ZoneAdjacency::new(adj_sets)
}

/// Add a bidirectional adjacency edge (avoids duplicates via sorted dedup later).
pub(crate) fn add_adjacency(adj: &mut [Vec<ZoneId>], a: ZoneId, b: ZoneId) {
    adj[a as usize].push(b);
    adj[b as usize].push(a);
}
