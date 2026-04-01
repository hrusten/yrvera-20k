//! Entity spawning for the Simulation.
//!
//! Handles spawning entities from map data (`spawn_from_map`) and from
//! production (`spawn_object`). All entities are stored in EntityStore only
//! (BTreeMap<u64, GameEntity>).
//!
//! Dependency rules: same as sim/ (depends on rules/, map/; never render/ui/audio/net).

use std::collections::BTreeMap;

use super::Simulation;
use crate::map::entities::{EntityCategory, MapEntity};
use crate::map::resolved_terrain::ResolvedTerrainGrid;
use crate::rules::object_type::ObjectCategory;
use crate::rules::ruleset::RuleSet;
use crate::sim::animation::{Animation, SequenceKind};
use crate::sim::components::{
    BridgeOccupancy, BuildingDown, BuildingUp, HarvestOverlay, Health, VoxelAnimation,
};
use crate::sim::game_entity::GameEntity;
use crate::sim::miner::{miner_kind_for_object, Miner, MinerConfig};
use crate::sim::movement::locomotor::{LocomotorState, MovementLayer};
use crate::sim::production::foundation_dimensions;
use crate::sim::vision::MAX_SIGHT_RANGE;
use crate::util::fixed_math::SimFixed;

impl Simulation {
    /// Spawn entities from parsed map placements into EntityStore.
    pub fn spawn_from_map(
        &mut self,
        entities: &[MapEntity],
        rules: Option<&RuleSet>,
        height_map: &BTreeMap<(u16, u16), u8>,
    ) -> u32 {
        self.spawn_from_map_with_resolved(entities, rules, height_map, None)
    }

