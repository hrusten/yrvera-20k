//! Deterministic sin/cos lookup table for 256 facings.
//!
//! RA2 facing convention: 0=N, 64=E, 128=S, 192=W (clockwise).
//! Aircraft move in facing direction: dx = sin(facing), dy = -cos(facing).
//!
//! Table values are fixed-point I16F16 (SimFixed), pre-computed at compile time
//! from a hardcoded quarter-wave table. The quarter-wave stores exact integer
//! values of `round(sin(n * pi / 128) * 65536)` for n=0..64, giving sub-0.001
//! error across all 256 facings.

use crate::util::fixed_math::SimFixed;

/// Quarter-wave sine table: sin(n * pi/128) as I16F16 raw bits for n in [0, 64].
/// 65536 = 1.0. Computed offline from: round(sin(n * pi / 128) * 65536).
///
/// Key checkpoints:
///   n=0  -> 0     (sin(0) = 0)
///   n=32 -> 46341 (sin(pi/4) = 0.707)
///   n=64 -> 65536 (sin(pi/2) = 1.0)
const QUARTER_SIN: [i32; 65] = [
        0,  1608,  3216,  4821,  6424,  8022,  9616, 11204,
    12785, 14359, 15924, 17479, 19024, 20557, 22078, 23586,
    25080, 26558, 28020, 29466, 30893, 32303, 33692, 35062,
    36410, 37736, 39040, 40320, 41576, 42806, 44011, 45190,
    46341, 47464, 48559, 49624, 50660, 51665, 52639, 53581,
    54491, 55368, 56212, 57022, 57798, 58538, 59244, 59914,
    60547, 61145, 61705, 62228, 62714, 63162, 63572, 63944,
    64277, 64571, 64827, 65043, 65220, 65358, 65457, 65516,
    65536,
];

/// Compute sin(facing * 2pi / 256) as I16F16 raw bits, const-compatible.
/// Uses quarter-wave symmetry on the hardcoded QUARTER_SIN table.
const fn sin_i16f16(facing: u32) -> i32 {
    // Normalize to [0, 255].
    let f = facing % 256;

    // Determine half-period and quadrant within the half.
    // f in [0, 127] is the positive half; [128, 255] is negative.
    let (negate, half_idx) = if f >= 128 {
        (true, f - 128)
    } else {
        (false, f)
    };

    // Fold into quarter-wave: [0, 64] rising, [65, 127] falling (mirror).
    let q_idx = if half_idx <= 64 {
        half_idx
    } else {
        128 - half_idx
    };

    let raw = QUARTER_SIN[q_idx as usize];

    if negate { -raw } else { raw }
}

/// Sin values for 256 facings. sin(0)=0, sin(64)=1, sin(128)=0, sin(192)=-1.
/// Movement: dx_leptons = SIN_TABLE[facing] * speed.
static SIN_TABLE: [SimFixed; 256] = {
    let mut table = [SimFixed::ZERO; 256];
    let mut i = 0u32;
    while i < 256 {
        table[i as usize] = SimFixed::from_bits(sin_i16f16(i));
        i += 1;
    }
    table
};

/// Cos values for 256 facings. cos(0)=1, cos(64)=0, cos(128)=-1, cos(192)=0.
/// Movement: dy_leptons = -COS_TABLE[facing] * speed.
static COS_TABLE: [SimFixed; 256] = {
    let mut table = [SimFixed::ZERO; 256];
    let mut i = 0u32;
    while i < 256 {
        // cos(f) = sin(f + 64)
        table[i as usize] = SimFixed::from_bits(sin_i16f16((i + 64) % 256));
        i += 1;
    }
    table
};

/// Compute movement delta from facing and speed (in leptons).
/// Returns (dx, dy) where the aircraft moves in its facing direction.
///
/// Matches the original engine convention:
///   dx = sin(angle) * speed
///   dy = -cos(angle) * speed
///
/// Facing 0 = North: dx=0, dy=-speed (up on screen).
/// Facing 64 = East:  dx=speed, dy=0.
pub fn facing_to_movement(facing: u8, speed: SimFixed) -> (SimFixed, SimFixed) {
    let sin_val = SIN_TABLE[facing as usize];
    let cos_val = COS_TABLE[facing as usize];
    (sin_val * speed, -(cos_val * speed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_facing_north() {
        let (dx, dy) = facing_to_movement(0, SimFixed::from_num(100));
        // Facing 0 = North -> dx = 0, dy = -100
        assert_eq!(dx, SimFixed::ZERO);
        assert!(dy < SimFixed::from_num(-99));
    }

    #[test]
    fn test_facing_east() {
        let (dx, dy) = facing_to_movement(64, SimFixed::from_num(100));
        // Facing 64 = East -> dx = 100, dy = 0
        assert!(dx > SimFixed::from_num(99));
        assert!(dy.abs() < SimFixed::from_num(1));
    }

    #[test]
    fn test_facing_south() {
        let (dx, dy) = facing_to_movement(128, SimFixed::from_num(100));
        // Facing 128 = South -> dx = 0, dy = +100
        assert!(dx.abs() < SimFixed::from_num(1));
        assert!(dy > SimFixed::from_num(99));
    }

    #[test]
    fn test_facing_west() {
        let (dx, dy) = facing_to_movement(192, SimFixed::from_num(100));
        // Facing 192 = West -> dx = -100, dy = 0
        assert!(dx < SimFixed::from_num(-99));
        assert!(dy.abs() < SimFixed::from_num(1));
    }

    #[test]
    fn test_facing_zero_speed() {
        let (dx, dy) = facing_to_movement(42, SimFixed::ZERO);
        assert_eq!(dx, SimFixed::ZERO);
        assert_eq!(dy, SimFixed::ZERO);
    }

    #[test]
    fn test_quarter_wave_checkpoints() {
        // n=0: sin=0
        assert_eq!(QUARTER_SIN[0], 0);
        // n=32: sin(pi/4) = 0.707 -> 46341
        assert_eq!(QUARTER_SIN[32], 46341);
        // n=64: sin(pi/2) = 1.0 -> 65536
        assert_eq!(QUARTER_SIN[64], 65536);
    }

    #[test]
    fn test_table_symmetry() {
        // sin(0) = 0
        assert_eq!(SIN_TABLE[0], SimFixed::ZERO);
        // sin(128) = sin(pi) = 0
        assert_eq!(SIN_TABLE[128], SimFixed::ZERO);
        // cos(0) = 1.0
        assert_eq!(COS_TABLE[0], SimFixed::ONE);
        // cos(128) = cos(pi) = -1.0
        assert_eq!(COS_TABLE[128], SimFixed::from_num(-1));
    }
}
