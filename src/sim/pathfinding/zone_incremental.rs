//! Incremental zone updates — avoids full map rebuild when few cells change.
//!
//! When a building is placed or sold, only a small region of cells changes
//! walkability. Instead of rebuilding all 12 non-Fly movement zones from scratch, this
//! module:
//! 1. Identifies which zone IDs are affected (have cells in the changed region).
//! 2. Clears those zone IDs everywhere on the map.
//! 3. Re-flood-fills the cleared cells to assign new zone IDs.
//! 4. Rebuilds adjacency and super-zone labels for affected categories.
//!
//! Falls back to full rebuild if too many cells changed, resolved-terrain zoning
//! is active, or zone IDs are getting exhausted.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/zone_map, sim/zone_build, sim/zone_hierarchy.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use std::collections::{BTreeMap, BTreeSet};

use super::PathGrid;
use super::terrain_cost::TerrainCostGrid;
use super::zone_build::{
    build_bridge_redirect, compute_zone_info, extract_adjacency, flood_fill,
    inject_bridge_adjacency, is_passable,
};
use super::zone_hierarchy::SuperZoneMap;
use super::zone_map::{ZONE_INVALID, ZoneGrid, ZoneId};
use crate::map::resolved_terrain::ResolvedTerrainGrid;
use crate::rules::locomotor_type::{MovementZone, SpeedType};
use crate::sim::movement::locomotor::MovementLayer;

/// Maximum changed cells before falling back to full rebuild.
pub(crate) const INCREMENTAL_THRESHOLD: usize = 200;

/// Force full rebuild to compact zone IDs when count approaches u16 max.
const ZONE_ID_COMPACTION_THRESHOLD: u16 = 60_000;

/// Padding around changed cells for the affected bounding box.
const BBOX_PADDING: u16 = 2;

/// Attempt an incremental zone update for the given changed cells.
///
/// Returns `true` if the incremental update succeeded. Returns `false` if a
/// full rebuild is needed (too many changes, zone ID exhaustion, etc.).
pub(crate) fn try_incremental_update(
    zone_grid: &mut ZoneGrid,
    changed_cells: &[(u16, u16)],
    path_grid: &PathGrid,
    terrain_costs: &BTreeMap<SpeedType, TerrainCostGrid>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
    bridge_records: &[crate::sim::bridge_state::BridgeEndpointRecord],
) -> bool {
    if changed_cells.is_empty() {
        return true;
    }
    if resolved_terrain.is_some() {
        // TODO(RE): terrain-aware zoning now rebuilds nodeIndex + zoneIdByNodeIndex tables on
        // full rebuilds. We have not closed the exact localized invalidation/update contract for
        // that model yet, so dynamic changes conservatively fall back to a full rebuild.
        return false;
    }
    if changed_cells.len() > INCREMENTAL_THRESHOLD {
        return false;
    }

    let width = zone_grid.width;
    let height = zone_grid.height;

    // Compute bounding box of changed cells with padding.
    let bbox = padded_bbox(changed_cells, width, height);

    // Process each category. We do two passes per category:
    // Pass 1 (mutable): clear + re-flood-fill zone IDs.
    // Pass 2 (immutable reads, then mutable writes): rebuild adjacency/info/super-zones.
    for &mz in MovementZone::all_ground() {
        if !update_category(
            zone_grid,
            mz,
            &bbox,
            changed_cells,
            path_grid,
            terrain_costs,
            bridge_records,
            width,
            height,
        ) {
            return false;
        }
    }

    true
}

