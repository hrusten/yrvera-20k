//! Building placement tests — verifies foundation overlap detection, placement validity,
//! and per-owner placement pool management for the production system.

use std::collections::{BTreeMap, VecDeque};

use super::{
    BuildQueueItem, BuildQueueState, BuildingPlacementError, ProductionCategory,
    cancel_last_for_owner, credits_for_owner, cycle_active_producer_for_owner_category,
    find_spawn_cell_for_owner, place_ready_building, placement_preview_for_owner,
    producer_candidates_for_owner_category, ready_buildings_for_owner, sell_building,
    tick_production,
};
use crate::map::resolved_terrain::{RampDirection, ResolvedTerrainCell, ResolvedTerrainGrid};
use crate::rules::ini_parser::IniFile;
use crate::rules::object_type::ObjectCategory;
use crate::rules::ruleset::RuleSet;
use crate::rules::terrain_rules::{SpeedCostProfile, TerrainClass};
use crate::sim::components::Health;
use crate::sim::pathfinding::PathGrid;
use crate::sim::world::Simulation;

// Re-use test helpers from the main production_tests module.
use super::tests::{
    basic_multi_queue_rules, build_catalog_rules, factory_rules, placement_radius_rules,
    sell_rules, spawn_structure,
};

fn resolved_clear_grid_with_override(
    width: u16,
    height: u16,
    mut override_cell: impl FnMut(&mut ResolvedTerrainCell),
) -> ResolvedTerrainGrid {
    let mut cells = Vec::with_capacity((width as usize) * (height as usize));
    for ry in 0..height {
        for rx in 0..width {
            let mut cell = ResolvedTerrainCell {
                rx,
                ry,
                source_tile_index: 0,
                source_sub_tile: 0,
                final_tile_index: 0,
                final_sub_tile: 0,
                level: 0,
                filled_clear: false,
                tileset_index: Some(0),
                land_type: 0,
                slope_type: 0,
                template_height: 0,
                render_offset_x: 0,
                render_offset_y: 0,
                terrain_class: TerrainClass::Clear,
                speed_costs: SpeedCostProfile::default(),
                is_water: false,
                is_cliff_like: false,
                is_cliff_redraw: false,
                variant: 0,
                is_rough: false,
                is_road: false,
                has_ramp: false,
                canonical_ramp: None,
                ground_walk_blocked: false,
                terrain_object_blocks: false,
                overlay_blocks: false,
                base_build_blocked: false,
                build_blocked: false,
                has_bridge_deck: false,
                bridge_walkable: false,
                bridge_transition: false,
                bridge_deck_level: 0,
                bridge_layer: None,
                radar_left: [0, 0, 0],
                radar_right: [0, 0, 0],
            };
            override_cell(&mut cell);
            cells.push(cell);
        }
    }
    ResolvedTerrainGrid::from_cells(width, height, cells)
}

fn naval_yard_placement_rules() -> RuleSet {
    let ini = IniFile::from_str(
        "[InfantryTypes]\n\
         [VehicleTypes]\n\
         [AircraftTypes]\n\
         [BuildingTypes]\n\
         0=GACNST\n\
         1=GAYARD\n\
         [GACNST]\n\
         Strength=1000\n\
         Armor=wood\n\
         Foundation=2x2\n\
         BaseNormal=yes\n\
         Adjacent=12\n\
         [GAYARD]\n\
         Strength=1500\n\
         Armor=concrete\n\
         Foundation=1x1\n\
         WaterBound=yes\n\
         Naval=yes\n\
         Adjacent=12\n",
    );
    RuleSet::from_ini(&ini).expect("naval yard placement rules should parse")
}

