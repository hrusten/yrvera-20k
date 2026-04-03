//! Air movement system — moves Fly and Jumpjet entities each tick.
//!
//! Air units differ from ground movers in several key ways:
//! - Fly units use **facing-based** movement: they fly in the direction they
//!   face, gradually turning toward the goal via ROT. This produces curved
//!   approach paths matching the original FlyLocomotionClass.
//! - They have altitude state machines (ascending, cruising, descending).
//! - They don't block ground cells (air layer is separate from ground).
//! - Jumpjets hover at a fixed altitude with optional wobble.
//!
//! ## How it works
//! 1. When an air unit receives a Move command, `issue_air_move_command()`
//!    stores the final_goal and attaches a MovementTarget.
//! 2. Each tick, `tick_air_movement()` processes air-layer entities:
//!    - Manages altitude transitions (ascend/descend).
//!    - Turns facing toward the goal by ROT per tick.
//!    - Computes approach speed zones based on distance to goal.
//!    - Moves in the entity's facing direction (not directly toward the goal).
//!    - Ramps `fly_current_speed` toward `speed_fraction` (target) by 0.1/tick.
//!    - Detects arrival when close AND speed is near zero.
//!    - Updates screen coordinates from iso position + altitude offset.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/components, sim/locomotor, map/terrain.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::rules::locomotor_type::LocomotorKind;
use crate::sim::components::MovementTarget;
use crate::sim::debug_event_log::DebugEventKind;
use crate::sim::entity_store::EntityStore;
use crate::sim::movement::facing_from_delta;
use crate::sim::movement::jumpjet_movement;
use crate::sim::movement::locomotor::{AirMovePhase, LocomotorState, MovementLayer};
use crate::util::fixed_math::{SIM_HALF, SIM_ONE, SIM_ZERO, SimFixed, sim_to_f32};
use crate::util::lepton::CELL_CENTER_LEPTON as CELL_CENTER;

/// Visual height offset per lepton of altitude.
/// Calibrated so that cruise altitude (1500 leptons) produces ~90px vertical
/// offset. KEPT as f32 — render-only visual scale.
const ALTITUDE_VISUAL_SCALE: f32 = 0.06;

/// Per-tick speed ramp step for Fly aircraft (0.1 per tick).
/// Original: _DAT_007e3860 = 0.1 (verified from binary).
/// Full acceleration 0->1 takes 10 ticks.
const FLY_SPEED_RAMP_STEP: SimFixed = SimFixed::lit("0.1");

/// Fine approach deceleration threshold in leptons (~1/3 cell).
/// Below this distance, speed is halved each tick for smooth landing.
const FINE_APPROACH_THRESHOLD: i32 = 86;

/// Speed halving factor for fine approach deceleration.
const RAPID_DECEL_FACTOR: SimFixed = SimFixed::lit("0.5");

/// Minimum creep speed to prevent zero-speed deadlock during final approach.
const MIN_CREEP_SPEED: SimFixed = SimFixed::lit("0.05");

/// Ramp fly_current_speed toward speed_fraction (target) by +/-FLY_SPEED_RAMP_STEP.
fn ramp_fly_speed(loco: &mut LocomotorState) {
    let target = loco.speed_fraction;
    let current = loco.fly_current_speed;
    if current < target {
        loco.fly_current_speed = (current + FLY_SPEED_RAMP_STEP).min(target);
    } else if current > target {
        loco.fly_current_speed = (current - FLY_SPEED_RAMP_STEP).max(target);
    }
}

/// Distance-based approach speed zones matching original Horizontal_Step.
/// Returns the target speed fraction for the given distance in leptons.
///
/// | Distance          | TargetSpeed |
/// |-------------------|-------------|
/// | >= 768 (3 cells)  | 1.0         |
/// | >= 512 (2 cells)  | 0.75        |
/// | >= 128 (0.5 cell) | 0.5         |
/// | < 128             | 0.0         |
fn approach_target_speed(dist_leptons: i32) -> SimFixed {
    if dist_leptons >= 768 {
        SIM_ONE
    } else if dist_leptons >= 512 {
        SimFixed::lit("0.75")
    } else if dist_leptons >= 128 {
        SIM_HALF
    } else {
        SIM_ZERO
    }
}

