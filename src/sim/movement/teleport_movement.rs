//! Teleport (chrono) locomotor — instant relocation with chrono delay.
//!
//! Implements the Teleport state machine for chrono-style movement:
//! Relocate (instant, one tick) → ChronoDelay (being_warped countdown) → Idle.
//!
//! Self-teleport relocates the unit in a single tick (Phase 0), then the unit
//! sits at the destination 50% translucent for `chrono_delay` ticks until fully
//! materialized.
//!
//! Units with `Locomotor=Teleport` always use this. Units with `Teleporter=yes`
//! but a different base locomotor (e.g., Chrono Miner with Drive) get a temporary
//! override via the piggyback mechanism, restoring their base locomotor after arrival.
//!
//! No pathfinding — the unit is relocated instantly. Occupancy is cleared at the
//! old position and marked at the new position during the Relocate phase.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/game_entity, sim/entity_store, sim/locomotor.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::rules::locomotor_type::LocomotorKind;
use crate::rules::ruleset::GeneralRules;
use crate::sim::debug_event_log::DebugEventKind;
use crate::sim::entity_store::EntityStore;
use crate::sim::movement::locomotor::OverrideKind;
use crate::sim::occupancy::OccupancyGrid;
use crate::util::fixed_math::isqrt_i64;
use crate::util::lepton::CELL_CENTER_LEPTON;

/// Phase within the teleport state machine.
///
/// Phase 0 relocates instantly in one tick, then the chrono delay timer
/// counts down while the unit is semi-transparent at the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TeleportPhase {
    /// Instant relocation: position updated, occupancy swapped. Executes in
    /// one tick, then transitions to ChronoDelay.
    Relocate,
    /// Post-warp chrono delay: unit sits at destination 50% translucent,
    /// `being_warped_ticks` counts down each tick. When it reaches 0 the
    /// teleport is complete and the base locomotor is restored.
    ChronoDelay,
}

/// State for an in-progress teleport.
///
/// Set by `issue_teleport_command()` and cleared when the chrono delay
/// expires. The render system reads `being_warped_ticks` to apply 50%
/// translucency while the unit materializes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TeleportState {
    /// Current phase in the teleport sequence.
    pub phase: TeleportPhase,
    /// Destination cell coordinates.
    pub target_rx: u16,
    pub target_ry: u16,
    /// Chrono delay countdown in game ticks. While > 0 the unit is "being warped"
    /// and the renderer draws it at 50% alpha. Set from the distance-based formula
    /// in the original engine: `delay = distance_leptons / ChronoDistanceFactor`,
    /// clamped to `ChronoMinimumDelay`.
    pub being_warped_ticks: u32,
}

/// Compute the chrono warp delay in game ticks from distance.
///
/// When `ChronoTrigger=yes`, delay scales linearly with distance in leptons,
/// divided by `ChronoDistanceFactor` (default 48), clamped to at least
/// `ChronoMinimumDelay` (default 16). Short distances below `ChronoRangeMinimum`
/// are forced to the minimum.
pub fn compute_chrono_delay(rules: &GeneralRules, distance_leptons: i32) -> u32 {
    if !rules.chrono_trigger {
        return rules.chrono_minimum_delay.max(0) as u32;
    }
    let mut delay = if rules.chrono_distance_factor > 0 {
        distance_leptons / rules.chrono_distance_factor
    } else {
        0
    };
    if delay < rules.chrono_minimum_delay {
        delay = rules.chrono_minimum_delay;
    }
    if distance_leptons < rules.chrono_range_minimum {
        delay = rules.chrono_minimum_delay;
    }
    delay.max(0) as u32
}

