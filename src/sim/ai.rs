//! Minimal AI opponent — produces deterministic commands via the same Command API as players.
//!
//! The AI uses a simple priority-based decision loop each tick:
//! 1. Deploy MCV if one exists but no construction yard
//! 2. Build power when low or missing
//! 3. Build refinery for economy
//! 4. Build barracks and war factory for unit production
//! 5. Queue infantry and vehicles continuously
//! 6. Place ready buildings near existing base
//! 7. Send attack waves toward the nearest enemy base periodically
//!
//! All decisions are deterministic (uses SimRng). AI commands are injected
//! into the same command stream as player commands, so replays stay valid.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on rules/, map/
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/

use std::collections::BTreeMap;

use crate::map::entities::EntityCategory;
use crate::rules::object_type::{FactoryType, ObjectCategory};
use crate::rules::ruleset::RuleSet;
use crate::sim::command::{Command, CommandEnvelope, QueueMode};
use crate::sim::intern::InternedId;
use crate::sim::pathfinding::PathGrid;
use crate::sim::production;
use crate::sim::world::Simulation;

/// How often (in ticks) the AI evaluates its build/production decisions (~0.5s).
const AI_THINK_INTERVAL_TICKS: u64 = 8;

/// How often (in ticks) the AI sends an attack wave (~15s).
const AI_ATTACK_INTERVAL_TICKS: u64 = 225;

/// Minimum ticks before the AI sends its first attack (~10s).
const AI_FIRST_ATTACK_TICK: u64 = 150;

/// Maximum units to send per attack wave.
const AI_ATTACK_WAVE_SIZE: usize = 8;

/// Per-AI-owner persistent state.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AiPlayerState {
    /// House/owner name this AI controls.
    pub owner: InternedId,
    /// Tick when the last attack wave was sent.
    pub last_attack_tick: u64,
    /// Whether MCV deploy has been attempted.
    pub mcv_deployed: bool,
}

impl AiPlayerState {
    pub fn new(owner: InternedId) -> Self {
        Self {
            owner,
            last_attack_tick: 0,
            mcv_deployed: false,
        }
    }
}

/// Run one AI decision cycle for all AI players. Returns commands to inject.
pub fn tick_ai(
    sim: &Simulation,
    ai_players: &mut [AiPlayerState],
    rules: &RuleSet,
    path_grid: Option<&PathGrid>,
    height_map: &BTreeMap<(u16, u16), u8>,
) -> Vec<CommandEnvelope> {
    let mut commands: Vec<CommandEnvelope> = Vec::new();
    let execute_tick = sim.tick.saturating_add(1);

    for ai in ai_players.iter_mut() {
        let owner_str = sim.interner.resolve(ai.owner);
        // Only think every N ticks to avoid spamming.
        if !sim.tick.is_multiple_of(AI_THINK_INTERVAL_TICKS) {
            // Still check for building placement every tick.
            place_ready_buildings(
                sim,
                ai,
                rules,
                path_grid,
                height_map,
                execute_tick,
                &mut commands,
            );
            continue;
        }

        // 1. Deploy MCV if needed.
        if !ai.mcv_deployed {
            if let Some(cmd) = try_deploy_mcv(sim, owner_str, rules, execute_tick) {
                commands.push(cmd);
                ai.mcv_deployed = true;
                continue; // Wait for next think cycle.
            }
        }

        // 2. Build structures in priority order.
        // A ConYard is any structure with UndeploysInto= set (data-driven from rules.ini).
        let has_conyard = has_conyard_dynamic(sim, owner_str, rules);
        if !has_conyard {
            continue; // No conyard — can't do anything.
        }

        let building_queued = has_active_building_queue(sim, owner_str, rules);
        if !building_queued {
            if let Some(cmd) = decide_next_building(sim, owner_str, rules, execute_tick) {
                commands.push(cmd);
            }
        }

        // 3. Queue units from barracks and war factory.
        queue_units(sim, owner_str, rules, execute_tick, &mut commands);

        // 4. Place ready buildings.
        place_ready_buildings(
            sim,
            ai,
            rules,
            path_grid,
            height_map,
            execute_tick,
            &mut commands,
        );

        // 5. Send attack waves.
        if sim.tick >= AI_FIRST_ATTACK_TICK
            && sim.tick.saturating_sub(ai.last_attack_tick) >= AI_ATTACK_INTERVAL_TICKS
        {
            let attack_cmds = send_attack_wave(sim, owner_str, rules, execute_tick);
            if !attack_cmds.is_empty() {
                ai.last_attack_tick = sim.tick;
                commands.extend(attack_cmds);
            }
        }
    }

    commands
}

