//! A* pathfinding on the isometric grid.
//!
//! PathGrid is a flat boolean array (true = walkable) indexed by (rx, ry).
//! A* uses octile heuristic (consistent with 8-dir movement, never overestimates).
//!
//! ## Dependency rules
//! - Part of sim/ — depends on map/ (MapCell, TilesetLookup for walkability).
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use super::passability;
use super::terrain_cost::TerrainCostGrid;
use crate::map::map_file::MapCell;
use crate::map::resolved_terrain::{ResolvedTerrainCell, ResolvedTerrainGrid};
use crate::map::theater::TilesetLookup;
use crate::rules::locomotor_type::MovementZone;
use crate::sim::bridge_state::BridgeRuntimeState;
use crate::sim::movement::locomotor::MovementLayer;
use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap};

/// Movement cost for a cardinal step (N, E, S, W). Scaled by 10 for integer math.
const CARDINAL_COST: i32 = 10;

/// Movement cost for a diagonal step (NE, SE, SW, NW). Approximates sqrt(2) * 10.
const DIAGONAL_COST: i32 = 14;

/// Maximum nodes to evaluate before giving up. Prevents freezing on
/// pathologically large or impossible searches.
/// Original engine uses 65,527 (0xFFF7). We match this to avoid failing
/// on complex paths that the original would find.
const MAX_SEARCH_NODES: u32 = 65_527;

/// RA2 computes paths in 24-step segments. When a segment is exhausted before
/// reaching the destination, the pathfinder replans from the current position.
/// This limits lookahead and makes units adapt to obstacles discovered en route.
pub const MAX_PATH_SEGMENT_STEPS: usize = 24;

/// Cost multiplier for cells with height transitions (ramps, slopes).
/// Original engine applies 4.0x base cost for cliff cells (0x007e37bc = 4.0).
/// With CARDINAL_COST=10, a height step costs 40 instead of 10.
const CLIFF_COST_MULTIPLIER: i32 = 4;

/// Per-direction tie-breaker offsets added to g-cost.
/// Original engine adds tiny floats (0.001–0.008) from table at 0x0081872c.
/// We scale by 10000 to stay in integer math: cardinals get lower values than
/// diagonals, preventing path oscillation when multiple routes have equal cost.
/// Index order matches NEIGHBORS: N, NE, E, SE, S, SW, W, NW.
const DIR_TIEBREAK: [i32; 8] = [
    1,  // N   (original ≈0.001)
    5,  // NE  (original ≈0.005)
    2,  // E   (original ≈0.002)
    6,  // SE  (original ≈0.006)
    3,  // S   (original ≈0.003)
    7,  // SW  (original ≈0.007)
    4,  // W   (original ≈0.004)
    8,  // NW  (original ≈0.008)
];

/// 8-directional neighbor offsets: (dx, dy, is_diagonal).
/// Order: N, NE, E, SE, S, SW, W, NW.
const NEIGHBORS: [(i32, i32, bool); 8] = [
    (0, -1, false), // N
    (1, -1, true),  // NE
    (1, 0, false),  // E
    (1, 1, true),   // SE
    (0, 1, false),  // S
    (-1, 1, true),  // SW
    (-1, 0, false), // W
    (-1, -1, true), // NW
];

/// Check if a cell is passable for pathfinding purposes.
///
/// For water movers (`MovementZone::Water` / `WaterBeach`), the normal PathGrid
/// marks water cells as non-walkable. Ships need to bypass PathGrid entirely and
/// use the passability matrix instead (zone 10 = water only).
///
/// For all other movers (or when `movement_zone` is `None`), uses the standard
/// `PathGrid::is_walkable()` check.
pub(crate) fn is_water_surface_cell_passable(
    cell: &ResolvedTerrainCell,
    movement_zone: MovementZone,
) -> bool {
    let matrix_ok = passability::is_passable_for_zone(cell.land_type, movement_zone);
    if matrix_ok {
        return true;
    }
    // Real RA2 maps contain shoreline/coast tiles that are still flagged as water
    // surfaces even when their TMP land_type is not the canonical water column.
    // Naval units should still treat those cells as navigable water.
    if cell.is_water {
        return true;
    }
    movement_zone == MovementZone::WaterBeach
        && cell.land_type == passability::LandType::Beach.as_index()
}

pub fn is_cell_passable_for_mover(
    grid: &PathGrid,
    x: u16,
    y: u16,
    movement_zone: Option<MovementZone>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
) -> bool {
    if let Some(mz) = movement_zone {
        if mz.is_water_mover() {
            // Water movers bypass PathGrid — use passability matrix directly.
            // cell.land_type is already the LandType column index (0-7), not
            // the raw TMP byte — do NOT re-convert with tmp_terrain_to_land_type().
            if let Some(terrain) = resolved_terrain {
                if let Some(cell) = terrain.cell(x, y) {
                    return is_water_surface_cell_passable(cell, mz);
                }
            }
            return false;
        }
    }
    grid.is_walkable(x, y)
}

/// 2D walkability grid for pathfinding.
///
/// Each cell is either walkable (true) or blocked (false).
/// Buildings, water, and other impassable terrain set cells to blocked.
#[derive(Debug, Clone)]
pub struct PathGrid {
    /// Flat array of walkability flags, indexed by `y * width + x`.
    cells: Vec<bool>,
    /// Grid width in cells.
    width: u16,
    /// Grid height in cells.
    height: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayeredPathCell {
    pub ground_walkable: bool,
    pub bridge_walkable: bool,
    pub transition: bool,
    pub ground_level: u8,
    pub bridge_deck_level: u8,
}

impl LayeredPathCell {
    pub fn is_bridge_transition_cell(&self) -> bool {
        self.transition
    }

    pub fn is_elevated_bridge_cell(&self) -> bool {
        self.bridge_deck_level_if_any()
            .is_some_and(|deck| deck > self.ground_level)
    }

    pub fn bridge_deck_level_if_any(&self) -> Option<u8> {
        self.bridge_walkable.then_some(self.bridge_deck_level)
    }

    pub fn effective_cell_z_for_layer(&self, layer: MovementLayer) -> u8 {
        match layer {
            MovementLayer::Bridge => self.bridge_deck_level_if_any().unwrap_or(self.ground_level),
            MovementLayer::Ground | MovementLayer::Air | MovementLayer::Underground => {
                self.ground_level
            }
        }
    }

    pub fn can_enter_bridge_layer_from_ground(&self) -> bool {
        self.bridge_walkable && self.is_bridge_transition_cell()
    }
}

#[derive(Debug, Clone)]
pub struct LayeredPathGrid {
    cells: Vec<LayeredPathCell>,
    width: u16,
    height: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayeredPathStep {
    pub rx: u16,
    pub ry: u16,
    pub layer: MovementLayer,
}

impl PathGrid {
    /// Create a new grid where all cells are walkable by default.
    pub fn new(width: u16, height: u16) -> Self {
        let size: usize = width as usize * height as usize;
        Self {
            cells: vec![true; size],
            width,
            height,
        }
    }

    /// Mark a cell as blocked (impassable) or unblocked (walkable).
    pub fn set_blocked(&mut self, x: u16, y: u16, blocked: bool) {
        if x < self.width && y < self.height {
            let idx: usize = y as usize * self.width as usize + x as usize;
            self.cells[idx] = !blocked;
        }
    }