    pub fn spawn_from_map_with_resolved(
        &mut self,
        entities: &[MapEntity],
        rules: Option<&RuleSet>,
        height_map: &BTreeMap<(u16, u16), u8>,
        resolved_terrain: Option<&ResolvedTerrainGrid>,
    ) -> u32 {
        let mut count: u32 = 0;

        for map_ent in entities {
            let bridge_spawn = map_ent
                .high
                .then(|| {
                    resolved_terrain
                        .and_then(|terrain| terrain.cell(map_ent.cell_x, map_ent.cell_y))
                        .filter(|cell| cell.bridge_walkable)
                        .map(|cell| cell.bridge_deck_level)
                })
                .flatten();
            if map_ent.high && bridge_spawn.is_none() {
                log::warn!(
                    "Map entity {} at ({},{}) requested HIGH spawn but no bridge deck was resolved; falling back to ground",
                    map_ent.type_id,
                    map_ent.cell_x,
                    map_ent.cell_y
                );
            }
            let z: u8 = bridge_spawn.unwrap_or_else(|| {
                height_map
                    .get(&(map_ent.cell_x, map_ent.cell_y))
                    .copied()
                    .unwrap_or(0)
            });

            let max_health: u16 = rules
                .and_then(|r| r.object(&map_ent.type_id))
                .map(|obj| obj.strength as u16)
                .unwrap_or(map_ent.health);

            let health = Health {
                current: if max_health > 0 {
                    // map_ent.health is 0-256 where 256 = 100%. Convert to absolute HP.
                    ((map_ent.health as u32 * max_health as u32) / 256) as u16
                } else {
                    map_ent.health
                },
                max: if max_health > 0 {
                    max_health
                } else {
                    map_ent.health
                },
            };

            let uses_voxel_default: bool = match map_ent.category {
                EntityCategory::Unit | EntityCategory::Aircraft => true,
                EntityCategory::Infantry | EntityCategory::Structure => false,
            };
            let uses_voxel: bool = rules
                .and_then(|r| r.object(&map_ent.type_id))
                .map(|obj| {
                    matches!(
                        obj.category,
                        ObjectCategory::Vehicle | ObjectCategory::Aircraft
                    )
                })
                .unwrap_or(uses_voxel_default);

            let sight_range = rules
                .and_then(|r| r.object(&map_ent.type_id))
                .map(|obj| (obj.sight.max(0) as u16).min(MAX_SIGHT_RANGE))
                .unwrap_or_else(|| Self::default_vision_range_for_category(map_ent.category));

            let stable_id = self.allocate_stable_id();
            let owner_id = self.interner.intern(&map_ent.owner);
            let type_id = self.interner.intern(&map_ent.type_id);

            // Build the GameEntity with all required fields.
            let mut ge = GameEntity::new(
                stable_id,
                map_ent.cell_x,
                map_ent.cell_y,
                z,
                map_ent.facing,
                owner_id,
                health,
                type_id,
                map_ent.category,
                map_ent.veterancy,
                sight_range,
                uses_voxel,
            );

            if self.debug_event_logging {
                ge.debug_log = Some(crate::sim::debug_event_log::DebugEventLog::new());
            }

            // Turret facing for voxel units with Turret=yes.
            let has_turret = rules
                .and_then(|r| r.object(&map_ent.type_id))
                .map(|obj| obj.has_turret)
                .unwrap_or(false);
            if has_turret {
                ge.turret_facing = Some(crate::sim::movement::turret::body_facing_to_turret(map_ent.facing));
            }
            // VoxelAnimation default for voxel entities.
            if uses_voxel {
                ge.voxel_animation = Some(VoxelAnimation::new(1, 100));
            }
            // Infantry animation and sub-cell position.
            if map_ent.category == EntityCategory::Infantry {
                ge.animation = Some(Animation::new(SequenceKind::Stand));
                ge.sub_cell = Some(map_ent.sub_cell);
                let (lx, ly) = crate::util::lepton::subcell_lepton_offset(Some(map_ent.sub_cell));
                ge.position.sub_x = lx;
                ge.position.sub_y = ly;
            }
            // SHP vehicles (Voxel=no non-infantry units like Dolphin, Terror Drone, Squid)
            // also need Animation for walk/attack frame cycling.
            if !uses_voxel
                && (map_ent.category == EntityCategory::Unit
                    || map_ent.category == EntityCategory::Aircraft)
            {
                ge.animation = Some(Animation::new(SequenceKind::Stand));
            }
            // Crush properties from rules.ini.
            if let Some(obj) = rules.and_then(|r| r.object(&map_ent.type_id)) {
                ge.crushable = obj.crushable;
                ge.omni_crusher = obj.omni_crusher;
                ge.omni_crush_resistant = obj.omni_crush_resistant;
                ge.zfudge_bridge = obj.zfudge_bridge;
                ge.too_big_to_fit_under_bridge = obj.too_big_to_fit_under_bridge;
            }
            // Locomotor for movable entities.
            if let Some(obj) = rules.and_then(|r| r.object(&map_ent.type_id)) {
                if obj.speed > 0 {
                    let mut loco = LocomotorState::from_object_type(obj);
                    if bridge_spawn.is_some() {
                        loco.layer = MovementLayer::Bridge;
                    }
                    // TEMP: GI and Conscript move 6× faster for testing.
                    if matches!(
                        map_ent.type_id.to_uppercase().as_str(),
                        "GI" | "CONS" | "E1" | "E2"
                    ) {
                        loco.speed_multiplier = SimFixed::from_num(6);
                    }
                    ge.locomotor = Some(loco);
                }
            }
            // Bridge occupancy.
            if let Some(deck_level) = bridge_spawn {
                ge.bridge_occupancy = Some(BridgeOccupancy { deck_level });
                ge.on_bridge = true;
            }
            // Miner + harvest overlay.
            let miner_kind = rules
                .and_then(|r| r.object(&map_ent.type_id))
                .and_then(miner_kind_for_object);
            if let Some(kind) = miner_kind {
                let mcfg: MinerConfig = rules
                    .map(|r| MinerConfig::from_general_rules(&r.general))
                    .unwrap_or_default();
                ge.miner = Some(Miner::new(kind, &mcfg));
                ge.harvest_overlay = Some(HarvestOverlay {
                    frame: 0,
                    visible: false,
                    elapsed_ms: 0,
                });
            }
            // Passenger cargo for transports and garrisonable buildings.
            if let Some(obj) = rules.and_then(|r| r.object(&map_ent.type_id)) {
                if obj.passengers > 0 {
                    ge.passenger_role = crate::sim::passenger::PassengerRole::Transport {
                        cargo: crate::sim::passenger::PassengerCargo::new(
                            obj.passengers,
                            obj.size_limit,
                        ),
                    };
                } else if obj.can_be_occupied && obj.max_number_occupants > 0 {
                    // Garrisonable buildings: capacity = MaxNumberOccupants, SizeLimit = 1 (infantry only).
                    ge.passenger_role = crate::sim::passenger::PassengerRole::Transport {
                        cargo: crate::sim::passenger::PassengerCargo::new(
                            obj.max_number_occupants,
                            1,
                        ),
                    };
                }
            }

            let owner_str = self.interner.resolve(ge.owner).to_string();
            let category = ge.category;
            self.entities.insert(ge);
            self.increment_owned_count(&owner_str, category);
            count += 1;
        }

        log::info!("Spawned {} entities", count);
        count
    }

