//! A* pathfinding on the isometric grid.
//!
//! PathGrid stores per-cell walkability and bridge metadata (ground_walkable,
//! bridge_walkable, transition, height levels). Flat A* reads ground_walkable;
//! layered A* reads both layers for bridge-aware routing.
//!
//! TODO(RE): The stock neighbor predicate is richer than the grid-level checks in this
//! module. The RE corpus has closed the existence and numeric shape of the cost/legality
//! classes, but not yet enough of the surrounding runtime state to replace these local
//! passability/cost shortcuts end-to-end.
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
use std::collections::{BTreeSet, BinaryHeap, HashMap};

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
/// With CARDINAL_COST=10, a height step costs 40 instead of 10.
const CLIFF_COST_MULTIPLIER: i32 = 4;

/// Code-2 (friendly moving) cost multipliers. Matches gamemd.exe
/// AStar_compute_edge_cost (0x00429830). See `compute_code2_multiplier`.
const CODE2_MULT_CLEARING: i32 = 1; // chain clears within 10 hops → baseline
const CODE2_MULT_JAM: i32 = 4; // urgency=1 OR full 10-step jam → traffic penalty
const CODE2_MULT_ROUTE_AROUND: i32 = 1000; // urgency=2 → route around blocker

/// Maximum hops in the code-2 blocker chain walk (urgency=0).
/// Matches the binary's `for (i = 0; i < 10; i++)` at 0x00429830.
const CODE2_CHAIN_MAX_HOPS: usize = 10;

/// Per-direction tie-breaker offsets added to g-cost.
/// Original engine adds tiny floats (0.001–0.008) from table at 0x0081872c.
/// We scale by 10000 to stay in integer math: cardinals get lower values than
/// diagonals, preventing path oscillation when multiple routes have equal cost.
/// Index order matches NEIGHBORS: N, NE, E, SE, S, SW, W, NW.
const DIR_TIEBREAK: [i32; 8] = [
    1, // N   (original ≈0.001)
    5, // NE  (original ≈0.005)
    2, // E   (original ≈0.002)
    6, // SE  (original ≈0.006)
    3, // S   (original ≈0.003)
    7, // SW  (original ≈0.007)
    4, // W   (original ≈0.004)
    8, // NW  (original ≈0.008)
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

/// Threshold for ground vs bridge closed-list selection.
/// Binary: abs(path_height - cell.height_level) < 2 at 0x00429e7d.
const BRIDGE_HEIGHT_THRESHOLD: u8 = 2;

/// Encode source cell index + bridge flag into came_from value.
/// Max map = 512x512 = 262,144 cells -> fits in 18 bits, leaving bit 20 free.
const CAME_FROM_BRIDGE: usize = 1 << 20;

/// Determine whether a node at `path_height` should use the bridge closed list
/// for a given neighbor cell. Uses the CURRENT node's height (not computed
/// neighbor height). Matches binary inline check at 0x00429e54.
fn is_at_bridge_level(path_height: u8, cell: &PathCell) -> bool {
    cell.bridge_walkable && path_height.abs_diff(cell.ground_level) >= BRIDGE_HEIGHT_THRESHOLD
}

/// Compute what height a new A* node carries forward when expanding into
/// `neighbor_cell` from a parent at `parent_height` in `parent_cell`.
/// Matches AStar_create_node (0x0042a460) 4-case decision tree.
fn compute_neighbor_height(
    parent_height: u8,
    parent_cell: &PathCell,
    neighbor_cell: &PathCell,
) -> u8 {
    // Case 1: Neighbor is not a bridge cell -> ground level
    if !neighbor_cell.bridge_walkable {
        return neighbor_cell.ground_level;
    }

    // Case 2: Parent is also a bridge cell
    if parent_cell.bridge_walkable {
        if parent_height == parent_cell.bridge_deck_level {
            // Parent was on bridge deck -> stay on bridge
            return neighbor_cell.bridge_deck_level;
        } else {
            // Parent was under bridge -> stay under
            return neighbor_cell.ground_level;
        }
    }

    // Case 3: Parent is NOT bridge, neighbor IS bridge.
    // Ramp-up restricted to diff in [2, 4].
    let diff = parent_height as i16 - neighbor_cell.ground_level as i16;
    if (2..=4).contains(&diff) {
        neighbor_cell.bridge_deck_level
    } else {
        neighbor_cell.ground_level
    }
}

fn encode_from(cell_idx: usize, on_bridge: bool) -> usize {
    cell_idx | if on_bridge { CAME_FROM_BRIDGE } else { 0 }
}

fn decode_from(value: usize) -> (usize, bool) {
    (value & !CAME_FROM_BRIDGE, value & CAME_FROM_BRIDGE != 0)
}

/// Configuration for the unified A* search. All fields optional; defaults
/// produce a bare ground-only search equivalent to the old `find_path`.
#[derive(Default)]
pub struct AStarOptions<'a> {
    /// Terrain speed multipliers (cost 0 = blocked for this SpeedType).
    pub terrain_costs: Option<&'a TerrainCostGrid>,
    /// Hard-blocked cells on ground layer (stationary/enemy units). Goal exempt.
    pub entity_blocks: Option<&'a BTreeSet<(u16, u16)>>,
    /// Hard-blocked cells on bridge layer. Goal exempt.
    pub bridge_blocks: Option<&'a BTreeSet<(u16, u16)>>,
    /// Code-2 blocker map: friendly-moving blocker's current cell → that
    /// blocker's next cell (movement_target.path[next_index]). Used by the
    /// cost function for the 10-hop chain walk (matches gamemd.exe
    /// AStar_compute_edge_cost). The map is denormalized so no EntityStore
    /// lookup is required inside A*.
    pub entity_block_map: Option<&'a HashMap<(u16, u16), (u16, u16)>>,
    /// Code-2 urgency escalation (0 = look-ahead chain walk, 1 = traffic penalty,
    /// 2 = route around). Matches gamemd.exe PathfinderClass+0x3C.
    pub urgency: u8,
    /// Zone corridor restriction — only expand cells in these zones.
    pub corridor: Option<(
        &'a super::zone_map::ZoneMap,
        &'a BTreeSet<super::zone_map::ZoneId>,
    )>,
    /// Movement zone for water mover bypass and passability matrix.
    pub movement_zone: Option<MovementZone>,
    /// Resolved terrain for cliff cost and water passability checks.
    pub resolved_terrain: Option<&'a ResolvedTerrainGrid>,
    /// Infantry units always target ground level at bridge destinations.
    pub is_infantry: bool,
}