/// Try to find an undeployed MCV and issue a deploy command.
///
/// An MCV is any unit with `DeploysInto=` set in rules.ini (data-driven).
fn try_deploy_mcv(
    sim: &Simulation,
    owner: &str,
    rules: &RuleSet,
    execute_tick: u64,
) -> Option<CommandEnvelope> {
    for entity in sim.entities.values() {
        if !sim
            .interner
            .resolve(entity.owner)
            .eq_ignore_ascii_case(owner)
        {
            continue;
        }
        let is_deployable: bool = rules
            .object(sim.interner.resolve(entity.type_ref))
            .is_some_and(|obj| obj.deploys_into.is_some());
        if is_deployable {
            let owner_id = sim.interner.get(owner)?;
            return Some(CommandEnvelope::new(
                owner_id,
                execute_tick,
                Command::DeployMcv {
                    entity_id: entity.stable_id,
                },
            ));
        }
    }
    None
}

/// Check if the owner has any ConYard-class structure (one with UndeploysInto= set).
fn has_conyard_dynamic(sim: &Simulation, owner: &str, rules: &RuleSet) -> bool {
    has_owned_structure_matching(sim, owner, |type_id| {
        rules
            .object(type_id)
            .is_some_and(|object| object.undeploys_into.is_some())
    })
}

fn has_owned_structure_matching<F>(sim: &Simulation, owner: &str, mut matches: F) -> bool
where
    F: FnMut(&str) -> bool,
{
    sim.entities.values().any(|e| {
        e.category == EntityCategory::Structure
            && sim.interner.resolve(e.owner).eq_ignore_ascii_case(owner)
            && matches(sim.interner.resolve(e.type_ref))
    })
}

fn has_refinery_structure(sim: &Simulation, owner: &str, rules: &RuleSet) -> bool {
    has_owned_structure_matching(sim, owner, |type_id| rules.is_refinery_type(type_id))
}

fn has_power_support_structure(sim: &Simulation, owner: &str, rules: &RuleSet) -> bool {
    has_owned_structure_matching(sim, owner, |type_id| {
        rules
            .object_case_insensitive(type_id)
            .is_some_and(|object| object.power > 0)
    })
}

fn has_factory_structure(
    sim: &Simulation,
    owner: &str,
    rules: &RuleSet,
    factory_type: FactoryType,
) -> bool {
    has_owned_structure_matching(sim, owner, |type_id| {
        rules.factory_type(type_id) == Some(factory_type)
    })
}

/// Check if the AI has an active (non-empty) building/defense production queue.
fn has_active_building_queue(sim: &Simulation, owner: &str, rules: &RuleSet) -> bool {
    let view = production::queue_view_for_owner(sim, rules, owner);
    view.iter().any(|item| {
        matches!(
            item.queue_category,
            production::ProductionCategory::Building | production::ProductionCategory::Defense
        )
    })
}

/// Decide which building to queue next, following a priority order.
fn decide_next_building(
    sim: &Simulation,
    owner: &str,
    rules: &RuleSet,
    execute_tick: u64,
) -> Option<CommandEnvelope> {
    let owner_id = sim.interner.get(owner)?;
    let options = production::build_options_for_owner(sim, rules, owner);

    // Priority 1: Power support if we have none or power balance is negative.
    let has_power = has_power_support_structure(sim, owner, rules);
    let (produced, drained) = production::power_balance_for_owner(sim, rules, owner);
    if !has_power || produced < drained {
        if let Some(type_id) = find_buildable_matching(&options, rules, &sim.interner, |object| {
            object.category == ObjectCategory::Building && object.power > 0
        }) {
            return Some(make_queue_cmd(owner_id, type_id, execute_tick));
        }
    }

    // Priority 2: Refinery if we have none.
    if !has_refinery_structure(sim, owner, rules) {
        if let Some(type_id) = find_buildable_refinery(&options, rules, &sim.interner) {
            return Some(make_queue_cmd(owner_id, type_id, execute_tick));
        }
    }

    // Priority 3: Infantry producer if we have none.
    if !has_factory_structure(sim, owner, rules, FactoryType::InfantryType) {
        if let Some(type_id) = find_buildable_matching(&options, rules, &sim.interner, |object| {
            object.category == ObjectCategory::Building
                && object.factory == Some(FactoryType::InfantryType)
        }) {
            return Some(make_queue_cmd(owner_id, type_id, execute_tick));
        }
    }

    // Priority 4: Vehicle producer if we have none.
    if !has_factory_structure(sim, owner, rules, FactoryType::UnitType) {
        if let Some(type_id) = find_buildable_matching(&options, rules, &sim.interner, |object| {
            object.category == ObjectCategory::Building
                && object.factory == Some(FactoryType::UnitType)
        }) {
            return Some(make_queue_cmd(owner_id, type_id, execute_tick));
        }
    }

    // Priority 5: Second refinery for faster economy.
    let refinery_count = count_refineries(sim, owner, rules);
    if refinery_count < 2 {
        if let Some(type_id) = find_buildable_refinery(&options, rules, &sim.interner) {
            return Some(make_queue_cmd(owner_id, type_id, execute_tick));
        }
    }

    // Priority 6: Extra power if needed.
    if produced < drained + 100 {
        if let Some(type_id) = find_buildable_matching(&options, rules, &sim.interner, |object| {
            object.category == ObjectCategory::Building && object.power > 0
        }) {
            return Some(make_queue_cmd(owner_id, type_id, execute_tick));
        }
    }

    None
}

