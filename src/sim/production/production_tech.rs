//! Tech tree, build options, factory matching, and spawn cell logic.
//!
//! Determines what a player can build based on owned structures, prerequisites,
//! faction ownership, and available factories. Also handles spawn cell selection
//! for newly produced units.

use crate::map::entities::EntityCategory;
use crate::rules::object_type::{BuildCategory, FactoryType, ObjectCategory};
use crate::rules::ruleset::RuleSet;
use crate::sim::entity_store::EntityStore;
use crate::sim::world::Simulation;

use super::production_queue::credits_for_owner;
use super::production_types::*;

/// Maximum tech level available in standard skirmish/multiplayer.
/// Units with TechLevel > this are not buildable.
const MATCH_TECH_LEVEL: i32 = 10;

pub(super) fn build_option_for_owner(
    sim: &Simulation,
    rules: &RuleSet,
    owner: &str,
    type_id: &str,
    mode: BuildMode,
) -> Option<BuildOption> {
    let obj = rules.object(type_id)?;
    let queue_category = production_category_for_object(obj);

    let mut reason: Option<BuildDisabledReason> = None;
    if obj.tech_level < 0 || obj.tech_level > MATCH_TECH_LEVEL {
        reason = Some(BuildDisabledReason::UnbuildableTechLevel);
    } else if mode == BuildMode::Strict
        && !obj.owner.is_empty()
        && !obj.owner.iter().any(|o| o.eq_ignore_ascii_case(owner))
    {
        reason = Some(BuildDisabledReason::WrongOwner);
    } else if mode == BuildMode::Strict
        && !obj.required_houses.is_empty()
        && !obj
            .required_houses
            .iter()
            .any(|o| o.eq_ignore_ascii_case(owner))
    {
        reason = Some(BuildDisabledReason::WrongHouse);
    } else if mode == BuildMode::Strict
        && !obj.forbidden_houses.is_empty()
        && obj
            .forbidden_houses
            .iter()
            .any(|h| h.eq_ignore_ascii_case(owner))
    {
        reason = Some(BuildDisabledReason::ForbiddenHouse);
    } else if mode == BuildMode::Strict
        && (obj.requires_stolen_allied_tech
            || obj.requires_stolen_soviet_tech
            || obj.requires_stolen_third_tech)
    {
        // Spy infiltration not yet implemented — always block stolen-tech units.
        reason = Some(BuildDisabledReason::RequiresStolenTech);
    } else if mode == BuildMode::Strict {
        // PrerequisiteOverride: if owner has ANY override building, skip normal prereqs.
        let override_satisfied = !obj.prerequisite_override.is_empty()
            && has_any_override_building(sim, owner, &obj.prerequisite_override);
        if !override_satisfied {
            if let Some(missing) = first_missing_prereq(sim, rules, owner, &obj.prerequisite) {
                reason = Some(BuildDisabledReason::MissingPrerequisite(missing));
            }
        }
    }
    if reason.is_none()
        && mode == BuildMode::Strict
        && !has_factory_for_owner(&sim.entities, rules, owner, queue_category, &sim.interner)
    {
        reason = Some(BuildDisabledReason::NoFactory);
    }
    // BuildLimit check: count owned entities + queued + ready-for-placement.
    if reason.is_none() && mode == BuildMode::Strict {
        if let Some(limit) = effective_build_limit(obj.build_limit) {
            if count_owned_and_queued(sim, owner, &obj.id) >= limit {
                reason = Some(BuildDisabledReason::AtBuildLimit);
            }
        }
    }
    if reason.is_none() && (obj.cost <= 0 || credits_for_owner(sim, owner) < obj.cost) {
        reason = Some(BuildDisabledReason::InsufficientCredits);
    }
    let type_interned = sim.interner.get(type_id).unwrap_or_default();
    Some(BuildOption {
        type_id: type_interned,
        display_name: obj.name.clone().unwrap_or_else(|| obj.id.clone()),
        cost: obj.cost,
        object_category: obj.category,
        queue_category,
        enabled: reason.is_none(),
        reason,
    })
}

