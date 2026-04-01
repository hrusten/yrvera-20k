//! Production queue tests — verifies build queue ordering, credit deduction, prerequisite
//! checks, multi-factory speed bonus, and queue pause/resume behavior.

use std::collections::{BTreeMap, VecDeque};

use super::{
    BuildQueueState, ProductionCategory, build_options_for_owner, credits_for_owner,
    queue_view_for_owner, tick_production, toggle_pause_for_owner_category,
};
use crate::rules::ini_parser::IniFile;
use crate::rules::locomotor_type::SpeedType;
use crate::rules::ruleset::RuleSet;
use crate::sim::pathfinding::PathGrid;
use crate::sim::pathfinding::terrain_cost::TerrainCostGrid;
use crate::sim::world::Simulation;

// Re-use test helpers from the main production_tests module.
use super::tests::{
    basic_infantry_rules, basic_multi_queue_rules, build_catalog_rules, naval_production_rules,
    production_modifier_rules, queued_item_via, spawn_structure, water_terrain,
};

#[test]
fn build_catalog_exposes_sidebar_categories_and_required_houses() {
    let mut sim = Simulation::new();
    let rules = build_catalog_rules();
    // Pre-intern all rule type IDs so build_options_for_owner can resolve them.
    rules.intern_all_ids(&mut sim.interner);

    spawn_structure(&mut sim, 1, "Americans", "GAPILE", 10, 10);
    spawn_structure(&mut sim, 2, "Americans", "GAWEAP", 12, 10);
    spawn_structure(&mut sim, 3, "Americans", "GAAIRC", 14, 10);
    spawn_structure(&mut sim, 4, "Americans", "GACNST", 16, 10);
    spawn_structure(&mut sim, 5, "Alliance", "GAPILE", 20, 10);
    spawn_structure(&mut sim, 6, "Alliance", "GAWEAP", 22, 10);
    spawn_structure(&mut sim, 7, "Alliance", "GAAIRC", 24, 10);
    spawn_structure(&mut sim, 8, "Alliance", "GACNST", 26, 10);

    let americans = build_options_for_owner(&sim, &rules, "Americans");
    let alliance = build_options_for_owner(&sim, &rules, "Alliance");

    // Americans should see all items (they satisfy RequiredHouses for GTUR).
    assert_eq!(
        americans
            .iter()
            .map(|opt| opt.queue_category)
            .collect::<Vec<_>>(),
        vec![
            ProductionCategory::Building,
            ProductionCategory::Building,
            ProductionCategory::Defense,
            ProductionCategory::Infantry,
            ProductionCategory::Vehicle,
            ProductionCategory::Vehicle,
            ProductionCategory::Aircraft,
        ]
    );
    assert!(
        americans
            .iter()
            .filter(|opt| {
                matches!(
                    opt.queue_category,
                    ProductionCategory::Infantry
                        | ProductionCategory::Vehicle
                        | ProductionCategory::Aircraft
                )
            })
            .all(|opt| opt.enabled)
    );

    // Alliance should NOT see GTUR — it has RequiredHouses=Americans,
    // so it's hidden (not just greyed out) per RA2 behavior.
    assert!(
        alliance
            .iter()
            .find(|opt| opt.type_id == sim.interner.intern("GTUR"))
            .is_none(),
        "GTUR should be hidden for Alliance (RequiredHouses=Americans)"
    );

    let americans_yard = americans
        .iter()
        .find(|opt| opt.type_id == sim.interner.intern("GACNST"))
        .expect("construction yard should be listed");
    assert!(americans_yard.enabled);
    assert_eq!(americans_yard.reason, None);

    let americans_turret = americans
        .iter()
        .find(|opt| opt.type_id == sim.interner.intern("GTUR"))
        .expect("defense should be listed for Americans");
    assert!(americans_turret.enabled);
    assert_eq!(americans_turret.reason, None);
}

