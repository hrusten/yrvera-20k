//! Combat system — attack targeting, weapon firing, and damage application.
//!
//! Handles the combat loop: units with an AttackTarget component fire their
//! primary weapon at the target each tick (respecting ROF cooldown). Damage
//! is computed from weapon damage * warhead verses[armor_index]. Entities
//! at 0 health are despawned.
//!
//! ## RA2 damage formula
//! `actual_damage = weapon.damage * warhead.verses[armor_index]`
//! where armor_index is looked up from the target's Armor string.
//!
//! ## Rate of fire
//! ROF in rules.ini is measured in game frames (at 15 fps in original RA2).
//! We convert to simulation ticks using integer math.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/components and rules/ (RuleSet).
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

pub(crate) mod cell_spread;
pub(crate) mod combat_aoe;
mod combat_fire_gate;
pub(crate) mod combat_targeting;
pub(crate) mod combat_weapon;

#[cfg(test)]
#[path = "combat_tests.rs"]
mod combat_tests;

use std::collections::BTreeMap;

use crate::sim::miner::ResourceNode;

use self::combat_weapon::select_weapon_with_ifv;
use crate::map::entities::EntityCategory;
use crate::rules::object_type::ObjectType;
use crate::rules::ruleset::RuleSet;
use crate::rules::warhead_type::WarheadType;
use crate::sim::animation::animation_is_prone;
use crate::sim::bridge_state::BridgeDamageEvent;
use crate::sim::entity_store::EntityStore;
use crate::sim::intern::{InternedId, StringInterner};
use crate::sim::passenger::PassengerRole;
use crate::sim::power_system::PowerState;
use crate::sim::vision::FogState;
use crate::sim::world::{SimFireEvent, SimSoundEvent};
use crate::util::fixed_math::{SIM_ZERO, SimFixed, sim_to_i32};

use super::game_entity::GameEntity;
use super::occupancy::OccupancyGrid;
use super::production::foundation_dimensions;

/// RA2 runs at 15 logical frames per second. ROF values are in frames.
const GAME_FPS: u32 = 15;
/// Radius in cells that RevealOnFire clears shroud around the fire location.
const REVEAL_ON_FIRE_RADIUS: u16 = 3;

/// A cell area to reveal due to a RevealOnFire weapon firing.
pub struct RevealEvent {
    pub owner: InternedId,
    pub rx: u16,
    pub ry: u16,
    pub radius: u16,
}

/// Armor type name → Verses index mapping.
/// Matches the order defined in warhead_type.rs: none(0), flak(1), plate(2),
/// light(3), medium(4), heavy(5), wood(6), steel(7), concrete(8),
/// special_1(9), special_2(10).
const ARMOR_NAMES: &[&str] = &[
    "none",
    "flak",
    "plate",
    "light",
    "medium",
    "heavy",
    "wood",
    "steel",
    "concrete",
    "special_1",
    "special_2",
];

/// Look up the Verses array index for an armor type name.
/// Returns 0 ("none") for unrecognized armor strings.
/// Used by combat_weapon.rs for weapon selection.
pub fn armor_index(armor: &str) -> usize {
    let lower: String = armor.to_ascii_lowercase();
    ARMOR_NAMES.iter().position(|&a| a == lower).unwrap_or(0)
}

pub(crate) fn apply_prone_damage_modifier(
    target_prone_infantry: bool,
    warhead: &WarheadType,
    damage: i32,
) -> u16 {
    if damage <= 0 {
        return 0;
    }

    let scaled = if target_prone_infantry {
        (damage as u64 * warhead.prone_damage_basis_points as u64 / 10_000) as i32
    } else {
        damage
    };

    scaled.clamp(0, u16::MAX as i32) as u16
}

/// Component: this entity is attacking a specific target entity.
///
/// Attached by `issue_attack_command()`. The combat system fires the
/// attacker's weapon at the target each tick. Supports burst firing:
/// multiple rapid shots per attack cycle, with ROF cooldown only after
/// the full burst completes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AttackTarget {
    /// Stable entity ID of the target being attacked.
    pub target: u64,
    /// Simulation ticks remaining before the next shot (ROF cooldown).
    pub cooldown_ticks: u16,
    /// Shots remaining in the current burst. When this reaches 0, ROF cooldown starts.
    pub burst_remaining: u8,
    /// Ticks between individual burst shots (short inter-shot delay).
    pub burst_delay_ticks: u8,
}

/// Delay in simulation ticks between individual shots within a burst.
/// 1 game frame (~66ms) — fast but visible.
const BURST_INTER_SHOT_DELAY: u8 = 1;

impl AttackTarget {
    /// Create a new AttackTarget with zero cooldown and no burst state.
    pub fn new(target_stable_id: u64) -> Self {
        Self {
            target: target_stable_id,
            cooldown_ticks: 0,
            burst_remaining: 0,
            burst_delay_ticks: 0,
        }
    }
}

/// Compute the effective target coordinates for an entity.
///
/// For structures, returns the **foundation center** instead of the raw
/// position (NW corner cell center):
///   X = Location.X + (foundationWidth  - 1) * 128
///   Y = Location.Y + (foundationHeight - 1) * 128
///
/// The vanilla game has bugs where some code paths use raw Location (NW corner)
/// instead of foundation center — e.g. Destroyers mis-targeting Naval Yards
/// from certain angles (Phobos bugfix at 0x70BCE6). We fix this from the start.
fn target_coords(
    entity: &GameEntity,
    rules: Option<&RuleSet>,
    interner: &StringInterner,
) -> (u16, u16, SimFixed, SimFixed) {
    let mut rx = entity.position.rx;
    let mut ry = entity.position.ry;
    let mut sub_x = entity.position.sub_x;
    let mut sub_y = entity.position.sub_y;

    if entity.category == EntityCategory::Structure {
        if let Some(obj) = rules.and_then(|r| r.object(interner.resolve(entity.type_ref))) {
            let (fw, fh) = foundation_dimensions(&obj.foundation);
            // Shift from NW corner cell center to foundation geometric center.
            // (fw-1)*128 leptons in X, (fh-1)*128 leptons in Y.
            // sub_x/sub_y may exceed 256 — lepton_distance_sq_raw handles
            // this correctly since it computes cell*256+sub as a flat value.
            let offset_x = (fw.saturating_sub(1) as i32) * 128;
            let offset_y = (fh.saturating_sub(1) as i32) * 128;
            let full_x: i32 = rx as i32 * 256 + sub_x.to_num::<i32>() + offset_x;
            let full_y: i32 = ry as i32 * 256 + sub_y.to_num::<i32>() + offset_y;
            rx = (full_x / 256) as u16;
            ry = (full_y / 256) as u16;
            sub_x = SimFixed::from_num(full_x % 256);
            sub_y = SimFixed::from_num(full_y % 256);
        }
    }

    (rx, ry, sub_x, sub_y)
}