pub(super) fn build_options_for_owner_mode(
    sim: &Simulation,
    rules: &RuleSet,
    owner: &str,
    mode: BuildMode,
) -> Vec<BuildOption> {
    let mut out: Vec<BuildOption> = Vec::new();
    for id in &rules.building_ids {
        if let Some(opt) = build_option_for_owner(sim, rules, owner, id, mode) {
            out.push(opt);
        }
    }
    for id in &rules.infantry_ids {
        if let Some(opt) = build_option_for_owner(sim, rules, owner, id, mode) {
            out.push(opt);
        }
    }
    for id in &rules.vehicle_ids {
        if let Some(opt) = build_option_for_owner(sim, rules, owner, id, mode) {
            out.push(opt);
        }
    }
    for id in &rules.aircraft_ids {
        if let Some(opt) = build_option_for_owner(sim, rules, owner, id, mode) {
            out.push(opt);
        }
    }
    out.sort_by_key(|opt| opt.queue_category);
    out
}

pub(super) fn should_use_relaxed_build_mode(
    sim: &Simulation,
    rules: &RuleSet,
    owner: &str,
) -> bool {
    if !prototype_fallback_enabled() {
        return false;
    }
    let strict = build_options_for_owner_mode(sim, rules, owner, BuildMode::Strict);
    !strict.iter().any(|o| o.enabled)
}

/// Prototype build fallback — disabled by default.
/// If needed in the future, move to GameOptions (synced across multiplayer peers).
const PROTOTYPE_BUILD_FALLBACK: bool = false;

pub(super) fn prototype_fallback_enabled() -> bool {
    PROTOTYPE_BUILD_FALLBACK
}

/// Check if the owner has ANY completed structure from the PrerequisiteOverride list.
fn has_any_override_building(sim: &Simulation, owner: &str, overrides: &[String]) -> bool {
    sim.entities.values().any(|e| {
        sim.interner.resolve(e.owner).eq_ignore_ascii_case(owner)
            && e.category == EntityCategory::Structure
            && e.building_up.is_none()
            && overrides
                .iter()
                .any(|ov| ov.eq_ignore_ascii_case(sim.interner.resolve(e.type_ref)))
    })
}

/// Interpret BuildLimit value. Returns None if no limit applies (0 = unlimited).
fn effective_build_limit(build_limit: i32) -> Option<u32> {
    if build_limit == 0 {
        return None;
    }
    Some(build_limit.unsigned_abs())
}

/// Count owned entities + queued items + ready-for-placement of this type for an owner.
fn count_owned_and_queued(sim: &Simulation, owner: &str, type_id: &str) -> u32 {
    let owner_id = sim.interner.get(owner);
    let type_interned = sim.interner.get(type_id);

    let owned = match (owner_id, type_interned) {
        (Some(oid), Some(tid)) => sim
            .entities
            .values()
            .filter(|e| e.owner == oid && e.type_ref == tid)
            .count() as u32,
        _ => 0,
    };

    let queued = owner_id
        .and_then(|oid| sim.production.queues_by_owner.get(&oid))
        .map(|queues| {
            queues
                .values()
                .flat_map(|queue| queue.iter())
                .filter(|item| type_interned.map_or(false, |tid| item.type_id == tid))
                .count() as u32
        })
        .unwrap_or(0);

    let ready = owner_id
        .and_then(|oid| sim.production.ready_by_owner.get(&oid))
        .map(|ready| {
            ready
                .iter()
                .filter(|&&tid| type_interned.map_or(false, |expected| tid == expected))
                .count() as u32
        })
        .unwrap_or(0);

    owned + queued + ready
}

fn first_missing_prereq(
    sim: &Simulation,
    rules: &RuleSet,
    owner: &str,
    prereqs: &[String],
) -> Option<String> {
    for p in prereqs {
        if p.is_empty() {
            continue;
        }
        // Only structures satisfy prerequisites — units/infantry/aircraft don't count.
        let ok = sim.entities.values().any(|e| {
            sim.interner.resolve(e.owner).eq_ignore_ascii_case(owner)
                && e.category == EntityCategory::Structure
                && e.building_up.is_none()
                && structure_satisfies_prerequisite(rules, sim.interner.resolve(e.type_ref), p)
        });
        if !ok {
            return Some(p.clone());
        }
    }
    None
}