#[test]
fn queue_view_uses_owner_power_modifier() {
    let mut sim = Simulation::new();
    let rules = production_modifier_rules();

    spawn_structure(&mut sim, 1, "Americans", "GAPILE", 10, 10);
    spawn_structure(&mut sim, 2, "Soviet", "NAHAND", 20, 20);
    spawn_structure(&mut sim, 3, "Soviet", "GAPOWR", 22, 20);

    // Populate cached power states so the speed multiplier sees the deficit.
    crate::sim::power_system::tick_power_states(
        &mut sim.power_states,
        &mut sim.entities,
        &rules,
        16,
        &sim.interner,
    );

    let americans_id = sim.interner.intern("Americans");
    let soviet_id = sim.interner.intern("Soviet");
    let qi_am = queued_item_via(
        &mut sim.interner,
        "Americans",
        "E1",
        ProductionCategory::Infantry,
        900,
        900,
    );
    let qi_so = queued_item_via(
        &mut sim.interner,
        "Soviet",
        "E1",
        ProductionCategory::Infantry,
        900,
        900,
    );
    sim.production.queues_by_owner.insert(
        americans_id,
        BTreeMap::from([(ProductionCategory::Infantry, VecDeque::from([qi_am]))]),
    );
    sim.production.queues_by_owner.insert(
        soviet_id,
        BTreeMap::from([(ProductionCategory::Infantry, VecDeque::from([qi_so]))]),
    );

    let americans = queue_view_for_owner(&sim, &rules, "Americans");
    let soviet = queue_view_for_owner(&sim, &rules, "Soviet");

    assert_eq!(americans[0].total_ms, 118_800);
    assert_eq!(soviet[0].total_ms, 59_400);
}

#[test]
fn matching_factory_bonus_is_category_specific() {
    let mut sim = Simulation::new();
    let rules = production_modifier_rules();

    spawn_structure(&mut sim, 1, "Americans", "GAPILE", 10, 10);
    spawn_structure(&mut sim, 2, "Americans", "GAPILE", 12, 10);
    spawn_structure(&mut sim, 3, "Americans", "GAWEAP", 14, 10);
    spawn_structure(&mut sim, 4, "Americans", "GAPOWR", 16, 10);

    let infantry_rate =
        super::effective_progress_rate_ppm_for_type(&sim, &rules, "Americans", "E1");
    let vehicle_rate =
        super::effective_progress_rate_ppm_for_type(&sim, &rules, "Americans", "MTNK");

    assert_eq!(infantry_rate, 1_250_000);
    assert_eq!(vehicle_rate, 1_000_000);
}

#[test]
fn base_build_frames_follow_ra2_cost_buildspeed_formula() {
    let rules = production_modifier_rules();
    let obj = rules.object("E1").expect("E1 should exist");

    assert_eq!(super::build_time_base_frames(&rules, obj), 900);
}

#[test]
fn wall_build_speed_coefficient_applies_after_factory_scaling() {
    let mut sim = Simulation::new();
    let ini = IniFile::from_str(
        "[General]\n\
             BuildSpeed=1.0\n\
             MultipleFactory=0.8\n\
             WallBuildSpeedCoefficient=0.5\n\
             [InfantryTypes]\n\
             [VehicleTypes]\n\
             [AircraftTypes]\n\
             [BuildingTypes]\n\
             0=GACNST\n\
             1=NACNST\n\
             2=GAWALL\n\
             [GACNST]\n\
             Factory=BuildingType\n\
             Owner=Americans\n\
             [NACNST]\n\
             Factory=BuildingType\n\
             Owner=Americans\n\
             [GAWALL]\n\
             Name=Wall\n\
             Cost=1000\n\
             Strength=100\n\
             Armor=wood\n\
             TechLevel=1\n\
             Owner=Americans\n\
             Wall=yes\n",
    );
    let rules = RuleSet::from_ini(&ini).expect("wall rules should parse");

    spawn_structure(&mut sim, 1, "Americans", "GACNST", 10, 10);
    spawn_structure(&mut sim, 2, "Americans", "NACNST", 12, 10);

    let wall = rules.object("GAWALL").expect("wall should exist");
    let base_frames = super::build_time_base_frames(&rules, wall);
    let total_frames = super::effective_time_to_build_frames_for_type(
        &sim,
        &rules,
        "Americans",
        "GAWALL",
        base_frames,
    );

    assert_eq!(base_frames, 900);
    assert_eq!(total_frames, 360);
}

