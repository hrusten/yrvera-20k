//! DropPod locomotor — falling entry controller.
//!
//! Units deployed via drop pod spawn at high altitude and fall to the ground.
//! On landing, the piggyback override is removed and the unit's base locomotor
//! (e.g., Walk or Drive) is restored.
//!
//! ## State machine
//! Falling → Landing → Done (override removed, entity becomes a normal unit)
//!
//! ## RA2 behavior
//! - Drop pods are used for reinforcement paradrop-style entry
//! - Unit spawns at a high altitude above the target cell
//! - Falls with deceleration (slows near ground for impact)
//! - On landing: restore base locomotor via `end_override()`
//! - Visual: altitude offset decreases each tick until 0
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/game_entity, sim/entity_store, sim/locomotor,
//!   map/terrain.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::sim::debug_event_log::DebugEventKind;
use crate::sim::entity_store::EntityStore;
use crate::sim::movement::locomotor::OverrideKind;
use crate::util::fixed_math::{SIM_ZERO, SimFixed, dt_from_tick_ms, sim_to_f32};

/// Initial drop altitude in leptons.
const DROP_ALTITUDE: SimFixed = SimFixed::lit("1200");
/// Base descent speed in leptons per second.
const DESCENT_SPEED: SimFixed = SimFixed::lit("600");
/// Deceleration factor — descent slows as altitude decreases (braking near ground).
const DECEL_ALTITUDE_THRESHOLD: SimFixed = SimFixed::lit("300");
/// Minimum descent speed (even when braking).
const MIN_DESCENT_SPEED: SimFixed = SimFixed::lit("100");
/// Duration of the landing phase in seconds (impact dust/animation).
const LANDING_DURATION_S: SimFixed = SimFixed::lit("0.4");
/// Visual height offset per lepton of altitude (matches air_movement). Render-only.
const ALTITUDE_VISUAL_SCALE: f32 = 0.06;

/// Phase within the drop pod state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DropPodPhase {
    /// Falling from high altitude toward ground.
    Falling,
    /// Just touched down — playing impact animation.
    Landing,
}

/// State for a unit entering via drop pod.
///
/// Set when a unit is deployed via drop pod reinforcement.
/// Removed when the landing animation completes — the unit then
/// reverts to its base locomotor (via the piggyback override system).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DropPodState {
    /// Current phase in the drop sequence.
    pub phase: DropPodPhase,
    /// Current altitude in leptons (starts at DROP_ALTITUDE, decreases to 0).
    pub altitude: SimFixed,
    /// Timer for the landing phase (seconds remaining).
    pub timer: SimFixed,
}

/// Attach a drop pod state to an entity, beginning the falling sequence.
///
/// The entity should already exist in the EntityStore with a position.
/// A piggyback override is applied so the unit behaves as a DropPod during
/// descent, then reverts to its base locomotor (Walk/Drive) on landing.
///
/// Returns `true` if the drop pod state was attached successfully.
pub fn begin_droppod_entry(entities: &mut EntityStore, entity_id: u64) -> bool {
    let Some(entity) = entities.get_mut(entity_id) else {
        return false;
    };

    // Apply piggyback override to temporarily switch to DropPod locomotor.
    if let Some(ref mut loco) = entity.locomotor {
        loco.begin_override(OverrideKind::DropPod);
    }

    entity.droppod_state = Some(DropPodState {
        phase: DropPodPhase::Falling,
        altitude: DROP_ALTITUDE,
        timer: SIM_ZERO,
    });
    entity.push_debug_event(
        0,
        DebugEventKind::SpecialMovementStart {
            kind: "DropPod".into(),
        },
    );
    true
}