/// Issue a teleport move command to an entity.
///
/// If the entity's base locomotor is not Teleport but it has `Teleporter=yes`,
/// a temporary override is applied via the piggyback mechanism.
///
/// The chrono delay is computed from the Euclidean distance in leptons
/// (see `compute_chrono_delay`). One cell = 256 leptons.
///
/// Returns `true` if the teleport was initiated, `false` if the entity
/// is missing required fields.
pub fn issue_teleport_command(
    entities: &mut EntityStore,
    entity_id: u64,
    target: (u16, u16),
    rules: &GeneralRules,
) -> bool {
    let Some(entity) = entities.get_mut(entity_id) else {
        log::warn!("issue_teleport_command: entity {} not found", entity_id);
        return false;
    };

    // Compute distance in leptons (1 cell = 256 leptons) for chrono delay.
    let dx = (entity.position.rx as i32 - target.0 as i32) * 256;
    let dy = (entity.position.ry as i32 - target.1 as i32) * 256;
    let dist_sq = (dx as i64) * (dx as i64) + (dy as i64) * (dy as i64);
    let distance_leptons = isqrt_i64(dist_sq) as i32;
    let chrono_ticks = compute_chrono_delay(rules, distance_leptons);

    // Apply piggyback override if the unit's base locomotor is not Teleport.
    if let Some(ref mut loco) = entity.locomotor {
        if loco.kind != LocomotorKind::Teleport {
            loco.begin_override(OverrideKind::Teleport);
        }
    }

    // Remove any existing ground movement.
    entity.movement_target = None;

    // Attach the teleport state machine — starts in Relocate (instant).
    entity.teleport_state = Some(TeleportState {
        phase: TeleportPhase::Relocate,
        target_rx: target.0,
        target_ry: target.1,
        being_warped_ticks: chrono_ticks,
    });
    entity.push_debug_event(
        0,
        DebugEventKind::SpecialMovementStart {
            kind: "Teleport".into(),
        },
    );

    true
}