#[test]
fn low_power_and_factory_bonus_apply_per_owner_and_category() {
    let mut sim = Simulation::new();
    let rules = production_modifier_rules();
    let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();

    spawn_structure(&mut sim, 1, "Americans", "GAPILE", 10, 10);
    spawn_structure(&mut sim, 2, "Americans", "GAPILE", 12, 10);
    spawn_structure(&mut sim, 3, "Americans", "GAWEAP", 14, 10);
    spawn_structure(&mut sim, 4, "Soviet", "NAHAND", 20, 20);
    spawn_structure(&mut sim, 5, "Soviet", "GAWEAP", 22, 20);
    spawn_structure(&mut sim, 6, "Soviet", "GAPOWR", 24, 20);

    // Populate cached power states so the speed multiplier sees the deficit.
    crate::sim::power_system::tick_power_states(
        &mut sim.power_states,
        &mut sim.entities,
        &rules,
        16,
        &sim.interner,
    );

    let americans_id = sim.interner.intern("Americans");
    let soviet_id = sim.interner.intern("Soviet");
    let qi_am = queued_item_via(
        &mut sim.interner,
        "Americans",
        "E1",
        ProductionCategory::Infantry,
        60_000,
        60_000,
    );
    let qi_so = queued_item_via(
        &mut sim.interner,
        "Soviet",
        "MTNK",
        ProductionCategory::Vehicle,
        60_000,
        60_000,
    );
    sim.production.queues_by_owner.insert(
        americans_id,
        BTreeMap::from([(ProductionCategory::Infantry, VecDeque::from([qi_am]))]),
    );
    sim.production.queues_by_owner.insert(
        soviet_id,
        BTreeMap::from([(ProductionCategory::Vehicle, VecDeque::from([qi_so]))]),
    );

    let _ = tick_production(&mut sim, &rules, &height_map, None, 1000);

    let americans_remaining = sim
        .production
        .queues_by_owner
        .get(&americans_id)
        .and_then(|queues| queues.get(&ProductionCategory::Infantry))
        .and_then(|queue| queue.front())
        .map(|item| item.remaining_base_frames)
        .expect("americans queue should still exist");
    let soviet_remaining = sim
        .production
        .queues_by_owner
        .get(&soviet_id)
        .and_then(|queues| queues.get(&ProductionCategory::Vehicle))
        .and_then(|queue| queue.front())
        .map(|item| item.remaining_base_frames)
        .expect("soviet queue should still exist");

    assert_eq!(americans_remaining, 59_991);
    assert_eq!(soviet_remaining, 59_985);
}

#[test]
fn naval_unit_rally_uses_water_pathing_after_spawn() {
    let mut sim = Simulation::new();
    let rules = naval_production_rules();
    let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();
    let terrain = water_terrain(32, 32);
    let grid = PathGrid::from_resolved_terrain(&terrain);
    sim.resolved_terrain = Some(terrain.clone());
    sim.terrain_costs.insert(
        SpeedType::Float,
        TerrainCostGrid::from_resolved_terrain(&terrain, SpeedType::Float),
    );
    spawn_structure(&mut sim, 1, "Americans", "GAYARD", 20, 20);
    let americans_key = sim.interner.intern("AMERICANS");
    let americans_display = sim.interner.intern("Americans");
    sim.houses.insert(
        americans_key,
        crate::sim::house_state::HouseState::new(
            americans_display,
            0,
            None,
            true,
            super::production_types::STARTING_CREDITS,
            10,
        ),
    );
    if let Some(h) = sim.houses.get_mut(&americans_key) {
        h.rally_point = Some((26, 21));
    }
    let qi_naval = queued_item_via(
        &mut sim.interner,
        "Americans",
        "DEST",
        ProductionCategory::Vehicle,
        100,
        0,
    );
    sim.production.queues_by_owner.insert(
        americans_display,
        BTreeMap::from([(ProductionCategory::Vehicle, VecDeque::from([qi_naval]))]),
    );

    let spawned = tick_production(&mut sim, &rules, &height_map, Some(&grid), 33);
    assert!(spawned, "completed naval production should spawn the unit");

    let ship = sim
        .entities
        .values()
        .find(|e| {
            sim.interner
                .resolve(e.type_ref)
                .eq_ignore_ascii_case("DEST")
        })
        .expect("spawned destroyer");
    assert!(
        ship.movement_target.is_some(),
        "spawned naval unit should receive a rally move over water"
    );
}