/// Reconstruct a layered path from dual came_from arrays.
/// Walks backward from goal using `decode_from` to follow the parent chain
/// across ground/bridge transitions.
fn reconstruct_path_dual(
    ground_from: &[usize],
    bridge_from: &[usize],
    start_idx: usize,
    start_on_bridge: bool,
    goal_idx: usize,
    goal_on_bridge: bool,
    width: usize,
) -> Vec<LayeredPathStep> {
    let mut path = Vec::new();
    let mut current_idx = goal_idx;
    let mut current_bridge = goal_on_bridge;

    loop {
        let x = (current_idx % width) as u16;
        let y = (current_idx / width) as u16;
        let layer = if current_bridge {
            MovementLayer::Bridge
        } else {
            MovementLayer::Ground
        };
        path.push(LayeredPathStep {
            rx: x,
            ry: y,
            layer,
        });

        if current_idx == start_idx && current_bridge == start_on_bridge {
            break;
        }

        let from_array = if current_bridge {
            bridge_from
        } else {
            ground_from
        };
        let encoded = from_array[current_idx];
        debug_assert_ne!(
            encoded,
            usize::MAX,
            "reconstruct_path_dual: hit unvisited cell at idx={} bridge={}",
            current_idx,
            current_bridge
        );
        let (parent_idx, parent_bridge) = decode_from(encoded);
        current_idx = parent_idx;
        current_bridge = parent_bridge;
    }

    path.reverse();
    path
}