#[test]
fn completed_building_moves_into_ready_placement_pool() {
    let mut sim = Simulation::new();
    let rules = build_catalog_rules();
    let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();

    spawn_structure(&mut sim, 1, "Americans", "GACNST", 10, 10);
    let qi = super::tests::queued_item_via(
        &mut sim.interner,
        "Americans",
        "GACNST",
        ProductionCategory::Building,
        100,
        10,
    );
    let americans = sim.interner.intern("Americans");
    let gacnst = sim.interner.intern("GACNST");
    sim.production.queues_by_owner.insert(
        americans,
        BTreeMap::from([(ProductionCategory::Building, VecDeque::from([qi]))]),
    );

    // tick_ms must be large enough to complete 10 remaining base frames in one
    // tick: 10 frames × 66 ms/frame = 660 ms minimum at 1× production rate.
    let spawned = tick_production(&mut sim, &rules, &height_map, None, 700);
    assert!(!spawned, "completed building should wait for placement");
    assert!(sim.production.queues_by_owner.is_empty());
    assert_eq!(
        ready_buildings_for_owner(&sim, &rules, "Americans")
            .into_iter()
            .map(|item| item.type_id)
            .collect::<Vec<_>>(),
        vec![gacnst]
    );
}

#[test]
fn place_ready_building_spawns_and_consumes_ready_item() {
    let mut sim = Simulation::new();
    let rules = build_catalog_rules();
    let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();
    let grid = PathGrid::new(64, 64);
    spawn_structure(&mut sim, 1, "Americans", "GACNST", 18, 18);

    let americans = sim.interner.intern("Americans");
    let gacnst = sim.interner.intern("GACNST");
    sim.production
        .ready_by_owner
        .insert(americans, VecDeque::from([gacnst]));

    assert!(place_ready_building(
        &mut sim,
        &rules,
        "Americans",
        "GACNST",
        20,
        20,
        Some(&grid),
        &height_map,
    ));
    assert!(ready_buildings_for_owner(&sim, &rules, "Americans").is_empty());

    let structures = sim
        .entities
        .values()
        .filter(|e| {
            sim.interner
                .resolve(e.owner)
                .eq_ignore_ascii_case("Americans")
                && sim
                    .interner
                    .resolve(e.type_ref)
                    .eq_ignore_ascii_case("GACNST")
                && e.position.rx == 20
                && e.position.ry == 20
                && e.category == crate::map::entities::EntityCategory::Structure
        })
        .count();
    assert_eq!(structures, 1);
}

#[test]
fn refinery_placement_spawns_one_starter_harvester() {
    let mut sim = Simulation::new();
    let rules = build_catalog_rules();
    let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();
    let grid = PathGrid::new(64, 64);
    spawn_structure(&mut sim, 1, "Americans", "GACNST", 18, 18);
    let americans = sim.interner.intern("Americans");
    let garefn = sim.interner.intern("GAREFN");
    sim.production
        .ready_by_owner
        .insert(americans, VecDeque::from([garefn]));

    assert!(place_ready_building(
        &mut sim,
        &rules,
        "Americans",
        "GAREFN",
        20,
        20,
        Some(&grid),
        &height_map,
    ));

    let harvesters: Vec<(u16, u16)> = sim
        .entities
        .values()
        .filter(|e| {
            sim.interner
                .resolve(e.owner)
                .eq_ignore_ascii_case("Americans")
                && sim
                    .interner
                    .resolve(e.type_ref)
                    .eq_ignore_ascii_case("HARV")
                && e.category == crate::map::entities::EntityCategory::Unit
        })
        .map(|e| (e.position.rx, e.position.ry))
        .collect();
    assert_eq!(harvesters.len(), 1);
    let (harv_rx, harv_ry) = harvesters[0];
    assert!(
        !(20..=22).contains(&harv_rx) || !(20..=22).contains(&harv_ry),
        "starter harvester spawned inside refinery footprint at ({harv_rx},{harv_ry})"
    );
}

