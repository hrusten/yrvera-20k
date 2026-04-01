//! Turret rotation system — rotates turrets toward attack targets or back to body facing.
//!
//! Units with `TurretFacing` have an independently rotating turret. When attacking,
//! the turret rotates toward the target at the unit's ROT speed. When idle, it
//! returns to body facing. The turret must be aligned with the target before the
//! weapon can fire (checked in combat.rs).
//!
//! ## RA2 ROT convention
//! ROT is in "degrees per game frame at 15 fps". Converting to facing delta per tick:
//! `delta = ROT * 256 / 360 * tick_ms * 15 / 1000`
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/components, sim/combat, rules/.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::rules::ruleset::RuleSet;
use crate::sim::entity_store::EntityStore;
use crate::sim::miner::MinerState;
use crate::util::fixed_math::{SimFixed, facing_from_delta_int_u16};

/// 16-bit turret alignment threshold — same angular tolerance as the u8 version
/// (8/256 ≈ 3.1% of full circle), scaled to 16-bit: 2048/65536 ≈ 3.1%.
pub const TURRET_ALIGN_THRESHOLD_U16: u16 = 2048;

/// Compute the signed shortest-path rotation from `current` to `target` in facing space.
/// Returns a value in -128..=127 (positive = clockwise, negative = counter-clockwise).
pub fn shortest_rotation(current: u8, target: u8) -> i16 {
    let diff: i16 = target as i16 - current as i16;
    // Wrap into -128..127 range for shortest path.
    if diff > 128 {
        diff - 256
    } else if diff < -128 {
        diff + 256
    } else {
        diff
    }
}

/// Convert ROT (degrees/frame at 15fps) + tick_ms into a facing delta per tick.
/// Returns the maximum facing units the entity can rotate this tick.
pub fn rot_to_facing_delta(rot: i32, tick_ms: u32) -> u8 {
    if rot <= 0 || tick_ms == 0 {
        return 0;
    }
    // ROT degrees/frame * 15 frames/sec = degrees/sec
    // degrees/sec * tick_ms/1000 = degrees this tick
    // degrees * 256/360 = facing units this tick
    let numerator: u64 = rot as u64 * 256 * 15 * tick_ms as u64;
    let denominator: u64 = 360 * 1000;
    let delta: u64 = numerator.div_ceil(denominator);
    delta.clamp(1, 128) as u8
}

// ---------------------------------------------------------------------------
// 16-bit turret facing functions (full FacingClass precision)
// ---------------------------------------------------------------------------

/// Signed shortest-path rotation in 16-bit facing space.
/// Returns -32768..=32767 (positive = clockwise, negative = counter-clockwise).
pub fn shortest_rotation_u16(current: u16, target: u16) -> i32 {
    let diff: i32 = target as i32 - current as i32;
    if diff > 32768 {
        diff - 65536
    } else if diff < -32768 {
        diff + 65536
    } else {
        diff
    }
}

/// Convert ROT (degrees/frame at 15fps) + tick_ms into a 16-bit facing delta.
///
/// With 65536 facing units per revolution, the per-tick delta is large enough
/// that ceiling-division rounding error is negligible (<1%), fixing the
/// rotation speed mismatch that plagued the 8-bit version at 45Hz.
pub fn rot_to_facing_delta_u16(rot: i32, tick_ms: u32) -> u16 {
    if rot <= 0 || tick_ms == 0 {
        return 0;
    }
    // ROT degrees/frame × 15 frames/sec × tick_ms/1000 = degrees this tick
    // degrees × 65536/360 = 16-bit facing units this tick
    let numerator: u64 = rot as u64 * 65536 * 15 * tick_ms as u64;
    let denominator: u64 = 360 * 1000;
    let delta: u64 = numerator.div_ceil(denominator);
    // No clamp(1,...) needed — at u16 scale even ROT=1, tick_ms=22 gives ~61.
    delta.min(32768) as u16
}

/// Check whether a 16-bit turret facing is aligned with the desired target.
pub fn is_turret_aligned_u16(turret_facing: u16, target_facing: u16) -> bool {
    let rot: i32 = shortest_rotation_u16(turret_facing, target_facing);
    rot.unsigned_abs() <= TURRET_ALIGN_THRESHOLD_U16 as u32
}

/// Compute 16-bit turret facing from source to target using lepton-precise
/// positions, providing sub-cell accuracy for targeting.
pub fn facing_toward_lepton(
    from_rx: u16,
    from_ry: u16,
    from_sub_x: SimFixed,
    from_sub_y: SimFixed,
    to_rx: u16,
    to_ry: u16,
    to_sub_x: SimFixed,
    to_sub_y: SimFixed,
) -> u16 {
    let from_lep_x: i32 = from_rx as i32 * 256 + from_sub_x.to_num::<i32>();
    let from_lep_y: i32 = from_ry as i32 * 256 + from_sub_y.to_num::<i32>();
    let to_lep_x: i32 = to_rx as i32 * 256 + to_sub_x.to_num::<i32>();
    let to_lep_y: i32 = to_ry as i32 * 256 + to_sub_y.to_num::<i32>();
    let dx: i32 = to_lep_x - from_lep_x;
    let dy: i32 = to_lep_y - from_lep_y;
    facing_from_delta_int_u16(dx, dy)
}

