//! 11-state attack mission state machine for aircraft.
//!
//! Implements the core attack cycle: approach target → check range →
//! fire weapon → return to base. Matches gamemd.exe Mission_Attack.
//!
//! ## State overview
//! - 0: Init — clear flags, validate target
//! - 3: InRangeCheck — check weapon range, close in if needed
//! - 4: FireWeapon — fire, set HasFired, handle result
//! - 10: ReturnToBase — decrement ammo if HasFired, find helipad
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/components, sim/combat, rules/.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::rules::ruleset::RuleSet;
use crate::sim::aircraft::AircraftMission;
use crate::sim::entity_store::EntityStore;
use crate::sim::intern::StringInterner;
use crate::util::fixed_math::SimFixed;

/// ±11.25° firing arc in 16-bit facing units.
/// 0x800 = 2048 out of 65536 = 11.25°.
/// Aircraft can only fire when target bearing is within this arc of their heading.
const FIRING_ARC_TOLERANCE: u16 = 0x800;

/// Distance threshold for "in weapon range" checks, in cells (SimFixed).
/// Used as fallback when weapon range lookup fails.
const DEFAULT_WEAPON_RANGE_CELLS: SimFixed = SimFixed::lit("5");

/// Advance the attack mission state machine for one aircraft entity.
///
/// Returns the new mission state (may be the same, or transition to Guard/RTB).
/// The caller is responsible for writing the returned mission back to the entity.
pub fn tick_attack_state(
    entities: &EntityStore,
    rules: &RuleSet,
    interner: &StringInterner,
    entity_id: u64,
    sub_state: u8,
    has_fired: bool,
    _is_strafe: bool,
) -> AttackTickResult {
    let Some(entity) = entities.get(entity_id) else {
        return AttackTickResult::transition(AircraftMission::Idle);
    };

    // Read target stable_id from attack_target.
    let target_id = entity.attack_target.as_ref().map(|at| at.target);
    let ammo_current = entity.aircraft_ammo.as_ref().map_or(-1, |a| a.current);
    let entity_rx = entity.position.rx;
    let entity_ry = entity.position.ry;
    let entity_facing = entity.facing;
    let type_ref = entity.type_ref;

    // Look up type info.
    let type_str = interner.resolve(type_ref);
    let obj = rules.object(type_str);
    let fly_by = obj.map_or(false, |o| o.fly_by);

    // Get weapon range in cells.
    let weapon_range_cells: SimFixed = obj
        .and_then(|o| {
            let wpn_name = o.primary.as_ref()?;
            let wpn = rules.weapon(wpn_name)?;
            Some(wpn.range)
        })
        .unwrap_or(DEFAULT_WEAPON_RANGE_CELLS);

    match sub_state {
        // ---------------------------------------------------------------
        // State 0: INIT
        // Clear HasFired, IsStrafe. Validate target exists.
        // → State 3 (has target) or State 10 (no target, RTB)
        // ---------------------------------------------------------------
        0 => {
            if target_id.is_none() || target_id.and_then(|tid| entities.get(tid)).is_none() {
                return AttackTickResult::transition(AircraftMission::Attack {
                    sub_state: 10,
                    has_fired: false,
                    is_strafe: false,
                });
            }
            AttackTickResult::stay(AircraftMission::Attack {
                sub_state: 3,
                has_fired: false,
                is_strafe: false,
            })
        }

        // ---------------------------------------------------------------
        // State 3: IN_RANGE_CHECK
        // Check if target is in weapon range. If not, continue approach.
        // If in range: → State 4 (fire).
        // ---------------------------------------------------------------
        3 => {
            let Some(tid) = target_id else {
                return AttackTickResult::transition(AircraftMission::Attack {
                    sub_state: 10,
                    has_fired,
                    is_strafe: false,
                });
            };
            let Some(target) = entities.get(tid) else {
                return AttackTickResult::transition(AircraftMission::Attack {
                    sub_state: 10,
                    has_fired,
                    is_strafe: false,
                });
            };
            if target.dying || target.health.current == 0 {
                return AttackTickResult::transition(AircraftMission::Attack {
                    sub_state: 10,
                    has_fired,
                    is_strafe: false,
                });
            }

            // Distance check (cell-based Chebyshev).
            let dx = (entity_rx as i32 - target.position.rx as i32).abs();
            let dy = (entity_ry as i32 - target.position.ry as i32).abs();
            let dist = SimFixed::from_num(dx.max(dy));

            if dist <= weapon_range_cells {
                // In range → fire.
                AttackTickResult::stay(AircraftMission::Attack {
                    sub_state: 4,
                    has_fired,
                    is_strafe: false,
                })
            } else {
                // Out of range — set movement toward target.
                AttackTickResult::approach(
                    AircraftMission::Attack {
                        sub_state: 3,
                        has_fired,
                        is_strafe: false,
                    },
                    (target.position.rx, target.position.ry),
                )
            }
        }

        // ---------------------------------------------------------------
        // State 4: FIRE_WEAPON
        // Check firing arc (±11.25°). If aligned: fire, set HasFired.
        // → State 10 (RTB) or State 5 (strafe) based on FlyBy.
        // ---------------------------------------------------------------
        4 => {
            let Some(tid) = target_id else {
                return AttackTickResult::transition(AircraftMission::Attack {
                    sub_state: 10,
                    has_fired,
                    is_strafe: false,
                });
            };
            let Some(target) = entities.get(tid) else {
                return AttackTickResult::transition(AircraftMission::Attack {
                    sub_state: 10,
                    has_fired,
                    is_strafe: false,
                });
            };

            // Firing arc check: ±11.25° (0x800 in 16-bit facing).
            let target_dx = target.position.rx as i32 - entity_rx as i32;
            let target_dy = target.position.ry as i32 - entity_ry as i32;
            let target_facing_u8 = crate::sim::movement::facing_from_delta(target_dx, target_dy);
            // Convert both to 16-bit for arc comparison.
            let entity_facing_16: u16 = (entity_facing as u16) << 8;
            let target_facing_16: u16 = (target_facing_u8 as u16) << 8;
            let facing_diff = (entity_facing_16 as i16)
                .wrapping_sub(target_facing_16 as i16)
                .unsigned_abs();

            if facing_diff > FIRING_ARC_TOLERANCE {
                // Not aligned — continue approach (don't fire).
                return AttackTickResult::approach(
                    AircraftMission::Attack {
                        sub_state: 4,
                        has_fired,
                        is_strafe: false,
                    },
                    (target.position.rx, target.position.ry),
                );
            }

            // Firing arc aligned — signal fire permission.
            let next_state: u8 = if fly_by && ammo_current > 1 {
                6 // → strafe states
            } else {
                10 // → RTB
            };

            AttackTickResult::fire(
                AircraftMission::Attack {
                    sub_state: next_state,
                    has_fired: true,
                    is_strafe: fly_by,
                },
                tid,
            )
        }

        // ---------------------------------------------------------------
        // State 5: STRAFE_FIRE — secondary fire pass, continues strafing.
        // ---------------------------------------------------------------
        5 => {
            let Some(tid) = target_id else {
                return AttackTickResult::transition(AircraftMission::Attack {
                    sub_state: 10,
                    has_fired,
                    is_strafe: false,
                });
            };
            AttackTickResult::fire(
                AircraftMission::Attack {
                    sub_state: 6,
                    has_fired: true,
                    is_strafe: true,
                },
                tid,
            )
        }

        // ---------------------------------------------------------------
        // States 6, 7, 8: STRAFE_PASS_N — multi-pass strafing.
        // Each state fires one burst, then advances to next state.
        // If ammo runs out: → State 10 (RTB).
        // ---------------------------------------------------------------
        6 | 7 | 8 => {
            if ammo_current <= 0 {
                return AttackTickResult::stay(AircraftMission::Attack {
                    sub_state: 10,
                    has_fired,
                    is_strafe: false,
                });
            }
            let Some(tid) = target_id else {
                return AttackTickResult::transition(AircraftMission::Attack {
                    sub_state: 10,
                    has_fired,
                    is_strafe: false,
                });
            };
            let Some(target) = entities.get(tid) else {
                return AttackTickResult::transition(AircraftMission::Attack {
                    sub_state: 10,
                    has_fired,
                    is_strafe: false,
                });
            };
            if target.dying || target.health.current == 0 {
                return AttackTickResult::transition(AircraftMission::Attack {
                    sub_state: 10,
                    has_fired,
                    is_strafe: false,
                });
            }

            AttackTickResult::fire(
                AircraftMission::Attack {
                    sub_state: sub_state + 1,
                    has_fired: true,
                    is_strafe: true,
                },
                tid,
            )
        }

        // ---------------------------------------------------------------
        // State 9: STRAFE_FINAL — last strafe pass, re-evaluate target.
        // ---------------------------------------------------------------
        9 => {
            let Some(tid) = target_id else {
                return AttackTickResult::transition(AircraftMission::Attack {
                    sub_state: 10,
                    has_fired,
                    is_strafe: false,
                });
            };
            AttackTickResult::fire(
                AircraftMission::Attack {
                    sub_state: 3,
                    has_fired: true,
                    is_strafe: false,
                },
                tid,
            )
        }

        // ---------------------------------------------------------------
        // State 10: RETURN_TO_BASE
        // Decrement ammo if HasFired. Clear flags.
        // If ammo > 0 and target still valid: re-engage (→ State 0).
        // Else: transition to Guard (which handles RTB to airfield).
        // ---------------------------------------------------------------
        10 => {
            let mut result_ammo_delta: i32 = 0;
            if has_fired {
                result_ammo_delta = -1;
            }

            // Re-engage check: still have ammo and target alive?
            let can_reengage = ammo_current + result_ammo_delta > 0
                && target_id
                    .and_then(|tid| entities.get(tid))
                    .is_some_and(|t| !t.dying && t.health.current > 0);

            if can_reengage {
                AttackTickResult {
                    new_mission: AircraftMission::Attack {
                        sub_state: 0,
                        has_fired: false,
                        is_strafe: false,
                    },
                    ammo_delta: result_ammo_delta,
                    fire_at: None,
                    move_to: None,
                }
            } else {
                AttackTickResult {
                    new_mission: AircraftMission::Guard,
                    ammo_delta: result_ammo_delta,
                    fire_at: None,
                    move_to: None,
                }
            }
        }

        // ---------------------------------------------------------------
        // State 1, 2: Legacy/spawner states — not yet needed.
        // ---------------------------------------------------------------
        _ => AttackTickResult::transition(AircraftMission::Guard),
    }
}

