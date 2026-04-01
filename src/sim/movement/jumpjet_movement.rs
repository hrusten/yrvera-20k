//! Jumpjet locomotor — altitude-holding hover flight with acceleration and wobble.
//!
//! Jumpjets are distinct from Fly aircraft: they hover at a fixed altitude with
//! wobble, use acceleration/deceleration curves, and have turn rate limits.
//! TS-style jumpjet infantry walk for short moves (≤3 cells) when !HoverAttack.
//!
//! ## Key RA2/YR rules (from locomotor report)
//! - JumpjetAccel: acceleration rate; deceleration = accel × 1.5
//! - JumpjetTurnRate: facing change limit per tick
//! - JumpjetWobbles + JumpjetDeviation: lateral oscillation during hover
//! - JumpjetCrash: crash descent speed = climb + crash
//! - BalloonHover=yes: stay airborne after reaching destination
//! - Infantry + Jumpjet + !HoverAttack: Walk for ≤3 cells
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/locomotor, sim/movement.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::sim::movement::locomotor::{AirMovePhase, LocomotorState};
use crate::util::fixed_math::{SimFixed, SIM_1_5, SIM_ZERO};

/// Max cells for infantry walk fallback (TS-style jumpjet infantry).
const INFANTRY_WALK_THRESHOLD_CELLS: u32 = 3;

/// Apply acceleration toward target speed.
///
/// Ramps `jumpjet_current_speed` toward `jumpjet_speed` using `jumpjet_accel`.
/// Deceleration (when current > target) uses `accel * 1.5` per the RA2 formula.
pub fn tick_jumpjet_acceleration(loco: &mut LocomotorState, dt: SimFixed, moving: bool) {
    let target_speed: SimFixed = if moving { loco.jumpjet_speed } else { SIM_ZERO };

    if loco.jumpjet_accel <= SIM_ZERO {
        // No acceleration — snap to target speed.
        loco.jumpjet_current_speed = target_speed;
        return;
    }

    if loco.jumpjet_current_speed < target_speed {
        // Accelerating.
        loco.jumpjet_current_speed += loco.jumpjet_accel * dt;
        if loco.jumpjet_current_speed > target_speed {
            loco.jumpjet_current_speed = target_speed;
        }
    } else if loco.jumpjet_current_speed > target_speed {
        // Decelerating at 1.5× acceleration rate.
        loco.jumpjet_current_speed -= loco.jumpjet_accel * SIM_1_5 * dt;
        if loco.jumpjet_current_speed < target_speed {
            loco.jumpjet_current_speed = target_speed;
        }
    }
}

/// Apply turn rate limit: rotate facing toward desired facing, clamped to max delta.
///
/// Uses shortest-arc rotation. `turn_rate` is max facing units (0-255) per tick.
/// Returns the new facing value.
pub fn apply_turn_rate(current: u8, desired: u8, turn_rate: i32) -> u8 {
    if turn_rate <= 0 || current == desired {
        return desired;
    }

    // Compute shortest-arc signed delta in the 0-255 wrapping space.
    let diff: i16 = desired as i16 - current as i16;
    let wrapped: i16 = if diff > 128 {
        diff - 256
    } else if diff < -128 {
        diff + 256
    } else {
        diff
    };

    let max_delta: i16 = turn_rate as i16;
    let clamped: i16 = wrapped.clamp(-max_delta, max_delta);
    (current as i16 + clamped).rem_euclid(256) as u8
}

/// Compute deterministic hover wobble offset for visual position.
///
/// Returns (wobble_x, wobble_y) offset in screen pixels. The wobble is a
/// sinusoidal oscillation based on the simulation tick, producing smooth
/// hovering motion. Each entity gets a unique phase from `entity_seed`.
/// KEPT as f32 — render-only visual effect.
pub fn compute_wobble(tick: u64, entity_seed: u64, wobbles: f32, deviation: i32) -> (f32, f32) {
    if wobbles <= 0.0 || deviation <= 0 {
        return (0.0, 0.0);
    }

    // Use tick and entity seed for deterministic phase.
    let phase_x: f32 = (tick as f32 * 0.1 + entity_seed as f32 * 0.37) % std::f32::consts::TAU;
    let phase_y: f32 = (tick as f32 * 0.13 + entity_seed as f32 * 0.53) % std::f32::consts::TAU;

    let amplitude: f32 = wobbles * deviation as f32 * 0.01;
    let wx: f32 = phase_x.sin() * amplitude;
    let wy: f32 = phase_y.cos() * amplitude;
    (wx, wy)
}