#[test]
fn modded_refinery_placement_uses_free_unit_from_rules() {
    let rules = RuleSet::from_ini(&IniFile::from_str(
        "[InfantryTypes]\n\
         [VehicleTypes]\n\
         0=MODHARV\n\
         [AircraftTypes]\n\
         [BuildingTypes]\n\
         0=GACNST\n\
         1=MODPROC\n\
         [GACNST]\n\
         Foundation=2x2\n\
         [MODPROC]\n\
         Refinery=yes\n\
         FreeUnit=MODHARV\n\
         Foundation=3x3\n\
         [MODHARV]\n\
         Harvester=yes\n\
         Dock=MODPROC\n\
         Speed=4\n",
    ))
    .expect("rules should parse");
    let mut sim = Simulation::new();
    let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();
    let grid = PathGrid::new(64, 64);

    spawn_structure(&mut sim, 1, "Americans", "GACNST", 18, 18);
    let americans = sim.interner.intern("Americans");
    let modproc = sim.interner.intern("MODPROC");
    sim.production
        .ready_by_owner
        .insert(americans, VecDeque::from([modproc]));

    assert!(place_ready_building(
        &mut sim,
        &rules,
        "Americans",
        "MODPROC",
        20,
        20,
        Some(&grid),
        &height_map,
    ));

    let harvesters = sim
        .entities
        .values()
        .filter(|e| {
            sim.interner
                .resolve(e.owner)
                .eq_ignore_ascii_case("Americans")
                && sim
                    .interner
                    .resolve(e.type_ref)
                    .eq_ignore_ascii_case("MODHARV")
                && e.category == crate::map::entities::EntityCategory::Unit
        })
        .count();
    assert_eq!(harvesters, 1);
}

#[test]
fn refinery_without_free_unit_spawns_nothing() {
    let rules = RuleSet::from_ini(&IniFile::from_str(
        "[InfantryTypes]\n\
         [VehicleTypes]\n\
         0=MODHARV\n\
         [AircraftTypes]\n\
         [BuildingTypes]\n\
         0=GACNST\n\
         1=MODPROC\n\
         [GACNST]\n\
         Foundation=2x2\n\
         [MODPROC]\n\
         Refinery=yes\n\
         Foundation=3x3\n\
         [MODHARV]\n\
         Harvester=yes\n\
         Dock=MODPROC\n\
         Speed=4\n",
    ))
    .expect("rules should parse");
    let mut sim = Simulation::new();
    let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();
    let grid = PathGrid::new(64, 64);

    spawn_structure(&mut sim, 1, "Americans", "GACNST", 18, 18);
    let americans = sim.interner.intern("Americans");
    let modproc = sim.interner.intern("MODPROC");
    sim.production
        .ready_by_owner
        .insert(americans, VecDeque::from([modproc]));

    assert!(place_ready_building(
        &mut sim,
        &rules,
        "Americans",
        "MODPROC",
        20,
        20,
        Some(&grid),
        &height_map,
    ));

    let harvesters = sim
        .entities
        .values()
        .filter(|e| {
            sim.interner
                .resolve(e.owner)
                .eq_ignore_ascii_case("Americans")
                && sim
                    .interner
                    .resolve(e.type_ref)
                    .eq_ignore_ascii_case("MODHARV")
                && e.category == crate::map::entities::EntityCategory::Unit
        })
        .count();
    assert_eq!(harvesters, 0);
}

#[test]
fn place_ready_building_rejects_blocked_or_overlapping_cells() {
    let mut sim = Simulation::new();
    let rules = build_catalog_rules();
    let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();
    let mut grid = PathGrid::new(64, 64);
    grid.set_blocked(31, 31, true);
    spawn_structure(&mut sim, 1, "Americans", "GACNST", 30, 30);
    spawn_structure(&mut sim, 2, "Americans", "GACNST", 40, 40);

    let americans = sim.interner.intern("Americans");
    let gacnst = sim.interner.intern("GACNST");
    sim.production
        .ready_by_owner
        .insert(americans, VecDeque::from([gacnst, gacnst]));

    assert!(!place_ready_building(
        &mut sim,
        &rules,
        "Americans",
        "GACNST",
        31,
        31,
        Some(&grid),
        &height_map,
    ));
    assert!(!place_ready_building(
        &mut sim,
        &rules,
        "Americans",
        "GACNST",
        40,
        40,
        Some(&grid),
        &height_map,
    ));
    assert_eq!(
        ready_buildings_for_owner(&sim, &rules, "Americans").len(),
        2,
        "invalid placement must not consume the ready building"
    );
}

