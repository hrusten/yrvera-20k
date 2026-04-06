//! 3×3 cell grid iteration helper for area-of-effect superweapons.
//!
//! Matches the binary's fixed grid offset table DAT_00B0C038 (9 packed
//! (dx:i16, dy:i16) entries) used by IronCurtain and GeneticConverter.
//!
//! ## Dependency rules
//! - Pure utility — no sim dependencies.

/// 9 cell offsets for a 3×3 grid centered on (0,0), in binary order.
pub const GRID_3X3_OFFSETS: [(i16, i16); 9] = [
    (-1, -1), (0, -1), (1, -1),
    (-1,  0), (0,  0), (1,  0),
    (-1,  1), (0,  1), (1,  1),
];

/// Iterate the 9 cells in a 3×3 grid around (center_rx, center_ry).
/// Coordinates are saturated to u16 bounds (underflow clamps to 0).
/// Caller is responsible for any further map-bounds filtering.
pub fn iter_cells_3x3(center_rx: u16, center_ry: u16) -> impl Iterator<Item = (u16, u16)> {
    GRID_3X3_OFFSETS.iter().map(move |(dx, dy)| {
        let rx = (center_rx as i32 + *dx as i32).max(0).min(u16::MAX as i32) as u16;
        let ry = (center_ry as i32 + *dy as i32).max(0).min(u16::MAX as i32) as u16;
        (rx, ry)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_nine_cells() {
        let cells: Vec<(u16, u16)> = iter_cells_3x3(10, 10).collect();
        assert_eq!(cells.len(), 9);
    }

    #[test]
    fn centered_at_given_cell() {
        let cells: Vec<(u16, u16)> = iter_cells_3x3(10, 10).collect();
        assert!(cells.contains(&(10, 10))); // center
        assert!(cells.contains(&(9, 9)));   // NW
        assert!(cells.contains(&(11, 11))); // SE
    }

    #[test]
    fn saturates_at_zero() {
        let cells: Vec<(u16, u16)> = iter_cells_3x3(0, 0).collect();
        assert_eq!(cells.len(), 9);
        // All negative offsets clamp to 0.
        assert!(cells.iter().all(|(x, y)| *x <= 1 && *y <= 1));
    }

    #[test]
    fn saturates_at_max() {
        let cells: Vec<(u16, u16)> = iter_cells_3x3(u16::MAX, u16::MAX).collect();
        assert_eq!(cells.len(), 9);
        assert!(cells.iter().all(|(x, y)| *x >= u16::MAX - 1 && *y >= u16::MAX - 1));
    }
}