/// Issue an attack command: make `attacker` fire at `target`.
///
/// Replaces any existing AttackTarget on the attacker. Also updates the
/// attacker's facing to point toward the target.
pub fn issue_attack_command(
    entities: &mut EntityStore,
    attacker_id: u64,
    target_id: u64,
    rules: Option<&RuleSet>,
    interner: &StringInterner,
) -> bool {
    // Read target position first (immutable borrow, lepton-precise).
    // Use foundation center for buildings (see target_coords doc comment).
    let target_pos = entities
        .get(target_id)
        .map(|t| target_coords(t, rules, interner));
    let (trx, try_, tsx, tsy) = match target_pos {
        Some(p) => p,
        None => return false,
    };

    // Read attacker position before mutable borrow (needed for lepton facing).
    let attacker_pos = entities.get(attacker_id).map(|a| {
        (
            a.position.rx,
            a.position.ry,
            a.position.sub_x,
            a.position.sub_y,
            a.turret_facing.is_some(),
        )
    });
    let (arx, ary, asx, asy, has_turret) = match attacker_pos {
        Some(p) => p,
        None => return false,
    };

    // Mutate attacker.
    let attacker = match entities.get_mut(attacker_id) {
        Some(a) => a,
        None => return false,
    };

    // Update facing toward target (lepton-precise for turrets, cell-level for body).
    if has_turret {
        let desired_u16 = crate::sim::movement::turret::facing_toward_lepton(
            arx, ary, asx, asy, trx, try_, tsx, tsy,
        );
        attacker.turret_facing = Some(desired_u16);
    } else {
        let dx: i32 = trx as i32 - arx as i32;
        let dy: i32 = try_ as i32 - ary as i32;
        attacker.facing = crate::sim::movement::facing_from_delta(dx, dy);
    }

    // Remove existing movement (stop moving to attack).
    attacker.movement_target = None;

    // Attach the attack target using stable ID (fire immediately).
    attacker.attack_target = Some(AttackTarget::new(target_id));

    true
}

/// Compute distance in cells between two entities' grid positions.
#[cfg(test)]
pub(crate) fn cell_distance(ax: u16, ay: u16, bx: u16, by: u16) -> f32 {
    let dx: f32 = ax as f32 - bx as f32;
    let dy: f32 = ay as f32 - by as f32;
    (dx * dx + dy * dy).sqrt()
}

use self::combat_targeting::{AttackerSnapshot, GarrisonSnapshot, acquire_best_target};

/// Advance combat for all entities with AttackTarget components.
pub fn tick_combat(
    entities: &mut EntityStore,
    occupancy: &mut OccupancyGrid,
    rules: &RuleSet,
    interner: &mut StringInterner,
    resource_nodes: &mut BTreeMap<(u16, u16), ResourceNode>,
    tick_ms: u32,
) -> CombatTickResult {
    tick_combat_with_fog(
        entities,
        occupancy,
        rules,
        interner,
        None,
        &BTreeMap::new(),
        None,
        resource_nodes,
        tick_ms,
    )
}

/// Destroyed crewed building — survivor ejection is deferred to the caller
/// (which has access to `Simulation` for spawning infantry).
pub struct DestroyedCrewedBuilding {
    pub type_id: InternedId,
    pub owner: InternedId,
    pub rx: u16,
    pub ry: u16,
    pub z: u8,
}

/// Explosion animation to spawn at a world position (deferred to caller
/// which has access to `Simulation` for WorldEffect spawning).
pub struct ExplosionEffect {
    pub shp_name: InternedId,
    pub rx: u16,
    pub ry: u16,
    pub z: u8,
}

/// Result of a combat tick: reveal events + stable IDs of despawned entities.
pub struct CombatTickResult {
    pub reveal_events: Vec<RevealEvent>,
    pub despawned_ids: Vec<u64>,
    /// A structure was destroyed — PathGrid needs footprint unblock.
    pub structure_destroyed: bool,
    /// Owners who lost their last SpySat building — need full reshroud.
    pub spy_sat_reshroud_owners: Vec<InternedId>,
    /// Bridge impact cells that should apply terrain damage after combat resolution.
    pub bridge_damage_events: Vec<BridgeDamageEvent>,
    /// Fire events for render-side muzzle flash / projectile origin computation.
    pub fire_events: Vec<SimFireEvent>,
    /// Crewed buildings destroyed this tick — survivors should be ejected by the caller.
    pub destroyed_crewed_buildings: Vec<DestroyedCrewedBuilding>,
    /// Explosion animations to spawn at death/impact locations.
    pub explosion_effects: Vec<ExplosionEffect>,
}

/// Look up death weapon AoE data from an ObjectType.
/// Returns (damage, warhead_id) if the entity should deal AoE damage on death.
/// Checks DeathWeapon first, then falls back to primary weapon if Explodes=yes.
fn death_weapon_aoe(
    rules: &RuleSet,
    obj: &ObjectType,
    interner: &mut StringInterner,
) -> Option<(i32, InternedId)> {
    if let Some(ref dw_id) = obj.death_weapon {
        let dw = rules.weapon(dw_id)?;
        let wh_id = dw.warhead.as_ref()?;
        return Some((dw.damage, interner.intern(wh_id)));
    }
    if obj.explodes {
        let pri = rules.weapon(obj.primary.as_ref()?)?;
        let wh_id = pri.warhead.as_ref()?;
        return Some((pri.damage, interner.intern(wh_id)));
    }
    None
}