#[test]
fn place_ready_building_requires_base_normal_provider_within_adjacent_range() {
    let rules = placement_radius_rules();
    let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();
    let grid = PathGrid::new(64, 64);

    let mut sim = Simulation::new();
    spawn_structure(&mut sim, 1, "Americans", "GACNST", 10, 10);
    let americans = sim.interner.intern("Americans");
    let gapowr = sim.interner.intern("GAPOWR");
    sim.production
        .ready_by_owner
        .insert(americans, VecDeque::from([gapowr]));

    assert!(place_ready_building(
        &mut sim,
        &rules,
        "Americans",
        "GAPOWR",
        12,
        10,
        Some(&grid),
        &height_map,
    ));

    let mut far_sim = Simulation::new();
    spawn_structure(&mut far_sim, 1, "Americans", "GACNST", 10, 10);
    let far_americans = far_sim.interner.intern("Americans");
    let far_gapowr = far_sim.interner.intern("GAPOWR");
    far_sim
        .production
        .ready_by_owner
        .insert(far_americans, VecDeque::from([far_gapowr]));
    // GACNST has Adjacent=6 (default), foundation 2x2 at (10,10).
    // Expanded zone: max_x = 10+2-1+7 = 18, so (20,10) is out of range.
    assert!(!place_ready_building(
        &mut far_sim,
        &rules,
        "Americans",
        "GAPOWR",
        20,
        10,
        Some(&grid),
        &height_map,
    ));
}

#[test]
fn base_normal_false_structures_do_not_extend_build_area() {
    let mut sim = Simulation::new();
    let rules = placement_radius_rules();
    let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();
    let grid = PathGrid::new(64, 64);

    spawn_structure(&mut sim, 1, "Americans", "GAGAP", 10, 10);
    let americans = sim.interner.intern("Americans");
    let gapowr = sim.interner.intern("GAPOWR");
    sim.production
        .ready_by_owner
        .insert(americans, VecDeque::from([gapowr]));

    assert!(!place_ready_building(
        &mut sim,
        &rules,
        "Americans",
        "GAPOWR",
        12,
        10,
        Some(&grid),
        &height_map,
    ));
}

#[test]
fn placement_preview_reports_out_of_build_area() {
    let mut sim = Simulation::new();
    let rules = placement_radius_rules();
    let grid = PathGrid::new(64, 64);

    spawn_structure(&mut sim, 1, "Americans", "GACNST", 10, 10);
    let americans = sim.interner.intern("Americans");
    let gapowr = sim.interner.intern("GAPOWR");
    sim.production
        .ready_by_owner
        .insert(americans, VecDeque::from([gapowr]));

    let preview = placement_preview_for_owner(
        &sim,
        &rules,
        "Americans",
        "GAPOWR",
        20,
        20,
        Some(&grid),
        &BTreeMap::new(),
    )
    .expect("preview should exist");
    assert!(!preview.valid);
    assert_eq!(preview.reason, Some(BuildingPlacementError::OutOfBuildArea));
}

#[test]
fn placement_preview_reports_blocked_terrain() {
    let mut sim = Simulation::new();
    let rules = placement_radius_rules();
    let mut grid = PathGrid::new(64, 64);
    grid.set_blocked(12, 10, true);

    spawn_structure(&mut sim, 1, "Americans", "GACNST", 10, 10);
    let americans = sim.interner.intern("Americans");
    let gapowr = sim.interner.intern("GAPOWR");
    sim.production
        .ready_by_owner
        .insert(americans, VecDeque::from([gapowr]));

    let preview = placement_preview_for_owner(
        &sim,
        &rules,
        "Americans",
        "GAPOWR",
        12,
        10,
        Some(&grid),
        &BTreeMap::new(),
    )
    .expect("preview should exist");
    assert!(!preview.valid);
    assert_eq!(preview.reason, Some(BuildingPlacementError::BlockedTerrain));
}

