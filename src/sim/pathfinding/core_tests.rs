//! Tests for A* pathfinding and PathGrid walkability.
//!
//! Extracted from pathfinding.rs to stay under the 400-line limit.

use super::*;
use crate::map::map_file::MapCell;
use crate::map::resolved_terrain::{ResolvedTerrainCell, ResolvedTerrainGrid};
use crate::rules::locomotor_type::SpeedType;
use crate::rules::terrain_rules::{SpeedCostProfile, TerrainClass};
use crate::sim::bridge_state::{BridgeDamageEvent, BridgeRuntimeState};
use crate::sim::movement::locomotor::MovementLayer;
use crate::sim::pathfinding::terrain_cost::TerrainCostGrid;

#[test]
fn test_path_grid_new_all_walkable() {
    let grid: PathGrid = PathGrid::new(10, 10);
    assert!(grid.is_walkable(0, 0));
    assert!(grid.is_walkable(9, 9));
    assert!(grid.is_walkable(5, 5));
}

#[test]
fn test_path_grid_out_of_bounds() {
    let grid: PathGrid = PathGrid::new(10, 10);
    assert!(!grid.is_walkable(10, 0));
    assert!(!grid.is_walkable(0, 10));
    assert!(!grid.is_walkable(255, 255));
}

#[test]
fn test_path_grid_set_blocked() {
    let mut grid: PathGrid = PathGrid::new(10, 10);
    grid.set_blocked(3, 4, true);
    assert!(!grid.is_walkable(3, 4));
    // Unblock it again.
    grid.set_blocked(3, 4, false);
    assert!(grid.is_walkable(3, 4));
}

#[test]
fn test_octile_heuristic_cardinal() {
    // Straight horizontal: 5 steps × 10 = 50.
    let h: i32 = octile_heuristic(0, 0, 5, 0);
    assert_eq!(h, 50);
}

#[test]
fn test_octile_heuristic_diagonal() {
    // Pure diagonal: 3 steps × 14 = 42.
    let h: i32 = octile_heuristic(0, 0, 3, 3);
    assert_eq!(h, 42);
}

#[test]
fn test_octile_heuristic_mixed() {
    // dx=5, dy=3: 3 diagonal + 2 cardinal = 3*14 + 2*10 = 62.
    let h: i32 = octile_heuristic(0, 0, 5, 3);
    assert_eq!(h, 62);
}

#[test]
fn test_find_path_trivial_same_cell() {
    let grid: PathGrid = PathGrid::new(10, 10);
    let path: Option<Vec<(u16, u16)>> = find_path(&grid, (5, 5), (5, 5));
    assert_eq!(path, Some(vec![(5, 5)]));
}

#[test]
fn test_find_path_straight_line() {
    let grid: PathGrid = PathGrid::new(10, 10);
    let path: Option<Vec<(u16, u16)>> = find_path(&grid, (0, 0), (4, 0));
    let path: Vec<(u16, u16)> = path.expect("Should find a path on open grid");
    // Path should start at (0,0) and end at (4,0).
    assert_eq!(*path.first().expect("non-empty"), (0, 0));
    assert_eq!(*path.last().expect("non-empty"), (4, 0));
    // Should be 5 cells (0,0) through (4,0).
    assert_eq!(path.len(), 5);
}

#[test]
fn test_find_path_diagonal() {
    let grid: PathGrid = PathGrid::new(10, 10);
    let path: Option<Vec<(u16, u16)>> = find_path(&grid, (0, 0), (3, 3));
    let path: Vec<(u16, u16)> = path.expect("Should find diagonal path");
    assert_eq!(*path.first().expect("non-empty"), (0, 0));
    assert_eq!(*path.last().expect("non-empty"), (3, 3));
    // Pure diagonal: exactly 4 cells.
    assert_eq!(path.len(), 4);
}

#[test]
fn test_layered_path_cell_bridge_helpers() {
    let bridge_cell = PathCell {
        ground_walkable: true,
        bridge_walkable: true,
        transition: true,
        ground_level: 1,
        bridge_deck_level: 4,
    };
    assert!(bridge_cell.is_bridge_transition_cell());
    assert!(bridge_cell.is_elevated_bridge_cell());
    assert_eq!(bridge_cell.bridge_deck_level_if_any(), Some(4));
    assert_eq!(
        bridge_cell.effective_cell_z_for_layer(MovementLayer::Bridge),
        4
    );
    assert_eq!(
        bridge_cell.effective_cell_z_for_layer(MovementLayer::Ground),
        1
    );
    assert!(bridge_cell.can_enter_bridge_layer_from_ground());

    let low_bridge = PathCell {
        ground_walkable: true,
        bridge_walkable: true,
        transition: false,
        ground_level: 2,
        bridge_deck_level: 2,
    };
    assert!(!low_bridge.is_bridge_transition_cell());
    assert!(!low_bridge.is_elevated_bridge_cell());
    assert_eq!(low_bridge.bridge_deck_level_if_any(), Some(2));
    assert_eq!(
        low_bridge.effective_cell_z_for_layer(MovementLayer::Bridge),
        2
    );
    assert!(!low_bridge.can_enter_bridge_layer_from_ground());
}

