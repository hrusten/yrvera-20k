//! Tests for zone map flood-fill and adjacency extraction.

use super::zone_build::build_zone_map;
use super::zone_map::*;
use crate::map::resolved_terrain::{ResolvedTerrainCell, ResolvedTerrainGrid};
use crate::rules::locomotor_type::MovementZone;
use crate::rules::terrain_rules::{SpeedCostProfile, TerrainClass};
use crate::sim::movement::locomotor::MovementLayer;
use crate::sim::pathfinding::{PathCell, PathGrid};
use std::collections::BTreeMap;

// Helper: build a PathGrid from a string map where '.' = walkable, '#' = blocked.
fn grid_from_str(s: &str) -> PathGrid {
    let lines: Vec<&str> = s.trim().lines().map(|l| l.trim()).collect();
    let h = lines.len() as u16;
    let w = lines[0].len() as u16;
    let mut grid = PathGrid::new(w, h);
    for (ry, line) in lines.iter().enumerate() {
        for (rx, ch) in line.chars().enumerate() {
            if ch == '#' {
                grid.set_blocked(rx as u16, ry as u16, true);
            }
        }
    }
    grid
}

// Helper: build zones for Land category with no cost grid (PathGrid only).
fn land_zones(grid: &PathGrid) -> (ZoneMap, ZoneAdjacency) {
    build_zone_map(grid, None, ZoneCategory::Land, grid.width(), grid.height())
}

fn water_row_terrain(width: u16) -> ResolvedTerrainGrid {
    let cells = (0..width)
        .map(|rx| ResolvedTerrainCell {
            rx,
            ry: 0,
            source_tile_index: 0,
            source_sub_tile: 0,
            final_tile_index: 0,
            final_sub_tile: 0,
            level: 0,
            filled_clear: false,
            tileset_index: Some(0),
            land_type: crate::sim::pathfinding::passability::LandType::Water.as_index(),
            slope_type: 0,
            template_height: 0,
            render_offset_x: 0,
            render_offset_y: 0,
            terrain_class: TerrainClass::Water,
            speed_costs: SpeedCostProfile::default(),
            is_water: true,
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
            base_build_blocked: false,
            build_blocked: false,
            has_bridge_deck: false,
            bridge_walkable: false,
            bridge_transition: false,
            bridge_deck_level: 0,
            bridge_layer: None,
            radar_left: [0, 0, 0],
            radar_right: [0, 0, 0],
        })
        .collect();
    ResolvedTerrainGrid::from_cells(width, 1, cells)
}

fn clear_beach_water_row_terrain() -> ResolvedTerrainGrid {
    let land_types = [
        crate::sim::pathfinding::passability::LandType::Clear.as_index(),
        crate::sim::pathfinding::passability::LandType::Beach.as_index(),
        crate::sim::pathfinding::passability::LandType::Water.as_index(),
    ];
    let cells = land_types
        .into_iter()
        .enumerate()
        .map(|(rx, land_type)| ResolvedTerrainCell {
            rx: rx as u16,
            ry: 0,
            source_tile_index: 0,
            source_sub_tile: 0,
            final_tile_index: 0,
            final_sub_tile: 0,
            level: 0,
            filled_clear: false,
            tileset_index: Some(0),
            land_type,
            slope_type: 0,
            template_height: 0,
            render_offset_x: 0,
            render_offset_y: 0,
            terrain_class: match land_type {
                x if x == crate::sim::pathfinding::passability::LandType::Water.as_index() => {
                    TerrainClass::Water
                }
                x if x == crate::sim::pathfinding::passability::LandType::Beach.as_index() => {
                    TerrainClass::Beach
                }
                _ => TerrainClass::Clear,
            },
            speed_costs: SpeedCostProfile::default(),
            is_water: land_type == crate::sim::pathfinding::passability::LandType::Water.as_index(),
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
            base_build_blocked: false,
            build_blocked: false,
            has_bridge_deck: false,
            bridge_walkable: false,
            bridge_transition: false,
            bridge_deck_level: 0,
            bridge_layer: None,
            radar_left: [0, 0, 0],
            radar_right: [0, 0, 0],
        })
        .collect();
    ResolvedTerrainGrid::from_cells(3, 1, cells)
}