/// Turn facing toward desired by at most `rot` steps per tick.
/// Returns the new facing. Handles wrapping around 0/255.
fn turn_facing_toward(current: u8, desired: u8, rot: i32) -> u8 {
    if rot <= 0 || current == desired {
        return desired; // instant turn or already aligned
    }
    let diff = desired.wrapping_sub(current) as i8;
    let abs_diff = (diff as i16).unsigned_abs() as i32;
    if abs_diff <= rot {
        return desired; // close enough, snap
    }
    // Turn by rot in the shorter direction.
    if diff > 0 {
        current.wrapping_add(rot as u8)
    } else {
        current.wrapping_sub(rot as u8)
    }
}

/// Issue a move command for an air unit.
///
/// Returns true if the command was accepted. Air units always accept moves
/// (no terrain blocking check needed — they fly over everything).
///
/// For Fly units, no Bresenham path is generated — movement direction comes
/// from the entity's facing, which is gradually turned toward the goal each
/// tick via ROT. This produces curved approach paths matching the original
/// FlyLocomotionClass.
pub fn issue_air_move_command(
    entities: &mut EntityStore,
    entity_id: u64,
    target: (u16, u16),
    speed: SimFixed,
) -> bool {
    let Some(entity) = entities.get(entity_id) else {
        return false;
    };
    if (entity.position.rx, entity.position.ry) == target {
        return true;
    }

    // Minimal MovementTarget — only final_goal matters for Fly units.
    // No Bresenham path needed; movement direction comes from facing.
    let movement = MovementTarget {
        path: vec![target],
        path_layers: vec![MovementLayer::Air],
        next_index: 0,
        speed,
        final_goal: Some(target),
        ..Default::default()
    };

    let Some(entity) = entities.get_mut(entity_id) else {
        return false;
    };
    entity.movement_target = Some(movement);

    // Trigger takeoff if on the ground.
    if let Some(ref mut loco) = entity.locomotor {
        if loco.air_phase == AirMovePhase::Landed {
            loco.air_phase = AirMovePhase::Ascending;
        }
    }
    true
}

/// Per-tick stats for air movement diagnostics.
#[derive(Debug, Clone, Copy, Default)]
pub struct AirMovementTickStats {
    /// Number of air entities processed.
    pub air_movers: u32,
    /// Number that completed their move this tick.
    pub arrivals: u32,
}