/// Unified A* search with height-based bridge routing.
///
/// Matches gamemd.exe's single AStar_main_loop (0x00429a90). Uses dual closed
/// lists (ground/bridge) per cell, with closed-list selection based on the
/// CURRENT node's height vs neighbor's ground_level (not computed neighbor height).
///
/// Always returns `Vec<LayeredPathStep>` with per-cell layer info derived from
/// height comparison. Thin public wrappers extract `(u16, u16)` for callers that
/// don't need layer info.
pub fn astar_search(
    grid: &PathGrid,
    start: (u16, u16),
    start_layer: MovementLayer,
    goal: (u16, u16),
    options: &AStarOptions<'_>,
) -> Option<Vec<LayeredPathStep>> {
    let is_water_mover = options.movement_zone.is_some_and(|mz| mz.is_water_mover());

    // --- Start/goal passability ---
    let start_passable = if is_at_bridge_level(
        grid.cell(start.0, start.1).map_or(0, |c| {
            if start_layer == MovementLayer::Bridge {
                c.bridge_deck_level
            } else {
                c.ground_level
            }
        }),
        grid.cell(start.0, start.1).unwrap_or(&DEFAULT_BLOCKED_CELL),
    ) {
        grid.is_walkable_on_layer(start.0, start.1, MovementLayer::Bridge)
    } else {
        is_cell_passable_for_mover(
            grid,
            start.0,
            start.1,
            options.movement_zone,
            options.resolved_terrain,
        )
    };
    if !start_passable {
        // Fallback: try the other layer (matches old find_layered_path fallback)
        let alt_layer = if start_layer == MovementLayer::Bridge {
            MovementLayer::Ground
        } else {
            MovementLayer::Bridge
        };
        let alt_passable = grid.is_walkable_on_layer(start.0, start.1, alt_layer);
        if !alt_passable {
            return None;
        }
        // Recurse with flipped layer
        return astar_search(grid, start, alt_layer, goal, options);
    }

    // Goal must be walkable on at least one layer.
    let goal_ground_ok = is_cell_passable_for_mover(
        grid,
        goal.0,
        goal.1,
        options.movement_zone,
        options.resolved_terrain,
    );
    let goal_bridge_ok = grid.is_walkable_on_layer(goal.0, goal.1, MovementLayer::Bridge);
    if !goal_ground_ok && !goal_bridge_ok {
        return None;
    }

    // --- Height initialization ---
    let start_cell = grid.cell(start.0, start.1).unwrap_or(&DEFAULT_BLOCKED_CELL);
    let start_height = match start_layer {
        MovementLayer::Bridge => start_cell.bridge_deck_level,
        _ => start_cell.ground_level,
    };

    let goal_cell = grid.cell(goal.0, goal.1).unwrap_or(&DEFAULT_BLOCKED_CELL);
    let goal_height = if !options.is_infantry && goal_bridge_ok {
        goal_cell.bridge_deck_level
    } else {
        goal_cell.ground_level
    };

    // Trivial: already at goal with matching height
    if start == goal && start_height == goal_height {
        let layer = if is_at_bridge_level(start_height, start_cell) {
            MovementLayer::Bridge
        } else {
            MovementLayer::Ground
        };
        return Some(vec![LayeredPathStep {
            rx: start.0,
            ry: start.1,
            layer,
        }]);
    }

    // --- Arrays ---
    let w = grid.width() as usize;
    let h = grid.height() as usize;
    let total_cells = w * h;

    let mut ground_g: Vec<i32> = vec![i32::MAX; total_cells];
    let mut bridge_g: Vec<i32> = vec![i32::MAX; total_cells];
    let mut ground_from: Vec<usize> = vec![usize::MAX; total_cells];
    let mut bridge_from: Vec<usize> = vec![usize::MAX; total_cells];
    let mut ground_closed: Vec<bool> = vec![false; total_cells];
    let mut bridge_closed: Vec<bool> = vec![false; total_cells];

    let start_idx = start.1 as usize * w + start.0 as usize;
    let start_on_bridge = is_at_bridge_level(start_height, start_cell);
    if start_on_bridge {
        bridge_g[start_idx] = 0;
    } else {
        ground_g[start_idx] = 0;
    }

    let mut open: BinaryHeap<Reverse<AStarNode>> = BinaryHeap::new();
    open.push(Reverse(AStarNode {
        f_cost: octile_heuristic(start.0, start.1, goal.0, goal.1),
        g_cost: 0,
        x: start.0,
        y: start.1,
        height: start_height,
        on_bridge: start_on_bridge,
    }));

    let mut nodes_evaluated: u32 = 0;

    // --- Main loop ---
    while let Some(Reverse(current)) = open.pop() {
        let cx = current.x;
        let cy = current.y;
        let c_idx = cy as usize * w + cx as usize;
        let cur_cell = grid.cell(cx, cy).unwrap_or(&DEFAULT_BLOCKED_CELL);
        // Use the push-time layer flag carried on the node, matching gamemd.exe:
        // the layer is decided once at push-time from predecessor.height vs this
        // cell, never re-derived from the node's own height at pop. Re-derivation
        // diverged at bridgehead transition cells when compute_neighbor_height
        // collapsed to ground_level but the predecessor was still high enough to
        // select the bridge arrays — causing reconstruct_path_dual to read
        // usize::MAX from the wrong came_from array.
        let on_bridge = current.on_bridge;

        // Skip if already closed on this list
        if on_bridge {
            if bridge_closed[c_idx] {
                continue;
            }
            bridge_closed[c_idx] = true;
        } else {
            if ground_closed[c_idx] {
                continue;
            }
            ground_closed[c_idx] = true;
        }

        // Goal check: cell AND height must match
        if (cx, cy) == goal && current.height == goal_height {
            // Use the node's push-time layer flag (same value came_from was keyed on).
            return Some(reconstruct_path_dual(
                &ground_from,
                &bridge_from,
                start_idx,
                start_on_bridge,
                c_idx,
                on_bridge,
                w,
            ));
        }

        nodes_evaluated += 1;
        if nodes_evaluated >= MAX_SEARCH_NODES {
            log::warn!(
                "A* search exhausted {} nodes without finding path from ({},{}) to ({},{})",
                MAX_SEARCH_NODES,
                start.0,
                start.1,
                goal.0,
                goal.1,
            );
            return None;
        }

        // --- Neighbor expansion ---
        for (dir_index, &(dx, dy, is_diagonal)) in NEIGHBORS.iter().enumerate() {
            let nx_i = cx as i32 + dx;
            let ny_i = cy as i32 + dy;
            if nx_i < 0 || ny_i < 0 || nx_i >= grid.width() as i32 || ny_i >= grid.height() as i32 {
                continue;
            }
            let nx = nx_i as u16;
            let ny = ny_i as u16;
            let n_idx = ny as usize * w + nx as usize;
            let neighbor_cell = grid.cell(nx, ny).unwrap_or(&DEFAULT_BLOCKED_CELL);

            // Closed-list selection: uses CURRENT node's height vs neighbor ground_level
            let neighbor_use_bridge = is_at_bridge_level(current.height, neighbor_cell);

            // Compute what height the NEW node carries forward (separate computation)
            let neighbor_height = compute_neighbor_height(current.height, cur_cell, neighbor_cell);

            // Closed check on appropriate list
            if neighbor_use_bridge {
                if bridge_closed[n_idx] {
                    continue;
                }
            } else if ground_closed[n_idx] {
                continue;
            }

            // Walkability check on the determined layer
            let neighbor_passable = if neighbor_use_bridge {
                grid.is_walkable_on_layer(nx, ny, MovementLayer::Bridge)
            } else {
                is_cell_passable_for_mover(
                    grid,
                    nx,
                    ny,
                    options.movement_zone,
                    options.resolved_terrain,
                )
            };
            if !neighbor_passable {
                // Near-miss goal fallback (0x0042a17d): if the impassable neighbor
                // IS the goal cell and start/goal heights are close, accept the
                // path ending at the current node. This lets units route to the
                // nearest passable cell when the goal itself is blocked.
                if (nx, ny) == goal && start_height.abs_diff(goal_height) <= 1 {
                    // Use the current node's push-time layer flag (same value
                    // came_from was keyed on when the node was pushed).
                    return Some(reconstruct_path_dual(
                        &ground_from,
                        &bridge_from,
                        start_idx,
                        start_on_bridge,
                        c_idx,
                        on_bridge,
                        w,
                    ));
                }
                continue;
            }

            // Entity blocks (layer-separated). Goal exempt.
            if (nx, ny) != goal {
                let blocks = if neighbor_use_bridge {
                    options.bridge_blocks
                } else {
                    options.entity_blocks
                };
                if let Some(b) = blocks {
                    if b.contains(&(nx, ny)) {
                        continue;
                    }
                }
            }

            // Zone corridor filter
            if let Some((zone_map, allowed)) = options.corridor {
                let cell_zone = zone_map.zone_at(nx, ny, MovementLayer::Ground);
                if cell_zone != super::zone_map::ZONE_INVALID && !allowed.contains(&cell_zone) {
                    continue;
                }
            }

            // Terrain cost
            let terrain_cost: u8 = if neighbor_use_bridge {
                100 // bridge layer: no terrain cost modifiers
            } else if is_water_mover {
                100
            } else if let Some(cost_grid) = options.terrain_costs {
                cost_grid.cost_at(nx, ny)
            } else {
                100 // no cost grid: uniform cost
            };
            if terrain_cost == 0 {
                continue;
            }

            // Diagonal corner-cutting: both cardinal neighbors must be passable on same layer
            if is_diagonal {
                if neighbor_use_bridge {
                    if !grid.is_walkable_on_layer(nx, cy, MovementLayer::Bridge)
                        || !grid.is_walkable_on_layer(cx, ny, MovementLayer::Bridge)
                    {
                        continue;
                    }
                } else {
                    let adj1_ok = is_cell_passable_for_mover(
                        grid,
                        nx,
                        cy,
                        options.movement_zone,
                        options.resolved_terrain,
                    ) && (is_water_mover
                        || options
                            .terrain_costs
                            .map_or(true, |tc| tc.cost_at(nx, cy) > 0));
                    let adj2_ok = is_cell_passable_for_mover(
                        grid,
                        cx,
                        ny,
                        options.movement_zone,
                        options.resolved_terrain,
                    ) && (is_water_mover
                        || options
                            .terrain_costs
                            .map_or(true, |tc| tc.cost_at(cx, ny) > 0));
                    if !adj1_ok || !adj2_ok {
                        continue;
                    }
                }
            }

            // Step cost
            let base_cost = if is_diagonal {
                DIAGONAL_COST
            } else {
                CARDINAL_COST
            };
            let mut step_cost = if terrain_cost == 100 {
                base_cost
            } else {
                base_cost * 100 / terrain_cost as i32
            };

            // Cliff cost: uses effective path heights, NOT raw ground_levels
            if current.height != neighbor_height {
                step_cost *= CLIFF_COST_MULTIPLIER;
            }

            // Code-2 (friendly-moving blocker) dynamic cost. Goal cell is
            // exempt — if the goal is occupied, we still want to reach it.
            if (nx, ny) != goal {
                if let Some(map) = options.entity_block_map {
                    if map.contains_key(&(nx, ny)) {
                        step_cost *= compute_code2_multiplier(options.urgency, (nx, ny), map);
                    }
                }
            }

            // Direction tie-breaker
            let tentative_g = current.g_cost + step_cost + DIR_TIEBREAK[dir_index];

            // Update appropriate g-cost array
            let (g_array, from_array) = if neighbor_use_bridge {
                (&mut bridge_g, &mut bridge_from)
            } else {
                (&mut ground_g, &mut ground_from)
            };
            if tentative_g < g_array[n_idx] {
                g_array[n_idx] = tentative_g;
                from_array[n_idx] = encode_from(c_idx, on_bridge);
                let h = octile_heuristic(nx, ny, goal.0, goal.1);
                open.push(Reverse(AStarNode {
                    f_cost: tentative_g + h,
                    g_cost: tentative_g,
                    x: nx,
                    y: ny,
                    height: neighbor_height,
                    on_bridge: neighbor_use_bridge,
                }));
            }
        }
    }

    None
}

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
    // TODO(RE): This is still the local path-grid legality gate, not the stock
    // search-time can-enter/cost predicate. Keep the distinction explicit so we
    // can swap the real evaluator in once the remaining runtime inputs are known.
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

