//! Zone-based connectivity map for hierarchical pathfinding.
//!
//! The map is partitioned into zones — connected regions of passable cells —
//! per `ZoneCategory`. This enables:
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

/// Canonical passability class — groups MovementZone variants that share the
/// same passability rules into a smaller set for zone computation.
///
/// TODO(RE): The recovered engine layout is per-MovementZone, not per reduced
/// ZoneCategory. This canonicalization is a memory/runtime shortcut until the
/// nodeIndex -> zoneIdByMovementZone tables and YR subzone tracking are wired in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ZoneCategory {
    /// Ground-only units (Normal, Crusher, Destroyer, CrusherAll, Subterranean).
    /// Uses Destroyer as representative — most permissive standard ground profile.
    Land,
    /// Water-only units.
    Water,
    /// Water + beach units.
    WaterBeach,
    /// Amphibious units (land + water).
    Amphibious,
    /// Infantry (slightly different passability than Land).
    Infantry,
    /// Airborne — trivially one zone (all cells reachable). Handled as special case.
    Fly,
}

impl ZoneCategory {
    /// Map a MovementZone to its canonical ZoneCategory.
    pub fn from_movement_zone(mz: MovementZone) -> Self {
        match mz {
            MovementZone::Normal
            | MovementZone::Crusher
            | MovementZone::Destroyer
            | MovementZone::CrusherAll
            | MovementZone::Subterranean => ZoneCategory::Land,
            MovementZone::Water => ZoneCategory::Water,
            MovementZone::WaterBeach => ZoneCategory::WaterBeach,
            MovementZone::AmphibiousCrusher
            | MovementZone::AmphibiousDestroyer
            | MovementZone::Amphibious => ZoneCategory::Amphibious,
            MovementZone::Infantry | MovementZone::InfantryDestroyer => ZoneCategory::Infantry,
            MovementZone::Fly => ZoneCategory::Fly,
        }
    }

    /// Which SpeedType to use for cost grid selection (speed multipliers).
    ///
    /// Note: this is NOT used for passability checks — use
    /// `representative_movement_zone()` for that. SpeedType controls how
    /// *fast* a unit moves on passable cells, while MovementZone controls
    /// *which* cells are passable at all.
    pub(crate) fn representative_speed_type(self) -> SpeedType {
        match self {
            ZoneCategory::Land => SpeedType::Track,
            ZoneCategory::Water => SpeedType::Float,
            ZoneCategory::WaterBeach => SpeedType::FloatBeach,
            ZoneCategory::Amphibious => SpeedType::Amphibious,
            ZoneCategory::Infantry => SpeedType::Foot,
            ZoneCategory::Fly => SpeedType::Winged,
        }
    }

    /// Which MovementZone to use for passability checks during flood-fill.
    ///
    /// Maps each category to the correct passability matrix row via
    /// `zone_layer_for_movement_zone()`. This is the authoritative passability
    /// check matching the original engine's Can_Enter_Cell logic.
    ///
    /// Critical distinction from `representative_speed_type()`:
    /// - SpeedType::Float → zone 9 (hover — passable everywhere except rock)
    /// - MovementZone::Water → zone 10 (water only — ships confined to water)
    ///
    /// Land uses Destroyer (row 2) as representative — most permissive standard
    /// ground profile (Ground + Road + WaterOverlay). This avoids false negatives
    /// for Crusher/Destroyer/CrusherAll units on road cells. Normal units get
    /// false positives on road overlay cells, which A* handles gracefully.
    /// Subterranean has a more exotic profile (can pass Impassable cells) but
    /// is too permissive as a shared representative.
    pub(crate) fn representative_movement_zone(self) -> MovementZone {
        match self {
            ZoneCategory::Land => MovementZone::Destroyer,
            ZoneCategory::Water => MovementZone::Water,
            ZoneCategory::WaterBeach => MovementZone::WaterBeach,
            ZoneCategory::Amphibious => MovementZone::Amphibious,
            ZoneCategory::Infantry => MovementZone::Infantry,
            ZoneCategory::Fly => MovementZone::Fly,
        }
    }

    /// All categories that require zone computation (Fly is trivial).
    pub fn all_nontrivial() -> &'static [ZoneCategory] {
        &[
            ZoneCategory::Land,
            ZoneCategory::Water,
            ZoneCategory::WaterBeach,
            ZoneCategory::Amphibious,
            ZoneCategory::Infantry,
        ]
    }
}

/// Per-zone metadata: centroid and cell count.
/// Used by the hierarchical zone Dijkstra to estimate inter-zone distances.
#[derive(Debug, Clone, Copy, Default)]
pub struct ZoneInfo {
    pub center: (u16, u16),
    pub cell_count: u32,
}