/// Update one movement zone incrementally. Returns false if full rebuild needed.
fn update_category(
    zone_grid: &mut ZoneGrid,
    mz: MovementZone,
    bbox: &(u16, u16, u16, u16),
    changed_cells: &[(u16, u16)],
    path_grid: &PathGrid,
    terrain_costs: &BTreeMap<SpeedType, TerrainCostGrid>,
    bridge_records: &[crate::sim::bridge_state::BridgeEndpointRecord],
    width: u16,
    height: u16,
) -> bool {
    let speed_type = mz.speed_type();
    let cost_grid = terrain_costs.get(&speed_type);
    let w = width as usize;
    let (bbox_min_x, bbox_min_y, bbox_max_x, bbox_max_y) = *bbox;

    let Some(zone_map) = zone_grid.map_mut(mz) else {
        return true;
    };

    // Check zone ID exhaustion.
    if zone_map.zone_count >= ZONE_ID_COMPACTION_THRESHOLD {
        return false;
    }

    // --- Pass 1: Collect affected zones, clear, re-flood-fill ---

    // Collect affected ground zone IDs inside bbox.
    let mut affected_ground: BTreeSet<ZoneId> = BTreeSet::new();
    for ry in bbox_min_y..=bbox_max_y {
        for rx in bbox_min_x..=bbox_max_x {
            let idx = ry as usize * w + rx as usize;
            let zid = zone_map.zone_ids_slice()[idx];
            if zid != ZONE_INVALID {
                affected_ground.insert(zid);
            }
        }
    }

    // If no zones affected, check if newly-passable cells appeared.
    if affected_ground.is_empty() {
        let any_new_passable = changed_cells.iter().any(|&(cx, cy)| {
            is_passable(
                cx,
                cy,
                mz,
                path_grid,
                cost_grid,
                None,
                MovementLayer::Ground,
            )
        });
        if !any_new_passable {
            return true; // nothing to do for this movement zone
        }
    }

    // Clear affected zone IDs everywhere.
    let ground_ids = zone_map.zone_ids_mut();
    for zid in ground_ids.iter_mut() {
        if affected_ground.contains(zid) {
            *zid = ZONE_INVALID;
        }
    }
    // Re-flood-fill cleared ground cells.
    let mut next_zone = zone_map.zone_count + 1;
    let ground_ids = zone_map.zone_ids_mut();
    for ry in 0..height {
        for rx in 0..width {
            let idx = ry as usize * w + rx as usize;
            if ground_ids[idx] != ZONE_INVALID {
                continue;
            }
            if !is_passable(
                rx,
                ry,
                mz,
                path_grid,
                cost_grid,
                None,
                MovementLayer::Ground,
            ) {
                continue;
            }
            flood_fill(
                rx,
                ry,
                next_zone,
                ground_ids,
                width,
                height,
                mz,
                path_grid,
                cost_grid,
                None,
                MovementLayer::Ground,
            );
            next_zone += 1;
        }
    }

    let new_zone_count = next_zone - 1;
    zone_map.set_zone_count(new_zone_count);

    // --- Pass 2: Rebuild adjacency, zone_info, super-zones ---
    let Some(zone_map) = zone_grid.map_for(mz) else {
        return false;
    };
    let ground_slice = zone_map.zone_ids_slice();

    let mut new_adj = extract_adjacency(
        ground_slice,
        width,
        height,
        new_zone_count,
    );

    // Inject bridge adjacency for ground-capable movement zones.
    if mz.can_use_bridges() {
        inject_bridge_adjacency(&mut new_adj, ground_slice, bridge_records, width);
    }

    let new_info = compute_zone_info(ground_slice, width, height, new_zone_count);
    let new_sz = SuperZoneMap::from_adjacency(&new_adj, new_zone_count);

    // Rebuild bridge redirect.
    let bridge_redirect = if mz.can_use_bridges() {
        build_bridge_redirect(path_grid, bridge_records, width, height)
    } else {
        None
    };

    // Apply computed results back.
    let Some(zone_map) = zone_grid.map_mut(mz) else {
        return false;
    };
    zone_map.set_zone_info(new_info);
    zone_map.set_bridge_redirect(bridge_redirect);

    if let Some(adj) = zone_grid.adjacency_mut(mz) {
        *adj = new_adj;
    }
    zone_grid.set_super_zone(mz, new_sz);

    true
}

/// Compute the padded bounding box around changed cells, clamped to map bounds.
fn padded_bbox(changed_cells: &[(u16, u16)], width: u16, height: u16) -> (u16, u16, u16, u16) {
    let mut min_x = u16::MAX;
    let mut min_y = u16::MAX;
    let mut max_x = 0u16;
    let mut max_y = 0u16;
    for &(x, y) in changed_cells {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    (
        min_x.saturating_sub(BBOX_PADDING),
        min_y.saturating_sub(BBOX_PADDING),
        (max_x + BBOX_PADDING).min(width - 1),
        (max_y + BBOX_PADDING).min(height - 1),
    )
}