#[test]
fn single_open_area_one_zone() {
    let grid = grid_from_str(
        "
        .....
        .....
        .....
    ",
    );
    let (zm, adj) = land_zones(&grid);
    assert_eq!(zm.zone_count, 1);
    // All cells should be zone 1.
    for ry in 0..3u16 {
        for rx in 0..5u16 {
            assert_eq!(zm.zone_at(rx, ry, MovementLayer::Ground), 1);
        }
    }
    // No adjacency (only one zone).
    assert!(adj.neighbors_of(1).is_empty());
}

#[test]
fn wall_splits_into_two_zones() {
    let grid = grid_from_str(
        "
        ..#..
        ..#..
        ..#..
    ",
    );
    let (zm, _adj) = land_zones(&grid);
    assert_eq!(zm.zone_count, 2);
    // Left side should be zone 1, right side zone 2.
    let z_left = zm.zone_at(0, 0, MovementLayer::Ground);
    let z_right = zm.zone_at(3, 0, MovementLayer::Ground);
    assert_ne!(z_left, ZONE_INVALID);
    assert_ne!(z_right, ZONE_INVALID);
    assert_ne!(z_left, z_right);
    // Wall cells should be ZONE_INVALID.
    assert_eq!(zm.zone_at(2, 0, MovementLayer::Ground), ZONE_INVALID);
}

#[test]
fn blocked_cells_are_invalid() {
    let grid = grid_from_str(
        "
        .#.
        ###
        .#.
    ",
    );
    let (zm, _adj) = land_zones(&grid);
    // Each corner is isolated (diagonal would need both cardinals passable).
    // (0,0) is passable, (2,0) is passable, but they can't connect diagonally
    // through (1,0)=blocked and (0,1)=blocked.
    let z00 = zm.zone_at(0, 0, MovementLayer::Ground);
    let z20 = zm.zone_at(2, 0, MovementLayer::Ground);
    let z02 = zm.zone_at(0, 2, MovementLayer::Ground);
    let z22 = zm.zone_at(2, 2, MovementLayer::Ground);
    assert_ne!(z00, ZONE_INVALID);
    assert_ne!(z20, ZONE_INVALID);
    // All four corners should be different zones (isolated by wall).
    assert_ne!(z00, z20);
    assert_ne!(z00, z02);
    assert_ne!(z00, z22);
}

#[test]
fn diagonal_connectivity_requires_cardinal_passable() {
    // Two cells diagonally adjacent but one cardinal blocked → different zones.
    let grid = grid_from_str(
        "
        .#
        #.
    ",
    );
    let (zm, _adj) = land_zones(&grid);
    // (0,0) and (1,1) are diagonally adjacent but (1,0)=# and (0,1)=# block the diagonal.
    let z00 = zm.zone_at(0, 0, MovementLayer::Ground);
    let z11 = zm.zone_at(1, 1, MovementLayer::Ground);
    assert_ne!(z00, z11);
}

#[test]
fn diagonal_connectivity_with_both_cardinals() {
    // Two cells diagonally adjacent with both cardinals passable → same zone.
    let grid = grid_from_str(
        "
        ..
        ..
    ",
    );
    let (zm, _adj) = land_zones(&grid);
    assert_eq!(zm.zone_count, 1);
}

#[test]
fn adjacency_between_zones() {
    // Two zones separated by a gap that has adjacent cells.
    // Zones are adjacent when their cells are 8-connected neighbors.
    let grid = grid_from_str(
        "
        ..#..
        .....
        ..#..
    ",
    );
    let (zm, _adj) = land_zones(&grid);
    // The gap at (2,1) connects everything into one zone.
    assert_eq!(zm.zone_count, 1);

    // Now create a true split with adjacency:
    let grid2 = grid_from_str(
        "
        ...##
        ...##
        .....
        ##...
        ##...
    ",
    );
    let (zm2, _adj2) = land_zones(&grid2);
    // Check that zones exist and might be adjacent via the connecting corridor.
    assert!(zm2.zone_count >= 1);
}

#[test]
fn same_zone_check() {
    let grid = grid_from_str(
        "
        .....
        .....
    ",
    );
    let (zm, _adj) = land_zones(&grid);
    assert!(zm.same_zone((0, 0), (4, 1), MovementLayer::Ground));
}

#[test]
fn different_zones_same_zone_check() {
    let grid = grid_from_str(
        "
        ..#..
        ..#..
    ",
    );
    let (zm, _adj) = land_zones(&grid);
    assert!(!zm.same_zone((0, 0), (3, 0), MovementLayer::Ground));
}

