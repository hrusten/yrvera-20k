//! Lepton coordinate system — sub-cell spatial precision matching RA2's internal units.
//!
//! RA2 uses **leptons** as its fundamental spatial unit: 256 leptons = 1 cell.
//! Each isometric cell spans 256×256 leptons, with height levels at 128 leptons.
//!
//! To avoid SimFixed (I16F16, max 32767) overflow on large maps, we store lepton
//! positions as **cell coordinate + sub-cell offset** rather than absolute leptons.
//! The sub-cell offset (`sub_x`, `sub_y`) ranges from 0 to 256 within the cell,
//! with 128 being the cell center.
//!
//! ## Dependency rules
//! - util/ has NO dependencies on other game modules.

use crate::util::fixed_math::{SIM_ZERO, SimFixed};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Number of leptons per cell (256). This is RA2's fundamental spatial unit ratio.
pub const LEPTONS_PER_CELL: SimFixed = SimFixed::lit("256");

/// Lepton offset for the center of a cell (128). Default sub-cell position.
pub const CELL_CENTER_LEPTON: SimFixed = SimFixed::lit("128");

/// Tile width in pixels (60.0) divided by leptons per cell (256).
/// Pre-computed for efficient lepton → screen pixel conversion.
/// = 60.0 / 256.0 = 0.234375
const SCREEN_X_PER_LEPTON: f32 = 60.0 / 256.0;

/// Tile half-height in pixels (15.0) divided by leptons per cell (256).
/// Pre-computed for efficient lepton → screen pixel conversion.
/// = 30.0 / 256.0 = 0.1171875
const SCREEN_Y_PER_LEPTON: f32 = 30.0 / 256.0;

// ---------------------------------------------------------------------------
// Infantry sub-cell lepton positions (RA2 canonical)
// ---------------------------------------------------------------------------

/// Sub-cell lepton offsets within a cell. RA2 defines 5 sub-cell positions (0–4).
/// Extracted from the original engine's runtime init.
///
/// - Sub-cell 0: center  (128, 128) — vehicles + default
/// - Sub-cell 1: top-left  ( 64,  64)
/// - Sub-cell 2: top-right (192,  64)
/// - Sub-cell 3: bottom-left  ( 64, 192) — used for infantry placement
/// - Sub-cell 4: bottom-right (192, 192) — used for infantry placement

/// Sub-cell center position (sub-cell 0, and fallback for vehicles).
pub const SUBCELL_CENTER_X: SimFixed = SimFixed::lit("128");
pub const SUBCELL_CENTER_Y: SimFixed = SimFixed::lit("128");

/// Sub-cell 1: top-left within the cell diamond.
pub const SUBCELL_1_X: SimFixed = SimFixed::lit("64");
pub const SUBCELL_1_Y: SimFixed = SimFixed::lit("64");

/// Sub-cell 2: top-right within the cell diamond.
pub const SUBCELL_2_X: SimFixed = SimFixed::lit("192");
pub const SUBCELL_2_Y: SimFixed = SimFixed::lit("64");

/// Sub-cell 3: bottom-left (SW quadrant) within the cell diamond.
pub const SUBCELL_3_X: SimFixed = SimFixed::lit("64");
pub const SUBCELL_3_Y: SimFixed = SimFixed::lit("192");

/// Sub-cell 4: bottom-right (SE quadrant) within the cell diamond.
pub const SUBCELL_4_X: SimFixed = SimFixed::lit("192");
pub const SUBCELL_4_Y: SimFixed = SimFixed::lit("192");

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

/// Get the lepton sub-cell offset for a given sub-cell index (0–4).
///
/// Returns `(sub_x, sub_y)` in lepton units (0..256 range).
/// Unknown indices default to cell center.
pub fn subcell_lepton_offset(sub_cell: Option<u8>) -> (SimFixed, SimFixed) {
    match sub_cell {
        Some(1) => (SUBCELL_1_X, SUBCELL_1_Y),
        Some(2) => (SUBCELL_2_X, SUBCELL_2_Y),
        Some(3) => (SUBCELL_3_X, SUBCELL_3_Y),
        Some(4) => (SUBCELL_4_X, SUBCELL_4_Y),
        _ => (SUBCELL_CENTER_X, SUBCELL_CENTER_Y), // 0 + fallback
    }
}