/// Result of one tick of the attack state machine.
pub struct AttackTickResult {
    /// New mission state to write back.
    pub new_mission: AircraftMission,
    /// Ammo change to apply (-1 for decrement on HasFired, 0 otherwise).
    pub ammo_delta: i32,
    /// If Some, the combat system should fire at this target this tick.
    pub fire_at: Option<u64>,
    /// If Some, issue an air move command toward this cell.
    pub move_to: Option<(u16, u16)>,
}

impl AttackTickResult {
    fn stay(mission: AircraftMission) -> Self {
        Self {
            new_mission: mission,
            ammo_delta: 0,
            fire_at: None,
            move_to: None,
        }
    }

    fn transition(mission: AircraftMission) -> Self {
        Self {
            new_mission: mission,
            ammo_delta: 0,
            fire_at: None,
            move_to: None,
        }
    }

    fn approach(mission: AircraftMission, target_cell: (u16, u16)) -> Self {
        Self {
            new_mission: mission,
            ammo_delta: 0,
            fire_at: None,
            move_to: Some(target_cell),
        }
    }

    fn fire(mission: AircraftMission, target_id: u64) -> Self {
        Self {
            new_mission: mission,
            ammo_delta: 0,
            fire_at: Some(target_id),
            move_to: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::ini_parser::IniFile;
    use crate::sim::combat::AttackTarget;
    use crate::sim::docking::aircraft_dock::AircraftAmmo;
    use crate::sim::entity_store::EntityStore;
    use crate::sim::game_entity::GameEntity;
    use crate::sim::intern::test_interner;

    fn test_rules() -> RuleSet {
        let ini_str = "\
[AircraftTypes]\n0=ORCA\n\n\
[VehicleTypes]\n0=RHINO\n\n\
[InfantryTypes]\n\n\
[BuildingTypes]\n\n\
[ORCA]\nStrength=150\nArmor=light\nSpeed=14\nPrimary=Hellfire\nAmmo=2\nFlyBy=yes\n\n\
[RHINO]\nStrength=400\nArmor=heavy\nSpeed=6\n\n\
[Hellfire]\nDamage=100\nROF=20\nRange=5\nWarhead=HE\n\n\
[HE]\nVerses=100%,100%,100%,100%,100%,100%,100%,100%,100%,0%,0%\n";
        let ini = IniFile::from_str(ini_str);
        RuleSet::from_ini(&ini).expect("test rules")
    }

    #[test]
    fn test_state0_no_target_goes_to_state10() {
        let mut store = EntityStore::new();
        let attacker = GameEntity::test_default(1, "ORCA", "Americans", 10, 10);
        store.insert(attacker);
        let interner = test_interner();
        let rules = test_rules();

        let result = tick_attack_state(&store, &rules, &interner, 1, 0, false, false);
        match result.new_mission {
            AircraftMission::Attack { sub_state: 10, .. } => {}
            other => panic!("Expected state 10, got {:?}", other),
        }
    }

    #[test]
    fn test_state0_with_target_goes_to_state3() {
        let mut store = EntityStore::new();
        let mut attacker = GameEntity::test_default(1, "ORCA", "Americans", 10, 10);
        attacker.attack_target = Some(AttackTarget::new(2));
        attacker.aircraft_ammo = Some(AircraftAmmo::new(2));
        store.insert(attacker);
        let target = GameEntity::test_default(2, "RHINO", "Soviet", 15, 15);
        store.insert(target);
        let interner = test_interner();
        let rules = test_rules();

        let result = tick_attack_state(&store, &rules, &interner, 1, 0, false, false);
        match result.new_mission {
            AircraftMission::Attack {
                sub_state: 3,
                has_fired: false,
                ..
            } => {}
            other => panic!("Expected state 3, got {:?}", other),
        }
    }

    #[test]
    fn test_state10_no_ammo_goes_to_guard() {
        let mut store = EntityStore::new();
        let mut attacker = GameEntity::test_default(1, "ORCA", "Americans", 10, 10);
        attacker.attack_target = Some(AttackTarget::new(2));
        attacker.aircraft_ammo = Some(AircraftAmmo::new(2));
        store.insert(attacker);
        let target = GameEntity::test_default(2, "RHINO", "Soviet", 15, 15);
        store.insert(target);
        // Deplete ammo.
        store
            .get_mut(1)
            .unwrap()
            .aircraft_ammo
            .as_mut()
            .unwrap()
            .current = 0;
        let interner = test_interner();
        let rules = test_rules();

        let result = tick_attack_state(&store, &rules, &interner, 1, 10, true, false);
        // has_fired=true → ammo_delta=-1, ammo was 0 so 0-1=-1 → no re-engage → Guard.
        assert!(matches!(result.new_mission, AircraftMission::Guard));
        assert_eq!(result.ammo_delta, -1);
    }

    #[test]
    fn test_state10_has_ammo_reengages() {
        let mut store = EntityStore::new();
        let mut attacker = GameEntity::test_default(1, "ORCA", "Americans", 10, 10);
        attacker.attack_target = Some(AttackTarget::new(2));
        attacker.aircraft_ammo = Some(AircraftAmmo::new(2));
        store.insert(attacker);
        let target = GameEntity::test_default(2, "RHINO", "Soviet", 15, 15);
        store.insert(target);
        let interner = test_interner();
        let rules = test_rules();

        let result = tick_attack_state(&store, &rules, &interner, 1, 10, true, false);
        // has_fired=true → ammo_delta=-1. ammo was 2, now 1 > 0 → re-engage.
        match result.new_mission {
            AircraftMission::Attack {
                sub_state: 0,
                has_fired: false,
                ..
            } => {}
            other => panic!("Expected re-engage (state 0), got {:?}", other),
        }
        assert_eq!(result.ammo_delta, -1);
    }

    #[test]
    fn test_state3_in_range_goes_to_state4() {
        let mut store = EntityStore::new();
        let mut attacker = GameEntity::test_default(1, "ORCA", "Americans", 10, 10);
        attacker.attack_target = Some(AttackTarget::new(2));
        attacker.aircraft_ammo = Some(AircraftAmmo::new(2));
        store.insert(attacker);
        // Target within 5-cell weapon range.
        let target = GameEntity::test_default(2, "RHINO", "Soviet", 13, 10);
        store.insert(target);
        let interner = test_interner();
        let rules = test_rules();

        let result = tick_attack_state(&store, &rules, &interner, 1, 3, false, false);
        match result.new_mission {
            AircraftMission::Attack { sub_state: 4, .. } => {}
            other => panic!("Expected state 4, got {:?}", other),
        }
    }

    #[test]
    fn test_state3_out_of_range_approaches() {
        let mut store = EntityStore::new();
        let mut attacker = GameEntity::test_default(1, "ORCA", "Americans", 10, 10);
        attacker.attack_target = Some(AttackTarget::new(2));
        attacker.aircraft_ammo = Some(AircraftAmmo::new(2));
        store.insert(attacker);
        // Target far away (20 cells).
        let target = GameEntity::test_default(2, "RHINO", "Soviet", 30, 10);
        store.insert(target);
        let interner = test_interner();
        let rules = test_rules();

        let result = tick_attack_state(&store, &rules, &interner, 1, 3, false, false);
        // Should stay in state 3 and issue a move command.
        match result.new_mission {
            AircraftMission::Attack { sub_state: 3, .. } => {}
            other => panic!("Expected state 3 (approach), got {:?}", other),
        }
        assert!(result.move_to.is_some());
    }
}