/// Queue infantry and vehicle production if queues are empty.
fn queue_units(
    sim: &Simulation,
    owner: &str,
    rules: &RuleSet,
    execute_tick: u64,
    commands: &mut Vec<CommandEnvelope>,
) {
    let Some(owner_id) = sim.interner.get(owner) else {
        return;
    };
    let current_queue = production::queue_view_for_owner(sim, rules, owner);
    let options = production::build_options_for_owner(sim, rules, owner);

    // Queue infantry if no infantry in production.
    let infantry_queued = current_queue
        .iter()
        .any(|item| item.queue_category == production::ProductionCategory::Infantry);
    if !infantry_queued {
        if let Some(type_id) = pick_combat_unit(&options, ObjectCategory::Infantry) {
            commands.push(make_queue_cmd(owner_id, type_id, execute_tick));
        }
    }

    // Queue vehicle if no vehicle in production.
    let vehicle_queued = current_queue
        .iter()
        .any(|item| item.queue_category == production::ProductionCategory::Vehicle);
    if !vehicle_queued {
        if let Some(type_id) = pick_combat_unit(&options, ObjectCategory::Vehicle) {
            commands.push(make_queue_cmd(owner_id, type_id, execute_tick));
        }
    }
}

/// Pick a combat unit to build from the available options.
/// Prefers units with weapons (Primary != None) and reasonable cost.
fn pick_combat_unit(
    options: &[production::BuildOption],
    category: ObjectCategory,
) -> Option<InternedId> {
    let mut candidates: Vec<&production::BuildOption> = options
        .iter()
        .filter(|o| o.enabled && o.object_category == category && o.cost > 0)
        .collect();
    // Sort by cost (cheapest first for fast army buildup).
    candidates.sort_by_key(|o| o.cost);
    // Pick the first (cheapest) available.
    candidates.first().map(|o| o.type_id)
}

/// Place any ready buildings near the AI's existing base.
fn place_ready_buildings(
    sim: &Simulation,
    _ai: &AiPlayerState,
    rules: &RuleSet,
    path_grid: Option<&PathGrid>,
    height_map: &BTreeMap<(u16, u16), u8>,
    execute_tick: u64,
    commands: &mut Vec<CommandEnvelope>,
) {
    let owner = sim.interner.resolve(_ai.owner);
    let ready = production::ready_buildings_for_owner(sim, rules, owner);
    if ready.is_empty() {
        return;
    }

    // Find the center of the AI's existing base.
    let base_center = find_base_center(sim, owner);
    let Some((center_rx, center_ry)) = base_center else {
        return;
    };

    for ready_building in &ready {
        let type_id = ready_building.type_id;
        let type_id_str = sim.interner.resolve(type_id);
        let foundation = rules
            .object(type_id_str)
            .map(|obj| obj.foundation.as_str())
            .unwrap_or("1x1");
        let (fw, fh) = production::foundation_dimensions(foundation);

        // Spiral outward from base center to find a valid placement.
        if let Some((rx, ry)) = find_placement_cell(
            sim,
            rules,
            owner,
            type_id_str,
            center_rx,
            center_ry,
            fw,
            fh,
            path_grid,
            height_map,
        ) {
            if let Some(owner_id) = sim.interner.get(owner) {
                commands.push(CommandEnvelope::new(
                    owner_id,
                    execute_tick,
                    Command::PlaceReadyBuilding {
                        owner: owner_id,
                        type_id,
                        rx,
                        ry,
                    },
                ));
            }
        }
    }
}