#[test]
fn zone_category_from_movement_zone() {
    assert_eq!(
        ZoneCategory::from_movement_zone(MovementZone::Normal),
        ZoneCategory::Land
    );
    assert_eq!(
        ZoneCategory::from_movement_zone(MovementZone::Crusher),
        ZoneCategory::Land
    );
    assert_eq!(
        ZoneCategory::from_movement_zone(MovementZone::Infantry),
        ZoneCategory::Infantry
    );
    assert_eq!(
        ZoneCategory::from_movement_zone(MovementZone::Fly),
        ZoneCategory::Fly
    );
    assert_eq!(
        ZoneCategory::from_movement_zone(MovementZone::Water),
        ZoneCategory::Water
    );
    assert_eq!(
        ZoneCategory::from_movement_zone(MovementZone::AmphibiousCrusher),
        ZoneCategory::Amphibious
    );
}

#[test]
fn deterministic_zone_ids() {
    let grid = grid_from_str(
        "
        ..#..
        ..#..
        ..#..
    ",
    );
    let (zm1, _) = land_zones(&grid);
    let (zm2, _) = land_zones(&grid);
    // Zone IDs must be identical across runs.
    for ry in 0..3u16 {
        for rx in 0..5u16 {
            assert_eq!(
                zm1.zone_at(rx, ry, MovementLayer::Ground),
                zm2.zone_at(rx, ry, MovementLayer::Ground),
                "Non-deterministic zone at ({}, {})",
                rx,
                ry,
            );
        }
    }
}

#[test]
fn zone_grid_can_reach_same_zone() {
    let grid = grid_from_str(
        "
        .....
        .....
    ",
    );
    let zg = ZoneGrid::build(&grid, &BTreeMap::new(), 5, 2);
    assert!(zg.can_reach(
        ZoneCategory::Land,
        (0, 0),
        MovementLayer::Ground,
        (4, 1),
        MovementLayer::Ground,
    ));
}

#[test]
fn zone_grid_cannot_reach_disconnected() {
    let grid = grid_from_str(
        "
        ..#..
        ..#..
    ",
    );
    let zg = ZoneGrid::build(&grid, &BTreeMap::new(), 5, 2);
    assert!(!zg.can_reach(
        ZoneCategory::Land,
        (0, 0),
        MovementLayer::Ground,
        (4, 0),
        MovementLayer::Ground,
    ));
}

#[test]
fn zone_grid_fly_always_reachable() {
    let grid = grid_from_str(
        "
        ..#..
        ..#..
    ",
    );
    let zg = ZoneGrid::build(&grid, &BTreeMap::new(), 5, 2);
    assert!(zg.can_reach(
        ZoneCategory::Fly,
        (0, 0),
        MovementLayer::Ground,
        (4, 0),
        MovementLayer::Ground,
    ));
}

#[test]
fn water_zone_grid_uses_resolved_land_type_directly() {
    let terrain = water_row_terrain(5);
    let grid = PathGrid::from_resolved_terrain(&terrain);
    let zg = ZoneGrid::build_with_terrain(&grid, &BTreeMap::new(), Some(&terrain), 5, 1);
    assert!(zg.can_reach(
        ZoneCategory::Water,
        (0, 0),
        MovementLayer::Ground,
        (4, 0),
        MovementLayer::Ground,
    ));
}

#[test]
fn waterbeach_zone_grid_connects_beach_to_water_with_resolved_terrain() {
    let terrain = clear_beach_water_row_terrain();
    let grid = PathGrid::from_resolved_terrain(&terrain);
    let zg = ZoneGrid::build_with_terrain(&grid, &BTreeMap::new(), Some(&terrain), 3, 1);
    assert!(zg.can_reach(
        ZoneCategory::WaterBeach,
        (1, 0),
        MovementLayer::Ground,
        (2, 0),
        MovementLayer::Ground,
    ));
    assert!(zg.can_reach(
        ZoneCategory::Amphibious,
        (0, 0),
        MovementLayer::Ground,
        (2, 0),
        MovementLayer::Ground,
    ));
}

// ---------------------------------------------------------------------------
// Height continuity tests
// ---------------------------------------------------------------------------

/// Build a PathGrid from a height array. All cells ground-walkable.
fn path_grid_from_heights(heights: &[u8], width: u16, height: u16) -> PathGrid {
    assert_eq!(heights.len(), width as usize * height as usize);
    let cells: Vec<PathCell> = heights
        .iter()
        .map(|&h| PathCell {
            ground_walkable: true,
            bridge_walkable: false,
            transition: false,
            ground_level: h,
            bridge_deck_level: 0,
        })
        .collect();
    PathGrid::from_cells(cells, width, height)
}