/// Collected side-effects from processing entity deaths in a single tick.
struct DeathEffects {
    despawned_ids: Vec<u64>,
    structure_destroyed: bool,
    spy_sat_reshroud_owners: Vec<InternedId>,
    destroyed_crewed_buildings: Vec<DestroyedCrewedBuilding>,
    explosion_effects: Vec<ExplosionEffect>,
    bridge_damage_events: Vec<BridgeDamageEvent>,
    death_sounds: Vec<(InternedId, u16, u16)>,
}

/// Process all entity deaths for this tick: death weapons, passengers, explosions, despawn.
///
/// Extracts death side-effects into a `DeathEffects` struct so the caller can apply them
/// (bridge damage, sound events, etc.) without the combat function growing unbounded.
fn handle_entity_deaths(
    entities: &mut EntityStore,
    occupancy: &mut OccupancyGrid,
    rules: &RuleSet,
    interner: &mut StringInterner,
    dead_entities: &[u64],
    damage_events: &[(u64, u16, u64, InternedId)],
    resource_nodes: &mut BTreeMap<(u16, u16), ResourceNode>,
) -> DeathEffects {
    let mut death_sounds: Vec<(InternedId, u16, u16)> = Vec::new();
    let mut death_aoe: Vec<(u16, u16, i32, InternedId, InternedId)> = Vec::new();
    let mut despawned_ids: Vec<u64> = Vec::new();
    let mut spy_sat_reshroud_owners: Vec<InternedId> = Vec::new();
    let mut destroyed_crewed_buildings: Vec<DestroyedCrewedBuilding> = Vec::new();
    let mut explosion_effects: Vec<ExplosionEffect> = Vec::new();
    let mut bridge_damage_events: Vec<BridgeDamageEvent> = Vec::new();
    let mut structure_destroyed: bool = false;
    // Step size for selecting explosion anim from AnimList (damage / 25).
    const ANIM_LIST_DAMAGE_STEP: u16 = 25;
    for &dead_id in dead_entities {
        let dead_info = entities.get(dead_id).map(|e| {
            if e.category == EntityCategory::Structure {
                structure_destroyed = true;
            }
            (
                e.type_ref,
                e.position.rx,
                e.position.ry,
                e.position.z,
                e.owner,
                e.animation.is_some(),
                e.category,
            )
        });

        if let Some((type_id, rx, ry, z, owner, has_animation, category)) = dead_info {
            let type_id_str = interner.resolve(type_id);
            if let Some(obj) = rules.object(type_id_str) {
                if let Some(ref die_sound) = obj.die_sound {
                    death_sounds.push((interner.intern(die_sound), rx, ry));
                }
                if let Some((dmg, wh_id)) = death_weapon_aoe(rules, obj, interner) {
                    death_aoe.push((rx, ry, dmg, wh_id, owner));
                }
                if obj.spy_sat {
                    spy_sat_reshroud_owners.push(owner);
                }
                // Crewed structures eject infantry survivors on destruction.
                if obj.crewed && category == EntityCategory::Structure {
                    destroyed_crewed_buildings.push(DestroyedCrewedBuilding {
                        type_id: type_id,
                        owner: owner,
                        rx,
                        ry,
                        z,
                    });
                }
            }

            // Kill all passengers inside a destroyed transport/garrison.
            let passenger_ids: Vec<u64> = entities
                .get(dead_id)
                .and_then(|e| e.passenger_role.cargo())
                .map(|c| c.passengers.clone())
                .unwrap_or_default();
            for &pid in &passenger_ids {
                if let Some(pax) = entities.get_mut(pid) {
                    pax.health.current = 0;
                    pax.dying = true;
                    pax.passenger_role = PassengerRole::None;
                    pax.attack_target = None;
                    pax.movement_target = None;
                    pax.selected = false;
                }
            }

            // Look up the warhead that dealt the killing blow for explosion selection.
            let killing_warhead = damage_events
                .iter()
                .rfind(|(tid, _, _, _)| *tid == dead_id)
                .and_then(|(_, dmg, _, wh_id)| {
                    rules.warhead(interner.resolve(*wh_id)).map(|wh| (wh, *dmg))
                });

            // Spawn explosion animation from the warhead's AnimList.
            if let Some((wh, dmg)) = &killing_warhead {
                if !wh.anim_list.is_empty() {
                    let idx = (*dmg / ANIM_LIST_DAMAGE_STEP) as usize;
                    let idx = idx.min(wh.anim_list.len() - 1);
                    explosion_effects.push(ExplosionEffect {
                        shp_name: interner.intern(&wh.anim_list[idx]),
                        rx,
                        ry,
                        z,
                    });
                }
            }

            clear_targets_on_dead_entity(entities, dead_id);

            if has_animation {
                // Infantry/SHP units: mark dying, trigger death animation.
                // The animation system will despawn when the death anim finishes.
                // Select InfDeath variant from the killing warhead (default Die1).
                let inf_death: u8 = killing_warhead
                    .as_ref()
                    .map(|(wh, _)| wh.inf_death)
                    .unwrap_or(1);
                if let Some(entity) = entities.get_mut(dead_id) {
                    entity.dying = true;
                    entity.attack_target = None;
                    entity.movement_target = None;
                    entity.selected = false;
                    if let Some(ref mut anim) = entity.animation {
                        use crate::sim::animation::death_sequence_for_inf_death;
                        anim.switch_to(death_sequence_for_inf_death(inf_death));
                    }
                }
                // Still report as "despawned" for fog/path updates — entity is
                // functionally dead even though the sprite lingers for the animation.
                despawned_ids.push(dead_id);
                log::trace!("Entity {} dying (death animation)", dead_id);
            } else {
                // Structures and voxel vehicles: immediate despawn.
                // Remove from occupancy before entity is gone.
                if let Some(entity) = entities.get(dead_id) {
                    occupancy.remove(entity.position.rx, entity.position.ry, dead_id);
                }
                entities.remove(dead_id);
                despawned_ids.push(dead_id);
                log::trace!("Entity {} destroyed", dead_id);
            }
        }
    }

    // Apply death explosion AoE damage.
    for (rx, ry, dmg, wh_id, owner_id) in &death_aoe {
        if let Some(warhead) = rules.warhead(interner.resolve(*wh_id)) {
            if warhead.wall && *dmg > 0 {
                // TODO(RE): Low-bridge overlay damage is not a raw "damage bridge cell" event.
                // The recovered engine uses a BridgeStrength RNG gate, AtomDamage bypass,
                // and 3-cell overlay pattern transitions before the pathfinding side-effects.
                // Keep these events scoped to the current elevated-bridge runtime until
                // mutable overlay bridge state is wired through the sim.
                bridge_damage_events.push(BridgeDamageEvent {
                    rx: *rx,
                    ry: *ry,
                    damage: (*dmg).max(0) as u16,
                });
            }
            let aoe_hits = self::combat_aoe::apply_aoe_damage(
                entities,
                *rx,
                *ry,
                *dmg,
                warhead,
                rules,
                interner,
                interner.resolve(*owner_id),
            );
            for (target_id, aoe_dmg) in aoe_hits {
                if let Some(target) = entities.get_mut(target_id) {
                    target.health.current = target.health.current.saturating_sub(aoe_dmg);
                }
            }
            // Ore destruction from death explosion.
            destroy_ore_at_impact(resource_nodes, *rx, *ry, *dmg, warhead.cell_spread);
        }
    }

    DeathEffects {
        despawned_ids,
        structure_destroyed,
        spy_sat_reshroud_owners,
        destroyed_crewed_buildings,
        explosion_effects,
        bridge_damage_events,
        death_sounds,
    }
}