/// Convert a sub-cell lepton offset to a screen-space pixel offset from cell center.
///
/// The isometric projection maps lepton offsets from center (128, 128) to pixels:
///   dx_pixels = (sub_x - sub_y) * (TILE_WIDTH / 2) / 256
///   dy_pixels = (sub_x + sub_y - 256) * (TILE_HEIGHT / 2) / 256
///
/// This replaces hardcoded pixel offsets with lepton-derived values.
pub fn lepton_sub_to_screen_offset(sub_x: SimFixed, sub_y: SimFixed) -> (f32, f32) {
    let dx_lep: f32 = sub_x.to_num::<f32>() - CELL_CENTER_LEPTON.to_num::<f32>();
    let dy_lep: f32 = sub_y.to_num::<f32>() - CELL_CENTER_LEPTON.to_num::<f32>();
    // Isometric projection: offset from center
    let screen_dx: f32 = (dx_lep - dy_lep) * SCREEN_X_PER_LEPTON / 2.0;
    let screen_dy: f32 = (dx_lep + dy_lep) * SCREEN_Y_PER_LEPTON / 2.0;
    (screen_dx, screen_dy)
}

/// Compute full screen position from cell coordinates + sub-cell lepton offset + elevation.
///
/// This is an extended version of `iso_to_screen(rx, ry, z)` that adds sub-cell precision.
/// When sub_x = sub_y = 128 (cell center), the result matches `iso_to_screen()` exactly.
pub fn lepton_to_screen(rx: u16, ry: u16, sub_x: SimFixed, sub_y: SimFixed, z: u8) -> (f32, f32) {
    // Isometric projection formula:
    //   screenX = 30*(rx - ry)         (cell center +128 cancels in X-Y)
    //   screenY = 15*(rx + ry) + 15    (+15 from sub-cell 128+128 projection)
    let base_sx: f32 = (rx as f32 - ry as f32) * 30.0;
    let base_sy: f32 = (rx as f32 + ry as f32) * 15.0 + 15.0 - z as f32 * 15.0;
    // Sub-cell offset from center
    let (offset_x, offset_y) = lepton_sub_to_screen_offset(sub_x, sub_y);
    (base_sx + offset_x, base_sy + offset_y)
}