#[test]
fn test_find_path_around_obstacle() {
    let mut grid: PathGrid = PathGrid::new(10, 10);
    // Build a wall from (3,0) to (3,8), leaving a gap at (3,9).
    for y in 0..9 {
        grid.set_blocked(3, y, true);
    }
    let path: Option<Vec<(u16, u16)>> = find_path(&grid, (1, 5), (5, 5));
    let path: Vec<(u16, u16)> = path.expect("Should find path around wall");
    assert_eq!(*path.first().expect("non-empty"), (1, 5));
    assert_eq!(*path.last().expect("non-empty"), (5, 5));
    // Path must avoid column 3, rows 0-8.
    for &(x, y) in &path {
        if x == 3 {
            assert!(y >= 9, "Path must not cross blocked cells at (3,{})", y);
        }
    }
}

#[test]
fn test_find_path_no_path_exists() {
    let mut grid: PathGrid = PathGrid::new(10, 10);
    // Completely wall off the right side.
    for y in 0..10 {
        grid.set_blocked(5, y, true);
    }
    let path: Option<Vec<(u16, u16)>> = find_path(&grid, (0, 0), (9, 9));
    assert!(path.is_none(), "Should find no path through complete wall");
}

#[test]
fn test_find_path_blocked_start() {
    let mut grid: PathGrid = PathGrid::new(10, 10);
    grid.set_blocked(0, 0, true);
    let path: Option<Vec<(u16, u16)>> = find_path(&grid, (0, 0), (5, 5));
    assert!(path.is_none(), "Blocked start should return None");
}

#[test]
fn test_find_path_blocked_goal() {
    let mut grid: PathGrid = PathGrid::new(10, 10);
    grid.set_blocked(5, 5, true);
    let path: Option<Vec<(u16, u16)>> = find_path(&grid, (0, 0), (5, 5));
    assert!(path.is_none(), "Blocked goal should return None");
}

#[test]
fn test_find_path_no_diagonal_corner_cutting() {
    let mut grid: PathGrid = PathGrid::new(5, 5);
    // Block (1,0) and (0,1) — diagonal (1,1) should not be reachable
    // directly from (0,0) because both adjacent cardinals are blocked.
    grid.set_blocked(1, 0, true);
    grid.set_blocked(0, 1, true);
    let path: Option<Vec<(u16, u16)>> = find_path(&grid, (0, 0), (1, 1));
    // Path should be None since (0,0) is completely boxed in (corners blocked).
    assert!(path.is_none(), "Should not cut through diagonal corners");
}

#[test]
fn test_path_grid_dimensions() {
    let grid: PathGrid = PathGrid::new(80, 70);
    assert_eq!(grid.width(), 80);
    assert_eq!(grid.height(), 70);
}

#[test]
fn test_from_map_data_marks_terrain_walkable() {
    let cells: Vec<MapCell> = vec![
        MapCell {
            rx: 2,
            ry: 3,
            tile_index: 0,
            sub_tile: 0,
            z: 0,
        },
        MapCell {
            rx: 4,
            ry: 5,
            tile_index: 1,
            sub_tile: 0,
            z: 0,
        },
    ];
    let grid: PathGrid = PathGrid::from_map_data(&cells, None, 10, 10);
    // Cells with terrain should be walkable.
    assert!(grid.is_walkable(2, 3));
    assert!(grid.is_walkable(4, 5));
    // Cells without terrain should be blocked (all start blocked).
    assert!(!grid.is_walkable(0, 0));
    assert!(!grid.is_walkable(9, 9));
}

#[test]
fn test_from_map_data_skips_no_tile() {
    let cells: Vec<MapCell> = vec![
        MapCell {
            rx: 1,
            ry: 1,
            tile_index: -1,
            sub_tile: 0,
            z: 0,
        },
        MapCell {
            rx: 2,
            ry: 2,
            tile_index: 5,
            sub_tile: 0,
            z: 0,
        },
    ];
    let grid: PathGrid = PathGrid::from_map_data(&cells, None, 10, 10);
    assert!(!grid.is_walkable(1, 1), "No-tile cells should be blocked");
    assert!(
        grid.is_walkable(2, 2),
        "Valid tile cells should be walkable"
    );
}

#[test]
fn test_block_building_footprint() {
    let cells: Vec<MapCell> = (0..10u16)
        .flat_map(|rx| {
            (0..10u16).map(move |ry| MapCell {
                rx,
                ry,
                tile_index: 0,
                sub_tile: 0,
                z: 0,
            })
        })
        .collect();
    let mut grid: PathGrid = PathGrid::from_map_data(&cells, None, 10, 10);
    // All cells should be walkable initially.
    assert!(grid.is_walkable(3, 3));
    assert!(grid.is_walkable(4, 4));
    // Block a 2x2 building at (3, 3).
    grid.block_building_footprint(3, 3, "2x2");
    assert!(!grid.is_walkable(3, 3));
    assert!(!grid.is_walkable(4, 3));
    assert!(!grid.is_walkable(3, 4));
    assert!(!grid.is_walkable(4, 4));
    // Adjacent cells remain walkable.
    assert!(grid.is_walkable(2, 3));
    assert!(grid.is_walkable(5, 3));
}