/// Advance all air-layer entities (Fly/Jumpjet) one tick.
///
/// Handles altitude changes and horizontal movement. Air units move in
/// straight lines at their speed, ignoring terrain and ground occupancy.
pub fn tick_air_movement(
    entities: &mut EntityStore,
    tick_ms: u32,
    sim_tick: u64,
) -> AirMovementTickStats {
    let mut stats = AirMovementTickStats::default();
    if tick_ms == 0 {
        return stats;
    }
    let dt: SimFixed = crate::util::fixed_math::dt_from_tick_ms(tick_ms);

    // Collect air entity IDs that need processing.
    let air_entity_ids: Vec<u64> = {
        let keys = entities.keys_sorted();
        keys.into_iter()
            .filter(|&id| {
                entities.get(id).is_some_and(|e| {
                    e.locomotor.as_ref().is_some_and(|loco| {
                        loco.layer == MovementLayer::Air && loco.kind != LocomotorKind::Rocket
                    })
                })
            })
            .collect()
    };

    let mut finished: Vec<u64> = Vec::new();

    for &entity_id in &air_entity_ids {
        stats.air_movers = stats.air_movers.saturating_add(1);

        let Some(entity) = entities.get_mut(entity_id) else {
            continue;
        };

        // --- Altitude state machine ---
        let Some(ref mut loco) = entity.locomotor else {
            continue;
        };

        let air_phase_before = loco.air_phase;
        let is_jumpjet: bool = loco.kind == LocomotorKind::Jumpjet;
        if is_jumpjet {
            jumpjet_movement::tick_jumpjet_altitude(loco, dt);
            let has_mt: bool = entity.movement_target.is_some();
            jumpjet_movement::tick_jumpjet_acceleration(loco, dt, has_mt);
        } else {
            tick_altitude(loco, dt);
        }
        let air_phase_after = loco.air_phase;
        if air_phase_after != air_phase_before {
            let from = format!("{:?}", air_phase_before);
            let to = format!("{:?}", air_phase_after);
            let _ = loco;
            entity.push_debug_event(
                sim_tick as u32,
                DebugEventKind::PhaseChange {
                    from,
                    to,
                    reason: "altitude change".into(),
                },
            );
        }

        // --- Horizontal movement (facing-based, only when airborne) ---
        let has_movement: bool = entity.movement_target.is_some();

        if has_movement {
            let can_move: bool = entity
                .locomotor
                .as_ref()
                .is_some_and(|l| l.altitude >= l.target_altitude * SIM_HALF);

            if can_move {
                let final_goal = entity
                    .movement_target
                    .as_ref()
                    .and_then(|t| t.final_goal)
                    .unwrap_or((entity.position.rx, entity.position.ry));

                // Compute distance to goal in leptons.
                use fixed::types::I48F16;
                let lep256 = I48F16::from_num(256);
                let lep128 = I48F16::from_num(128);
                let goal_lx = I48F16::from_num(final_goal.0) * lep256 + lep128;
                let goal_ly = I48F16::from_num(final_goal.1) * lep256 + lep128;
                let cur_lx = I48F16::from_num(entity.position.rx) * lep256
                    + I48F16::from(entity.position.sub_x);
                let cur_ly = I48F16::from_num(entity.position.ry) * lep256
                    + I48F16::from(entity.position.sub_y);
                let dlx = goal_lx - cur_lx;
                let dly = goal_ly - cur_ly;
                let dist_sq = dlx * dlx + dly * dly;
                let dist = if dist_sq <= I48F16::ZERO {
                    I48F16::ZERO
                } else {
                    let two = I48F16::from_num(2);
                    let mut g = dist_sq / two;
                    for _ in 0..20 {
                        if g <= I48F16::ZERO {
                            break;
                        }
                        g = (g + dist_sq / g) / two;
                    }
                    g
                };
                let dist_i32: i32 = dist.to_num::<i32>();

                // 1. Compute desired facing toward goal.
                let face_dx = final_goal.0 as i32 - entity.position.rx as i32;
                let face_dy = final_goal.1 as i32 - entity.position.ry as i32;
                let desired_facing = if face_dx != 0 || face_dy != 0 {
                    facing_from_delta(face_dx, face_dy)
                } else {
                    entity.facing
                };

                // 2. Gradually turn toward desired facing (ROT per tick).
                let rot = entity.locomotor.as_ref().map_or(0, |l| l.rot);
                entity.facing = turn_facing_toward(entity.facing, desired_facing, rot);

                // 3. Set approach target speed based on distance.
                let approach_speed = approach_target_speed(dist_i32);
                if let Some(ref mut loco) = entity.locomotor {
                    // Only lower speed_fraction for approach; missions can set it
                    // higher (e.g., full speed during attack run).
                    loco.speed_fraction = loco.speed_fraction.min(approach_speed);
                }

                // 4. Fine approach deceleration.
                if dist_i32 < FINE_APPROACH_THRESHOLD {
                    if let Some(ref mut loco) = entity.locomotor {
                        loco.fly_current_speed = loco.fly_current_speed * RAPID_DECEL_FACTOR;
                        if loco.fly_current_speed < MIN_CREEP_SPEED && dist_i32 > 0 {
                            loco.fly_current_speed = MIN_CREEP_SPEED;
                        }
                    }
                }

                // 5. Move in FACING direction (not toward goal).
                let fly_speed = entity
                    .locomotor
                    .as_ref()
                    .map_or(SIM_ZERO, |l| l.fly_current_speed);
                if let Some(ref target) = entity.movement_target {
                    let move_lep = target.speed * fly_speed * dt;
                    if move_lep > SIM_ZERO {
                        let (step_x, step_y) =
                            crate::util::facing_table::facing_to_movement(entity.facing, move_lep);
                        let new_lx = cur_lx + I48F16::from(step_x);
                        let new_ly = cur_ly + I48F16::from(step_y);
                        let new_rx = (new_lx / lep256).to_num::<i32>();
                        let new_ry = (new_ly / lep256).to_num::<i32>();
                        entity.position.rx = (new_rx.max(0) as u16).min(511);
                        entity.position.ry = (new_ry.max(0) as u16).min(511);
                        let sub_x = new_lx - I48F16::from_num(entity.position.rx) * lep256;
                        let sub_y = new_ly - I48F16::from_num(entity.position.ry) * lep256;
                        entity.position.sub_x =
                            SimFixed::from_num(sub_x.to_num::<i32>().max(0).min(255));
                        entity.position.sub_y =
                            SimFixed::from_num(sub_y.to_num::<i32>().max(0).min(255));
                    }
                }

                // 6. Arrival detection: close enough AND speed near zero.
                let arrived = dist_i32 < 128
                    && entity
                        .locomotor
                        .as_ref()
                        .is_some_and(|l| l.fly_current_speed < MIN_CREEP_SPEED);
                if arrived {
                    entity.position.rx = final_goal.0;
                    entity.position.ry = final_goal.1;
                    entity.position.sub_x = CELL_CENTER;
                    entity.position.sub_y = CELL_CENTER;
                    finished.push(entity_id);
                    stats.arrivals = stats.arrivals.saturating_add(1);
                }
            }
        }

        // Speed ramping for Fly aircraft (after altitude and movement).
        if !is_jumpjet {
            if let Some(ref mut loco) = entity.locomotor {
                ramp_fly_speed(loco);
            }
        }

        // Update screen position including altitude visual offset.
        let alt_f32: f32 = entity
            .locomotor
            .as_ref()
            .map(|l| sim_to_f32(l.altitude))
            .unwrap_or(0.0);
        let (sx, sy) = crate::util::lepton::lepton_to_screen(
            entity.position.rx,
            entity.position.ry,
            entity.position.sub_x,
            entity.position.sub_y,
            entity.position.z,
        );
        entity.position.screen_x = sx;
        // Altitude lifts the unit visually upward (negative Y in screen space).
        entity.position.screen_y = sy - alt_f32 * ALTITUDE_VISUAL_SCALE;
    }

    // Remove MovementTarget from arrived air units and update air phase.
    for &entity_id in &finished {
        let Some(entity) = entities.get_mut(entity_id) else {
            continue;
        };
        entity.movement_target = None;
        // Start descending for Fly units; Jumpjets hover or land based on BalloonHover.
        if let Some(ref mut loco) = entity.locomotor {
            let phase_before = loco.air_phase;
            match loco.kind {
                LocomotorKind::Fly => {
                    // Fly units stay at altitude until given another order.
                    loco.air_phase = AirMovePhase::Cruising;
                }
                LocomotorKind::Jumpjet => {
                    loco.air_phase = AirMovePhase::Hovering;
                    // BalloonHover=false: begin landing immediately after arrival.
                    if jumpjet_movement::should_land(loco) {
                        loco.air_phase = AirMovePhase::Descending;
                    }
                }
                _ => {}
            }
            let phase_after = loco.air_phase;
            if phase_after != phase_before {
                let from = format!("{:?}", phase_before);
                let to = format!("{:?}", phase_after);
                let _ = loco;
                entity.push_debug_event(
                    sim_tick as u32,
                    DebugEventKind::PhaseChange {
                        from,
                        to,
                        reason: "arrival phase transition".into(),
                    },
                );
            }
        }
    }

    // Tick idle air entities (no MovementTarget) — maintain altitude state.
    // Jumpjets with no order stay hovering; Fly units without order stay at altitude.
    for &entity_id in &air_entity_ids {
        let Some(entity) = entities.get_mut(entity_id) else {
            continue;
        };
        if entity.movement_target.is_some() {
            continue; // Already processed above.
        }
        if let Some(ref mut loco) = entity.locomotor {
            let idle_phase_before = loco.air_phase;
            if loco.kind == LocomotorKind::Jumpjet {
                // Idle jumpjets: ascend to hover altitude if not already there.
                if loco.air_phase == AirMovePhase::Landed {
                    loco.air_phase = AirMovePhase::Ascending;
                }
                // BalloonHover=false idle jumpjets begin landing.
                if jumpjet_movement::should_land(loco) {
                    loco.air_phase = AirMovePhase::Descending;
                }
                jumpjet_movement::tick_jumpjet_altitude(loco, dt);
                // Decelerate to zero while idle.
                jumpjet_movement::tick_jumpjet_acceleration(loco, dt, false);
            } else {
                tick_altitude(loco, dt);
            }
            let idle_phase_after = loco.air_phase;
            if idle_phase_after != idle_phase_before {
                let from = format!("{:?}", idle_phase_before);
                let to = format!("{:?}", idle_phase_after);
                let _ = loco;
                entity.push_debug_event(
                    sim_tick as u32,
                    DebugEventKind::PhaseChange {
                        from,
                        to,
                        reason: "altitude change".into(),
                    },
                );
            }
        }
        // Update screen position for idle air entities too (altitude may be changing).
        let alt_f32: f32 = entity
            .locomotor
            .as_ref()
            .map(|l| sim_to_f32(l.altitude))
            .unwrap_or(0.0);
        let (sx, sy) = crate::util::lepton::lepton_to_screen(
            entity.position.rx,
            entity.position.ry,
            entity.position.sub_x,
            entity.position.sub_y,
            entity.position.z,
        );
        entity.position.screen_x = sx;
        entity.position.screen_y = sy - alt_f32 * ALTITUDE_VISUAL_SCALE;
    }

    stats
}

