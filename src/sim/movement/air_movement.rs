//! Air movement system — moves Fly and Jumpjet entities each tick.
//!
//! Air units differ from ground movers in several key ways:
//! - They fly in straight lines (no A* pathfinding through terrain).
//! - They have altitude state machines (ascending, cruising, descending).
//! - They don't block ground cells (air layer is separate from ground).
//! - Jumpjets hover at a fixed altitude with optional wobble.
//!
//! ## How it works
//! 1. When an air unit receives a Move command, `issue_air_move_command()`
//!    creates a simple two-cell path (start → goal) and attaches MovementTarget.
//! 2. Each tick, `tick_air_movement()` processes air-layer entities:
//!    - Manages altitude transitions (ascend/descend).
//!    - Advances horizontal position toward the goal (straight line, no occupancy).
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
/// Calibrated so that cruise altitude (600 leptons) produces ~36px vertical
/// offset (about 2.4 cells worth of visual height at HEIGHT_STEP=15).
/// KEPT as f32 — render-only visual scale.
const ALTITUDE_VISUAL_SCALE: f32 = 0.06;

/// Issue a move command for an air unit: straight-line path, no A*.
///
/// Returns true if the command was accepted. Air units always accept moves
/// (no terrain blocking check needed — they fly over everything).
pub fn issue_air_move_command(
    entities: &mut EntityStore,
    entity_id: u64,
    target: (u16, u16),
    speed: SimFixed,
) -> bool {
    let Some(entity) = entities.get(entity_id) else {
        return false;
    };
    let start_rx: u16 = entity.position.rx;
    let start_ry: u16 = entity.position.ry;

    // Already at destination.
    if (start_rx, start_ry) == target {
        return true;
    }

    // Update facing toward destination.
    let dx: i32 = target.0 as i32 - start_rx as i32;
    let dy: i32 = target.1 as i32 - start_ry as i32;
    let new_facing: u8 = facing_from_delta(dx, dy);

    // Generate cell-by-cell waypoints along a straight line (Bresenham).
    // Air units fly in straight lines, so every cell along the path is a waypoint.
    let path: Vec<(u16, u16)> = bresenham_line(start_rx, start_ry, target.0, target.1);
    let path_len = path.len();
    let movement = MovementTarget {
        path,
        path_layers: vec![MovementLayer::Air; path_len],
        next_index: 1,
        speed,
        final_goal: Some(target),
        ..Default::default()
    };

    let Some(entity) = entities.get_mut(entity_id) else {
        return false;
    };
    entity.facing = new_facing;
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

        // --- Horizontal movement (only when airborne or ascending) ---
        let has_movement: bool = entity.movement_target.is_some();

        if has_movement {
            // Only move horizontally when at or above half cruise altitude.
            let can_move_horizontally: bool = entity
                .locomotor
                .as_ref()
                .is_some_and(|l| l.altitude >= l.target_altitude * SIM_HALF);

            if can_move_horizontally {
                if let Some(ref mut target) = entity.movement_target {
                    use fixed::types::I48F16;
                    let final_goal = target.final_goal.unwrap_or_else(|| {
                        *target
                            .path
                            .last()
                            .unwrap_or(&(entity.position.rx, entity.position.ry))
                    });

                    // Lepton math uses I48F16 to avoid I16F16 overflow on large maps.
                    // Cell coordinates * 256 can exceed SimFixed's 32767 max integer.
                    let lep256 = I48F16::from_num(256);
                    let lep128 = I48F16::from_num(128);
                    let goal_lx: I48F16 = I48F16::from_num(final_goal.0) * lep256 + lep128;
                    let goal_ly: I48F16 = I48F16::from_num(final_goal.1) * lep256 + lep128;
                    let cur_lx: I48F16 = I48F16::from_num(entity.position.rx) * lep256
                        + I48F16::from(entity.position.sub_x);
                    let cur_ly: I48F16 = I48F16::from_num(entity.position.ry) * lep256
                        + I48F16::from(entity.position.sub_y);

                    let dlx: I48F16 = goal_lx - cur_lx;
                    let dly: I48F16 = goal_ly - cur_ly;
                    let dist_sq: I48F16 = dlx * dlx + dly * dly;
                    // Newton's method sqrt in I48F16.
                    let dist: I48F16 = if dist_sq <= I48F16::ZERO {
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

                    // Apply mission-controlled speed fraction.
                    let speed_frac: SimFixed = entity
                        .locomotor
                        .as_ref()
                        .map_or(SIM_ONE, |l| l.speed_fraction);
                    // speed is in leptons/sec; multiply by dt to get leptons this tick.
                    let move_lep: I48F16 = I48F16::from(target.speed * speed_frac * dt);
                    let snap_threshold = I48F16::from_num(4);

                    if dist <= move_lep || dist < snap_threshold {
                        // Close enough — snap to destination.
                        entity.position.rx = final_goal.0;
                        entity.position.ry = final_goal.1;
                        entity.position.sub_x = CELL_CENTER;
                        entity.position.sub_y = CELL_CENTER;
                        finished.push(entity_id);
                        stats.arrivals = stats.arrivals.saturating_add(1);
                    } else if dist > I48F16::ZERO {
                        // Move toward goal by move_lep along the heading.
                        let step_x: I48F16 = dlx * move_lep / dist;
                        let step_y: I48F16 = dly * move_lep / dist;
                        let new_lx: I48F16 = cur_lx + step_x;
                        let new_ly: I48F16 = cur_ly + step_y;

                        // Convert back to cell + sub-cell.
                        let new_rx: i32 = (new_lx / lep256).to_num::<i32>();
                        let new_ry: i32 = (new_ly / lep256).to_num::<i32>();
                        entity.position.rx = (new_rx.max(0) as u16).min(511);
                        entity.position.ry = (new_ry.max(0) as u16).min(511);
                        let sub_x: I48F16 = new_lx - I48F16::from_num(entity.position.rx) * lep256;
                        let sub_y: I48F16 = new_ly - I48F16::from_num(entity.position.ry) * lep256;
                        entity.position.sub_x =
                            SimFixed::from_num(sub_x.to_num::<i32>().max(0).min(255));
                        entity.position.sub_y =
                            SimFixed::from_num(sub_y.to_num::<i32>().max(0).min(255));

                        // Update facing toward goal.
                        let face_dx: i32 = final_goal.0 as i32 - entity.position.rx as i32;
                        let face_dy: i32 = final_goal.1 as i32 - entity.position.ry as i32;
                        if face_dx != 0 || face_dy != 0 {
                            entity.facing = facing_from_delta(face_dx, face_dy);
                        }
                    }
                }

                // (air_progress no longer used — lepton interpolation replaces it)
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

/// Generate cell-by-cell waypoints along a straight line using Bresenham's algorithm.
/// Includes both start and end points. Returns at least 2 elements for any non-zero move.
fn bresenham_line(x0: u16, y0: u16, x1: u16, y1: u16) -> Vec<(u16, u16)> {
    let mut points: Vec<(u16, u16)> = Vec::new();
    let mut x = x0 as i32;
    let mut y = y0 as i32;
    let ex = x1 as i32;
    let ey = y1 as i32;
    let dx = (ex - x).abs();
    let dy = -(ey - y).abs();
    let sx: i32 = if x < ex { 1 } else { -1 };
    let sy: i32 = if y < ey { 1 } else { -1 };
    let mut err = dx + dy;

    loop {
        points.push((x as u16, y as u16));
        if x == ex && y == ey {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
    points
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

        // Should have a MovementTarget with cell-by-cell waypoints.
        let e = entities.get(1).expect("has entity");
        let target = e.movement_target.as_ref().expect("has target");
        assert!(
            target.path.len() > 2,
            "path should have intermediate waypoints"
        );
        assert_eq!(target.path[0], (10, 10));
        assert_eq!(*target.path.last().unwrap(), (20, 15));

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
            altitude: SIM_ZERO,
            target_altitude: SimFixed::from_num(600),
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
}
