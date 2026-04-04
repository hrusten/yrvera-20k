//! Per-cell mutable overlay state — runtime fork of the immutable map OverlayPack.
//!
//! Mirrors CellClass +0x44 (OverlayTypeIndex) and +0x11E (OverlayData) from gamemd.exe.
//! Seeded from map data at init, mutated during gameplay by ore growth, wall damage,
//! and bridge overlay damage.
//!
//! Dependency rules: depends on map/overlay (OverlayEntry for seeding).
//! Never depends on render/, ui/, sidebar/, audio/, net/.

use crate::map::overlay::OverlayEntry;
use crate::map::overlay_types::OverlayTypeRegistry;
use crate::map::resolved_terrain::ResolvedTerrainGrid;

/// Per-cell mutable overlay state — mirrors CellClass +0x44 / +0x11E.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct OverlayCell {
    /// Overlay type index into OverlayTypeRegistry. None = no overlay.
    pub overlay_id: Option<u8>,
    /// Multi-purpose data byte:
    /// - Ore/gems (Tiberium=true): density 0-11 (SHP frame index)
    /// - Walls (Wall=true): (damage_level << 4) | connectivity_bitmask
    ///   Connectivity: N=1, E=2, S=4, W=8
    /// - Bridges: damage state 0-17 (EW 0-8, NS 9-17)
    /// - Other: raw frame index
    pub overlay_data: u8,
}

impl Default for OverlayCell {
    fn default() -> Self {
        Self {
            overlay_id: None,
            overlay_data: 0,
        }
    }
}

/// Mutable overlay state grid — seeded from map [OverlayPack] at init,
/// mutated during gameplay by ore growth, wall damage, bridge damage.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OverlayGrid {
    width: u16,
    height: u16,
    cells: Vec<OverlayCell>,
}

impl OverlayGrid {
    /// Create an empty grid with no overlays.
    pub fn new(width: u16, height: u16) -> Self {
        let count = width as usize * height as usize;
        Self {
            width,
            height,
            cells: vec![OverlayCell::default(); count],
        }
    }

    /// Seed from parsed map overlay entries.
    pub fn from_overlay_entries(entries: &[OverlayEntry], width: u16, height: u16) -> Self {
        let mut grid = Self::new(width, height);
        for entry in entries {
            if let Some(idx) = index_of(width, height, entry.rx, entry.ry) {
                grid.cells[idx] = OverlayCell {
                    overlay_id: Some(entry.overlay_id),
                    overlay_data: entry.frame,
                };
            }
        }
        grid
    }

    /// Read cell at (rx, ry). Returns default (no overlay) for out-of-bounds.
    pub fn cell(&self, rx: u16, ry: u16) -> &OverlayCell {
        match index_of(self.width, self.height, rx, ry) {
            Some(idx) => &self.cells[idx],
            None => &DEFAULT_CELL,
        }
    }

    /// Mutable access to cell. Panics if out-of-bounds.
    pub fn cell_mut(&mut self, rx: u16, ry: u16) -> &mut OverlayCell {
        let idx =
            index_of(self.width, self.height, rx, ry).expect("OverlayGrid::cell_mut out of bounds");
        &mut self.cells[idx]
    }

    /// Remove overlay from cell entirely. Returns previous overlay_id if any.
    pub fn clear_overlay(&mut self, rx: u16, ry: u16) -> Option<u8> {
        let idx = index_of(self.width, self.height, rx, ry)?;
        let prev = self.cells[idx].overlay_id;
        self.cells[idx] = OverlayCell::default();
        prev
    }

    /// Place overlay at cell.
    pub fn place_overlay(&mut self, rx: u16, ry: u16, overlay_id: u8, data: u8) {
        if let Some(idx) = index_of(self.width, self.height, rx, ry) {
            self.cells[idx] = OverlayCell {
                overlay_id: Some(overlay_id),
                overlay_data: data,
            };
        }
    }

    /// Update overlay_data in place (density change, damage increment).
    /// No-op if out-of-bounds or cell has no overlay.
    pub fn set_overlay_data(&mut self, rx: u16, ry: u16, data: u8) {
        if let Some(idx) = index_of(self.width, self.height, rx, ry) {
            if self.cells[idx].overlay_id.is_some() {
                self.cells[idx].overlay_data = data;
            }
        }
    }

    /// Count ore/gem neighbors (8-dir) on demand.
    pub fn count_ore_neighbors(&self, rx: u16, ry: u16, registry: &OverlayTypeRegistry) -> u8 {
        let mut count: u8 = 0;
        for (dx, dy) in ADJACENT_8 {
            let nx = rx as i32 + dx;
            let ny = ry as i32 + dy;
            if nx < 0 || ny < 0 {
                continue;
            }
            let cell = self.cell(nx as u16, ny as u16);
            if let Some(id) = cell.overlay_id {
                if registry.flags(id).is_some_and(|f| f.tiberium) {
                    count += 1;
                }
            }
        }
        count
    }