#[test]
fn build_options_dedupe_house_specific_sidebar_clone() {
    let mut sim = Simulation::new();
    let ini = IniFile::from_str(
        "[InfantryTypes]\n\
         [VehicleTypes]\n\
         [AircraftTypes]\n\
         [BuildingTypes]\n\
         0=GACNST\n\
         1=GAAIRC\n\
         2=AMRADR\n\
         [GACNST]\n\
         Name=Construction Yard\n\
         Cost=3000\n\
         Strength=1000\n\
         Armor=wood\n\
         TechLevel=1\n\
         Owner=Americans,Alliance\n\
         Image=GACNST\n\
         Factory=BuildingType\n\
         [GAAIRC]\n\
         Name=Airforce Command\n\
         Cost=1000\n\
         Strength=600\n\
         Armor=wood\n\
         TechLevel=1\n\
         Owner=Americans,Alliance,British,French,Germans,Koreans\n\
         Image=GAAIRC\n\
         BuildCat=Tech\n\
         [AMRADR]\n\
         Name=Airforce Command\n\
         Cost=1000\n\
         Strength=600\n\
         Armor=wood\n\
         TechLevel=1\n\
         Owner=Americans,Alliance,British,French,Germans,Koreans\n\
         RequiredHouses=Americans\n\
         Image=GAAIRC\n\
         BuildCat=Tech\n",
    );
    let rules = RuleSet::from_ini(&ini).expect("rules should parse");
    rules.intern_all_ids(&mut sim.interner);

    spawn_structure(&mut sim, 1, "Americans", "GACNST", 10, 10);

    let americans = build_options_for_owner(&sim, &rules, "Americans");
    let airforce: Vec<_> = americans
        .iter()
        .filter(|opt| opt.display_name == "Airforce Command")
        .collect();

    assert_eq!(
        airforce.len(),
        1,
        "sidebar should only show one Airforce Command"
    );
    assert_eq!(airforce[0].type_id, sim.interner.intern("AMRADR"));
}

#[test]
fn tick_production_advances_each_owner_queue() {
    let mut sim = Simulation::new();
    let rules = basic_infantry_rules();
    let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();

    spawn_structure(&mut sim, 1, "Americans", "GAPILE", 10, 10);
    spawn_structure(&mut sim, 2, "Soviet", "NAHAND", 20, 20);

    let americans_id = sim.interner.intern("Americans");
    let soviet_id = sim.interner.intern("Soviet");
    let qi_am = queued_item_via(
        &mut sim.interner,
        "Americans",
        "E1",
        ProductionCategory::Infantry,
        100,
        10,
    );
    let qi_so = queued_item_via(
        &mut sim.interner,
        "Soviet",
        "E1",
        ProductionCategory::Infantry,
        100,
        10,
    );
    sim.production.queues_by_owner.insert(
        americans_id,
        BTreeMap::from([(ProductionCategory::Infantry, VecDeque::from([qi_am]))]),
    );
    sim.production.queues_by_owner.insert(
        soviet_id,
        BTreeMap::from([(ProductionCategory::Infantry, VecDeque::from([qi_so]))]),
    );

    let spawned = tick_production(&mut sim, &rules, &height_map, None, 33);
    assert!(spawned, "At least one queue completion should spawn");
    assert!(
        sim.production.queues_by_owner.is_empty(),
        "Completed owner queues should be drained"
    );

    let americans = sim
        .entities
        .values()
        .filter(|e| {
            sim.interner
                .resolve(e.owner)
                .eq_ignore_ascii_case("Americans")
                && sim.interner.resolve(e.type_ref).eq_ignore_ascii_case("E1")
        })
        .count();
    let soviet = sim
        .entities
        .values()
        .filter(|e| {
            sim.interner.resolve(e.owner).eq_ignore_ascii_case("Soviet")
                && sim.interner.resolve(e.type_ref).eq_ignore_ascii_case("E1")
        })
        .count();
    assert_eq!(americans, 1);
    assert_eq!(soviet, 1);
}

