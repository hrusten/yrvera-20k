//! Tests for zone-aware pathfinding wrappers.

use super::zone_search::*;
use super::zone_map::ZoneGrid;
use crate::rules::locomotor_type::MovementZone;
use crate::sim::pathfinding::PathGrid;
use std::collections::{BTreeMap, BTreeSet};

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

#[test]
fn zoned_path_reachable_returns_path() {
    let grid = grid_from_str("
        .....
        .....
        .....
    ");
    let zg = ZoneGrid::build(&grid, None, &BTreeMap::new(), 5, 3);
    let path = find_path_zoned(
        &grid, (0, 0), (4, 2), None, None,
        Some(&zg), MovementZone::Normal, None, None, 0,
    );
    assert!(path.is_some());
    let path = path.unwrap();
    assert_eq!(*path.first().unwrap(), (0, 0));
    assert_eq!(*path.last().unwrap(), (4, 2));
}

#[test]
fn zoned_path_unreachable_returns_none_instantly() {
    let grid = grid_from_str("
        ..#..
        ..#..
        ..#..
    ");
    let zg = ZoneGrid::build(&grid, None, &BTreeMap::new(), 5, 3);
    // (0,0) and (4,0) are in different disconnected zones.
    let path = find_path_zoned(
        &grid, (0, 0), (4, 0), None, None,
        Some(&zg), MovementZone::Normal, None, None, 0,
    );
    assert!(path.is_none());
}

#[test]
fn zoned_path_no_zone_grid_falls_through() {
    let grid = grid_from_str("
        .....
        .....
    ");
    // Without zone grid, should just run normal A*.
    let path = find_path_zoned(
        &grid, (0, 0), (4, 1), None, None,
        None, MovementZone::Normal, None, None, 0,
    );
    assert!(path.is_some());
}

#[test]
fn zoned_path_same_cell() {
    let grid = grid_from_str("
        .....
    ");
    let zg = ZoneGrid::build(&grid, None, &BTreeMap::new(), 5, 1);
    let path = find_path_zoned(
        &grid, (2, 0), (2, 0), None, None,
        Some(&zg), MovementZone::Normal, None, None, 0,
    );
    assert!(path.is_some());
    assert_eq!(path.unwrap(), vec![(2, 0)]);
}

#[test]
fn zoned_path_entity_blocks_respected() {
    let grid = grid_from_str("
        ...
        ...
        ...
    ");
    let zg = ZoneGrid::build(&grid, None, &BTreeMap::new(), 3, 3);
    // Block the direct path with entities.
    let mut blocks = BTreeSet::new();
    blocks.insert((1, 0));
    blocks.insert((1, 1));
    blocks.insert((1, 2));
    // Zone says reachable (static terrain is connected), but entities block.
    // A* should still find no path since the wall of entities cuts off (2,x).
    let path = find_path_zoned(
        &grid, (0, 0), (2, 0), None, Some(&blocks),
        Some(&zg), MovementZone::Normal, None, None, 0,
    );
    // Path exists because goal cell is always reachable even if entity-blocked.
    // But the path would need to go around — with a 3x3 grid fully blocked
    // in column 1, there's no way around.
    assert!(path.is_none());
}
