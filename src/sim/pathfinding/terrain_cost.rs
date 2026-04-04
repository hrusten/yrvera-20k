//! Per-SpeedType terrain cost grid for variable-cost A* pathfinding.
//!
//! Each cell has a speed modifier (0 = blocked, 100 = normal, <100 = slow terrain).
//! The A* pathfinder multiplies its step cost by `100 / cost_at(x,y)` so that
//! slower terrain costs more and the planner routes around it.
//!
//! ## Design
//! `TerrainCostGrid` is built from map data + a `SpeedType` and provides the
//! cost lookup that `find_path_with_costs()` uses. It is separate from `PathGrid`
//! (which is boolean walkability) to keep the fast path working for simple queries.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on map/ (MapCell, TilesetLookup).
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use super::passability;
use crate::map::map_file::MapCell;
use crate::map::resolved_terrain::ResolvedTerrainGrid;
use crate::map::theater::TilesetLookup;
use crate::rules::locomotor_type::SpeedType;

/// Normal terrain speed — no bonus, no penalty.
const COST_NORMAL: u8 = 100;
/// Rough terrain — slower movement for tracked/wheeled.
const COST_ROUGH: u8 = 75;
/// Blocked / impassable for this SpeedType.
const COST_BLOCKED: u8 = 0;

/// Per-cell speed modifier grid for one SpeedType.
///
/// Values: 0 = blocked, 100 = normal speed, <100 = slow terrain.
/// Built once per SpeedType from map data. The A* planner reads this to weight
/// step costs, making units avoid rough terrain.
#[derive(Debug, Clone)]
pub struct TerrainCostGrid {
    costs: Vec<u8>,
    width: u16,
    height: u16,
}

impl TerrainCostGrid {
    /// Build a terrain cost grid from map cell data for a given SpeedType.
    ///
    /// Uses the theater's `TilesetLookup` to classify tiles by their SetName.
    /// Cells outside the map or with no tile data get `COST_BLOCKED`.
    pub fn build(
        cells: &[MapCell],
        lookup: Option<&TilesetLookup>,
        speed_type: SpeedType,
        map_width: u16,
        map_height: u16,
    ) -> Self {
        let size: usize = map_width as usize * map_height as usize;
        let mut costs: Vec<u8> = vec![COST_BLOCKED; size];

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

            let is_water: bool = lookup.map_or(false, |l| l.is_water(tile_id));
            let is_cliff: bool = lookup.map_or(false, |l| l.is_cliff(tile_id));
            let set_name: Option<&str> = lookup.and_then(|l| l.set_name(tile_id));
            let is_rough: bool = set_name
                .map(|n| n.to_ascii_lowercase().contains("rough"))
                .unwrap_or(false);
            let is_road: bool = set_name
                .map(|n| {
                    let lower = n.to_ascii_lowercase();
                    lower.contains("road") || lower.contains("pavement")
                })
                .unwrap_or(false);

            let cost: u8 = classify_terrain_cost(speed_type, is_water, is_cliff, is_rough, is_road);
            let idx: usize = cell.ry as usize * map_width as usize + cell.rx as usize;
            costs[idx] = cost;
        }