#[test]
fn place_ready_building_rejects_bridge_deck_cells() {
    let mut sim = Simulation::new();
    let rules = placement_radius_rules();
    let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();
    let grid = PathGrid::new(64, 64);

    spawn_structure(&mut sim, 1, "Americans", "GACNST", 10, 10);
    let americans = sim.interner.intern("Americans");
    let gapowr = sim.interner.intern("GAPOWR");
    sim.production
        .ready_by_owner
        .insert(americans, VecDeque::from([gapowr]));
    sim.resolved_terrain = Some(resolved_clear_grid_with_override(64, 64, |cell| {
        if cell.rx == 12 && cell.ry == 10 {
            cell.build_blocked = true;
            cell.has_bridge_deck = true;
            cell.bridge_walkable = true;
            cell.bridge_transition = true;
            cell.bridge_deck_level = 3;
        }
    }));

    assert!(!place_ready_building(
        &mut sim,
        &rules,
        "Americans",
        "GAPOWR",
        12,
        10,
        Some(&grid),
        &height_map,
    ));

    let preview = placement_preview_for_owner(
        &sim,
        &rules,
        "Americans",
        "GAPOWR",
        12,
        10,
        Some(&grid),
        &height_map,
    )
    .expect("preview should exist");
    assert_eq!(preview.reason, Some(BuildingPlacementError::BlockedTerrain));
}

#[test]
fn place_ready_building_rejects_canonical_ramp_cells() {
    let mut sim = Simulation::new();
    let rules = placement_radius_rules();
    let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();
    let grid = PathGrid::new(64, 64);

    spawn_structure(&mut sim, 1, "Americans", "GACNST", 10, 10);
    let americans = sim.interner.intern("Americans");
    let gapowr = sim.interner.intern("GAPOWR");
    sim.production
        .ready_by_owner
        .insert(americans, VecDeque::from([gapowr]));
    sim.resolved_terrain = Some(resolved_clear_grid_with_override(64, 64, |cell| {
        if cell.rx == 12 && cell.ry == 10 {
            cell.has_ramp = true;
            cell.canonical_ramp = Some(RampDirection::West);
            cell.slope_type = 1;
            cell.ground_walk_blocked = false;
            cell.build_blocked = true;
        }
    }));

    assert!(!place_ready_building(
        &mut sim,
        &rules,
        "Americans",
        "GAPOWR",
        12,
        10,
        Some(&grid),
        &height_map,
    ));

    let preview = placement_preview_for_owner(
        &sim,
        &rules,
        "Americans",
        "GAPOWR",
        12,
        10,
        Some(&grid),
        &height_map,
    )
    .expect("preview should exist");
    assert_eq!(preview.reason, Some(BuildingPlacementError::BlockedTerrain));
    assert!(
        sim.resolved_terrain
            .as_ref()
            .and_then(|terrain| terrain.cell(12, 10))
            .is_some_and(|cell| !cell.ground_walk_blocked && cell.build_blocked),
        "canonical ramp fixture should stay movement-passable while rejecting placement"
    );
}

#[test]
fn place_ready_building_rejects_destroyed_bridge_over_blocked_ground() {
    let mut sim = Simulation::new();
    let rules = placement_radius_rules();
    let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();
    let grid = PathGrid::new(64, 64);

    spawn_structure(&mut sim, 1, "Americans", "GACNST", 10, 10);
    let americans = sim.interner.intern("Americans");
    let gapowr = sim.interner.intern("GAPOWR");
    sim.production
        .ready_by_owner
        .insert(americans, VecDeque::from([gapowr]));
    let resolved = resolved_clear_grid_with_override(64, 64, |cell| {
        if cell.rx == 12 && cell.ry == 10 {
            cell.ground_walk_blocked = true;
            cell.is_water = true;
            cell.base_build_blocked = true;
            cell.build_blocked = true;
            cell.has_bridge_deck = true;
            cell.bridge_walkable = true;
            cell.bridge_transition = true;
            cell.bridge_deck_level = 3;
        }
    });
    sim.bridge_state = Some(
        crate::sim::bridge_state::BridgeRuntimeState::from_resolved_terrain(&resolved, true, 5),
    );
    sim.resolved_terrain = Some(resolved);
    if let Some(state) = sim.bridge_state.as_mut() {
        let _ = state.apply_damage(crate::sim::bridge_state::BridgeDamageEvent {
            rx: 12,
            ry: 10,
            damage: 5,
        });
    }

    assert!(!place_ready_building(
        &mut sim,
        &rules,
        "Americans",
        "GAPOWR",
        12,
        10,
        Some(&grid),
        &height_map,
    ));
}

