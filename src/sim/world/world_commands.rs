//! Command dispatch for the Simulation.
//!
//! Contains `apply_command()` and its helper methods: selection snapshots,
//! ownership checks, and friendship queries. Split from world.rs for size.
//!
//! Dependency rules: same as sim/ (depends on rules/, map/; never render/ui/audio/net).

use std::collections::BTreeMap;

use super::Simulation;
use crate::map::houses::are_houses_friendly;
use crate::rules::locomotor_type::{LocomotorKind, MovementZone, SpeedType};
use crate::rules::object_type::ObjectCategory;
use crate::rules::ruleset::RuleSet;
use crate::sim::combat;
use crate::sim::command::Command;
use crate::sim::components::OrderIntent;
use crate::sim::docking::building_dock::{self, DockPhase, DockState};
use crate::sim::movement;
use crate::sim::movement::air_movement;
use crate::sim::movement::bump_crush;
use crate::sim::movement::jumpjet_movement;
use crate::sim::movement::locomotor::MovementLayer;
use crate::sim::movement::teleport_movement;
use crate::sim::movement::tunnel_movement;
use crate::sim::passenger;
use crate::sim::pathfinding::PathGrid;
use crate::sim::production;
use crate::util::fixed_math::{SIM_ZERO, SimFixed, ra2_speed_to_leptons_per_second};

/// Read-only snapshot of entity + rules data needed for issuing movement commands.
/// Captured once to avoid repeated entity lookups and type_ref clones.
struct MoveInfo {
    speed: SimFixed,
    loco_kind: Option<LocomotorKind>,
    loco_layer: MovementLayer,
    speed_type: SpeedType,
    hover_attack: bool,
    is_teleporter: bool,
    is_harvester: bool,
    is_infantry: bool,
    accel_factor: SimFixed,
    decel_factor: SimFixed,
    slowdown_distance: SimFixed,
    movement_zone: MovementZone,
    position: (u16, u16),
}

impl Simulation {
    /// Snapshot entity + rules data needed for movement dispatch in one lookup.
    fn resolve_move_info(&self, entity_id: u64, rules: Option<&RuleSet>) -> Option<MoveInfo> {
        let e = self.entities.get(entity_id)?;
        let loco = e.locomotor.as_ref();
        let loco_kind = loco.map(|l| l.kind);
        let loco_layer = e.movement_layer_or_ground();
        let speed_type = loco.map(|l| l.speed_type).unwrap_or(SpeedType::Track);
        let hover_attack = loco.map(|l| l.hover_attack).unwrap_or(false);
        let loco_multiplier = loco
            .map(|l| l.speed_multiplier)
            .unwrap_or(SimFixed::from_num(1));

        let obj = rules.and_then(|r| r.object(self.interner.resolve(e.type_ref)));
        // DEBUG: 3x speed boost for MCVs during development.
        let speed_mult = obj.map_or(1, |o| if o.deploys_into.is_some() { 3 } else { 1 });
        let base_speed = obj
            .map(|o| ra2_speed_to_leptons_per_second(o.speed * speed_mult))
            .unwrap_or(ra2_speed_to_leptons_per_second(4));
        let speed = (base_speed * loco_multiplier).max(SimFixed::lit("25"));

        Some(MoveInfo {
            speed,
            loco_kind,
            loco_layer,
            speed_type,
            hover_attack,
            is_teleporter: obj.map_or(false, |o| o.teleporter),
            is_harvester: obj.map_or(false, |o| o.harvester),
            is_infantry: obj.map_or(false, |o| o.category == ObjectCategory::Infantry),
            accel_factor: obj.map_or(SIM_ZERO, |o| o.accel_factor),
            decel_factor: obj.map_or(SIM_ZERO, |o| o.decel_factor),
            slowdown_distance: obj.map_or(SIM_ZERO, |o| SimFixed::from_num(o.slowdown_distance)),
            movement_zone: obj.map_or(MovementZone::Normal, |o| o.movement_zone),
            position: (e.position.rx, e.position.ry),
        })
    }

