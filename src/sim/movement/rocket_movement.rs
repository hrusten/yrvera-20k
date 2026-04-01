//! Rocket locomotor — scripted missile controller.
//!
//! Rockets are projectile entities that fly from origin to target in an arc,
//! then detonate (entity despawned). They do NOT use A* pathfinding — movement
//! is a straight line with altitude curve.
//!
//! ## State machine
//! Launch → Ascending → Terminal → Detonation
//!
//! ## RA2 behavior
//! - Rockets spawn at the firing unit's position
//! - Fly in a ballistic arc: ascending phase then terminal dive
//! - Speed comes from the Projectile= entry in rules.ini
//! - Entity is despawned on detonation (damage applied by combat system)
//! - Uses `MovementLayer::Air` — ignores ground occupancy
//!
//! ## Determinism
//! All sim-critical fields (speed, altitude, progress, timer) use `SimFixed`
//! for deterministic lockstep. Only render-only values (pitch, visual scale)
//! remain as f32.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/game_entity, sim/entity_store, map/terrain.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::sim::debug_event_log::DebugEventKind;
use crate::sim::entity_store::EntityStore;
use crate::sim::movement::facing_from_delta;
use crate::util::fixed_math::{
    SIM_ONE, SIM_TWO, SIM_ZERO, SimFixed, dt_from_tick_ms, int_distance_to_sim, sim_to_f32,
};

/// Duration of the initial launch phase in seconds (vertical boost).
const LAUNCH_DURATION_S: SimFixed = SimFixed::lit("0.3");
/// Fraction of total flight distance spent ascending (0.0–1.0).
const ASCEND_FRACTION: SimFixed = SimFixed::lit("0.4");
/// Peak altitude in leptons during the ascending phase.
const PEAK_ALTITUDE: SimFixed = SimFixed::lit("400");
/// Visual height offset per lepton of altitude (matches air_movement).
/// Render-only — kept as f32.
const ALTITUDE_VISUAL_SCALE: f32 = 0.06;

/// Phase within the rocket state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RocketPhase {
    /// Initial vertical boost — rocket rises from launcher.
    Launch,
    /// Climbing toward peak altitude while advancing horizontally.
    Ascending,
    /// Diving toward target — descending altitude, still advancing.
    Terminal,
    /// Impact — entity will be despawned this tick.
    Detonation,
}

/// State for an in-flight rocket/missile.
///
/// Set when a weapon fires a rocket projectile. Removed (along with the
/// entity) when the rocket detonates at the target.
///
/// Sim-critical fields use `SimFixed` for deterministic lockstep.
/// `pitch` is render-only and stays as `f32`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RocketState {
    /// Current phase in the rocket flight.
    pub phase: RocketPhase,
    /// Origin cell coordinates (where the rocket was fired from).
    pub origin_rx: u16,
    pub origin_ry: u16,
    /// Target cell coordinates.
    pub target_rx: u16,
    pub target_ry: u16,
    /// Flight speed in cells per second (SimFixed for determinism).
    pub speed: SimFixed,
    /// Current altitude in leptons (SimFixed for determinism).
    pub altitude: SimFixed,
    /// Progress along the flight path (0.0 = origin, 1.0 = target) (SimFixed for determinism).
    pub progress: SimFixed,
    /// Timer for the launch phase — seconds remaining (SimFixed for determinism).
    pub timer: SimFixed,
    /// Visual pitch angle in radians (positive = nose up, negative = nose down).
    /// Computed from the altitude change rate. Render system reads this for rotation.
    /// Render-only — stays as f32.
    pub pitch: f32,
}

