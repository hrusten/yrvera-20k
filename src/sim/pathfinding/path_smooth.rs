//! Path smoothing — post-processes raw A* paths for natural-looking movement.
//!
//! Two passes, matching the original YR engine:
//!
//! **Pass 1 — Zigzag smoothing**: Replaces 90-degree zigzag pairs (e.g. N then E)
//! with a single diagonal shortcut (NE), if the shortcut cell is walkable and
//! diagonal corner-cutting rules are satisfied.
//!
//! **Pass 2 — Drift correction**: Identifies segments where cumulative deviation
//! from a straight line exceeds a threshold, then reroutes those segments with
//! a straighter cardinal+diagonal decomposition.
//!
//! ## Dependency rules
//! - Part of sim/ — depends only on sim/locomotor (MovementLayer).
//! - Walkability is injected via closures, no direct grid dependency.

use crate::sim::movement::locomotor::MovementLayer;

// ---------------------------------------------------------------------------
// Direction utilities
// ---------------------------------------------------------------------------

/// Direction index: 0–7 = compass directions matching pathfinding::NEIGHBORS order.
/// N=0, NE=1, E=2, SE=3, S=4, SW=5, W=6, NW=7.
// Direction index is u8: 0–7 compass, 255 = invalid/deleted.

/// Sentinel for deleted/invalid path entries.
const DIR_INVALID: u8 = 255;

/// Delta table: direction index → (dx, dy). Same order as pathfinding::NEIGHBORS.
const DIR_DELTAS: [(i32, i32); 8] = [
    (0, -1),  // 0 = N
    (1, -1),  // 1 = NE
    (1, 0),   // 2 = E
    (1, 1),   // 3 = SE
    (0, 1),   // 4 = S
    (-1, 1),  // 5 = SW
    (-1, 0),  // 6 = W
    (-1, -1), // 7 = NW
];

/// Returns which of the 8 compass directions connects two adjacent cells,
/// or `DIR_INVALID` if the cells are not 8-connected neighbors.
fn direction_between(from: (u16, u16), to: (u16, u16)) -> u8 {
    let dx = to.0 as i32 - from.0 as i32;
    let dy = to.1 as i32 - from.1 as i32;
    for (i, &(ddx, ddy)) in DIR_DELTAS.iter().enumerate() {
        if dx == ddx && dy == ddy {
            return i as u8;
        }
    }
    DIR_INVALID
}

/// Minimum angular distance between two directions on the 8-direction wheel.
/// Returns 0–4 (0 = same direction, 4 = opposite).
fn dir_diff(a: u8, b: u8) -> u8 {
    let raw = a.abs_diff(b);
    raw.min(8 - raw)
}

/// Whether a direction index represents a diagonal (NE, SE, SW, NW).
fn is_diagonal_dir(d: u8) -> bool {
    d < 8 && (d & 1) != 0
}

/// Average of two directions that differ by exactly 2 (the diagonal between them).
/// E.g. N(0) and E(2) → NE(1). Handles wraparound (NW(7) and N(0) → still works).
fn midpoint_dir(a: u8, b: u8) -> u8 {
    // The midpoint on the 8-direction wheel. We need the one that's between them,
    // not the one on the opposite side.
    let lo = a.min(b);
    let hi = a.max(b);
    if hi - lo == 2 {
        lo + 1
    } else {
        // Wraparound case: e.g. dir 7 and dir 1 → midpoint is 0
        // or dir 0 and dir 6 → midpoint is 7
        (hi + 1) % 8
    }
}

// ---------------------------------------------------------------------------
// Pass 1: Zigzag smoothing (matches original SmoothPath)
// ---------------------------------------------------------------------------