    /// Spawn one object instance (used by production). Returns the stable_id on success.
    pub fn spawn_object(
        &mut self,
        type_id: &str,
        owner: &str,
        rx: u16,
        ry: u16,
        facing: u8,
        rules: &RuleSet,
        height_map: &BTreeMap<(u16, u16), u8>,
    ) -> Option<u64> {
        let z: u8 = height_map.get(&(rx, ry)).copied().unwrap_or(0);
        self.spawn_object_at_height(type_id, owner, rx, ry, facing, z, rules)
    }

    pub(crate) fn spawn_object_at_height(
        &mut self,
        type_id: &str,
        owner: &str,
        rx: u16,
        ry: u16,
        facing: u8,
        z: u8,
        rules: &RuleSet,
    ) -> Option<u64> {
        let obj = rules.object(type_id)?;
        let health = Health {
            current: obj.strength.max(1) as u16,
            max: obj.strength.max(1) as u16,
        };
        let category = match obj.category {
            ObjectCategory::Infantry => EntityCategory::Infantry,
            ObjectCategory::Vehicle => EntityCategory::Unit,
            ObjectCategory::Aircraft => EntityCategory::Aircraft,
            ObjectCategory::Building => EntityCategory::Structure,
        };
        let uses_voxel = matches!(
            obj.category,
            ObjectCategory::Vehicle | ObjectCategory::Aircraft
        );
        let sight_range = (obj.sight.max(0) as u16).min(MAX_SIGHT_RANGE);
        let stable_id = self.allocate_stable_id();
        let owner_iid = self.interner.intern(owner);
        let type_iid = self.interner.intern(type_id);

        let mut ge = GameEntity::new(
            stable_id,
            rx,
            ry,
            z,
            facing,
            owner_iid,
            health,
            type_iid,
            category,
            0, // veterancy = rookie for production spawns
            sight_range,
            uses_voxel,
        );

        if self.debug_event_logging {
            ge.debug_log = Some(crate::sim::debug_event_log::DebugEventLog::new());
        }

        if obj.has_turret {
            ge.turret_facing = Some(crate::sim::movement::turret::body_facing_to_turret(facing));
        }
        if uses_voxel {
            ge.voxel_animation = Some(VoxelAnimation::new(1, 100));
        }
        if category == EntityCategory::Infantry {
            ge.animation = Some(Animation::new(SequenceKind::Stand));
            ge.sub_cell = Some(self.allocate_infantry_sub_cell(rx, ry));
            let (lx, ly) = crate::util::lepton::subcell_lepton_offset(ge.sub_cell);
            ge.position.sub_x = lx;
            ge.position.sub_y = ly;
        }
        // SHP vehicles also need animation for walk/attack frame cycling.
        if !uses_voxel && (category == EntityCategory::Unit || category == EntityCategory::Aircraft)
        {
            ge.animation = Some(Animation::new(SequenceKind::Stand));
        }
        ge.crushable = obj.crushable;
        ge.omni_crusher = obj.omni_crusher;
        ge.omni_crush_resistant = obj.omni_crush_resistant;
        ge.zfudge_bridge = obj.zfudge_bridge;
        ge.too_big_to_fit_under_bridge = obj.too_big_to_fit_under_bridge;
        if obj.speed > 0 {
            let mut loco = LocomotorState::from_object_type(obj);
            // TEMP: GI and Conscript move 6× faster for testing.
            if matches!(
                self.interner.resolve(ge.type_ref).to_uppercase().as_str(),
                "GI" | "CONS" | "E1" | "E2"
            ) {
                loco.speed_multiplier = SimFixed::from_num(6);
            }
            ge.locomotor = Some(loco);
        }
        // Aircraft ammo: set up ammo tracking for aircraft with finite Ammo=.
        if obj.ammo >= 0 && category == EntityCategory::Aircraft {
            ge.aircraft_ammo = Some(crate::sim::docking::aircraft_dock::AircraftAmmo::new(
                obj.ammo,
            ));
        }
        // Initialize aircraft mission for Fly-locomotor aircraft.
        if ge
            .locomotor
            .as_ref()
            .is_some_and(|l| l.kind == crate::rules::locomotor_type::LocomotorKind::Fly)
        {
            ge.aircraft_mission = Some(crate::sim::aircraft::AircraftMission::Idle);
        }

        if let Some(kind) = miner_kind_for_object(obj) {
            let mcfg: MinerConfig = MinerConfig::from_general_rules(&rules.general);
            ge.miner = Some(Miner::new(kind, &mcfg));
            ge.harvest_overlay = Some(HarvestOverlay {
                frame: 0,
                visible: false,
                elapsed_ms: 0,
            });
        }
        // Passenger cargo for transports and garrisonable buildings.
        if obj.passengers > 0 {
            ge.passenger_role = crate::sim::passenger::PassengerRole::Transport {
                cargo: crate::sim::passenger::PassengerCargo::new(obj.passengers, obj.size_limit),
            };
        } else if obj.can_be_occupied && obj.max_number_occupants > 0 {
            ge.passenger_role = crate::sim::passenger::PassengerRole::Transport {
                cargo: crate::sim::passenger::PassengerCargo::new(obj.max_number_occupants, 1),
            };
        }

        let spawn_owner_str = self.interner.resolve(ge.owner).to_string();
        let spawn_category = ge.category;
        self.entities.insert(ge);
        self.increment_owned_count(&spawn_owner_str, spawn_category);
        Some(stable_id)
    }