/// Whether an idle jumpjet should begin landing (descending).
///
/// Returns true if the unit has `balloon_hover=false` (should land when idle)
/// and is currently hovering without a movement order.
pub fn should_land(loco: &LocomotorState) -> bool {
    !loco.balloon_hover && loco.air_phase == AirMovePhase::Hovering
}

/// Whether a jumpjet infantry unit should use ground Walk for this move distance.
///
/// TS-style rule: infantry with Jumpjet + !HoverAttack walk for ≤3 cells.
pub fn should_use_walk_fallback(
    hover_attack: bool,
    is_infantry: bool,
    distance_cells: u32,
) -> bool {
    is_infantry && !hover_attack && distance_cells <= INFANTRY_WALK_THRESHOLD_CELLS
}

/// Advance the jumpjet altitude state machine for one entity.
///
/// Like `tick_altitude` but uses jumpjet-specific crash speed when the unit
/// is in a crash descent (air_phase == Descending with crash_speed > 0).
pub fn tick_jumpjet_altitude(loco: &mut LocomotorState, dt: SimFixed) {
    match loco.air_phase {
        AirMovePhase::Ascending => {
            loco.altitude += loco.climb_rate * dt;
            if loco.altitude >= loco.target_altitude {
                loco.altitude = loco.target_altitude;
                loco.air_phase = AirMovePhase::Hovering;
            }
        }
        AirMovePhase::Descending => {
            // Use crash speed if set (unit was killed mid-air), otherwise normal climb.
            let descent_rate: SimFixed = if loco.jumpjet_crash_speed > SIM_ZERO {
                loco.jumpjet_crash_speed
            } else {
                loco.climb_rate
            };
            loco.altitude -= descent_rate * dt;
            if loco.altitude <= SIM_ZERO {
                loco.altitude = SIM_ZERO;
                loco.air_phase = AirMovePhase::Landed;
            }
        }
        AirMovePhase::Hovering => {
            // Maintain hover altitude.
            loco.altitude = loco.target_altitude;
        }
        AirMovePhase::Cruising => {
            // Jumpjets shouldn't be in Cruising, but treat as Hovering.
            loco.altitude = loco.target_altitude;
            loco.air_phase = AirMovePhase::Hovering;
        }
        AirMovePhase::Landed => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::fixed_math::{sim_from_f32, SIM_ONE};

    #[test]
    fn test_acceleration_ramps_up() {
        let mut loco = make_test_jj_loco();
        loco.jumpjet_accel = SimFixed::from_num(2);
        loco.jumpjet_speed = SimFixed::from_num(14);
        loco.jumpjet_current_speed = SIM_ZERO;

        tick_jumpjet_acceleration(&mut loco, SIM_ONE, true);
        assert_eq!(loco.jumpjet_current_speed, SimFixed::from_num(2));

        tick_jumpjet_acceleration(&mut loco, SIM_ONE, true);
        assert_eq!(loco.jumpjet_current_speed, SimFixed::from_num(4));
    }

    #[test]
    fn test_deceleration_is_1_5x() {
        let mut loco = make_test_jj_loco();
        loco.jumpjet_accel = SimFixed::from_num(2);
        loco.jumpjet_speed = SimFixed::from_num(14);
        loco.jumpjet_current_speed = SimFixed::from_num(14);

        // Decelerate (not moving).
        tick_jumpjet_acceleration(&mut loco, SIM_ONE, false);
        // Should decrease by 2.0 * 1.5 = 3.0.
        assert_eq!(loco.jumpjet_current_speed, SimFixed::from_num(11));
    }

    #[test]
    fn test_acceleration_caps_at_target() {
        let mut loco = make_test_jj_loco();
        loco.jumpjet_accel = SimFixed::from_num(100);
        loco.jumpjet_speed = SimFixed::from_num(14);
        loco.jumpjet_current_speed = SIM_ZERO;

        tick_jumpjet_acceleration(&mut loco, SIM_ONE, true);
        assert_eq!(loco.jumpjet_current_speed, SimFixed::from_num(14));
    }

    #[test]
    fn test_turn_rate_limits_facing() {
        // Current=0 (north), desired=64 (east), turn_rate=8.
        let result = apply_turn_rate(0, 64, 8);
        assert_eq!(result, 8, "Should turn by max 8 units");
    }

    #[test]
    fn test_turn_rate_shortest_arc() {
        // Current=250, desired=10 — shortest arc is +16 (wrapping through 0).
        let result = apply_turn_rate(250, 10, 8);
        // Should turn +8 (from 250 toward 10 through 0).
        assert_eq!(
            result, 2,
            "Should wrap through 0: 250 + 8 = 258 mod 256 = 2"
        );
    }

    #[test]
    fn test_turn_rate_already_at_target() {
        let result = apply_turn_rate(64, 64, 8);
        assert_eq!(result, 64);
    }

    #[test]
    fn test_wobble_nonzero_when_enabled() {
        let (wx, wy) = compute_wobble(100, 42, 0.15, 40);
        // At least one axis should be non-zero with these params.
        assert!(wx.abs() > 0.0001 || wy.abs() > 0.0001);
    }

    #[test]
    fn test_wobble_zero_when_disabled() {
        let (wx, wy) = compute_wobble(100, 42, 0.0, 40);
        assert!((wx).abs() < 0.0001);
        assert!((wy).abs() < 0.0001);

        let (wx2, wy2) = compute_wobble(100, 42, 0.15, 0);
        assert!((wx2).abs() < 0.0001);
        assert!((wy2).abs() < 0.0001);
    }

    #[test]
    fn test_should_land_balloon_hover_false() {
        let mut loco = make_test_jj_loco();
        loco.balloon_hover = false;
        loco.air_phase = AirMovePhase::Hovering;
        assert!(should_land(&loco));
    }

    #[test]
    fn test_should_not_land_balloon_hover_true() {
        let mut loco = make_test_jj_loco();
        loco.balloon_hover = true;
        loco.air_phase = AirMovePhase::Hovering;
        assert!(!should_land(&loco));
    }

    #[test]
    fn test_infantry_walk_fallback() {
        assert!(should_use_walk_fallback(false, true, 2));
        assert!(should_use_walk_fallback(false, true, 3));
        assert!(!should_use_walk_fallback(false, true, 4));
        assert!(!should_use_walk_fallback(true, true, 2)); // hover_attack blocks fallback
        assert!(!should_use_walk_fallback(false, false, 2)); // not infantry
    }

    #[test]
    fn test_jumpjet_crash_descent() {
        let mut loco = make_test_jj_loco();
        loco.altitude = SimFixed::from_num(500);
        loco.air_phase = AirMovePhase::Descending;
        loco.jumpjet_crash_speed = SimFixed::from_num(150); // (5+5)*15

        tick_jumpjet_altitude(&mut loco, SIM_ONE);
        assert_eq!(
            loco.altitude,
            SimFixed::from_num(350),
            "Should descend at crash speed"
        );
    }

    fn make_test_jj_loco() -> LocomotorState {
        use crate::rules::locomotor_type::{LocomotorKind, SpeedType};
        use crate::sim::movement::locomotor::{GroundMovePhase, MovementLayer};
        LocomotorState {
            kind: LocomotorKind::Jumpjet,
            layer: MovementLayer::Air,
            phase: GroundMovePhase::Idle,
            air_phase: AirMovePhase::Landed,
            speed_multiplier: SIM_ONE,
            speed_fraction: SIM_ONE,
            altitude: SIM_ZERO,
            target_altitude: SimFixed::from_num(500),
            climb_rate: sim_from_f32(75.0),
            jumpjet_speed: SimFixed::from_num(14),
            jumpjet_wobbles: 0.15,
            jumpjet_accel: SimFixed::from_num(2),
            jumpjet_current_speed: SIM_ZERO,
            jumpjet_deviation: 40,
            jumpjet_crash_speed: SimFixed::from_num(150),
            jumpjet_turn_rate: 4,
            balloon_hover: true,
            hover_attack: true,
            speed_type: SpeedType::Track,
            movement_zone: crate::rules::locomotor_type::MovementZone::Normal,
            rot: 0,
            override_state: None,
            air_progress: SIM_ZERO,
            infantry_wobble_phase: 0.0,
            subcell_dest: None,
        }
    }
}