/// Smooths 90-degree zigzag patterns in a path by replacing them with diagonal
/// shortcuts when the shortcut cell is walkable.
///
/// A zigzag is two consecutive steps whose directions differ by exactly 2
/// (a 90-degree turn). For example, N then E could be replaced by a single NE
/// step if the diagonal cell is reachable.
///
/// `walkable(x, y)` must return true if the cell is passable for this unit.
pub fn smooth_path(path: Vec<(u16, u16)>, walkable: &dyn Fn(u16, u16) -> bool) -> Vec<(u16, u16)> {
    // Need at least 3 cells (2 steps) to have a zigzag.
    if path.len() < 3 {
        return path;
    }

    let mut result = path;
    let mut i = 0;

    // Iterate through consecutive direction pairs.
    while i + 2 < result.len() {
        let d0 = direction_between(result[i], result[i + 1]);
        let d1 = direction_between(result[i + 1], result[i + 2]);

        // Skip non-adjacent or invalid direction pairs.
        if d0 == DIR_INVALID || d1 == DIR_INVALID || dir_diff(d0, d1) != 2 {
            i += 1;
            continue;
        }

        // Binary-fidelity: only diagonal directions anchor zigzags. gamemd.exe's
        // Path_smooth_corners resets prev_dir to -1 after any cardinal step, so
        // cardinal→cardinal 90° turns (e.g. N→E) are never smoothed. Only
        // diagonal→diagonal pairs (e.g. NE→SE) collapse to a cardinal midpoint.
        if !is_diagonal_dir(d0) {
            i += 1;
            continue;
        }

        // Compute the diagonal shortcut direction.
        let shortcut_dir = midpoint_dir(d0, d1);
        let (sdx, sdy) = DIR_DELTAS[shortcut_dir as usize];
        let sx = (result[i].0 as i32 + sdx) as u16;
        let sy = (result[i].1 as i32 + sdy) as u16;

        // The shortcut cell must be walkable.
        if !walkable(sx, sy) {
            i += 1;
            continue;
        }

        // Diagonal corner-cutting check: both adjacent cardinal cells must be walkable.
        // The two cardinals are the directions d0 and d1 applied from result[i].
        if is_diagonal_dir(shortcut_dir) {
            let (c0x, c0y) = DIR_DELTAS[d0 as usize];
            let (c1x, c1y) = DIR_DELTAS[d1 as usize];
            let card_a = (
                (result[i].0 as i32 + c0x) as u16,
                (result[i].1 as i32 + c0y) as u16,
            );
            let card_b = (
                (result[i].0 as i32 + c1x) as u16,
                (result[i].1 as i32 + c1y) as u16,
            );
            if !walkable(card_a.0, card_a.1) || !walkable(card_b.0, card_b.1) {
                i += 1;
                continue;
            }
        }

        // Replace: remove the intermediate cell (result[i+1]), replace with shortcut.
        // path[i] → shortcut → path[i+2]  instead of  path[i] → path[i+1] → path[i+2]
        result[i + 1] = (sx, sy);

        // The shortcut cell IS the diagonal, so check if result[i] → shortcut → result[i+2]
        // collapses to a single step. If shortcut == result[i+2], remove the duplicate.
        if result[i + 1] == result[i + 2] {
            result.remove(i + 2);
        }

        // Don't advance — the new cell at i+1 might form another zigzag with i+2.
        // But do advance at least once to avoid infinite loops on unchanged paths.
        i += 1;
    }

    result
}

/// Smooths a layered path, skipping any zigzag that crosses a layer transition.
pub fn smooth_layered_path(
    path: Vec<(u16, u16)>,
    layers: Vec<MovementLayer>,
    walkable: &dyn Fn(u16, u16, MovementLayer) -> bool,
) -> (Vec<(u16, u16)>, Vec<MovementLayer>) {
    debug_assert_eq!(path.len(), layers.len());
    if path.len() < 3 {
        return (path, layers);
    }

    let mut coords = path;
    let mut lyrs = layers;
    let mut i = 0;

    while i + 2 < coords.len() {
        // Never smooth across layer transitions.
        if lyrs[i] != lyrs[i + 1] || lyrs[i + 1] != lyrs[i + 2] {
            i += 1;
            continue;
        }
        let layer = lyrs[i];

        let d0 = direction_between(coords[i], coords[i + 1]);
        let d1 = direction_between(coords[i + 1], coords[i + 2]);

        if d0 == DIR_INVALID || d1 == DIR_INVALID || dir_diff(d0, d1) != 2 {
            i += 1;
            continue;
        }

        // Binary-fidelity: only diagonal anchors trigger zigzag smoothing.
        if !is_diagonal_dir(d0) {
            i += 1;
            continue;
        }

        let shortcut_dir = midpoint_dir(d0, d1);
        let (sdx, sdy) = DIR_DELTAS[shortcut_dir as usize];
        let sx = (coords[i].0 as i32 + sdx) as u16;
        let sy = (coords[i].1 as i32 + sdy) as u16;

        if !walkable(sx, sy, layer) {
            i += 1;
            continue;
        }

        if is_diagonal_dir(shortcut_dir) {
            let (c0x, c0y) = DIR_DELTAS[d0 as usize];
            let (c1x, c1y) = DIR_DELTAS[d1 as usize];
            let card_a = (
                (coords[i].0 as i32 + c0x) as u16,
                (coords[i].1 as i32 + c0y) as u16,
            );
            let card_b = (
                (coords[i].0 as i32 + c1x) as u16,
                (coords[i].1 as i32 + c1y) as u16,
            );
            if !walkable(card_a.0, card_a.1, layer) || !walkable(card_b.0, card_b.1, layer) {
                i += 1;
                continue;
            }
        }

        coords[i + 1] = (sx, sy);
        if coords[i + 1] == coords[i + 2] {
            coords.remove(i + 2);
            lyrs.remove(i + 2);
        }

        i += 1;
    }

    (coords, lyrs)
}