    /// Dispatch a single command, returning true if it was successfully applied.
    pub(crate) fn apply_command(
        &mut self,
        command_owner: &str,
        cmd: &Command,
        rules: Option<&RuleSet>,
        path_grid: Option<&PathGrid>,
        height_map: &BTreeMap<(u16, u16), u8>,
    ) -> bool {
        match cmd {
            Command::Select { entity_ids, .. } => {
                let mut snapshot = entity_ids.clone();
                snapshot.sort_unstable();
                snapshot.dedup();
                self.apply_selection_snapshot(&snapshot)
            }
            Command::Move {
                entity_id,
                target_rx,
                target_ry,
                queue,
                group_id,
            } => {
                if !self.entity_owned_by_id(command_owner, *entity_id) {
                    return false;
                }
                // Cancel any dock state when given a new move order.
                self.cancel_depot_dock(*entity_id);
                self.cancel_aircraft_dock(*entity_id);
                self.release_docked_idle(*entity_id);
                // Clear attack and order intent.
                if let Some(e) = self.entities.get_mut(*entity_id) {
                    e.attack_target = None;
                    e.order_intent = None;
                    e.dock_state = None;
                    Self::clear_aircraft_dock_phase(e);
                }
                // Snapshot speed, locomotor, and rules data in one lookup.
                let Some(info) = self.resolve_move_info(*entity_id, rules) else {
                    return false;
                };
                // Chrono Miners (Teleporter=yes + Harvester=yes) drive normally for
                // player commands — they only teleport on return-to-refinery
                // (handled by miner_system::chrono_teleport, not here).
                let use_teleport_move = !info.is_harvester
                    && (info.loco_kind == Some(LocomotorKind::Teleport) || info.is_teleporter);

                // Build entity block set for friendly-passable pathfinding.
                let (entity_blocks, entity_block_map) = bump_crush::build_entity_block_set(
                    &self.entities,
                    command_owner,
                    &self.house_alliances,
                    &self.interner,
                );
                let general_rules = rules.map(|r| &r.general);
                let result = if use_teleport_move {
                    // Teleport locomotor or non-harvester Teleporter=yes: instant relocation.
                    let default_general = crate::rules::ruleset::GeneralRules::default();
                    teleport_movement::issue_teleport_command(
                        &mut self.entities,
                        *entity_id,
                        (*target_rx, *target_ry),
                        general_rules.unwrap_or(&default_general),
                    )
                } else if info.loco_kind == Some(LocomotorKind::Tunnel) {
                    // Tunnel locomotor: short routes use surface, long routes burrow.
                    let Some(grid) = path_grid else { return false };
                    let tunnel_speed = rules
                        .map(|r| r.general.tunnel_speed)
                        .unwrap_or(SimFixed::from_num(6));
                    let cost_grid = self.terrain_costs.get(&info.speed_type);
                    tunnel_movement::issue_tunnel_move_command(
                        grid,
                        (*target_rx, *target_ry),
                        info.speed,
                        tunnel_speed,
                        cost_grid,
                        info.movement_zone,
                        &mut self.entities,
                        *entity_id,
                    )
                } else if info.loco_layer == MovementLayer::Air {
                    // Jumpjet infantry walk fallback: ≤3 cells + !HoverAttack → ground walk.
                    if info.loco_kind == Some(LocomotorKind::Jumpjet) && info.is_infantry {
                        let dx = (*target_rx as i32 - info.position.0 as i32).unsigned_abs();
                        let dy = (*target_ry as i32 - info.position.1 as i32).unsigned_abs();
                        let dist_cells = dx.max(dy);
                        if jumpjet_movement::should_use_walk_fallback(
                            info.hover_attack,
                            true,
                            dist_cells,
                        ) {
                            let Some(grid) = path_grid else { return false };
                            let cost_grid = self.terrain_costs.get(&info.speed_type);
                            return movement::issue_move_command_with_layered(
                                &mut self.entities,
                                grid,
                                *entity_id,
                                (*target_rx, *target_ry),
                                info.speed,
                                *queue,
                                cost_grid,
                                Some(&entity_blocks),
                                self.resolved_terrain.as_ref(),
                                Some(&entity_block_map),
                            );
                        }
                    }
                    // Air units fly in straight lines — no A* pathfinding needed.
                    let ok = air_movement::issue_air_move_command(
                        &mut self.entities,
                        *entity_id,
                        (*target_rx, *target_ry),
                        info.speed,
                    );
                    // Set Move mission so the aircraft flies to destination
                    // before the Idle handler can redirect it to RTB.
                    if ok {
                        if let Some(e) = self.entities.get_mut(*entity_id) {
                            if e.aircraft_mission.is_some() {
                                e.aircraft_mission =
                                    Some(crate::sim::aircraft::AircraftMission::Move {
                                        sub_state: 0,
                                    });
                            }
                        }
                    }
                    ok
                } else {
                    let Some(grid) = path_grid else { return false };
                    let cost_grid = self.terrain_costs.get(&info.speed_type);
                    movement::issue_move_command_with_layered(
                        &mut self.entities,
                        grid,
                        *entity_id,
                        (*target_rx, *target_ry),
                        info.speed,
                        *queue,
                        cost_grid,
                        Some(&entity_blocks),
                        self.resolved_terrain.as_ref(),
                        Some(&entity_block_map),
                    )
                };
                // Stamp acceleration/deceleration parameters onto the newly created
                // MovementTarget so the per-tick movement loop can ramp speed.
                if result {
                    if let Some(e) = self.entities.get_mut(*entity_id) {
                        if let Some(ref mut mt) = e.movement_target {
                            mt.accel_factor = info.accel_factor;
                            mt.decel_factor = info.decel_factor;
                            mt.slowdown_distance = info.slowdown_distance;
                            mt.group_id = *group_id;
                        }
                    }
                }
                result
            }
            Command::Stop { entity_id } => {
                if !self.entity_owned_by_id(command_owner, *entity_id) {
                    return false;
                }
                // Cancel any dock state before clearing movement.
                self.cancel_depot_dock(*entity_id);
                if let Some(e) = self.entities.get_mut(*entity_id) {
                    e.movement_target = None;
                    e.attack_target = None;
                    e.order_intent = None;
                    e.dock_state = None;
                }
                // Cancel any special locomotor states in progress.
                if let Some(e) = self.entities.get_mut(*entity_id) {
                    e.teleport_state = None;
                    e.tunnel_state = None;
                    e.droppod_state = None;
                    // Restore ground layer and base locomotor if overridden.
                    if let Some(ref mut loco) = e.locomotor {
                        if loco.layer == MovementLayer::Underground {
                            loco.layer = MovementLayer::Ground;
                        }
                        if loco.is_overridden() {
                            loco.end_override();
                        }
                    }
                }
                true
            }
            Command::Attack {
                attacker_id,
                target_id,
            } => {
                if !self.entity_owned_by_id(command_owner, *attacker_id) {
                    return false;
                }
                if !self.entities.contains(*target_id) {
                    return false;
                }
                if !self.can_attack_target_by_id(*attacker_id, *target_id) {
                    return false;
                }
                // Cancel aircraft RTB if interruptible.
                self.cancel_aircraft_dock(*attacker_id);
                self.release_docked_idle(*attacker_id);
                if let Some(e) = self.entities.get_mut(*attacker_id) {
                    e.order_intent = None;
                    Self::clear_aircraft_dock_phase(e);
                }
                combat::issue_attack_command(
                    &mut self.entities,
                    *attacker_id,
                    *target_id,
                    rules,
                    &self.interner,
                )
            }
            Command::ForceAttack {
                attacker_id,
                target_id,
            } => {
                if !self.entity_owned_by_id(command_owner, *attacker_id) {
                    return false;
                }
                if !self.entities.contains(*target_id) {
                    return false;
                }
                // Force-attack bypasses friendship check (Ctrl+click).
                self.release_docked_idle(*attacker_id);
                if let Some(e) = self.entities.get_mut(*attacker_id) {
                    e.order_intent = None;
                }
                combat::issue_attack_command(
                    &mut self.entities,
                    *attacker_id,
                    *target_id,
                    rules,
                    &self.interner,
                )
            }
            Command::AttackMove {
                entity_id,
                target_rx,
                target_ry,
                queue,
            } => {
                if !self.entity_owned_by_id(command_owner, *entity_id) {
                    return false;
                }
                self.release_docked_idle(*entity_id);
                if let Some(e) = self.entities.get_mut(*entity_id) {
                    e.attack_target = None;
                }

                // Snapshot speed, locomotor, and rules data in one lookup.
                let Some(info) = self.resolve_move_info(*entity_id, rules) else {
                    return false;
                };
                // Chrono Miners drive normally for player commands.
                let use_teleport_move = !info.is_harvester
                    && (info.loco_kind == Some(LocomotorKind::Teleport) || info.is_teleporter);

                let (entity_blocks, entity_block_map) = bump_crush::build_entity_block_set(
                    &self.entities,
                    command_owner,
                    &self.house_alliances,
                    &self.interner,
                );
                let default_general = crate::rules::ruleset::GeneralRules::default();
                let general_rules_ref = rules.map(|r| &r.general).unwrap_or(&default_general);
                let issued = if use_teleport_move {
                    teleport_movement::issue_teleport_command(
                        &mut self.entities,
                        *entity_id,
                        (*target_rx, *target_ry),
                        general_rules_ref,
                    )
                } else if info.loco_layer == MovementLayer::Air {
                    // Air units fly in straight lines.
                    let ok = air_movement::issue_air_move_command(
                        &mut self.entities,
                        *entity_id,
                        (*target_rx, *target_ry),
                        info.speed,
                    );
                    if ok {
                        if let Some(e) = self.entities.get_mut(*entity_id) {
                            if e.aircraft_mission.is_some() {
                                e.aircraft_mission =
                                    Some(crate::sim::aircraft::AircraftMission::Move {
                                        sub_state: 0,
                                    });
                            }
                        }
                    }
                    ok
                } else {
                    let Some(grid) = path_grid else { return false };
                    let cost_grid = self.terrain_costs.get(&info.speed_type);
                    movement::issue_move_command_with_layered(
                        &mut self.entities,
                        grid,
                        *entity_id,
                        (*target_rx, *target_ry),
                        info.speed,
                        *queue,
                        cost_grid,
                        Some(&entity_blocks),
                        self.resolved_terrain.as_ref(),
                        Some(&entity_block_map),
                    )
                };
                if issued {
                    if let Some(e) = self.entities.get_mut(*entity_id) {
                        e.order_intent = Some(OrderIntent::AttackMove {
                            goal_rx: *target_rx,
                            goal_ry: *target_ry,
                        });
                    }
                }
                issued
            }
            Command::Guard {
                entity_id,
                target_id,
            } => self.apply_guard_command(command_owner, *entity_id, *target_id, rules),
            Command::DeployMcv { entity_id } => {
                let Some(rules) = rules else { return false };
                if !self.entity_owned_by_id(command_owner, *entity_id) {
                    return false;
                }
                self.deploy_mcv(*entity_id, rules, height_map)
            }
            Command::UndeployBuilding { entity_id } => {
                let Some(rules) = rules else { return false };
                if !self.entity_owned_by_id(command_owner, *entity_id) {
                    return false;
                }
                self.undeploy_building(*entity_id, rules)
            }
            Command::SetRally { owner, rx, ry } => {
                production::set_rally_point_for_owner(self, owner, *rx, *ry);
                true
            }
            Command::QueueProduction { owner, type_id, .. } => {
                let Some(rules) = rules else { return false };
                let owner_s = self.interner.resolve(*owner).to_string();
                let type_s = self.interner.resolve(*type_id).to_string();
                production::enqueue_by_type(self, rules, &owner_s, &type_s)
            }
            Command::TogglePauseProduction { owner, category } => {
                let owner_s = self.interner.resolve(*owner).to_string();
                production::toggle_pause_for_owner_category(self, &owner_s, *category)
            }
            Command::CycleProducerFocus { owner, category } => {
                let Some(rules) = rules else { return false };
                let owner_s = self.interner.resolve(*owner).to_string();
                production::cycle_active_producer_for_owner_category(
                    self, rules, &owner_s, *category,
                )
            }
            Command::PlaceReadyBuilding {
                owner,
                type_id,
                rx,
                ry,
            } => {
                let Some(rules) = rules else { return false };
                let owner_s = self.interner.resolve(*owner).to_string();
                let type_s = self.interner.resolve(*type_id).to_string();
                production::place_ready_building(
                    self, rules, &owner_s, &type_s, *rx, *ry, path_grid, height_map,
                )
            }
            Command::CancelLastProduction { owner } => {
                let Some(rules) = rules else { return false };
                let owner_s = self.interner.resolve(*owner).to_string();
                production::cancel_last_for_owner(self, rules, &owner_s)
            }
            Command::CancelProductionByType { owner, type_id } => {
                let Some(rules) = rules else { return false };
                let owner_s = self.interner.resolve(*owner).to_string();
                let type_s = self.interner.resolve(*type_id).to_string();
                production::cancel_by_type_for_owner(self, rules, &owner_s, &type_s)
            }
            Command::SellBuilding { entity_id } => {
                let Some(rules) = rules else { return false };
                if !self.entity_owned_by_id(command_owner, *entity_id) {
                    return false;
                }
                production::sell_building(self, rules, *entity_id)
            }
            Command::ToggleRepair { entity_id } => {
                if !self.entity_owned_by_id(command_owner, *entity_id) {
                    return false;
                }
                production::toggle_repair(self, *entity_id)
            }
            Command::MinerReturn { entity_id } => {
                if !self.entity_owned_by_id(command_owner, *entity_id) {
                    return false;
                }
                // Update miner state in EntityStore.
                let Some(e) = self.entities.get_mut(*entity_id) else {
                    return false;
                };
                let Some(ref mut miner) = e.miner else {
                    return false;
                };
                miner.forced_return = true;
                miner.state = crate::sim::miner::MinerState::ForcedReturn;
                // Clear any in-progress movement — the miner system will path to refinery.
                e.movement_target = None;
                true
            }
            Command::RepairAtDepot {
                entity_id,
                depot_id,
            } => {
                let Some(rules) = rules else { return false };
                if !self.entity_owned_by_id(command_owner, *entity_id) {
                    return false;
                }
                // Validate depot exists, is friendly, and has UnitRepair=yes.
                let depot_info = self.entities.get(*depot_id).and_then(|depot| {
                    if !command_owner.eq_ignore_ascii_case(self.interner.resolve(depot.owner)) {
                        return None;
                    }
                    let obj = rules.object(self.interner.resolve(depot.type_ref))?;
                    if !obj.unit_repair {
                        return None;
                    }
                    Some((depot.position.rx, depot.position.ry, obj.foundation.clone()))
                });
                let Some((depot_rx, depot_ry, foundation)) = depot_info else {
                    return false;
                };
                // Validate entity is a unit or infantry (not structure/aircraft).
                let entity_ok = self.entities.get(*entity_id).is_some_and(|e| {
                    matches!(
                        e.category,
                        crate::map::entities::EntityCategory::Unit
                            | crate::map::entities::EntityCategory::Infantry
                    ) && e.health.current < e.health.max
                        && !e.dying
                });
                if !entity_ok {
                    return false;
                }
                // Cancel any existing dock state.
                self.cancel_depot_dock(*entity_id);
                // Set dock state and issue move toward depot.
                let (dock_rx, dock_ry) =
                    building_dock::depot_dock_cell(depot_rx, depot_ry, &foundation);
                if let Some(e) = self.entities.get_mut(*entity_id) {
                    e.attack_target = None;
                    e.order_intent = None;
                    e.dock_state = Some(DockState {
                        dock_building_id: *depot_id,
                        phase: DockPhase::Approach,
                        service_timer: 0,
                        no_funds_ticks: 0,
                    });
                }
                // Issue movement toward dock cell.
                let info = self.resolve_move_info(*entity_id, Some(rules));
                let speed = info
                    .as_ref()
                    .map(|i| i.speed)
                    .unwrap_or(ra2_speed_to_leptons_per_second(4));
                let speed_type = info
                    .as_ref()
                    .map(|i| i.speed_type)
                    .unwrap_or(SpeedType::Track);
                let (entity_blocks, entity_block_map) = bump_crush::build_entity_block_set(
                    &self.entities,
                    command_owner,
                    &self.house_alliances,
                    &self.interner,
                );
                if let Some(grid) = path_grid {
                    let cost_grid = self.terrain_costs.get(&speed_type);
                    movement::issue_move_command_with_layered(
                        &mut self.entities,
                        grid,
                        *entity_id,
                        (dock_rx, dock_ry),
                        speed,
                        false,
                        cost_grid,
                        Some(&entity_blocks),
                        self.resolved_terrain.as_ref(),
                        Some(&entity_block_map),
                    );
                }
                true
            }
            Command::EnterTransport {
                passenger_id,
                transport_id,
            } => {
                let Some(rules) = rules else { return false };
                if !self.entity_owned_by_id(command_owner, *passenger_id) {
                    return false;
                }
                // Validate transport exists and has cargo capacity.
                let transport_info = self.entities.get(*transport_id).and_then(|t| {
                    let obj = rules.object(self.interner.resolve(t.type_ref))?;
                    let cargo = t.passenger_role.cargo()?;
                    Some((t.position.rx, t.position.ry, obj.clone(), cargo.clone()))
                });
                let Some((trx, try_, transport_obj, cargo)) = transport_info else {
                    return false;
                };
                // Validate passenger can enter.
                let pax_ok = self.entities.get(*passenger_id).and_then(|p| {
                    let pobj = rules.object(self.interner.resolve(p.type_ref))?;
                    if passenger::can_enter_transport(
                        p,
                        self.entities.get(*transport_id)?,
                        pobj,
                        &transport_obj,
                        &cargo,
                        rules.general.condition_red_x1000,
                        &self.interner,
                    ) {
                        Some(())
                    } else {
                        None
                    }
                });
                if pax_ok.is_none() {
                    return false;
                }
                // Clear existing state on the passenger.
                if let Some(e) = self.entities.get_mut(*passenger_id) {
                    e.attack_target = None;
                    e.order_intent = None;
                    e.dock_state = None;
                    e.passenger_role = passenger::PassengerRole::Boarding {
                        target_transport_id: *transport_id,
                        phase: passenger::BoardingPhase::Approach,
                    };
                }
                // Issue movement toward transport cell.
                let info = self.resolve_move_info(*passenger_id, Some(rules));
                let speed = info
                    .as_ref()
                    .map(|i| i.speed)
                    .unwrap_or(ra2_speed_to_leptons_per_second(4));
                let speed_type = info
                    .as_ref()
                    .map(|i| i.speed_type)
                    .unwrap_or(SpeedType::Track);
                let (entity_blocks, entity_block_map) = bump_crush::build_entity_block_set(
                    &self.entities,
                    command_owner,
                    &self.house_alliances,
                    &self.interner,
                );
                if let Some(grid) = path_grid {
                    let cost_grid = self.terrain_costs.get(&speed_type);
                    movement::issue_move_command_with_layered(
                        &mut self.entities,
                        grid,
                        *passenger_id,
                        (trx, try_),
                        speed,
                        false,
                        cost_grid,
                        Some(&entity_blocks),
                        self.resolved_terrain.as_ref(),
                        Some(&entity_block_map),
                    );
                }
                true
            }
            Command::UnloadPassengers { transport_id } => {
                if !self.entity_owned_by_id(command_owner, *transport_id) {
                    return false;
                }
                let has_passengers = self
                    .entities
                    .get(*transport_id)
                    .and_then(|t| t.passenger_role.cargo())
                    .is_some_and(|c| !c.is_empty());
                if !has_passengers {
                    return false;
                }
                if let Some(e) = self.entities.get_mut(*transport_id) {
                    e.order_intent = Some(OrderIntent::Unloading);
                }
                true
            }
            Command::HarvestCell {
                entity_id,
                target_rx,
                target_ry,
            } => {
                if !self.entity_owned_by_id(command_owner, *entity_id) {
                    return false;
                }
                let Some(e) = self.entities.get_mut(*entity_id) else {
                    return false;
                };
                let Some(ref mut miner) = e.miner else {
                    return false;
                };
                miner.target_ore_cell = Some((*target_rx, *target_ry));
                miner.state = crate::sim::miner::MinerState::MoveToOre;
                // Clear in-progress movement so the miner re-paths to the new target.
                e.movement_target = None;
                true
            }
            Command::CaptureBuilding {
                engineer_id,
                target_building_id,
            } => {
                let Some(rules) = rules else { return false };
                if !self.entity_owned_by_id(command_owner, *engineer_id) {
                    return false;
                }
                // Validate engineer has Engineer=yes flag.
                let eng_ok = self.entities.get(*engineer_id).and_then(|e| {
                    let obj = rules.object(self.interner.resolve(e.type_ref))?;
                    obj.engineer.then_some(())
                });
                if eng_ok.is_none() {
                    return false;
                }
                // Validate target is a capturable enemy building.
                let target_info = self.entities.get(*target_building_id).and_then(|b| {
                    if b.category != crate::map::entities::EntityCategory::Structure {
                        return None;
                    }
                    if b.dying {
                        return None;
                    }
                    let obj = rules.object(self.interner.resolve(b.type_ref))?;
                    if !obj.capturable {
                        return None;
                    }
                    Some((b.position.rx, b.position.ry, b.owner))
                });
                let Some((trx, try_, target_owner)) = target_info else {
                    return false;
                };
                // Must be an enemy building.
                if crate::map::houses::are_houses_friendly(
                    &self.house_alliances,
                    command_owner,
                    self.interner.resolve(target_owner),
                ) {
                    return false;
                }
                // Clear conflicting state and set capture target.
                if let Some(e) = self.entities.get_mut(*engineer_id) {
                    e.attack_target = None;
                    e.order_intent = None;
                    e.dock_state = None;
                    e.capture_target = Some(*target_building_id);
                }
                // Issue movement toward the building's cell.
                let info = self.resolve_move_info(*engineer_id, Some(rules));
                let speed = info
                    .as_ref()
                    .map(|i| i.speed)
                    .unwrap_or(ra2_speed_to_leptons_per_second(4));
                let speed_type = info
                    .as_ref()
                    .map(|i| i.speed_type)
                    .unwrap_or(crate::rules::locomotor_type::SpeedType::Foot);
                let (entity_blocks, entity_block_map) =
                    crate::sim::movement::bump_crush::build_entity_block_set(
                        &self.entities,
                        command_owner,
                        &self.house_alliances,
                        &self.interner,
                    );
                if let Some(grid) = path_grid {
                    let cost_grid = self.terrain_costs.get(&speed_type);
                    movement::issue_move_command_with_layered(
                        &mut self.entities,
                        grid,
                        *engineer_id,
                        (trx, try_),
                        speed,
                        false,
                        cost_grid,
                        Some(&entity_blocks),
                        self.resolved_terrain.as_ref(),
                        Some(&entity_block_map),
                    );
                }
                true
            }
            Command::LaunchSuperWeapon {
                sw_type_id,
                target_rx,
                target_ry,
            } => {
                if !self.game_options.super_weapons {
                    return false;
                }
                let owner_iid = self.interner.intern(command_owner);
                let sw_type_str = self.interner.resolve(*sw_type_id).to_string();

                // Look up the instance and verify it's ready.
                let is_ready = self
                    .super_weapons
                    .get(&owner_iid)
                    .and_then(|weapons| weapons.get(sw_type_id))
                    .map_or(false, |inst| inst.is_active && inst.is_ready);
                if !is_ready {
                    log::warn!(
                        "LaunchSuperWeapon '{}' by '{}' — not ready",
                        sw_type_str,
                        command_owner,
                    );
                    return false;
                }

                // Look up the type to determine dispatch kind.
                let Some(sw_type) = rules.and_then(|r| r.super_weapon(&sw_type_str)) else {
                    return false;
                };
                let kind = sw_type.kind;
                let recharge = sw_type.recharge_time_frames;

                // Dispatch based on kind.
                let success = match kind {
                    crate::rules::superweapon_type::SuperWeaponKind::LightningStorm => {
                        let rules = rules.unwrap();
                        crate::sim::superweapon::lightning_storm::start(
                            self, rules, owner_iid, *target_rx, *target_ry,
                        )
                    }
                    crate::rules::superweapon_type::SuperWeaponKind::IronCurtain => {
                        let rules = rules.unwrap();
                        crate::sim::superweapon::iron_curtain::launch(
                            self, rules, owner_iid, *target_rx, *target_ry,
                        )
                    }
                    crate::rules::superweapon_type::SuperWeaponKind::ForceShield => {
                        let rules = rules.unwrap();
                        crate::sim::superweapon::force_shield::launch(
                            self, rules, owner_iid, *target_rx, *target_ry,
                        )
                    }
                    crate::rules::superweapon_type::SuperWeaponKind::GeneticConverter => {
                        let rules = rules.unwrap();
                        crate::sim::superweapon::genetic_converter::launch(
                            self, rules, owner_iid, *target_rx, *target_ry,
                        )
                    }
                    crate::rules::superweapon_type::SuperWeaponKind::PsychicReveal => {
                        let rules = rules.unwrap();
                        crate::sim::superweapon::psychic_reveal::launch(
                            self, rules, owner_iid, *target_rx, *target_ry,
                        )
                    }
                    other => {
                        log::warn!("SuperWeapon kind {:?} not yet implemented", other);
                        false
                    }
                };

                if success {
                    // Reset the instance — restart charging.
                    if let Some(weapons) = self.super_weapons.get_mut(&owner_iid) {
                        if let Some(inst) = weapons.get_mut(sw_type_id) {
                            inst.reset_after_fire(recharge, self.tick);
                        }
                    }
                }
                success
            }
        }
    }

