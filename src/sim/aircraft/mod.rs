//! Aircraft mission state machines — orchestrates attack runs, guard/RTB,
//! movement, and idle behavior for Fly-locomotor aircraft.
//!
//! This module implements the mission layer that sits between air movement
//! physics (air_movement.rs) and combat firing (combat/). Missions control
//! WHEN aircraft fire, HOW they approach targets, and WHAT they do after
//! completing an attack pass.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/components, sim/combat, sim/docking, rules/.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

pub mod attack_mission;
pub mod idle_mode;

use serde::{Deserialize, Serialize};

use crate::map::entities::EntityCategory;
use crate::rules::locomotor_type::LocomotorKind;
use crate::rules::ruleset::RuleSet;
use crate::sim::combat::AttackTarget;
use crate::sim::movement::air_movement;
use crate::sim::movement::locomotor::AirMovePhase;
use crate::sim::production::foundation_dimensions;
use crate::sim::world::Simulation;
use crate::util::fixed_math::{SIM_ONE, SIM_ZERO, SimFixed};

/// Aircraft mission — determines the high-level behavior each tick.
///
/// Replaces the original engine's MissionClass dispatch for aircraft.
/// Each variant carries its own sub-state for the state machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AircraftMission {
    /// Idle on the ground or hovering — waiting for orders.
    /// BalloonHover aircraft idle at cruise altitude.
    Idle,

    /// Flying toward a destination (player Move command).
    Move {
        /// 0=init, 1=set_course, 2=in_flight, 3=arrived, 4=course_correction
        sub_state: u8,
    },

    /// Attacking a target — 11-state machine from gamemd.exe.
    Attack {
        /// State within the attack state machine (0-10).
        sub_state: u8,
        /// Set to true when weapon fires during this attack pass.
        /// Ammo is decremented at the START of the next state transition,
        /// not when Fire_At is called. This ensures exactly one ammo per pass.
        has_fired: bool,
        /// Set during strafing attack runs (states 6-9).
        /// Controls whether the aircraft continues forward after firing.
        is_strafe: bool,
    },

    /// Guard — idle in the air, scanning for targets, RTB when low ammo.
    Guard,

    /// Returning to base — flying toward airfield for reload.
    /// Absorbed from the old AircraftDockPhase::ReturnToBase.
    ReturnToBase {
        /// Target airfield entity stable_id.
        airfield_id: u64,
    },

    /// Docking at an airfield — descending, reloading, launching.
    Docking {
        /// Target airfield entity stable_id.
        airfield_id: u64,
        /// 0=wait_for_dock, 1=descending, 2=reloading, 3=launching
        sub_state: u8,
        /// Ticks remaining until next ammo point is restored (during reloading).
        reload_timer: u32,
    },

    /// Parked on helipad pad — freshly built, waiting for player command.
    /// Dock slot is reserved. Aircraft is Landed, altitude 0.
    /// Exits via Move/Attack command (releases dock, triggers takeoff).
    DockedIdle {
        /// Airfield entity stable_id this aircraft is docked at.
        airfield_id: u64,
    },
}

impl AircraftMission {
    /// Whether this mission is an active attack (any Attack sub-state).
    pub fn is_attacking(&self) -> bool {
        matches!(self, AircraftMission::Attack { .. })
    }

    /// Whether this mission involves docking or returning to base.
    pub fn is_rtb_or_docking(&self) -> bool {
        matches!(
            self,
            AircraftMission::ReturnToBase { .. } | AircraftMission::Docking { .. }
        )
    }

    /// Whether this aircraft is parked on a helipad waiting for orders.
    pub fn is_docked_idle(&self) -> bool {
        matches!(self, AircraftMission::DockedIdle { .. })
    }
}