    /// Update VoxelAnimation frame_counts for all voxel entities from atlas data.
    ///
    /// Called after the unit atlas is built, since frame counts are only known after
    /// loading HVA files.
    pub fn update_voxel_anim_frame_counts(
        &mut self,
        frame_counts: &std::collections::BTreeMap<(String, crate::sim::components::VxlLayer), u32>,
    ) {
        use crate::sim::components::VxlLayer;

        let keys = self.entities.keys_sorted();
        let mut updated: u32 = 0;
        for &sid in &keys {
            let Some(entity) = self.entities.get_mut(sid) else {
                continue;
            };
            let Some(ref mut va) = entity.voxel_animation else {
                continue;
            };
            let max_fc: u32 = [
                VxlLayer::Composite,
                VxlLayer::Body,
                VxlLayer::Turret,
                VxlLayer::Barrel,
            ]
            .iter()
            .filter_map(|layer| frame_counts.get(&(self.interner.resolve(entity.type_ref).to_string(), *layer)))
            .copied()
            .max()
            .unwrap_or(1);

            if max_fc > 1 && va.frame_count != max_fc {
                va.frame_count = max_fc;
                updated += 1;
            }
        }
        if updated > 0 {
            log::info!(
                "Updated VoxelAnimation frame_count for {} entities",
                updated
            );
        }
    }

