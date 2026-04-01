//! Ore growth and spread system — data-driven from rules.ini and map INI.
//!
//! Ports the proven RA1 algorithm (MapClass::Logic + CellClass::Grow/Spread_Tiberium)
//! into the RA2 engine's ResourceNode model. All tuning comes from INI files:
//! - rules.ini [General]: GrowthRate, TiberiumGrows, TiberiumSpreads
//! - map INI [Basic]: TiberiumGrowthEnabled
//! - map INI [SpecialFlags]: TiberiumGrows, TiberiumSpreads
//!
//! ## Algorithm (matching RA1 MapClass::Logic)
//! 1. Incremental scan: each tick processes a fraction of the map
//! 2. Collect growth/spread candidates via reservoir sampling
//! 3. When full scan completes: execute growth, then spread
//! 4. Growth = increase ore remaining by one richness level (ore only, not gems)
//! 5. Spread = spawn new ore in a random adjacent empty+walkable cell
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/miner (ResourceNode, ResourceType),
//!   sim/pathfinding (PathGrid), sim/rng (SimRng), rules/.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use std::collections::BTreeMap;

use crate::map::basic::{BasicSection, SpecialFlagsSection};
use crate::rules::ruleset::GeneralRules;
use crate::sim::miner::{ResourceNode, ResourceType};
use crate::sim::pathfinding::PathGrid;
use crate::sim::rng::SimRng;
use crate::util::fixed_math::{SIM_TICK_HZ, SimFixed};

/// Base ore stock per richness level — matches seed_resource_nodes_from_overlays().
const ORE_BASE_PER_LEVEL: u16 = 120;
/// Maximum ore richness = 12 levels (OverlayData 0-11 in RA1).
const MAX_ORE_LEVELS: u16 = 12;
/// Maximum ore `remaining` value (12 levels * 120 per level).
const MAX_ORE_REMAINING: u16 = ORE_BASE_PER_LEVEL * MAX_ORE_LEVELS;
/// Ore must be above this threshold to spread (>6 levels, matching RA1 OverlayData > 6).
const SPREAD_THRESHOLD: u16 = ORE_BASE_PER_LEVEL * 6;
/// Max candidates collected per scan cycle (bounded like RA1's fixed-size arrays).
const MAX_CANDIDATES: usize = 50;

/// 8 adjacent directions for spread: N, NE, E, SE, S, SW, W, NW.
const ADJACENT_OFFSETS: [(i32, i32); 8] = [
    (0, -1),
    (1, -1),
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
];

/// Effective ore growth configuration resolved from merged INI sources.
///
/// Constructed once at map load. The resolution order is:
/// map [SpecialFlags] > map [Basic] > rules.ini [General]
/// All flags must be true for growth/spread to be active.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OreGrowthConfig {
    /// Whether ore cells grow denser over time.
    pub grows: bool,
    /// Whether rich ore spreads to adjacent empty cells.
    pub spreads: bool,
    /// Seconds per full map growth scan cycle (from GrowthRate= in minutes, converted
    /// to integer seconds at config construction to avoid f32 in the tick path).
    pub growth_rate_seconds: u32,
}

impl OreGrowthConfig {
    /// Resolve effective config from rules.ini [General] + map [Basic] + map [SpecialFlags].
    ///
    /// Resolution: each flag must be true at ALL levels to be enabled.
    /// GrowthRate comes only from rules.ini (not overridable per-map).
    pub fn from_ini(
        general: &GeneralRules,
        basic: &BasicSection,
        special_flags: &SpecialFlagsSection,
    ) -> Self {
        let grows = general.tiberium_grows
            && basic.tiberium_growth_enabled.unwrap_or(true)
            && special_flags.tiberium_grows.unwrap_or(true);
        let spreads = general.tiberium_spreads && special_flags.tiberium_spreads.unwrap_or(true);
        let growth_rate_minutes = general.growth_rate_minutes.max(0.01);
        // Convert f32 minutes → integer seconds at the INI boundary via
        // fixed-point to avoid platform-dependent f32 multiplication rounding.
        let rate_fixed = SimFixed::saturating_from_num(growth_rate_minutes);
        let growth_rate_seconds =
            (rate_fixed * SimFixed::from_num(60)).to_num::<i32>().max(1) as u32;

        log::info!(
            "OreGrowthConfig: grows={}, spreads={}, rate={}s",
            grows,
            spreads,
            growth_rate_seconds,
        );

        Self {
            grows,
            spreads,
            growth_rate_seconds,
        }
    }