/// Remove AttackTarget from any entity currently targeting the dead entity.
fn clear_targets_on_dead_entity(entities: &mut EntityStore, dead_id: u64) {
    let keys: Vec<u64> = entities.keys_sorted();
    for &eid in &keys {
        if let Some(entity) = entities.get_mut(eid) {
            if entity
                .attack_target
                .as_ref()
                .is_some_and(|a| a.target == dead_id)
            {
                entity.attack_target = None;
            }
        }
    }
}

/// Destroy ore/gem resources at cells affected by a warhead detonation.
///
/// Iterates cells in the warhead's CellSpread radius and reduces ore density
/// by `base_damage / 10` at each cell. Matches gamemd's `Apply_area_damage`
/// ore destruction logic (0x00489280).
///
/// ALL warheads destroy ore unconditionally — the `Tiberium=` INI flag only
/// gates vein destruction (not implemented).
fn destroy_ore_at_impact(
    resource_nodes: &mut BTreeMap<(u16, u16), ResourceNode>,
    impact_rx: u16,
    impact_ry: u16,
    base_damage: i32,
    cell_spread: SimFixed,
) {
    let ore_damage = (base_damage / 10).max(0) as u16;
    if ore_damage == 0 {
        return;
    }
    let spread_radius = cell_spread.to_num::<u32>();
    for &(dx, dy) in self::cell_spread::cells_in_spread(spread_radius) {
        let cx = impact_rx as i32 + dx as i32;
        let cy = impact_ry as i32 + dy as i32;
        if cx >= 0 && cy >= 0 {
            crate::sim::miner::reduce_tiberium(resource_nodes, (cx as u16, cy as u16), ore_damage);
        }
    }
}