    /// Deploy an MCV entity: despawn it and spawn a construction yard in its place.
    /// Checks that the footprint area is free of other structures and passable terrain
    /// before deploying. Returns false if deployment is blocked.
    pub(crate) fn deploy_mcv(
        &mut self,
        stable_id: u64,
        rules: &RuleSet,
        height_map: &BTreeMap<(u16, u16), u8>,
    ) -> bool {
        // Read deploy data from EntityStore before mutating.
        let deploy_data = self.entities.get(stable_id).and_then(|entity| {
            let type_str = self.interner.resolve(entity.type_ref);
            let yard_type = construction_yard_type_for_mcv(type_str, rules)?;
            let yard_obj = rules.object(&yard_type)?;
            let (spawn_rx, spawn_ry) = deploy_origin_from_center(
                entity.position.rx,
                entity.position.ry,
                &yard_obj.foundation,
            );
            Some((
                entity.owner,
                spawn_rx,
                spawn_ry,
                entity.position.z,
                yard_type.clone(),
                entity.selected,
                yard_obj.foundation.clone(),
            ))
        });
        let Some((owner_id, rx, ry, z, yard_type, was_selected, foundation)) =
            deploy_data
        else {
            return false;
        };

        // Check that all footprint cells are free before deploying.
        let (fw, fh) = foundation_dimensions(&foundation);
        let ref_height: u8 = height_map.get(&(rx, ry)).copied().unwrap_or(z);
        for dy in 0..fw {
            for dx in 0..fh {
                let cell_x = rx.saturating_add(dx);
                let cell_y = ry.saturating_add(dy);
                // Check for existing structures (excluding the MCV itself).
                let occupied = self.entities.values().any(|e| {
                    if e.stable_id == stable_id || e.category != EntityCategory::Structure {
                        return false;
                    }
                    let Some(existing) = rules.object(self.interner.resolve(e.type_ref)) else {
                        return false;
                    };
                    if existing.wall {
                        return false;
                    }
                    let (ew, eh) = foundation_dimensions(&existing.foundation);
                    cell_x >= e.position.rx
                        && cell_x < e.position.rx.saturating_add(ew)
                        && cell_y >= e.position.ry
                        && cell_y < e.position.ry.saturating_add(eh)
                });
                if occupied {
                    log::info!("MCV deploy blocked: structure at ({},{})", cell_x, cell_y,);
                    return false;
                }
                // Check terrain build-blocked.
                if self
                    .effective_build_blocked(cell_x, cell_y)
                    .unwrap_or(false)
                {
                    log::info!("MCV deploy blocked: terrain at ({},{})", cell_x, cell_y,);
                    return false;
                }
                // Check same height (buildings can't span elevation changes).
                if height_map.get(&(cell_x, cell_y)).copied().unwrap_or(z) != ref_height {
                    log::info!(
                        "MCV deploy blocked: height mismatch at ({},{})",
                        cell_x,
                        cell_y,
                    );
                    return false;
                }
            }
        }

        // Despawn the MCV.
        self.despawn_entity(stable_id);

        // Spawn the construction yard.
        let owner_str = self.interner.resolve(owner_id).to_string();
        let Some(new_sid) =
            self.spawn_object_at_height(&yard_type, &owner_str, rx, ry, 0, z, rules)
        else {
            return false;
        };

        // Set selected and building-up state on the new entity.
        if let Some(ge) = self.entities.get_mut(new_sid) {
            ge.selected = was_selected;
            ge.building_up = Some(BuildingUp {
                elapsed_ticks: 0,
                total_ticks: 30,
            });
        }

        true
    }