    /// Disabled config — no growth or spread.
    pub fn disabled() -> Self {
        Self {
            grows: false,
            spreads: false,
            growth_rate_seconds: 300, // 5 minutes
        }
    }
}

/// Persistent state for the incremental map scanner.
///
/// Lives in ProductionState. The scanner processes a fraction of the map each
/// tick and collects candidates via reservoir sampling (fair random selection
/// from a stream of unknown length, bounded to MAX_CANDIDATES).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OreGrowthState {
    /// Current position in the cell iteration (wraps to 0 after full scan).
    scan_cursor: usize,
    /// Total number of cells to scan (map_width * map_height).
    total_cells: usize,
    /// Map dimensions for cell coordinate conversion.
    map_width: u16,
    /// Cells eligible for growth this scan cycle.
    growth_candidates: Vec<(u16, u16)>,
    /// Cells eligible for spread this scan cycle.
    spread_candidates: Vec<(u16, u16)>,
    /// Reservoir sampling counter for growth (total candidates seen).
    growth_seen: usize,
    /// Reservoir sampling counter for spread (total candidates seen).
    spread_seen: usize,
}

impl OreGrowthState {
    /// Create a new scanner for a map of the given dimensions.
    pub fn new(map_width: u16, map_height: u16) -> Self {
        Self {
            scan_cursor: 0,
            total_cells: map_width as usize * map_height as usize,
            map_width,
            growth_candidates: Vec::with_capacity(MAX_CANDIDATES),
            spread_candidates: Vec::with_capacity(MAX_CANDIDATES),
            growth_seen: 0,
            spread_seen: 0,
        }
    }
}

/// Advance ore growth/spread by one sim tick.
///
/// This is the main entry point called from advance_tick(). It scans a fraction
/// of the map each tick and executes growth/spread when a full cycle completes.
pub fn tick_ore_growth(
    config: &OreGrowthConfig,
    state: &mut OreGrowthState,
    resource_nodes: &mut BTreeMap<(u16, u16), ResourceNode>,
    path_grid: Option<&PathGrid>,
    rng: &mut SimRng,
) {
    if !config.grows && !config.spreads {
        return;
    }
    if state.total_cells == 0 {
        return;
    }

    // How many cells to scan this tick: total_cells / (rate_seconds * tick_hz).
    // This ensures one full scan completes every `growth_rate_seconds` seconds.
    let rate_seconds: u32 = config.growth_rate_seconds.max(1);
    let ticks_per_cycle: u32 = rate_seconds.saturating_mul(SIM_TICK_HZ).max(1);
    let cells_per_tick: usize =
        (state.total_cells as u32).div_ceil(ticks_per_cycle).max(1) as usize;

    // Scan a chunk of cells from the cursor position.
    let scan_end = (state.scan_cursor + cells_per_tick).min(state.total_cells);

    // We iterate over resource_nodes rather than all cells — much more efficient
    // since only a small fraction of cells have ore. We filter by coordinate range
    // corresponding to the current scan chunk.
    for (&(rx, ry), node) in resource_nodes.iter() {
        let cell_index = ry as usize * state.map_width as usize + rx as usize;
        if cell_index < state.scan_cursor || cell_index >= scan_end {
            continue;
        }

        // Only ore grows/spreads (not gems), matching RA1 behavior.
        if node.resource_type != ResourceType::Ore {
            continue;
        }

        // Can this cell grow? (ore present, below max richness)
        if config.grows && node.remaining < MAX_ORE_REMAINING {
            reservoir_sample(
                &mut state.growth_candidates,
                &mut state.growth_seen,
                (rx, ry),
                rng,
            );
        }

        // Can this cell spread? (ore present, above spread threshold)
        if config.spreads && node.remaining > SPREAD_THRESHOLD {
            reservoir_sample(
                &mut state.spread_candidates,
                &mut state.spread_seen,
                (rx, ry),
                rng,
            );
        }
    }

    state.scan_cursor = scan_end;

    // When full scan completes, execute collected growth and spread actions.
    if state.scan_cursor >= state.total_cells {
        // Phase 1: Growth — increase remaining by one richness level.
        if config.grows {
            for &(rx, ry) in &state.growth_candidates {
                if let Some(node) = resource_nodes.get_mut(&(rx, ry)) {
                    if node.resource_type == ResourceType::Ore && node.remaining < MAX_ORE_REMAINING
                    {
                        let new_remaining = node.remaining + ORE_BASE_PER_LEVEL;
                        node.remaining = new_remaining.min(MAX_ORE_REMAINING);
                    }
                }
            }
        }

        // Phase 2: Spread — spawn new ore in a random adjacent empty cell.
        if config.spreads {
            for &(rx, ry) in &state.spread_candidates {
                try_spread_ore(resource_nodes, path_grid, rng, rx, ry, state.map_width);
            }
        }

        // Reset for next cycle.
        state.scan_cursor = 0;
        state.growth_candidates.clear();
        state.spread_candidates.clear();
        state.growth_seen = 0;
        state.spread_seen = 0;

        let node_count = resource_nodes.len();
        log::debug!(
            "Ore growth cycle complete: {} resource nodes on map",
            node_count
        );
    }
}