/// Advance combat with optional owner visibility gating and sound event sink.
/// Returns reveal events and stable IDs of entities despawned this tick.
pub fn tick_combat_with_fog(
    entities: &mut EntityStore,
    occupancy: &mut OccupancyGrid,
    rules: &RuleSet,
    interner: &mut StringInterner,
    fog: Option<&FogState>,
    power_states: &BTreeMap<InternedId, PowerState>,
    sound_sink: Option<&mut Vec<SimSoundEvent>>,
    resource_nodes: &mut BTreeMap<(u16, u16), ResourceNode>,
    tick_ms: u32,
) -> CombatTickResult {
    if tick_ms == 0 {
        return CombatTickResult {
            reveal_events: Vec::new(),
            despawned_ids: Vec::new(),
            structure_destroyed: false,
            spy_sat_reshroud_owners: Vec::new(),
            bridge_damage_events: Vec::new(),
            fire_events: Vec::new(),
            destroyed_crewed_buildings: Vec::new(),
            explosion_effects: Vec::new(),
        };
    }
    // Pre-scan: collect entities blocked from firing by locomotor or power state.
    let fire_blocked = combat_fire_gate::collect_fire_blocked_entities(
        entities,
        power_states,
        Some(rules),
        interner,
    );

    let keys: Vec<u64> = entities.keys_sorted();

    // Garrison auto-acquire: idle garrisoned buildings scan for hostile targets.
    // Runs before Phase 1 so newly-targeted buildings are included in snapshots.
    for &id in &keys {
        let (is_candidate, owner, pos_rx, pos_ry, sub_x, sub_y, type_id, _turret_facing) = {
            let entity = match entities.get(id) {
                Some(e) => e,
                None => continue,
            };
            if entity.category != EntityCategory::Structure
                || entity.attack_target.is_some()
                || entity.dying
                || !entity.is_alive()
                || fire_blocked.contains(&id)
            {
                continue;
            }
            (
                true,
                entity.owner,
                entity.position.rx,
                entity.position.ry,
                entity.position.sub_x,
                entity.position.sub_y,
                entity.type_ref,
                entity.turret_facing,
            )
        };
        if !is_candidate {
            continue;
        }

        let obj = match rules.object(interner.resolve(type_id)) {
            Some(o) => o,
            None => continue,
        };
        if !obj.can_be_occupied || !obj.can_occupy_fire {
            continue;
        }

        // Read cargo info (immutable borrow).
        let (occ_id, half_foundation) = {
            let entity = match entities.get(id) {
                Some(e) => e,
                None => continue,
            };
            let cargo = match entity.passenger_role.cargo() {
                Some(c) if !c.is_empty() => c,
                _ => continue,
            };
            let fi = cargo.garrison_fire_index as usize % cargo.count() as usize;
            let occ_id = cargo.passengers[fi];
            let (fw, fh) = foundation_dimensions(&obj.foundation);
            (occ_id, fw.min(fh) / 2)
        };

        // Resolve occupant type + veterancy for garrison weapon validation.
        let (occ_type, occ_vet) = match entities.get(occ_id) {
            Some(occ) => (occ.type_ref, occ.veterancy),
            None => continue,
        };

        // Scan range = half_foundation + 1 + OccupyWeaponRange (gamemd Greatest_Threat).
        let scan_cells = half_foundation as i32 + 1 + rules.garrison_rules.occupy_weapon_range;
        let scan_range = SimFixed::from_num(scan_cells.max(1));

        // Scan for best hostile target using garrison weapon for Verses/projectile checks.
        // gamemd's Greatest_Threat calls GetWeapon on the building, which returns
        // the occupant's OccupyWeapon — not the occupant's primary weapon.
        let mut best_target: Option<(i64, u8, u64)> = None;
        let owner_str = interner.resolve(owner);
        for candidate in entities.values() {
            if candidate.stable_id == id
                || candidate.health.current == 0
                || candidate.dying
                || candidate.passenger_role.is_inside_transport()
            {
                continue;
            }
            if candidate.owner == owner {
                continue;
            }
            if let Some(fog_state) = fog {
                let candidate_owner_str = interner.resolve(candidate.owner);
                if fog_state.is_friendly(owner_str, candidate_owner_str) {
                    continue;
                }
                if !fog_state.is_cell_visible(owner, candidate.position.rx, candidate.position.ry) {
                    continue;
                }
            }
            let target_cat = candidate.category;
            let target_armor = rules
                .object(interner.resolve(candidate.type_ref))
                .map(|o| o.armor.as_str())
                .unwrap_or("none");
            // Use garrison weapon (OccupyWeapon) for target compatibility check.
            let occ_type_str = interner.resolve(occ_type);
            let selected = match combat_weapon::select_garrison_weapon(
                rules,
                occ_type_str,
                occ_vet,
                target_cat,
                target_armor,
            ) {
                Some(s) => s,
                None => continue,
            };
            if combat_weapon::verses_gate(selected.verses_pct)
                == combat_weapon::VersesGate::Suppressed
            {
                continue;
            }
            let dist_sq = lepton_distance_sq_raw(
                pos_rx,
                pos_ry,
                sub_x,
                sub_y,
                candidate.position.rx,
                candidate.position.ry,
                candidate.position.sub_x,
                candidate.position.sub_y,
            );
            if !is_within_range_leptons(dist_sq, scan_range) {
                continue;
            }
            let class = match rules.object(interner.resolve(candidate.type_ref)) {
                Some(o) if o.primary.is_some() => 0u8,
                _ => 1,
            };
            let rank = (dist_sq, class, candidate.stable_id);
            match best_target {
                Some(current) if rank >= current => {}
                _ => best_target = Some(rank),
            }
        }

        if let Some((_, _, target_id)) = best_target {
            if let Some(building) = entities.get_mut(id) {
                building.attack_target = Some(AttackTarget::new(target_id));
            }
        }
    }

    // Phase 1: snapshot all attackers and advance cooldowns / burst delays.
    let mut snapshots: Vec<AttackerSnapshot> = Vec::new();
    for &id in &keys {
        // Mutable borrow: tick cooldowns and extract entity data + garrison cargo info.
        let (snap_base, garrison_cargo) = {
            let entity = match entities.get_mut(id) {
                Some(e) => e,
                None => continue,
            };
            // Skip entities inside a transport — they can't fire (unless OpenTopped, deferred).
            if entity.passenger_role.is_inside_transport() {
                continue;
            }
            let attack = match entity.attack_target.as_mut() {
                Some(a) => a,
                None => continue,
            };
            attack.cooldown_ticks = attack.cooldown_ticks.saturating_sub(1);
            attack.burst_delay_ticks = attack.burst_delay_ticks.saturating_sub(1);
            // Skip snapshot for entities blocked by locomotor state (cooldowns still tick).
            if fire_blocked.contains(&id) {
                continue;
            }

            // Extract garrison cargo info while we have the entity.
            let garrison_cargo: Option<(u8, u8, u64)> =
                if entity.category == EntityCategory::Structure {
                    entity.passenger_role.cargo().and_then(|c| {
                        if c.is_empty() {
                            return None;
                        }
                        let fi = c.garrison_fire_index;
                        let count = c.count() as u8;
                        let oi = fi as usize % count as usize;
                        Some((fi, count, c.passengers[oi]))
                    })
                } else {
                    None
                };

            let base = (
                entity.stable_id,
                entity.owner,
                attack.target,
                entity.position.rx,
                entity.position.ry,
                entity.position.sub_x,
                entity.position.sub_y,
                entity.type_ref,
                attack.cooldown_ticks,
                entity.turret_facing,
                attack.burst_remaining,
                attack.burst_delay_ticks,
                entity.ifv_weapon_index,
            );
            (base, garrison_cargo)
        }; // mutable borrow released

        let (
            stable_id,
            owner,
            target,
            pos_rx,
            pos_ry,
            sub_x,
            sub_y,
            type_id,
            cooldown_ticks,
            turret_facing,
            burst_remaining,
            burst_delay_ticks,
            ifv_weapon_index,
        ) = snap_base;

        // Resolve garrison snapshot from cargo info (read-only borrow).
        let garrison = garrison_cargo.and_then(|(fire_idx, count, occ_id)| {
            let obj = rules.object(interner.resolve(type_id))?;
            if !obj.can_be_occupied || !obj.can_occupy_fire {
                return None;
            }
            let occ = entities.get(occ_id)?;
            let (fw, fh) = foundation_dimensions(&obj.foundation);
            Some(GarrisonSnapshot {
                occupant_type_id: occ.type_ref,
                occupant_veterancy: occ.veterancy,
                fire_index: fire_idx,
                occupant_count: count,
                half_foundation: fw.min(fh) / 2,
            })
        });

        snapshots.push(AttackerSnapshot {
            stable_id,
            owner,
            target,
            pos_rx,
            pos_ry,
            sub_x,
            sub_y,
            type_id,
            cooldown_ticks,
            turret_facing,
            burst_remaining,
            burst_delay_ticks,
            ifv_weapon_index,
            garrison,
        });
    }
    snapshots.sort_by_key(|s| s.stable_id);

    // Phase 2: process each attacker against its target.
    // (target_id, damage, attacker_id, warhead_id)
    let mut damage_events: Vec<(u64, u16, u64, InternedId)> = Vec::new();
    let mut remove_attack: Vec<u64> = Vec::new();
    let mut retarget_events: Vec<(u64, u64)> = Vec::new(); // (attacker_id, new_target_id)
    let mut fire_sounds: Vec<(InternedId, u16, u16)> = Vec::new();
    let mut fire_events: Vec<SimFireEvent> = Vec::new();
    let mut reveal_events: Vec<RevealEvent> = Vec::new();
    let mut bridge_damage_events: Vec<BridgeDamageEvent> = Vec::new();
    let mut burst_updates: Vec<(u64, u8, u8, u16)> = Vec::new(); // (id, burst_rem, burst_delay, rof_cd)
    let mut ammo_deduct: Vec<u64> = Vec::new(); // aircraft that fired this tick
    let mut garrison_advance: Vec<u64> = Vec::new(); // building IDs to advance fire index

    for snap in &snapshots {
        // Pre-compute garrison scan range for retargeting (includes +1 buffer).
        let garrison_retarget_range: Option<SimFixed> = snap.garrison.as_ref().map(|gs| {
            let cells = gs.half_foundation as i32 + 1 + rules.garrison_rules.occupy_weapon_range;
            SimFixed::from_num(cells.max(1))
        });
        let obj = match rules.object(interner.resolve(snap.type_id)) {
            Some(o) => o,
            None => {
                remove_attack.push(snap.stable_id);
                continue;
            }
        };

        // Check if target is alive and get its data.
        // For structures, target_coords returns the foundation center instead
        // of the NW corner.
        let target_data = entities.get(snap.target).map(|t| {
            let (trx, try_, tsx, tsy) = target_coords(t, Some(rules), interner);
            (
                trx,
                try_,
                tsx,
                tsy,
                t.health.current,
                t.category,
                t.type_ref,
                t.owner,
                // TODO(RE): This currently keys off prone animation sequences because
                // the runtime has no separate prone-state bit yet. Reverse engineer
                // and implement the real infantry prone-entry conditions so
                // ProneDamage applies during normal gameplay instead of only when
                // prone sequences are explicitly driven.
                t.category == EntityCategory::Infantry && animation_is_prone(t.animation.as_ref()),
            )
        });

        let (
            target_rx,
            target_ry,
            target_sub_x,
            target_sub_y,
            _target_hp,
            target_cat,
            target_type_ref,
            target_owner,
            target_prone_infantry,
        ) = match target_data {
            Some((rx, ry, sx, sy, hp, cat, tr, own, prone)) if hp > 0 => {
                (rx, ry, sx, sy, hp, cat, tr, own, prone)
            }
            _ => {
                if let Some(new_target) = acquire_best_target(
                    entities,
                    rules,
                    interner,
                    snap,
                    obj,
                    fog,
                    garrison_retarget_range,
                ) {
                    retarget_events.push((snap.stable_id, new_target));
                } else {
                    remove_attack.push(snap.stable_id);
                }
                continue;
            }
        };

        let target_armor: String = rules
            .object(interner.resolve(target_type_ref))
            .map(|o| o.armor.clone())
            .unwrap_or_else(|| "none".to_string());

        // Weapon selection: garrison uses occupant's OccupyWeapon, standard uses IFV/Primary/Secondary.
        let (selected, is_garrison) = if let Some(ref gs) = snap.garrison {
            match combat_weapon::select_garrison_weapon(
                rules,
                interner.resolve(gs.occupant_type_id),
                gs.occupant_veterancy,
                target_cat,
                &target_armor,
            ) {
                Some(s) => (s, true),
                None => {
                    remove_attack.push(snap.stable_id);
                    continue;
                }
            }
        } else {
            match select_weapon_with_ifv(
                rules,
                obj,
                target_cat,
                &target_armor,
                snap.ifv_weapon_index,
            ) {
                Some(s) => (s, false),
                None => {
                    remove_attack.push(snap.stable_id);
                    continue;
                }
            }
        };
        let weapon = selected.weapon;

        if let Some(fog_state) = fog {
            let snap_owner_str = interner.resolve(snap.owner);
            let target_owner_str = interner.resolve(target_owner);
            if fog_state.is_friendly(snap_owner_str, target_owner_str) {
                if let Some(new_target) = acquire_best_target(
                    entities,
                    rules,
                    interner,
                    snap,
                    obj,
                    fog,
                    garrison_retarget_range,
                ) {
                    retarget_events.push((snap.stable_id, new_target));
                } else {
                    remove_attack.push(snap.stable_id);
                }
                continue;
            }
            if !fog_state.is_cell_visible(snap.owner, target_rx, target_ry) {
                if let Some(new_target) = acquire_best_target(
                    entities,
                    rules,
                    interner,
                    snap,
                    obj,
                    fog,
                    garrison_retarget_range,
                ) {
                    retarget_events.push((snap.stable_id, new_target));
                } else {
                    remove_attack.push(snap.stable_id);
                }
                continue;
            }
        }

        // Range check (lepton-precise, sub-cell aware).
        let dist_sq = lepton_distance_sq_raw(
            snap.pos_rx,
            snap.pos_ry,
            snap.sub_x,
            snap.sub_y,
            target_rx,
            target_ry,
            target_sub_x,
            target_sub_y,
        );
        // Garrison range: (half_foundation + OccupyWeaponRange) cells (no +1 buffer for fire).
        let effective_range = if let Some(ref gs) = snap.garrison {
            let cells = gs.half_foundation as i32 + rules.garrison_rules.occupy_weapon_range;
            SimFixed::from_num(cells.max(1))
        } else {
            weapon.range
        };
        if !is_within_range_leptons(dist_sq, effective_range) {
            if let Some(new_target) = acquire_best_target(
                entities,
                rules,
                interner,
                snap,
                obj,
                fog,
                garrison_retarget_range,
            ) {
                retarget_events.push((snap.stable_id, new_target));
            } else {
                remove_attack.push(snap.stable_id);
            }
            continue;
        }

        // Burst / cooldown state machine.
        if snap.cooldown_ticks > 0 || snap.burst_delay_ticks > 0 {
            continue;
        }

        // Turret alignment check (lepton-precise, 16-bit).
        if let Some(turret_facing) = snap.turret_facing {
            let desired: u16 = crate::sim::movement::turret::facing_toward_lepton(
                snap.pos_rx,
                snap.pos_ry,
                snap.sub_x,
                snap.sub_y,
                target_rx,
                target_ry,
                target_sub_x,
                target_sub_y,
            );
            if !crate::sim::movement::turret::is_turret_aligned_u16(turret_facing, desired) {
                continue;
            }
        }

        // Fire one shot!
        let warhead = selected.warhead;
        // Garrison damage: apply OccupyDamageMultiplier to base damage before AoE or
        // single-target paths. Matches gamemd Fire_At which modifies damage before bullet
        // creation, so AoE splash uses the modified value.
        let base_damage = if is_garrison {
            sim_to_i32(
                SimFixed::from_num(weapon.damage) * rules.garrison_rules.occupy_damage_multiplier,
            )
        } else {
            weapon.damage
        };
        if warhead.cell_spread > SIM_ZERO {
            let aoe_hits = self::combat_aoe::apply_aoe_damage(
                entities,
                target_rx,
                target_ry,
                base_damage,
                warhead,
                rules,
                interner,
                interner.resolve(snap.owner),
            );
            for (target_id, dmg) in aoe_hits {
                let wh_iid = interner.intern(&warhead.id);
                damage_events.push((target_id, dmg, snap.stable_id, wh_iid));
            }
            if warhead.wall && weapon.damage > 0 {
                // TODO(RE): Low-bridge overlay damage still needs the recovered overlay-step
                // logic and connected-section selection. These events currently feed only the
                // elevated-bridge runtime state.
                bridge_damage_events.push(BridgeDamageEvent {
                    rx: target_rx,
                    ry: target_ry,
                    damage: weapon.damage.max(0) as u16,
                });
            }
        } else {
            // Integer damage: base_damage * verses_pct / 100.
            // base_damage already includes OccupyDamageMultiplier for garrison.
            let raw_damage: i32 = base_damage * selected.verses_pct as i32 / 100;
            let actual_damage: u16 =
                apply_prone_damage_modifier(target_prone_infantry, warhead, raw_damage);
            if actual_damage > 0 {
                let wh_iid = interner.intern(&warhead.id);
                damage_events.push((snap.target, actual_damage, snap.stable_id, wh_iid));
            }
            if warhead.wall && weapon.damage > 0 {
                // TODO(RE): Low-bridge overlay damage still needs the recovered overlay-step
                // logic and connected-section selection. These events currently feed only the
                // elevated-bridge runtime state.
                bridge_damage_events.push(BridgeDamageEvent {
                    rx: target_rx,
                    ry: target_ry,
                    damage: weapon.damage.max(0) as u16,
                });
            }
        }

        // Ore destruction: all warheads unconditionally destroy ore at impact cells.
        // CellSpreadTable[0] = 1, so even CellSpread=0 weapons check the center cell.
        destroy_ore_at_impact(
            resource_nodes,
            target_rx,
            target_ry,
            base_damage,
            warhead.cell_spread,
        );

        if let Some(ref report_id) = weapon.report {
            fire_sounds.push((interner.intern(report_id), snap.pos_rx, snap.pos_ry));
        }
        fire_events.push(SimFireEvent {
            attacker_id: snap.stable_id,
            weapon_slot: selected.slot,
            target_id: snap.target,
            garrison_muzzle_index: snap.garrison.as_ref().map(|gs| gs.fire_index),
            occupant_anim: if is_garrison {
                weapon.occupant_anim.as_ref().map(|s| interner.intern(s))
            } else {
                None
            },
        });
        if weapon.reveal_on_fire {
            reveal_events.push(RevealEvent {
                owner: snap.owner,
                rx: snap.pos_rx,
                ry: snap.pos_ry,
                radius: REVEAL_ON_FIRE_RADIUS,
            });
        }

        let weapon_burst: u8 = weapon.burst.max(1) as u8;
        let current_remaining: u8 = if snap.burst_remaining == 0 {
            weapon_burst.saturating_sub(1)
        } else {
            snap.burst_remaining.saturating_sub(1)
        };
        if current_remaining > 0 {
            burst_updates.push((snap.stable_id, current_remaining, BURST_INTER_SHOT_DELAY, 0));
        } else {
            let mut rof_ticks = rof_to_cooldown_ticks(weapon.rof, tick_ms);
            // Garrison ROF: divide by occupant count, then by multiplier.
            // More occupants = proportionally faster fire (gamemd GetROF 0x006FCFA0).
            if let Some(ref gs) = snap.garrison {
                let count = (gs.occupant_count as u16).max(1);
                rof_ticks /= count;
                if rules.garrison_rules.occupy_rof_multiplier > SIM_ZERO {
                    rof_ticks = sim_to_i32(
                        SimFixed::from_num(rof_ticks) / rules.garrison_rules.occupy_rof_multiplier,
                    ) as u16;
                }
                rof_ticks = rof_ticks.max(1);
            }
            burst_updates.push((snap.stable_id, 0, 0, rof_ticks));
        }

        // Aircraft ammo deduction: one ammo per burst completion (not per shot).
        if current_remaining == 0 {
            ammo_deduct.push(snap.stable_id);
        }

        // Track garrison buildings that fired for round-robin advancement.
        if is_garrison {
            garrison_advance.push(snap.stable_id);
        }
    }

    // Phase 3: apply retargets and burst/cooldown updates.
    for &(attacker_id, new_target_sid) in &retarget_events {
        if let Some(entity) = entities.get_mut(attacker_id) {
            if let Some(ref mut attack) = entity.attack_target {
                attack.target = new_target_sid;
            }
        }
    }
    for &(attacker_id, burst_rem, burst_delay, rof_cd) in &burst_updates {
        if let Some(entity) = entities.get_mut(attacker_id) {
            if let Some(ref mut attack) = entity.attack_target {
                attack.burst_remaining = burst_rem;
                attack.burst_delay_ticks = burst_delay;
                attack.cooldown_ticks = rof_cd;
            }
        }
    }

    // Phase 3b: deduct ammo from aircraft that completed a burst this tick.
    for &attacker_id in &ammo_deduct {
        if let Some(entity) = entities.get_mut(attacker_id) {
            if let Some(ref mut ammo) = entity.aircraft_ammo {
                ammo.current = (ammo.current - 1).max(0);
            }
        }
    }

    // Phase 3c: advance garrison fire index for buildings that fired this tick.
    // Round-robin: (idx + 1) % count — matches gamemd Fire_At 0x006FDD50.
    for &building_id in &garrison_advance {
        if let Some(entity) = entities.get_mut(building_id) {
            if let Some(cargo) = entity.passenger_role.cargo_mut() {
                let count = cargo.count() as u8;
                if count > 0 {
                    cargo.garrison_fire_index = (cargo.garrison_fire_index + 1) % count;
                }
            }
        }
    }

    // Phase 4: apply damage to targets and track last attacker for retaliation.
    let mut dead_entities: Vec<u64> = Vec::new();
    for (target_id, damage, attacker_id, _wh_id) in &damage_events {
        if let Some(target) = entities.get_mut(*target_id) {
            target.health.current = target.health.current.saturating_sub(*damage);
            if target.health.current == 0 {
                dead_entities.push(*target_id);
            }
            target.last_attacker_id = Some(*attacker_id);
        }
    }

    // Phase 5: remove AttackTarget from finished attackers.
    remove_attack.sort_unstable();
    remove_attack.dedup();
    for &attacker_id in &remove_attack {
        if let Some(entity) = entities.get_mut(attacker_id) {
            entity.attack_target = None;
        }
    }

    // Phase 6: handle death effects — death weapons, passengers, explosions, despawn.
    let death = handle_entity_deaths(
        entities,
        occupancy,
        rules,
        interner,
        &dead_entities,
        &damage_events,
        resource_nodes,
    );
    bridge_damage_events.extend(death.bridge_damage_events);

    // Phase 7: push sound events to the sink.
    if let Some(sink) = sound_sink {
        for (report_id, rx, ry) in fire_sounds {
            sink.push(SimSoundEvent::WeaponFired {
                report_sound_id: report_id,
                rx,
                ry,
            });
        }
        for (die_id, rx, ry) in death.death_sounds {
            sink.push(SimSoundEvent::EntityDied {
                die_sound_id: die_id,
                rx,
                ry,
            });
        }
    }

    if !damage_events.is_empty() {
        log::trace!(
            "Combat tick: {} shots fired, {} entities destroyed",
            damage_events.len(),
            dead_entities.len(),
        );
    }

    CombatTickResult {
        reveal_events,
        despawned_ids: death.despawned_ids,
        structure_destroyed: death.structure_destroyed,
        spy_sat_reshroud_owners: death.spy_sat_reshroud_owners,
        bridge_damage_events,
        fire_events,
        destroyed_crewed_buildings: death.destroyed_crewed_buildings,
        explosion_effects: death.explosion_effects,
    }
}