#[test]
fn tick_production_advances_multiple_queue_categories_for_same_owner() {
    let mut sim = Simulation::new();
    let rules = basic_multi_queue_rules();
    let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();

    spawn_structure(&mut sim, 1, "Americans", "GAPILE", 10, 10);
    spawn_structure(&mut sim, 2, "Americans", "GAWEAP", 14, 10);

    let americans_id = sim.interner.intern("Americans");
    let qi_inf = queued_item_via(
        &mut sim.interner,
        "Americans",
        "E1",
        ProductionCategory::Infantry,
        100,
        10,
    );
    let qi_veh = queued_item_via(
        &mut sim.interner,
        "Americans",
        "MTNK",
        ProductionCategory::Vehicle,
        100,
        10,
    );
    sim.production.queues_by_owner.insert(
        americans_id,
        BTreeMap::from([
            (ProductionCategory::Infantry, VecDeque::from([qi_inf])),
            (ProductionCategory::Vehicle, VecDeque::from([qi_veh])),
        ]),
    );

    let spawned = tick_production(&mut sim, &rules, &height_map, None, 33);
    assert!(spawned);
    assert!(
        sim.production.queues_by_owner.is_empty(),
        "all completed category queues should be drained"
    );

    let infantry = sim
        .entities
        .values()
        .filter(|e| {
            sim.interner
                .resolve(e.owner)
                .eq_ignore_ascii_case("Americans")
                && sim.interner.resolve(e.type_ref).eq_ignore_ascii_case("E1")
        })
        .count();
    let vehicles = sim
        .entities
        .values()
        .filter(|e| {
            sim.interner
                .resolve(e.owner)
                .eq_ignore_ascii_case("Americans")
                && sim
                    .interner
                    .resolve(e.type_ref)
                    .eq_ignore_ascii_case("MTNK")
        })
        .count();
    assert_eq!(infantry, 1);
    assert_eq!(vehicles, 1);
}

#[test]
fn paused_queue_category_does_not_advance_while_other_category_does() {
    let mut sim = Simulation::new();
    let rules = basic_multi_queue_rules();
    let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();

    spawn_structure(&mut sim, 1, "Americans", "GAPILE", 10, 10);
    spawn_structure(&mut sim, 2, "Americans", "GAWEAP", 14, 10);

    let americans_id = sim.interner.intern("Americans");
    let qi_inf = queued_item_via(
        &mut sim.interner,
        "Americans",
        "E1",
        ProductionCategory::Infantry,
        1000,
        1000,
    );
    let qi_veh = queued_item_via(
        &mut sim.interner,
        "Americans",
        "MTNK",
        ProductionCategory::Vehicle,
        1000,
        1000,
    );
    sim.production.queues_by_owner.insert(
        americans_id,
        BTreeMap::from([
            (ProductionCategory::Infantry, VecDeque::from([qi_inf])),
            (ProductionCategory::Vehicle, VecDeque::from([qi_veh])),
        ]),
    );

    let paused =
        toggle_pause_for_owner_category(&mut sim, "Americans", ProductionCategory::Infantry);
    assert!(paused);

    let _ = tick_production(&mut sim, &rules, &height_map, None, 100);

    let infantry = sim
        .production
        .queues_by_owner
        .get(&americans_id)
        .and_then(|queues| queues.get(&ProductionCategory::Infantry))
        .and_then(|queue| queue.front())
        .expect("infantry queue should remain");
    let vehicle = sim
        .production
        .queues_by_owner
        .get(&americans_id)
        .and_then(|queues| queues.get(&ProductionCategory::Vehicle))
        .and_then(|queue| queue.front())
        .expect("vehicle queue should remain");

    assert_eq!(infantry.state, BuildQueueState::Paused);
    assert_eq!(infantry.remaining_base_frames, 1000);
    assert_eq!(vehicle.state, BuildQueueState::Building);
    assert_eq!(vehicle.remaining_base_frames, 899);
}