// ---------------------------------------------------------------------------
// Pass 2: Drift correction (matches original OptimizePath)
// ---------------------------------------------------------------------------

/// Maximum number of steps to analyze for drift correction.
const MAX_OPTIMIZE_STEPS: usize = 20;

/// Drift threshold multiplier (squared). When `drift^2 > distance * THRESHOLD`,
/// reroute the segment. A value of 1 means reroute when perpendicular drift
/// exceeds the distance traveled along the ideal line.
const DRIFT_THRESHOLD: i32 = 1;

/// Optimizes a path by correcting segments that drift too far from the ideal
/// straight line between their endpoints.
///
/// Analyzes up to `MAX_OPTIMIZE_STEPS` steps. When cumulative perpendicular
/// drift exceeds a threshold, the drifting segment is replaced with a straighter
/// cardinal+diagonal decomposition.
pub fn optimize_path(
    path: Vec<(u16, u16)>,
    walkable: &dyn Fn(u16, u16) -> bool,
) -> Vec<(u16, u16)> {
    if path.len() < 4 {
        return path;
    }

    let mut result = path;
    let steps_to_check = (result.len() - 1).min(MAX_OPTIMIZE_STEPS);

    // Analyze from the start, looking for segments that drift.
    let mut seg_start = 0;
    while seg_start + 2 < result.len() && seg_start < steps_to_check {
        // Find the end of a segment worth rerouting: look ahead for drift.
        if let Some(seg_end) = find_drift_segment(&result, seg_start, steps_to_check) {
            // Attempt to reroute this segment with a straighter path.
            if let Some(replacement) = reroute_segment(result[seg_start], result[seg_end], walkable)
            {
                // Splice the replacement into the result.
                let old_len = seg_end - seg_start + 1;
                let new_len = replacement.len();
                result.splice(seg_start..=seg_end, replacement);
                // Advance past the replaced segment.
                seg_start += new_len.max(1);
                // Adjust steps_to_check for the new length.
                let _ = old_len; // consumed by splice
            } else {
                seg_start += 1;
            }
        } else {
            break;
        }
    }

    result
}

/// Optimizes a layered path. Layer transitions split optimization — each
/// layer-homogeneous segment is optimized independently.
pub fn optimize_layered_path(
    path: Vec<(u16, u16)>,
    layers: Vec<MovementLayer>,
    walkable: &dyn Fn(u16, u16, MovementLayer) -> bool,
) -> (Vec<(u16, u16)>, Vec<MovementLayer>) {
    debug_assert_eq!(path.len(), layers.len());
    if path.len() < 4 {
        return (path, layers);
    }

    // Find layer-homogeneous segments and optimize each independently.
    let mut result_coords: Vec<(u16, u16)> = Vec::with_capacity(path.len());
    let mut result_layers: Vec<MovementLayer> = Vec::with_capacity(path.len());

    let mut seg_start = 0;
    while seg_start < path.len() {
        // Find end of this layer-homogeneous segment.
        let layer = layers[seg_start];
        let mut seg_end = seg_start;
        while seg_end + 1 < path.len() && layers[seg_end + 1] == layer {
            seg_end += 1;
        }

        // Extract and optimize this segment.
        let segment: Vec<(u16, u16)> = path[seg_start..=seg_end].to_vec();
        let layer_check = |x: u16, y: u16| walkable(x, y, layer);
        let optimized = optimize_path(segment, &layer_check);

        // Avoid duplicating the junction cell between segments.
        if !result_coords.is_empty()
            && !optimized.is_empty()
            && result_coords.last() == optimized.first()
        {
            result_coords.extend_from_slice(&optimized[1..]);
            result_layers.extend(std::iter::repeat(layer).take(optimized.len() - 1));
        } else {
            let count = optimized.len();
            result_coords.extend(optimized);
            result_layers.extend(std::iter::repeat(layer).take(count));
        }

        seg_start = seg_end + 1;
    }

    (result_coords, result_layers)
}

// ---------------------------------------------------------------------------
// Drift detection and rerouting helpers
// ---------------------------------------------------------------------------