/// Attach a rocket state to an entity at the given origin, targeting a destination cell.
///
/// The entity should already exist in the EntityStore with a position.
/// This function sets the RocketState to begin the flight sequence.
pub fn attach_rocket_state(
    entities: &mut EntityStore,
    entity_id: u64,
    origin: (u16, u16),
    target: (u16, u16),
    speed: SimFixed,
) -> bool {
    let Some(entity) = entities.get_mut(entity_id) else {
        return false;
    };

    // Set initial facing toward target.
    let dx: i32 = target.0 as i32 - origin.0 as i32;
    let dy: i32 = target.1 as i32 - origin.1 as i32;
    entity.facing = facing_from_delta(dx, dy);

    entity.rocket_state = Some(RocketState {
        phase: RocketPhase::Launch,
        origin_rx: origin.0,
        origin_ry: origin.1,
        target_rx: target.0,
        target_ry: target.1,
        speed: speed.max(SIM_ONE),
        altitude: SIM_ZERO,
        progress: SIM_ZERO,
        timer: LAUNCH_DURATION_S,
        pitch: std::f32::consts::FRAC_PI_2, // Nose up during launch.
    });
    entity.push_debug_event(
        0,
        DebugEventKind::SpecialMovementStart {
            kind: "Rocket".into(),
        },
    );
    true
}

/// Advance all in-flight rocket state machines.
///
/// Called once per simulation tick from `advance_tick()`.
/// Rockets that reach Detonation phase are collected for despawning.
/// Returns a list of entity IDs that detonated this tick (caller handles despawn/damage).
pub fn tick_rocket_movement(entities: &mut EntityStore, tick_ms: u32, sim_tick: u64) -> Vec<u64> {
    let mut detonated: Vec<u64> = Vec::new();
    if tick_ms == 0 {
        return detonated;
    }
    let dt: SimFixed = dt_from_tick_ms(tick_ms);

    let keys = entities.keys_sorted();
    for &id in &keys {
        let Some(entity) = entities.get_mut(id) else {
            continue;
        };
        let Some(ref mut rocket) = entity.rocket_state else {
            continue;
        };

        // Track phase before processing to detect transitions.
        let phase_before = rocket.phase;

        match rocket.phase {
            RocketPhase::Launch => {
                // Vertical boost phase — rise in place briefly.
                rocket.timer -= dt;
                rocket.altitude += PEAK_ALTITUDE * dt / LAUNCH_DURATION_S * SimFixed::lit("0.3");
                rocket.pitch = std::f32::consts::FRAC_PI_2; // Nose straight up.
                if rocket.timer <= SIM_ZERO {
                    rocket.phase = RocketPhase::Ascending;
                }
            }
            RocketPhase::Ascending => {
                // Fly toward target while climbing to peak altitude.
                let total_dist: SimFixed = flight_distance(rocket);
                let speed_frac: SimFixed = if total_dist > SIM_ZERO {
                    rocket.speed * dt / total_dist
                } else {
                    SIM_ONE
                };
                let prev_alt: SimFixed = rocket.altitude;
                rocket.progress += speed_frac;

                // Altitude: parabolic curve peaking at ASCEND_FRACTION of flight.
                let alt_frac: SimFixed = rocket.progress / ASCEND_FRACTION;
                rocket.altitude = PEAK_ALTITUDE * parabolic_up(alt_frac.min(SIM_ONE));

                // Pitch from altitude change vs horizontal distance covered.
                // Convert SimFixed values to f32 for atan() — pitch is render-only.
                let alt_delta_f32: f32 = sim_to_f32(rocket.altitude - prev_alt);
                let horiz_delta_f32: f32 = sim_to_f32(speed_frac * total_dist);
                rocket.pitch = if horiz_delta_f32 > 0.001 {
                    (alt_delta_f32 / horiz_delta_f32).atan()
                } else {
                    std::f32::consts::FRAC_PI_2
                };

                if rocket.progress >= ASCEND_FRACTION {
                    rocket.phase = RocketPhase::Terminal;
                }

                update_rocket_position(rocket, &mut entity.position);
            }
            RocketPhase::Terminal => {
                // Dive toward target — descending altitude.
                let total_dist: SimFixed = flight_distance(rocket);
                let speed_frac: SimFixed = if total_dist > SIM_ZERO {
                    rocket.speed * dt / total_dist
                } else {
                    SIM_ONE
                };
                let prev_alt: SimFixed = rocket.altitude;
                rocket.progress += speed_frac;

                // Altitude: descend from peak to 0 over remaining flight.
                let terminal_frac: SimFixed =
                    (rocket.progress - ASCEND_FRACTION) / (SIM_ONE - ASCEND_FRACTION);
                rocket.altitude = PEAK_ALTITUDE * (SIM_ONE - terminal_frac.min(SIM_ONE));

                // Pitch: negative (nose down) during terminal dive.
                // Convert SimFixed values to f32 for atan() — pitch is render-only.
                let alt_delta_f32: f32 = sim_to_f32(rocket.altitude - prev_alt);
                let horiz_delta_f32: f32 = sim_to_f32(speed_frac * total_dist);
                rocket.pitch = if horiz_delta_f32 > 0.001 {
                    (alt_delta_f32 / horiz_delta_f32).atan()
                } else {
                    -std::f32::consts::FRAC_PI_2
                };

                if rocket.progress >= SIM_ONE {
                    // Arrived at target.
                    entity.position.rx = rocket.target_rx;
                    entity.position.ry = rocket.target_ry;
                    rocket.altitude = SIM_ZERO;
                    rocket.pitch = -std::f32::consts::FRAC_PI_2; // Nose down at impact.
                    rocket.phase = RocketPhase::Detonation;
                    detonated.push(id);
                } else {
                    update_rocket_position(rocket, &mut entity.position);
                }
            }
            RocketPhase::Detonation => {
                // Already queued for despawn — no further processing.
                detonated.push(id);
            }
        }

        // Capture altitude and phase change before dropping the rocket borrow.
        let phase_after = rocket.phase;
        let alt_for_screen = rocket.altitude;

        // Update screen coordinates with altitude offset.
        let (sx, sy) = crate::util::lepton::lepton_to_screen(
            entity.position.rx,
            entity.position.ry,
            entity.position.sub_x,
            entity.position.sub_y,
            entity.position.z,
        );
        entity.position.screen_x = sx;
        entity.position.screen_y = sy - sim_to_f32(alt_for_screen) * ALTITUDE_VISUAL_SCALE;

        // Log phase transition if it changed.
        if phase_after != phase_before {
            let phase_name = format!("{:?}", phase_after);
            entity.push_debug_event(
                sim_tick as u32,
                DebugEventKind::SpecialMovementPhase { phase: phase_name },
            );
            // Log end when detonation is reached.
            if phase_after == RocketPhase::Detonation {
                entity.push_debug_event(sim_tick as u32, DebugEventKind::SpecialMovementEnd);
            }
        }
    }

    detonated
}