/// Per-cell walkability and bridge metadata for pathfinding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathCell {
    pub ground_walkable: bool,
    pub bridge_walkable: bool,
    pub transition: bool,
    pub ground_level: u8,
    pub bridge_deck_level: u8,
}

impl PathCell {
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

/// Default ground-only cell: walkable, no bridges, level 0.
const DEFAULT_WALKABLE_CELL: PathCell = PathCell {
    ground_walkable: true,
    bridge_walkable: false,
    transition: false,
    ground_level: 0,
    bridge_deck_level: 0,
};

/// Default blocked cell: not walkable, no bridges, level 0.
const DEFAULT_BLOCKED_CELL: PathCell = PathCell {
    ground_walkable: false,
    bridge_walkable: false,
    transition: false,
    ground_level: 0,
    bridge_deck_level: 0,
};

/// Unified walkability grid for pathfinding.
///
/// Each cell stores ground walkability, bridge walkability, transition flags,
/// and height levels. Flat A* reads `ground_walkable` via `is_walkable()`;
/// layered A* reads both layers for bridge-aware routing.
#[derive(Debug, Clone)]
pub struct PathGrid {
    cells: Vec<PathCell>,
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
    /// Create a new grid where all cells are ground-walkable with no bridges.
    pub fn new(width: u16, height: u16) -> Self {
        let size = width as usize * height as usize;
        Self {
            cells: vec![DEFAULT_WALKABLE_CELL; size],
            width,
            height,
        }
    }

