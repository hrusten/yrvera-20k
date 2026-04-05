//! Mutable bridge runtime state layered on top of resolved terrain.
//!
//! Bridges are modeled as terrain, not spawned entities. This module owns the
//! destroyable runtime state used by combat, layered pathing, and bridge-deck
//! fallout handling.
//!
//! TODO(RE): This runtime currently models elevated bridge-deck presence/destruction only.
//! The recovered low-bridge overlay damage progression now lives in
//! `sim::bridge_specs`, but wiring it here still needs mutable overlay state,
//! connected-section selection, and `AtomDamage`/BridgeStrength gate inputs.

use crate::map::resolved_terrain::ResolvedTerrainGrid;
use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct BridgeDamageEvent {
    pub rx: u16,
    pub ry: u16,
    pub damage: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BridgeStateChange {
    pub destroyed_cells: Vec<(u16, u16)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct BridgeRuntimeCell {
    pub deck_present: bool,
    pub destroyed: bool,
    pub destroyable: bool,
    pub deck_level: u8,
    pub bridge_group_id: Option<u16>,
}

/// A bridge's ground-level endpoint pair for zone connectivity.
/// Each record connects two ground cells on opposite sides of a bridge.
/// Mirrors gamemd.exe BridgeRecord at MapClass+0x54 (16 bytes each).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct BridgeEndpointRecord {
    /// Ground cell on one side of the bridge.
    pub endpoint_a: (u16, u16),
    /// Ground cell on the other side of the bridge.
    pub endpoint_b: (u16, u16),
    /// Which bridge group this record belongs to.
    pub group_id: u16,
    /// Whether the bridge is traversable (false = destroyed).
    pub active: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct BridgeRuntimeState {
    width: u16,
    height: u16,
    cells: Vec<Option<BridgeRuntimeCell>>,
    group_cells: BTreeMap<u16, Vec<(u16, u16)>>,
    group_hitpoints: BTreeMap<u16, u16>,
    strength_per_group: u16,
    endpoint_records: Vec<BridgeEndpointRecord>,
}

impl BridgeRuntimeState {
    pub fn from_resolved_terrain(
        terrain: &ResolvedTerrainGrid,
        destroyable: bool,
        strength_per_group: u16,
    ) -> Self {
        let width = terrain.width();
        let height = terrain.height();
        let mut cells = vec![None; width as usize * height as usize];
        let mut group_cells: BTreeMap<u16, Vec<(u16, u16)>> = BTreeMap::new();
        let mut visited = vec![false; cells.len()];
        let mut next_group_id: u16 = 1;

        for cell in terrain.iter() {
            let Some(index) = index_of(width, height, cell.rx, cell.ry) else {
                continue;
            };
            if visited[index] || !cell.has_bridge_deck {
                continue;
            }

            let group_id = next_group_id;
            next_group_id = next_group_id.saturating_add(1);
            let mut queue = VecDeque::from([(cell.rx, cell.ry)]);
            let mut members = Vec::new();

            while let Some((rx, ry)) = queue.pop_front() {
                let Some(idx) = index_of(width, height, rx, ry) else {
                    continue;
                };
                if visited[idx] {
                    continue;
                }
                let Some(resolved) = terrain.cell(rx, ry) else {
                    continue;
                };
                if !resolved.has_bridge_deck {
                    continue;
                }

                visited[idx] = true;
                members.push((rx, ry));
                cells[idx] = Some(BridgeRuntimeCell {
                    deck_present: true,
                    destroyed: false,
                    destroyable,
                    deck_level: resolved.bridge_deck_level,
                    bridge_group_id: Some(group_id),
                });

                for (nx, ny) in cardinal_neighbors(rx, ry, width, height) {
                    if let Some(neighbor) = terrain.cell(nx, ny) {
                        if neighbor.has_bridge_deck {
                            queue.push_back((nx, ny));
                        }
                    }
                }
            }

            if !members.is_empty() {
                group_cells.insert(group_id, members);
            }
        }

        let mut group_hitpoints = BTreeMap::new();
        let strength = strength_per_group.max(1);
        for group_id in group_cells.keys().copied() {
            group_hitpoints.insert(group_id, strength);
        }

        let endpoint_records = compute_bridge_endpoints(&group_cells, terrain, width, height);

        Self {
            width,
            height,
            cells,
            group_cells,
            group_hitpoints,
            strength_per_group: strength,
            endpoint_records,
        }
    }

    pub fn cell(&self, rx: u16, ry: u16) -> Option<&BridgeRuntimeCell> {
        index_of(self.width, self.height, rx, ry)
            .and_then(|idx| self.cells.get(idx))
            .and_then(|cell| cell.as_ref())
    }

    pub fn is_bridge_walkable(&self, rx: u16, ry: u16) -> bool {
        self.cell(rx, ry)
            .is_some_and(|cell| cell.deck_present && !cell.destroyed)
    }

    pub fn apply_damage(&mut self, event: BridgeDamageEvent) -> Option<BridgeStateChange> {
        if event.damage == 0 {
            return None;
        }
        let cell = self.cell(event.rx, event.ry).copied()?;
        if !cell.deck_present || cell.destroyed || !cell.destroyable {
            return None;
        }
        let Some(group_id) = cell.bridge_group_id else {
            return None;
        };
        let hp = self
            .group_hitpoints
            .entry(group_id)
            .or_insert(self.strength_per_group);
        *hp = hp.saturating_sub(event.damage);
        if *hp > 0 {
            return None;
        }

        let mut destroyed_cells = self.group_cells.get(&group_id).cloned().unwrap_or_default();
        destroyed_cells.sort_unstable();
        for &(rx, ry) in &destroyed_cells {
            if let Some(idx) = index_of(self.width, self.height, rx, ry) {
                if let Some(cell) = self.cells[idx].as_mut() {
                    cell.destroyed = true;
                }
            }
        }
        for record in &mut self.endpoint_records {
            if record.group_id == group_id {
                record.active = false;
            }
        }
        Some(BridgeStateChange { destroyed_cells })
    }

    /// Bridge endpoint records for zone connectivity.
    /// Each active record connects ground zones on opposite sides of a bridge.
    pub fn endpoint_records(&self) -> &[BridgeEndpointRecord] {
        &self.endpoint_records
    }

    pub fn iter_cells(&self) -> impl Iterator<Item = ((u16, u16), &BridgeRuntimeCell)> {
        self.cells
            .iter()
            .enumerate()
            .filter_map(move |(idx, cell)| {
                let cell = cell.as_ref()?;
                let rx = (idx % self.width as usize) as u16;
                let ry = (idx / self.width as usize) as u16;
                Some(((rx, ry), cell))
            })
    }
}

fn index_of(width: u16, height: u16, rx: u16, ry: u16) -> Option<usize> {
    (rx < width && ry < height).then_some(ry as usize * width as usize + rx as usize)
}

/// For each bridge group, find the two ground cells on opposite sides.
///
/// Algorithm: collect all ground cells cardinally adjacent to any bridge cell
/// in the group, then pick the pair with maximum Manhattan distance.
fn compute_bridge_endpoints(
    group_cells: &BTreeMap<u16, Vec<(u16, u16)>>,
    terrain: &ResolvedTerrainGrid,
    width: u16,
    height: u16,
) -> Vec<BridgeEndpointRecord> {
    let mut records = Vec::new();

    for (&group_id, members) in group_cells {
        // Collect ground cells adjacent to this bridge group.
        let mut ground_neighbors: Vec<(u16, u16)> = Vec::new();
        for &(bx, by) in members {
            for (nx, ny) in cardinal_neighbors(bx, by, width, height) {
                if members.contains(&(nx, ny)) {
                    continue;
                }
                if let Some(cell) = terrain.cell(nx, ny) {
                    if !cell.ground_walk_blocked && !cell.is_water
                        && !ground_neighbors.contains(&(nx, ny))
                    {
                        ground_neighbors.push((nx, ny));
                    }
                }
            }
        }

        if ground_neighbors.len() < 2 {
            continue;
        }

        // Pick the pair with maximum Manhattan distance.
        let mut best_a = ground_neighbors[0];
        let mut best_b = ground_neighbors[1];
        let mut best_dist: u32 = 0;
        for i in 0..ground_neighbors.len() {
            for j in (i + 1)..ground_neighbors.len() {
                let (ax, ay) = ground_neighbors[i];
                let (bx, by) = ground_neighbors[j];
                let dist = (ax as i32 - bx as i32).unsigned_abs()
                    + (ay as i32 - by as i32).unsigned_abs();
                if dist > best_dist {
                    best_dist = dist;
                    best_a = ground_neighbors[i];
                    best_b = ground_neighbors[j];
                }
            }
        }

        records.push(BridgeEndpointRecord {
            endpoint_a: best_a,
            endpoint_b: best_b,
            group_id,
            active: true,
        });
    }

    records
}

fn cardinal_neighbors(
    rx: u16,
    ry: u16,
    width: u16,
    height: u16,
) -> impl Iterator<Item = (u16, u16)> {
    const OFFSETS: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
    OFFSETS.into_iter().filter_map(move |(dx, dy)| {
        let nx = rx as i32 + dx;
        let ny = ry as i32 + dy;
        (nx >= 0 && ny >= 0 && (nx as u16) < width && (ny as u16) < height)
            .then_some((nx as u16, ny as u16))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::resolved_terrain::{ResolvedTerrainCell, ResolvedTerrainGrid};
    use crate::rules::terrain_rules::{SpeedCostProfile, TerrainClass};

    /// 5x1 grid: ground at (0,0), bridge at (1,0)-(3,0), ground at (4,0).
    fn make_bridge_terrain() -> ResolvedTerrainGrid {
        let mut cells = Vec::new();
        for rx in 0..5u16 {
            let on_bridge = (1..=3).contains(&rx);
            cells.push(ResolvedTerrainCell {
                rx,
                ry: 0,
                source_tile_index: 0,
                source_sub_tile: 0,
                final_tile_index: 0,
                final_sub_tile: 0,
                level: 0,
                filled_clear: false,
                tileset_index: Some(0),
                land_type: 0,
                slope_type: 0,
                template_height: 0,
                render_offset_x: 0,
                render_offset_y: 0,
                terrain_class: TerrainClass::Clear,
                speed_costs: SpeedCostProfile::default(),
                is_water: false,
                is_cliff_like: false,
                is_cliff_redraw: false,
                variant: 0,
                is_rough: false,
                is_road: false,
                has_ramp: false,
                canonical_ramp: None,
                ground_walk_blocked: on_bridge,
                terrain_object_blocks: false,
                overlay_blocks: false,
                zone_type: if on_bridge { 6 } else { 0 },
                base_ground_walk_blocked: false,
                base_build_blocked: false,
                build_blocked: on_bridge,
                has_bridge_deck: on_bridge,
                bridge_walkable: on_bridge,
                bridge_transition: rx == 1 || rx == 3,
                bridge_deck_level: if on_bridge { 4 } else { 0 },
                bridge_layer: None,
                radar_left: [0, 0, 0],
                radar_right: [0, 0, 0],
            });
        }
        ResolvedTerrainGrid::from_cells(5, 1, cells)
    }

    #[test]
    fn bridge_runtime_initializes_intact_groups() {
        let state = BridgeRuntimeState::from_resolved_terrain(&make_bridge_terrain(), true, 300);
        let cell = state.cell(1, 0).expect("bridge cell");
        assert!(cell.deck_present);
        assert!(!cell.destroyed);
        assert_eq!(cell.deck_level, 4);
        assert_eq!(cell.bridge_group_id, Some(1));
        assert!(state.cell(0, 0).is_none());
    }

    #[test]
    fn destroying_a_bridge_group_marks_all_members_destroyed() {
        let mut state = BridgeRuntimeState::from_resolved_terrain(&make_bridge_terrain(), true, 50);
        let change = state
            .apply_damage(BridgeDamageEvent {
                rx: 1,
                ry: 0,
                damage: 50,
            })
            .expect("bridge should be destroyed");
        assert_eq!(change.destroyed_cells, vec![(1, 0), (2, 0), (3, 0)]);
        assert!(!state.is_bridge_walkable(1, 0));
        assert!(!state.is_bridge_walkable(2, 0));
        assert!(!state.is_bridge_walkable(3, 0));
    }

    #[test]
    fn indestructible_bridge_ignores_damage() {
        let mut state =
            BridgeRuntimeState::from_resolved_terrain(&make_bridge_terrain(), false, 50);
        assert!(state
            .apply_damage(BridgeDamageEvent {
                rx: 1,
                ry: 0,
                damage: 50,
            })
            .is_none());
        assert!(state.is_bridge_walkable(1, 0));
    }

    #[test]
    fn bridge_endpoints_detected() {
        let state = BridgeRuntimeState::from_resolved_terrain(&make_bridge_terrain(), true, 300);
        let records = state.endpoint_records();
        assert_eq!(records.len(), 1, "should have exactly one bridge endpoint record");
        let rec = &records[0];
        assert!(rec.active);
        assert_eq!(rec.group_id, 1);
        let endpoints = [rec.endpoint_a, rec.endpoint_b];
        assert!(endpoints.contains(&(0, 0)), "endpoint_a or _b should be (0,0)");
        assert!(endpoints.contains(&(4, 0)), "endpoint_a or _b should be (4,0)");
    }

    #[test]
    fn bridge_destruction_deactivates_endpoints() {
        let mut state = BridgeRuntimeState::from_resolved_terrain(&make_bridge_terrain(), true, 50);
        state.apply_damage(BridgeDamageEvent {
            rx: 1,
            ry: 0,
            damage: 50,
        });
        let records = state.endpoint_records();
        assert!(!records.is_empty());
        assert!(
            !records[0].active,
            "endpoint should be deactivated after destruction"
        );
    }
}