/// Send an attack wave: gather idle military units and attack-move toward enemy.
fn send_attack_wave(
    sim: &Simulation,
    owner: &str,
    rules: &RuleSet,
    execute_tick: u64,
) -> Vec<CommandEnvelope> {
    let mut commands: Vec<CommandEnvelope> = Vec::new();

    // Find nearest enemy structure as attack target.
    let Some(target) = find_nearest_enemy_structure(sim, owner) else {
        return commands;
    };

    // Gather idle military units (no MovementTarget, not harvesters).
    let mut idle_units: Vec<(u64, u16, u16)> = Vec::new();
    for entity in sim.entities.values() {
        if !sim
            .interner
            .resolve(entity.owner)
            .eq_ignore_ascii_case(owner)
        {
            continue;
        }
        if !matches!(
            entity.category,
            EntityCategory::Unit | EntityCategory::Infantry
        ) {
            continue;
        }
        if production::is_harvester_type(rules, sim.interner.resolve(entity.type_ref)) {
            continue;
        }
        // Check if unit has no movement target (idle).
        if entity.movement_target.is_none() {
            idle_units.push((entity.stable_id, entity.position.rx, entity.position.ry));
        }
    }
    idle_units.sort_by_key(|(sid, _, _)| *sid);

    // Send up to WAVE_SIZE units.
    let owner_id = match sim.interner.get(owner) {
        Some(id) => id,
        None => return commands,
    };
    for (entity_id, _, _) in idle_units.into_iter().take(AI_ATTACK_WAVE_SIZE) {
        commands.push(CommandEnvelope::new(
            owner_id,
            execute_tick,
            Command::AttackMove {
                entity_id,
                target_rx: target.0,
                target_ry: target.1,
                queue: false,
            },
        ));
    }

    if !commands.is_empty() {
        log::info!(
            "AI [{}] sending attack wave: {} units toward ({}, {})",
            owner,
            commands.len(),
            target.0,
            target.1
        );
    }

    commands
}

/// Find the center of the AI's base (average position of owned structures).
fn find_base_center(sim: &Simulation, owner: &str) -> Option<(u16, u16)> {
    let mut sum_x: i64 = 0;
    let mut sum_y: i64 = 0;
    let mut count: i64 = 0;
    for entity in sim.entities.values() {
        if entity.category == EntityCategory::Structure
            && sim
                .interner
                .resolve(entity.owner)
                .eq_ignore_ascii_case(owner)
        {
            sum_x += i64::from(entity.position.rx);
            sum_y += i64::from(entity.position.ry);
            count += 1;
        }
    }
    if count == 0 {
        return None;
    }
    Some((
        u16::try_from(sum_x / count).unwrap_or(u16::MAX),
        u16::try_from(sum_y / count).unwrap_or(u16::MAX),
    ))
}

/// Find the nearest enemy structure to attack.
fn find_nearest_enemy_structure(sim: &Simulation, owner: &str) -> Option<(u16, u16)> {
    let base_center = find_base_center(sim, owner)?;
    let mut best: Option<(u32, u16, u16)> = None;

    for entity in sim.entities.values() {
        if entity.category != EntityCategory::Structure {
            continue;
        }
        let e_owner = sim.interner.resolve(entity.owner);
        if e_owner.eq_ignore_ascii_case(owner) {
            continue;
        }
        // Skip neutral/civilian houses.
        let up = e_owner.to_ascii_uppercase();
        if matches!(
            up.as_str(),
            "NEUTRAL" | "SPECIAL" | "CIVILIAN" | "GOODGUY" | "BADGUY"
        ) {
            continue;
        }

        let dx = entity.position.rx as i64 - base_center.0 as i64;
        let dy = entity.position.ry as i64 - base_center.1 as i64;
        let dist_sq = (dx * dx + dy * dy) as u32;
        match best {
            Some((d, _, _)) if dist_sq >= d => {}
            _ => best = Some((dist_sq, entity.position.rx, entity.position.ry)),
        }
    }
    best.map(|(_, rx, ry)| (rx, ry))
}