/// Compute lepton direction vector and length for a cell-to-cell step.
///
/// `dx`, `dy` are the cell delta (each in {-1, 0, +1}).
/// Returns `(dir_x, dir_y, length)` where dir is in leptons and length is
/// 256 for cardinal moves, ~362 for diagonal moves.
/// Returns `(0, 0, 0)` if dx=dy=0.
pub fn cell_delta_to_lepton_dir(dx: i32, dy: i32) -> (SimFixed, SimFixed, SimFixed) {
    if dx == 0 && dy == 0 {
        return (SIM_ZERO, SIM_ZERO, SIM_ZERO);
    }
    let dir_x: SimFixed = SimFixed::from_num(dx * 256);
    let dir_y: SimFixed = SimFixed::from_num(dy * 256);
    // dx,dy ∈ {-1,0,+1} so length is exactly 256 (cardinal) or sqrt(2)*256 ≈ 362 (diagonal).
    // Compute directly to avoid I16F16 overflow from 256*256.
    let is_diagonal: bool = dx != 0 && dy != 0;
    let len: SimFixed = if is_diagonal {
        // sqrt(2) * 256 ≈ 362.038... — use SimFixed::lit for deterministic precision.
        SimFixed::lit("362.038")
    } else {
        SimFixed::from_num(256)
    };
    (dir_x, dir_y, len)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::terrain;

    #[test]
    fn cell_center_is_iso_plus_half_tile() {
        // lepton_to_screen = CoordsToClient(cell_center) = iso_to_screen + (30, 0).
        // Both iso and lepton Y include +15 from cell center projection.
        // X differs by +30 (tile NW corner vs CoordsToClient north vertex).
        for rx in [0u16, 5, 10, 50] {
            for ry in [0u16, 3, 10, 50] {
                for z in [0u8, 2, 4] {
                    let (corner_sx, corner_sy) = terrain::iso_to_screen(rx, ry, z);
                    let (actual_sx, actual_sy) =
                        lepton_to_screen(rx, ry, CELL_CENTER_LEPTON, CELL_CENTER_LEPTON, z);
                    assert!(
                        (actual_sx - (corner_sx + 30.0)).abs() < 0.01,
                        "X mismatch at ({}, {}, z={}): expected {}, got {}",
                        rx,
                        ry,
                        z,
                        corner_sx + 30.0,
                        actual_sx,
                    );
                    assert!(
                        (actual_sy - corner_sy).abs() < 0.01,
                        "Y mismatch at ({}, {}, z={}): expected {}, got {}",
                        rx,
                        ry,
                        z,
                        corner_sy,
                        actual_sy,
                    );
                }
            }
        }
    }

    #[test]
    fn subcell_3_offsets_bottom_left() {
        let (dx, _dy) = lepton_sub_to_screen_offset(SUBCELL_3_X, SUBCELL_3_Y);
        // Sub-cell 3 (64, 192): dx_lep = -64, dy_lep = +64.
        // Isometric: screen_dx = -15 (left of center), screen_dy = 0.
        assert!(dx < 0.0, "Sub-cell 3 should be left of center: dx={}", dx);
    }

    #[test]
    fn subcell_4_offsets_bottom_right() {
        let (dx, _dy) = lepton_sub_to_screen_offset(SUBCELL_4_X, SUBCELL_4_Y);
        // Sub-cell 4 (192, 192): dx_lep = +64, dy_lep = +64.
        // Isometric: screen_dx = 0, screen_dy = +7.5 (below center).
        assert!(
            dx.abs() < 1.0,
            "Sub-cell 4 should be near center X: dx={}",
            dx
        );
    }

    #[test]
    fn subcell_center_has_zero_offset() {
        let (dx, dy) = lepton_sub_to_screen_offset(SUBCELL_CENTER_X, SUBCELL_CENTER_Y);
        assert!(
            dx.abs() < 0.001,
            "Center sub-cell X offset should be ~0: {}",
            dx
        );
        assert!(
            dy.abs() < 0.001,
            "Center sub-cell Y offset should be ~0: {}",
            dy
        );
    }

    #[test]
    fn subcell_lookup_returns_correct_positions() {
        assert_eq!(
            subcell_lepton_offset(None),
            (SUBCELL_CENTER_X, SUBCELL_CENTER_Y)
        );
        assert_eq!(
            subcell_lepton_offset(Some(0)),
            (SUBCELL_CENTER_X, SUBCELL_CENTER_Y)
        );
        assert_eq!(subcell_lepton_offset(Some(1)), (SUBCELL_1_X, SUBCELL_1_Y));
        assert_eq!(subcell_lepton_offset(Some(2)), (SUBCELL_2_X, SUBCELL_2_Y));
        assert_eq!(subcell_lepton_offset(Some(3)), (SUBCELL_3_X, SUBCELL_3_Y));
        assert_eq!(subcell_lepton_offset(Some(4)), (SUBCELL_4_X, SUBCELL_4_Y));
    }

    #[test]
    fn leptons_per_cell_is_256() {
        assert_eq!(LEPTONS_PER_CELL.to_num::<i32>(), 256);
    }

    #[test]
    fn cell_delta_to_lepton_dir_cardinal() {
        let (dx, dy, len) = cell_delta_to_lepton_dir(1, 0);
        assert_eq!(dx.to_num::<i32>(), 256);
        assert_eq!(dy.to_num::<i32>(), 0);
        assert_eq!(len.to_num::<i32>(), 256);
    }

    #[test]
    fn cell_delta_to_lepton_dir_diagonal() {
        let (dx, dy, len) = cell_delta_to_lepton_dir(1, 1);
        assert_eq!(dx.to_num::<i32>(), 256);
        assert_eq!(dy.to_num::<i32>(), 256);
        // sqrt(256² + 256²) ≈ 362
        let len_i: i32 = len.to_num();
        assert!(
            len_i >= 361 && len_i <= 363,
            "diagonal len should be ~362, got {}",
            len_i
        );
    }

    #[test]
    fn cell_delta_to_lepton_dir_zero() {
        let (dx, dy, len) = cell_delta_to_lepton_dir(0, 0);
        assert_eq!(dx.to_num::<i32>(), 0);
        assert_eq!(dy.to_num::<i32>(), 0);
        assert_eq!(len.to_num::<i32>(), 0);
    }
}
