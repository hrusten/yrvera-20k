//! Zone-based connectivity map for hierarchical pathfinding.
//!
//! The map is partitioned into zones — connected regions of passable cells —
//! per `MovementZone`. This enables:
//! - **O(1) reachability checks**: two cells are mutually reachable iff they
//!   share the same zone ID (or zones are connected via the adjacency graph).
//! - **Hierarchical search**: Dijkstra on the zone graph finds a corridor of
//!   zones, then A* only explores cells within that corridor.
//!
//! Zones are computed via flood-fill at map load and rebuilt when terrain
//! changes (building placement/destruction, bridge destruction).
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/pathfinding, sim/terrain_cost, sim/locomotor.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use std::collections::{BTreeMap, VecDeque};

use super::PathGrid;
use super::terrain_cost::TerrainCostGrid;
use super::zone_build;
use super::zone_hierarchy::SuperZoneMap;
use crate::map::resolved_terrain::ResolvedTerrainGrid;
use crate::rules::locomotor_type::{MovementZone, SpeedType};
use crate::sim::movement::locomotor::MovementLayer;

/// Zone ID: 0 = impassable/unassigned, 1+ = valid zone.
pub type ZoneId = u16;

/// Sentinel for impassable or unassigned cells.
pub const ZONE_INVALID: ZoneId = 0;

/// Per-zone metadata: centroid and cell count.
/// Used by the hierarchical zone Dijkstra to estimate inter-zone distances.
#[derive(Debug, Clone, Copy, Default)]
pub struct ZoneInfo {
    pub center: (u16, u16),
    pub cell_count: u32,
}

/// Per-movement-zone cell-to-zone lookup.
#[derive(Debug, Clone)]
pub struct ZoneMap {
    /// Zone ID per cell, indexed by `y * width + x`. ZONE_INVALID = impassable.
    ///
    /// TODO(RE): RA2/YR does not store zone IDs directly per cell. Each cell carries
    /// a nodeIndex, and each MovementZone has its own zoneIdByNodeIndex table.
    zone_ids: Vec<ZoneId>,
    /// Per-cell bridge redirect: for bridge cells, the ground endpoint cell
    /// whose zone ID should be returned for bridge-layer queries.
    /// None = no bridges on map. Mirrors gamemd.exe GetZoneID redirect (0x0056d230).
    bridge_redirect: Option<Vec<Option<(u16, u16)>>>,
    pub width: u16,
    pub height: u16,
    /// Number of distinct zones (ground + bridge combined).
    pub zone_count: u16,
    /// Per-zone centroid and cell count (index = zone_id - 1, since zones are 1-based).
    pub zone_info: Vec<ZoneInfo>,
}

impl ZoneMap {
    /// Construct a ZoneMap from pre-computed arrays.
    pub(crate) fn new(
        zone_ids: Vec<ZoneId>,
        bridge_redirect: Option<Vec<Option<(u16, u16)>>>,
        width: u16,
        height: u16,
        zone_count: u16,
        zone_info: Vec<ZoneInfo>,
    ) -> Self {
        Self {
            zone_ids,
            bridge_redirect,
            width,
            height,
            zone_count,
            zone_info,
        }
    }

    /// Look up the zone ID for a cell at the given layer.
    ///
    /// For bridge-layer queries, returns the ground zone of the nearest bridge
    /// endpoint. This mirrors gamemd.exe GetZoneID bridge redirect (0x0056d230).
    /// If no bridge endpoint record covers this cell, returns ZONE_INVALID.
    pub fn zone_at(&self, x: u16, y: u16, layer: MovementLayer) -> ZoneId {
        if x >= self.width || y >= self.height {
            return ZONE_INVALID;
        }
        let idx = y as usize * self.width as usize + x as usize;
        match layer {
            MovementLayer::Bridge => {
                if let Some(redirect) = &self.bridge_redirect {
                    if let Some(Some((ex, ey))) = redirect.get(idx) {
                        let e_idx = *ey as usize * self.width as usize + *ex as usize;
                        return self.zone_ids.get(e_idx).copied().unwrap_or(ZONE_INVALID);
                    }
                }
                ZONE_INVALID
            }
            _ => self.zone_ids[idx],
        }
    }