    /// Iterate all cells that have an overlay (for hashing).
    pub fn iter_occupied(&self) -> impl Iterator<Item = (u16, u16, &OverlayCell)> {
        self.cells.iter().enumerate().filter_map(move |(idx, cell)| {
            if cell.overlay_id.is_some() {
                let rx = (idx % self.width as usize) as u16;
                let ry = (idx / self.width as usize) as u16;
                Some((rx, ry, cell))
            } else {
                None
            }
        })
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }
}

/// Recompute overlay_blocks on ResolvedTerrainCell after an overlay mutation.
///
/// Reads overlay_id from the grid, checks registry flags for wall/tiberium/rock,
/// computes new overlay_blocks value, writes to resolved_terrain. Returns true if
/// the passability value changed (caller should trigger zone rebuild).
///
/// Mirrors gamemd.exe RecalcAttributes stage 3a, scoped to overlay->passability.
pub fn recalc_overlay_passability(
    overlay_grid: &OverlayGrid,
    resolved_terrain: &mut ResolvedTerrainGrid,
    registry: &OverlayTypeRegistry,
    rx: u16,
    ry: u16,
) -> bool {
    use crate::map::resolved_terrain::zone_class;

    let cell = overlay_grid.cell(rx, ry);
    let (new_blocks, new_zone_type) = match cell.overlay_id {
        Some(id) => {
            let flags = registry.flags(id);
            let blocks = flags.is_some_and(|f| f.wall || f.tiberium);
            // Mirror RecalcZoneType priority for overlay-driven zone classification.
            let zt = match flags {
                Some(f) if f.crate_type => zone_class::ROAD,
                Some(f) if f.wall => zone_class::WALL,
                Some(f) if f.tiberium => zone_class::IMPASSABLE,
                Some(f) if f.is_gate => zone_class::IMPASSABLE,
                _ => zone_class::GROUND, // Fallback — refined below from base terrain
            };
            (blocks, zt)
        }
        None => (false, zone_class::GROUND), // No overlay — refined below
    };

    let Some(terrain_cell) = resolved_terrain.cell_mut(rx, ry) else {
        return false;
    };

    let old_blocks = terrain_cell.overlay_blocks;
    terrain_cell.overlay_blocks = new_blocks;

    // If the overlay didn't determine a specific zone_type (GROUND fallback),
    // re-derive from base terrain, matching the init-time logic.
    // Uses `base_ground_walk_blocked` (terrain-only) to avoid the conflated
    // `ground_walk_blocked` which includes stale overlay/terrain-object contributions.
    let final_zone_type = if new_zone_type != zone_class::GROUND {
        new_zone_type
    } else if terrain_cell.is_water {
        zone_class::WATER
    } else if terrain_cell.land_type
        == crate::sim::pathfinding::passability::LandType::Beach.as_index()
    {
        zone_class::BEACH
    } else if terrain_cell.base_ground_walk_blocked {
        zone_class::IMPASSABLE
    } else if terrain_cell.terrain_object_blocks {
        zone_class::BUILDING
    } else {
        zone_class::GROUND
    };

    let old_zone = terrain_cell.zone_type;
    terrain_cell.zone_type = final_zone_type;

    old_blocks != new_blocks || old_zone != final_zone_type
}

/// Result of a wall damage attempt.
#[derive(Debug, Clone, Default)]
pub struct WallDamageResult {
    /// Cells where overlay_data changed (need re-render).
    pub changed_cells: Vec<(u16, u16)>,
    /// Cells where wall was fully destroyed (need zone rebuild + render removal).
    pub destroyed_cells: Vec<(u16, u16)>,
}

/// Damage a wall overlay, matching gamemd.exe CellClass::DestroyOverlay (0x00480CB0).
///
/// 1. Random damage check against Strength
/// 2. Increment damage level (upper nibble of overlay_data)
/// 3. At penultimate damage level: chain-damage cardinal neighbors
/// 4. At full destruction: clear overlay, add to destroyed list
///
/// `damage == u16::MAX` bypasses the random check (forced destruction).
pub fn damage_wall_overlay(
    overlay_grid: &mut OverlayGrid,
    registry: &OverlayTypeRegistry,
    rx: u16,
    ry: u16,
    damage: u16,
    rng: &mut crate::sim::rng::SimRng,
) -> WallDamageResult {
    let mut result = WallDamageResult::default();
    damage_wall_recursive(overlay_grid, registry, rx, ry, damage, rng, &mut result);
    result
}