#[test]
fn water_bound_building_rejects_beach_like_water_cells() {
    let mut sim = Simulation::new();
    let rules = naval_yard_placement_rules();
    let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();

    spawn_structure(&mut sim, 1, "Americans", "GACNST", 10, 10);
    let americans = sim.interner.intern("Americans");
    let gayard = sim.interner.intern("GAYARD");
    sim.production
        .ready_by_owner
        .insert(americans, VecDeque::from([gayard]));
    sim.resolved_terrain = Some(resolved_clear_grid_with_override(64, 64, |cell| {
        if cell.rx == 20 && cell.ry == 20 {
            cell.is_water = true;
            cell.land_type = 3; // Beach/shallow shore: amphibious OK, ships blocked.
            cell.terrain_class = TerrainClass::Water;
            cell.base_build_blocked = true;
            cell.build_blocked = true;
        }
    }));
    let grid =
        PathGrid::from_resolved_terrain(sim.resolved_terrain.as_ref().expect("resolved terrain"));

    assert!(!place_ready_building(
        &mut sim,
        &rules,
        "Americans",
        "GAYARD",
        20,
        20,
        Some(&grid),
        &height_map,
    ));

    let preview = placement_preview_for_owner(
        &sim,
        &rules,
        "Americans",
        "GAYARD",
        20,
        20,
        Some(&grid),
        &height_map,
    )
    .expect("preview should exist");
    assert_eq!(preview.reason, Some(BuildingPlacementError::BlockedTerrain));
}

#[test]
fn water_bound_building_accepts_true_ship_water_cells() {
    let mut sim = Simulation::new();
    let rules = naval_yard_placement_rules();
    let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();

    spawn_structure(&mut sim, 1, "Americans", "GACNST", 10, 10);
    let americans = sim.interner.intern("Americans");
    let gayard = sim.interner.intern("GAYARD");
    sim.production
        .ready_by_owner
        .insert(americans, VecDeque::from([gayard]));
    sim.resolved_terrain = Some(resolved_clear_grid_with_override(64, 64, |cell| {
        if cell.rx == 20 && cell.ry == 20 {
            cell.is_water = true;
            cell.land_type = 4; // Water
            cell.terrain_class = TerrainClass::Water;
            cell.base_build_blocked = true;
            cell.build_blocked = true;
        }
    }));
    let grid =
        PathGrid::from_resolved_terrain(sim.resolved_terrain.as_ref().expect("resolved terrain"));

    assert!(place_ready_building(
        &mut sim,
        &rules,
        "Americans",
        "GAYARD",
        20,
        20,
        Some(&grid),
        &height_map,
    ));
}

#[test]
fn producer_candidates_are_sorted_by_stable_id() {
    let mut sim = Simulation::new();
    let rules = factory_rules();

    spawn_structure(&mut sim, 9, "Americans", "GAWEAP", 20, 20);
    spawn_structure(&mut sim, 3, "Americans", "GAWEAP", 10, 10);
    spawn_structure(&mut sim, 5, "Americans", "GAWEAP", 15, 15);

    let candidates = producer_candidates_for_owner_category(
        &sim.entities,
        &rules,
        "Americans",
        ProductionCategory::Vehicle,
        true,
        &sim.interner,
    );
    let ids: Vec<u64> = candidates.into_iter().map(|entry| entry.0).collect();
    assert_eq!(ids, vec![3, 5, 9]);
}