pub(super) fn production_category_for_object(
    obj: &crate::rules::object_type::ObjectType,
) -> ProductionCategory {
    match obj.category {
        ObjectCategory::Infantry => ProductionCategory::Infantry,
        ObjectCategory::Vehicle => ProductionCategory::Vehicle,
        ObjectCategory::Aircraft => ProductionCategory::Aircraft,
        ObjectCategory::Building => match obj.build_cat {
            Some(BuildCategory::Combat) => ProductionCategory::Defense,
            _ => ProductionCategory::Building,
        },
    }
}

pub(super) fn supports_live_production(obj: &crate::rules::object_type::ObjectType) -> bool {
    matches!(
        production_category_for_object(obj),
        ProductionCategory::Building
            | ProductionCategory::Defense
            | ProductionCategory::Infantry
            | ProductionCategory::Vehicle
            | ProductionCategory::Aircraft
    )
}

fn has_factory_for_owner(
    entities: &EntityStore,
    rules: &RuleSet,
    owner: &str,
    category: ProductionCategory,
    interner: &crate::sim::intern::StringInterner,
) -> bool {
    entities.values().any(|e| {
        interner.resolve(e.owner).eq_ignore_ascii_case(owner)
            && e.category == EntityCategory::Structure
            && e.building_up.is_none()
            && is_production_factory(rules, interner.resolve(e.type_ref), category)
    })
}

/// Check if a structure is a production factory for the given category.
///
/// Uses the data-driven Factory= key from rules.ini via RuleSet.factory_map.
/// A building with `Factory=InfantryType` produces infantry, `Factory=UnitType`
/// produces vehicles, etc. Buildings without Factory= are never factories.
pub(super) fn is_production_factory(
    rules: &RuleSet,
    structure_id: &str,
    category: ProductionCategory,
) -> bool {
    let Some(factory_type) = rules.factory_type(structure_id) else {
        return false;
    };
    match category {
        ProductionCategory::Infantry => factory_type == FactoryType::InfantryType,
        ProductionCategory::Vehicle => factory_type == FactoryType::UnitType,
        ProductionCategory::Aircraft => factory_type == FactoryType::AircraftType,
        ProductionCategory::Building | ProductionCategory::Defense => {
            factory_type == FactoryType::BuildingType
        }
    }
}

const RA2_QUEUE_FRAME_MS: u64 = 66;
const RA2_BUILD_SPEED_TICK_SCALE: f64 = 0.9;

#[inline]
fn trunc_to_i32(value: f64) -> i32 {
    value.trunc() as i32
}

pub(in crate::sim) fn build_time_base_frames(
    rules: &RuleSet,
    obj: &crate::rules::object_type::ObjectType,
) -> u32 {
    if obj.cost <= 0 {
        return 0;
    }
    // RE-proven base path:
    //   baseValue = trunc(cost * BuildSpeed * 0.9)
    //   rawFrames = trunc(baseValue * typeBuildTimeMult) then
    //   trunc(rawFrames * technoTypeBuildTimeMult)
    //
    // The local sim does not yet model the separate house/type build-time
    // multiplier helper, so that term stays at 1.0 here and the per-object
    // `BuildTimeMultiplier` remains the only type-specific multiplier.
    let base_value = trunc_to_i32(
        obj.cost.max(0) as f64
            * rules.production.build_speed.max(0.0) as f64
            * RA2_BUILD_SPEED_TICK_SCALE,
    );
    let raw_frames = trunc_to_i32(base_value as f64 * obj.build_time_multiplier as f64).max(0);
    raw_frames as u32
}

pub(in crate::sim) fn effective_progress_rate_ppm_for_type(
    sim: &Simulation,
    rules: &RuleSet,
    owner: &str,
    type_id: &str,
) -> u64 {
    let Some(obj) = rules.object(type_id) else {
        return PRODUCTION_RATE_SCALE;
    };
    effective_progress_rate_ppm_for_category(sim, rules, owner, obj.category)
}