#[test]
fn test_from_resolved_terrain_uses_resolved_blocking_flags() {
    let terrain = ResolvedTerrainGrid::from_cells(
        2,
        2,
        vec![
            make_resolved_cell(0, 0),
            ResolvedTerrainCell {
                is_water: true,
                ground_walk_blocked: true,
                ..make_resolved_cell(1, 0)
            },
            ResolvedTerrainCell {
                terrain_object_blocks: true,
                build_blocked: true,
                ..make_resolved_cell(0, 1)
            },
            ResolvedTerrainCell {
                overlay_blocks: true,
                build_blocked: true,
                ..make_resolved_cell(1, 1)
            },
        ],
    );
    let grid = PathGrid::from_resolved_terrain(&terrain);
    assert!(grid.is_walkable(0, 0), "Clear land is walkable");
    // Water cells are PathGrid-walkable — SpeedType-dependent blocking
    // is handled by TerrainCostGrid (cost=0 blocks ground units in A*).
    assert!(grid.is_walkable(1, 0), "Water is PathGrid-walkable");
    assert!(!grid.is_walkable(0, 1), "Terrain object blocks all units");
    assert!(!grid.is_walkable(1, 1), "Overlay blocks all units");
}

#[test]
fn test_parse_foundation() {
    assert_eq!(parse_foundation("2x2"), (2, 2));
    assert_eq!(parse_foundation("3x3"), (3, 3));
    assert_eq!(parse_foundation("1x1"), (1, 1));
    assert_eq!(parse_foundation("4x2"), (4, 2));
    assert_eq!(parse_foundation(""), (1, 1));
    assert_eq!(parse_foundation("custom"), (1, 1));
}

#[test]
fn test_layered_path_transitions_onto_bridge_and_stays_on_deck() {
    // Height-based bridge routing requires deck_level - ground_level >= 2
    // for the pathfinder to recognize bridge cells as "at bridge level".
    // Use realistic heights: ground=0, deck=4.
    let terrain = ResolvedTerrainGrid::from_cells(
        5,
        1,
        vec![
            ResolvedTerrainCell {
                level: 4,
                ..make_resolved_cell(0, 0)
            },
            ResolvedTerrainCell {
                bridge_walkable: true,
                bridge_transition: true,
                bridge_deck_level: 4,
                has_bridge_deck: true,
                ..make_resolved_cell(1, 0)
            },
            ResolvedTerrainCell {
                ground_walk_blocked: true,
                build_blocked: true,
                bridge_walkable: true,
                bridge_deck_level: 4,
                has_bridge_deck: true,
                is_water: true,
                ..make_resolved_cell(2, 0)
            },
            ResolvedTerrainCell {
                bridge_walkable: true,
                bridge_transition: true,
                bridge_deck_level: 4,
                has_bridge_deck: true,
                ..make_resolved_cell(3, 0)
            },
            ResolvedTerrainCell {
                level: 4,
                ..make_resolved_cell(4, 0)
            },
        ],
    );
    let grid = PathGrid::from_resolved_terrain(&terrain);
    let path = find_layered_path(
        &grid,
        None,
        None,
        (0, 0),
        MovementLayer::Ground,
        (4, 0),
        None,
        None,
    )
    .expect("bridge path should exist");

    assert_eq!(path.first().map(|step| (step.rx, step.ry)), Some((0, 0)));
    assert_eq!(path.last().map(|step| (step.rx, step.ry)), Some((4, 0)));
    assert!(path.len() >= 2, "path should have at least start and goal");
}

#[test]
fn test_layered_path_stays_on_ground_when_bridge_not_needed() {
    let terrain = ResolvedTerrainGrid::from_cells(
        3,
        1,
        vec![
            make_resolved_cell(0, 0),
            make_resolved_cell(1, 0),
            make_resolved_cell(2, 0),
        ],
    );
    let grid = PathGrid::from_resolved_terrain(&terrain);
    let path = find_layered_path(
        &grid,
        None,
        None,
        (0, 0),
        MovementLayer::Ground,
        (2, 0),
        None,
        None,
    )
    .expect("ground path should exist");
    assert!(path.iter().all(|step| step.layer == MovementLayer::Ground));
}

#[test]
fn test_layered_path_rebuild_blocks_destroyed_bridge_deck() {
    // Height-based routing needs deck - ground >= 2 to recognize bridge level.
    // Use 5 cells: land(h=4) → transition → water+bridge → transition → land(h=4)
    let terrain = ResolvedTerrainGrid::from_cells(
        5,
        1,
        vec![
            ResolvedTerrainCell {
                level: 4,
                ..make_resolved_cell(0, 0)
            },
            ResolvedTerrainCell {
                bridge_walkable: true,
                bridge_transition: true,
                bridge_deck_level: 4,
                has_bridge_deck: true,
                ..make_resolved_cell(1, 0)
            },
            ResolvedTerrainCell {
                ground_walk_blocked: true,
                build_blocked: true,
                base_build_blocked: true,
                bridge_walkable: true,
                bridge_deck_level: 4,
                has_bridge_deck: true,
                is_water: true,
                ..make_resolved_cell(2, 0)
            },
            ResolvedTerrainCell {
                bridge_walkable: true,
                bridge_transition: true,
                bridge_deck_level: 4,
                has_bridge_deck: true,
                ..make_resolved_cell(3, 0)
            },
            ResolvedTerrainCell {
                level: 4,
                ..make_resolved_cell(4, 0)
            },
        ],
    );
    let mut bridge_state = BridgeRuntimeState::from_resolved_terrain(&terrain, true, 10);
    let intact_grid = PathGrid::from_resolved_terrain_with_bridges(&terrain, Some(&bridge_state));
    assert!(
        find_layered_path(
            &intact_grid,
            None,
            None,
            (0, 0),
            MovementLayer::Ground,
            (4, 0),
            None,
            None
        )
        .is_some(),
        "intact bridge should be traversable"
    );

    let change = bridge_state
        .apply_damage(BridgeDamageEvent {
            rx: 1,
            ry: 0,
            damage: 10,
        })
        .expect("bridge group should be destroyed");
    assert!(
        change.destroyed_cells.len() >= 1,
        "at least one bridge cell should be destroyed"
    );

    let destroyed_grid =
        PathGrid::from_resolved_terrain_with_bridges(&terrain, Some(&bridge_state));
    assert!(
        find_layered_path(
            &destroyed_grid,
            None,
            None,
            (0, 0),
            MovementLayer::Ground,
            (4, 0),
            None,
            None
        )
        .is_none(),
        "destroyed bridge should invalidate the layered route"
    );
}

