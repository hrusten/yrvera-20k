//! Group destination distribution — assigns unique cells to units in a group move.
//!
//! When multiple units are selected and given a move order, this module distributes
//! destinations via radial ring expansion so each unit gets a unique nearby cell
//! instead of all converging on the same point. This matches RA2's behavior where
//! the clicked cell is the center of a destination area and each unit is assigned
//! a unique cell near the click point.
//!
//! Vehicles get one cell each. Infantry pack up to 3 per cell (matching RA2's
//! 3-functional-sub-cell model). Vehicles are assigned first so infantry can
//! fill remaining cells without wasting vehicle-suitable positions.
//!
//! ## Dependency rules
//! - Part of sim/ — depends only on sim/pathfinding (PathGrid), sim/bump_crush (constants).
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use std::collections::{BTreeMap, BTreeSet};

use crate::sim::movement::bump_crush::MAX_INFANTRY_PER_CELL;
use crate::sim::pathfinding::PathGrid;

/// Maximum radius (in cells) for spreading unit destinations around the click point.
/// A radius of 12 covers a diamond of ~312 cells — enough for any realistic selection.
const MAX_GROUP_SPREAD_RADIUS: u16 = 12;

/// Distribute unique destination cells for a group of units moving to a common target.
///
/// Vehicles get one-per-cell. Infantry pack up to 3 per cell (sub-cell sharing).
/// The center cell is assigned to the first unit. Remaining units get cells from
/// expanding rings outward from center.
///
/// `vehicle_ids` and `infantry_ids` must be sorted for deterministic output.
/// Returns `Vec<(entity_id, rx, ry)>` with one entry per input unit.
pub fn distribute_group_destinations(
    grid: &PathGrid,
    center: (u16, u16),
    vehicle_ids: &[u64],
    infantry_ids: &[u64],
) -> Vec<(u64, u16, u16)> {
    let mut assignments: Vec<(u64, u16, u16)> =
        Vec::with_capacity(vehicle_ids.len() + infantry_ids.len());

    // Cells fully claimed by a vehicle (no infantry or other vehicles allowed).
    let mut claimed_vehicle: BTreeSet<(u16, u16)> = BTreeSet::new();
    // Infantry count per cell (max MAX_INFANTRY_PER_CELL before moving to next cell).
    let mut infantry_counts: BTreeMap<(u16, u16), usize> = BTreeMap::new();

    // Pre-collect ring cells for reuse across both vehicle and infantry passes.
    let ring_cells: Vec<(u16, u16)> =
        collect_ring_cells(center, MAX_GROUP_SPREAD_RADIUS, grid.width(), grid.height());

    // --- Assign vehicles first (one per cell) ---
    let mut ring_iter_idx: usize = 0;
    for &vid in vehicle_ids {
        let cell: (u16, u16) = find_next_vehicle_cell(
            &ring_cells,
            &mut ring_iter_idx,
            grid,
            &claimed_vehicle,
            &infantry_counts,
        )
        .unwrap_or(center);
        claimed_vehicle.insert(cell);
        assignments.push((vid, cell.0, cell.1));
    }

    // --- Assign infantry (up to 3 per cell) ---
    let mut inf_ring_idx: usize = 0;
    // Track the current cell being packed with infantry.
    let mut current_inf_cell: Option<(u16, u16)> = None;

    for &iid in infantry_ids {
        // Check if the current cell still has room.
        let has_room: bool = current_inf_cell.is_some_and(|cell| {
            let count: usize = infantry_counts.get(&cell).copied().unwrap_or(0);
            count < MAX_INFANTRY_PER_CELL && !claimed_vehicle.contains(&cell)
        });

        let cell: (u16, u16) = if has_room {
            current_inf_cell.unwrap()
        } else {
            // Find the next cell that infantry can use.
            let next: (u16, u16) = find_next_infantry_cell(
                &ring_cells,
                &mut inf_ring_idx,
                grid,
                &claimed_vehicle,
                &infantry_counts,
            )
            .unwrap_or(center);
            current_inf_cell = Some(next);
            next
        };

        *infantry_counts.entry(cell).or_insert(0) += 1;
        assignments.push((iid, cell.0, cell.1));
    }

    assignments
}