/// Advance all in-progress drop pod state machines.
///
/// Called once per simulation tick from `advance_tick()`.
pub fn tick_droppod_movement(entities: &mut EntityStore, tick_ms: u32, sim_tick: u64) {
    if tick_ms == 0 {
        return;
    }
    let dt: SimFixed = dt_from_tick_ms(tick_ms);

    let mut finished: Vec<u64> = Vec::new();

    let keys = entities.keys_sorted();
    for &id in &keys {
        let Some(entity) = entities.get_mut(id) else {
            continue;
        };
        let Some(ref mut droppod) = entity.droppod_state else {
            continue;
        };

        // Track phase before processing to detect transitions.
        let phase_before = droppod.phase;

        match droppod.phase {
            DropPodPhase::Falling => {
                // Compute descent speed with deceleration near ground.
                let speed: SimFixed = if droppod.altitude < DECEL_ALTITUDE_THRESHOLD {
                    let brake_factor: SimFixed = droppod.altitude / DECEL_ALTITUDE_THRESHOLD;
                    (DESCENT_SPEED * brake_factor).max(MIN_DESCENT_SPEED)
                } else {
                    DESCENT_SPEED
                };

                droppod.altitude -= speed * dt;

                if droppod.altitude <= SIM_ZERO {
                    droppod.altitude = SIM_ZERO;
                    droppod.phase = DropPodPhase::Landing;
                    droppod.timer = LANDING_DURATION_S;
                }

                // Update screen position with altitude offset.
                // sim_to_f32 at render boundary — ALTITUDE_VISUAL_SCALE is render-only f32.
                let (sx, sy) = crate::util::lepton::lepton_to_screen(
                    entity.position.rx,
                    entity.position.ry,
                    entity.position.sub_x,
                    entity.position.sub_y,
                    entity.position.z,
                );
                entity.position.screen_x = sx;
                entity.position.screen_y =
                    sy - sim_to_f32(droppod.altitude) * ALTITUDE_VISUAL_SCALE;
            }
            DropPodPhase::Landing => {
                droppod.timer -= dt;
                if droppod.timer <= SIM_ZERO {
                    finished.push(id);
                }

                // Grounded — update screen position without altitude offset.
                let (sx, sy) = crate::util::lepton::lepton_to_screen(
                    entity.position.rx,
                    entity.position.ry,
                    entity.position.sub_x,
                    entity.position.sub_y,
                    entity.position.z,
                );
                entity.position.screen_x = sx;
                entity.position.screen_y = sy;
            }
        }

        // Log phase transition if it changed.
        let phase_after = droppod.phase;
        if phase_after != phase_before {
            let phase_name = format!("{:?}", phase_after);
            let _ = droppod;
            entity.push_debug_event(
                sim_tick as u32,
                DebugEventKind::SpecialMovementPhase { phase: phase_name },
            );
        }
    }

    // Clean up finished drops: remove DropPodState and restore base locomotor.
    for id in finished {
        if let Some(entity) = entities.get_mut(id) {
            entity.droppod_state = None;
            if let Some(ref mut loco) = entity.locomotor {
                if loco.is_overridden() {
                    loco.end_override();
                }
            }
            entity.push_debug_event(sim_tick as u32, DebugEventKind::SpecialMovementEnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::locomotor_type::{LocomotorKind, SpeedType};
    use crate::sim::entity_store::EntityStore;
    use crate::sim::game_entity::GameEntity;
    use crate::sim::movement::locomotor::{
        AirMovePhase, GroundMovePhase, LocomotorState, MovementLayer,
    };
    use crate::util::fixed_math::{SIM_ONE, SIM_ZERO};

    fn make_walk_loco() -> LocomotorState {
        LocomotorState {
            kind: LocomotorKind::Walk,
            layer: MovementLayer::Ground,
            phase: GroundMovePhase::Idle,
            air_phase: AirMovePhase::Landed,
            speed_multiplier: SIM_ONE,
            speed_fraction: SIM_ONE,
            fly_current_speed: SIM_ZERO,
            altitude: SIM_ZERO,
            target_altitude: SIM_ZERO,
            climb_rate: SIM_ZERO,
            jumpjet_speed: SIM_ZERO,
            jumpjet_wobbles: 0.0,
            jumpjet_accel: SIM_ZERO,
            jumpjet_current_speed: SIM_ZERO,
            jumpjet_deviation: 0,
            jumpjet_crash_speed: SIM_ZERO,
            jumpjet_turn_rate: 4,
            balloon_hover: false,
            hover_attack: false,
            speed_type: SpeedType::Foot,
            movement_zone: crate::rules::locomotor_type::MovementZone::Normal,
            rot: 0,
            override_state: None,
            air_progress: SIM_ZERO,
            infantry_wobble_phase: 0.0,
            subcell_dest: None,
        }
    }

    #[test]
    fn test_droppod_full_sequence() {
        let mut entities = EntityStore::new();
        let loco = make_walk_loco();
        let mut e = GameEntity::test_default(1, "E1", "Americans", 10, 10);
        e.locomotor = Some(loco);
        entities.insert(e);

        assert!(begin_droppod_entry(&mut entities, 1));

        // Should have DropPodState and be overridden.
        let entity = entities.get(1).expect("should exist");
        let dp = entity.droppod_state.as_ref().expect("has DropPodState");
        assert_eq!(dp.phase, DropPodPhase::Falling);
        assert_eq!(dp.altitude, DROP_ALTITUDE);

        let loco = entity.locomotor.as_ref().expect("has loco");
        assert!(loco.is_overridden());
        assert_eq!(loco.kind, LocomotorKind::DropPod);

        // Tick through entire descent + landing.
        for _ in 0..200 {
            tick_droppod_movement(&mut entities, 33, 0);
        }

        // Should be done: no DropPodState, locomotor restored to Walk.
        let entity = entities.get(1).expect("should exist");
        assert!(
            entity.droppod_state.is_none(),
            "DropPodState should be removed"
        );
        let loco = entity.locomotor.as_ref().expect("has loco");
        assert_eq!(loco.kind, LocomotorKind::Walk, "Should restore to Walk");
        assert!(!loco.is_overridden());
        assert_eq!(loco.layer, MovementLayer::Ground);
    }

    #[test]
    fn test_droppod_altitude_decreases() {
        let mut entities = EntityStore::new();
        let loco = make_walk_loco();
        let mut e = GameEntity::test_default(1, "E1", "Americans", 5, 5);
        e.locomotor = Some(loco);
        entities.insert(e);

        begin_droppod_entry(&mut entities, 1);

        // Tick a few times — altitude should decrease.
        for _ in 0..5 {
            tick_droppod_movement(&mut entities, 33, 0);
        }

        let entity = entities.get(1).expect("should exist");
        let dp = entity.droppod_state.as_ref().expect("has DropPodState");
        assert!(
            dp.altitude < DROP_ALTITUDE,
            "Altitude should decrease (got {})",
            dp.altitude
        );
        assert!(dp.altitude > SIM_ZERO, "Should not have landed yet");
    }

    #[test]
    fn test_droppod_without_loco_still_works() {
        let mut entities = EntityStore::new();
        // Entity without LocomotorState — should still accept droppod.
        let e = GameEntity::test_default(1, "E1", "Americans", 5, 5);
        entities.insert(e);

        assert!(begin_droppod_entry(&mut entities, 1));

        // Tick through — should complete without panic.
        for _ in 0..200 {
            tick_droppod_movement(&mut entities, 33, 0);
        }

        let entity = entities.get(1).expect("should exist");
        assert!(entity.droppod_state.is_none());
    }
}