/// Per-category cell-to-zone lookup.
#[derive(Debug, Clone)]
pub struct ZoneMap {
    /// Zone ID per cell, indexed by `y * width + x`. ZONE_INVALID = impassable.
    ///
    /// TODO(RE): RA2/YR does not store zone IDs directly per cell. Each cell carries
    /// a nodeIndex, and each MovementZone has its own zoneIdByNodeIndex table.
    zone_ids: Vec<ZoneId>,
    /// Optional bridge-layer zone IDs (same index space, continuation of zone_count).
    ///
    /// TODO(RE): Bridge-layer zone queries in the original engine go through the
    /// onBridge flag plus ZoneConnection remap records near the cell, not a standalone
    /// bridge zone grid. The recovered gate/record helpers now live in
    /// `sim::bridge_specs`, but the live runtime still lacks stored ZoneConnection
    /// records and bridge-remap integration.
    bridge_zone_ids: Option<Vec<ZoneId>>,
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
        bridge_zone_ids: Option<Vec<ZoneId>>,
        width: u16,
        height: u16,
        zone_count: u16,
        zone_info: Vec<ZoneInfo>,
    ) -> Self {
        Self {
            zone_ids,
            bridge_zone_ids,
            width,
            height,
            zone_count,
            zone_info,
        }
    }

    /// Look up the zone ID for a cell at the given layer.
    pub fn zone_at(&self, x: u16, y: u16, layer: MovementLayer) -> ZoneId {
        if x >= self.width || y >= self.height {
            return ZONE_INVALID;
        }
        let idx = y as usize * self.width as usize + x as usize;
        match layer {
            MovementLayer::Bridge => self
                .bridge_zone_ids
                .as_ref()
                .map(|bz| bz[idx])
                .unwrap_or(ZONE_INVALID),
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

    /// Immutable access to the bridge-layer zone ID array.
    pub(crate) fn bridge_zone_ids_slice(&self) -> Option<&[ZoneId]> {
        self.bridge_zone_ids.as_deref()
    }

    /// Mutable access to the ground-layer zone ID array.
    pub(crate) fn zone_ids_mut(&mut self) -> &mut Vec<ZoneId> {
        &mut self.zone_ids
    }

    /// Mutable access to the bridge-layer zone ID array.
    pub(crate) fn bridge_zone_ids_mut(&mut self) -> Option<&mut Vec<ZoneId>> {
        self.bridge_zone_ids.as_mut()
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

/// Complete zone system: zone maps + adjacency graphs for all categories.
#[derive(Debug, Clone)]
pub struct ZoneGrid {
    maps: BTreeMap<ZoneCategory, ZoneMap>,
    adjacency: BTreeMap<ZoneCategory, ZoneAdjacency>,
    /// Connected-component labels for O(1) reachability checks.
    super_zones: BTreeMap<ZoneCategory, SuperZoneMap>,
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
        Self::build_with_terrain(path_grid, terrain_costs, None, width, height)
    }

    /// Build zone maps using resolved terrain passability when available.
    pub fn build_with_terrain(
        path_grid: &PathGrid,
        terrain_costs: &BTreeMap<SpeedType, TerrainCostGrid>,
        resolved_terrain: Option<&ResolvedTerrainGrid>,
        width: u16,
        height: u16,
    ) -> Self {
        let mut maps = BTreeMap::new();
        let mut adjacency = BTreeMap::new();
        let mut super_zones = BTreeMap::new();

        for &cat in ZoneCategory::all_nontrivial() {
            let speed_type = cat.representative_speed_type();
            let cost_grid = terrain_costs.get(&speed_type);

            let (zone_map, adj) = zone_build::build_zone_map_with_terrain(
                path_grid,
                cost_grid,
                resolved_terrain,
                cat,
                width,
                height,
            );
            let sz = SuperZoneMap::from_adjacency(&adj, zone_map.zone_count);
            super_zones.insert(cat, sz);
            maps.insert(cat, zone_map);
            adjacency.insert(cat, adj);
        }

        ZoneGrid {
            maps,
            adjacency,
            super_zones,
            width,
            height,
        }
    }

    /// Get the zone map for a category.
    pub fn map_for(&self, cat: ZoneCategory) -> Option<&ZoneMap> {
        self.maps.get(&cat)
    }

    /// Get the adjacency graph for a category.
    pub fn adjacency_for(&self, cat: ZoneCategory) -> Option<&ZoneAdjacency> {
        self.adjacency.get(&cat)
    }

    /// Mutable access to the zone map for a category (for incremental updates).
    pub(crate) fn map_mut(&mut self, cat: ZoneCategory) -> Option<&mut ZoneMap> {
        self.maps.get_mut(&cat)
    }

    /// Mutable access to the adjacency graph for a category (for incremental updates).
    pub(crate) fn adjacency_mut(&mut self, cat: ZoneCategory) -> Option<&mut ZoneAdjacency> {
        self.adjacency.get_mut(&cat)
    }

    /// Replace the super-zone map for a category (after incremental adjacency update).
    pub(crate) fn set_super_zone(&mut self, cat: ZoneCategory, sz: SuperZoneMap) {
        self.super_zones.insert(cat, sz);
    }

    /// O(1) reachability check: can a unit of this category reach `to` from `from`?
    /// Returns true if both cells are in the same zone or connected via adjacency.
    /// For truly disconnected regions, this returns false without any A* search.
    pub fn can_reach(
        &self,
        cat: ZoneCategory,
        from: (u16, u16),
        from_layer: MovementLayer,
        to: (u16, u16),
        to_layer: MovementLayer,
    ) -> bool {
        if cat == ZoneCategory::Fly {
            // TODO(RE): Air/jumpjet navigation is not just "one global zone". The RE queue
            // still needs to close which movers bypass grid A*, which still do local tests,
            // and how crowd/yield/replan interacts with those movers.
            return true; // Fly units can reach anywhere
        }
        let Some(zone_map) = self.maps.get(&cat) else {
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
        if let Some(sz) = self.super_zones.get(&cat) {
            return sz.are_connected(za, zb);
        }
        // Fallback to BFS if super-zones not available (should not happen).
        let Some(adj) = self.adjacency.get(&cat) else {
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
