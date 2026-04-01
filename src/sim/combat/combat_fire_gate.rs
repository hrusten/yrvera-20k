//! Fire-gating — prevents units from firing during special locomotor states
//! or when a building is disabled by low power.
//!
//! RA2's `ILocomotion::Can_Fire()` prevents weapons from firing during certain
//! movement phases (teleport warp, tunnel dig, droppod fall, rocket flight).
//! Additionally, `Powered=yes` defense buildings cannot fire when the owner
//! is in low-power state.
//!
//! This module provides a pre-scan that collects fire-blocked entities before
//! the combat snapshot loop, avoiding borrow conflicts with `query_mut`.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/ movement state components, power_system.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use std::collections::{BTreeMap, BTreeSet};

use crate::map::entities::EntityCategory;
use crate::rules::ruleset::RuleSet;
use crate::sim::entity_store::EntityStore;
use crate::sim::intern::{InternedId, StringInterner};
use crate::sim::movement::droppod_movement::DropPodPhase;
use crate::sim::movement::teleport_movement::TeleportPhase;
use crate::sim::movement::tunnel_movement::TunnelPhase;
use crate::sim::power_system::{self, PowerState};

/// Collect stable IDs of entities that are currently blocked from firing weapons.
///
/// Checks locomotor state (teleport/tunnel/droppod/rocket) and power state
/// (Powered=yes buildings disabled during low power).
pub fn collect_fire_blocked_entities(
    entities: &EntityStore,
    power_states: &BTreeMap<InternedId, PowerState>,
    rules: Option<&RuleSet>,
    interner: &StringInterner,
) -> BTreeSet<u64> {
    let mut blocked: BTreeSet<u64> = BTreeSet::new();

    for entity in entities.values() {
        // Teleporting units cannot fire during the instant relocation tick.
        // ChronoDelay allows fire — the unit has arrived and is materializing.
        if let Some(ref state) = entity.teleport_state {
            match state.phase {
                TeleportPhase::Relocate => {
                    blocked.insert(entity.stable_id);
                    continue;
                }
                TeleportPhase::ChronoDelay => {}
            }
        }

        // Tunneling units cannot fire during dig/underground phases.
        if let Some(ref state) = entity.tunnel_state {
            match state.phase {
                TunnelPhase::DigIn | TunnelPhase::DigOut | TunnelPhase::UndergroundTravel => {
                    blocked.insert(entity.stable_id);
                    continue;
                }
                TunnelPhase::SurfaceMove => {} // Normal surface movement — can fire.
            }
        }

        // Falling drop pod units cannot fire until landed.
        if let Some(ref state) = entity.droppod_state {
            if state.phase == DropPodPhase::Falling {
                blocked.insert(entity.stable_id);
                continue;
            }
        }

        // Rockets are projectiles, not weapon-bearing units — never fire.
        if entity.rocket_state.is_some() {
            blocked.insert(entity.stable_id);
            continue;
        }

        // Aircraft with 0 ammo cannot fire — must reload at an airfield first.
        if let Some(ref ammo) = entity.aircraft_ammo {
            if ammo.current <= 0 {
                blocked.insert(entity.stable_id);
                continue;
            }
        }

        // Aircraft with an active Attack mission fire through the mission system,
        // not through generic combat. Block them here to prevent double-firing.
        // Docked-idle aircraft are parked on helipad — don't fire.
        if let Some(ref mission) = entity.aircraft_mission {
            if mission.is_attacking() || mission.is_docked_idle() {
                blocked.insert(entity.stable_id);
                continue;
            }
        }

        // Buildings still deploying cannot fire.
        if entity.building_up.is_some() {
            blocked.insert(entity.stable_id);
            continue;
        }

        // Powered=yes defense buildings cannot fire when the owner is in low power.
        if entity.category == EntityCategory::Structure {
            if let Some(rules) = rules {
                if !power_system::is_building_powered(power_states, rules, entity, interner) {
                    blocked.insert(entity.stable_id);
                    continue;
                }
            }
        }

        // Garrison fire gate: CanBeOccupied buildings with no occupants cannot fire.
        // gamemd FUN_007091D0: even if the building has its own weapons, an empty
        // garrisonable building is defenseless.
        if entity.category == EntityCategory::Structure {
            if let Some(rules) = rules {
                if let Some(obj) = rules.object(interner.resolve(entity.type_ref)) {
                    if obj.can_be_occupied
                        && entity.passenger_role.cargo().map_or(true, |c| c.is_empty())
                    {
                        blocked.insert(entity.stable_id);
                    }
                }
            }
        }
    }

    blocked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::game_entity::GameEntity;
    use crate::sim::intern::StringInterner;
    use crate::sim::movement::droppod_movement::{DropPodPhase, DropPodState};
    use crate::sim::movement::teleport_movement::{TeleportPhase, TeleportState};
    use crate::sim::movement::tunnel_movement::{TunnelPhase, TunnelState};
    use crate::util::fixed_math::{SIM_ZERO, SimFixed};

    fn make_entity(id: u64) -> GameEntity {
        GameEntity::test_default(id, "MTNK", "Americans", 5, 5)
    }

    /// Empty power states and no rules — locomotor-only gating.
    fn no_power() -> (BTreeMap<InternedId, PowerState>, Option<&'static RuleSet>) {
        (BTreeMap::new(), None)
    }

    fn test_interner() -> StringInterner {
        let mut interner = StringInterner::new();
        interner.intern("MTNK");
        interner.intern("Americans");
        interner
    }

    #[test]
    fn test_teleport_relocate_blocks_fire() {
        let mut store = EntityStore::new();
        let mut e = make_entity(1);
        e.teleport_state = Some(TeleportState {
            phase: TeleportPhase::Relocate,
            target_rx: 20,
            target_ry: 20,
            being_warped_ticks: 16,
        });
        store.insert(e);
        let (ps, r) = no_power();
        let interner = test_interner();
        let blocked = collect_fire_blocked_entities(&store, &ps, r, &interner);
        assert!(blocked.contains(&1), "Relocate should block firing");
    }

    #[test]
    fn test_teleport_chrono_delay_allows_fire() {
        let mut store = EntityStore::new();
        let mut e = make_entity(1);
        e.teleport_state = Some(TeleportState {
            phase: TeleportPhase::ChronoDelay,
            target_rx: 20,
            target_ry: 20,
            being_warped_ticks: 10,
        });
        store.insert(e);
        let (ps, r) = no_power();
        let interner = test_interner();
        let blocked = collect_fire_blocked_entities(&store, &ps, r, &interner);
        assert!(!blocked.contains(&1), "ChronoDelay should allow firing");
    }

    #[test]
    fn test_tunnel_dig_in_blocks_fire() {
        let mut store = EntityStore::new();
        let mut e = make_entity(1);
        e.tunnel_state = Some(TunnelState {
            phase: TunnelPhase::DigIn,
            target_rx: 20,
            target_ry: 5,
            timer: SimFixed::lit("0.5"),
            tunnel_speed: SimFixed::from_num(6),
            progress: SIM_ZERO,
        });
        store.insert(e);
        let (ps, r) = no_power();
        let interner = test_interner();
        let blocked = collect_fire_blocked_entities(&store, &ps, r, &interner);
        assert!(blocked.contains(&1), "DigIn should block firing");
    }

    #[test]
    fn test_tunnel_underground_blocks_fire() {
        let mut store = EntityStore::new();
        let mut e = make_entity(1);
        e.tunnel_state = Some(TunnelState {
            phase: TunnelPhase::UndergroundTravel,
            target_rx: 20,
            target_ry: 5,
            timer: SIM_ZERO,
            tunnel_speed: SimFixed::from_num(6),
            progress: SimFixed::from_num(3),
        });
        store.insert(e);
        let (ps, r) = no_power();
        let interner = test_interner();
        let blocked = collect_fire_blocked_entities(&store, &ps, r, &interner);
        assert!(
            blocked.contains(&1),
            "Underground travel should block firing"
        );
    }

    #[test]
    fn test_droppod_falling_blocks_fire() {
        let mut store = EntityStore::new();
        let mut e = make_entity(1);
        e.droppod_state = Some(DropPodState {
            phase: DropPodPhase::Falling,
            altitude: SimFixed::from_num(800),
            timer: SIM_ZERO,
        });
        store.insert(e);
        let (ps, r) = no_power();
        let interner = test_interner();
        let blocked = collect_fire_blocked_entities(&store, &ps, r, &interner);
        assert!(blocked.contains(&1), "Falling should block firing");
    }

    #[test]
    fn test_droppod_landing_allows_fire() {
        let mut store = EntityStore::new();
        let mut e = make_entity(1);
        e.droppod_state = Some(DropPodState {
            phase: DropPodPhase::Landing,
            altitude: SIM_ZERO,
            timer: SimFixed::lit("0.2"),
        });
        store.insert(e);
        let (ps, r) = no_power();
        let interner = test_interner();
        let blocked = collect_fire_blocked_entities(&store, &ps, r, &interner);
        assert!(!blocked.contains(&1), "Landing phase should allow firing");
    }

    #[test]
    fn test_entity_without_special_state_can_fire() {
        let mut store = EntityStore::new();
        store.insert(make_entity(1));
        let (ps, r) = no_power();
        let interner = test_interner();
        let blocked = collect_fire_blocked_entities(&store, &ps, r, &interner);
        assert!(
            !blocked.contains(&1),
            "Normal entity should be able to fire"
        );
    }
}