/// Find the next unclaimed walkable cell for a vehicle.
/// Advances `start_idx` past checked cells so subsequent calls continue the scan.
fn find_next_vehicle_cell(
    ring_cells: &[(u16, u16)],
    start_idx: &mut usize,
    grid: &PathGrid,
    claimed_vehicle: &BTreeSet<(u16, u16)>,
    infantry_counts: &BTreeMap<(u16, u16), usize>,
) -> Option<(u16, u16)> {
    while *start_idx < ring_cells.len() {
        let cell: (u16, u16) = ring_cells[*start_idx];
        *start_idx += 1;
        if grid.is_any_layer_walkable(cell.0, cell.1)
            && !claimed_vehicle.contains(&cell)
            && !infantry_counts.contains_key(&cell)
        {
            return Some(cell);
        }
    }
    None
}

/// Find the next cell where infantry can be placed (walkable, not vehicle-claimed,
/// has room for at least one more infantry).
fn find_next_infantry_cell(
    ring_cells: &[(u16, u16)],
    start_idx: &mut usize,
    grid: &PathGrid,
    claimed_vehicle: &BTreeSet<(u16, u16)>,
    infantry_counts: &BTreeMap<(u16, u16), usize>,
) -> Option<(u16, u16)> {
    while *start_idx < ring_cells.len() {
        let cell: (u16, u16) = ring_cells[*start_idx];
        *start_idx += 1;
        if grid.is_any_layer_walkable(cell.0, cell.1) && !claimed_vehicle.contains(&cell)
        {
            let count: usize = infantry_counts.get(&cell).copied().unwrap_or(0);
            if count < MAX_INFANTRY_PER_CELL {
                return Some(cell);
            }
        }
    }
    None
}