// --- Bridge height helper tests ---

#[test]
fn test_is_at_bridge_level_no_bridge() {
    let cell = PathCell {
        ground_walkable: true,
        bridge_walkable: false,
        transition: false,
        ground_level: 0,
        bridge_deck_level: 0,
    };
    // Non-bridge cell is never "at bridge level"
    assert!(!is_at_bridge_level(0, &cell));
    assert!(!is_at_bridge_level(4, &cell));
}

#[test]
fn test_is_at_bridge_level_ground_near() {
    let cell = PathCell {
        ground_walkable: true,
        bridge_walkable: true,
        transition: false,
        ground_level: 0,
        bridge_deck_level: 4,
    };
    // path_height=0, ground=0 -> diff=0 < 2 -> ground list
    assert!(!is_at_bridge_level(0, &cell));
    // path_height=1, ground=0 -> diff=1 < 2 -> ground list
    assert!(!is_at_bridge_level(1, &cell));
}

#[test]
fn test_is_at_bridge_level_bridge_far() {
    let cell = PathCell {
        ground_walkable: true,
        bridge_walkable: true,
        transition: false,
        ground_level: 0,
        bridge_deck_level: 4,
    };
    // path_height=4, ground=0 -> diff=4 >= 2 -> bridge list
    assert!(is_at_bridge_level(4, &cell));
    // path_height=2, ground=0 -> diff=2 >= 2 -> bridge list
    assert!(is_at_bridge_level(2, &cell));
}

#[test]
fn test_compute_neighbor_height_no_bridge() {
    let parent = PathCell {
        ground_walkable: true,
        bridge_walkable: false,
        transition: false,
        ground_level: 2,
        bridge_deck_level: 0,
    };
    let neighbor = PathCell {
        ground_walkable: true,
        bridge_walkable: false,
        transition: false,
        ground_level: 3,
        bridge_deck_level: 0,
    };
    // Case 1: neighbor not bridge -> ground_level
    assert_eq!(compute_neighbor_height(2, &parent, &neighbor), 3);
}

#[test]
fn test_compute_neighbor_height_parent_on_bridge_deck() {
    let parent = PathCell {
        ground_walkable: true,
        bridge_walkable: true,
        transition: false,
        ground_level: 0,
        bridge_deck_level: 4,
    };
    let neighbor = PathCell {
        ground_walkable: true,
        bridge_walkable: true,
        transition: false,
        ground_level: 0,
        bridge_deck_level: 4,
    };
    // Case 2a: parent on bridge at deck level -> stay on bridge
    assert_eq!(compute_neighbor_height(4, &parent, &neighbor), 4);
}

#[test]
fn test_compute_neighbor_height_parent_under_bridge() {
    let parent = PathCell {
        ground_walkable: true,
        bridge_walkable: true,
        transition: false,
        ground_level: 0,
        bridge_deck_level: 4,
    };
    let neighbor = PathCell {
        ground_walkable: true,
        bridge_walkable: true,
        transition: false,
        ground_level: 0,
        bridge_deck_level: 4,
    };
    // Case 2b: parent on bridge cell but at ground level -> stay under
    assert_eq!(compute_neighbor_height(0, &parent, &neighbor), 0);
}

#[test]
fn test_compute_neighbor_height_ramp_up() {
    let parent = PathCell {
        ground_walkable: true,
        bridge_walkable: false,
        transition: false,
        ground_level: 4,
        bridge_deck_level: 0,
    };
    let neighbor = PathCell {
        ground_walkable: true,
        bridge_walkable: true,
        transition: true,
        ground_level: 0,
        bridge_deck_level: 4,
    };
    // Case 3: parent not bridge, neighbor is bridge,
    // diff = 4 - 0 = 4, in [2,4] -> ramp up to bridge deck
    assert_eq!(compute_neighbor_height(4, &parent, &neighbor), 4);
}

#[test]
fn test_compute_neighbor_height_pass_under() {
    let parent = PathCell {
        ground_walkable: true,
        bridge_walkable: false,
        transition: false,
        ground_level: 0,
        bridge_deck_level: 0,
    };
    let neighbor = PathCell {
        ground_walkable: true,
        bridge_walkable: true,
        transition: false,
        ground_level: 0,
        bridge_deck_level: 4,
    };
    // Case 3: parent not bridge, neighbor is bridge,
    // diff = 0 - 0 = 0, NOT in [2,4] -> pass under
    assert_eq!(compute_neighbor_height(0, &parent, &neighbor), 0);
}