/// Spiral outward from a center cell to find a valid building placement.
fn find_placement_cell(
    sim: &Simulation,
    rules: &RuleSet,
    owner: &str,
    type_id: &str,
    center_rx: u16,
    center_ry: u16,
    _fw: u16,
    _fh: u16,
    path_grid: Option<&PathGrid>,
    height_map: &BTreeMap<(u16, u16), u8>,
) -> Option<(u16, u16)> {
    // Try placement in a spiral pattern around the base center.
    let max_radius: i32 = 12;
    for r in 0..=max_radius {
        let min_x = (center_rx as i32 - r).max(0);
        let max_x = (center_rx as i32 + r).min(511);
        let min_y = (center_ry as i32 - r).max(0);
        let max_y = (center_ry as i32 + r).min(511);

        // Only check the perimeter of this ring.
        for x in min_x..=max_x {
            for y in [min_y, max_y] {
                let preview = production::placement_preview_for_owner(
                    sim, rules, owner, type_id, x as u16, y as u16, path_grid, height_map,
                );
                if preview.as_ref().is_some_and(|p| p.valid) {
                    return Some((x as u16, y as u16));
                }
            }
        }
        for y in (min_y + 1)..max_y {
            for x in [min_x, max_x] {
                let preview = production::placement_preview_for_owner(
                    sim, rules, owner, type_id, x as u16, y as u16, path_grid, height_map,
                );
                if preview.as_ref().is_some_and(|p| p.valid) {
                    return Some((x as u16, y as u16));
                }
            }
        }
    }
    None
}

fn find_buildable_matching<F>(
    options: &[production::BuildOption],
    rules: &RuleSet,
    interner: &crate::sim::intern::StringInterner,
    mut matches: F,
) -> Option<InternedId>
where
    F: FnMut(&crate::rules::object_type::ObjectType) -> bool,
{
    options.iter().find_map(|option| {
        let object = rules.object(interner.resolve(option.type_id))?;
        (option.enabled && matches(object)).then_some(option.type_id)
    })
}

fn find_buildable_refinery(
    options: &[production::BuildOption],
    rules: &RuleSet,
    interner: &crate::sim::intern::StringInterner,
) -> Option<InternedId> {
    find_buildable_matching(options, rules, interner, |object| {
        object.category == ObjectCategory::Building && object.refinery
    })
}

fn count_refineries(sim: &Simulation, owner: &str, rules: &RuleSet) -> usize {
    sim.entities
        .values()
        .filter(|e| {
            e.category == EntityCategory::Structure
                && sim.interner.resolve(e.owner).eq_ignore_ascii_case(owner)
                && rules.is_refinery_type(sim.interner.resolve(e.type_ref))
        })
        .count()
}

