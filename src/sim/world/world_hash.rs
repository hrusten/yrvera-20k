//! Deterministic state hashing for the Simulation.
//!
//! Produces a reproducible u64 hash over the entire simulation state:
//! tick counter, RNG state, production queues, fog-of-war, entity components.
//! Used for replay verification and desync detection in multiplayer.
//!
//! Dependency rules: same as sim/ (depends on rules/, map/; never render/ui/audio/net).

use std::hash::{Hash, Hasher};

use super::Simulation;

impl Simulation {
    /// Deterministic state hash over canonicalized simulation state.
    ///
    /// Hashes tick, RNG, production, fog, alliances, and all entity components
    /// in stable-entity-ID order (EntityStore keys_sorted) for determinism.
    pub fn state_hash(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();

        self.tick.hash(&mut hasher);
        self.rng.state().hash(&mut hasher);
        self.next_stable_entity_id.hash(&mut hasher);

        self.hash_game_options(&mut hasher);
        self.hash_houses(&mut hasher);
        self.hash_production(&mut hasher);
        self.hash_power_states(&mut hasher);
        self.hash_fog_and_alliances(&mut hasher);
        self.hash_bridge_state(&mut hasher);
        self.hash_entities(&mut hasher);

        hasher.finish()
    }

    /// Hash per-match game options for lockstep verification.
    fn hash_game_options(&self, hasher: &mut impl Hasher) {
        let opts = &self.game_options;
        opts.short_game.hash(hasher);
        opts.bases.hash(hasher);
        opts.bridges_destroyable.hash(hasher);
        opts.super_weapons.hash(hasher);
        opts.build_off_ally.hash(hasher);
        opts.crates.hash(hasher);
        opts.mcv_redeploy.hash(hasher);
        opts.fog_of_war.hash(hasher);
        opts.shroud.hash(hasher);
        opts.tiberium_grows.hash(hasher);
        opts.multi_engineer.hash(hasher);
        opts.harvester_truce.hash(hasher);
        opts.ally_change_allowed.hash(hasher);
        opts.starting_credits.hash(hasher);
        opts.unit_count.hash(hasher);
        opts.tech_level.hash(hasher);
        opts.game_speed.hash(hasher);
        opts.ai_difficulty.hash(hasher);
        opts.ai_players.hash(hasher);
    }

    /// Hash per-player house state (BTreeMap = deterministic order).
    fn hash_houses(&self, hasher: &mut impl Hasher) {
        for (owner, house) in &self.houses {
            owner.hash(hasher);
            house.credits.hash(hasher);
            house.side_index.hash(hasher);
            house.is_human.hash(hasher);
            house.is_defeated.hash(hasher);
            house.has_won.hash(hasher);
            house.has_lost.hash(hasher);
            house.owned_building_count.hash(hasher);
            house.owned_unit_count.hash(hasher);
            house.tech_level.hash(hasher);
            if let Some((rx, ry)) = house.rally_point {
                1u8.hash(hasher);
                rx.hash(hasher);
                ry.hash(hasher);
            } else {
                0u8.hash(hasher);
            }
            if let Some((rx, ry)) = house.base_center {
                1u8.hash(hasher);
                rx.hash(hasher);
                ry.hash(hasher);
            } else {
                0u8.hash(hasher);
            }
        }
    }

    /// Hash all production-related state: queues, ready items, resources.
    fn hash_production(&self, hasher: &mut impl Hasher) {
        for (owner, queues) in &self.production.queues_by_owner {
            owner.hash(hasher);
            for (category, queue) in queues {
                category.hash(hasher);
                for item in queue {
                    item.owner.hash(hasher);
                    item.type_id.hash(hasher);
                    item.queue_category.hash(hasher);
                    item.state.hash(hasher);
                    item.total_base_frames.hash(hasher);
                    item.remaining_base_frames.hash(hasher);
                    item.progress_carry.hash(hasher);
                    item.enqueue_order.hash(hasher);
                }
            }
        }
        for (owner, ready) in &self.production.ready_by_owner {
            owner.hash(hasher);
            for type_id in ready {
                type_id.hash(hasher);
            }
        }
        for (owner, categories) in &self.production.active_producer_by_owner {
            owner.hash(hasher);
            for (category, sid) in categories {
                category.hash(hasher);
                sid.hash(hasher);
            }
        }
        self.production.next_enqueue_order.hash(hasher);

        for (&(rx, ry), node) in &self.production.resource_nodes {
            rx.hash(hasher);
            ry.hash(hasher);
            (node.resource_type as u8).hash(hasher);
            node.remaining.hash(hasher);
        }
        // Hash dock reservations.
        for (&ref_sid, &miner_sid) in &self.production.dock_reservations.occupied {
            ref_sid.hash(hasher);
            miner_sid.hash(hasher);
        }
        for (&ref_sid, queue) in &self.production.dock_reservations.queues {
            ref_sid.hash(hasher);
            for &miner_sid in queue {
                miner_sid.hash(hasher);
            }
        }
    }