/// Check if a squared cell distance is within weapon range.
/// Compares entirely in u32 to avoid I16F16 overflow on large maps
/// (dist_sq can exceed SimFixed max of 32,767 for distant entities).
pub(crate) fn is_within_weapon_range_sq(dist_sq_cells: u32, range_cells: SimFixed) -> bool {
    let range_i64: i64 = sim_to_i32(range_cells) as i64;
    let range_sq: u32 = (range_i64 * range_i64) as u32;
    dist_sq_cells <= range_sq
}

pub(crate) fn cell_distance_sq(ax: u16, ay: u16, bx: u16, by: u16) -> u32 {
    let dx = ax as i64 - bx as i64;
    let dy = ay as i64 - by as i64;
    (dx * dx + dy * dy) as u32
}

/// Squared distance in leptons between two positions (sub-cell precise).
///
/// Uses i64 arithmetic to avoid overflow on large maps — a 200-cell lepton
/// delta squared is ~2.6 billion, which exceeds i32 max (2.1 billion).
/// 256 leptons = 1 cell.
#[allow(dead_code)] // Convenience API — callers currently use lepton_distance_sq_raw.
pub(crate) fn lepton_distance_sq(
    a: &crate::sim::components::Position,
    b: &crate::sim::components::Position,
) -> i64 {
    lepton_distance_sq_raw(a.rx, a.ry, a.sub_x, a.sub_y, b.rx, b.ry, b.sub_x, b.sub_y)
}