pub(super) fn effective_progress_rate_ppm_for_category(
    sim: &Simulation,
    rules: &RuleSet,
    owner: &str,
    category: ObjectCategory,
) -> u64 {
    // power_speed and queue_time are both scaled by PRODUCTION_RATE_SCALE (1M).
    // effective_rate = power_speed / queue_time, also at 1M scale.
    let power_speed: u64 = owner_power_speed_multiplier_ppm(sim, rules, owner);
    let queue_time: u64 =
        matching_factory_time_multiplier_ppm(&sim.entities, rules, owner, category, &sim.interner);
    // (power_speed / queue_time) * PRODUCTION_RATE_SCALE
    // = power_speed * PRODUCTION_RATE_SCALE / queue_time
    let rate: u64 = (u128::from(power_speed) * u128::from(PRODUCTION_RATE_SCALE)
        / u128::from(queue_time.max(1))) as u64;
    rate.max(1)
}

pub(super) fn estimated_real_time_ms(base_frames: u32, rate_ppm: u64) -> u32 {
    if base_frames == 0 {
        return 0;
    }
    let denom = u128::from(rate_ppm.max(1));
    let numer = u128::from(base_frames)
        * u128::from(RA2_QUEUE_FRAME_MS)
        * u128::from(PRODUCTION_RATE_SCALE);
    let rounded_up = numer.div_ceil(denom);
    rounded_up.min(u128::from(u32::MAX)) as u32
}

pub(in crate::sim) fn effective_time_to_build_frames_for_type(
    sim: &Simulation,
    rules: &RuleSet,
    owner: &str,
    type_id: &str,
    base_frames: u32,
) -> u32 {
    let Some(obj) = rules.object(type_id) else {
        return base_frames;
    };
    effective_time_to_build_frames_for_object(sim, rules, owner, obj, base_frames)
}

fn effective_time_to_build_frames_for_object(
    sim: &Simulation,
    rules: &RuleSet,
    owner: &str,
    obj: &crate::rules::object_type::ObjectType,
    base_frames: u32,
) -> u32 {
    let speed = owner_effective_production_speed(sim, rules, owner);
    let mut time_to_build = trunc_to_i32(base_frames as f64 / speed.max(0.01));
    time_to_build = apply_multiple_factory_scaling(
        time_to_build,
        rules.production.multiple_factory,
        matching_factory_count_for_owner(&sim.entities, rules, owner, obj.category, &sim.interner),
    );
    if obj.category == ObjectCategory::Building && obj.wall {
        time_to_build = trunc_to_i32(
            time_to_build as f64 * rules.production.wall_build_speed_coefficient as f64,
        );
    }
    time_to_build.max(0) as u32
}

fn apply_multiple_factory_scaling(
    time_to_build: i32,
    multiple_factory: f32,
    queue_factory_count: u32,
) -> i32 {
    if multiple_factory <= 0.0 || queue_factory_count <= 1 {
        return time_to_build;
    }
    let mut scaled = time_to_build;
    for _ in 1..queue_factory_count {
        scaled = trunc_to_i32(scaled as f64 * multiple_factory as f64);
    }
    scaled
}

fn owner_effective_production_speed(sim: &Simulation, rules: &RuleSet, owner: &str) -> f64 {
    let power_pct = owner_power_percentage(sim, owner);
    let mut speed = 1.0 - (1.0 - power_pct) * rules.production.low_power_penalty_modifier as f64;
    speed = speed.max(rules.production.min_low_power_production_speed as f64);
    if power_pct < 1.0 {
        speed = speed.min(rules.production.max_low_power_production_speed as f64);
    }
    if speed == 0.0 { 0.01 } else { speed }
}

fn owner_power_percentage(sim: &Simulation, owner: &str) -> f64 {
    let (produced, drained) = sim
        .interner
        .get(owner)
        .and_then(|id| sim.power_states.get(&id))
        .map(|state| (state.total_output, state.total_drain))
        .unwrap_or((0, 0));

    if drained <= 0 {
        return 1.0;
    }

    ((produced.max(0) as f64) / (drained as f64)).clamp(0.0, 1.0)
}

/// Power-speed multiplier scaled by PRODUCTION_RATE_SCALE (1M = 1.0×).
fn owner_power_speed_multiplier_ppm(sim: &Simulation, rules: &RuleSet, owner: &str) -> u64 {
    (owner_effective_production_speed(sim, rules, owner) * PRODUCTION_RATE_SCALE as f64).trunc()
        as u64
}