    /// Undeploy a structure back into its mobile unit (e.g. ConYard → MCV).
    /// Reads `UndeploysInto` from rules.ini to determine the spawned unit type.
    /// Starts a reverse build-up animation (`BuildingDown`); the actual unit
    /// spawn happens when the animation completes (see `tick_building_down`).
    pub(crate) fn undeploy_building(&mut self, stable_id: u64, rules: &RuleSet) -> bool {
        // Read undeploy data before mutating.
        let undeploy_data = self.entities.get(stable_id).and_then(|entity| {
            if entity.category != EntityCategory::Structure {
                return None;
            }
            // Can't undeploy while still constructing or already undeploying.
            if entity.building_up.is_some() || entity.building_down.is_some() {
                return None;
            }
            let type_str = self.interner.resolve(entity.type_ref);
            let unit_type = undeploy_target_for_building(type_str, rules)?;
            let obj = rules.object(type_str)?;
            let (center_rx, center_ry) =
                undeploy_center_cell(entity.position.rx, entity.position.ry, &obj.foundation);
            Some((
                entity.owner,
                center_rx,
                center_ry,
                entity.position.z,
                unit_type,
                entity.selected,
            ))
        });
        let Some((owner_id, rx, ry, z, unit_type, was_selected)) = undeploy_data else {
            return false;
        };

        // Start the reverse build-up animation instead of instant despawn.
        let unit_type_id = self.interner.intern(&unit_type);
        if let Some(ge) = self.entities.get_mut(stable_id) {
            ge.building_down = Some(BuildingDown {
                elapsed_ticks: 0,
                total_ticks: 30,
                spawn_type: unit_type_id,
                spawn_owner: owner_id,
                spawn_rx: rx,
                spawn_ry: ry,
                spawn_z: z,
                was_selected,
            });
        }
        true
    }

    /// Find the next available infantry sub-cell at a given cell position.
    /// Scans existing infantry entities at (rx, ry) and returns the first unused
    /// spot from FUNCTIONAL_SUB_CELLS. Falls back to the first entry if all taken
    /// (caller should have avoided full cells via spawn cell selection).
    fn allocate_infantry_sub_cell(&self, rx: u16, ry: u16) -> u8 {
        let mut occupied: [bool; 5] = [false; 5];
        for entity in self.entities.values() {
            if entity.position.rx == rx
                && entity.position.ry == ry
                && entity.category == EntityCategory::Infantry
            {
                if let Some(sub) = entity.sub_cell {
                    if (sub as usize) < occupied.len() {
                        occupied[sub as usize] = true;
                    }
                }
            }
        }
        for &spot in &crate::sim::movement::bump_crush::FUNCTIONAL_SUB_CELLS {
            if !occupied[spot as usize] {
                return spot;
            }
        }
        crate::sim::movement::bump_crush::FUNCTIONAL_SUB_CELLS[0]
    }
}

fn deploy_origin_from_center(center_rx: u16, center_ry: u16, foundation: &str) -> (u16, u16) {
    let (width, height) = foundation_dimensions(foundation);
    (
        center_rx.saturating_sub(width / 2),
        center_ry.saturating_sub(height / 2),
    )
}

/// Resolve the deploy target for an MCV-like unit via rules.ini `DeploysInto=`.
fn construction_yard_type_for_mcv(type_id: &str, rules: &RuleSet) -> Option<String> {
    let obj = rules.object(type_id)?;
    let target: &str = obj.deploys_into.as_deref()?;
    rules.object(target)?;
    Some(target.to_string())
}

/// Resolve the undeploy target for a building via rules.ini `UndeploysInto=`.
fn undeploy_target_for_building(type_id: &str, rules: &RuleSet) -> Option<String> {
    let obj = rules.object(type_id)?;
    let target: &str = obj.undeploys_into.as_deref()?;
    rules.object(target)?;
    Some(target.to_string())
}

/// Compute the center cell of a foundation for MCV spawn during undeploy.
/// Reverse of `deploy_origin_from_center`: origin + half_size = center.
fn undeploy_center_cell(origin_rx: u16, origin_ry: u16, foundation: &str) -> (u16, u16) {
    let (width, height) = foundation_dimensions(foundation);
    (origin_rx + width / 2, origin_ry + height / 2)
}