    /// Check if a cell is walkable. Out-of-bounds cells are always impassable.
    pub fn is_walkable(&self, x: u16, y: u16) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        let idx: usize = y as usize * self.width as usize + x as usize;
        self.cells[idx]
    }

    /// Find the nearest walkable cell to `(x, y)`, searching in expanding rings.
    ///
    /// Returns `None` if no walkable cell exists within `max_radius` cells.
    /// If `(x, y)` is already walkable, returns it immediately.
    /// Also checks `entity_blocks` — a cell must be both PathGrid-walkable AND
    /// not entity-blocked to be returned (unless it equals `allow`, typically
    /// the goal cell which is exempt from entity blocks in A*).
    pub fn nearest_walkable(
        &self,
        x: u16,
        y: u16,
        max_radius: u16,
        entity_blocks: Option<&BTreeSet<(u16, u16)>>,
        allow: Option<(u16, u16)>,
    ) -> Option<(u16, u16)> {
        if self.is_walkable(x, y)
            && (allow == Some((x, y)) || entity_blocks.map_or(true, |b| !b.contains(&(x, y))))
        {
            return Some((x, y));
        }
        for radius in 1..=max_radius {
            let r: i32 = radius as i32;
            // Scan the perimeter of the square ring at distance `radius`.
            for d in -r..=r {
                let candidates: [(i32, i32); 4] = [
                    (x as i32 + d, y as i32 - r),
                    (x as i32 + d, y as i32 + r),
                    (x as i32 - r, y as i32 + d),
                    (x as i32 + r, y as i32 + d),
                ];
                for (cx, cy) in candidates {
                    if cx < 0 || cy < 0 || cx >= self.width as i32 || cy >= self.height as i32 {
                        continue;
                    }
                    let cx: u16 = cx as u16;
                    let cy: u16 = cy as u16;
                    if self.is_walkable(cx, cy)
                        && (allow == Some((cx, cy))
                            || entity_blocks.map_or(true, |b| !b.contains(&(cx, cy))))
                    {
                        return Some((cx, cy));
                    }
                }
            }
        }
        None
    }
}

/// Check if a cell is walkable on either the ground or bridge layer.
///
/// Used by move target validation and group destination distribution so that
/// bridge deck cells (ground-blocked water but bridge-layer walkable) are
/// accepted as valid destinations.
pub fn is_any_layer_walkable(
    grid: &PathGrid,
    layered: Option<&LayeredPathGrid>,
    x: u16,
    y: u16,
) -> bool {
    if grid.is_walkable(x, y) {
        return true;
    }
    layered.is_some_and(|lg| lg.is_walkable(x, y, MovementLayer::Bridge))
}

/// Find the nearest walkable cell on either layer, searching in expanding rings.
///
/// Same ring-scan logic as `PathGrid::nearest_walkable` but also considers
/// bridge-layer walkability via the `LayeredPathGrid`.
pub fn nearest_walkable_layered(
    grid: &PathGrid,
    layered: Option<&LayeredPathGrid>,
    x: u16,
    y: u16,
    max_radius: u16,
    entity_blocks: Option<&BTreeSet<(u16, u16)>>,
    allow: Option<(u16, u16)>,
) -> Option<(u16, u16)> {
    let check = |cx: u16, cy: u16| -> bool {
        is_any_layer_walkable(grid, layered, cx, cy)
            && (allow == Some((cx, cy)) || entity_blocks.map_or(true, |b| !b.contains(&(cx, cy))))
    };
    if check(x, y) {
        return Some((x, y));
    }
    for radius in 1..=max_radius {
        let r: i32 = radius as i32;
        for d in -r..=r {
            let candidates: [(i32, i32); 4] = [
                (x as i32 + d, y as i32 - r),
                (x as i32 + d, y as i32 + r),
                (x as i32 - r, y as i32 + d),
                (x as i32 + r, y as i32 + d),
            ];
            for (cx, cy) in candidates {
                if cx < 0 || cy < 0 || cx >= grid.width() as i32 || cy >= grid.height() as i32 {
                    continue;
                }
                if check(cx as u16, cy as u16) {
                    return Some((cx as u16, cy as u16));
                }
            }
        }
    }
    None
}

impl PathGrid {
    /// Grid width accessor.
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Grid height accessor.
    pub fn height(&self) -> u16 {
        self.height
    }

    /// Build a walkability grid from map cell data and tileset classification.
    ///
    /// Strategy: start with all cells **blocked**, then mark cells that have
    /// valid terrain data (non-water, non-cliff) as walkable. This ensures
    /// cells outside the map bounds are impassable by default.
    ///
    /// The TilesetLookup (if available) classifies tiles by their SetName
    /// from the theater INI — tilesets named "Water" or "Cliff" are impassable.
    pub fn from_map_data(
        cells: &[MapCell],
        lookup: Option<&TilesetLookup>,
        map_width: u16,
        map_height: u16,
    ) -> Self {
        // Start with all cells blocked (unlike `new()` which starts all walkable).
        let size: usize = map_width as usize * map_height as usize;
        let mut grid: PathGrid = PathGrid {
            cells: vec![false; size],
            width: map_width,
            height: map_height,
        };

        let mut walkable_count: u32 = 0;
        let mut water_count: u32 = 0;
        let mut cliff_count: u32 = 0;

        for cell in cells {
            // Skip true no-tile sentinel cells.
            // 0x0000FFFF is treated as clear ground (tile 0), not as an absent cell.
            if cell.tile_index < 0 {
                continue;
            }

            // Bounds check: cells outside the grid dimensions are ignored.
            if cell.rx >= map_width || cell.ry >= map_height {
                continue;
            }

            let tile_id: u16 = if cell.tile_index == 0xFFFF {
                0
            } else {
                cell.tile_index as u16
            };

            // Classify terrain type for this tile.
            let is_water: bool = lookup.map_or(false, |l| l.is_water(tile_id));
            let is_cliff: bool = lookup.map_or(false, |l| l.is_cliff(tile_id));

            if is_water {
                water_count += 1;
                // Water: do NOT block — per-SpeedType TerrainCostGrid handles
                // passability (Float/Hover/Amphibious cost>0, ground cost=0).
            }
            if is_cliff {
                cliff_count += 1;
                continue; // Cliffs are universally impassable.
            }

            // Mark this cell as walkable.
            let idx: usize = cell.ry as usize * map_width as usize + cell.rx as usize;
            if idx < grid.cells.len() {
                grid.cells[idx] = true;
                walkable_count += 1;
            }
        }

        log::info!(
            "PathGrid: {}x{} — {} walkable, {} water, {} cliff, {} total blocked",
            map_width,
            map_height,
            walkable_count,
            water_count,
            cliff_count,
            size as u32 - walkable_count,
        );

        grid
    }

