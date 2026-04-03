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
    let cell = overlay_grid.cell(rx, ry);
    let new_blocks = match cell.overlay_id {
        Some(id) => registry.flags(id).is_some_and(|f| f.wall || f.tiberium),
        None => false,
    };

    let Some(terrain_cell) = resolved_terrain.cell_mut(rx, ry) else {
        return false;
    };
    let old_blocks = terrain_cell.overlay_blocks;
    terrain_cell.overlay_blocks = new_blocks;
    old_blocks != new_blocks
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
