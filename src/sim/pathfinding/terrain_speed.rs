//! Runtime per-cell speed modifiers applied during movement execution.
//!
//! The original engine applies terrain speed as a runtime modifier (not an
//! A* cost weight). Three multiplicative factors are combined each tick:
//!
//! 1. **Terrain type** — from rules.ini land-type sections ([Clear] Foot=100%, etc.)
//! 2. **Slope** — uphill slows, downhill speeds up (SlopeClimb / SlopeDescend)
//! 3. **Crowd density** — units slow when many neighbors occupy nearby cells
//!
//! Depends on: `ResolvedTerrainGrid` (cell height + land type), `OccupancyGrid`
//! (nearby unit count), `SpeedCostProfile` (INI-parsed terrain percentages).

use crate::map::resolved_terrain::ResolvedTerrainGrid;
use crate::rules::locomotor_type::{LocomotorKind, SpeedType};
use crate::sim::occupancy::OccupancyGrid;
use crate::util::fixed_math::{SIM_HALF, SIM_ONE, SimFixed};

// --- Constants from the original engine ---

/// Original engine boosts 0% terrain speed to 50% so passable terrain never
/// fully immobilizes a unit. Applied when the INI percentage is exactly 0.
const TERRAIN_SPEED_MIN: SimFixed = SIM_HALF;

/// Original engine clamps terrain speed multipliers above 1.0 to exactly 1.0.
const TERRAIN_SPEED_MAX: SimFixed = SIM_ONE;

/// Maximum allowed combined speed modifier (downhill on road can exceed 1.0).
const COMBINED_MAX: SimFixed = SimFixed::lit("1.2");

/// Minimum combined speed modifier — prevents near-zero crawl.
const COMBINED_MIN: SimFixed = SimFixed::lit("0.3");

/// Number of occupied cells within scan radius before crowd jam kicks in.
const DEFAULT_CROWD_DENSITY_THRESHOLD: u8 = 3;

/// Speed multiplier applied when crowd density exceeds the threshold.
const DEFAULT_CROWD_JAM_FACTOR: SimFixed = SimFixed::lit("0.7");

/// Cell radius around the unit scanned for crowd density.
const CROWD_SCAN_RADIUS: i32 = 2;

/// Configuration for terrain speed modifiers, parsed from [General] in rules.ini.
#[derive(Debug, Clone)]
pub struct TerrainSpeedConfig {
    /// Speed multiplier when moving uphill (next cell higher than current).
    pub slope_climb: SimFixed,
    /// Speed multiplier when moving downhill (next cell lower than current).
    pub slope_descend: SimFixed,
    /// Occupied-cell count before crowd slowdown activates.
    pub crowd_density_threshold: u8,
    /// Speed multiplier when crowd density exceeds the threshold.
    pub crowd_jam_factor: SimFixed,
}

impl Default for TerrainSpeedConfig {
    fn default() -> Self {
        Self {
            slope_climb: SimFixed::lit("0.6"),
            slope_descend: SimFixed::lit("1.2"),
            crowd_density_threshold: DEFAULT_CROWD_DENSITY_THRESHOLD,
            crowd_jam_factor: DEFAULT_CROWD_JAM_FACTOR,
        }
    }
}

impl TerrainSpeedConfig {
    /// Build config from parsed GeneralRules.
    pub fn from_general(slope_climb: SimFixed, slope_descend: SimFixed) -> Self {
        Self {
            slope_climb,
            slope_descend,
            ..Self::default()
        }
    }
}

/// Compute the combined per-cell speed multiplier for a unit moving between cells.
///
/// Returns a SimFixed in [`COMBINED_MIN`, `COMBINED_MAX`] that should be multiplied
/// with the unit's base speed each movement tick.
pub fn compute_cell_speed_modifier(
    speed_type: SpeedType,
    locomotor_kind: LocomotorKind,
    current_cell: (u16, u16),
    next_cell: (u16, u16),
    terrain: &ResolvedTerrainGrid,
    occupancy: &OccupancyGrid,
    config: &TerrainSpeedConfig,
) -> SimFixed {
    let terrain_factor = terrain_speed_factor(speed_type, next_cell, terrain);
    let slope_factor = slope_speed_factor(locomotor_kind, current_cell, next_cell, terrain, config);
    let crowd_factor = crowd_speed_factor(current_cell, occupancy, config);

    let combined = terrain_factor * slope_factor * crowd_factor;
    combined.clamp(COMBINED_MIN, COMBINED_MAX)
}