    /// Build a walkability grid from resolved terrain metadata.
    ///
    /// Water cells are marked walkable here — SpeedType-dependent blocking is
    /// handled by `TerrainCostGrid` (cost=0 blocks ground units in A*).
    /// Only structural blocks (overlays, terrain objects) and universal blocks
    /// (cliffs) are rejected at this level.
    pub fn from_resolved_terrain(terrain: &ResolvedTerrainGrid) -> Self {
        let size: usize = terrain.width() as usize * terrain.height() as usize;
        let mut grid = PathGrid {
            cells: vec![false; size],
            width: terrain.width(),
            height: terrain.height(),
        };
        let mut walkable_count: u32 = 0;
        let mut water_count: u32 = 0;
        let mut cliff_count: u32 = 0;
        let mut overlay_block_count: u32 = 0;
        let mut object_block_count: u32 = 0;
        let mut bridge_deck_count: u32 = 0;

        for cell in terrain.iter() {
            let idx: usize = cell.ry as usize * terrain.width() as usize + cell.rx as usize;
            if idx >= grid.cells.len() {
                continue;
            }
            if cell.is_water {
                water_count += 1;
            }
            if cell.overlay_blocks {
                overlay_block_count += 1;
                continue;
            }
            if cell.terrain_object_blocks {
                object_block_count += 1;
                continue;
            }
            // Bridge decks override underlying terrain — always walkable.
            // Units walk on the bridge surface, not the cliff/water below.
            if cell.has_bridge_deck {
                bridge_deck_count += 1;
                grid.cells[idx] = true;
                walkable_count += 1;
                continue;
            }
            // Cliffs are universally impassable — block in PathGrid.
            if cell.is_cliff_like {
                cliff_count += 1;
                continue;
            }
            // Water cells: do NOT block here. The per-SpeedType TerrainCostGrid
            // handles passability (Float/Hover/Amphibious see cost>0 = passable,
            // Foot/Track/Wheel see cost=0 = blocked in A*).
            grid.cells[idx] = true;
            walkable_count += 1;
        }

        log::info!(
            "PathGrid(resolved): {}x{} — {} walkable, {} water, {} cliff, {} overlay-blocked, {} object-blocked, {} bridge-deck",
            terrain.width(),
            terrain.height(),
            walkable_count,
            water_count,
            cliff_count,
            overlay_block_count,
            object_block_count,
            bridge_deck_count,
        );

        grid
    }

    /// Compute cells that differ between two PathGrids.
    /// Returns coordinates of cells whose walkability changed.
    /// Returns `None` if grids have different dimensions (full rebuild needed).
    pub fn diff_cells(&self, other: &PathGrid) -> Option<Vec<(u16, u16)>> {
        if self.width != other.width || self.height != other.height {
            return None;
        }
        let w = self.width as usize;
        let mut changed = Vec::new();
        for (idx, (&a, &b)) in self.cells.iter().zip(other.cells.iter()).enumerate() {
            if a != b {
                changed.push(((idx % w) as u16, (idx / w) as u16));
            }
        }
        Some(changed)
    }

    /// Mark cells occupied by building footprints as blocked.
    ///
    /// Buildings occupy a rectangular footprint (e.g., "2x2", "3x3") centered
    /// at their cell position. This blocks pathfinding through structures.
    pub fn block_building_footprint(&mut self, cell_rx: u16, cell_ry: u16, foundation: &str) {
        // Parse foundation string like "2x2", "3x3", "1x1".
        let (fw, fh): (u16, u16) = parse_foundation(foundation);

        for dy in 0..fh {
            for dx in 0..fw {
                let bx: u16 = cell_rx.wrapping_add(dx);
                let by: u16 = cell_ry.wrapping_add(dy);
                self.set_blocked(bx, by, true);
            }
        }
    }
}

impl LayeredPathGrid {
    pub fn from_resolved_terrain(terrain: &ResolvedTerrainGrid) -> Self {
        Self::from_resolved_terrain_with_bridges(terrain, None)
    }

    pub fn from_resolved_terrain_with_bridges(
        terrain: &ResolvedTerrainGrid,
        bridge_state: Option<&BridgeRuntimeState>,
    ) -> Self {
        let cells = terrain
            .iter()
            .map(|cell| LayeredPathCell {
                // Bridge deck cells are NOT ground-walkable by override.
                // The layered A* routes units onto bridges via the Bridge
                // layer: Ground → bridgehead (transition) → Bridge deck →
                // bridgehead (transition) → Ground. Bridges over land have
                // naturally walkable ground below, so units can walk under.
                // The non-layered PathGrid still marks deck cells as walkable
                // for the fallback pathfinder (see PathGrid construction).
                ground_walkable: !cell.ground_walk_blocked,
                bridge_walkable: bridge_state.map_or(cell.bridge_walkable, |state| {
                    state.is_bridge_walkable(cell.rx, cell.ry)
                }),
                transition: bridge_state.map_or(cell.bridge_transition, |state| {
                    cell.bridge_transition && state.is_bridge_walkable(cell.rx, cell.ry)
                }),
                ground_level: cell.level,
                bridge_deck_level: bridge_state
                    .and_then(|state| state.cell(cell.rx, cell.ry))
                    .map(|runtime| runtime.deck_level)
                    .unwrap_or(cell.bridge_deck_level),
            })
            .collect();
        Self {
            cells,
            width: terrain.width(),
            height: terrain.height(),
        }
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    pub fn cell(&self, x: u16, y: u16) -> Option<&LayeredPathCell> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.cells
            .get(y as usize * self.width as usize + x as usize)
    }

    pub fn is_walkable(&self, x: u16, y: u16, layer: MovementLayer) -> bool {
        let Some(cell) = self.cell(x, y) else {
            return false;
        };
        match layer {
            MovementLayer::Ground => cell.ground_walkable,
            MovementLayer::Bridge => cell.bridge_walkable,
            MovementLayer::Air | MovementLayer::Underground => false,
        }
    }

    /// Whether this cell is a bridge transition point (units can switch layers here).
    pub fn is_transition(&self, x: u16, y: u16) -> bool {
        self.cell(x, y)
            .is_some_and(|c| c.is_bridge_transition_cell())
    }

    pub fn bridge_deck_level(&self, x: u16, y: u16) -> Option<u8> {
        self.cell(x, y)
            .and_then(LayeredPathCell::bridge_deck_level_if_any)
    }

    pub fn can_enter_bridge_layer_from_ground(&self, x: u16, y: u16) -> bool {
        self.cell(x, y)
            .is_some_and(LayeredPathCell::can_enter_bridge_layer_from_ground)
    }

    /// Mark ground-layer cells occupied by a building footprint as blocked.
    ///
    /// Mirrors `PathGrid::block_building_footprint` so both grids stay in sync.
    /// Buildings only block the ground layer — bridge decks above are unaffected.
    pub fn block_building_footprint(&mut self, cell_rx: u16, cell_ry: u16, foundation: &str) {
        let (fw, fh): (u16, u16) = parse_foundation(foundation);
        for dy in 0..fh {
            for dx in 0..fw {
                let bx: u16 = cell_rx.wrapping_add(dx);
                let by: u16 = cell_ry.wrapping_add(dy);
                if bx < self.width && by < self.height {
                    let idx: usize = by as usize * self.width as usize + bx as usize;
                    self.cells[idx].ground_walkable = false;
                }
            }
        }
    }