/// Reservoir sampling: maintain a bounded random sample from a stream.
///
/// Ensures each candidate has an equal probability of being in the final sample,
/// regardless of the total stream length. Matches RA1's MapClass::Logic approach.
fn reservoir_sample(
    candidates: &mut Vec<(u16, u16)>,
    seen: &mut usize,
    cell: (u16, u16),
    rng: &mut SimRng,
) {
    *seen += 1;
    if candidates.len() < MAX_CANDIDATES {
        candidates.push(cell);
    } else {
        // Replace a random existing candidate with probability MAX_CANDIDATES / seen.
        let r = rng.next_range_u32(*seen as u32) as usize;
        if r < MAX_CANDIDATES {
            candidates[r] = cell;
        }
    }
}

/// Try to spread ore from (rx, ry) to a random adjacent cell.
///
/// Picks a random starting direction and checks all 8 neighbors. The first
/// cell that passes `can_germinate()` gets a new ore node at level 1.
fn try_spread_ore(
    resource_nodes: &mut BTreeMap<(u16, u16), ResourceNode>,
    path_grid: Option<&PathGrid>,
    rng: &mut SimRng,
    rx: u16,
    ry: u16,
    map_width: u16,
) {
    // Random starting direction for fairness (matching RA1 Random_Pick(FACING_N, FACING_NW)).
    let start_dir = rng.next_range_u32(8) as usize;

    for i in 0..8 {
        let dir = (start_dir + i) % 8;
        let (dx, dy) = ADJACENT_OFFSETS[dir];
        let nx = rx as i32 + dx;
        let ny = ry as i32 + dy;

        // Bounds check.
        if nx < 0 || ny < 0 || nx >= map_width as i32 {
            continue;
        }
        let nx = nx as u16;
        let ny = ny as u16;

        if can_germinate(resource_nodes, path_grid, nx, ny) {
            resource_nodes.insert(
                (nx, ny),
                ResourceNode {
                    resource_type: ResourceType::Ore,
                    remaining: ORE_BASE_PER_LEVEL,
                },
            );
            return;
        }
    }
}