    /// Grid width accessor.
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Grid height accessor.
    pub fn height(&self) -> u16 {
        self.height
    }

    /// Mark a cell as blocked (ground layer) or unblocked.
    pub fn set_blocked(&mut self, x: u16, y: u16, blocked: bool) {
        if x < self.width && y < self.height {
            let idx = y as usize * self.width as usize + x as usize;
            self.cells[idx].ground_walkable = !blocked;
        }
    }

    /// Check if a cell is ground-walkable. Out-of-bounds = impassable.
    pub fn is_walkable(&self, x: u16, y: u16) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        let idx = y as usize * self.width as usize + x as usize;
        self.cells[idx].ground_walkable
    }

    /// Check if a cell is walkable on a specific movement layer.
    pub fn is_walkable_on_layer(&self, x: u16, y: u16, layer: MovementLayer) -> bool {
        let Some(cell) = self.cell(x, y) else {
            return false;
        };
        match layer {
            MovementLayer::Ground => cell.ground_walkable,
            MovementLayer::Bridge => cell.bridge_walkable,
            MovementLayer::Air | MovementLayer::Underground => false,
        }
    }

    /// Check if a cell is walkable on either ground or bridge layer.
    pub fn is_any_layer_walkable(&self, x: u16, y: u16) -> bool {
        if self.is_walkable(x, y) {
            return true;
        }
        self.is_walkable_on_layer(x, y, MovementLayer::Bridge)
    }