/// Convert 8-bit body facing to 16-bit turret facing.
/// Maps 0..255 → 0..65280 (shifts into the upper byte).
#[inline]
pub fn body_facing_to_turret(body: u8) -> u16 {
    (body as u16) << 8
}

/// Advance turret rotation for all entities with TurretFacing.
///
/// - If entity has AttackTarget: rotate turret toward target (lepton-precise).
/// - Otherwise: rotate turret back to body facing (idle return).
///
/// Uses 16-bit DirStruct precision (full FacingClass range), which eliminates
/// the rotation-speed overshoot that occurred with 8-bit stepping.
pub fn tick_turret_rotation(
    entities: &mut EntityStore,
    rules: &RuleSet,
    tick_ms: u32,
    interner: &crate::sim::intern::StringInterner,
) {
    if tick_ms == 0 {
        return;
    }

    struct TurretUpdate {
        id: u64,
        current_turret: u16,
        target_facing: u16,
        max_delta: u16,
    }
    let mut updates: Vec<TurretUpdate> = Vec::new();

    // Phase 1: collect all turret entities and their desired rotation.
    let keys: Vec<u64> = entities.keys_sorted();
    for &id in &keys {
        let entity = match entities.get(id) {
            Some(e) => e,
            None => continue,
        };
        let current_turret: u16 = match entity.turret_facing {
            Some(tf) => tf,
            None => continue,
        };

        let rot: i32 = rules
            .object(interner.resolve(entity.type_ref))
            .map(|obj| obj.turret_rot)
            .unwrap_or(5);
        let max_delta: u16 = rot_to_facing_delta_u16(rot, tick_ms);
        let is_harvesting: bool = entity
            .miner
            .as_ref()
            .is_some_and(|m| m.state == MinerState::Harvest);

        let desired_facing: u16 = if let Some(ref attack) = entity.attack_target {
            // Look up target position by stable ID (lepton-precise).
            let target_pos = entities.get(attack.target).map(|t| {
                (
                    t.position.rx,
                    t.position.ry,
                    t.position.sub_x,
                    t.position.sub_y,
                )
            });
            if let Some((trx, try_, tsx, tsy)) = target_pos {
                facing_toward_lepton(
                    entity.position.rx,
                    entity.position.ry,
                    entity.position.sub_x,
                    entity.position.sub_y,
                    trx,
                    try_,
                    tsx,
                    tsy,
                )
            } else {
                body_facing_to_turret(entity.facing)
            }
        } else if is_harvesting {
            // Harvesting: spin turret continuously by targeting half-turn ahead.
            current_turret.wrapping_add(32768)
        } else {
            body_facing_to_turret(entity.facing)
        };

        updates.push(TurretUpdate {
            id,
            current_turret,
            target_facing: desired_facing,
            max_delta,
        });
    }

    // Phase 2: apply rotation.
    for update in &updates {
        let rotation: i32 = shortest_rotation_u16(update.current_turret, update.target_facing);
        if rotation == 0 {
            continue;
        }

        let clamped: i32 = if rotation > 0 {
            rotation.min(update.max_delta as i32)
        } else {
            rotation.max(-(update.max_delta as i32))
        };

        let new_facing: u16 = (update.current_turret as i32 + clamped).rem_euclid(65536) as u16;
        if let Some(entity) = entities.get_mut(update.id) {
            entity.turret_facing = Some(new_facing);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shortest_rotation_clockwise() {
        assert_eq!(shortest_rotation(0, 10), 10);
        assert_eq!(shortest_rotation(200, 210), 10);
    }

    #[test]
    fn test_shortest_rotation_counter_clockwise() {
        assert_eq!(shortest_rotation(10, 0), -10);
        assert_eq!(shortest_rotation(10, 250), -16); // 250 - 10 = 240 > 128, so 240-256=-16
    }

    #[test]
    fn test_shortest_rotation_wrap_around() {
        // From 250 to 10: clockwise is +16, counter-clockwise is -240. Should pick +16.
        assert_eq!(shortest_rotation(250, 10), 16);
        // From 10 to 250: clockwise is +240, counter-clockwise is -16. Should pick -16.
        assert_eq!(shortest_rotation(10, 250), -16);
    }

    #[test]
    fn test_rot_to_facing_delta() {
        // ROT=5, tick_ms=33 (30Hz): 5 * 256 * 15 * 33 / (360 * 1000) = ~1.76 -> 2
        let delta: u8 = rot_to_facing_delta(5, 33);
        assert!(delta >= 1 && delta <= 3, "delta={}", delta);

        // ROT=0 -> 0
        assert_eq!(rot_to_facing_delta(0, 33), 0);

        // ROT=7 (Grizzly), tick_ms=33: 7*256*15*33 / 360000 = ~2.46 -> 3
        let delta: u8 = rot_to_facing_delta(7, 33);
        assert!(delta >= 2 && delta <= 4, "delta={}", delta);
    }

    // -----------------------------------------------------------------------
    // 16-bit turret facing tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_shortest_rotation_u16_clockwise() {
        assert_eq!(shortest_rotation_u16(0, 1000), 1000);
        assert_eq!(shortest_rotation_u16(50000, 51000), 1000);
    }

    #[test]
    fn test_shortest_rotation_u16_counter_clockwise() {
        assert_eq!(shortest_rotation_u16(1000, 0), -1000);
    }

    #[test]
    fn test_shortest_rotation_u16_wrap() {
        // From 65000 to 500: clockwise +1036, counter-clockwise -64536. Pick +1036.
        assert_eq!(shortest_rotation_u16(65000, 500), 1036);
        // From 500 to 65000: pick -1036.
        assert_eq!(shortest_rotation_u16(500, 65000), -1036);
    }

    #[test]
    fn test_rot_to_facing_delta_u16_speed_accuracy() {
        // ROT=5 at tick_ms=22 (45Hz):
        // numerator = 5 * 65536 * 15 * 22 = 108,288,000
        // denominator = 360,000
        // exact = 300.8, ceil = 301
        let delta = rot_to_facing_delta_u16(5, 22);
        assert_eq!(delta, 301);

        // Per-second rate: 301 * 45 = 13,545
        // gamemd: 5° × 65536/360 × 15 ≈ 13,653
        // Error: (13653-13545)/13653 = 0.8% — acceptable.
        let per_sec: u64 = delta as u64 * 45;
        let gamemd_per_sec: u64 = 13653;
        let error_pct: f64 =
            (gamemd_per_sec as f64 - per_sec as f64).abs() / gamemd_per_sec as f64 * 100.0;
        assert!(
            error_pct < 1.5,
            "ROT=5 speed error {error_pct:.1}% exceeds 1.5%"
        );
    }

    #[test]
    fn test_rot_to_facing_delta_u16_slow_turret() {
        // ROT=1 at tick_ms=22:
        // numerator = 1 * 65536 * 15 * 22 = 21,626,880
        // denominator = 360,000
        // exact = 60.08, ceil = 61
        let delta = rot_to_facing_delta_u16(1, 22);
        assert_eq!(delta, 61);

        // Per-second: 61 * 45 = 2,745. gamemd: 2,731. Error ~0.5%.
        let per_sec: u64 = delta as u64 * 45;
        let gamemd_per_sec: u64 = 2731;
        let error_pct: f64 =
            (gamemd_per_sec as f64 - per_sec as f64).abs() / gamemd_per_sec as f64 * 100.0;
        assert!(
            error_pct < 1.5,
            "ROT=1 speed error {error_pct:.1}% exceeds 1.5%"
        );
    }

    #[test]
    fn test_rot_to_facing_delta_u16_zero() {
        assert_eq!(rot_to_facing_delta_u16(0, 22), 0);
        assert_eq!(rot_to_facing_delta_u16(5, 0), 0);
    }

    #[test]
    fn test_is_turret_aligned_u16() {
        assert!(is_turret_aligned_u16(25600, 25600)); // exact
        assert!(is_turret_aligned_u16(25600, 27000)); // within 2048
        assert!(!is_turret_aligned_u16(25600, 30000)); // 4400 > 2048
        // Wrap-around.
        assert!(is_turret_aligned_u16(500, 65000)); // diff = -1036, within 2048
        assert!(!is_turret_aligned_u16(500, 60000)); // diff = -6036, outside
    }

    #[test]
    fn test_facing_toward_lepton_cardinal() {
        use crate::util::fixed_math::SimFixed;
        let center = SimFixed::from_num(128);
        // Target 5 cells east: should be ~16384 (E).
        let f = facing_toward_lepton(10, 10, center, center, 15, 10, center, center);
        assert!((f as i32 - 16384).abs() < 2, "east facing={f}");
        // Target 5 cells south: should be ~32768 (S).
        let f = facing_toward_lepton(10, 10, center, center, 10, 15, center, center);
        assert!((f as i32 - 32768).abs() < 2, "south facing={f}");
    }

    #[test]
    fn test_facing_toward_lepton_subcell_precision() {
        use crate::util::fixed_math::SimFixed;
        // Same cell, but target is at sub_x=200, sub_y=128 vs source at sub_x=50, sub_y=128.
        // Delta: dx_lep = +150, dy_lep = 0 → pure east → ~16384.
        let f = facing_toward_lepton(
            10,
            10,
            SimFixed::from_num(50),
            SimFixed::from_num(128),
            10,
            10,
            SimFixed::from_num(200),
            SimFixed::from_num(128),
        );
        assert!((f as i32 - 16384).abs() < 2, "sub-cell east facing={f}");
    }

    #[test]
    fn test_body_facing_to_turret() {
        assert_eq!(body_facing_to_turret(0), 0);
        assert_eq!(body_facing_to_turret(64), 16384);
        assert_eq!(body_facing_to_turret(128), 32768);
        assert_eq!(body_facing_to_turret(255), 65280);
    }
}