        Self {
            costs,
            width: map_width,
            height: map_height,
        }
    }

    /// Build a terrain cost grid from resolved terrain metadata.
    ///
    /// Uses INI speed costs as the primary terrain check (from rules.ini land-type
    /// sections like [Clear], [Tiberium], etc.). Falls back to the passability
    /// matrix for cells without INI speed data. This ordering is critical because
    /// the passability matrix marks Tiberium as PASS_BLOCKED for ground zones
    /// (zone flood-fill semantics), but ore/gem cells are passable per the INI
    /// speed table ([Tiberium] Track=70%, Foot=90%, etc.).
    pub fn from_resolved_terrain(terrain: &ResolvedTerrainGrid, speed_type: SpeedType) -> Self {
        let size: usize = terrain.width() as usize * terrain.height() as usize;
        let mut costs: Vec<u8> = vec![COST_BLOCKED; size];

        for cell in terrain.iter() {
            let idx: usize = cell.ry as usize * terrain.width() as usize + cell.rx as usize;
            if idx >= costs.len() {
                continue;
            }
            let hard_blocked =
                cell.is_cliff_like || cell.overlay_blocks || cell.terrain_object_blocks;
            // Bridge deck overrides underlying terrain (water/cliff) for ground units.
            // Units walk on the bridge surface, not the terrain below.
            let cost = if cell.has_bridge_deck && !cell.overlay_blocks {
                COST_NORMAL
            } else if hard_blocked {
                COST_BLOCKED
            } else if let Some(resolved) = cell.speed_costs.cost_for_speed_type(speed_type) {
                // INI speed costs are the primary source — they come from rules.ini
                // [Clear], [Rough], [Tiberium], etc. sections and encode the actual
                // speed percentage per SpeedType. 0 = blocked, >0 = passable.
                // This must be checked BEFORE the passability matrix because the
                // matrix marks Tiberium as PASS_BLOCKED for ground zones (used for
                // zone flood-fill), but ore/gem cells ARE passable per the INI table.
                resolved
            } else if !passability::is_passable_for_speed_type(cell.land_type, speed_type) {
                // Fallback: passability matrix for cells without INI speed data.
                COST_BLOCKED
            } else {
                classify_terrain_cost(
                    speed_type,
                    cell.is_water,
                    cell.ground_walk_blocked,
                    cell.is_rough,
                    cell.is_road,
                )
            };
            costs[idx] = cost;
        }

        Self {
            costs,
            width: terrain.width(),
            height: terrain.height(),
        }
    }

    /// Get the speed modifier for a cell (0 = blocked, 100 = normal).
    pub fn cost_at(&self, x: u16, y: u16) -> u8 {
        if x >= self.width || y >= self.height {
            return COST_BLOCKED;
        }
        self.costs[y as usize * self.width as usize + x as usize]
    }

    /// Map width in cells.
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Map height in cells.
    pub fn height(&self) -> u16 {
        self.height
    }
}