    /// Get the centroid and cell count for a zone.
    pub fn info_for(&self, zone_id: ZoneId) -> Option<&ZoneInfo> {
        if zone_id == ZONE_INVALID {
            return None;
        }
        self.zone_info.get(zone_id as usize - 1)
    }

    /// Check if two cells are in the same zone (same layer assumed).
    pub fn same_zone(&self, a: (u16, u16), b: (u16, u16), layer: MovementLayer) -> bool {
        let za = self.zone_at(a.0, a.1, layer);
        let zb = self.zone_at(b.0, b.1, layer);
        za != ZONE_INVALID && za == zb
    }

    /// Immutable access to the ground-layer zone ID array.
    pub(crate) fn zone_ids_slice(&self) -> &[ZoneId] {
        &self.zone_ids
    }

    /// Mutable access to the ground-layer zone ID array.
    pub(crate) fn zone_ids_mut(&mut self) -> &mut Vec<ZoneId> {
        &mut self.zone_ids
    }

    /// Replace the bridge redirect table (e.g. after incremental recomputation).
    pub(crate) fn set_bridge_redirect(&mut self, redirect: Option<Vec<Option<(u16, u16)>>>) {
        self.bridge_redirect = redirect;
    }

    /// Update zone_count (e.g. after incremental zone assignment).
    pub(crate) fn set_zone_count(&mut self, n: u16) {
        self.zone_count = n;
    }

    /// Replace zone_info (e.g. after incremental recomputation).
    pub(crate) fn set_zone_info(&mut self, info: Vec<ZoneInfo>) {
        self.zone_info = info;
    }
}

/// Zone adjacency graph — which zones border each other.
#[derive(Debug, Clone)]
pub struct ZoneAdjacency {
    /// For each zone ID (1-indexed), the sorted list of adjacent zone IDs.
    pub neighbors: Vec<Vec<ZoneId>>,
}

impl ZoneAdjacency {
    /// Construct from a pre-built neighbor list.
    pub(crate) fn new(neighbors: Vec<Vec<ZoneId>>) -> Self {
        Self { neighbors }
    }

    /// Check if two zones are directly adjacent.
    pub fn are_adjacent(&self, a: ZoneId, b: ZoneId) -> bool {
        if a == ZONE_INVALID || b == ZONE_INVALID {
            return false;
        }
        let idx = a as usize;
        if idx >= self.neighbors.len() {
            return false;
        }
        self.neighbors[idx].binary_search(&b).is_ok()
    }

    /// Get the neighbors of a zone.
    pub fn neighbors_of(&self, z: ZoneId) -> &[ZoneId] {
        if z == ZONE_INVALID || z as usize >= self.neighbors.len() {
            return &[];
        }
        &self.neighbors[z as usize]
    }
}

/// Complete zone system: zone maps + adjacency graphs for all movement zones.
#[derive(Debug, Clone)]
pub struct ZoneGrid {
    maps: BTreeMap<MovementZone, ZoneMap>,
    adjacency: BTreeMap<MovementZone, ZoneAdjacency>,
    /// Connected-component labels for O(1) reachability checks.
    super_zones: BTreeMap<MovementZone, SuperZoneMap>,
    pub width: u16,
    pub height: u16,
}

impl ZoneGrid {
    /// Build zone maps for all non-trivial categories from terrain data.
    pub fn build(
        path_grid: &PathGrid,
        terrain_costs: &BTreeMap<SpeedType, TerrainCostGrid>,
        width: u16,
        height: u16,
    ) -> Self {
        Self::build_with_terrain(path_grid, terrain_costs, None, &[], width, height)
    }