/// Advance aircraft mission state machines for all Fly-locomotor aircraft.
///
/// Called once per tick from `advance_tick()`, after air_movement and before combat.
/// This is the mission orchestration layer — it decides when aircraft fire,
/// where they fly, and what they do after completing an attack pass.
pub fn tick_aircraft_missions(sim: &mut Simulation, rules: &RuleSet) {
    // Phase 1: Snapshot all aircraft with missions.
    struct MissionSnap {
        id: u64,
        mission: AircraftMission,
    }

    let snapshots: Vec<MissionSnap> = sim
        .entities
        .values()
        .filter_map(|e| {
            let mission = e.aircraft_mission.as_ref()?;
            let loco = e.locomotor.as_ref()?;
            if loco.kind != LocomotorKind::Fly {
                return None;
            }
            Some(MissionSnap {
                id: e.stable_id,
                mission: mission.clone(),
            })
        })
        .collect();

    if snapshots.is_empty() {
        return;
    }

    // Phase 2: Process each aircraft through its mission handler.
    struct MissionMutation {
        id: u64,
        new_mission: AircraftMission,
        ammo_delta: i32,
        fire_at: Option<u64>,
        move_to: Option<(u16, u16)>,
        self_destruct: bool,
        set_speed_fraction: Option<SimFixed>,
        set_target_altitude: Option<SimFixed>,
    }

    let mut mutations: Vec<MissionMutation> = Vec::new();

    for snap in &snapshots {
        let mut m = MissionMutation {
            id: snap.id,
            new_mission: snap.mission.clone(),
            ammo_delta: 0,
            fire_at: None,
            move_to: None,
            self_destruct: false,
            set_speed_fraction: None,
            set_target_altitude: None,
        };

        match &snap.mission {
            AircraftMission::Idle => {
                let entity = match sim.entities.get(snap.id) {
                    Some(e) => e,
                    None => continue,
                };
                let type_str = sim.interner.resolve(entity.type_ref);
                let obj = rules.object(type_str);
                let has_weapon = obj.map_or(false, |o| o.primary.is_some());
                let airport_bound = obj.map_or(false, |o| o.airport_bound);
                let is_airborne = entity
                    .locomotor
                    .as_ref()
                    .map_or(false, |l| l.altitude > SIM_ZERO);
                let ammo = entity.aircraft_ammo.as_ref();

                let nearest = find_nearest_airfield_for(
                    sim,
                    rules,
                    entity.owner,
                    entity.type_ref,
                    (entity.position.rx, entity.position.ry),
                );

                let input = idle_mode::IdleModeInput {
                    ammo_current: ammo.map_or(-1, |a| a.current),
                    ammo_max: ammo.map_or(-1, |a| a.max),
                    has_weapon,
                    has_target: entity.attack_target.is_some(),
                    airport_bound,
                    is_airborne,
                    nearest_airfield: nearest,
                };

                match idle_mode::enter_idle_mode(&input) {
                    idle_mode::IdleModeResult::Mission(new_m) => {
                        m.new_mission = new_m;
                    }
                    idle_mode::IdleModeResult::SelfDestruct => {
                        m.self_destruct = true;
                    }
                }
            }

            AircraftMission::Attack {
                sub_state,
                has_fired,
                is_strafe,
            } => {
                let result = attack_mission::tick_attack_state(
                    &sim.entities,
                    rules,
                    &sim.interner,
                    snap.id,
                    *sub_state,
                    *has_fired,
                    *is_strafe,
                );
                m.new_mission = result.new_mission;
                m.ammo_delta = result.ammo_delta;
                m.fire_at = result.fire_at;
                m.move_to = result.move_to;

                // Dive bombing: when in attack states 3-4, lower altitude to 1/3 cruise.
                if matches!(*sub_state, 3 | 4) {
                    if let Some(entity) = sim.entities.get(snap.id) {
                        if let Some(loco) = &entity.locomotor {
                            let cruise = loco.target_altitude;
                            let dive_alt = cruise / SimFixed::from_num(3);
                            m.set_target_altitude = Some(dive_alt);
                        }
                    }
                } else if *sub_state == 10 {
                    // Restore cruise altitude on RTB.
                    if let Some(entity) = sim.entities.get(snap.id) {
                        let type_str = sim.interner.resolve(entity.type_ref);
                        if let Some(obj) = rules.object(type_str) {
                            let cruise =
                                crate::sim::movement::locomotor::LocomotorState::from_object_type(
                                    obj,
                                    rules.general.flight_level,
                                )
                                .target_altitude;
                            m.set_target_altitude = Some(cruise);
                        }
                    }
                    m.set_speed_fraction = Some(SIM_ONE);
                }

                // Speed tiers based on distance to target.
                if matches!(*sub_state, 3 | 4) {
                    if let Some(entity) = sim.entities.get(snap.id) {
                        if let Some(tid) = entity.attack_target.as_ref().map(|at| at.target) {
                            if let Some(target) = sim.entities.get(tid) {
                                let dx =
                                    (entity.position.rx as i32 - target.position.rx as i32).abs();
                                let dy =
                                    (entity.position.ry as i32 - target.position.ry as i32).abs();
                                let dist_cells = dx.max(dy);

                                let speed_frac = if dist_cells < 1 {
                                    SIM_ZERO
                                } else if dist_cells < 2 {
                                    SimFixed::lit("0.5")
                                } else if dist_cells < 3 {
                                    SimFixed::lit("0.75")
                                } else {
                                    SIM_ONE
                                };
                                m.set_speed_fraction = Some(speed_frac);
                            }
                        }
                    }
                }
            }

            AircraftMission::Guard => {
                m.set_speed_fraction = Some(SIM_ONE);
                let entity = match sim.entities.get(snap.id) {
                    Some(e) => e,
                    None => continue,
                };
                let ammo = entity.aircraft_ammo.as_ref();
                let ammo_current = ammo.map_or(-1, |a| a.current);
                let ammo_max = ammo.map_or(-1, |a| a.max);

                if ammo_current <= 0 && ammo_max > 0 {
                    let nearest = find_nearest_airfield_for(
                        sim,
                        rules,
                        entity.owner,
                        entity.type_ref,
                        (entity.position.rx, entity.position.ry),
                    );
                    if let Some((af_id, af_rx, af_ry)) = nearest {
                        m.new_mission = AircraftMission::ReturnToBase { airfield_id: af_id };
                        m.move_to = Some((af_rx, af_ry));
                    } else {
                        let type_str = sim.interner.resolve(entity.type_ref);
                        let airport_bound =
                            rules.object(type_str).map_or(false, |o| o.airport_bound);
                        if airport_bound {
                            m.self_destruct = true;
                        }
                    }
                } else if entity.attack_target.is_some() && ammo_current > 0 {
                    m.new_mission = AircraftMission::Attack {
                        sub_state: 0,
                        has_fired: false,
                        is_strafe: false,
                    };
                }
            }

            AircraftMission::ReturnToBase { airfield_id } => {
                let entity = match sim.entities.get(snap.id) {
                    Some(e) => e,
                    None => continue,
                };
                let af_ok = sim
                    .entities
                    .get(*airfield_id)
                    .is_some_and(|af| af.health.current > 0 && !af.dying);
                if !af_ok {
                    m.new_mission = AircraftMission::Idle;
                    mutations.push(m);
                    continue;
                }
                let af = sim.entities.get(*airfield_id).unwrap();
                let type_str = sim.interner.resolve(af.type_ref);
                let (fw, fh) = rules
                    .object(type_str)
                    .map(|o| foundation_dimensions(&o.foundation))
                    .unwrap_or((1, 1));
                let dock_rx = af.position.rx + fw / 2;
                let dock_ry = af.position.ry + fh / 2;

                let dx = (entity.position.rx as i32 - dock_rx as i32).abs();
                let dy = (entity.position.ry as i32 - dock_ry as i32).abs();
                let dist = dx.max(dy);

                if dist <= 2 {
                    m.new_mission = AircraftMission::Docking {
                        airfield_id: *airfield_id,
                        sub_state: 0,
                        reload_timer: 0,
                    };
                } else if entity.movement_target.is_none() {
                    m.move_to = Some((dock_rx, dock_ry));
                }
            }

            AircraftMission::Docking {
                airfield_id,
                sub_state,
                reload_timer,
            } => {
                let entity = match sim.entities.get(snap.id) {
                    Some(e) => e,
                    None => continue,
                };
                let air_phase = entity.locomotor.as_ref().map(|l| l.air_phase);
                let ammo = entity.aircraft_ammo.as_ref();
                let ammo_current = ammo.map_or(0, |a| a.current);
                let ammo_max = ammo.map_or(0, |a| a.max);
                let reload_rate = rules.general.reload_rate_ticks;

                match sub_state {
                    0 => {
                        // Wait for dock slot.
                        let af_type_ref = sim
                            .entities
                            .get(*airfield_id)
                            .map_or(entity.type_ref, |af| af.type_ref);
                        let type_str = sim.interner.resolve(af_type_ref);
                        let max_slots = rules
                            .object(type_str)
                            .map(|o| o.number_of_docks.max(1))
                            .unwrap_or(1);
                        if sim.production.airfield_docks.try_reserve(
                            *airfield_id,
                            snap.id,
                            max_slots,
                        ) {
                            m.new_mission = AircraftMission::Docking {
                                airfield_id: *airfield_id,
                                sub_state: 1,
                                reload_timer: 0,
                            };
                        }
                    }
                    1 => {
                        // Descending — wait for Landed.
                        if air_phase == Some(AirMovePhase::Landed) {
                            m.new_mission = AircraftMission::Docking {
                                airfield_id: *airfield_id,
                                sub_state: 2,
                                reload_timer: reload_rate,
                            };
                        }
                    }
                    2 => {
                        // Reloading.
                        let timer = reload_timer.saturating_sub(1);
                        if timer == 0 {
                            m.ammo_delta = 1;
                            if ammo_current + 1 >= ammo_max {
                                // Fully reloaded → launch.
                                sim.production.airfield_docks.release(snap.id);
                                m.new_mission = AircraftMission::Docking {
                                    airfield_id: *airfield_id,
                                    sub_state: 3,
                                    reload_timer: 0,
                                };
                            } else {
                                m.new_mission = AircraftMission::Docking {
                                    airfield_id: *airfield_id,
                                    sub_state: 2,
                                    reload_timer: reload_rate,
                                };
                            }
                        } else {
                            m.new_mission = AircraftMission::Docking {
                                airfield_id: *airfield_id,
                                sub_state: 2,
                                reload_timer: timer,
                            };
                        }
                    }
                    3 => {
                        // Launching — wait for cruising altitude.
                        if air_phase == Some(AirMovePhase::Cruising) {
                            m.new_mission = AircraftMission::Idle;
                        }
                    }
                    _ => {
                        m.new_mission = AircraftMission::Idle;
                    }
                }
            }

            AircraftMission::Move { .. } => {
                let entity = match sim.entities.get(snap.id) {
                    Some(e) => e,
                    None => continue,
                };
                if entity.movement_target.is_none() {
                    m.new_mission = AircraftMission::Idle;
                }
            }

            AircraftMission::DockedIdle { airfield_id } => {
                // Check if airfield still alive.
                let af_ok = sim
                    .entities
                    .get(*airfield_id)
                    .is_some_and(|af| af.health.current > 0 && !af.dying);
                if !af_ok {
                    // Airfield destroyed — release dock and go to Idle.
                    // Idle mode will handle AirportBound self-destruct.
                    sim.production.airfield_docks.release(snap.id);
                    m.new_mission = AircraftMission::Idle;
                }
                // Otherwise: stay parked, do nothing.
            }
        }

        mutations.push(m);
    }

    // Phase 3: Apply mutations.
    for m in &mutations {
        if m.self_destruct {
            if let Some(entity) = sim.entities.get_mut(m.id) {
                entity.health.current = 0;
                entity.dying = true;
                entity.aircraft_mission = None;
            }
            continue;
        }

        if let Some(entity) = sim.entities.get_mut(m.id) {
            entity.aircraft_mission = Some(m.new_mission.clone());

            if m.ammo_delta != 0 {
                if let Some(ref mut ammo) = entity.aircraft_ammo {
                    ammo.current = (ammo.current + m.ammo_delta).max(0).min(ammo.max);
                }
            }

            if let Some(speed_frac) = m.set_speed_fraction {
                if let Some(ref mut loco) = entity.locomotor {
                    loco.speed_fraction = speed_frac;
                }
            }

            if let Some(target_alt) = m.set_target_altitude {
                if let Some(ref mut loco) = entity.locomotor {
                    loco.target_altitude = target_alt;
                    if loco.altitude > target_alt {
                        loco.air_phase = AirMovePhase::Descending;
                    } else if loco.altitude < target_alt {
                        loco.air_phase = AirMovePhase::Ascending;
                    }
                }
            }

            // Docking sub_state 1: set air phase to Descending.
            if let AircraftMission::Docking { sub_state: 1, .. } = &m.new_mission {
                if let Some(ref mut loco) = entity.locomotor {
                    loco.air_phase = AirMovePhase::Descending;
                }
                entity.movement_target = None;
            }
            // Docking sub_state 3: set air phase to Ascending (launch).
            if let AircraftMission::Docking { sub_state: 3, .. } = &m.new_mission {
                if let Some(ref mut loco) = entity.locomotor {
                    loco.air_phase = AirMovePhase::Ascending;
                }
            }
        }
    }

    // Phase 4: Issue air move commands and fire commands.
    let air_moves: Vec<(u64, u16, u16)> = mutations
        .iter()
        .filter_map(|m| m.move_to.map(|(rx, ry)| (m.id, rx, ry)))
        .collect();
    for (id, rx, ry) in air_moves {
        let speed = sim
            .entities
            .get(id)
            .and_then(|e| {
                let obj = rules.object(sim.interner.resolve(e.type_ref))?;
                Some(crate::util::fixed_math::ra2_speed_to_leptons_per_second(obj.speed.max(1)))
            })
            .unwrap_or(SimFixed::from_num(8));
        air_movement::issue_air_move_command(&mut sim.entities, id, (rx, ry), speed);
    }

    // Fire commands: set attack_target so combat system fires this tick.
    let fire_commands: Vec<(u64, u64)> = mutations
        .iter()
        .filter_map(|m| m.fire_at.map(|tid| (m.id, tid)))
        .collect();
    for (attacker_id, target_id) in fire_commands {
        if let Some(entity) = sim.entities.get_mut(attacker_id) {
            entity.attack_target = Some(AttackTarget::new(target_id));
        }
    }
}