/// Determine the terrain cost for a cell given its SpeedType and tile classification.
///
/// Roads use uniform cost (same as clear terrain), matching the original engine's
/// A* behavior where all passable cells have equal pathfinding weight.
fn classify_terrain_cost(
    speed_type: SpeedType,
    is_water: bool,
    is_cliff: bool,
    is_rough: bool,
    _is_road: bool,
) -> u8 {
    match speed_type {
        SpeedType::Foot => {
            if is_water || is_cliff {
                COST_BLOCKED
            } else if is_rough {
                // Infantry handle rough terrain better than vehicles.
                90
            } else {
                COST_NORMAL
            }
        }
        SpeedType::Track => {
            if is_water || is_cliff {
                COST_BLOCKED
            } else if is_rough {
                COST_ROUGH
            } else {
                COST_NORMAL
            }
        }
        SpeedType::Wheel => {
            if is_water || is_cliff {
                COST_BLOCKED
            } else if is_rough {
                // Wheeled vehicles are even slower on rough terrain.
                60
            } else {
                COST_NORMAL
            }
        }
        SpeedType::Float | SpeedType::FloatBeach | SpeedType::Hover => {
            // Hover/float units can cross water.
            if is_cliff { COST_BLOCKED } else { COST_NORMAL }
        }
        SpeedType::Amphibious => {
            // Amphibious units cross both land and water.
            if is_cliff { COST_BLOCKED } else { COST_NORMAL }
        }
        SpeedType::Winged => {
            // Aircraft ignore all terrain.
            COST_NORMAL
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::resolved_terrain::ResolvedTerrainCell;
    use crate::map::resolved_terrain::ResolvedTerrainGrid;
    use crate::rules::terrain_rules::{SpeedCostProfile, TerrainClass};

    #[test]
    fn test_track_on_water_is_blocked() {
        let cost = classify_terrain_cost(SpeedType::Track, true, false, false, false);
        assert_eq!(cost, COST_BLOCKED);
    }

    #[test]
    fn test_track_on_road_is_normal() {
        let cost = classify_terrain_cost(SpeedType::Track, false, false, false, true);
        assert_eq!(cost, COST_NORMAL);
    }

    #[test]
    fn test_track_on_rough_is_slow() {
        let cost = classify_terrain_cost(SpeedType::Track, false, false, true, false);
        assert_eq!(cost, COST_ROUGH);
    }

    #[test]
    fn test_float_on_water_is_passable() {
        let cost = classify_terrain_cost(SpeedType::Float, true, false, false, false);
        assert_eq!(cost, COST_NORMAL);
    }

    #[test]
    fn test_winged_ignores_terrain() {
        assert_eq!(
            classify_terrain_cost(SpeedType::Winged, true, true, true, false),
            COST_NORMAL
        );
    }

    #[test]
    fn test_foot_on_rough_is_less_penalized() {
        let foot = classify_terrain_cost(SpeedType::Foot, false, false, true, false);
        let track = classify_terrain_cost(SpeedType::Track, false, false, true, false);
        assert!(
            foot > track,
            "Infantry handle rough terrain better than vehicles"
        );
    }

    #[test]
    fn test_wheel_on_rough_is_most_penalized() {
        let wheel = classify_terrain_cost(SpeedType::Wheel, false, false, true, false);
        let track = classify_terrain_cost(SpeedType::Track, false, false, true, false);
        assert!(
            wheel < track,
            "Wheeled vehicles suffer more on rough terrain"
        );
    }

    #[test]
    fn test_from_resolved_terrain_uses_resolved_surface_classes() {
        use crate::sim::pathfinding::passability::LandType;
        let terrain = ResolvedTerrainGrid::from_cells(
            2,
            2,
            vec![
                ResolvedTerrainCell {
                    is_road: true,
                    land_type: LandType::Road.as_index(),
                    speed_costs: SpeedCostProfile {
                        track: Some(120),
                        ..SpeedCostProfile::default()
                    },
                    ..make_resolved_cell(0, 0)
                },
                ResolvedTerrainCell {
                    is_rough: true,
                    land_type: LandType::Rough.as_index(),
                    speed_costs: SpeedCostProfile {
                        track: Some(75),
                        wheel: Some(60),
                        foot: Some(90),
                        ..SpeedCostProfile::default()
                    },
                    ..make_resolved_cell(1, 0)
                },
                ResolvedTerrainCell {
                    is_water: true,
                    land_type: LandType::Water.as_index(),
                    ground_walk_blocked: true,
                    speed_costs: SpeedCostProfile {
                        track: Some(0),
                        hover: Some(100),
                        ..SpeedCostProfile::default()
                    },
                    ..make_resolved_cell(0, 1)
                },
                ResolvedTerrainCell {
                    is_cliff_like: true,
                    land_type: LandType::Rock.as_index(),
                    ground_walk_blocked: true,
                    ..make_resolved_cell(1, 1)
                },
            ],
        );
        let track = TerrainCostGrid::from_resolved_terrain(&terrain, SpeedType::Track);
        let hover = TerrainCostGrid::from_resolved_terrain(&terrain, SpeedType::Hover);
        // Road cell uses INI speed cost (Track=120) — no road bonus in A*.
        assert_eq!(track.cost_at(0, 0), 120);
        assert_eq!(track.cost_at(1, 0), COST_ROUGH);
        assert_eq!(track.cost_at(0, 1), COST_BLOCKED);
        assert_eq!(hover.cost_at(0, 1), COST_NORMAL);
        assert_eq!(hover.cost_at(1, 1), COST_BLOCKED);
    }

    fn make_resolved_cell(rx: u16, ry: u16) -> ResolvedTerrainCell {
        ResolvedTerrainCell {
            rx,
            ry,
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
            ground_walk_blocked: false,
            terrain_object_blocks: false,
            overlay_blocks: false,
            zone_type: 0,
            base_ground_walk_blocked: false,
            base_build_blocked: false,
            build_blocked: false,
            has_bridge_deck: false,
            bridge_walkable: false,
            bridge_transition: false,
            bridge_deck_level: 0,
            bridge_layer: None,
            radar_left: [0, 0, 0],
            radar_right: [0, 0, 0],
        }
    }
}