/// Factor 1: terrain type speed from INI land-type percentages.
///
/// Looks up the *destination* cell's terrain speed for the unit's SpeedType.
/// Matches original engine: 0% → 50%, >100% → clamped to 100%, missing → 100%.
fn terrain_speed_factor(
    speed_type: SpeedType,
    next_cell: (u16, u16),
    terrain: &ResolvedTerrainGrid,
) -> SimFixed {
    let Some(cell) = terrain.cell(next_cell.0, next_cell.1) else {
        return SIM_ONE;
    };
    let multiplier = cell.speed_costs.speed_multiplier_for(speed_type);
    multiplier.clamp(TERRAIN_SPEED_MIN, TERRAIN_SPEED_MAX)
}

/// Factor 2: slope-based speed penalty/bonus from cell height differences.
///
/// Only applies to ground locomotors that interact with terrain grade.
/// Hover/Fly/Jumpjet/Rocket float above the surface and ignore slopes.
fn slope_speed_factor(
    locomotor_kind: LocomotorKind,
    current_cell: (u16, u16),
    next_cell: (u16, u16),
    terrain: &ResolvedTerrainGrid,
    config: &TerrainSpeedConfig,
) -> SimFixed {
    // Airborne and hovering locomotors don't interact with terrain grade.
    if !is_slope_affected(locomotor_kind) {
        return SIM_ONE;
    }

    let cur_level = terrain
        .cell(current_cell.0, current_cell.1)
        .map(|c| c.level)
        .unwrap_or(0);
    let next_level = terrain
        .cell(next_cell.0, next_cell.1)
        .map(|c| c.level)
        .unwrap_or(0);

    if next_level > cur_level {
        config.slope_climb
    } else if next_level < cur_level {
        config.slope_descend
    } else {
        SIM_ONE
    }
}

/// Factor 3: crowd density slowdown when many units occupy nearby cells.
///
/// Counts occupied cells within a small radius. If the count exceeds a
/// threshold, a jam factor is applied to simulate traffic congestion.
fn crowd_speed_factor(
    current_cell: (u16, u16),
    occupancy: &OccupancyGrid,
    config: &TerrainSpeedConfig,
) -> SimFixed {
    let (cx, cy) = (current_cell.0 as i32, current_cell.1 as i32);
    let mut occupied_count: u8 = 0;

    for dy in -CROWD_SCAN_RADIUS..=CROWD_SCAN_RADIUS {
        for dx in -CROWD_SCAN_RADIUS..=CROWD_SCAN_RADIUS {
            if dx == 0 && dy == 0 {
                continue; // don't count the unit's own cell
            }
            let nx = cx + dx;
            let ny = cy + dy;
            if nx < 0 || ny < 0 {
                continue;
            }
            if occupancy.get(nx as u16, ny as u16).is_some() {
                occupied_count = occupied_count.saturating_add(1);
            }
        }
    }

    if occupied_count > config.crowd_density_threshold {
        config.crowd_jam_factor
    } else {
        SIM_ONE
    }
}

/// Whether a locomotor type is affected by terrain slope.
/// Ground movers interact with hills; airborne and hovering ones don't.
fn is_slope_affected(kind: LocomotorKind) -> bool {
    matches!(
        kind,
        LocomotorKind::Drive
            | LocomotorKind::Walk
            | LocomotorKind::Mech
            | LocomotorKind::Ship
            | LocomotorKind::Tunnel
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::terrain_rules::SpeedCostProfile;

    #[test]
    fn speed_multiplier_for_normal_terrain() {
        let profile = SpeedCostProfile {
            foot: Some(100),
            track: Some(100),
            ..Default::default()
        };
        assert_eq!(profile.speed_multiplier_for(SpeedType::Foot), SIM_ONE);
    }

    #[test]
    fn speed_multiplier_for_rough_terrain() {
        let profile = SpeedCostProfile {
            track: Some(75),
            ..Default::default()
        };
        let mult = profile.speed_multiplier_for(SpeedType::Track);
        assert_eq!(mult, SimFixed::lit("0.75"));
    }

    #[test]
    fn speed_multiplier_zero_boosted_to_half() {
        let profile = SpeedCostProfile {
            foot: Some(0),
            ..Default::default()
        };
        assert_eq!(profile.speed_multiplier_for(SpeedType::Foot), SIM_HALF);
    }

    #[test]
    fn speed_multiplier_none_defaults_to_one() {
        let profile = SpeedCostProfile::default();
        assert_eq!(profile.speed_multiplier_for(SpeedType::Foot), SIM_ONE);
    }

    #[test]
    fn crowd_below_threshold_no_slowdown() {
        let config = TerrainSpeedConfig::default();
        let occupancy = OccupancyGrid::new();
        let factor = crowd_speed_factor((5, 5), &occupancy, &config);
        assert_eq!(factor, SIM_ONE);
    }

    #[test]
    fn default_config_values() {
        let config = TerrainSpeedConfig::default();
        assert_eq!(config.slope_climb, SimFixed::lit("0.6"));
        assert_eq!(config.slope_descend, SimFixed::lit("1.2"));
        assert_eq!(config.crowd_density_threshold, 3);
    }
}