    /// Cancel depot dock reservation for an entity. Called before issuing new orders.
    fn cancel_depot_dock(&mut self, entity_id: u64) {
        if let Some(e) = self.entities.get(entity_id) {
            if let Some(ref ds) = e.dock_state {
                self.production
                    .depot_dock_reservations
                    .cancel(ds.dock_building_id, entity_id);
            }
        }
    }

    /// Cancel aircraft dock reservation if in ReturnToBase or WaitForDock phase.
    fn cancel_aircraft_dock(&mut self, entity_id: u64) {
        if let Some(e) = self.entities.get(entity_id) {
            if let Some(ref ammo) = e.aircraft_ammo {
                use crate::sim::docking::aircraft_dock::AircraftDockPhase;
                if matches!(
                    ammo.dock_phase,
                    Some(AircraftDockPhase::ReturnToBase) | Some(AircraftDockPhase::WaitForDock)
                ) {
                    self.production.airfield_docks.cancel(entity_id);
                }
            }
        }
    }

    /// Clear aircraft dock phase on an entity if interruptible (RTB/WaitForDock).
    fn clear_aircraft_dock_phase(entity: &mut crate::sim::game_entity::GameEntity) {
        if let Some(ref mut ammo) = entity.aircraft_ammo {
            use crate::sim::docking::aircraft_dock::AircraftDockPhase;
            if matches!(
                ammo.dock_phase,
                Some(AircraftDockPhase::ReturnToBase) | Some(AircraftDockPhase::WaitForDock)
            ) {
                ammo.dock_phase = None;
                ammo.target_airfield = None;
            }
        }
    }