#[test]
fn test_compute_neighbor_height_extreme_diff_no_ramp() {
    let parent = PathCell {
        ground_walkable: true,
        bridge_walkable: false,
        transition: false,
        ground_level: 8,
        bridge_deck_level: 0,
    };
    let neighbor = PathCell {
        ground_walkable: true,
        bridge_walkable: true,
        transition: true,
        ground_level: 0,
        bridge_deck_level: 4,
    };
    // Case 3: diff = 8 - 0 = 8, NOT in [2,4] -> stays at ground (no ramp)
    assert_eq!(compute_neighbor_height(8, &parent, &neighbor), 0);
}

#[test]
fn test_encode_decode_from_ground() {
    let encoded = encode_from(1234, false);
    let (idx, bridge) = decode_from(encoded);
    assert_eq!(idx, 1234);
    assert!(!bridge);
}

#[test]
fn test_encode_decode_from_bridge() {
    let encoded = encode_from(1234, true);
    let (idx, bridge) = decode_from(encoded);
    assert_eq!(idx, 1234);
    assert!(bridge);
}

#[test]
fn test_encode_decode_from_max_map() {
    // 512x512 = 262144 cells — must not collide with bridge bit (1 << 20 = 1048576)
    let encoded = encode_from(262143, true);
    let (idx, bridge) = decode_from(encoded);
    assert_eq!(idx, 262143);
    assert!(bridge);
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

// ---------------------------------------------------------------------------
// Entity-block (friendly-passable) pathfinding tests
// ---------------------------------------------------------------------------

#[test]
fn test_entity_blocks_routes_around_blocked_cell() {
    let grid = PathGrid::new(10, 10);
    let mut blocks: BTreeSet<(u16, u16)> = BTreeSet::new();
    blocks.insert((3, 0)); // Block cell (3,0) — directly on the straight path.

    let path = find_path_with_costs(&grid, (0, 0), (5, 0), None, Some(&blocks), None, None, None);
    assert!(path.is_some(), "Should find a path around the entity block");
    let path = path.unwrap();
    assert!(
        !path.contains(&(3, 0)),
        "Path should not go through entity-blocked cell"
    );
    assert_eq!(path.last(), Some(&(5, 0)), "Path should reach goal");
}

#[test]
fn test_entity_blocks_goal_cell_still_reachable() {
    // Goal cell is in entity_blocks — path should still reach it.
    let grid = PathGrid::new(10, 10);
    let mut blocks: BTreeSet<(u16, u16)> = BTreeSet::new();
    blocks.insert((5, 0)); // Block the GOAL cell.

    let path = find_path_with_costs(&grid, (0, 0), (5, 0), None, Some(&blocks), None, None, None);
    assert!(
        path.is_some(),
        "Goal cell should always be reachable even if entity-blocked"
    );
    assert_eq!(path.unwrap().last(), Some(&(5, 0)));
}

#[test]
fn test_entity_blocks_empty_set_same_as_none() {
    let grid = PathGrid::new(10, 10);
    let empty: BTreeSet<(u16, u16)> = BTreeSet::new();

    let path_none = find_path_with_costs(&grid, (0, 0), (5, 5), None, None, None, None, None);
    let path_empty =
        find_path_with_costs(&grid, (0, 0), (5, 5), None, Some(&empty), None, None, None);
    assert_eq!(path_none, path_empty);
}

#[test]
fn test_entity_blocks_fully_surrounded_no_path() {
    let grid = PathGrid::new(10, 10);
    let mut blocks: BTreeSet<(u16, u16)> = BTreeSet::new();
    // Surround (5,5) on all 8 sides.
    for dx in -1..=1i32 {
        for dy in -1..=1i32 {
            if dx == 0 && dy == 0 {
                continue;
            }
            blocks.insert(((5i32 + dx) as u16, (5i32 + dy) as u16));
        }
    }
    let path = find_path_with_costs(&grid, (0, 0), (5, 5), None, Some(&blocks), None, None, None);
    // Goal itself is reachable, but all approaches blocked → no path.
    assert!(
        path.is_none(),
        "Should not find path when all approaches to goal are entity-blocked"
    );
}

// ---------------------------------------------------------------------------
// Path truncation tests (RA2 24-step segment system)
// ---------------------------------------------------------------------------

#[test]
fn test_truncate_path_shorter_than_limit_unchanged() {
    let path: Vec<(u16, u16)> = vec![(0, 0), (1, 0), (2, 0)]; // 2 steps
    let result = truncate_path(path.clone(), 24);
    assert_eq!(result, path, "Short path should be returned unchanged");
}

#[test]
fn test_truncate_path_exactly_at_limit() {
    // 24 steps = 25 entries (start + 24 moves).
    let path: Vec<(u16, u16)> = (0..25).map(|i| (i as u16, 0)).collect();
    let result = truncate_path(path.clone(), 24);
    assert_eq!(result, path, "Path at exact limit should be unchanged");
}

#[test]
fn test_truncate_path_longer_than_limit() {
    // 30 steps = 31 entries → should be truncated to 25 entries (24 steps).
    let path: Vec<(u16, u16)> = (0..31).map(|i| (i as u16, 0)).collect();
    let result = truncate_path(path, 24);
    assert_eq!(
        result.len(),
        25,
        "Should truncate to 25 entries (24 steps + start)"
    );
    assert_eq!(result[0], (0, 0));
    assert_eq!(result[24], (24, 0));
}

#[test]
fn test_truncate_layered_path_truncates_both_vecs() {
    let path: Vec<(u16, u16)> = (0..31).map(|i| (i as u16, 0)).collect();
    let layers: Vec<MovementLayer> = vec![MovementLayer::Ground; 31];
    let (rp, rl) = truncate_layered_path(path, layers, 24);
    assert_eq!(rp.len(), 25);
    assert_eq!(rl.len(), 25);
    assert_eq!(rp[24], (24, 0));
}

#[test]
fn test_truncate_path_single_cell() {
    let path: Vec<(u16, u16)> = vec![(5, 5)]; // 0 steps
    let result = truncate_path(path.clone(), 24);
    assert_eq!(result, path);
}

#[test]
fn test_find_path_long_gets_full_result() {
    // A* itself returns the full path — truncation happens at the caller level.
    let grid = PathGrid::new(50, 1);
    let path = find_path(&grid, (0, 0), (40, 0)).expect("should find path on open grid");
    assert_eq!(path.len(), 41, "A* should return the complete 40-step path");
}

// ---------------------------------------------------------------------------
// Water / naval pathfinding tests
// ---------------------------------------------------------------------------

/// Helper: build a resolved terrain grid with a water channel across the middle.
/// Layout (7 wide × 3 tall):
///   Row 0: land  land  land  land  land  land  land
///   Row 1: water water water water water water water
///   Row 2: land  land  land  land  land  land  land
fn make_water_channel_terrain() -> ResolvedTerrainGrid {
    let width: u16 = 7;
    let height: u16 = 3;
    let mut cells: Vec<ResolvedTerrainCell> = Vec::new();
    for ry in 0..height {
        for rx in 0..width {
            let is_water: bool = ry == 1;
            cells.push(ResolvedTerrainCell {
                is_water,
                ground_walk_blocked: is_water,
                terrain_class: if is_water {
                    TerrainClass::Water
                } else {
                    TerrainClass::Clear
                },
                speed_costs: if is_water {
                    // Water: ground blocked, Float/Hover/Amphibious passable.
                    SpeedCostProfile {
                        foot: Some(0),
                        track: Some(0),
                        wheel: Some(0),
                        float: Some(100),
                        amphibious: Some(100),
                        hover: Some(100),
                        float_beach: Some(100),
                    }
                } else {
                    // Land: ground passable, Float blocked.
                    SpeedCostProfile {
                        foot: Some(100),
                        track: Some(100),
                        wheel: Some(100),
                        float: Some(0),
                        amphibious: Some(80),
                        hover: Some(50),
                        float_beach: Some(0),
                    }
                },
                ..make_resolved_cell(rx, ry)
            });
        }
    }
    ResolvedTerrainGrid::from_cells(width, height, cells)
}

#[test]
fn test_water_cells_are_pathgrid_walkable() {
    let terrain: ResolvedTerrainGrid = make_water_channel_terrain();
    let grid: PathGrid = PathGrid::from_resolved_terrain(&terrain);
    // Water cells should be walkable in PathGrid (passability is SpeedType-dependent).
    assert!(
        grid.is_walkable(0, 1),
        "Water cell should be PathGrid-walkable"
    );
    assert!(
        grid.is_walkable(3, 1),
        "Water cell should be PathGrid-walkable"
    );
    // Land cells still walkable.
    assert!(grid.is_walkable(0, 0), "Land cell should be walkable");
}

#[test]
fn test_cliff_cells_remain_pathgrid_blocked() {
    let terrain: ResolvedTerrainGrid = ResolvedTerrainGrid::from_cells(
        3,
        1,
        vec![
            make_resolved_cell(0, 0),
            ResolvedTerrainCell {
                is_cliff_like: true,
                ..make_resolved_cell(1, 0)
            },
            make_resolved_cell(2, 0),
        ],
    );
    let grid: PathGrid = PathGrid::from_resolved_terrain(&terrain);
    assert!(grid.is_walkable(0, 0));
    assert!(!grid.is_walkable(1, 0), "Cliff cell must remain blocked");
    assert!(grid.is_walkable(2, 0));
}

#[test]
fn test_float_unit_pathfinds_through_water() {
    let terrain: ResolvedTerrainGrid = make_water_channel_terrain();
    let grid: PathGrid = PathGrid::from_resolved_terrain(&terrain);
    let float_costs: TerrainCostGrid =
        TerrainCostGrid::from_resolved_terrain(&terrain, SpeedType::Float);

    // Float unit paths along the water channel (row 1).
    let path = find_path_with_costs(
        &grid,
        (0, 1),
        (6, 1),
        Some(&float_costs),
        None,
        None,
        None,
        None,
    );
    assert!(path.is_some(), "Float unit should pathfind through water");
    let path: Vec<(u16, u16)> = path.unwrap();
    assert_eq!(path.first(), Some(&(0, 1)));
    assert_eq!(path.last(), Some(&(6, 1)));
    // All cells should be on the water row.
    for &(_, ry) in &path {
        assert_eq!(ry, 1, "Float path should stay on water row");
    }
}

#[test]
fn test_track_unit_cannot_pathfind_through_water() {
    let terrain: ResolvedTerrainGrid = make_water_channel_terrain();
    let grid: PathGrid = PathGrid::from_resolved_terrain(&terrain);
    let track_costs: TerrainCostGrid =
        TerrainCostGrid::from_resolved_terrain(&terrain, SpeedType::Track);

    // Track unit trying to cross from land (0,0) to land (6,2) — must go around water.
    // But with a full water channel blocking, there is no path.
    let path = find_path_with_costs(
        &grid,
        (0, 0),
        (6, 2),
        Some(&track_costs),
        None,
        None,
        None,
        None,
    );
    assert!(
        path.is_none(),
        "Track unit cannot cross water channel — no path should exist"
    );
}

#[test]
fn test_amphibious_unit_crosses_land_water_land() {
    let terrain: ResolvedTerrainGrid = make_water_channel_terrain();
    let grid: PathGrid = PathGrid::from_resolved_terrain(&terrain);
    let amphi_costs: TerrainCostGrid =
        TerrainCostGrid::from_resolved_terrain(&terrain, SpeedType::Amphibious);

    // Amphibious unit crosses from land (0,0) through water (row 1) to land (0,2).
    let path = find_path_with_costs(
        &grid,
        (0, 0),
        (0, 2),
        Some(&amphi_costs),
        None,
        None,
        None,
        None,
    );
    assert!(path.is_some(), "Amphibious unit should cross water channel");
    let path: Vec<(u16, u16)> = path.unwrap();
    assert_eq!(path.first(), Some(&(0, 0)));
    assert_eq!(path.last(), Some(&(0, 2)));
    // Path should go through water row 1.
    assert!(
        path.contains(&(0, 1)),
        "Amphibious path should traverse the water row"
    );
}

#[test]
fn test_ground_unit_no_diagonal_through_water() {
    // 4×3 grid with water blocking the direct diagonal from (1,0) to (2,1).
    // Water at (2,0) and (1,1) — foot unit at (1,0) must not diagonal to (2,1).
    //
    // Layout:
    //   (0,0)L  (1,0)L  (2,0)W  (3,0)L
    //   (0,1)L  (1,1)W  (2,1)L  (3,1)L
    //   (0,2)L  (1,2)L  (2,2)L  (3,2)L
    let water_cost: SpeedCostProfile = SpeedCostProfile {
        foot: Some(0),
        track: Some(0),
        wheel: Some(0),
        float: Some(100),
        ..SpeedCostProfile::default()
    };
    let terrain: ResolvedTerrainGrid = ResolvedTerrainGrid::from_cells(
        4,
        3,
        vec![
            make_resolved_cell(0, 0),
            make_resolved_cell(1, 0), // start
            ResolvedTerrainCell {
                is_water: true,
                ground_walk_blocked: true,
                speed_costs: water_cost,
                ..make_resolved_cell(2, 0)
            },
            make_resolved_cell(3, 0),
            make_resolved_cell(0, 1),
            ResolvedTerrainCell {
                is_water: true,
                ground_walk_blocked: true,
                speed_costs: water_cost,
                ..make_resolved_cell(1, 1)
            },
            make_resolved_cell(2, 1), // goal
            make_resolved_cell(3, 1),
            make_resolved_cell(0, 2),
            make_resolved_cell(1, 2),
            make_resolved_cell(2, 2),
            make_resolved_cell(3, 2),
        ],
    );
    let grid: PathGrid = PathGrid::from_resolved_terrain(&terrain);
    let foot_costs: TerrainCostGrid =
        TerrainCostGrid::from_resolved_terrain(&terrain, SpeedType::Foot);

    let path = find_path_with_costs(
        &grid,
        (1, 0),
        (2, 1),
        Some(&foot_costs),
        None,
        None,
        None,
        None,
    );
    assert!(path.is_some(), "Foot unit should find a path around water");
    let path: Vec<(u16, u16)> = path.unwrap();
    // The direct diagonal (1,0)→(2,1) is blocked because both cardinal
    // neighbors (2,0) and (1,1) are water (cost=0). Path must go around.
    assert!(
        !path.windows(2).any(|w| w[0] == (1, 0) && w[1] == (2, 1)),
        "Foot unit must not diagonal-cut through water cells"
    );
}

#[test]
fn test_ground_units_on_land_no_regression() {
    // Basic sanity: ground pathfinding on land still works correctly.
    let grid: PathGrid = PathGrid::new(10, 10);
    let path = find_path(&grid, (0, 0), (5, 5));
    assert!(
        path.is_some(),
        "Ground pathfinding on open land must still work"
    );
    let path: Vec<(u16, u16)> = path.unwrap();
    assert_eq!(path.first(), Some(&(0, 0)));
    assert_eq!(path.last(), Some(&(5, 5)));
}

// --- nearest_walkable tests ---

#[test]
fn test_nearest_walkable_already_walkable() {
    let grid: PathGrid = PathGrid::new(10, 10);
    let result = grid.nearest_walkable(5, 5, 3, None, None);
    assert_eq!(result, Some((5, 5)));
}

#[test]
fn test_nearest_walkable_blocked_center() {
    let mut grid: PathGrid = PathGrid::new(10, 10);
    grid.set_blocked(5, 5, true);
    let result = grid.nearest_walkable(5, 5, 3, None, None);
    assert!(result.is_some());
    let (rx, ry) = result.unwrap();
    // Must be adjacent (distance 1) since surrounding cells are walkable.
    let dx = (rx as i32 - 5).abs();
    let dy = (ry as i32 - 5).abs();
    assert!(
        dx <= 1 && dy <= 1,
        "Expected adjacent cell, got ({},{})",
        rx,
        ry
    );
    assert!(grid.is_walkable(rx, ry));
}

#[test]
fn test_nearest_walkable_building_footprint() {
    // 3x3 building at (4,4) — block cells (4,4) through (6,6).
    let mut grid: PathGrid = PathGrid::new(10, 10);
    for dy in 0..3u16 {
        for dx in 0..3u16 {
            grid.set_blocked(4 + dx, 4 + dy, true);
        }
    }
    // Searching from center of building (5,5) should find a cell outside.
    let result = grid.nearest_walkable(5, 5, 5, None, None);
    assert!(result.is_some());
    let (rx, ry) = result.unwrap();
    assert!(grid.is_walkable(rx, ry));
}

#[test]
fn test_nearest_walkable_with_entity_blocks() {
    let grid: PathGrid = PathGrid::new(10, 10);
    let mut blocks: BTreeSet<(u16, u16)> = BTreeSet::new();
    blocks.insert((5, 5));
    blocks.insert((5, 4)); // block north neighbor too
    let result = grid.nearest_walkable(5, 5, 3, Some(&blocks), None);
    assert!(result.is_some());
    let (rx, ry) = result.unwrap();
    assert_ne!((rx, ry), (5, 5));
    assert_ne!((rx, ry), (5, 4));
    assert!(grid.is_walkable(rx, ry));
}

#[test]
fn test_nearest_walkable_no_walkable_in_radius() {
    // Tiny grid entirely blocked.
    let mut grid: PathGrid = PathGrid::new(3, 3);
    for y in 0..3u16 {
        for x in 0..3u16 {
            grid.set_blocked(x, y, true);
        }
    }
    let result = grid.nearest_walkable(1, 1, 5, None, None);
    assert_eq!(result, None);
}

#[test]
fn test_height_based_bridge_routing_deck_at_4() {
    // 5x1 grid: land(h=4) → transition(g=0,d=4) → bridge(g=0,d=4) → transition(g=0,d=4) → land(h=4)
    let terrain = ResolvedTerrainGrid::from_cells(
        5,
        1,
        vec![
            ResolvedTerrainCell {
                level: 4,
                ..make_resolved_cell(0, 0)
            },
            ResolvedTerrainCell {
                level: 0,
                bridge_walkable: true,
                bridge_transition: true,
                bridge_deck_level: 4,
                has_bridge_deck: true,
                ..make_resolved_cell(1, 0)
            },
            ResolvedTerrainCell {
                level: 0,
                ground_walk_blocked: true,
                is_water: true,
                bridge_walkable: true,
                bridge_deck_level: 4,
                has_bridge_deck: true,
                ..make_resolved_cell(2, 0)
            },
            ResolvedTerrainCell {
                level: 0,
                bridge_walkable: true,
                bridge_transition: true,
                bridge_deck_level: 4,
                has_bridge_deck: true,
                ..make_resolved_cell(3, 0)
            },
            ResolvedTerrainCell {
                level: 4,
                ..make_resolved_cell(4, 0)
            },
        ],
    );
    let grid = PathGrid::from_resolved_terrain(&terrain);
    let path = find_layered_path(
        &grid,
        None,
        None,
        (0, 0),
        MovementLayer::Ground,
        (4, 0),
        None,
        None,
    )
    .expect("path across bridge should exist");

    assert_eq!(path.first().map(|s| (s.rx, s.ry)), Some((0, 0)));
    assert_eq!(path.last().map(|s| (s.rx, s.ry)), Some((4, 0)));

    // Middle cells (bridge span) should be on Bridge layer since
    // start height=4, bridge ground_level=0, diff=4 >= 2 → bridge list
    let bridge_steps: Vec<_> = path.iter().filter(|s| s.layer == MovementLayer::Bridge).collect();
    assert!(
        !bridge_steps.is_empty(),
        "Bridge cells should route on Bridge layer with height diff >= 2"
    );
}

#[test]
fn test_cliff_cost_uses_effective_height_not_ground_level() {
    // Verify: bridge deck (effective 4) → land (ground 4) has NO cliff penalty.
    // Old behavior: skipped cliff on bridge layer. New: compares node heights.
    // node.height=4 (deck), neighbor_height=4 (land ground_level) → equal → no penalty.
    let terrain = ResolvedTerrainGrid::from_cells(
        3,
        1,
        vec![
            ResolvedTerrainCell {
                level: 0,
                bridge_walkable: true,
                bridge_transition: true,
                bridge_deck_level: 4,
                has_bridge_deck: true,
                ..make_resolved_cell(0, 0)
            },
            ResolvedTerrainCell {
                level: 0,
                bridge_walkable: true,
                bridge_deck_level: 4,
                has_bridge_deck: true,
                ..make_resolved_cell(1, 0)
            },
            ResolvedTerrainCell {
                level: 4,
                ..make_resolved_cell(2, 0)
            },
        ],
    );
    let grid = PathGrid::from_resolved_terrain(&terrain);
    // Path from bridge start to land at same effective height
    let path = find_layered_path(
        &grid,
        None,
        None,
        (0, 0),
        MovementLayer::Bridge,
        (2, 0),
        None,
        None,
    );
    // Should find a path (no false cliff penalty blocking it)
    assert!(
        path.is_some(),
        "Bridge(deck=4) to land(ground=4) should not have false cliff penalty"
    );
}