/// Whether a cell can receive new ore via spread.
///
/// Matches RA1 CellClass::Can_Tiberium_Germinate:
/// - No existing resource node on the cell
/// - Cell is within map bounds
/// - Cell is walkable (not water, cliff, or building footprint)
fn can_germinate(
    resource_nodes: &BTreeMap<(u16, u16), ResourceNode>,
    path_grid: Option<&PathGrid>,
    rx: u16,
    ry: u16,
) -> bool {
    // Already has a resource node — can't place another.
    if resource_nodes.contains_key(&(rx, ry)) {
        return false;
    }

    // Must be walkable terrain (not water, cliff, or building).
    if let Some(grid) = path_grid {
        if !grid.is_walkable(rx, ry) {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::miner::{ResourceNode, ResourceType};
    use crate::sim::rng::SimRng;

    fn make_config(grows: bool, spreads: bool) -> OreGrowthConfig {
        OreGrowthConfig {
            grows,
            spreads,
            growth_rate_seconds: 1, // Very fast for testing
        }
    }

    fn make_state(width: u16, height: u16) -> OreGrowthState {
        OreGrowthState::new(width, height)
    }

    fn ore_node(remaining: u16) -> ResourceNode {
        ResourceNode {
            resource_type: ResourceType::Ore,
            remaining,
        }
    }

    fn gem_node(remaining: u16) -> ResourceNode {
        ResourceNode {
            resource_type: ResourceType::Gem,
            remaining,
        }
    }

    /// Run enough ticks to complete one full scan cycle.
    fn run_full_cycle(
        config: &OreGrowthConfig,
        state: &mut OreGrowthState,
        nodes: &mut BTreeMap<(u16, u16), ResourceNode>,
        rng: &mut SimRng,
    ) {
        for _ in 0..10000 {
            tick_ore_growth(config, state, nodes, None, rng);
            if state.scan_cursor == 0 {
                return;
            }
        }
        panic!("Full cycle did not complete within 10000 ticks");
    }

    #[test]
    fn growth_increments_ore_remaining() {
        let config = make_config(true, false);
        let mut state = make_state(10, 10);
        let mut nodes = BTreeMap::new();
        nodes.insert((5, 5), ore_node(120)); // Level 1
        let mut rng = SimRng::new(42);

        run_full_cycle(&config, &mut state, &mut nodes, &mut rng);

        let node = nodes.get(&(5, 5)).expect("node still exists");
        assert_eq!(node.remaining, 240, "Should grow by one level (120)");
    }

    #[test]
    fn growth_caps_at_max_remaining() {
        let config = make_config(true, false);
        let mut state = make_state(10, 10);
        let mut nodes = BTreeMap::new();
        nodes.insert((3, 3), ore_node(MAX_ORE_REMAINING - 10)); // Near max
        let mut rng = SimRng::new(42);

        run_full_cycle(&config, &mut state, &mut nodes, &mut rng);

        let node = nodes.get(&(3, 3)).expect("node still exists");
        assert_eq!(node.remaining, MAX_ORE_REMAINING, "Should cap at max");
    }

    #[test]
    fn gems_do_not_grow_or_spread() {
        let config = make_config(true, true);
        let mut state = make_state(10, 10);
        let mut nodes = BTreeMap::new();
        nodes.insert((5, 5), gem_node(900)); // Rich gems — above spread threshold
        let mut rng = SimRng::new(42);

        run_full_cycle(&config, &mut state, &mut nodes, &mut rng);

        let node = nodes.get(&(5, 5)).expect("node still exists");
        assert_eq!(node.remaining, 900, "Gems should not grow");
        // Only the original gem node should exist (no spread).
        assert_eq!(nodes.len(), 1, "Gems should not spread");
    }

    #[test]
    fn spread_creates_new_ore_node() {
        let config = make_config(false, true);
        let mut state = make_state(10, 10);
        let mut nodes = BTreeMap::new();
        // Rich ore above spread threshold.
        nodes.insert((5, 5), ore_node(SPREAD_THRESHOLD + 120));
        let mut rng = SimRng::new(42);

        run_full_cycle(&config, &mut state, &mut nodes, &mut rng);

        assert!(
            nodes.len() > 1,
            "Should have spread to at least one adjacent cell"
        );
        // New node should be ore at base level.
        for (&(rx, ry), node) in &nodes {
            if rx == 5 && ry == 5 {
                continue;
            }
            assert_eq!(node.resource_type, ResourceType::Ore);
            assert_eq!(node.remaining, ORE_BASE_PER_LEVEL);
            // Must be adjacent to (5,5).
            let dx = (rx as i32 - 5).unsigned_abs();
            let dy = (ry as i32 - 5).unsigned_abs();
            assert!(dx <= 1 && dy <= 1, "Spread node must be adjacent");
        }
    }

    #[test]
    fn ore_below_threshold_does_not_spread() {
        let config = make_config(false, true);
        let mut state = make_state(10, 10);
        let mut nodes = BTreeMap::new();
        nodes.insert((5, 5), ore_node(SPREAD_THRESHOLD - 1)); // Below threshold
        let mut rng = SimRng::new(42);

        run_full_cycle(&config, &mut state, &mut nodes, &mut rng);

        assert_eq!(nodes.len(), 1, "Low ore should not spread");
    }

    #[test]
    fn disabled_flags_prevent_all_activity() {
        let config = make_config(false, false);
        let mut state = make_state(10, 10);
        let mut nodes = BTreeMap::new();
        nodes.insert((5, 5), ore_node(120));
        let mut rng = SimRng::new(42);

        // Run many ticks — nothing should change.
        for _ in 0..100 {
            tick_ore_growth(&config, &mut state, &mut nodes, None, &mut rng);
        }

        let node = nodes.get(&(5, 5)).expect("node still exists");
        assert_eq!(node.remaining, 120, "Nothing should change when disabled");
    }

    #[test]
    fn cannot_germinate_on_existing_node() {
        let mut nodes = BTreeMap::new();
        nodes.insert((5, 5), ore_node(120));

        assert!(!can_germinate(&nodes, None, 5, 5));
        assert!(can_germinate(&nodes, None, 5, 6));
    }

    #[test]
    fn reservoir_sampling_stays_bounded() {
        let mut candidates: Vec<(u16, u16)> = Vec::new();
        let mut seen: usize = 0;
        let mut rng = SimRng::new(99);

        for i in 0..500 {
            reservoir_sample(&mut candidates, &mut seen, (i, 0), &mut rng);
        }

        assert_eq!(seen, 500);
        assert!(
            candidates.len() <= MAX_CANDIDATES,
            "Candidates should not exceed MAX_CANDIDATES"
        );
    }

    #[test]
    fn full_scan_cycle_resets_cursor() {
        let config = make_config(true, false);
        let mut state = make_state(5, 5); // 25 cells — very small
        let mut nodes = BTreeMap::new();
        nodes.insert((2, 2), ore_node(120));
        let mut rng = SimRng::new(42);

        // Run ticks until cursor wraps.
        let mut wrapped = false;
        for _ in 0..1000 {
            tick_ore_growth(&config, &mut state, &mut nodes, None, &mut rng);
            if state.scan_cursor == 0 {
                wrapped = true;
                break;
            }
        }

        assert!(wrapped, "Scan cursor should wrap to 0 after full cycle");
    }

    #[test]
    fn growth_rate_controls_scan_speed() {
        // Fast rate: 0.01 minutes → scans many cells per tick.
        let fast = make_config(true, false);
        let mut state_fast = make_state(100, 100); // 10000 cells
        let mut nodes_fast = BTreeMap::new();
        nodes_fast.insert((50, 50), ore_node(120));
        let mut rng = SimRng::new(42);

        tick_ore_growth(&fast, &mut state_fast, &mut nodes_fast, None, &mut rng);
        let fast_progress = state_fast.scan_cursor;

        // Slow rate: 100 minutes → scans very few cells per tick.
        let slow = OreGrowthConfig {
            grows: true,
            spreads: false,
            growth_rate_seconds: 6000, // 100 minutes
        };
        let mut state_slow = make_state(100, 100);
        let mut nodes_slow = BTreeMap::new();
        nodes_slow.insert((50, 50), ore_node(120));
        let mut rng2 = SimRng::new(42);

        tick_ore_growth(&slow, &mut state_slow, &mut nodes_slow, None, &mut rng2);
        let slow_progress = state_slow.scan_cursor;

        assert!(
            fast_progress > slow_progress,
            "Fast rate ({}) should scan more cells per tick than slow rate ({})",
            fast_progress,
            slow_progress,
        );
    }

    #[test]
    fn spread_does_not_overwrite_existing_nodes() {
        let config = make_config(false, true);
        let mut state = make_state(10, 10);
        let mut nodes = BTreeMap::new();
        // Rich source at center.
        nodes.insert((5, 5), ore_node(SPREAD_THRESHOLD + 120));
        // Surround with existing gem nodes — spread should not overwrite them.
        for &(dx, dy) in &ADJACENT_OFFSETS {
            let nx = (5 + dx) as u16;
            let ny = (5 + dy) as u16;
            nodes.insert((nx, ny), gem_node(500));
        }
        let mut rng = SimRng::new(42);

        run_full_cycle(&config, &mut state, &mut nodes, &mut rng);

        // Should still have exactly 9 nodes (center + 8 neighbors).
        assert_eq!(nodes.len(), 9, "No new nodes should appear when surrounded");
        // All neighbors should still be gems.
        for &(dx, dy) in &ADJACENT_OFFSETS {
            let nx = (5 + dx) as u16;
            let ny = (5 + dy) as u16;
            let node = nodes.get(&(nx, ny)).expect("neighbor exists");
            assert_eq!(
                node.resource_type,
                ResourceType::Gem,
                "Neighbors should be unchanged gems"
            );
        }
    }
}