    /// Construct from raw cell data (test helper and incremental zone updates).
    #[cfg(test)]
    pub fn from_cells(cells: Vec<LayeredPathCell>, width: u16, height: u16) -> Self {
        Self {
            cells,
            width,
            height,
        }
    }
}

/// A* search node stored in the open set (priority queue).
///
/// Implements Ord so BinaryHeap<Reverse<AStarNode>> gives us a min-heap
/// ordered by f_cost (lowest cost explored first).
#[derive(Debug, Clone, Eq, PartialEq)]
struct AStarNode {
    /// Total estimated cost: g_cost + heuristic.
    f_cost: i32,
    /// Cost from start to this node (actual, not estimated).
    g_cost: i32,
    /// Cell coordinates.
    x: u16,
    y: u16,
}

impl Ord for AStarNode {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Primary: f_cost ascending (lower is better).
        // Tiebreak: higher g_cost preferred (closer to goal, more "explored").
        self.f_cost
            .cmp(&other.f_cost)
            .then_with(|| other.g_cost.cmp(&self.g_cost))
            .then_with(|| self.y.cmp(&other.y))
            .then_with(|| self.x.cmp(&other.x))
    }
}

impl PartialOrd for AStarNode {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Octile distance heuristic for 8-directional grid movement.
///
/// Consistent (never overestimates) with diagonal cost = 14, cardinal cost = 10.
/// Formula: max(dx, dy) * CARDINAL_COST + (min(dx, dy)) * (DIAGONAL_COST - CARDINAL_COST)
fn octile_heuristic(ax: u16, ay: u16, bx: u16, by: u16) -> i32 {
    let dx: i32 = (ax as i32 - bx as i32).abs();
    let dy: i32 = (ay as i32 - by as i32).abs();
    let min_d: i32 = dx.min(dy);
    let max_d: i32 = dx.max(dy);
    // max_d cardinal steps + min_d upgrades from cardinal to diagonal.
    max_d * CARDINAL_COST + min_d * (DIAGONAL_COST - CARDINAL_COST)
}

/// Find a path from start to goal using A* search.
///
/// Returns `Some(path)` where path is a sequence of (rx, ry) cells from
/// start to goal (both inclusive). Returns `None` if no path exists or
/// the search exceeds MAX_SEARCH_NODES.
///
/// The path is suitable for the movement system to follow cell-by-cell.
pub fn find_path(grid: &PathGrid, start: (u16, u16), goal: (u16, u16)) -> Option<Vec<(u16, u16)>> {
    // Early exit: start or goal is blocked.
    if !grid.is_walkable(start.0, start.1) || !grid.is_walkable(goal.0, goal.1) {
        return None;
    }

    // Trivial case: already at the goal.
    if start == goal {
        return Some(vec![start]);
    }

    let w: usize = grid.width as usize;
    let h: usize = grid.height as usize;
    let total_cells: usize = w * h;

    // g_cost[i] = best known cost from start to cell i. i32::MAX = unvisited.
    let mut g_cost: Vec<i32> = vec![i32::MAX; total_cells];
    // came_from[i] = index of the cell we reached i from. usize::MAX = no parent.
    let mut came_from: Vec<usize> = vec![usize::MAX; total_cells];
    // closed[i] = true if cell has been fully processed.
    let mut closed: Vec<bool> = vec![false; total_cells];

    let start_idx: usize = start.1 as usize * w + start.0 as usize;
    let goal_idx: usize = goal.1 as usize * w + goal.0 as usize;

    g_cost[start_idx] = 0;

    // Min-heap: Reverse wrapping makes BinaryHeap pop smallest f_cost first.
    let mut open: BinaryHeap<Reverse<AStarNode>> = BinaryHeap::new();
    open.push(Reverse(AStarNode {
        f_cost: octile_heuristic(start.0, start.1, goal.0, goal.1),
        g_cost: 0,
        x: start.0,
        y: start.1,
    }));

    let mut nodes_evaluated: u32 = 0;

    while let Some(Reverse(current)) = open.pop() {
        let cx: u16 = current.x;
        let cy: u16 = current.y;
        let c_idx: usize = cy as usize * w + cx as usize;

        // Skip if already processed (duplicate entry in the heap).
        if closed[c_idx] {
            continue;
        }
        closed[c_idx] = true;

        // Found the goal — reconstruct and return the path.
        if c_idx == goal_idx {
            return Some(reconstruct_path(&came_from, start_idx, goal_idx, w));
        }

        nodes_evaluated += 1;
        if nodes_evaluated >= MAX_SEARCH_NODES {
            log::warn!(
                "A* search exhausted {} nodes without finding path from ({},{}) to ({},{})",
                MAX_SEARCH_NODES,
                start.0,
                start.1,
                goal.0,
                goal.1
            );
            return None;
        }

        // Explore all 8 neighbors.
        for &(dx, dy, is_diagonal) in &NEIGHBORS {
            let nx: i32 = cx as i32 + dx;
            let ny: i32 = cy as i32 + dy;

            // Bounds check (negative values caught by the u16 conversion below).
            if nx < 0 || ny < 0 || nx >= grid.width as i32 || ny >= grid.height as i32 {
                continue;
            }

            let nx: u16 = nx as u16;
            let ny: u16 = ny as u16;
            let n_idx: usize = ny as usize * w + nx as usize;

            if closed[n_idx] || !grid.is_walkable(nx, ny) {
                continue;
            }

            // For diagonal moves, ensure both adjacent cardinal cells are walkable.
            // This prevents cutting through diagonal wall corners.
            // Reuse bounds-checked nx/ny: (cx+dx, cy) = (nx, cy), (cx, cy+dy) = (cx, ny).
            if is_diagonal {
                if !grid.is_walkable(nx, cy) || !grid.is_walkable(cx, ny) {
                    continue;
                }
            }

            let step_cost: i32 = if is_diagonal {
                DIAGONAL_COST
            } else {
                CARDINAL_COST
            };
            let tentative_g: i32 = current.g_cost + step_cost;

            if tentative_g < g_cost[n_idx] {
                g_cost[n_idx] = tentative_g;
                came_from[n_idx] = c_idx;
                let h: i32 = octile_heuristic(nx, ny, goal.0, goal.1);
                open.push(Reverse(AStarNode {
                    f_cost: tentative_g + h,
                    g_cost: tentative_g,
                    x: nx,
                    y: ny,
                }));
            }
        }
    }

    // Open set exhausted — no path exists.
    None
}

/// A* pathfinding with optional terrain cost modifiers and entity blocking.
///
/// When `costs` is `Some`, step cost is scaled by `100 / cost_at(x,y)` so that
/// slower terrain (cost < 100) increases path cost and faster terrain (cost > 100)
/// decreases it. Cells with cost 0 are treated as blocked.
///
/// When `entity_blocks` is `Some`, cells in the set are treated as blocked UNLESS
/// they are the goal cell (units must be able to path to an occupied destination).
/// This implements RA2's "friendly moving units as passable" optimization: the caller
/// builds the set from stationary/enemy entities only, excluding moving friendlies.
///
/// When `costs` is `None`, behaves identically to `find_path` (plus entity blocking).
pub fn find_path_with_costs(
    grid: &PathGrid,
    start: (u16, u16),
    goal: (u16, u16),
    costs: Option<&TerrainCostGrid>,
    entity_blocks: Option<&BTreeSet<(u16, u16)>>,
    movement_zone: Option<MovementZone>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
) -> Option<Vec<(u16, u16)>> {
    // Delegate to base find_path when no cost grid and no entity blocks.
    if entity_blocks.is_none() || entity_blocks.is_some_and(|s| s.is_empty()) {
        let Some(cost_grid) = costs else {
            // No cost grid — PathGrid-only A*. This path is only safe for Winged
            // units (air movement) since water cells are PathGrid-walkable.
            log::debug!(
                "find_path_with_costs: no cost grid, using PathGrid-only A* ({:?}→{:?})",
                start,
                goal,
            );
            return find_path(grid, start, goal);
        };
        return find_path_with_costs_inner(
            grid,
            start,
            goal,
            cost_grid,
            None,
            movement_zone,
            resolved_terrain,
        );
    }
    let Some(cost_grid) = costs else {
        // Entity blocks but no cost grid — PathGrid-only A* with entity blocking.
        // Only safe for Winged units since water is PathGrid-walkable.
        log::debug!(
            "find_path_with_costs: no cost grid with entity blocks, using PathGrid-only A* ({:?}→{:?})",
            start,
            goal,
        );
        return find_path_with_entity_blocks(grid, start, goal, entity_blocks);
    };
    find_path_with_costs_inner(
        grid,
        start,
        goal,
        cost_grid,
        entity_blocks,
        movement_zone,
        resolved_terrain,
    )
}

/// Corridor-restricted A*: only expands cells whose zone ID is in `allowed_zones`.
/// Used by the hierarchical zone search to confine A* to a Dijkstra-computed corridor.
pub fn find_path_with_costs_corridor(
    grid: &PathGrid,
    start: (u16, u16),
    goal: (u16, u16),
    costs: Option<&TerrainCostGrid>,
    entity_blocks: Option<&BTreeSet<(u16, u16)>>,
    zone_map: &super::zone_map::ZoneMap,
    allowed_zones: &BTreeSet<super::zone_map::ZoneId>,
    movement_zone: Option<MovementZone>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
) -> Option<Vec<(u16, u16)>> {
    let cost_grid = costs?;
    find_path_with_costs_corridor_inner(
        grid,
        start,
        goal,
        cost_grid,
        entity_blocks,
        zone_map,
        allowed_zones,
        movement_zone,
        resolved_terrain,
    )
}

/// A* restricted to cells in `allowed_zones`. Identical to `find_path_with_costs_inner`
/// except cells outside the corridor are skipped during neighbor expansion.
fn find_path_with_costs_corridor_inner(
    grid: &PathGrid,
    start: (u16, u16),
    goal: (u16, u16),
    cost_grid: &TerrainCostGrid,
    entity_blocks: Option<&BTreeSet<(u16, u16)>>,
    zone_map: &super::zone_map::ZoneMap,
    allowed_zones: &BTreeSet<super::zone_map::ZoneId>,
    movement_zone: Option<MovementZone>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
) -> Option<Vec<(u16, u16)>> {
    let is_water_mover = movement_zone.is_some_and(|mz| mz.is_water_mover());
    if !is_cell_passable_for_mover(grid, start.0, start.1, movement_zone, resolved_terrain)
        || !is_cell_passable_for_mover(grid, goal.0, goal.1, movement_zone, resolved_terrain)
    {
        return None;
    }
    if start == goal {
        return Some(vec![start]);
    }

    let w: usize = grid.width as usize;
    let h: usize = grid.height as usize;
    let total_cells: usize = w * h;

    let mut g_cost: Vec<i32> = vec![i32::MAX; total_cells];
    let mut came_from: Vec<usize> = vec![usize::MAX; total_cells];
    let mut closed: Vec<bool> = vec![false; total_cells];

    let start_idx: usize = start.1 as usize * w + start.0 as usize;
    let goal_idx: usize = goal.1 as usize * w + goal.0 as usize;

    g_cost[start_idx] = 0;

    let mut open: BinaryHeap<Reverse<AStarNode>> = BinaryHeap::new();
    open.push(Reverse(AStarNode {
        f_cost: octile_heuristic(start.0, start.1, goal.0, goal.1),
        g_cost: 0,
        x: start.0,
        y: start.1,
    }));

    let mut nodes_evaluated: u32 = 0;

    while let Some(Reverse(current)) = open.pop() {
        let cx: u16 = current.x;
        let cy: u16 = current.y;
        let c_idx: usize = cy as usize * w + cx as usize;

        if closed[c_idx] {
            continue;
        }
        closed[c_idx] = true;

        if c_idx == goal_idx {
            return Some(reconstruct_path(&came_from, start_idx, goal_idx, w));
        }

        nodes_evaluated += 1;
        if nodes_evaluated >= MAX_SEARCH_NODES {
            return None;
        }

        for (dir_index, &(dx, dy, is_diagonal)) in NEIGHBORS.iter().enumerate() {
            let nx_i: i32 = cx as i32 + dx;
            let ny_i: i32 = cy as i32 + dy;

            if nx_i < 0 || ny_i < 0 || nx_i >= grid.width as i32 || ny_i >= grid.height as i32 {
                continue;
            }

            let nx: u16 = nx_i as u16;
            let ny: u16 = ny_i as u16;
            let n_idx: usize = ny as usize * w + nx as usize;

            if closed[n_idx]
                || !is_cell_passable_for_mover(grid, nx, ny, movement_zone, resolved_terrain)
            {
                continue;
            }

            // Corridor filter: skip cells outside the allowed zone set.
            let cell_zone = zone_map.zone_at(
                nx,
                ny,
                crate::sim::movement::locomotor::MovementLayer::Ground,
            );
            if cell_zone != super::zone_map::ZONE_INVALID && !allowed_zones.contains(&cell_zone) {
                continue;
            }

            if (nx, ny) != goal {
                if let Some(blocks) = entity_blocks {
                    if blocks.contains(&(nx, ny)) {
                        continue;
                    }
                }
            }

            let terrain_cost: u8 = if is_water_mover {
                100
            } else {
                cost_grid.cost_at(nx, ny)
            };
            if terrain_cost == 0 {
                continue;
            }

            if is_diagonal {
                let adj_ok_1: bool =
                    is_cell_passable_for_mover(grid, nx, cy, movement_zone, resolved_terrain)
                        && (is_water_mover || cost_grid.cost_at(nx, cy) > 0);
                let adj_ok_2: bool =
                    is_cell_passable_for_mover(grid, cx, ny, movement_zone, resolved_terrain)
                        && (is_water_mover || cost_grid.cost_at(cx, ny) > 0);
                if !adj_ok_1 || !adj_ok_2 {
                    continue;
                }
            }

            let base_cost: i32 = if is_diagonal {
                DIAGONAL_COST
            } else {
                CARDINAL_COST
            };
            let mut step_cost: i32 = base_cost * 100 / terrain_cost as i32;

            // Cliff ramp cost: height transitions cost 4× more.
            if let Some(rt) = resolved_terrain {
                if let (Some(cur), Some(nxt)) = (rt.cell(cx, cy), rt.cell(nx, ny)) {
                    if cur.level != nxt.level {
                        step_cost *= CLIFF_COST_MULTIPLIER;
                    }
                }
            }

            let tentative_g: i32 = current.g_cost + step_cost + DIR_TIEBREAK[dir_index];

            if tentative_g < g_cost[n_idx] {
                g_cost[n_idx] = tentative_g;
                came_from[n_idx] = c_idx;
                let h: i32 = octile_heuristic(nx, ny, goal.0, goal.1);
                open.push(Reverse(AStarNode {
                    f_cost: tentative_g + h,
                    g_cost: tentative_g,
                    x: nx,
                    y: ny,
                }));
            }
        }
    }
    None
}

/// A* with cost grid and optional entity blocking. Core implementation.
fn find_path_with_costs_inner(
    grid: &PathGrid,
    start: (u16, u16),
    goal: (u16, u16),
    cost_grid: &TerrainCostGrid,
    entity_blocks: Option<&BTreeSet<(u16, u16)>>,
    movement_zone: Option<MovementZone>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
) -> Option<Vec<(u16, u16)>> {
    let is_water_mover = movement_zone.is_some_and(|mz| mz.is_water_mover());
    if !is_cell_passable_for_mover(grid, start.0, start.1, movement_zone, resolved_terrain)
        || !is_cell_passable_for_mover(grid, goal.0, goal.1, movement_zone, resolved_terrain)
    {
        return None;
    }
    if start == goal {
        return Some(vec![start]);
    }

    let w: usize = grid.width as usize;
    let h: usize = grid.height as usize;
    let total_cells: usize = w * h;

    let mut g_cost: Vec<i32> = vec![i32::MAX; total_cells];
    let mut came_from: Vec<usize> = vec![usize::MAX; total_cells];
    let mut closed: Vec<bool> = vec![false; total_cells];

    let start_idx: usize = start.1 as usize * w + start.0 as usize;
    let goal_idx: usize = goal.1 as usize * w + goal.0 as usize;

    g_cost[start_idx] = 0;

    let mut open: BinaryHeap<Reverse<AStarNode>> = BinaryHeap::new();
    open.push(Reverse(AStarNode {
        f_cost: octile_heuristic(start.0, start.1, goal.0, goal.1),
        g_cost: 0,
        x: start.0,
        y: start.1,
    }));

    let mut nodes_evaluated: u32 = 0;

    while let Some(Reverse(current)) = open.pop() {
        let cx: u16 = current.x;
        let cy: u16 = current.y;
        let c_idx: usize = cy as usize * w + cx as usize;

        if closed[c_idx] {
            continue;
        }
        closed[c_idx] = true;

        if c_idx == goal_idx {
            return Some(reconstruct_path(&came_from, start_idx, goal_idx, w));
        }

        nodes_evaluated += 1;
        if nodes_evaluated >= MAX_SEARCH_NODES {
            return None;
        }

        for (dir_index, &(dx, dy, is_diagonal)) in NEIGHBORS.iter().enumerate() {
            let nx_i: i32 = cx as i32 + dx;
            let ny_i: i32 = cy as i32 + dy;

            if nx_i < 0 || ny_i < 0 || nx_i >= grid.width as i32 || ny_i >= grid.height as i32 {
                continue;
            }

            let nx: u16 = nx_i as u16;
            let ny: u16 = ny_i as u16;
            let n_idx: usize = ny as usize * w + nx as usize;

            if closed[n_idx]
                || !is_cell_passable_for_mover(grid, nx, ny, movement_zone, resolved_terrain)
            {
                continue;
            }

            // Entity-blocked cell — skip unless it's the goal (must be reachable).
            if (nx, ny) != goal {
                if let Some(blocks) = entity_blocks {
                    if blocks.contains(&(nx, ny)) {
                        continue;
                    }
                }
            }

            // Check terrain cost — 0 means blocked for this SpeedType.
            let terrain_cost: u8 = if is_water_mover {
                100
            } else {
                cost_grid.cost_at(nx, ny)
            };
            if terrain_cost == 0 {
                continue;
            }

            // Reuse bounds-checked nx/ny for diagonal corner-cutting.
            if is_diagonal {
                // Both cardinal neighbors must be passable for this movement profile.
                let adj_ok_1: bool =
                    is_cell_passable_for_mover(grid, nx, cy, movement_zone, resolved_terrain)
                        && (is_water_mover || cost_grid.cost_at(nx, cy) > 0);
                let adj_ok_2: bool =
                    is_cell_passable_for_mover(grid, cx, ny, movement_zone, resolved_terrain)
                        && (is_water_mover || cost_grid.cost_at(cx, ny) > 0);
                if !adj_ok_1 || !adj_ok_2 {
                    continue;
                }
            }

            // Weighted step cost — higher terrain_cost (faster terrain) = cheaper A* step.
            let base_cost: i32 = if is_diagonal {
                DIAGONAL_COST
            } else {
                CARDINAL_COST
            };
            let mut step_cost: i32 = base_cost * 100 / terrain_cost as i32;

            // Cliff ramp cost: cells with height transitions cost 4× more.
            // Original engine checks cell.Flags & 0x40000 and multiplies by 4.0.
            if let Some(rt) = resolved_terrain {
                if let (Some(cur), Some(nxt)) = (rt.cell(cx, cy), rt.cell(nx, ny)) {
                    if cur.level != nxt.level {
                        step_cost *= CLIFF_COST_MULTIPLIER;
                    }
                }
            }

            // Direction tie-breaker: small per-direction offset prevents path
            // oscillation when multiple routes have equal cost. Cardinals get
            // lower values than diagonals, matching original engine (0x0081872c).
            let tentative_g: i32 = current.g_cost + step_cost + DIR_TIEBREAK[dir_index];

            if tentative_g < g_cost[n_idx] {
                g_cost[n_idx] = tentative_g;
                came_from[n_idx] = c_idx;
                let h: i32 = octile_heuristic(nx, ny, goal.0, goal.1);
                open.push(Reverse(AStarNode {
                    f_cost: tentative_g + h,
                    g_cost: tentative_g,
                    x: nx,
                    y: ny,
                }));
            }
        }
    }
    None
}

/// A* with entity blocking but no terrain cost grid (uniform step costs).
fn find_path_with_entity_blocks(
    grid: &PathGrid,
    start: (u16, u16),
    goal: (u16, u16),
    entity_blocks: Option<&BTreeSet<(u16, u16)>>,
) -> Option<Vec<(u16, u16)>> {
    if !grid.is_walkable(start.0, start.1) || !grid.is_walkable(goal.0, goal.1) {
        return None;
    }
    if start == goal {
        return Some(vec![start]);
    }

    let w: usize = grid.width as usize;
    let h: usize = grid.height as usize;
    let total_cells: usize = w * h;

    let mut g_cost: Vec<i32> = vec![i32::MAX; total_cells];
    let mut came_from: Vec<usize> = vec![usize::MAX; total_cells];
    let mut closed: Vec<bool> = vec![false; total_cells];

    let start_idx: usize = start.1 as usize * w + start.0 as usize;
    let goal_idx: usize = goal.1 as usize * w + goal.0 as usize;

    g_cost[start_idx] = 0;

    let mut open: BinaryHeap<Reverse<AStarNode>> = BinaryHeap::new();
    open.push(Reverse(AStarNode {
        f_cost: octile_heuristic(start.0, start.1, goal.0, goal.1),
        g_cost: 0,
        x: start.0,
        y: start.1,
    }));

    let mut nodes_evaluated: u32 = 0;

    while let Some(Reverse(current)) = open.pop() {
        let cx: u16 = current.x;
        let cy: u16 = current.y;
        let c_idx: usize = cy as usize * w + cx as usize;

        if closed[c_idx] {
            continue;
        }
        closed[c_idx] = true;

        if c_idx == goal_idx {
            return Some(reconstruct_path(&came_from, start_idx, goal_idx, w));
        }

        nodes_evaluated += 1;
        if nodes_evaluated >= MAX_SEARCH_NODES {
            return None;
        }

        for &(dx, dy, is_diagonal) in &NEIGHBORS {
            let nx: i32 = cx as i32 + dx;
            let ny: i32 = cy as i32 + dy;

            if nx < 0 || ny < 0 || nx >= grid.width as i32 || ny >= grid.height as i32 {
                continue;
            }

            let nx: u16 = nx as u16;
            let ny: u16 = ny as u16;
            let n_idx: usize = ny as usize * w + nx as usize;

            if closed[n_idx] || !grid.is_walkable(nx, ny) {
                continue;
            }

            // Entity-blocked cell — skip unless it's the goal.
            if (nx, ny) != goal {
                if let Some(blocks) = entity_blocks {
                    if blocks.contains(&(nx, ny)) {
                        continue;
                    }
                }
            }

            // Reuse bounds-checked nx/ny for diagonal corner-cutting.
            if is_diagonal {
                if !grid.is_walkable(nx, cy) || !grid.is_walkable(cx, ny) {
                    continue;
                }
            }

            let step_cost: i32 = if is_diagonal {
                DIAGONAL_COST
            } else {
                CARDINAL_COST
            };
            let tentative_g: i32 = current.g_cost + step_cost;

            if tentative_g < g_cost[n_idx] {
                g_cost[n_idx] = tentative_g;
                came_from[n_idx] = c_idx;
                let h: i32 = octile_heuristic(nx, ny, goal.0, goal.1);
                open.push(Reverse(AStarNode {
                    f_cost: tentative_g + h,
                    g_cost: tentative_g,
                    x: nx,
                    y: ny,
                }));
            }
        }
    }
    None
}

/// Parse a foundation string like "2x2" or "3x3" into (width, height).
/// Returns (1, 1) for malformed or missing input.
fn parse_foundation(foundation: &str) -> (u16, u16) {
    let parts: Vec<&str> = foundation.split('x').collect();
    if parts.len() == 2 {
        let w: u16 = parts[0].parse().unwrap_or(1);
        let h: u16 = parts[1].parse().unwrap_or(1);
        (w.max(1), h.max(1))
    } else {
        (1, 1)
    }
}

/// Truncate a path to at most `max_steps` movement steps.
///
/// The path includes the start cell at index 0, so a path with `max_steps`
/// movement steps has `max_steps + 1` entries. If the path is already short
/// enough, it is returned unchanged.
pub fn truncate_path(path: Vec<(u16, u16)>, max_steps: usize) -> Vec<(u16, u16)> {
    let max_len: usize = max_steps + 1; // +1 for start cell at index 0
    if path.len() <= max_len {
        path
    } else {
        path[..max_len].to_vec()
    }
}

/// Truncate a layered path (coords + layers) to at most `max_steps` movement steps.
pub fn truncate_layered_path(
    path: Vec<(u16, u16)>,
    layers: Vec<MovementLayer>,
    max_steps: usize,
) -> (Vec<(u16, u16)>, Vec<MovementLayer>) {
    debug_assert_eq!(
        path.len(),
        layers.len(),
        "truncate_layered_path: input length mismatch: {} vs {}",
        path.len(),
        layers.len()
    );
    let max_len: usize = max_steps + 1;
    if path.len() <= max_len {
        (path, layers)
    } else {
        (path[..max_len].to_vec(), layers[..max_len].to_vec())
    }
}

/// Reconstruct the path from goal back to start by following came_from links.
fn reconstruct_path(
    came_from: &[usize],
    start_idx: usize,
    goal_idx: usize,
    width: usize,
) -> Vec<(u16, u16)> {
    let mut path: Vec<(u16, u16)> = Vec::new();
    let mut current: usize = goal_idx;

    // Walk backwards from goal to start.
    while current != start_idx {
        let x: u16 = (current % width) as u16;
        let y: u16 = (current / width) as u16;
        path.push((x, y));
        current = came_from[current];
    }

    // Add the start cell.
    let sx: u16 = (start_idx % width) as u16;
    let sy: u16 = (start_idx / width) as u16;
    path.push((sx, sy));

    // Reverse so path goes from start to goal.
    path.reverse();
    path
}

fn layered_state_index(width: usize, x: u16, y: u16, layer: MovementLayer) -> usize {
    let cell_index = y as usize * width + x as usize;
    match layer {
        MovementLayer::Ground => cell_index * 2,
        MovementLayer::Bridge => cell_index * 2 + 1,
        MovementLayer::Air | MovementLayer::Underground => cell_index * 2,
    }
}

fn goal_layers_for(
    grid: &LayeredPathGrid,
    start_layer: MovementLayer,
    goal: (u16, u16),
) -> Vec<MovementLayer> {
    let ground = grid.is_walkable(goal.0, goal.1, MovementLayer::Ground);
    let bridge = grid.is_walkable(goal.0, goal.1, MovementLayer::Bridge);
    match (ground, bridge, start_layer) {
        (true, true, MovementLayer::Bridge) => vec![MovementLayer::Bridge, MovementLayer::Ground],
        (true, true, _) => vec![MovementLayer::Ground, MovementLayer::Bridge],
        (true, false, _) => vec![MovementLayer::Ground],
        (false, true, _) => vec![MovementLayer::Bridge],
        (false, false, _) => Vec::new(),
    }
}

fn reconstruct_layered_path(
    came_from: &[usize],
    start_idx: usize,
    goal_idx: usize,
    width: usize,
) -> Vec<LayeredPathStep> {
    let mut path: Vec<LayeredPathStep> = Vec::new();
    let mut current = goal_idx;
    while current != start_idx {
        let cell = current / 2;
        let x = (cell % width) as u16;
        let y = (cell / width) as u16;
        let layer = if current.is_multiple_of(2) {
            MovementLayer::Ground
        } else {
            MovementLayer::Bridge
        };
        path.push(LayeredPathStep {
            rx: x,
            ry: y,
            layer,
        });
        current = came_from[current];
    }
    let start_cell = start_idx / 2;
    path.push(LayeredPathStep {
        rx: (start_cell % width) as u16,
        ry: (start_cell / width) as u16,
        layer: if start_idx.is_multiple_of(2) {
            MovementLayer::Ground
        } else {
            MovementLayer::Bridge
        },
    });
    path.reverse();
    path
}

pub fn find_layered_path(
    grid: &LayeredPathGrid,
    path_grid: Option<&PathGrid>,
    ground_blocks: Option<&BTreeSet<(u16, u16)>>,
    bridge_blocks: Option<&BTreeSet<(u16, u16)>>,
    start: (u16, u16),
    start_layer: MovementLayer,
    goal: (u16, u16),
    terrain_costs: Option<&TerrainCostGrid>,
) -> Option<Vec<LayeredPathStep>> {
    if !matches!(start_layer, MovementLayer::Ground | MovementLayer::Bridge) {
        return None;
    }
    let start_layer = if grid.is_walkable(start.0, start.1, start_layer) {
        start_layer
    } else if start_layer == MovementLayer::Bridge
        && grid.is_walkable(start.0, start.1, MovementLayer::Ground)
    {
        MovementLayer::Ground
    } else if start_layer == MovementLayer::Ground
        && grid.is_walkable(start.0, start.1, MovementLayer::Bridge)
    {
        MovementLayer::Bridge
    } else {
        return None;
    };
    let goal_layers = goal_layers_for(grid, start_layer, goal);
    if goal_layers.is_empty() {
        return None;
    }
    if start == goal && goal_layers.contains(&start_layer) {
        return Some(vec![LayeredPathStep {
            rx: start.0,
            ry: start.1,
            layer: start_layer,
        }]);
    }

    let w = grid.width as usize;
    let h = grid.height as usize;
    let total_states = w * h * 2;
    let mut g_cost = vec![i32::MAX; total_states];
    let mut came_from = vec![usize::MAX; total_states];
    let mut closed = vec![false; total_states];

    let start_idx = layered_state_index(w, start.0, start.1, start_layer);
    g_cost[start_idx] = 0;
    let mut open_layers: BinaryHeap<Reverse<(i32, usize)>> = BinaryHeap::new();
    open_layers.push(Reverse((0, start_idx)));
    let mut nodes_evaluated = 0u32;

    while let Some(Reverse((_priority, state_idx))) = open_layers.pop() {
        if closed[state_idx] {
            continue;
        }
        let cell = state_idx / 2;
        let cx = (cell % w) as u16;
        let cy = (cell / w) as u16;
        let layer = if state_idx % 2 == 0 {
            MovementLayer::Ground
        } else {
            MovementLayer::Bridge
        };
        closed[state_idx] = true;
        if (cx, cy) == goal && goal_layers.contains(&layer) {
            return Some(reconstruct_layered_path(
                &came_from, start_idx, state_idx, w,
            ));
        }

        nodes_evaluated += 1;
        if nodes_evaluated >= MAX_SEARCH_NODES {
            return None;
        }

        if grid.cell(cx, cy).is_some_and(|cell| cell.transition) {
            let other = if layer == MovementLayer::Ground {
                MovementLayer::Bridge
            } else {
                MovementLayer::Ground
            };
            if grid.is_walkable(cx, cy, other) {
                let other_idx = layered_state_index(w, cx, cy, other);
                let tentative_g = g_cost[state_idx] + CARDINAL_COST;
                if tentative_g < g_cost[other_idx] {
                    g_cost[other_idx] = tentative_g;
                    came_from[other_idx] = state_idx;
                    let h_cost = octile_heuristic(cx, cy, goal.0, goal.1);
                    open_layers.push(Reverse((tentative_g + h_cost, other_idx)));
                }
            }
        }

        for &(dx, dy, is_diagonal) in &NEIGHBORS {
            let nx_i = cx as i32 + dx;
            let ny_i = cy as i32 + dy;
            if nx_i < 0 || ny_i < 0 || nx_i >= grid.width as i32 || ny_i >= grid.height as i32 {
                continue;
            }
            let nx = nx_i as u16;
            let ny = ny_i as u16;
            if !grid.is_walkable(nx, ny, layer) {
                continue;
            }
            // Ground-layer steps must also pass PathGrid (building footprints).
            // Bridge-layer steps skip this — buildings can't be on bridges.
            if layer == MovementLayer::Ground {
                if let Some(pg) = path_grid {
                    if !pg.is_walkable(nx, ny) {
                        continue;
                    }
                }
            }
            // Entity blocking — layer-separated so bridge units don't block
            // ground pathfinding and vice versa.
            let blocks = match layer {
                MovementLayer::Ground => ground_blocks,
                MovementLayer::Bridge => bridge_blocks,
                _ => None,
            };
            if let Some(b) = blocks {
                if b.contains(&(nx, ny)) && (nx, ny) != goal {
                    continue;
                }
            }
            // Reuse bounds-checked nx/ny: (cx+dx, cy) = (nx, cy), (cx, cy+dy) = (cx, ny).
            if is_diagonal {
                if !grid.is_walkable(nx, cy, layer)
                    || !grid.is_walkable(cx, ny, layer)
                {
                    continue;
                }
                // Also check PathGrid for diagonal corner cells on ground layer.
                if layer == MovementLayer::Ground {
                    if let Some(pg) = path_grid {
                        if !pg.is_walkable(nx, cy) || !pg.is_walkable(cx, ny) {
                            continue;
                        }
                    }
                    // Diagonal corners must also be passable for this SpeedType.
                    if let Some(tc) = terrain_costs {
                        if tc.cost_at(nx, cy) == 0 || tc.cost_at(cx, ny) == 0 {
                            continue;
                        }
                    }
                }
            }
            // Terrain cost check — 0 means blocked for this SpeedType (ground layer only).
            let terrain_cost: u8 = if layer == MovementLayer::Ground {
                terrain_costs.map_or(100, |tc| tc.cost_at(nx, ny))
            } else {
                100 // bridge layer: no terrain cost modifiers
            };
            if terrain_cost == 0 {
                continue;
            }
            let next_idx = layered_state_index(w, nx, ny, layer);
            if closed[next_idx] {
                continue;
            }
            let base_cost = if is_diagonal {
                DIAGONAL_COST
            } else {
                CARDINAL_COST
            };
            // Weighted step cost — higher terrain_cost = cheaper step.
            // Roads (250) preferred, clear (100) normal, rough (75) avoided.
            let mut step_cost = base_cost * 100 / terrain_cost as i32;
            // Height-based cost penalty: the original engine gives cliff/ramp
            // cells a 4x base cost. Units prefer flat routes
            // over climbing hills when both paths lead to the same destination.
            if layer == MovementLayer::Ground {
                if let (Some(cur_cell), Some(next_cell)) = (grid.cell(cx, cy), grid.cell(nx, ny)) {
                    let h_diff =
                        (cur_cell.ground_level as i32 - next_cell.ground_level as i32).abs();
                    if h_diff > 0 {
                        step_cost *= CLIFF_COST_MULTIPLIER;
                    }
                }
            }
            let tentative_g = g_cost[state_idx] + step_cost;
            if tentative_g < g_cost[next_idx] {
                g_cost[next_idx] = tentative_g;
                came_from[next_idx] = state_idx;
                let h_cost = octile_heuristic(nx, ny, goal.0, goal.1);
                open_layers.push(Reverse((tentative_g + h_cost, next_idx)));
            }
        }
    }

    None
}

// Tests extracted to core_tests.rs to stay under 400 lines.
#[cfg(test)]
#[path = "core_tests.rs"]
mod core_tests;