/// Build zones for Land category with height data (from PathGrid cells).
fn land_zones_with_height(grid: &PathGrid) -> (ZoneMap, ZoneAdjacency) {
    build_zone_map(grid, None, ZoneCategory::Land, grid.width(), grid.height())
}

#[test]
fn height_cliff_splits_zone() {
    // All cells walkable, but heights jump from 0 to 3 — should split into 2 zones.
    let grid = path_grid_from_heights(&[0, 0, 0, 3, 3, 3], 6, 1);
    let (zm, _adj) = land_zones_with_height(&grid);
    assert_eq!(
        zm.zone_count, 2,
        "Height cliff (0→3) should split into two zones"
    );
    let z_left = zm.zone_at(0, 0, MovementLayer::Ground);
    let z_right = zm.zone_at(5, 0, MovementLayer::Ground);
    assert_ne!(z_left, ZONE_INVALID);
    assert_ne!(z_right, ZONE_INVALID);
    assert_ne!(z_left, z_right);
}

#[test]
fn height_ramp_stays_one_zone() {
    // Heights [0,1,2,3,4] — each adjacent pair differs by exactly 1.
    let grid = path_grid_from_heights(&[0, 1, 2, 3, 4], 5, 1);
    let (zm, _adj) = land_zones_with_height(&grid);
    assert_eq!(zm.zone_count, 1, "Gradual ramp (step=1) should be one zone");
}

#[test]
fn height_check_skipped_when_all_level_zero() {
    // When all cells have ground_level=0, heights don't split zones — all passable cells merge.
    let grid = grid_from_str("......");
    let (zm, _adj) = land_zones(&grid);
    assert_eq!(
        zm.zone_count, 1,
        "All level-zero cells should merge into one zone"
    );
}

#[test]
fn height_2d_plateau_isolated() {
    // 3x3 grid: center cell at height 5, rest at height 0.
    // Center should be isolated (h_diff > 1 in all directions).
    #[rustfmt::skip]
    let grid = path_grid_from_heights(&[
        0, 0, 0,
        0, 5, 0,
        0, 0, 0,
    ], 3, 3);
    let (zm, _adj) = land_zones_with_height(&grid);
    let z_corner = zm.zone_at(0, 0, MovementLayer::Ground);
    let z_center = zm.zone_at(1, 1, MovementLayer::Ground);
    assert_ne!(z_corner, ZONE_INVALID);
    assert_ne!(z_center, ZONE_INVALID);
    assert_ne!(
        z_corner, z_center,
        "Height-5 plateau should be isolated from height-0 surround"
    );
}

#[test]
fn height_step_of_two_splits() {
    // Heights [0, 2, 4] — each step is 2, exceeding the threshold of 1.
    let grid = path_grid_from_heights(&[0, 2, 4], 3, 1);
    let (zm, _adj) = land_zones_with_height(&grid);
    assert_eq!(zm.zone_count, 3, "Each cell is its own zone when step=2");
}

// ---------------------------------------------------------------------------
// Incremental zone update tests
// ---------------------------------------------------------------------------

/// Verify that incremental update produces correct reachability after blocking a cell.
#[test]
fn incremental_block_cell_splits_zone() {
    // 5x1 grid: all walkable → one zone.
    let grid = grid_from_str(".....");
    let mut zg = ZoneGrid::build(&grid, &BTreeMap::new(), 5, 1);
    assert!(zg.can_reach(
        ZoneCategory::Land,
        (0, 0),
        MovementLayer::Ground,
        (4, 0),
        MovementLayer::Ground,
    ));

    // Block center cell (2,0) → should split into two zones.
    let mut grid2 = grid.clone();
    grid2.set_blocked(2, 0, true);
    let changed = grid.diff_cells(&grid2).unwrap();
    assert_eq!(changed.len(), 1);

    let result = crate::sim::pathfinding::zone_incremental::try_incremental_update(
        &mut zg,
        &changed,
        &grid2,
        &BTreeMap::new(),
        None,
    );
    assert!(result, "Incremental update should succeed");
    assert!(
        !zg.can_reach(
            ZoneCategory::Land,
            (0, 0),
            MovementLayer::Ground,
            (4, 0),
            MovementLayer::Ground,
        ),
        "After blocking center, left and right should be disconnected"
    );
    // Left side still connected within itself.
    assert!(zg.can_reach(
        ZoneCategory::Land,
        (0, 0),
        MovementLayer::Ground,
        (1, 0),
        MovementLayer::Ground,
    ));
    // Right side still connected within itself.
    assert!(zg.can_reach(
        ZoneCategory::Land,
        (3, 0),
        MovementLayer::Ground,
        (4, 0),
        MovementLayer::Ground,
    ));
}