#[test]
fn cancel_by_type_removes_ready_building_and_refunds() {
    use super::cancel_by_type_for_owner;

    let mut sim = Simulation::new();
    let rules = build_catalog_rules();

    spawn_structure(&mut sim, 1, "Americans", "GACNST", 10, 10);

    // Place a building in the ready queue (simulating completion).
    let americans_id = sim.interner.intern("Americans");
    let garefn_id = sim.interner.intern("GAREFN");
    sim.production
        .ready_by_owner
        .entry(americans_id)
        .or_default()
        .push_back(garefn_id);

    let before_credits = credits_for_owner(&sim, "Americans");

    let cancelled = cancel_by_type_for_owner(&mut sim, &rules, "Americans", "GAREFN");
    assert!(cancelled, "should cancel ready building");

    // Ready queue should be empty now.
    let ready_count = sim
        .production
        .ready_by_owner
        .get(&americans_id)
        .map(|q| q.len())
        .unwrap_or(0);
    assert_eq!(ready_count, 0, "ready queue should be empty after cancel");

    // Cost should be refunded.
    let after_credits = credits_for_owner(&sim, "Americans");
    let refund = rules.object("GAREFN").map(|o| o.cost).unwrap_or(0);
    assert!(refund > 0, "GAREFN should have a cost");
    assert_eq!(after_credits, before_credits + refund);
}

#[test]
fn cancel_by_type_prefers_build_queue_over_ready_queue() {
    use super::cancel_by_type_for_owner;

    let mut sim = Simulation::new();
    let rules = build_catalog_rules();

    spawn_structure(&mut sim, 1, "Americans", "GACNST", 10, 10);

    // Put GAREFN in both the build queue AND ready queue.
    let americans_id = sim.interner.intern("Americans");
    let garefn_id = sim.interner.intern("GAREFN");
    let qi = queued_item_via(
        &mut sim.interner,
        "Americans",
        "GAREFN",
        ProductionCategory::Building,
        10000,
        5000,
    );
    sim.production
        .ready_by_owner
        .entry(americans_id)
        .or_default()
        .push_back(garefn_id);
    sim.production
        .queues_by_owner
        .entry(americans_id)
        .or_default()
        .entry(ProductionCategory::Building)
        .or_default()
        .push_back(qi);

    // First cancel should remove from build queue (not ready queue).
    let cancelled = cancel_by_type_for_owner(&mut sim, &rules, "Americans", "GAREFN");
    assert!(cancelled);

    // Ready queue should still have the item.
    let ready_count = sim
        .production
        .ready_by_owner
        .get(&americans_id)
        .map(|q| q.len())
        .unwrap_or(0);
    assert_eq!(ready_count, 1, "ready queue should still have the item");

    // Second cancel should remove from ready queue.
    let cancelled2 = cancel_by_type_for_owner(&mut sim, &rules, "Americans", "GAREFN");
    assert!(cancelled2);

    let ready_count2 = sim
        .production
        .ready_by_owner
        .get(&americans_id)
        .map(|q| q.len())
        .unwrap_or(0);
    assert_eq!(
        ready_count2, 0,
        "ready queue should be empty after second cancel"
    );
}