#[test]
fn cycle_active_producer_rotates_matching_factories() {
    let mut sim = Simulation::new();
    let rules = factory_rules();

    spawn_structure(&mut sim, 3, "Americans", "GAWEAP", 10, 10);
    spawn_structure(&mut sim, 5, "Americans", "GAWEAP", 15, 15);
    spawn_structure(&mut sim, 9, "Americans", "GAWEAP", 20, 20);

    assert!(cycle_active_producer_for_owner_category(
        &mut sim,
        &rules,
        "Americans",
        ProductionCategory::Vehicle,
    ));
    assert_eq!(
        sim.production
            .active_producer_by_owner
            .get(&sim.interner.intern("Americans"))
            .and_then(|categories| categories.get(&ProductionCategory::Vehicle))
            .copied(),
        Some(3)
    );
    assert!(cycle_active_producer_for_owner_category(
        &mut sim,
        &rules,
        "Americans",
        ProductionCategory::Vehicle,
    ));
    assert_eq!(
        sim.production
            .active_producer_by_owner
            .get(&sim.interner.intern("Americans"))
            .and_then(|categories| categories.get(&ProductionCategory::Vehicle))
            .copied(),
        Some(5)
    );
}

#[test]
fn spawn_routing_falls_back_to_next_factory_when_first_exit_is_blocked() {
    let mut sim = Simulation::new();
    let rules = factory_rules();
    let mut grid = PathGrid::new(64, 64);

    spawn_structure(&mut sim, 1, "Americans", "GAWEAP", 10, 10);
    spawn_structure(&mut sim, 2, "Americans", "GAWEAP", 30, 30);

    for rx in 0..=22 {
        for ry in 0..=22 {
            grid.set_blocked(rx, ry, true);
        }
    }

    let spawn = find_spawn_cell_for_owner(
        &mut sim,
        &rules,
        "Americans",
        ObjectCategory::Vehicle,
        Some(&grid),
        false,
    )
    .expect("second factory should provide a valid exit");

    assert!(
        spawn.0 >= 31 && spawn.0 <= 33 && spawn.1 >= 30 && spawn.1 <= 32,
        "spawn should come from the second war factory, got {:?}",
        spawn
    );
}

#[test]
fn spawn_routing_prefers_active_producer_when_available() {
    let mut sim = Simulation::new();
    let rules = factory_rules();
    let grid = PathGrid::new(64, 64);

    spawn_structure(&mut sim, 3, "Americans", "GAWEAP", 10, 10);
    spawn_structure(&mut sim, 5, "Americans", "GAWEAP", 30, 30);
    let americans = sim.interner.intern("Americans");
    sim.production.active_producer_by_owner.insert(
        americans,
        BTreeMap::from([(ProductionCategory::Vehicle, 5)]),
    );

    let spawn = find_spawn_cell_for_owner(
        &mut sim,
        &rules,
        "Americans",
        ObjectCategory::Vehicle,
        Some(&grid),
        false,
    )
    .expect("active producer should provide a valid exit");

    assert!(
        spawn.0 >= 31 && spawn.0 <= 33 && spawn.1 >= 30 && spawn.1 <= 32,
        "spawn should prefer the active war factory, got {:?}",
        spawn
    );
}