/// Find nearest airfield for a given aircraft.
/// Returns (stable_id, dock_rx, dock_ry) if found.
fn find_nearest_airfield_for(
    sim: &Simulation,
    rules: &RuleSet,
    owner: crate::sim::intern::InternedId,
    type_ref: crate::sim::intern::InternedId,
    from: (u16, u16),
) -> Option<(u64, u16, u16)> {
    let aircraft_type_str = sim.interner.resolve(type_ref);
    let aircraft_obj = rules.object(aircraft_type_str)?;
    let dock_list = &aircraft_obj.dock;
    if dock_list.is_empty() {
        return None;
    }

    let mut best: Option<(u64, u16, u16, u32)> = None;
    for entity in sim.entities.values() {
        if entity.category != EntityCategory::Structure {
            continue;
        }
        if entity.health.current == 0 || entity.dying {
            continue;
        }
        if entity.owner != owner {
            continue;
        }
        let entity_type_str = sim.interner.resolve(entity.type_ref);
        let Some(obj) = rules.object(entity_type_str) else {
            continue;
        };
        if !obj.unit_reload && !obj.helipad {
            continue;
        }
        if !dock_list
            .iter()
            .any(|d| d.eq_ignore_ascii_case(entity_type_str))
        {
            continue;
        }
        let (w, h) = foundation_dimensions(&obj.foundation);
        let dock_rx = entity.position.rx + w / 2;
        let dock_ry = entity.position.ry + h / 2;
        let dx = (from.0 as i32 - dock_rx as i32).unsigned_abs();
        let dy = (from.1 as i32 - dock_ry as i32).unsigned_abs();
        let dist = dx * dx + dy * dy;

        if best.is_none() || dist < best.unwrap().3 {
            best = Some((entity.stable_id, dock_rx, dock_ry, dist));
        }
    }

    best.map(|(sid, rx, ry, _)| (sid, rx, ry))
}