/// Compute the straight-line distance between rocket origin and target.
/// Uses i32 arithmetic for squaring to avoid I16F16 overflow on large maps.
fn flight_distance(rocket: &RocketState) -> SimFixed {
    let dx: i32 = rocket.target_rx as i32 - rocket.origin_rx as i32;
    let dy: i32 = rocket.target_ry as i32 - rocket.origin_ry as i32;
    int_distance_to_sim(dx, dy)
}

/// Interpolate rocket position along the straight-line path based on progress.
/// Uses SimFixed arithmetic to avoid platform-dependent f32 rounding.
fn update_rocket_position(rocket: &RocketState, pos: &mut crate::sim::components::Position) {
    let progress: SimFixed = rocket.progress;
    let ox: SimFixed = SimFixed::from_num(rocket.origin_rx);
    let oy: SimFixed = SimFixed::from_num(rocket.origin_ry);
    let dx: SimFixed = SimFixed::from_num(rocket.target_rx as i32 - rocket.origin_rx as i32);
    let dy: SimFixed = SimFixed::from_num(rocket.target_ry as i32 - rocket.origin_ry as i32);
    let new_rx_fixed: SimFixed = ox + dx * progress;
    let new_ry_fixed: SimFixed = oy + dy * progress;
    // Clamp to valid u16 map coordinate range.
    pos.rx = new_rx_fixed.to_num::<i32>().clamp(0, u16::MAX as i32) as u16;
    pos.ry = new_ry_fixed.to_num::<i32>().clamp(0, u16::MAX as i32) as u16;
}

