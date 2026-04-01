//! CellSpread offset table — precomputed diamond-pattern cell offsets
//! matching gamemd's CellSpreadTable (0x007ed3d0).
//!
//! Used by combat to iterate cells affected by a warhead detonation for
//! ore destruction (and future wall/bridge/radiation effects).
//!
//! ## Dependency rules
//! - Part of sim/combat/ — no external dependencies beyond std.

use std::sync::LazyLock;

/// CellSpreadTable counts per integer radius, matching gamemd (0x007ed3d0).
/// Index = integer CellSpread radius, value = total cells to process.
const CELL_SPREAD_COUNTS: [usize; 12] = [1, 9, 21, 37, 61, 89, 121, 161, 205, 253, 309, 369];

/// Maximum supported CellSpread radius (index into CELL_SPREAD_COUNTS).
const MAX_SPREAD_RADIUS: usize = 11;

/// Pre-computed cell offsets sorted by distance from center.
/// Total entries = CELL_SPREAD_COUNTS[MAX_SPREAD_RADIUS] = 369.
static SPREAD_OFFSETS: LazyLock<Vec<(i16, i16)>> = LazyLock::new(compute_spread_offsets);

/// Compute all cell offsets for radii 0–11.
///
/// Generates candidate cells by Euclidean distance (`dx² + dy²`), sorted with
/// symmetric tie-breaking so that truncation to `CELL_SPREAD_COUNTS[R]` entries
/// preserves point symmetry. The sort key `(d², |dx|, |dy|, dy, dx)` groups
/// mirror-symmetric quads together, so partial shells always include complete
/// symmetric subgroups.
fn compute_spread_offsets() -> Vec<(i16, i16)> {
    // Generate candidates within a generous Euclidean bound.
    // R*(R+1) for R=11 is 132 — gives ~421 candidates, well above the 369 needed.
    let candidate_threshold = (MAX_SPREAD_RADIUS * (MAX_SPREAD_RADIUS + 1)) as i32;
    let bound = MAX_SPREAD_RADIUS as i16;
    let mut cells: Vec<(i32, i16, i16, i16, i16)> = Vec::new();

    for dy in -bound..=bound {
        for dx in -bound..=bound {
            let d2 = dx as i32 * dx as i32 + dy as i32 * dy as i32;
            if d2 <= candidate_threshold {
                cells.push((d2, dx.abs(), dy.abs(), dy, dx));
            }
        }
    }

    // Sort by (d², |dx|, |dy|, dy, dx). This groups mirror-symmetric quads
    // together: all 4 sign variants of a given (|dx|, |dy|) pair are adjacent.
    // Truncating at group boundaries (which CELL_SPREAD_COUNTS aligns to)
    // preserves point symmetry.
    cells.sort();

    let max_count = CELL_SPREAD_COUNTS[MAX_SPREAD_RADIUS];
    debug_assert!(
        cells.len() >= max_count,
        "need at least {max_count} candidates, got {}",
        cells.len()
    );

    cells.truncate(max_count);
    cells
        .into_iter()
        .map(|(_, _, _, dy, dx)| (dx, dy))
        .collect()
}

/// Returns cell offsets for a given integer spread radius (0–11).
///
/// Index 0 is always `(0, 0)` — the center cell. Radii beyond 11 are
/// clamped to 11. Counts match gamemd's CellSpreadTable:
/// `[1, 9, 21, 37, 61, 89, 121, 161, 205, 253, 309, 369]`.
pub fn cells_in_spread(radius: u32) -> &'static [(i16, i16)] {
    let r = (radius as usize).min(MAX_SPREAD_RADIUS);
    let count = CELL_SPREAD_COUNTS[r];
    &SPREAD_OFFSETS[..count]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_match_gamemd_cell_spread_table() {
        let expected = [1, 9, 21, 37, 61, 89, 121, 161, 205, 253, 309, 369];
        for (radius, &expected_count) in expected.iter().enumerate() {
            let offsets = cells_in_spread(radius as u32);
            assert_eq!(
                offsets.len(),
                expected_count,
                "radius {radius}: expected {expected_count} cells, got {}",
                offsets.len()
            );
        }
    }

    #[test]
    fn center_cell_always_first() {
        for radius in 0..=11u32 {
            let offsets = cells_in_spread(radius);
            assert_eq!(
                offsets[0],
                (0, 0),
                "radius {radius}: first offset must be (0,0)"
            );
        }
    }

    #[test]
    fn offsets_are_symmetric() {
        let offsets = cells_in_spread(11);
        for &(dx, dy) in offsets {
            if dx == 0 && dy == 0 {
                continue;
            }
            assert!(
                offsets.contains(&(-dx, -dy)),
                "offset ({dx}, {dy}) missing mirror ({}, {})",
                -dx,
                -dy
            );
        }
    }

    #[test]
    fn radius_beyond_max_clamped() {
        let r11 = cells_in_spread(11);
        let r99 = cells_in_spread(99);
        assert_eq!(r11.len(), r99.len());
    }

    #[test]
    fn radius_zero_is_center_only() {
        let offsets = cells_in_spread(0);
        assert_eq!(offsets.len(), 1);
        assert_eq!(offsets[0], (0, 0));
    }
}