fn damage_wall_recursive(
    grid: &mut OverlayGrid,
    registry: &OverlayTypeRegistry,
    rx: u16,
    ry: u16,
    damage: u16,
    rng: &mut crate::sim::rng::SimRng,
    result: &mut WallDamageResult,
) {
    let cell = *grid.cell(rx, ry);
    let Some(overlay_id) = cell.overlay_id else {
        return;
    };
    let Some(flags) = registry.flags(overlay_id) else {
        return;
    };
    if !flags.wall {
        return;
    }

    // Random damage check: if damage < Strength && Random(0, Strength) > damage -> no effect.
    if damage != u16::MAX && flags.strength > 0 && damage < flags.strength {
        let roll = rng.next_range_u32(flags.strength as u32) as u16;
        if roll > damage {
            return;
        }
    }

    // Increment damage level (upper nibble).
    let new_data = cell.overlay_data.wrapping_add(0x10);
    let damage_level = new_data >> 4;

    // At penultimate damage level: chain-damage cardinal neighbors.
    if flags.damage_levels > 2
        && damage_level == (flags.damage_levels as u8).saturating_sub(1)
    {
        const CARDINAL: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)];
        for (dx, dy) in CARDINAL {
            let nx = rx as i32 + dx;
            let ny = ry as i32 + dy;
            if nx < 0 || ny < 0 {
                continue;
            }
            let (nx, ny) = (nx as u16, ny as u16);
            let neighbor = *grid.cell(nx, ny);
            if neighbor.overlay_id == Some(overlay_id) && (neighbor.overlay_data >> 4) == 0 {
                damage_wall_recursive(grid, registry, nx, ny, 200, rng, result);
            }
        }
    }

    // Check if fully destroyed.
    if damage != u16::MAX && (damage_level as u16) < flags.damage_levels {
        // Not fully destroyed — just update damage level.
        grid.set_overlay_data(rx, ry, new_data);
        result.changed_cells.push((rx, ry));
        return;
    }

    // Full destruction.
    grid.clear_overlay(rx, ry);
    result.destroyed_cells.push((rx, ry));
}

/// 8-direction offsets: N, NE, E, SE, S, SW, W, NW.
const ADJACENT_8: [(i32, i32); 8] = [
    (0, -1),
    (1, -1),
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
];

/// Static default for out-of-bounds reads.
const DEFAULT_CELL: OverlayCell = OverlayCell {
    overlay_id: None,
    overlay_data: 0,
};

fn index_of(width: u16, height: u16, rx: u16, ry: u16) -> Option<usize> {
    (rx < width && ry < height).then_some(ry as usize * width as usize + rx as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_grid_is_empty() {
        let grid = OverlayGrid::new(4, 4);
        assert_eq!(grid.cell(0, 0).overlay_id, None);
        assert_eq!(grid.cell(3, 3).overlay_id, None);
    }

    #[test]
    fn from_overlay_entries_seeds_cells() {
        let entries = vec![
            OverlayEntry {
                rx: 1,
                ry: 2,
                overlay_id: 5,
                frame: 7,
            },
            OverlayEntry {
                rx: 3,
                ry: 0,
                overlay_id: 10,
                frame: 0,
            },
        ];
        let grid = OverlayGrid::from_overlay_entries(&entries, 4, 4);
        assert_eq!(grid.cell(1, 2).overlay_id, Some(5));
        assert_eq!(grid.cell(1, 2).overlay_data, 7);
        assert_eq!(grid.cell(3, 0).overlay_id, Some(10));
        assert_eq!(grid.cell(0, 0).overlay_id, None);
    }

    #[test]
    fn place_and_clear_overlay() {
        let mut grid = OverlayGrid::new(4, 4);
        grid.place_overlay(2, 2, 42, 11);
        assert_eq!(grid.cell(2, 2).overlay_id, Some(42));
        assert_eq!(grid.cell(2, 2).overlay_data, 11);

        let prev = grid.clear_overlay(2, 2);
        assert_eq!(prev, Some(42));
        assert_eq!(grid.cell(2, 2).overlay_id, None);
    }

    #[test]
    fn set_overlay_data_updates_existing() {
        let mut grid = OverlayGrid::new(4, 4);
        grid.place_overlay(1, 1, 5, 3);
        grid.set_overlay_data(1, 1, 9);
        assert_eq!(grid.cell(1, 1).overlay_data, 9);
        assert_eq!(grid.cell(1, 1).overlay_id, Some(5));
    }

    #[test]
    fn set_overlay_data_noop_on_empty_cell() {
        let mut grid = OverlayGrid::new(4, 4);
        grid.set_overlay_data(1, 1, 9);
        assert_eq!(grid.cell(1, 1).overlay_id, None);
        assert_eq!(grid.cell(1, 1).overlay_data, 0);
    }

    #[test]
    fn out_of_bounds_returns_default() {
        let grid = OverlayGrid::new(2, 2);
        assert_eq!(grid.cell(5, 5).overlay_id, None);
    }

    #[test]
    fn iter_occupied_skips_empty() {
        let mut grid = OverlayGrid::new(3, 3);
        grid.place_overlay(0, 0, 1, 0);
        grid.place_overlay(2, 2, 2, 5);
        let occupied: Vec<_> = grid.iter_occupied().collect();
        assert_eq!(occupied.len(), 2);
        assert_eq!(occupied[0].0, 0); // rx
        assert_eq!(occupied[0].1, 0); // ry
        assert_eq!(occupied[1].0, 2);
        assert_eq!(occupied[1].1, 2);
    }
}