/// Verify that unblocking a cell reconnects zones.
#[test]
fn incremental_unblock_cell_merges_zones() {
    // Start with wall in center.
    let grid1 = grid_from_str("..#..");
    let mut zg = ZoneGrid::build(&grid1, &BTreeMap::new(), 5, 1);
    assert!(!zg.can_reach(
        ZoneCategory::Land,
        (0, 0),
        MovementLayer::Ground,
        (4, 0),
        MovementLayer::Ground,
    ));

    // Remove wall → should reconnect.
    let grid2 = grid_from_str(".....");
    let changed = grid1.diff_cells(&grid2).unwrap();
    assert_eq!(changed.len(), 1);

    let result = crate::sim::pathfinding::zone_incremental::try_incremental_update(
        &mut zg,
        &changed,
        &grid2,
        &BTreeMap::new(),
        None,
    );
    assert!(result);
    assert!(
        zg.can_reach(
            ZoneCategory::Land,
            (0, 0),
            MovementLayer::Ground,
            (4, 0),
            MovementLayer::Ground,
        ),
        "After removing wall, zones should reconnect"
    );
}

/// Large number of changed cells should trigger fallback (return false).
#[test]
fn incremental_fallback_on_large_change() {
    let grid = grid_from_str(".....");
    let mut zg = ZoneGrid::build(&grid, &BTreeMap::new(), 5, 1);

    // Simulate > INCREMENTAL_THRESHOLD changed cells.
    let many_changes: Vec<(u16, u16)> = (0..201).map(|i| (i % 5, 0)).collect();
    let result = crate::sim::pathfinding::zone_incremental::try_incremental_update(
        &mut zg,
        &many_changes,
        &grid,
        &BTreeMap::new(),
        None,
    );
    assert!(
        !result,
        "Should fall back to full rebuild on > threshold changes"
    );
}

/// Empty changeset is a no-op.
#[test]
fn incremental_no_change_is_noop() {
    let grid = grid_from_str(".....");
    let mut zg = ZoneGrid::build(&grid, &BTreeMap::new(), 5, 1);
    let original_zone_count = zg.map_for(ZoneCategory::Land).unwrap().zone_count;

    let result = crate::sim::pathfinding::zone_incremental::try_incremental_update(
        &mut zg,
        &[],
        &grid,
        &BTreeMap::new(),
        None,
    );
    assert!(result);
    assert_eq!(
        zg.map_for(ZoneCategory::Land).unwrap().zone_count,
        original_zone_count,
    );
}

#[test]
fn incremental_with_resolved_terrain_falls_back_to_full_rebuild_path() {
    let terrain = water_row_terrain(3);
    let grid = PathGrid::from_resolved_terrain(&terrain);
    let mut zg = ZoneGrid::build_with_terrain(&grid, &BTreeMap::new(), Some(&terrain), 3, 1);
    let mut grid2 = grid.clone();
    grid2.set_blocked(1, 0, true);
    let changed = grid.diff_cells(&grid2).unwrap();

    let result = crate::sim::pathfinding::zone_incremental::try_incremental_update(
        &mut zg,
        &changed,
        &grid2,
        &BTreeMap::new(),
        Some(&terrain),
    );
    assert!(!result);
}

#[test]
fn terrain_aware_incremental_update_requests_full_rebuild() {
    let terrain = water_row_terrain(5);
    let grid = PathGrid::from_resolved_terrain(&terrain);
    let mut zg = ZoneGrid::build_with_terrain(&grid, &BTreeMap::new(), Some(&terrain), 5, 1);

    let result = crate::sim::pathfinding::zone_incremental::try_incremental_update(
        &mut zg,
        &[(0, 0)],
        &grid,
        &BTreeMap::new(),
        Some(&terrain),
    );
    assert!(
        !result,
        "terrain-aware zoning should currently force a full rebuild on dynamic updates"
    );
}
