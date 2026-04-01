//! Order-intent tick systems for the Simulation.
//!
//! Handles automatic target acquisition for attack-move and guard orders
//! (pre-combat), and resuming movement after combat ends (post-combat).
//!
//! Dependency rules: same as sim/ (depends on rules/, map/; never render/ui/audio/net).

use super::Simulation;
use crate::map::entities::EntityCategory;
use crate::rules::ruleset::RuleSet;
use crate::sim::combat;
use crate::sim::components::OrderIntent;
use crate::sim::intern::InternedId;
use crate::sim::movement;
use crate::sim::movement::air_movement;
use crate::sim::movement::locomotor::MovementLayer;
use crate::sim::pathfinding::PathGrid;
use crate::util::fixed_math::SimFixed;
use crate::util::fixed_math::ra2_speed_to_leptons_per_second;

impl Simulation {
    /// Pre-combat: entities with an OrderIntent but no current AttackTarget
    /// try to acquire a nearby enemy to engage.
    pub(crate) fn tick_order_intents_pre_combat(&mut self, rules: &RuleSet) {
        // Collect attacker candidates from EntityStore.
        let keys: Vec<u64> = self.entities.keys_sorted();
        let mut attacker_ids: Vec<u64> = Vec::new();
        for &id in &keys {
            if let Some(entity) = self.entities.get(id) {
                if entity.order_intent.is_some() && entity.attack_target.is_none() {
                    attacker_ids.push(id);
                }
            }
        }

        for attacker_id in attacker_ids {
            let Some(target_sid) = combat::acquire_best_target_for_entity(
                &self.entities,
                rules,
                &self.interner,
                attacker_id,
                Some(&self.fog),
            ) else {
                continue;
            };
            let _ = combat::issue_attack_command(
                &mut self.entities,
                attacker_id,
                target_sid,
                Some(rules),
                &self.interner,
            );
        }
    }

    /// Post-combat: entities with an OrderIntent but no active attack or movement
    /// resume their patrol/guard movement toward the original goal.
    pub(crate) fn tick_order_intents_post_combat(
        &mut self,
        path_grid: Option<&PathGrid>,
        rules: Option<&RuleSet>,
    ) {
        let Some(grid) = path_grid else { return };
        // Collect (stable_id, goal) for entities that need to resume movement.
        let keys: Vec<u64> = self.entities.keys_sorted();
        let mut resumes: Vec<(u64, u16, u16)> = Vec::new();
        for &id in &keys {
            if let Some(entity) = self.entities.get(id) {
                let intent = match entity.order_intent {
                    Some(ref i) => *i,
                    None => continue,
                };
                if entity.attack_target.is_some() || entity.movement_target.is_some() {
                    continue;
                }
                match intent {
                    OrderIntent::AttackMove { goal_rx, goal_ry }
                        if (entity.position.rx, entity.position.ry) != (goal_rx, goal_ry) =>
                    {
                        resumes.push((id, goal_rx, goal_ry));
                    }
                    OrderIntent::Guard {
                        anchor_rx,
                        anchor_ry,
                    } if (entity.position.rx, entity.position.ry) != (anchor_rx, anchor_ry) => {
                        resumes.push((id, anchor_rx, anchor_ry));
                    }
                    _ => {}
                }
            }
        }

        for (stable_id, goal_rx, goal_ry) in resumes {
            let (base_speed, loco_multiplier, is_air) = self
                .entities
                .get(stable_id)
                .map(|e| {
                    let bs: SimFixed = rules
                        .and_then(|r| r.object(self.interner.resolve(e.type_ref)))
                        .map(|obj| ra2_speed_to_leptons_per_second(obj.speed))
                        .unwrap_or(ra2_speed_to_leptons_per_second(4));
                    let lm: SimFixed = e
                        .locomotor
                        .as_ref()
                        .map(|l| l.speed_multiplier)
                        .unwrap_or(SimFixed::from_num(1));
                    let air: bool = e
                        .locomotor
                        .as_ref()
                        .is_some_and(|l| l.layer == MovementLayer::Air);
                    (bs, lm, air)
                })
                .unwrap_or((
                    ra2_speed_to_leptons_per_second(4),
                    SimFixed::from_num(1),
                    false,
                ));
            let speed: SimFixed = (base_speed * loco_multiplier).max(SimFixed::lit("25"));

            if is_air {
                let _ = air_movement::issue_air_move_command(
                    &mut self.entities,
                    stable_id,
                    (goal_rx, goal_ry),
                    speed,
                );
            } else {
                let _ = movement::issue_move_command_with_layered(
                    &mut self.entities,
                    grid,
                    self.layered_path_grid.as_ref(),
                    stable_id,
                    (goal_rx, goal_ry),
                    speed,
                    false,
                    None,
                    None,
                    self.resolved_terrain.as_ref(),
                );
            }
        }
    }

    /// Tick engineer capture orders: check if any engineer with a capture_target
    /// has arrived adjacent to its target building. If so, transfer ownership and
    /// consume the engineer.
    /// Returns true if any capture occurred (triggers atlas rebuild for new owner color).
    pub(crate) fn tick_capture_orders(&mut self) -> bool {
        let mut any_captured = false;
        // Snapshot engineers with active capture targets.
        let captures: Vec<(u64, u64, InternedId)> = self
            .entities
            .values()
            .filter(|e| e.capture_target.is_some() && !e.dying)
            .map(|e| (e.stable_id, e.capture_target.unwrap(), e.owner))
            .collect();

        for (engineer_id, building_id, engineer_owner) in captures {
            // Check building still exists and is capturable.
            let building_ok = self
                .entities
                .get(building_id)
                .is_some_and(|b| b.category == EntityCategory::Structure && !b.dying);
            if !building_ok {
                // Target lost — clear capture order.
                if let Some(e) = self.entities.get_mut(engineer_id) {
                    e.capture_target = None;
                }
                continue;
            }

            // Distance check: adjacent = Chebyshev distance <= 1 cell.
            let (eng_rx, eng_ry) = self
                .entities
                .get(engineer_id)
                .map(|e| (e.position.rx, e.position.ry))
                .unwrap_or((0, 0));
            let (bld_rx, bld_ry) = self
                .entities
                .get(building_id)
                .map(|e| (e.position.rx, e.position.ry))
                .unwrap_or((0, 0));
            let dx = (eng_rx as i32 - bld_rx as i32).abs();
            let dy = (eng_ry as i32 - bld_ry as i32).abs();

            if dx <= 1 && dy <= 1 {
                // CAPTURE: transfer building ownership.
                let old_owner = self.entities.get(building_id).map(|b| b.owner);
                if let Some(b) = self.entities.get_mut(building_id) {
                    b.owner = engineer_owner;
                }
                // Update house owned counts for both old and new owner.
                // Resolve interned IDs to strings before &mut self calls.
                let engineer_owner_str = self.interner.resolve(engineer_owner).to_string();
                if let Some(old_owner_id) = old_owner {
                    let old_owner_str = self.interner.resolve(old_owner_id).to_string();
                    self.decrement_owned_count(&old_owner_str, EntityCategory::Structure);
                }
                self.increment_owned_count(&engineer_owner_str, EntityCategory::Structure);
                // Destroy engineer (consumed on capture).
                self.despawn_entity(engineer_id);
                any_captured = true;
            }
        }
        any_captured
    }
}