    /// Hash per-player power states for deterministic replay.
    fn hash_power_states(&self, hasher: &mut impl Hasher) {
        // BTreeMap<InternedId, _> iterates in deterministic sorted order.
        for (owner_id, state) in &self.power_states {
            owner_id.hash(hasher);
            state.total_output.hash(hasher);
            state.total_drain.hash(hasher);
            state.spy_blackout_remaining.hash(hasher);
            state.degradation_accum_ms.hash(hasher);
        }
    }

    /// Hash fog-of-war visibility and house alliance data.
    fn hash_fog_and_alliances(&self, hasher: &mut impl Hasher) {
        self.fog.width.hash(hasher);
        self.fog.height.hash(hasher);
        for (owner, fog) in &self.fog.by_owner {
            owner.hash(hasher);
            fog.cells_raw().hash(hasher);
        }
        for (owner, allies) in &self.house_alliances {
            owner.hash(hasher);
            for ally in allies {
                ally.hash(hasher);
            }
        }
    }

    fn hash_bridge_state(&self, hasher: &mut impl Hasher) {
        let Some(bridge_state) = &self.bridge_state else {
            0u8.hash(hasher);
            return;
        };
        1u8.hash(hasher);
        let mut entries: Vec<_> = bridge_state.iter_cells().collect();
        entries.sort_by_key(|((rx, ry), _)| (*rx, *ry));
        for ((rx, ry), cell) in entries {
            rx.hash(hasher);
            ry.hash(hasher);
            cell.deck_present.hash(hasher);
            cell.destroyed.hash(hasher);
            cell.destroyable.hash(hasher);
            cell.deck_level.hash(hasher);
            cell.bridge_group_id.hash(hasher);
        }
    }

    /// Hash all entity components in stable-entity-ID order.
    /// BTreeMap iterates in key order (= stable_id), so no manual sort needed.
    fn hash_entities(&self, hasher: &mut impl Hasher) {
        for entity in self.entities.values() {
            entity.stable_id.hash(hasher);
            entity.position.rx.hash(hasher);
            entity.position.ry.hash(hasher);
            entity.position.z.hash(hasher);
            entity.facing.hash(hasher);
            entity.owner.hash(hasher);
            entity.health.current.hash(hasher);
            entity.health.max.hash(hasher);
            entity.type_ref.hash(hasher);
            (entity.category as u8).hash(hasher);
            entity.vision_range.hash(hasher);

            if let Some(ref movement) = entity.movement_target {
                1u8.hash(hasher);
                movement.next_index.hash(hasher);
                movement.speed.hash(hasher);
                movement.movement_delay.hash(hasher);
                movement.blocked_delay.hash(hasher);
                movement.path_blocked.hash(hasher);
                movement.path_stuck_counter.hash(hasher);
                movement.path.hash(hasher);
                movement.path_layers.hash(hasher);
            } else {
                0u8.hash(hasher);
            }

            if let Some(ref loco) = entity.locomotor {
                1u8.hash(hasher);
                (loco.kind as u8).hash(hasher);
                (loco.layer as u8).hash(hasher);
                (loco.phase as u8).hash(hasher);
            } else {
                0u8.hash(hasher);
            }

            if let Some(ref bridge) = entity.bridge_occupancy {
                1u8.hash(hasher);
                bridge.deck_level.hash(hasher);
            } else {
                0u8.hash(hasher);
            }

            if let Some(ref attack) = entity.attack_target {
                1u8.hash(hasher);
                attack.cooldown_ticks.hash(hasher);
                attack.target.hash(hasher);
            } else {
                0u8.hash(hasher);
            }

            entity.capture_target.hash(hasher);

            if let Some(ref miner) = entity.miner {
                1u8.hash(hasher);
                (miner.state as u8).hash(hasher);
                (miner.kind as u8).hash(hasher);
                (miner.cargo.len() as u16).hash(hasher);
                for bale in &miner.cargo {
                    (bale.resource_type as u8).hash(hasher);
                    bale.value.hash(hasher);
                }
                miner.home_refinery.hash(hasher);
                miner.reserved_refinery.hash(hasher);
                miner.target_ore_cell.hash(hasher);
                miner.harvest_timer.hash(hasher);
                miner.unload_timer.hash(hasher);
                miner.forced_return.hash(hasher);
                miner.dock_queued.hash(hasher);
            } else {
                0u8.hash(hasher);
            }

            // Passenger/transport state.
            match &entity.passenger_role {
                crate::sim::passenger::PassengerRole::None => {
                    0u8.hash(hasher);
                }
                crate::sim::passenger::PassengerRole::Transport { cargo } => {
                    1u8.hash(hasher);
                    cargo.capacity.hash(hasher);
                    (cargo.passengers.len() as u32).hash(hasher);
                    for &pid in &cargo.passengers {
                        pid.hash(hasher);
                    }
                    cargo.total_size.hash(hasher);
                }
                crate::sim::passenger::PassengerRole::Boarding {
                    target_transport_id,
                    phase,
                } => {
                    2u8.hash(hasher);
                    target_transport_id.hash(hasher);
                    (*phase as u8).hash(hasher);
                }
                crate::sim::passenger::PassengerRole::Inside { transport_id } => {
                    3u8.hash(hasher);
                    transport_id.hash(hasher);
                }
            }
            entity.ifv_weapon_index.hash(hasher);
        }
    }
}