/// Advance all in-progress teleport state machines.
///
/// Called once per simulation tick from `advance_tick()`.
/// Relocate executes instantly (one tick), then ChronoDelay counts down
/// `being_warped_ticks` each subsequent tick until the teleport completes.
pub fn tick_teleport_movement(
    entities: &mut EntityStore,
    occupancy: &mut OccupancyGrid,
    tick_ms: u32,
    sim_tick: u64,
) {
    if tick_ms == 0 {
        return;
    }

    // Collect entity IDs that need cleanup after ticking.
    let mut finished: Vec<u64> = Vec::new();

    let keys = entities.keys_sorted();
    for &id in &keys {
        let Some(entity) = entities.get_mut(id) else {
            continue;
        };
        let Some(ref mut teleport) = entity.teleport_state else {
            continue;
        };

        // Track phase before processing to detect transitions.
        let phase_before = teleport.phase;

        match teleport.phase {
            TeleportPhase::Relocate => {
                // Instant relocation in one tick — matches original Phase 0.
                let old_rx = entity.position.rx;
                let old_ry = entity.position.ry;
                entity.position.rx = teleport.target_rx;
                entity.position.ry = teleport.target_ry;
                entity.position.sub_x = CELL_CENTER_LEPTON;
                entity.position.sub_y = CELL_CENTER_LEPTON;
                entity.position.refresh_screen_coords();
                let layer = entity.locomotor.as_ref().map_or(
                    crate::sim::movement::locomotor::MovementLayer::Ground,
                    |l| l.layer,
                );
                occupancy.move_entity(
                    old_rx,
                    old_ry,
                    teleport.target_rx,
                    teleport.target_ry,
                    id,
                    layer,
                    entity.sub_cell,
                );
                teleport.phase = TeleportPhase::ChronoDelay;
            }
            TeleportPhase::ChronoDelay => {
                // Count down chrono delay ticks. Unit remains 50% translucent until 0.
                if teleport.being_warped_ticks > 0 {
                    teleport.being_warped_ticks -= 1;
                }
                if teleport.being_warped_ticks == 0 {
                    finished.push(id);
                }
            }
        }

        // Log phase transition if it changed.
        let phase_after = teleport.phase;
        if phase_after != phase_before {
            let phase_name = format!("{:?}", phase_after);
            // Drop the borrow on teleport before pushing debug event.
            let _ = teleport;
            entity.push_debug_event(
                sim_tick as u32,
                DebugEventKind::SpecialMovementPhase { phase: phase_name },
            );
        }
    }

    // Clean up finished teleports: remove TeleportState and restore base locomotor.
    for id in finished {
        if let Some(entity) = entities.get_mut(id) {
            entity.teleport_state = None;
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
    use crate::rules::locomotor_type::{LocomotorKind, MovementZone, SpeedType};
    use crate::rules::object_type::{ObjectCategory, ObjectType, PipScale};
    use crate::sim::entity_store::EntityStore;
    use crate::sim::game_entity::GameEntity;
    use crate::sim::movement::locomotor::{LocomotorState, MovementLayer};
    use crate::util::fixed_math::SimFixed;

    fn make_drive_obj() -> ObjectType {
        ObjectType {
            id: "CMIN".to_string(),
            category: ObjectCategory::Vehicle,
            name: None,
            cost: 0,
            strength: 100,
            armor: "none".to_string(),
            speed: 6,
            accel_factor: SimFixed::lit("0.03"),
            decel_factor: SimFixed::lit("0.02"),
            slowdown_distance: 512,
            sight: 5,
            tech_level: -1,
            build_time_multiplier: 1.0,
            build_time_multiplier_x1000: 1000,
            owner: vec![],
            required_houses: vec![],
            forbidden_houses: vec![],
            prerequisite: vec![],
            prerequisite_override: vec![],
            build_limit: 0,
            requires_stolen_allied_tech: false,
            requires_stolen_soviet_tech: false,
            requires_stolen_third_tech: false,
            primary: None,
            secondary: None,
            image: "CMIN".to_string(),
            power: 0,
            foundation: "1x1".to_string(),
            pixel_selection_bracket_delta: 0,
            build_cat: None,
            adjacent: 6,
            base_normal: true,
            crewed: false,
            voice_select: None,
            voice_move: None,
            voice_attack: None,
            die_sound: None,
            move_sound: None,
            has_turret: false,
            turret_rot: 0,
            turret_anim: None,
            turret_anim_is_voxel: false,
            turret_anim_x: 0,
            turret_anim_y: 0,
            turret_anim_z_adjust: 0,
            guard_range: None,
            explodes: false,
            death_weapon: None,
            super_weapon: None,
            super_weapon2: None,
            spy_sat: false,
            gap_generator: false,
            radar: false,
            radar_invisible: false,
            radar_visible: false,
            harvester: false,
            refinery: false,
            storage: 0,
            free_unit: None,
            dock: vec![],
            queueing_cell: None,
            docking_offset: None,
            unloading_class: None,
            ammo: -1,
            enslaves: None,
            slaves_number: 0,
            slave_regen_rate: 0,
            slave_reload_rate: 0,
            slaved: false,
            harvest_rate: 0,
            resource_gatherer: false,
            resource_destination: false,
            ore_purifier: false,
            locomotor: LocomotorKind::Drive,
            speed_type: SpeedType::Track,
            movement_zone: MovementZone::Normal,
            considered_aircraft: false,
            zfudge_bridge: 7,
            too_big_to_fit_under_bridge: false,
            crashable: false,
            teleporter: true,
            hover_attack: false,
            balloon_hover: false,
            airport_bound: false,
            fighter: false,
            fly_by: false,
            fly_back: false,
            landable: false,
            jumpjet: false,
            jumpjet_params: None,
            deploys_into: None,
            undeploys_into: None,
            factory: None,
            exit_coord: None,
            crushable: false,
            omni_crusher: false,
            omni_crush_resistant: false,
            engineer: false,
            deployer: false,
            capturable: false,
            repairable: false,
            can_be_occupied: false,
            can_occupy_fire: false,
            show_occupant_pips: false,
            passengers: 0,
            size_limit: 0,
            size: 3,
            open_topped: false,
            gunner: false,
            ifv_mode: 0,
            max_number_occupants: 0,
            occupier: false,
            assaulter: false,
            occupy_weapon: None,
            elite_occupy_weapon: None,
            occupy_pip: 7,
            pip_scale: PipScale::None,
            infantry_absorb: false,
            unit_absorb: false,
            weapon_list: vec![],
            attack_cursor_on_friendlies: false,
            sabotage_cursor: false,
            unit_repair: false,
            unit_reload: false,
            helipad: false,
            number_of_docks: 1,
            toggle_power: false,
            powered: false,
            can_disguise: false,
            wall: false,
            light_visibility: 0,
            light_intensity: 0.0,
            light_red_tint: 1.0,
            light_green_tint: 1.0,
            light_blue_tint: 1.0,
            water_bound: false,
            naval: false,
            number_impassable_rows: -1,
        }
    }

    fn default_rules() -> GeneralRules {
        GeneralRules::default()
    }

    #[test]
    fn test_teleport_issues_and_completes() {
        let mut entities = EntityStore::new();
        let mut e = GameEntity::test_default(1, "CLEG", "Americans", 5, 5);
        e.position.z = 0;
        entities.insert(e);
        let rules = default_rules();

        assert!(issue_teleport_command(&mut entities, 1, (20, 20), &rules));
        let entity = entities.get(1).expect("should exist");
        let ts = entity
            .teleport_state
            .as_ref()
            .expect("should have TeleportState");
        assert_eq!(ts.phase, TeleportPhase::Relocate);
        assert!(
            ts.being_warped_ticks >= 16,
            "should have at least minimum delay"
        );

        // One tick relocates instantly (matches original Phase 0).
        tick_teleport_movement(&mut entities, &mut OccupancyGrid::new(), 33, 0);

        let entity = entities.get(1).expect("should exist");
        assert_eq!(entity.position.rx, 20, "Should have relocated to target");
        assert_eq!(entity.position.ry, 20);
        let ts = entity.teleport_state.as_ref().expect("still warping");
        assert_eq!(
            ts.phase,
            TeleportPhase::ChronoDelay,
            "should be in chrono delay"
        );

        // Tick through ChronoDelay (being_warped_ticks countdown).
        let delay = ts.being_warped_ticks;
        for _ in 0..delay + 5 {
            tick_teleport_movement(&mut entities, &mut OccupancyGrid::new(), 33, 0);
        }

        // TeleportState should be removed after completion.
        let entity = entities.get(1).expect("should exist");
        assert!(
            entity.teleport_state.is_none(),
            "TeleportState should be removed after completion"
        );
    }

    #[test]
    fn test_teleport_with_piggyback_restores_drive() {
        let mut entities = EntityStore::new();
        let obj = make_drive_obj();
        let loco = LocomotorState::from_object_type(&obj, 1500);
        let mut e = GameEntity::test_default(1, "CMIN", "Americans", 5, 5);
        e.locomotor = Some(loco);
        entities.insert(e);
        let rules = default_rules();

        assert!(issue_teleport_command(&mut entities, 1, (20, 20), &rules));
        // Should have overridden to Teleport.
        let entity = entities.get(1).expect("should exist");
        let loco = entity.locomotor.as_ref().expect("has loco");
        assert_eq!(loco.kind, LocomotorKind::Teleport);
        assert!(loco.is_overridden());

        // Complete the whole sequence: 1 tick for Relocate + chrono delay ticks.
        for _ in 0..200 {
            tick_teleport_movement(&mut entities, &mut OccupancyGrid::new(), 33, 0);
        }

        // Should have restored to Drive.
        let entity = entities.get(1).expect("should exist");
        let loco = entity.locomotor.as_ref().expect("has loco");
        assert_eq!(loco.kind, LocomotorKind::Drive);
        assert!(!loco.is_overridden());
        assert_eq!(loco.layer, MovementLayer::Ground);
    }

    #[test]
    fn test_chrono_delay_formula() {
        let mut rules = default_rules();
        // Default: factor=48, minimum=16, trigger=true, range_minimum=0

        // Short distance: 256 leptons (1 cell) → 256/48 = 5, clamped to 16
        assert_eq!(compute_chrono_delay(&rules, 256), 16);

        // Medium distance: 5120 leptons (20 cells) → 5120/48 = 106
        assert_eq!(compute_chrono_delay(&rules, 5120), 106);

        // Very short distance below range minimum
        rules.chrono_range_minimum = 512;
        assert_eq!(compute_chrono_delay(&rules, 200), 16); // forced to minimum

        // ChronoTrigger=false → always minimum
        rules.chrono_trigger = false;
        assert_eq!(compute_chrono_delay(&rules, 5120), 16);
    }
}