    /// Release a DockedIdle aircraft from its helipad and trigger takeoff.
    /// Called when a docked aircraft receives a Move or Attack command.
    fn release_docked_idle(&mut self, entity_id: u64) {
        let Some(entity) = self.entities.get_mut(entity_id) else {
            return;
        };
        if let Some(crate::sim::aircraft::AircraftMission::DockedIdle { .. }) =
            entity.aircraft_mission
        {
            // Release dock slot.
            self.production.airfield_docks.release(entity_id);
            // Clear to Idle — the command handler will set the appropriate mission.
            entity.aircraft_mission = Some(crate::sim::aircraft::AircraftMission::Idle);
            // Trigger takeoff.
            if let Some(ref mut loco) = entity.locomotor {
                if loco.air_phase == crate::sim::movement::locomotor::AirMovePhase::Landed {
                    loco.air_phase = crate::sim::movement::locomotor::AirMovePhase::Ascending;
                }
            }
        }
    }

    /// Replace the current selection with exactly the given stable entity IDs.
    fn apply_selection_snapshot(&mut self, stable_ids: &[u64]) -> bool {
        // Deselect all via EntityStore.
        let keys: Vec<u64> = self.entities.keys_sorted();
        for &id in &keys {
            if let Some(e) = self.entities.get_mut(id) {
                e.selected = false;
            }
        }
        // Select the requested IDs.
        for &stable_id in stable_ids {
            if let Some(e) = self.entities.get_mut(stable_id) {
                e.selected = true;
            }
        }
        true
    }