    /// Access full cell data. Returns `None` for out-of-bounds.
    pub fn cell(&self, x: u16, y: u16) -> Option<&PathCell> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.cells
            .get(y as usize * self.width as usize + x as usize)
    }

    /// Whether this cell is a bridge transition point (units can switch layers here).
    pub fn is_transition(&self, x: u16, y: u16) -> bool {
        self.cell(x, y)
            .is_some_and(|c| c.is_bridge_transition_cell())
    }

    pub fn bridge_deck_level(&self, x: u16, y: u16) -> Option<u8> {
        self.cell(x, y).and_then(PathCell::bridge_deck_level_if_any)
    }

    pub fn can_enter_bridge_layer_from_ground(&self, x: u16, y: u16) -> bool {
        self.cell(x, y)
            .is_some_and(PathCell::can_enter_bridge_layer_from_ground)
    }

    /// Find the nearest ground-walkable cell to `(x, y)`, searching in expanding rings.
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
            let r = radius as i32;
            for d in -r..=r {
                let candidates = [
                    (x as i32 + d, y as i32 - r),
                    (x as i32 + d, y as i32 + r),
                    (x as i32 - r, y as i32 + d),
                    (x as i32 + r, y as i32 + d),
                ];
                for (cx, cy) in candidates {
                    if cx < 0 || cy < 0 || cx >= self.width as i32 || cy >= self.height as i32 {
                        continue;
                    }
                    let cx = cx as u16;
                    let cy = cy as u16;
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

    /// Find nearest walkable cell on either layer, searching in expanding rings.
    pub fn nearest_walkable_any_layer(
        &self,
        x: u16,
        y: u16,
        max_radius: u16,
        entity_blocks: Option<&BTreeSet<(u16, u16)>>,
        allow: Option<(u16, u16)>,
    ) -> Option<(u16, u16)> {
        let check = |cx: u16, cy: u16| -> bool {
            self.is_any_layer_walkable(cx, cy)
                && (allow == Some((cx, cy))
                    || entity_blocks.map_or(true, |b| !b.contains(&(cx, cy))))
        };
        if check(x, y) {
            return Some((x, y));
        }
        for radius in 1..=max_radius {
            let r = radius as i32;
            for d in -r..=r {
                let candidates = [
                    (x as i32 + d, y as i32 - r),
                    (x as i32 + d, y as i32 + r),
                    (x as i32 - r, y as i32 + d),
                    (x as i32 + r, y as i32 + d),
                ];
                for (cx, cy) in candidates {
                    if cx < 0 || cy < 0 || cx >= self.width as i32 || cy >= self.height as i32 {
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

    /// Build a walkability grid from map cell data and tileset classification.
    ///
    /// Strategy: start with all cells **blocked**, then mark cells that have
    /// valid terrain data (non-water, non-cliff) as walkable. This ensures
    /// cells outside the map bounds are impassable by default.
    /// No bridge data is populated — use `from_resolved_terrain_with_bridges` for that.
    pub fn from_map_data(
        cells: &[MapCell],
        lookup: Option<&TilesetLookup>,
        map_width: u16,
        map_height: u16,
    ) -> Self {
        let size = map_width as usize * map_height as usize;
        let mut grid = PathGrid {
            cells: vec![DEFAULT_BLOCKED_CELL; size],
            width: map_width,
            height: map_height,
        };

        let mut walkable_count: u32 = 0;
        let mut water_count: u32 = 0;
        let mut cliff_count: u32 = 0;

        for cell in cells {
            if cell.tile_index < 0 {
                continue;
            }
            if cell.rx >= map_width || cell.ry >= map_height {
                continue;
            }
            let tile_id: u16 = if cell.tile_index == 0xFFFF {
                0
            } else {
                cell.tile_index as u16
            };
            let is_water = lookup.map_or(false, |l| l.is_water(tile_id));
            let is_cliff = lookup.map_or(false, |l| l.is_cliff(tile_id));
            if is_water {
                water_count += 1;
            }
            if is_cliff {
                cliff_count += 1;
                continue;
            }
            let idx = cell.ry as usize * map_width as usize + cell.rx as usize;
            if idx < grid.cells.len() {
                grid.cells[idx].ground_walkable = true;
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

    /// Build from resolved terrain without bridge data.
    pub fn from_resolved_terrain(terrain: &ResolvedTerrainGrid) -> Self {
        Self::from_resolved_terrain_with_bridges(terrain, None)
    }

    /// Build from resolved terrain with bridge metadata.
    ///
    /// Ground walkability: water cells and bridge deck cells are kept walkable
    /// even when `ground_walk_blocked` is true — SpeedType-dependent blocking
    /// is handled by TerrainCostGrid (cost=0 blocks ground units in A*).
    /// This preserves the behavior of the old flat PathGrid where Float/Hover/
    /// Amphibious units could path through water via cost > 0.
    pub fn from_resolved_terrain_with_bridges(
        terrain: &ResolvedTerrainGrid,
        bridge_state: Option<&BridgeRuntimeState>,
    ) -> Self {
        let cells = terrain
            .iter()
            .map(|cell| PathCell {
                // Walkability rules (matching old PathGrid::from_resolved_terrain):
                // - Overlay blocks / terrain object blocks → blocked
                // - Intact bridge deck → walkable (overrides underlying terrain)
                // - Destroyed bridge deck → revert to underlying terrain
                // - Cliff → blocked
                // - Water → walkable (SpeedType cost=0 blocks ground in A*)
                // - Everything else → use ground_walk_blocked
                ground_walkable: if cell.overlay_blocks || cell.terrain_object_blocks {
                    false
                } else if cell.has_bridge_deck {
                    let bridge_intact = bridge_state
                        .map_or(true, |state| state.is_bridge_walkable(cell.rx, cell.ry));
                    if bridge_intact {
                        true
                    } else {
                        // Destroyed bridge: revert to underlying terrain walkability.
                        !cell.is_cliff_like && !cell.ground_walk_blocked
                    }
                } else if cell.is_cliff_like {
                    false
                } else {
                    !cell.ground_walk_blocked || cell.is_water
                },
                bridge_walkable: bridge_state.map_or(cell.bridge_walkable, |state| {
                    state.is_bridge_walkable(cell.rx, cell.ry)
                }),
                transition: bridge_state.map_or(cell.bridge_transition, |state| {
                    cell.bridge_transition
                        && (!cell.has_bridge_deck || state.is_bridge_walkable(cell.rx, cell.ry))
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

    /// Compute cells whose path-relevant walkability differs between two grids.
    /// Returns `None` if grids have different dimensions (full rebuild needed).
    pub fn diff_cells(&self, other: &PathGrid) -> Option<Vec<(u16, u16)>> {
        if self.width != other.width || self.height != other.height {
            return None;
        }
        let w = self.width as usize;
        let mut changed = Vec::new();
        for (idx, (a, b)) in self.cells.iter().zip(other.cells.iter()).enumerate() {
            if a.ground_walkable != b.ground_walkable
                || a.bridge_walkable != b.bridge_walkable
                || a.transition != b.transition
                || a.ground_level != b.ground_level
                || a.bridge_deck_level != b.bridge_deck_level
            {
                changed.push(((idx % w) as u16, (idx / w) as u16));
            }
        }
        Some(changed)
    }

    /// Mark cells occupied by a building footprint as blocked (ground layer).
    /// Buildings only block the ground layer — bridge decks above are unaffected.
    pub fn block_building_footprint(&mut self, cell_rx: u16, cell_ry: u16, foundation: &str) {
        let (fw, fh): (u16, u16) = parse_foundation(foundation);
        for dy in 0..fh {
            for dx in 0..fw {
                let bx = cell_rx.wrapping_add(dx);
                let by = cell_ry.wrapping_add(dy);
                self.set_blocked(bx, by, true);
            }
        }
    }

    /// Construct from raw cell data (test helper).
    #[cfg(test)]
    pub fn from_cells(cells: Vec<PathCell>, width: u16, height: u16) -> Self {
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
    /// Path height at this node — used for bridge-aware routing.
    /// Ground-only searches carry ground_level throughout.
    height: u8,
    /// Layer flag decided at push-time from predecessor.height vs this cell,
    /// matching gamemd.exe's push-time layer selection. Used at pop-time for
    /// closed-list marking and for `reconstruct_path_dual` array selection
    /// so storage and retrieval always agree (fixes bridgehead transition
    /// cells where pop-time re-derivation from own height would diverge).
    on_bridge: bool,
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

/// Compute the code-2 cost multiplier for a friendly-moving blocker.
///
/// Matches gamemd.exe AStar_compute_edge_cost @ 0x00429830 — the dynamic
/// cost computation for Can_Enter_Cell return code 2 (friendly moving).
///
/// - `urgency == 2` → `1000` (route around the blocker)
/// - `urgency == 1` → `4`   (traffic penalty, no look-ahead)
/// - `urgency == 0` → walk up to 10 hops along the blocker chain. Each hop
///   reads `map[cur_cell]` which gives the blocker's next cell, then jumps
///   to that cell and repeats. Returns `1` if the chain clears (next cell
///   has no blocker) within 10 hops. Returns `4` if the chain lasts all
///   10 hops.
fn compute_code2_multiplier(
    urgency: u8,
    start_cell: (u16, u16),
    map: &HashMap<(u16, u16), (u16, u16)>,
) -> i32 {
    if urgency >= 2 {
        return CODE2_MULT_ROUTE_AROUND;
    }
    if urgency == 1 {
        return CODE2_MULT_JAM;
    }
    // urgency == 0: chain walk.
    let mut cur = start_cell;
    for _ in 0..CODE2_CHAIN_MAX_HOPS {
        let Some(&next) = map.get(&cur) else {
            // Chain clears: no blocker at this cell → the blocker upstream
            // will vacate into free space.
            return CODE2_MULT_CLEARING;
        };
        if next == cur {
            // Degenerate self-loop — treat as jam.
            return CODE2_MULT_JAM;
        }
        cur = next;
    }
    // Full 10 hops still jammed.
    CODE2_MULT_JAM
}

/// Find a path from start to goal using A* search.
///
/// Returns `Some(path)` where path is a sequence of (rx, ry) cells from
/// start to goal (both inclusive). Returns `None` if no path exists or
/// the search exceeds MAX_SEARCH_NODES.
pub fn find_path(grid: &PathGrid, start: (u16, u16), goal: (u16, u16)) -> Option<Vec<(u16, u16)>> {
    let steps = astar_search(
        grid,
        start,
        MovementLayer::Ground,
        goal,
        &AStarOptions::default(),
    )?;
    Some(steps.into_iter().map(|s| (s.rx, s.ry)).collect())
}

/// A* pathfinding with optional terrain cost modifiers and entity blocking.
///
/// When `costs` is `Some`, step cost is scaled by `100 / cost_at(x,y)`.
/// When `entity_blocks` is `Some`, cells in the set are treated as blocked
/// UNLESS they are the goal cell.
pub fn find_path_with_costs(
    grid: &PathGrid,
    start: (u16, u16),
    goal: (u16, u16),
    costs: Option<&TerrainCostGrid>,
    entity_blocks: Option<&BTreeSet<(u16, u16)>>,
    movement_zone: Option<MovementZone>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
    entity_block_map: Option<&HashMap<(u16, u16), (u16, u16)>>,
    urgency: u8,
) -> Option<Vec<(u16, u16)>> {
    let steps = astar_search(
        grid,
        start,
        MovementLayer::Ground,
        goal,
        &AStarOptions {
            terrain_costs: costs,
            entity_blocks,
            entity_block_map,
            urgency,
            movement_zone,
            resolved_terrain,
            ..Default::default()
        },
    )?;
    Some(steps.into_iter().map(|s| (s.rx, s.ry)).collect())
}

/// Corridor-restricted A*: only expands cells whose zone ID is in `allowed_zones`.
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
    entity_block_map: Option<&HashMap<(u16, u16), (u16, u16)>>,
    urgency: u8,
) -> Option<Vec<(u16, u16)>> {
    let steps = astar_search(
        grid,
        start,
        MovementLayer::Ground,
        goal,
        &AStarOptions {
            terrain_costs: costs,
            entity_blocks,
            corridor: Some((zone_map, allowed_zones)),
            entity_block_map,
            urgency,
            movement_zone,
            resolved_terrain,
            ..Default::default()
        },
    )?;
    Some(steps.into_iter().map(|s| (s.rx, s.ry)).collect())
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

/// Bridge-aware A* pathfinding with height-based routing.
///
/// Uses dual closed lists (ground/bridge) per cell for bridge-aware routing.
/// Returns per-cell layer assignment derived from height comparison.
pub fn find_layered_path(
    grid: &PathGrid,
    ground_blocks: Option<&BTreeSet<(u16, u16)>>,
    bridge_blocks: Option<&BTreeSet<(u16, u16)>>,
    start: (u16, u16),
    start_layer: MovementLayer,
    goal: (u16, u16),
    terrain_costs: Option<&TerrainCostGrid>,
    entity_block_map: Option<&HashMap<(u16, u16), (u16, u16)>>,
    urgency: u8,
) -> Option<Vec<LayeredPathStep>> {
    if !matches!(start_layer, MovementLayer::Ground | MovementLayer::Bridge) {
        return None;
    }
    astar_search(
        grid,
        start,
        start_layer,
        goal,
        &AStarOptions {
            terrain_costs,
            entity_blocks: ground_blocks,
            bridge_blocks,
            entity_block_map,
            urgency,
            ..Default::default()
        },
    )
}

// Tests extracted to core_tests.rs to stay under 400 lines.
#[cfg(test)]
#[path = "core_tests.rs"]
mod core_tests;