/// Create a QueueProduction command envelope.
fn make_queue_cmd(owner: InternedId, type_id: InternedId, execute_tick: u64) -> CommandEnvelope {
    CommandEnvelope::new(
        owner,
        execute_tick,
        Command::QueueProduction {
            owner,
            type_id,
            mode: QueueMode::Append,
        },
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::rules::ini_parser::IniFile;
    use crate::sim::components::Health;
    use crate::sim::miner::{MinerState, RefineryDockPhase, ResourceNode, ResourceType};
    use crate::sim::pathfinding::PathGrid;

    fn modded_ai_rules() -> RuleSet {
        RuleSet::from_ini(&IniFile::from_str(
            "[InfantryTypes]\n\
             [VehicleTypes]\n\
             0=MODHARV\n\
             [AircraftTypes]\n\
             [BuildingTypes]\n\
             0=MODCYRD\n\
             1=MODPOWR\n\
             2=FAKEREF\n\
             3=MODPROC\n\
             4=MODBARR\n\
             5=MODFACT\n\
             [MODCYRD]\n\
             Name=Mod Construction Yard\n\
             Foundation=2x2\n\
             Factory=BuildingType\n\
             UndeploysInto=MODMCV\n\
             Strength=1000\n\
             Cost=3000\n\
             TechLevel=1\n\
             Owner=Americans\n\
             BaseNormal=yes\n\
             [MODPOWR]\n\
             Name=Mod Power Support\n\
             Foundation=2x2\n\
             Power=200\n\
             Strength=800\n\
             Cost=600\n\
             TechLevel=1\n\
             Owner=Americans\n\
             [FAKEREF]\n\
             Name=Fake Refinery\n\
             Foundation=3x3\n\
             Strength=800\n\
             Cost=900\n\
             TechLevel=1\n\
             Owner=Americans\n\
             [MODPROC]\n\
             Name=Mod Ore Processor\n\
             Foundation=3x3\n\
             Refinery=yes\n\
             FreeUnit=MODHARV\n\
             Strength=900\n\
             Cost=900\n\
             TechLevel=1\n\
             Owner=Americans\n\
             [MODBARR]\n\
             Name=Mod Infantry Node\n\
             Foundation=2x2\n\
             Factory=InfantryType\n\
             Strength=700\n\
             Cost=500\n\
             TechLevel=1\n\
             Owner=Americans\n\
             [MODFACT]\n\
             Name=Mod Vehicle Node\n\
             Foundation=3x2\n\
             Factory=UnitType\n\
             Strength=900\n\
             Cost=800\n\
             TechLevel=1\n\
             Owner=Americans\n\
             ExitCoord=512,256,0\n\
             [MODHARV]\n\
             Name=Mod Harvester\n\
             Harvester=yes\n\
             Dock=MODPROC\n\
             Speed=6\n\
             Strength=600\n\
             Cost=1400\n\
             TechLevel=1\n\
             Owner=Americans\n",
        ))
        .expect("modded AI rules should parse")
    }

    fn spawn_structure(
        sim: &mut Simulation,
        sid: u64,
        owner: &str,
        type_id: &str,
        rx: u16,
        ry: u16,
    ) {
        let owner_id = sim.interner.intern(owner);
        let type_id_interned = sim.interner.intern(type_id);
        let ge = crate::sim::game_entity::GameEntity::new(
            sid,
            rx,
            ry,
            0,
            0,
            owner_id,
            Health {
                current: 1000,
                max: 1000,
            },
            type_id_interned,
            EntityCategory::Structure,
            0,
            5,
            false,
        );
        sim.entities.insert(ge);
        if sim.next_stable_entity_id <= sid {
            sim.next_stable_entity_id = sid + 1;
        }
    }

    #[test]
    fn test_ai_player_state_new() {
        let mut interner = crate::sim::intern::StringInterner::new();
        let owner_id = interner.intern("Russians");
        let state = AiPlayerState::new(owner_id);
        assert_eq!(state.owner, owner_id);
        assert_eq!(state.last_attack_tick, 0);
        assert!(!state.mcv_deployed);
    }

    #[test]
    fn test_find_buildable_matching_positive_power_uses_rules_data() {
        let rules = modded_ai_rules();
        let mut interner = crate::sim::intern::StringInterner::new();
        let modpowr_id = interner.intern("MODPOWR");
        let options = vec![production::BuildOption {
            type_id: modpowr_id,
            display_name: "Mod Power Support".to_string(),
            cost: 600,
            object_category: ObjectCategory::Building,
            queue_category: production::ProductionCategory::Building,
            enabled: true,
            reason: None,
        }];
        assert_eq!(
            find_buildable_matching(&options, &rules, &interner, |object| object.power > 0),
            Some(modpowr_id)
        );
    }

    #[test]
    fn test_find_buildable_matching_factory_uses_rules_data() {
        let rules = modded_ai_rules();
        let mut interner = crate::sim::intern::StringInterner::new();
        let modbarr_id = interner.intern("MODBARR");
        let options = vec![production::BuildOption {
            type_id: modbarr_id,
            display_name: "Mod Infantry Node".to_string(),
            cost: 500,
            object_category: ObjectCategory::Building,
            queue_category: production::ProductionCategory::Building,
            enabled: true,
            reason: None,
        }];
        assert_eq!(
            find_buildable_matching(&options, &rules, &interner, |object| {
                object.factory == Some(FactoryType::InfantryType)
            }),
            Some(modbarr_id)
        );
    }

    #[test]
    fn test_find_buildable_matching_no_match_when_disabled() {
        let rules = modded_ai_rules();
        let mut interner = crate::sim::intern::StringInterner::new();
        let modpowr_id = interner.intern("MODPOWR");
        let options = vec![production::BuildOption {
            type_id: modpowr_id,
            display_name: "Mod Power Support".to_string(),
            cost: 600,
            object_category: ObjectCategory::Building,
            queue_category: production::ProductionCategory::Building,
            enabled: false,
            reason: None,
        }];
        assert_eq!(
            find_buildable_matching(&options, &rules, &interner, |object| object.power > 0),
            None
        );
    }

    #[test]
    fn test_make_queue_cmd() {
        let mut interner = crate::sim::intern::StringInterner::new();
        let owner_id = interner.intern("Americans");
        let type_id = interner.intern("MODPOWR");
        let cmd = make_queue_cmd(owner_id, type_id, 10);
        assert_eq!(cmd.owner, owner_id);
        assert_eq!(cmd.execute_tick, 10);
        assert!(matches!(
            cmd.payload,
            Command::QueueProduction {
                owner: cmd_owner,
                type_id: cmd_type,
                ..
            } if cmd_owner == owner_id && cmd_type == type_id
        ));
    }

    #[test]
    fn test_find_buildable_refinery_matches_rules_flag() {
        let rules = RuleSet::from_ini(&IniFile::from_str(
            "[InfantryTypes]\n\
             [VehicleTypes]\n\
             [AircraftTypes]\n\
             [BuildingTypes]\n\
             0=FAKEREF\n\
             1=MODPROC\n\
             [FAKEREF]\n\
             Name=Fake Refinery\n\
             [MODPROC]\n\
             Refinery=yes\n",
        ))
        .expect("rules should parse");
        let mut interner = crate::sim::intern::StringInterner::new();
        let fakeref_id = interner.intern("FAKEREF");
        let modproc_id = interner.intern("MODPROC");
        let options = vec![
            production::BuildOption {
                type_id: fakeref_id,
                display_name: "Fake Refinery".to_string(),
                cost: 1000,
                object_category: ObjectCategory::Building,
                queue_category: production::ProductionCategory::Building,
                enabled: true,
                reason: None,
            },
            production::BuildOption {
                type_id: modproc_id,
                display_name: "Mod Refinery".to_string(),
                cost: 1200,
                object_category: ObjectCategory::Building,
                queue_category: production::ProductionCategory::Building,
                enabled: true,
                reason: None,
            },
        ];

        assert_eq!(
            find_buildable_refinery(&options, &rules, &interner),
            Some(modproc_id)
        );
    }

    #[test]
    fn test_count_refineries_ignores_ref_like_names_without_flag() {
        let rules = modded_ai_rules();
        let mut sim = Simulation::new();
        spawn_structure(&mut sim, 1, "Americans", "FAKEREF", 10, 10);
        spawn_structure(&mut sim, 2, "Americans", "MODPROC", 14, 10);

        assert!(has_refinery_structure(&sim, "Americans", &rules));
        assert_eq!(count_refineries(&sim, "Americans", &rules), 1);
    }

    #[test]
    fn test_has_factory_structure_uses_factory_flag() {
        let rules = modded_ai_rules();
        let mut sim = Simulation::new();
        spawn_structure(&mut sim, 1, "Americans", "MODBARR", 10, 10);
        spawn_structure(&mut sim, 2, "Americans", "MODFACT", 15, 10);

        assert!(has_factory_structure(
            &sim,
            "Americans",
            &rules,
            FactoryType::InfantryType
        ));
        assert!(has_factory_structure(
            &sim,
            "Americans",
            &rules,
            FactoryType::UnitType
        ));
    }

    #[test]
    fn test_decide_next_building_uses_custom_power_and_factory_flags() {
        let rules = modded_ai_rules();
        let mut sim = Simulation::new();
        // Pre-intern all type names so decide_next_building can find them.
        let modpowr_id = sim.interner.intern("MODPOWR");
        let modproc_id = sim.interner.intern("MODPROC");
        let modbarr_id = sim.interner.intern("MODBARR");
        let modfact_id = sim.interner.intern("MODFACT");
        spawn_structure(&mut sim, 1, "Americans", "MODCYRD", 10, 10);

        let first = decide_next_building(&sim, "Americans", &rules, 1).expect("power build");
        assert!(matches!(
            first.payload,
            Command::QueueProduction { type_id, .. } if type_id == modpowr_id
        ));

        spawn_structure(&mut sim, 2, "Americans", "MODPOWR", 14, 10);
        let second = decide_next_building(&sim, "Americans", &rules, 2).expect("refinery build");
        assert!(matches!(
            second.payload,
            Command::QueueProduction { type_id, .. } if type_id == modproc_id
        ));

        spawn_structure(&mut sim, 3, "Americans", "MODPROC", 18, 10);
        let third =
            decide_next_building(&sim, "Americans", &rules, 3).expect("infantry factory build");
        assert!(matches!(
            third.payload,
            Command::QueueProduction { type_id, .. } if type_id == modbarr_id
        ));

        spawn_structure(&mut sim, 4, "Americans", "MODBARR", 22, 10);
        let fourth =
            decide_next_building(&sim, "Americans", &rules, 4).expect("vehicle factory build");
        assert!(matches!(
            fourth.payload,
            Command::QueueProduction { type_id, .. } if type_id == modfact_id
        ));
    }

    #[test]
    fn test_modded_refinery_cycle_uses_custom_flags_end_to_end() {
        let rules = modded_ai_rules();
        let mut sim = Simulation::new();
        let grid = PathGrid::new(64, 64);
        let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();

        spawn_structure(&mut sim, 1, "Americans", "MODCYRD", 10, 10);

        assert!(production::enqueue_by_type(
            &mut sim,
            &rules,
            "Americans",
            "MODPROC"
        ));
        for _ in 0..240 {
            let _ = sim.advance_tick(&[], Some(&rules), &height_map, Some(&grid), 33);
            if !production::ready_buildings_for_owner(&sim, &rules, "Americans").is_empty() {
                break;
            }
        }
        let modproc_id = sim
            .interner
            .get("MODPROC")
            .expect("MODPROC should be interned");
        assert_eq!(
            production::ready_buildings_for_owner(&sim, &rules, "Americans")
                .into_iter()
                .map(|building| building.type_id)
                .collect::<Vec<_>>(),
            vec![modproc_id]
        );

        assert!(production::place_ready_building(
            &mut sim,
            &rules,
            "Americans",
            "MODPROC",
            14,
            10,
            Some(&grid),
            &height_map,
        ));

        let refinery_sid = sim
            .entities
            .values()
            .find_map(|e| {
                (sim.interner
                    .resolve(e.owner)
                    .eq_ignore_ascii_case("Americans")
                    && sim.interner.resolve(e.type_ref) == "MODPROC"
                    && e.category == EntityCategory::Structure)
                    .then_some(e.stable_id)
            })
            .expect("placed refinery should exist");

        let miner_sid = sim
            .entities
            .values()
            .find_map(|e| {
                (sim.interner
                    .resolve(e.owner)
                    .eq_ignore_ascii_case("Americans")
                    && sim.interner.resolve(e.type_ref) == "MODHARV"
                    && e.category == EntityCategory::Unit)
                    .then_some(e.stable_id)
            })
            .expect("free harvester should spawn");

        assert_eq!(count_refineries(&sim, "Americans", &rules), 1);
        assert!(has_refinery_structure(&sim, "Americans", &rules));
        let credits_before_unload = production::credits_for_owner(&sim, "Americans");

        sim.production.resource_nodes.insert(
            (19, 12),
            ResourceNode {
                resource_type: ResourceType::Ore,
                remaining: 20,
            },
        );

        let mut saw_harvest = false;
        let mut saw_return = false;
        let mut saw_dock_or_unload = false;
        let mut saw_dock_reservation = false;
        let mut saw_unload = false;
        let mut saw_home_refinery = false;

        // Needs enough ticks for: harvest + return + unload.
        // Unload alone: up to 20 bales × 57 ticks/bale = 1140 ticks at 60Hz.
        for _ in 0..2000 {
            let _ = sim.advance_tick(&[], Some(&rules), &height_map, Some(&grid), 33);

            let miner = sim
                .entities
                .get(miner_sid)
                .and_then(|e| e.miner.as_ref())
                .expect("miner component should exist");
            match miner.state {
                MinerState::Harvest => saw_harvest = true,
                MinerState::ReturnToRefinery => saw_return = true,
                MinerState::Dock => {
                    saw_dock_or_unload = true;
                    if miner.dock_phase == RefineryDockPhase::Unloading {
                        saw_unload = true;
                    }
                }
                MinerState::Unload => {
                    saw_dock_or_unload = true;
                    saw_unload = true;
                }
                _ => {}
            }
            if miner.home_refinery == Some(refinery_sid) {
                saw_home_refinery = true;
            }

            if sim
                .production
                .dock_reservations
                .occupied
                .get(&refinery_sid)
                .copied()
                == Some(miner_sid)
            {
                saw_dock_reservation = true;
            }

            if saw_unload
                && production::credits_for_owner(&sim, "Americans") > credits_before_unload
                && saw_home_refinery
            {
                break;
            }
        }

        let miner = sim
            .entities
            .get(miner_sid)
            .and_then(|e| e.miner.as_ref())
            .expect("miner component should exist");

        assert!(saw_harvest, "miner should harvest ore");
        assert!(saw_return, "miner should return to refinery");
        assert!(
            saw_dock_or_unload,
            "miner should dock or unload at the refinery"
        );
        assert!(
            saw_dock_reservation,
            "refinery dock reservation should be used"
        );
        assert!(saw_unload, "miner should reach unload state");
        assert!(
            saw_home_refinery,
            "miner should complete unloading at the refinery"
        );
        assert!(
            production::credits_for_owner(&sim, "Americans") > credits_before_unload,
            "unloading should increase owner credits"
        );
        assert!(
            sim.production
                .resource_nodes
                .get(&(19, 12))
                .map(|node| node.remaining < 20)
                .unwrap_or(true),
            "ore node should be consumed during harvesting"
        );
        assert_eq!(miner.home_refinery, Some(refinery_sid));
        assert_eq!(count_refineries(&sim, "Americans", &rules), 1);
    }
}