    /// Check ownership using stable_id via EntityStore.
    pub(crate) fn entity_owned_by_id(&self, command_owner: &str, stable_id: u64) -> bool {
        self.entities
            .get(stable_id)
            .is_some_and(|e| command_owner.eq_ignore_ascii_case(self.interner.resolve(e.owner)))
    }

    /// Check whether the attacker can attack the target (i.e. they are not allies).
    /// Uses EntityStore for ownership lookup.
    fn can_attack_target_by_id(&self, attacker_id: u64, target_id: u64) -> bool {
        let Some(attacker) = self.entities.get(attacker_id) else {
            return false;
        };
        let Some(target) = self.entities.get(target_id) else {
            return false;
        };
        !are_houses_friendly(
            &self.house_alliances,
            self.interner.resolve(attacker.owner),
            self.interner.resolve(target.owner),
        )
    }

    /// Apply a Guard command: anchor at current position, optionally attack a target.
    fn apply_guard_command(
        &mut self,
        command_owner: &str,
        entity_id: u64,
        target_id: Option<u64>,
        rules: Option<&RuleSet>,
    ) -> bool {
        if !self.entity_owned_by_id(command_owner, entity_id) {
            return false;
        }
        let anchor = self
            .entities
            .get(entity_id)
            .map(|e| (e.position.rx, e.position.ry));
        let Some((anchor_rx, anchor_ry)) = anchor else {
            return false;
        };
        if let Some(e) = self.entities.get_mut(entity_id) {
            e.movement_target = None;
        }
        match target_id.filter(|&tid| self.entities.contains(tid)) {
            Some(tid) => {
                if !self.can_attack_target_by_id(entity_id, tid) {
                    return false;
                }
                let issued = combat::issue_attack_command(
                    &mut self.entities,
                    entity_id,
                    tid,
                    rules,
                    &self.interner,
                );
                if issued {
                    if let Some(e) = self.entities.get_mut(entity_id) {
                        e.order_intent = Some(OrderIntent::Guard {
                            anchor_rx,
                            anchor_ry,
                        });
                    }
                }
                issued
            }
            None => {
                if let Some(e) = self.entities.get_mut(entity_id) {
                    e.attack_target = None;
                    e.order_intent = Some(OrderIntent::Guard {
                        anchor_rx,
                        anchor_ry,
                    });
                }
                true
            }
        }
    }
}