/// Factory time multiplier scaled by PRODUCTION_RATE_SCALE (1M = 1.0×).
/// MultipleFactory^(n-1) computed via repeated integer multiply.
fn matching_factory_time_multiplier_ppm(
    entities: &EntityStore,
    rules: &RuleSet,
    owner: &str,
    category: ObjectCategory,
    interner: &crate::sim::intern::StringInterner,
) -> u64 {
    let factory_count: u32 =
        matching_factory_count_for_owner(entities, rules, owner, category, interner);
    if factory_count <= 1 {
        return PRODUCTION_RATE_SCALE; // 1.0×
    }
    // Use pre-computed PPM value from rules (converted at INI parse time).
    let mf_ppm: u64 = rules.production.multiple_factory_ppm;
    // Compute mf_ppm^(n-1) / PRODUCTION_RATE_SCALE^(n-2) via repeated multiply+divide.
    let mut result: u64 = mf_ppm;
    for _ in 1..(factory_count - 1) {
        result = result * mf_ppm / PRODUCTION_RATE_SCALE;
    }
    result.max(1)
}

fn matching_factory_count_for_owner(
    entities: &EntityStore,
    rules: &RuleSet,
    owner: &str,
    category: ObjectCategory,
    interner: &crate::sim::intern::StringInterner,
) -> u32 {
    entities
        .values()
        .filter(|e| {
            interner.resolve(e.owner).eq_ignore_ascii_case(owner)
                && e.category == EntityCategory::Structure
                && e.building_up.is_none()
                && is_matching_factory(rules, interner.resolve(e.type_ref), category)
        })
        .count() as u32
}

pub fn producer_candidates_for_owner_category(
    entities: &EntityStore,
    rules: &RuleSet,
    owner: &str,
    category: ProductionCategory,
    require_matching_factory: bool,
    interner: &crate::sim::intern::StringInterner,
) -> Vec<(u64, u16, u16, String)> {
    let mut preferred_factories: Vec<(u64, u16, u16, String)> = Vec::new();
    for e in entities.values() {
        if !interner.resolve(e.owner).eq_ignore_ascii_case(owner) {
            continue;
        }
        if e.category != EntityCategory::Structure {
            continue;
        }
        if e.building_up.is_some() {
            continue;
        }
        let type_ref_str = interner.resolve(e.type_ref);
        let is_match = is_production_factory(rules, type_ref_str, category);
        if require_matching_factory && !is_match {
            continue;
        }
        if !require_matching_factory || is_match {
            preferred_factories.push((
                e.stable_id,
                e.position.rx,
                e.position.ry,
                type_ref_str.to_string(),
            ));
        }
    }
    preferred_factories.sort_by(|a, b| a.0.cmp(&b.0));
    preferred_factories
}

pub fn is_matching_factory(
    rules: &RuleSet,
    structure_id: &str,
    produced_category: ObjectCategory,
) -> bool {
    match produced_category {
        ObjectCategory::Infantry => {
            is_production_factory(rules, structure_id, ProductionCategory::Infantry)
        }
        ObjectCategory::Vehicle => {
            is_production_factory(rules, structure_id, ProductionCategory::Vehicle)
        }
        ObjectCategory::Aircraft => {
            is_production_factory(rules, structure_id, ProductionCategory::Aircraft)
        }
        ObjectCategory::Building => {
            is_production_factory(rules, structure_id, ProductionCategory::Building)
        }
    }
}

pub fn structure_satisfies_prerequisite(rules: &RuleSet, structure_id: &str, prereq: &str) -> bool {
    // Direct match: the structure ID is exactly the prerequisite.
    if structure_id.eq_ignore_ascii_case(prereq) {
        return true;
    }
    // Alias match: look up the prerequisite in [General] PrerequisiteXxx groups.
    // e.g. prereq="POWER" → check if structure_id is in PrerequisitePower list.
    if let Some(group) = rules.prerequisite_group(prereq) {
        let sid_upper: String = structure_id.to_ascii_uppercase();
        return group.iter().any(|id| *id == sid_upper);
    }
    false
}

pub fn foundation_dimensions(foundation: &str) -> (u16, u16) {
    let mut parts = foundation.split('x');
    let width = parts
        .next()
        .and_then(|value| value.trim().parse::<u16>().ok())
        .unwrap_or(1);
    let height = parts
        .next()
        .and_then(|value| value.trim().parse::<u16>().ok())
        .unwrap_or(1);
    (width.max(1), height.max(1))
}