/// Scans from `start` looking for the first segment that drifts too far from
/// the ideal straight line. Returns the end index of the drifting segment,
/// or None if no drift is found within `max_steps`.
fn find_drift_segment(path: &[(u16, u16)], start: usize, max_steps: usize) -> Option<usize> {
    let limit = (path.len() - 1).min(start + max_steps);
    if start + 2 > limit {
        return None;
    }

    // Accumulate actual displacement vs. ideal direction.
    let mut cum_dx: i32 = 0;
    let mut cum_dy: i32 = 0;

    for i in start..limit {
        let step_dx = path[i + 1].0 as i32 - path[i].0 as i32;
        let step_dy = path[i + 1].1 as i32 - path[i].1 as i32;
        cum_dx += step_dx;
        cum_dy += step_dy;

        // Compare actual displacement with ideal (straight line from start to here).
        let seg_len = i + 1 - start;
        if seg_len < 2 {
            continue;
        }

        // Ideal direction from path[start] to path[i+1].
        let ideal_dx = path[i + 1].0 as i32 - path[start].0 as i32;
        let ideal_dy = path[i + 1].1 as i32 - path[start].1 as i32;

        // Chebyshev length of the straight-line displacement.
        let ideal_dist = ideal_dx.abs().max(ideal_dy.abs());
        if ideal_dist < 2 {
            continue;
        }

        // Perpendicular drift: cross product magnitude gives area of parallelogram.
        // drift = |cum × ideal| / |ideal|, but we compare squared to avoid sqrt.
        // Actually, compare step count vs Chebyshev distance: if we've taken more
        // steps than the Chebyshev distance warrants, we're drifting.
        let cross = (cum_dx * ideal_dy - cum_dy * ideal_dx).abs();
        let drift_sq = cross * cross;
        let dist_sq = ideal_dist * ideal_dist;

        if drift_sq > dist_sq * DRIFT_THRESHOLD {
            return Some(i + 1);
        }
    }

    None
}

/// Attempts to reroute from `start` to `end` with a straighter path using
/// cardinal + diagonal decomposition. Returns None if the route is blocked.
fn reroute_segment(
    start: (u16, u16),
    end: (u16, u16),
    walkable: &dyn Fn(u16, u16) -> bool,
) -> Option<Vec<(u16, u16)>> {
    let dx = end.0 as i32 - start.0 as i32;
    let dy = end.1 as i32 - start.1 as i32;

    if dx == 0 && dy == 0 {
        return Some(vec![start]);
    }

    let abs_dx = dx.abs();
    let abs_dy = dy.abs();
    let diag_steps = abs_dx.min(abs_dy);
    let cardinal_steps = (abs_dx - abs_dy).abs();

    // Determine the diagonal direction.
    let diag_dir = match (dx.signum(), dy.signum()) {
        (1, -1) => 1,  // NE
        (1, 1) => 3,   // SE
        (-1, 1) => 5,  // SW
        (-1, -1) => 7, // NW
        _ => DIR_INVALID,
    };

    // Determine the cardinal direction (along the longer axis).
    let card_dir = if abs_dx > abs_dy {
        if dx > 0 { 2 } else { 6 } // E or W
    } else {
        if dy > 0 { 4 } else { 0 } // S or N
    };

    // Build the rerouted path: interleave diagonal and cardinal steps
    // for the smoothest result.
    let total_steps = diag_steps + cardinal_steps;
    let mut route = Vec::with_capacity(total_steps as usize + 1);
    route.push(start);

    let mut cx = start.0 as i32;
    let mut cy = start.1 as i32;
    let mut diag_remaining = diag_steps;
    let mut card_remaining = cardinal_steps;

    for _ in 0..total_steps {
        // Interleave: take diagonal steps when proportionally due.
        let take_diag = if diag_remaining == 0 {
            false
        } else if card_remaining == 0 {
            true
        } else {
            // Bresenham-style: prefer diagonal when diag_remaining / total_remaining
            // is >= card_remaining / total_remaining.
            diag_remaining * (card_remaining + diag_remaining)
                >= card_remaining * diag_remaining + diag_remaining
            // Simplified: always alternate by ratio.
        };

        let dir = if take_diag && diag_dir != DIR_INVALID {
            diag_remaining -= 1;
            diag_dir
        } else {
            card_remaining -= 1;
            card_dir
        };

        let (ddx, ddy) = DIR_DELTAS[dir as usize];
        let nx = cx + ddx;
        let ny = cy + ddy;

        if !walkable(nx as u16, ny as u16) {
            return None;
        }

        // Diagonal corner-cutting check (i32 arithmetic avoids u16 overflow).
        if is_diagonal_dir(dir) {
            if !walkable((cx + ddx) as u16, cy as u16) || !walkable(cx as u16, (cy + ddy) as u16) {
                return None;
            }
        }

        cx = nx;
        cy = ny;
        route.push((cx as u16, cy as u16));
    }

    // Verify we actually reached the destination.
    if *route.last().unwrap() != end {
        return None;
    }

    Some(route)
}

#[cfg(test)]
#[path = "path_smooth_tests.rs"]
mod tests;