/// Squared distance in leptons from raw coordinates.
///
/// Same as `lepton_distance_sq` but takes individual fields instead of
/// `&Position`, for use with snapshots where positions are destructured.
pub(crate) fn lepton_distance_sq_raw(
    ax_cell: u16,
    ay_cell: u16,
    ax_sub: SimFixed,
    ay_sub: SimFixed,
    bx_cell: u16,
    by_cell: u16,
    bx_sub: SimFixed,
    by_sub: SimFixed,
) -> i64 {
    let ax: i64 = ax_cell as i64 * 256 + ax_sub.to_num::<i64>();
    let ay: i64 = ay_cell as i64 * 256 + ay_sub.to_num::<i64>();
    let bx: i64 = bx_cell as i64 * 256 + bx_sub.to_num::<i64>();
    let by: i64 = by_cell as i64 * 256 + by_sub.to_num::<i64>();
    let dx: i64 = ax - bx;
    let dy: i64 = ay - by;
    dx * dx + dy * dy
}

/// Check if a squared lepton distance is within weapon range.
///
/// Converts weapon range from cells to leptons (×256) before squaring.
/// Uses i64 to match `lepton_distance_sq()` output.
pub(crate) fn is_within_range_leptons(dist_sq_leptons: i64, range_cells: SimFixed) -> bool {
    let range_leptons: i64 = range_cells.to_num::<i64>() * 256;
    let range_sq: i64 = range_leptons * range_leptons;
    dist_sq_leptons <= range_sq
}

fn rof_to_cooldown_ticks(rof_frames: i32, tick_ms: u32) -> u16 {
    let cooldown_ms = if rof_frames <= 0 {
        500
    } else {
        let frames = rof_frames as u32;
        frames.saturating_mul(1000).div_ceil(GAME_FPS)
    };
    let step_ms = tick_ms.max(1);
    let ticks = cooldown_ms.div_ceil(step_ms);
    ticks.clamp(1, u16::MAX as u32) as u16
}

pub use self::combat_targeting::{acquire_best_target_for_entity, tick_retaliation};