#[test]
fn cancel_last_for_owner_cancels_latest_item_across_categories() {
    let mut sim = Simulation::new();
    let rules = basic_multi_queue_rules();

    *super::credits_entry_for_owner(&mut sim, "Americans") = 1000;
    let americans = sim.interner.intern("Americans");
    let e1 = sim.interner.intern("E1");
    let mtnk = sim.interner.intern("MTNK");
    sim.production.queues_by_owner.insert(
        americans,
        BTreeMap::from([
            (
                ProductionCategory::Infantry,
                VecDeque::from([BuildQueueItem {
                    owner: americans,
                    type_id: e1,
                    queue_category: ProductionCategory::Infantry,
                    state: BuildQueueState::Building,
                    total_base_frames: 100,
                    remaining_base_frames: 100,
                    progress_carry: 0,
                    enqueue_order: 1,
                }]),
            ),
            (
                ProductionCategory::Vehicle,
                VecDeque::from([BuildQueueItem {
                    owner: americans,
                    type_id: mtnk,
                    queue_category: ProductionCategory::Vehicle,
                    state: BuildQueueState::Building,
                    total_base_frames: 100,
                    remaining_base_frames: 100,
                    progress_carry: 0,
                    enqueue_order: 2,
                }]),
            ),
        ]),
    );

    let canceled = cancel_last_for_owner(&mut sim, &rules, "Americans");
    assert!(canceled);
    assert_eq!(credits_for_owner(&sim, "Americans"), 1700);

    let owner_queues = sim
        .production
        .queues_by_owner
        .get(&americans)
        .expect("owner queues should remain");
    assert!(owner_queues.contains_key(&ProductionCategory::Infantry));
    assert!(!owner_queues.contains_key(&ProductionCategory::Vehicle));
}

#[test]
fn sell_building_refunds_half_current_value_and_ejects_allied_infantry() {
    let mut sim = Simulation::new();
    let rules = sell_rules();
    *super::credits_entry_for_owner(&mut sim, "Americans") = 1000;

    // Use spawn_structure for dual-write, then reduce health for the test.
    spawn_structure(&mut sim, 1, "Americans", "GAPOWR", 20, 20);
    if let Some(ge) = sim.entities.get_mut(1) {
        ge.health = Health {
            current: 375,
            max: 750,
        };
    }

    assert!(sell_building(&mut sim, &rules, 1));
    assert_eq!(credits_for_owner(&sim, "Americans"), 1200);

    let survivors: Vec<(String, u16, u16)> = sim
        .entities
        .values()
        .filter(|e| {
            sim.interner
                .resolve(e.owner)
                .eq_ignore_ascii_case("Americans")
                && sim.interner.resolve(e.type_ref).eq_ignore_ascii_case("E1")
        })
        .map(|e| ("E1".to_string(), e.position.rx, e.position.ry))
        .collect();
    // RA2 formula: refund = 800 * 50% * 50% = 200, survivors = 200 / 500 = 0.
    // Cheap Allied buildings at half health don't eject survivors.
    assert_eq!(
        survivors.len(),
        0,
        "800-cost Allied building at half health: refund 200 / divisor 500 = 0 survivors"
    );
    assert!(
        !sim.entities.contains(1),
        "sold building should be removed from the store"
    );
}

#[test]
fn sell_building_uses_owner_appropriate_survivor_type_and_caps_count() {
    let mut sim = Simulation::new();
    let rules = sell_rules();
    // Soviet house: side_index=1 so the sell system picks E2 survivor type.
    let russians_key = sim.interner.intern("RUSSIANS");
    let russians_display = sim.interner.intern("Russians");
    sim.houses.insert(
        russians_key,
        crate::sim::house_state::HouseState::new(russians_display, 1, None, false, 1000, 10),
    );

    spawn_structure(&mut sim, 2, "Russians", "NAHAND", 30, 30);
    if let Some(ge) = sim.entities.get_mut(2) {
        ge.health = Health {
            current: 500,
            max: 500,
        };
    }

    assert!(sell_building(&mut sim, &rules, 2));
    assert_eq!(credits_for_owner(&sim, "Russians"), 1250);

    let conscripts = sim
        .entities
        .values()
        .filter(|e| {
            sim.interner
                .resolve(e.owner)
                .eq_ignore_ascii_case("Russians")
                && sim.interner.resolve(e.type_ref).eq_ignore_ascii_case("E2")
        })
        .count();
    // RA2 formula: refund = 500 * 50% * 100% = 250, survivors = 250 / 250 = 1.
    assert_eq!(
        conscripts, 1,
        "500-cost Soviet building at full health: refund 250 / divisor 250 = 1 survivor"
    );
}