/// Collect cells in expanding rings from `center`, yielding (rx, ry) in deterministic order.
///
/// Ring 0 = center itself. Ring r = perimeter cells at Chebyshev distance r.
/// Perimeter order: top edge L→R, right edge T→B, bottom edge R→L, left edge B→T.
/// This matches the ring pattern used in `nearest_walkable_cell`.
fn collect_ring_cells(
    center: (u16, u16),
    max_radius: u16,
    grid_width: u16,
    grid_height: u16,
) -> Vec<(u16, u16)> {
    let cx: i32 = center.0 as i32;
    let cy: i32 = center.1 as i32;
    let w: i32 = grid_width as i32;
    let h: i32 = grid_height as i32;

    // Estimate capacity: ring 0 = 1, ring r = 8*r, total ≈ 4*r^2.
    let cap: usize = (4 * max_radius as usize * max_radius as usize).min(1024) + 1;
    let mut cells: Vec<(u16, u16)> = Vec::with_capacity(cap);

    // Ring 0: center cell.
    if cx >= 0 && cx < w && cy >= 0 && cy < h {
        cells.push(center);
    }

    for r in 1..=max_radius as i32 {
        let min_x: i32 = (cx - r).max(0);
        let max_x: i32 = (cx + r).min(w - 1);
        let min_y: i32 = (cy - r).max(0);
        let max_y: i32 = (cy + r).min(h - 1);

        // Top edge: left to right (y = min_y, x = min_x..=max_x).
        if cy - r >= 0 {
            for x in min_x..=max_x {
                cells.push((x as u16, min_y as u16));
            }
        }

        // Right edge: top to bottom, excluding corners (x = max_x, y = min_y+1..max_y).
        if cx + r < w {
            for y in (min_y + 1)..max_y {
                cells.push((max_x as u16, y as u16));
            }
        }

        // Bottom edge: right to left (y = max_y, x = max_x..=min_x reversed).
        if cy + r < h {
            for x in (min_x..=max_x).rev() {
                cells.push((x as u16, max_y as u16));
            }
        }

        // Left edge: bottom to top, excluding corners (x = min_x, y = max_y-1..min_y+1 reversed).
        if cx - r >= 0 {
            for y in ((min_y + 1)..max_y).rev() {
                cells.push((min_x as u16, y as u16));
            }
        }
    }

    cells
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_vehicle_gets_center() {
        let grid: PathGrid = PathGrid::new(20, 20);
        let result = distribute_group_destinations(&grid, (10, 10), &[1], &[]);
        assert_eq!(result, vec![(1, 10, 10)]);
    }

    #[test]
    fn test_two_vehicles_get_unique_cells() {
        let grid: PathGrid = PathGrid::new(20, 20);
        let result = distribute_group_destinations(&grid, (10, 10), &[1, 2], &[]);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], (1, 10, 10), "First vehicle gets center");
        assert_ne!(
            (result[1].1, result[1].2),
            (10, 10),
            "Second vehicle gets a different cell"
        );
    }

    #[test]
    fn test_three_infantry_share_center() {
        let grid: PathGrid = PathGrid::new(20, 20);
        let result = distribute_group_destinations(&grid, (10, 10), &[], &[1, 2, 3]);
        assert_eq!(result.len(), 3);
        // All three should get the center cell (sub-cell packing).
        for &(_, rx, ry) in &result {
            assert_eq!((rx, ry), (10, 10), "Infantry should share center cell");
        }
    }

    #[test]
    fn test_four_infantry_overflow_to_next_cell() {
        let grid: PathGrid = PathGrid::new(20, 20);
        let result = distribute_group_destinations(&grid, (10, 10), &[], &[1, 2, 3, 4]);
        assert_eq!(result.len(), 4);
        // First 3 at center.
        for i in 0..3 {
            assert_eq!(
                (result[i].1, result[i].2),
                (10, 10),
                "Infantry {} should be at center",
                i
            );
        }
        // Fourth at a different cell.
        assert_ne!(
            (result[3].1, result[3].2),
            (10, 10),
            "Fourth infantry overflows to adjacent cell"
        );
    }

    #[test]
    fn test_mixed_vehicles_and_infantry() {
        let grid: PathGrid = PathGrid::new(20, 20);
        let result = distribute_group_destinations(&grid, (10, 10), &[1, 2], &[3, 4, 5]);
        assert_eq!(result.len(), 5);
        // Vehicles get unique cells.
        let v1: (u16, u16) = (result[0].1, result[0].2);
        let v2: (u16, u16) = (result[1].1, result[1].2);
        assert_ne!(v1, v2, "Vehicles must have different cells");
        // Infantry should not overlap with vehicle cells.
        for &(_, rx, ry) in &result[2..] {
            assert_ne!((rx, ry), v1, "Infantry must not overlap vehicle 1");
            assert_ne!((rx, ry), v2, "Infantry must not overlap vehicle 2");
        }
    }

    #[test]
    fn test_unwalkable_center_spreads_to_walkable() {
        let mut grid: PathGrid = PathGrid::new(20, 20);
        grid.set_blocked(10, 10, true);
        let result = distribute_group_destinations(&grid, (10, 10), &[1, 2], &[]);
        // Neither vehicle should be at the unwalkable center.
        for &(_, rx, ry) in &result {
            assert_ne!((rx, ry), (10, 10), "Should not assign to unwalkable cell");
        }
    }

    #[test]
    fn test_large_group_all_assigned() {
        let grid: PathGrid = PathGrid::new(30, 30);
        let vehicles: Vec<u64> = (1..=10).collect();
        let infantry: Vec<u64> = (11..=25).collect();
        let result = distribute_group_destinations(&grid, (15, 15), &vehicles, &infantry);
        assert_eq!(result.len(), 25, "All 25 units should get assignments");
        // All vehicle cells should be unique.
        let vehicle_cells: BTreeSet<(u16, u16)> =
            result[..10].iter().map(|&(_, rx, ry)| (rx, ry)).collect();
        assert_eq!(vehicle_cells.len(), 10, "All 10 vehicles at unique cells");
    }

    #[test]
    fn test_determinism_same_inputs_same_outputs() {
        let grid: PathGrid = PathGrid::new(20, 20);
        let v: Vec<u64> = vec![1, 2, 3];
        let i: Vec<u64> = vec![4, 5, 6, 7];
        let r1 = distribute_group_destinations(&grid, (10, 10), &v, &i);
        let r2 = distribute_group_destinations(&grid, (10, 10), &v, &i);
        assert_eq!(r1, r2, "Same inputs must produce same outputs");
    }

    #[test]
    fn test_ring_cells_center_first() {
        let cells = collect_ring_cells((5, 5), 2, 10, 10);
        assert_eq!(cells[0], (5, 5), "Ring 0 should be center");
        assert!(cells.len() > 1, "Should have cells beyond center");
    }

    #[test]
    fn test_ring_cells_at_map_edge() {
        // Center at corner (0,0) — only quadrant cells should appear.
        let cells = collect_ring_cells((0, 0), 2, 10, 10);
        assert_eq!(cells[0], (0, 0));
        for &(x, y) in &cells {
            assert!(x < 10 && y < 10, "All cells should be in bounds");
        }
    }
}