/// Advance the altitude state machine for one air entity.
fn tick_altitude(loco: &mut LocomotorState, dt: SimFixed) {
    match loco.air_phase {
        AirMovePhase::Ascending => {
            loco.altitude += loco.climb_rate * dt;
            if loco.altitude >= loco.target_altitude {
                loco.altitude = loco.target_altitude;
                // Transition to appropriate cruising/hovering phase.
                match loco.kind {
                    crate::rules::locomotor_type::LocomotorKind::Jumpjet => {
                        loco.air_phase = AirMovePhase::Hovering;
                    }
                    _ => {
                        loco.air_phase = AirMovePhase::Cruising;
                    }
                }
            }
        }
        AirMovePhase::Descending => {
            let before = loco.altitude;
            loco.altitude -= loco.climb_rate * dt;
            // Partial descent (dive bombing): if we crossed target_altitude from above,
            // stop there instead of going to ground.
            if loco.target_altitude > SIM_ZERO
                && before > loco.target_altitude
                && loco.altitude <= loco.target_altitude
            {
                loco.altitude = loco.target_altitude;
                loco.air_phase = AirMovePhase::Cruising;
            } else if loco.altitude <= SIM_ZERO {
                loco.altitude = SIM_ZERO;
                loco.air_phase = AirMovePhase::Landed;
            }
        }
        AirMovePhase::Cruising | AirMovePhase::Hovering => {
            // If target altitude changed (dive bombing or recovery), adjust.
            let tolerance = SimFixed::from_num(10);
            if loco.altitude > loco.target_altitude + tolerance {
                // Descend toward new lower target.
                loco.altitude -= loco.climb_rate * dt;
                if loco.altitude <= loco.target_altitude {
                    loco.altitude = loco.target_altitude;
                }
            } else if loco.altitude < loco.target_altitude - tolerance {
                // Ascend toward new higher target (restoring from dive).
                loco.altitude += loco.climb_rate * dt;
                if loco.altitude >= loco.target_altitude {
                    loco.altitude = loco.target_altitude;
                }
            } else {
                loco.altitude = loco.target_altitude;
            }
        }
        AirMovePhase::Landed => {
            // On the ground — nothing to do.
            loco.altitude = SIM_ZERO;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::game_entity::GameEntity;
    use crate::util::fixed_math::sim_from_f32;

    #[test]
    fn test_altitude_ascending() {
        let mut loco = make_fly_loco();
        loco.air_phase = AirMovePhase::Ascending;
        loco.target_altitude = SimFixed::from_num(600);
        loco.climb_rate = SimFixed::from_num(300);
        loco.altitude = SIM_ZERO;

        // After 1 second, should be at 300 leptons.
        tick_altitude(&mut loco, SIM_ONE);
        assert_eq!(loco.altitude, SimFixed::from_num(300));
        assert_eq!(loco.air_phase, AirMovePhase::Ascending);

        // After another 1 second, should reach 600 and transition to Cruising.
        tick_altitude(&mut loco, SIM_ONE);
        assert_eq!(loco.altitude, SimFixed::from_num(600));
        assert_eq!(loco.air_phase, AirMovePhase::Cruising);
    }

    #[test]
    fn test_altitude_descending() {
        let mut loco = make_fly_loco();
        loco.air_phase = AirMovePhase::Descending;
        loco.altitude = SimFixed::from_num(300);
        loco.climb_rate = SimFixed::from_num(300);

        tick_altitude(&mut loco, SIM_ONE);
        assert_eq!(loco.altitude, SIM_ZERO);
        assert_eq!(loco.air_phase, AirMovePhase::Landed);
    }

    #[test]
    fn test_jumpjet_ascends_to_hovering() {
        let mut loco = make_jumpjet_loco();
        loco.air_phase = AirMovePhase::Ascending;
        loco.altitude = SimFixed::from_num(400);
        loco.target_altitude = SimFixed::from_num(500);
        loco.climb_rate = SimFixed::from_num(150);

        // 1 second at 150/s should overshoot 500, clamped.
        tick_altitude(&mut loco, SIM_ONE);
        assert_eq!(loco.altitude, SimFixed::from_num(500));
        assert_eq!(loco.air_phase, AirMovePhase::Hovering);
    }

    #[test]
    fn test_issue_air_move_command() {
        let mut entities = EntityStore::new();
        let mut entity = GameEntity::test_default(1, "ORCA", "Americans", 10, 10);
        entity.locomotor = Some(make_fly_loco());
        entities.insert(entity);

        let ok = issue_air_move_command(&mut entities, 1, (20, 15), SimFixed::from_num(10));
        assert!(ok);

        // Should have a MovementTarget with final_goal set.
        let e = entities.get(1).expect("has entity");
        let target = e.movement_target.as_ref().expect("has target");
        assert_eq!(target.final_goal, Some((20, 15)));
        // Path contains only the destination (no Bresenham).
        assert_eq!(target.path.len(), 1);
        assert_eq!(target.path[0], (20, 15));

        // Should trigger ascending.
        let loco = e.locomotor.as_ref().expect("has loco");
        assert_eq!(loco.air_phase, AirMovePhase::Ascending);
    }

    #[test]
    fn test_issue_air_move_already_at_target() {
        let mut entities = EntityStore::new();
        let mut entity = GameEntity::test_default(1, "ORCA", "Americans", 10, 10);
        entity.locomotor = Some(make_fly_loco());
        entities.insert(entity);

        let ok = issue_air_move_command(&mut entities, 1, (10, 10), SimFixed::from_num(10));
        assert!(ok);
        // No MovementTarget should be added — already at goal.
        let e = entities.get(1).expect("has entity");
        assert!(e.movement_target.is_none());
    }

    fn make_fly_loco() -> LocomotorState {
        LocomotorState {
            kind: crate::rules::locomotor_type::LocomotorKind::Fly,
            layer: MovementLayer::Air,
            phase: crate::sim::movement::locomotor::GroundMovePhase::Idle,
            air_phase: AirMovePhase::Landed,
            speed_multiplier: SIM_ONE,
            speed_fraction: SIM_ONE,
            fly_current_speed: SIM_ZERO,
            altitude: SIM_ZERO,
            target_altitude: SimFixed::from_num(1500),
            climb_rate: SimFixed::from_num(300),
            jumpjet_speed: SIM_ZERO,
            jumpjet_wobbles: 0.0,
            jumpjet_accel: SIM_ZERO,
            jumpjet_current_speed: SIM_ZERO,
            jumpjet_deviation: 0,
            jumpjet_crash_speed: SIM_ZERO,
            jumpjet_turn_rate: 4,
            balloon_hover: false,
            hover_attack: false,
            speed_type: crate::rules::locomotor_type::SpeedType::Track,
            movement_zone: crate::rules::locomotor_type::MovementZone::Normal,
            rot: 0,
            override_state: None,
            air_progress: SIM_ZERO,
            infantry_wobble_phase: 0.0,
            subcell_dest: None,
        }
    }

    fn make_jumpjet_loco() -> LocomotorState {
        LocomotorState {
            kind: crate::rules::locomotor_type::LocomotorKind::Jumpjet,
            layer: MovementLayer::Air,
            phase: crate::sim::movement::locomotor::GroundMovePhase::Idle,
            air_phase: AirMovePhase::Landed,
            speed_multiplier: SIM_ONE,
            speed_fraction: SIM_ONE,
            fly_current_speed: SIM_ZERO,
            altitude: SIM_ZERO,
            target_altitude: SimFixed::from_num(500),
            climb_rate: sim_from_f32(75.0),
            jumpjet_speed: SimFixed::from_num(14),
            jumpjet_wobbles: 0.15,
            jumpjet_accel: SimFixed::from_num(2),
            jumpjet_current_speed: SIM_ZERO,
            jumpjet_deviation: 40,
            jumpjet_crash_speed: SimFixed::from_num(150), // (5+5)*15
            jumpjet_turn_rate: 4,
            balloon_hover: true,
            hover_attack: true,
            speed_type: crate::rules::locomotor_type::SpeedType::Track,
            movement_zone: crate::rules::locomotor_type::MovementZone::Normal,
            rot: 0,
            override_state: None,
            air_progress: SIM_ZERO,
            infantry_wobble_phase: 0.0,
            subcell_dest: None,
        }
    }

    #[test]
    fn test_fly_speed_ramp() {
        let mut loco = make_fly_loco();
        loco.speed_fraction = SIM_ONE; // target = 1.0
        loco.fly_current_speed = SIM_ZERO; // start at 0
        // After 5 ramps: should be ~0.5 (fixed-point 0.1 is approximate).
        for _ in 0..5 {
            ramp_fly_speed(&mut loco);
        }
        let half_diff = (loco.fly_current_speed - SIM_HALF).abs();
        assert!(
            half_diff < SimFixed::lit("0.001"),
            "Expected ~0.5, got {:?}",
            loco.fly_current_speed
        );
        // After 5 more: should reach exactly 1.0 (clamped by min(target)).
        for _ in 0..5 {
            ramp_fly_speed(&mut loco);
        }
        assert_eq!(loco.fly_current_speed, SIM_ONE);
    }

    #[test]
    fn test_fly_speed_ramp_decel() {
        let mut loco = make_fly_loco();
        loco.speed_fraction = SIM_ZERO; // target = 0.0
        loco.fly_current_speed = SIM_ONE; // start at 1.0
        for _ in 0..10 {
            ramp_fly_speed(&mut loco);
        }
        assert_eq!(loco.fly_current_speed, SIM_ZERO);
    }

    #[test]
    fn test_turn_facing_toward() {
        // Turn from 0 toward 10 with rot=3: should go 0 -> 3
        assert_eq!(turn_facing_toward(0, 10, 3), 3);
        // Turn from 0 toward 2 with rot=3: snap to 2
        assert_eq!(turn_facing_toward(0, 2, 3), 2);
        // Turn from 0 toward 250 (shorter path is clockwise-negative, wrapping):
        // diff = 250u8.wrapping_sub(0) = 250, as i8 = -6.
        // abs_diff = 6, > rot=3. diff < 0 so subtract: 0.wrapping_sub(3) = 253
        assert_eq!(turn_facing_toward(0, 250, 3), 253);
        // rot=0: instant snap
        assert_eq!(turn_facing_toward(50, 200, 0), 200);
        // Already aligned
        assert_eq!(turn_facing_toward(128, 128, 5), 128);
    }

    #[test]
    fn test_approach_speed_zones() {
        assert_eq!(approach_target_speed(1000), SIM_ONE);
        assert_eq!(approach_target_speed(768), SIM_ONE);
        assert_eq!(approach_target_speed(600), SimFixed::lit("0.75"));
        assert_eq!(approach_target_speed(512), SimFixed::lit("0.75"));
        assert_eq!(approach_target_speed(300), SIM_HALF);
        assert_eq!(approach_target_speed(128), SIM_HALF);
        assert_eq!(approach_target_speed(100), SIM_ZERO);
        assert_eq!(approach_target_speed(0), SIM_ZERO);
    }
}