/// Parabolic ease-in curve: starts slow, accelerates to peak (0→1 maps to 0→1).
/// Uses SimFixed for deterministic math.
fn parabolic_up(t: SimFixed) -> SimFixed {
    // Simple quadratic: peaks at t=1 with value 1.
    let clamped: SimFixed = t.clamp(SIM_ZERO, SIM_ONE);
    SIM_TWO * clamped - clamped * clamped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::entity_store::EntityStore;
    use crate::sim::game_entity::GameEntity;
    use crate::util::fixed_math::{SIM_ZERO, SimFixed};

    #[test]
    fn test_rocket_full_flight() {
        let mut entities = EntityStore::new();
        let e = GameEntity::test_default(1, "V3RKT", "Soviet", 5, 5);
        entities.insert(e);

        let ok = attach_rocket_state(&mut entities, 1, (5, 5), (20, 5), SimFixed::from_num(30));
        assert!(ok);

        let entity = entities.get(1).expect("should exist");
        let rs = entity.rocket_state.as_ref().expect("has RocketState");
        assert_eq!(rs.phase, RocketPhase::Launch);

        // Tick through entire flight (~15 cells at 30 cells/s = ~0.5s = ~15 ticks at 33ms).
        let mut detonated = false;
        for _ in 0..60 {
            let det = tick_rocket_movement(&mut entities, 33, 0);
            if det.contains(&1) {
                detonated = true;
                break;
            }
        }
        assert!(detonated, "Rocket should have detonated");

        let entity = entities.get(1).expect("should exist");
        assert_eq!(entity.position.rx, 20, "Should be at target X");
        assert_eq!(entity.position.ry, 5, "Should be at target Y");
    }

    #[test]
    fn test_rocket_altitude_arc() {
        let mut entities = EntityStore::new();
        let e = GameEntity::test_default(1, "V3RKT", "Soviet", 0, 0);
        entities.insert(e);

        attach_rocket_state(&mut entities, 1, (0, 0), (30, 0), SimFixed::from_num(15));

        // Tick past launch into ascending.
        for _ in 0..15 {
            tick_rocket_movement(&mut entities, 33, 0);
        }

        let entity = entities.get(1).expect("should exist");
        let rs = entity.rocket_state.as_ref().expect("has RocketState");
        assert!(
            rs.altitude > SIM_ZERO,
            "Altitude should be positive during flight (got {})",
            rs.altitude
        );
        assert!(
            rs.phase == RocketPhase::Ascending || rs.phase == RocketPhase::Terminal,
            "Should be in flight phase, got {:?}",
            rs.phase
        );
    }

    #[test]
    fn test_rocket_facing_toward_target() {
        let mut entities = EntityStore::new();
        let e = GameEntity::test_default(1, "V3RKT", "Soviet", 5, 5);
        entities.insert(e);

        // Target is south (+ry direction) → facing should be ~128.
        attach_rocket_state(&mut entities, 1, (5, 5), (5, 20), SimFixed::from_num(10));
        let entity = entities.get(1).expect("should exist");
        assert_eq!(entity.facing, 128, "Should face south toward target");
    }

    #[test]
    fn test_rocket_zero_distance() {
        let mut entities = EntityStore::new();
        let e = GameEntity::test_default(1, "V3RKT", "Soviet", 5, 5);
        entities.insert(e);

        attach_rocket_state(&mut entities, 1, (5, 5), (5, 5), SimFixed::from_num(10));

        // Should detonate quickly even with zero distance.
        let mut detonated = false;
        for _ in 0..30 {
            let det = tick_rocket_movement(&mut entities, 33, 0);
            if det.contains(&1) {
                detonated = true;
                break;
            }
        }
        assert!(detonated, "Rocket should detonate even with zero distance");
    }
}