    /// Build zone maps using resolved terrain passability when available.
    /// Bridge endpoint records inject cross-bridge adjacency edges for
    /// ground-capable movement zones.
    pub fn build_with_terrain(
        path_grid: &PathGrid,
        terrain_costs: &BTreeMap<SpeedType, TerrainCostGrid>,
        resolved_terrain: Option<&ResolvedTerrainGrid>,
        bridge_records: &[crate::sim::bridge_state::BridgeEndpointRecord],
        width: u16,
        height: u16,
    ) -> Self {
        let mut maps = BTreeMap::new();
        let mut adjacency = BTreeMap::new();
        let mut super_zones = BTreeMap::new();

        for &mz in MovementZone::all_ground() {
            let speed_type = mz.speed_type();
            let cost_grid = terrain_costs.get(&speed_type);

            let (mut zone_map, mut adj) = zone_build::build_zone_map_with_terrain(
                path_grid,
                cost_grid,
                resolved_terrain,
                mz,
                width,
                height,
            );

            if mz.can_use_bridges() {
                zone_build::inject_bridge_adjacency(
                    &mut adj,
                    zone_map.zone_ids_slice(),
                    bridge_records,
                    width,
                );
                zone_map.set_bridge_redirect(zone_build::build_bridge_redirect(
                    path_grid,
                    bridge_records,
                    width,
                    height,
                ));
            }

            let sz = SuperZoneMap::from_adjacency(&adj, zone_map.zone_count);
            super_zones.insert(mz, sz);
            maps.insert(mz, zone_map);
            adjacency.insert(mz, adj);
        }

        ZoneGrid {
            maps,
            adjacency,
            super_zones,
            width,
            height,
        }
    }

    /// Get the zone map for a movement zone.
    pub fn map_for(&self, mz: MovementZone) -> Option<&ZoneMap> {
        self.maps.get(&mz)
    }

    /// Get the adjacency graph for a movement zone.
    pub fn adjacency_for(&self, mz: MovementZone) -> Option<&ZoneAdjacency> {
        self.adjacency.get(&mz)
    }

    /// Mutable access to the zone map for a movement zone (for incremental updates).
    pub(crate) fn map_mut(&mut self, mz: MovementZone) -> Option<&mut ZoneMap> {
        self.maps.get_mut(&mz)
    }

    /// Mutable access to the adjacency graph for a movement zone (for incremental updates).
    pub(crate) fn adjacency_mut(&mut self, mz: MovementZone) -> Option<&mut ZoneAdjacency> {
        self.adjacency.get_mut(&mz)
    }

    /// Replace the super-zone map for a movement zone (after incremental adjacency update).
    pub(crate) fn set_super_zone(&mut self, mz: MovementZone, sz: SuperZoneMap) {
        self.super_zones.insert(mz, sz);
    }

    /// O(1) reachability check: can a unit with this movement zone reach `to` from `from`?
    /// Returns true if both cells are in the same zone or connected via adjacency.
    /// For truly disconnected regions, this returns false without any A* search.
    pub fn can_reach(
        &self,
        mz: MovementZone,
        from: (u16, u16),
        from_layer: MovementLayer,
        to: (u16, u16),
        to_layer: MovementLayer,
    ) -> bool {
        if mz == MovementZone::Fly {
            return true; // Fly units can reach anywhere
        }
        let Some(zone_map) = self.maps.get(&mz) else {
            return true; // No zone data — assume reachable (conservative)
        };
        let za = zone_map.zone_at(from.0, from.1, from_layer);
        let zb = zone_map.zone_at(to.0, to.1, to_layer);
        if za == ZONE_INVALID || zb == ZONE_INVALID {
            return false;
        }
        if za == zb {
            return true;
        }
        // Different zones — O(1) super-zone check (union-find connected components).
        if let Some(sz) = self.super_zones.get(&mz) {
            return sz.are_connected(za, zb);
        }
        // Fallback to BFS if super-zones not available (should not happen).
        let Some(adj) = self.adjacency.get(&mz) else {
            return false;
        };
        zone_graph_connected(adj, za, zb, zone_map.zone_count)
    }
}

/// BFS on the zone adjacency graph to check connectivity.
pub(crate) fn zone_graph_connected(
    adj: &ZoneAdjacency,
    start: ZoneId,
    goal: ZoneId,
    max_zones: u16,
) -> bool {
    if start == goal {
        return true;
    }
    let mut visited = vec![false; max_zones as usize + 1];
    let mut queue = VecDeque::new();
    visited[start as usize] = true;
    queue.push_back(start);

    while let Some(z) = queue.pop_front() {
        for &neighbor in adj.neighbors_of(z) {
            if neighbor == goal {
                return true;
            }
            if !visited[neighbor as usize] {
                visited[neighbor as usize] = true;
                queue.push_back(neighbor);
            }
        }
    }
    false
}

// Tests are declared in zone/mod.rs (zone_map_tests.rs).
