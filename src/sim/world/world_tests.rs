//! Simulation integration tests — exercises the full tick pipeline: entity spawning,
//! movement commands, combat, bridge traversal, ship pathfinding, deploy/undeploy,
//! and multi-system interactions.

use std::collections::BTreeMap;

use super::*;
use crate::map::entities::{EntityCategory, MapEntity};
use crate::map::houses::HouseAllianceMap;
use crate::map::resolved_terrain::ResolvedTerrainGrid;
use crate::map::terrain;
use crate::rules::ini_parser::IniFile;
use crate::rules::ruleset::RuleSet;
use crate::sim::bridge_state::{BridgeDamageEvent, BridgeRuntimeState};
use crate::sim::combat::AttackTarget;
use crate::sim::command::{Command, CommandEnvelope};
use crate::sim::components::MovementTarget;
use crate::sim::movement::locomotor::MovementLayer;
use crate::sim::pathfinding::PathGrid;
use crate::util::fixed_math::{SIM_ZERO, SimFixed};

fn make_test_entity(type_id: &str, category: EntityCategory) -> MapEntity {
    MapEntity {
        owner: "Americans".to_string(),
        type_id: type_id.to_string(),
        health: 256,
        cell_x: 30,
        cell_y: 40,
        facing: 64,
        category,
        sub_cell: 0,
        veterancy: 0,
        high: false,
    }
}

fn empty_heights() -> BTreeMap<(u16, u16), u8> {
    BTreeMap::new()
}

/// Create a CommandEnvelope with a string owner, interning it via the sim's interner.
fn cmd_envelope(
    sim: &Simulation,
    owner: &str,
    execute_tick: u64,
    payload: Command,
) -> CommandEnvelope {
    let owner_id = sim
        .interner
        .get(owner)
        .unwrap_or_else(|| panic!("owner '{}' not interned", owner));
    CommandEnvelope::new(owner_id, execute_tick, payload)
}

/// Create a water terrain grid (all cells are water, land_type=4) for ship tests.
fn water_terrain(width: u16, height: u16) -> ResolvedTerrainGrid {
    water_terrain_with_land_type(width, height, 4, false)
}

fn water_terrain_with_land_type(
    width: u16,
    height: u16,
    land_type: u8,
    is_cliff_like: bool,
) -> ResolvedTerrainGrid {
    let mut cells = Vec::new();
    for y in 0..height {
        for x in 0..width {
            cells.push(crate::map::resolved_terrain::ResolvedTerrainCell {
                rx: x,
                ry: y,
                source_tile_index: 0,
                source_sub_tile: 0,
                final_tile_index: 0,
                final_sub_tile: 0,
                level: 0,
                filled_clear: false,
                tileset_index: Some(0),
                land_type,
                slope_type: 0,
                template_height: 0,
                render_offset_x: 0,
                render_offset_y: 0,
                terrain_class: crate::rules::terrain_rules::TerrainClass::Clear,
                speed_costs: crate::rules::terrain_rules::SpeedCostProfile::default(),
                is_water: true,
                is_cliff_like,
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
            });
        }
    }
    ResolvedTerrainGrid::from_cells(width, height, cells)
}

fn single_bridge_cell(rx: u16, ry: u16, deck_level: u8) -> ResolvedTerrainGrid {
    let mut cells = Vec::new();
    for y in 0..=ry {
        for x in 0..=rx {
            cells.push(crate::map::resolved_terrain::ResolvedTerrainCell {
                rx: x,
                ry: y,
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
                terrain_class: crate::rules::terrain_rules::TerrainClass::Clear,
                speed_costs: crate::rules::terrain_rules::SpeedCostProfile::default(),
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
                has_bridge_deck: x == rx && y == ry,
                bridge_walkable: x == rx && y == ry,
                bridge_transition: x == rx && y == ry,
                bridge_deck_level: if x == rx && y == ry { deck_level } else { 0 },
                bridge_layer: None,
                radar_left: [0, 0, 0],
                radar_right: [0, 0, 0],
            });
        }
    }
    ResolvedTerrainGrid::from_cells(rx + 1, ry + 1, cells)
}

fn bridge_cell_with_ground_block(
    rx: u16,
    ry: u16,
    deck_level: u8,
    ground_walk_blocked: bool,
    level: u8,
) -> ResolvedTerrainGrid {
    let mut terrain = single_bridge_cell(rx, ry, deck_level);
    let idx = terrain.index(rx, ry).expect("bridge index");
    let cell = &mut terrain.cells[idx];
    cell.level = level;
    cell.ground_walk_blocked = ground_walk_blocked;
    cell.is_water = ground_walk_blocked;
    cell.base_build_blocked = ground_walk_blocked;
    cell.build_blocked = true;
    terrain
}

fn alliance_map(pairs: &[(&str, &[&str])]) -> HouseAllianceMap {
    let mut map = HouseAllianceMap::default();
    for &(owner, allies) in pairs {
        let mut set = std::collections::BTreeSet::new();
        for ally in allies {
            set.insert(ally.trim().to_ascii_uppercase());
        }
        map.insert(owner.trim().to_ascii_uppercase(), set);
    }
    map
}

fn combat_test_rules() -> RuleSet {
    let ini: IniFile = IniFile::from_str(
        "[InfantryTypes]\n0=E1\n\n\
         [VehicleTypes]\n0=MTNK\n1=AMCV\n\n\
         [AircraftTypes]\n\n\
         [BuildingTypes]\n0=GACNST\n\n\
         [E1]\nStrength=125\nArmor=flak\nSpeed=4\nPrimary=M60\n\n\
         [MTNK]\nStrength=300\nArmor=heavy\nSpeed=6\nPrimary=105mm\n\n\
         [AMCV]\nStrength=450\nArmor=heavy\nSpeed=5\nPrimary=none\nDeploysInto=GACNST\n\n\
         [GACNST]\nStrength=1000\nArmor=wood\nFoundation=4x3\nUndeploysInto=AMCV\n\n\
         [M60]\nDamage=25\nROF=20\nRange=5\nWarhead=SA\n\n\
         [105mm]\nDamage=65\nROF=50\nRange=6\nWarhead=AP\n\n\
         [SA]\nVerses=100%,100%,100%,90%,70%,25%,100%,25%,25%,0%,0%\n\n\
         [AP]\nVerses=100%,100%,90%,75%,75%,75%,60%,30%,20%,0%,0%\n",
    );
    RuleSet::from_ini(&ini).expect("combat test rules should parse")
}

fn naval_bridge_test_rules() -> RuleSet {
    let ini: IniFile = IniFile::from_str(
        "[InfantryTypes]\n\n\
         [VehicleTypes]\n0=BOAT\n1=DRED\n\n\
         [AircraftTypes]\n\n\
         [BuildingTypes]\n\n\
         [BOAT]\nStrength=300\nArmor=heavy\nSpeed=6\nMovementZone=Water\nSpeedType=Float\nNaval=yes\n\n\
         [DRED]\nStrength=600\nArmor=heavy\nSpeed=5\nMovementZone=Water\nSpeedType=Float\nNaval=yes\nTooBigToFitUnderBridge=yes\n",
    );
    RuleSet::from_ini(&ini).expect("naval bridge test rules should parse")
}

fn real_ship_test_rules() -> RuleSet {
    let ini: IniFile = IniFile::from_str(
        "[InfantryTypes]\n\n\
         [VehicleTypes]\n0=DEST\n\n\
         [AircraftTypes]\n\n\
         [BuildingTypes]\n\n\
         [DEST]\nStrength=600\nArmor=heavy\nSpeed=6\nROT=5\nNaval=yes\nLocomotor={2BEA74E1-7CCA-11d3-BE14-00104B62A16C}\nMovementZone=Water\nSpeedType=Float\nTooBigToFitUnderBridge=yes\n",
    );
    RuleSet::from_ini(&ini).expect("real ship rules should parse")
}

fn teleport_command_test_rules() -> RuleSet {
    let ini: IniFile = IniFile::from_str(
        "[InfantryTypes]\n\n\
         [VehicleTypes]\n0=CMIN\n1=CHRONO\n\n\
         [AircraftTypes]\n\n\
         [BuildingTypes]\n0=GAREFN\n\n\
         [CMIN]\nStrength=400\nArmor=light\nSpeed=4\nHarvester=yes\nTeleporter=yes\nDock=GAREFN\n\n\
         [CHRONO]\nStrength=200\nArmor=light\nSpeed=5\nTeleporter=yes\n\n\
         [GAREFN]\nStrength=900\nArmor=wood\nFoundation=4x3\nRefinery=yes\n",
    );
    RuleSet::from_ini(&ini).expect("teleport command rules should parse")
}

#[test]
fn test_spawn_vehicle_has_voxel_marker() {
    let mut sim: Simulation = Simulation::new();
    let entities: Vec<MapEntity> = vec![make_test_entity("MTNK", EntityCategory::Unit)];
    let count: u32 = sim.spawn_from_map(&entities, None, &empty_heights());

    assert_eq!(count, 1);
    let voxel_count: usize = sim.entities.values().filter(|e| e.is_voxel).count();
    assert_eq!(voxel_count, 1, "Vehicle should have VoxelModel marker");
}

#[test]
fn test_spawn_infantry_has_sprite_marker() {
    let mut sim: Simulation = Simulation::new();
    let entities: Vec<MapEntity> = vec![make_test_entity("E1", EntityCategory::Infantry)];
    sim.spawn_from_map(&entities, None, &empty_heights());

    let sprite_count: usize = sim.entities.values().filter(|e| !e.is_voxel).count();
    assert_eq!(sprite_count, 1, "Infantry should have SpriteModel marker");
}

#[test]
fn test_spawn_sets_position_and_facing() {
    let mut sim: Simulation = Simulation::new();
    let entities: Vec<MapEntity> = vec![make_test_entity("HTNK", EntityCategory::Unit)];
    sim.spawn_from_map(&entities, None, &empty_heights());

    for e in sim.entities.values() {
        assert_eq!(e.position.rx, 30);
        assert_eq!(e.position.ry, 40);
        assert_eq!(e.facing, 64);
        assert_eq!(sim.interner.resolve(e.type_ref), "HTNK");
        // lepton_to_screen = CoordsToClient(cell_center) = (30*(30-40), 15*(30+40)+15) = (-300, 1065)
        assert!((e.position.screen_x - (-300.0)).abs() < 0.1);
        assert!((e.position.screen_y - 1065.0).abs() < 0.1);
    }
}

#[test]
fn test_spawn_from_map_high_unit_uses_bridge_layer_and_deck_level() {
    let mut sim = Simulation::new();
    let heights = empty_heights();
    let resolved = single_bridge_cell(5, 5, 3);
    let count = sim.spawn_from_map_with_resolved(
        &[MapEntity {
            owner: "Americans".to_string(),
            type_id: "MTNK".to_string(),
            health: 256,
            cell_x: 5,
            cell_y: 5,
            facing: 64,
            category: EntityCategory::Unit,
            sub_cell: 0,
            veterancy: 0,
            high: true,
        }],
        Some(&combat_test_rules()),
        &heights,
        Some(&resolved),
    );

    assert_eq!(count, 1);
    let e = sim.entities.get(1).expect("spawned entity");
    assert_eq!(e.position.z, 3);
    let bridge = e.bridge_occupancy.as_ref().expect("bridge occupancy");
    assert_eq!(bridge.deck_level, 3);
    assert!(e.on_bridge);
    let loco = e.locomotor.as_ref().expect("loco");
    assert_eq!(loco.layer, MovementLayer::Bridge);
}

#[test]
fn test_spawn_from_map_high_without_bridge_falls_back_to_ground() {
    let mut sim = Simulation::new();
    let heights = BTreeMap::from([((5, 5), 1)]);
    let resolved = ResolvedTerrainGrid::from_cells(
        6,
        6,
        (0..6u16)
            .flat_map(|ry| {
                (0..6u16).map(
                    move |rx| crate::map::resolved_terrain::ResolvedTerrainCell {
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
                        terrain_class: crate::rules::terrain_rules::TerrainClass::Clear,
                        speed_costs: crate::rules::terrain_rules::SpeedCostProfile::default(),
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
                    },
                )
            })
            .collect(),
    );
    sim.spawn_from_map_with_resolved(
        &[MapEntity {
            owner: "Americans".to_string(),
            type_id: "MTNK".to_string(),
            health: 256,
            cell_x: 5,
            cell_y: 5,
            facing: 64,
            category: EntityCategory::Unit,
            sub_cell: 0,
            veterancy: 0,
            high: true,
        }],
        Some(&combat_test_rules()),
        &heights,
        Some(&resolved),
    );
    let e = sim.entities.get(1).expect("spawned entity");
    assert_eq!(e.position.z, 1);
    assert!(e.bridge_occupancy.is_none());
    assert!(!e.on_bridge);
    let loco = e.locomotor.as_ref().expect("loco");
    assert_eq!(loco.layer, MovementLayer::Ground);
}

#[test]
fn test_bridge_damage_rebuilds_path_grid() {
    let mut sim = Simulation::new();
    let resolved = single_bridge_cell(2, 0, 2);
    sim.resolved_terrain = Some(resolved.clone());
    sim.bridge_state = Some(BridgeRuntimeState::from_resolved_terrain(
        &resolved, true, 20,
    ));
    // Build PathGrid before damage — bridge should be walkable.
    let grid_before = PathGrid::from_resolved_terrain_with_bridges(
        &resolved,
        sim.bridge_state.as_ref(),
    );
    assert!(grid_before.is_walkable_on_layer(2, 0, MovementLayer::Bridge));

    let changes = sim.apply_bridge_damage_events(&[BridgeDamageEvent {
        rx: 2,
        ry: 0,
        damage: 20,
    }]);
    assert_eq!(changes.len(), 1);
    let _ = sim.resolve_bridge_state_changes(&changes);
    // Rebuild PathGrid after damage — bridge should no longer be walkable.
    let grid_after = PathGrid::from_resolved_terrain_with_bridges(
        sim.resolved_terrain.as_ref().unwrap(),
        sim.bridge_state.as_ref(),
    );
    assert!(!grid_after.is_walkable_on_layer(2, 0, MovementLayer::Bridge));
}

#[test]
fn test_destroyed_bridge_snaps_unit_to_ground_when_ground_exists() {
    let mut sim = Simulation::new();
    let resolved = bridge_cell_with_ground_block(5, 5, 3, false, 1);
    sim.resolved_terrain = Some(resolved.clone());
    sim.bridge_state = Some(BridgeRuntimeState::from_resolved_terrain(
        &resolved, true, 15,
    ));

    sim.spawn_from_map_with_resolved(
        &[MapEntity {
            owner: "Americans".to_string(),
            type_id: "MTNK".to_string(),
            health: 256,
            cell_x: 5,
            cell_y: 5,
            facing: 64,
            category: EntityCategory::Unit,
            sub_cell: 0,
            veterancy: 0,
            high: true,
        }],
        Some(&combat_test_rules()),
        &BTreeMap::from([((5, 5), 1)]),
        Some(&resolved),
    );

    let changes = sim.apply_bridge_damage_events(&[BridgeDamageEvent {
        rx: 5,
        ry: 5,
        damage: 15,
    }]);
    assert_eq!(changes.len(), 1);
    let fallout = sim.resolve_bridge_state_changes(&changes);
    assert!(fallout.is_empty());

    let e = sim.entities.get(1).expect("surviving bridge unit");
    assert_eq!(e.position.z, 1);
    assert!(e.bridge_occupancy.is_none());
    assert!(!e.on_bridge);
    let loco = e.locomotor.as_ref().expect("locomotor");
    assert_eq!(loco.layer, MovementLayer::Ground);
    assert!(e.movement_target.is_none());
}

#[test]
fn test_destroyed_bridge_despawns_unit_over_blocked_ground() {
    let mut sim = Simulation::new();
    let resolved = bridge_cell_with_ground_block(5, 5, 3, true, 0);
    sim.resolved_terrain = Some(resolved.clone());
    sim.bridge_state = Some(BridgeRuntimeState::from_resolved_terrain(
        &resolved, true, 15,
    ));

    sim.spawn_from_map_with_resolved(
        &[MapEntity {
            owner: "Americans".to_string(),
            type_id: "MTNK".to_string(),
            health: 256,
            cell_x: 5,
            cell_y: 5,
            facing: 64,
            category: EntityCategory::Unit,
            sub_cell: 0,
            veterancy: 0,
            high: true,
        }],
        Some(&combat_test_rules()),
        &BTreeMap::new(),
        Some(&resolved),
    );

    let changes = sim.apply_bridge_damage_events(&[BridgeDamageEvent {
        rx: 5,
        ry: 5,
        damage: 15,
    }]);
    let fallout = sim.resolve_bridge_state_changes(&changes);
    assert_eq!(fallout, vec![1]);
    assert!(sim.entities.get(1).is_none());
}

#[test]
fn test_water_mover_lookahead_does_not_attach_bridge_occupancy_under_bridge() {
    let rules = naval_bridge_test_rules();
    let mut sim = Simulation::new();
    let resolved = bridge_cell_with_ground_block(1, 0, 3, true, 0);
    sim.resolved_terrain = Some(resolved.clone());
    sim.bridge_state = Some(BridgeRuntimeState::from_resolved_terrain(
        &resolved, true, 15,
    ));


    let boat_id = sim
        .spawn_object("BOAT", "Americans", 0, 0, 64, &rules, &BTreeMap::new())
        .expect("spawn boat");
    let boat = sim.entities.get_mut(boat_id).expect("boat entity");
    boat.movement_target = Some(MovementTarget {
        path: vec![(0, 0), (1, 0)],
        path_layers: vec![MovementLayer::Ground, MovementLayer::Ground],
        next_index: 1,
        speed: SimFixed::from_num(256),
        current_speed: SimFixed::from_num(256),
        move_dir_x: SimFixed::from_num(256),
        move_dir_y: SIM_ZERO,
        move_dir_len: SimFixed::from_num(256),
        ..Default::default()
    });

    let path_grid = PathGrid::new(2, 1);
    let _ = sim.advance_tick(&[], Some(&rules), &BTreeMap::new(), Some(&path_grid), 33);

    let boat = sim.entities.get(boat_id).expect("boat still exists");
    assert!(
        boat.bridge_occupancy.is_none(),
        "Ship under a bridge should stay on the water layer"
    );
    assert_eq!(boat.position.z, 0);
}

#[test]
fn test_too_big_ship_can_move_under_bridge_route() {
    let rules = naval_bridge_test_rules();
    let mut sim = Simulation::new();
    // Build a 2x1 water terrain where cell (1,0) has a bridge deck.
    // Water movers need land_type=4 (Water) for passability.
    let mut resolved = water_terrain(2, 1);
    let idx = resolved.index(1, 0).expect("bridge cell index");
    resolved.cells[idx].has_bridge_deck = true;
    resolved.cells[idx].bridge_walkable = true;
    resolved.cells[idx].bridge_transition = true;
    resolved.cells[idx].bridge_deck_level = 3;
    resolved.cells[idx].ground_walk_blocked = true;
    resolved.cells[idx].build_blocked = true;
    sim.resolved_terrain = Some(resolved.clone());
    sim.bridge_state = Some(BridgeRuntimeState::from_resolved_terrain(
        &resolved, true, 15,
    ));


    let ship_id = sim
        .spawn_object("DRED", "Americans", 0, 0, 64, &rules, &BTreeMap::new())
        .expect("spawn dreadnought");
    let ship = sim.entities.get_mut(ship_id).expect("ship entity");
    ship.movement_target = Some(MovementTarget {
        path: vec![(0, 0), (1, 0)],
        path_layers: vec![MovementLayer::Ground, MovementLayer::Ground],
        next_index: 1,
        speed: SimFixed::from_num(256),
        current_speed: SimFixed::from_num(256),
        move_dir_x: SimFixed::from_num(256),
        move_dir_y: SIM_ZERO,
        move_dir_len: SimFixed::from_num(256),
        ..Default::default()
    });

    // Use tick_ms=1000 so the ship crosses the cell boundary in 1 tick
    // (speed=256 * dt=1.0 = 256 leptons = 1 cell).
    let path_grid = PathGrid::new(2, 1);
    let _ = sim.advance_tick(&[], Some(&rules), &BTreeMap::new(), Some(&path_grid), 1000);

    let ship = sim.entities.get(ship_id).expect("ship still exists");
    assert!(
        ship.movement_target.is_none(),
        "Naval ships should finish a direct move under bridge structural cells in the experimental behavior"
    );
    assert_eq!((ship.position.rx, ship.position.ry), (1, 0));
}

#[test]
fn test_ship_turn_path_completes_without_drive_track_stall() {
    let rules = naval_bridge_test_rules();
    let mut sim = Simulation::new();
    // Water movers need resolved_terrain with water cells (land_type=4) for
    // the passability check in is_cell_passable_for_mover.
    sim.resolved_terrain = Some(water_terrain(3, 3));
    let boat_id = sim
        .spawn_object("BOAT", "Americans", 0, 0, 64, &rules, &BTreeMap::new())
        .expect("spawn boat");
    let boat = sim.entities.get_mut(boat_id).expect("boat entity");
    boat.movement_target = Some(MovementTarget {
        path: vec![(0, 0), (1, 0), (1, 1)],
        path_layers: vec![
            MovementLayer::Ground,
            MovementLayer::Ground,
            MovementLayer::Ground,
        ],
        next_index: 1,
        speed: SimFixed::from_num(1024),
        current_speed: SimFixed::from_num(1024),
        move_dir_x: SimFixed::from_num(256),
        move_dir_y: SIM_ZERO,
        move_dir_len: SimFixed::from_num(256),
        ..Default::default()
    });

    let path_grid = PathGrid::new(3, 3);
    for _ in 0..10 {
        let _ = sim.advance_tick(&[], Some(&rules), &BTreeMap::new(), Some(&path_grid), 100);
    }

    let boat = sim.entities.get(boat_id).expect("boat still exists");
    assert_eq!(
        (boat.position.rx, boat.position.ry),
        (1, 1),
        "ship should finish a simple turn path instead of stalling in place"
    );
    assert!(
        boat.movement_target.is_none(),
        "ship movement should complete after reaching the goal"
    );
}

#[test]
fn test_real_ship_locomotor_move_command_crosses_water_cells() {
    let rules = real_ship_test_rules();
    let mut sim = Simulation::new();
    let terrain = water_terrain(4, 4);
    let path_grid = PathGrid::from_resolved_terrain(&terrain);
    sim.resolved_terrain = Some(terrain.clone());

    sim.terrain_costs.insert(
        crate::rules::locomotor_type::SpeedType::Float,
        crate::sim::pathfinding::terrain_cost::TerrainCostGrid::from_resolved_terrain(
            &terrain,
            crate::rules::locomotor_type::SpeedType::Float,
        ),
    );

    let ship_id = sim
        .spawn_object("DEST", "Americans", 0, 0, 64, &rules, &BTreeMap::new())
        .expect("spawn destroyer");
    let cmd = cmd_envelope(
        &sim,
        "Americans",
        1,
        Command::Move {
            entity_id: ship_id,
            target_rx: 3,
            target_ry: 1,
            queue: false,
            group_id: None,
        },
    );

    let _ = sim.advance_tick(
        &[cmd],
        Some(&rules),
        &BTreeMap::new(),
        Some(&path_grid),
        100,
    );
    for _ in 0..80 {
        let _ = sim.advance_tick(&[], Some(&rules), &BTreeMap::new(), Some(&path_grid), 100);
    }

    let ship = sim.entities.get(ship_id).expect("ship still exists");
    assert_eq!(
        (ship.position.rx, ship.position.ry),
        (3, 1),
        "real Ship locomotor should complete a simple move command over water"
    );
    assert!(
        ship.movement_target.is_none(),
        "real Ship locomotor should finish its move command"
    );
}

#[test]
fn test_real_ship_locomotor_crosses_water_surface_cells_with_non_water_land_type() {
    let rules = real_ship_test_rules();
    let mut sim = Simulation::new();
    // Real maps contain water-surface tiles that keep is_water=true while carrying
    // shoreline/coast land_type values. Ships should still navigate them.
    let terrain = water_terrain_with_land_type(4, 4, 7, false);
    let path_grid = PathGrid::from_resolved_terrain(&terrain);
    sim.resolved_terrain = Some(terrain.clone());

    sim.terrain_costs.insert(
        crate::rules::locomotor_type::SpeedType::Float,
        crate::sim::pathfinding::terrain_cost::TerrainCostGrid::from_resolved_terrain(
            &terrain,
            crate::rules::locomotor_type::SpeedType::Float,
        ),
    );

    let ship_id = sim
        .spawn_object("DEST", "Americans", 0, 0, 64, &rules, &BTreeMap::new())
        .expect("spawn destroyer");
    let cmd = cmd_envelope(
        &sim,
        "Americans",
        1,
        Command::Move {
            entity_id: ship_id,
            target_rx: 3,
            target_ry: 1,
            queue: false,
            group_id: None,
        },
    );

    let _ = sim.advance_tick(
        &[cmd],
        Some(&rules),
        &BTreeMap::new(),
        Some(&path_grid),
        100,
    );
    for _ in 0..80 {
        let _ = sim.advance_tick(&[], Some(&rules), &BTreeMap::new(), Some(&path_grid), 100);
    }

    let ship = sim.entities.get(ship_id).expect("ship still exists");
    assert_eq!(
        (ship.position.rx, ship.position.ry),
        (3, 1),
        "real Ship locomotor should treat water-surface cells as navigable even when land_type is not the pure water column"
    );
    assert!(
        ship.movement_target.is_none(),
        "real Ship locomotor should finish its move command on water-surface cells"
    );
}

#[test]
fn test_real_ship_move_command_can_path_under_bridge_when_too_big() {
    let rules = real_ship_test_rules();
    let mut sim = Simulation::new();
    let mut terrain = water_terrain(5, 3);
    let bridge_idx = terrain.index(2, 1).expect("bridge cell index");
    terrain.cells[bridge_idx].bridge_deck_level = 1;
    terrain.cells[bridge_idx].bridge_walkable = true;
    terrain.cells[bridge_idx].bridge_transition = true;
    let path_grid = PathGrid::from_resolved_terrain(&terrain);
    sim.resolved_terrain = Some(terrain.clone());

    sim.terrain_costs.insert(
        crate::rules::locomotor_type::SpeedType::Float,
        crate::sim::pathfinding::terrain_cost::TerrainCostGrid::from_resolved_terrain(
            &terrain,
            crate::rules::locomotor_type::SpeedType::Float,
        ),
    );

    let ship_id = sim
        .spawn_object("DEST", "Americans", 0, 1, 64, &rules, &BTreeMap::new())
        .expect("spawn destroyer");
    let cmd = cmd_envelope(
        &sim,
        "Americans",
        1,
        Command::Move {
            entity_id: ship_id,
            target_rx: 4,
            target_ry: 1,
            queue: false,
            group_id: None,
        },
    );

    let _ = sim.advance_tick(
        &[cmd],
        Some(&rules),
        &BTreeMap::new(),
        Some(&path_grid),
        100,
    );
    let initial_path = sim
        .entities
        .get(ship_id)
        .and_then(|ship| ship.movement_target.as_ref())
        .map(|mt| mt.path.clone())
        .expect("ship should have an initial path");
    for _ in 0..120 {
        let _ = sim.advance_tick(&[], Some(&rules), &BTreeMap::new(), Some(&path_grid), 100);
    }

    let ship = sim.entities.get(ship_id).expect("ship still exists");
    assert_eq!(
        (ship.position.rx, ship.position.ry),
        (4, 1),
        "Naval ships should still complete move commands when the straight route passes under a bridge"
    );
    assert!(
        initial_path.contains(&(2, 1)),
        "planned path should be allowed to include under-bridge structural cells for naval movers"
    );
}

#[test]
fn test_spawn_multiple_entities() {
    let mut sim: Simulation = Simulation::new();
    let entities: Vec<MapEntity> = vec![
        make_test_entity("MTNK", EntityCategory::Unit),
        make_test_entity("HTNK", EntityCategory::Unit),
        make_test_entity("E1", EntityCategory::Infantry),
        make_test_entity("GAPOWR", EntityCategory::Structure),
    ];
    let count: u32 = sim.spawn_from_map(&entities, None, &empty_heights());
    assert_eq!(count, 4);

    let total: usize = sim.entities.values().count();
    assert_eq!(total, 4);
}

#[test]
fn test_empty_entities_spawns_nothing() {
    let mut sim: Simulation = Simulation::new();
    let count: u32 = sim.spawn_from_map(&[], None, &empty_heights());
    assert_eq!(count, 0);
    assert_eq!(sim.entities.values().count(), 0);
}

#[test]
fn test_stable_ids_are_assigned() {
    let mut sim: Simulation = Simulation::new();
    let entities: Vec<MapEntity> = vec![
        make_test_entity("MTNK", EntityCategory::Unit),
        make_test_entity("E1", EntityCategory::Infantry),
    ];
    sim.spawn_from_map(&entities, None, &empty_heights());

    let mut ids: Vec<u64> = sim.entities.values().map(|e| e.stable_id).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 2]);
}

#[test]
fn test_select_command_applies_snapshot_selection() {
    let mut sim: Simulation = Simulation::new();
    sim.spawn_from_map(
        &[
            make_test_entity("MTNK", EntityCategory::Unit),
            make_test_entity("E1", EntityCategory::Infantry),
        ],
        None,
        &empty_heights(),
    );

    let select = cmd_envelope(
        &sim,
        "Americans",
        1,
        Command::Select {
            entity_ids: vec![2],
            additive: false,
        },
    );
    let _ = sim.advance_tick(&[select], None, &empty_heights(), None, 33);

    assert!(!sim.entities.get(1).is_some_and(|e| e.selected));
    assert!(sim.entities.get(2).is_some_and(|e| e.selected));
}

#[test]
fn test_select_command_replaces_previous_selection() {
    let mut sim: Simulation = Simulation::new();
    sim.spawn_from_map(
        &[
            make_test_entity("MTNK", EntityCategory::Unit),
            make_test_entity("E1", EntityCategory::Infantry),
        ],
        None,
        &empty_heights(),
    );

    let cmd1 = cmd_envelope(
        &sim,
        "Americans",
        1,
        Command::Select {
            entity_ids: vec![1],
            additive: false,
        },
    );
    let _ = sim.advance_tick(&[cmd1], None, &empty_heights(), None, 33);

    let cmd2 = cmd_envelope(
        &sim,
        "Americans",
        2,
        Command::Select {
            entity_ids: vec![2],
            additive: true,
        },
    );
    let _ = sim.advance_tick(&[cmd2], None, &empty_heights(), None, 33);

    assert!(!sim.entities.get(1).is_some_and(|e| e.selected));
    assert!(sim.entities.get(2).is_some_and(|e| e.selected));
}

#[test]
fn test_select_command_deduplicates_and_sorts_ids() {
    let mut sim: Simulation = Simulation::new();
    sim.spawn_from_map(
        &[
            make_test_entity("MTNK", EntityCategory::Unit),
            make_test_entity("E1", EntityCategory::Infantry),
        ],
        None,
        &empty_heights(),
    );

    let select = cmd_envelope(
        &sim,
        "Americans",
        1,
        Command::Select {
            entity_ids: vec![2, 2, 1],
            additive: false,
        },
    );
    let _ = sim.advance_tick(&[select], None, &empty_heights(), None, 33);

    assert!(sim.entities.get(1).is_some_and(|e| e.selected));
    assert!(sim.entities.get(2).is_some_and(|e| e.selected));
}

#[test]
fn test_deploy_mcv_replaces_vehicle_with_conyard() {
    let mut sim = Simulation::new();
    let rules = combat_test_rules();
    let heights = empty_heights();
    let mcv = sim
        .spawn_object("AMCV", "Americans", 20, 22, 64, &rules, &heights)
        .expect("spawn MCV");
    if let Some(e) = sim.entities.get_mut(mcv) {
        e.selected = true;
    }

    let cmd = cmd_envelope(&sim, "Americans", 1, Command::DeployMcv { entity_id: mcv });
    let _ = sim.advance_tick(&[cmd], Some(&rules), &heights, None, 33);

    assert!(sim.entities.get(mcv).is_none(), "MCV should be removed");
    let gacnst_id = sim
        .interner
        .get("GACNST")
        .expect("GACNST should be interned");
    assert!(
        sim.entities
            .values()
            .any(|e| e.type_ref == gacnst_id && e.position.rx == 18 && e.position.ry == 21),
        "Construction yard should spawn with its foundation origin centered from the former MCV cell"
    );
}

#[test]
fn test_execute_tick_delay_blocks_early_execution() {
    let mut sim: Simulation = Simulation::new();
    sim.spawn_from_map(
        &[MapEntity {
            owner: "Americans".to_string(),
            type_id: "MTNK".to_string(),
            health: 256,
            cell_x: 2,
            cell_y: 2,
            facing: 64,
            category: EntityCategory::Unit,
            sub_cell: 0,
            veterancy: 0,
            high: false,
        }],
        None,
        &empty_heights(),
    );
    let grid = PathGrid::new(32, 32);
    let delayed = cmd_envelope(
        &sim,
        "Americans",
        3,
        Command::Move {
            entity_id: 1,
            target_rx: 8,
            target_ry: 2,

            queue: false,
            group_id: None,
        },
    );

    let _ = sim.advance_tick(&[delayed.clone()], None, &empty_heights(), Some(&grid), 33);
    assert!(
        sim.entities
            .get(1)
            .and_then(|e| e.movement_target.as_ref())
            .is_none()
    );

    let _ = sim.advance_tick(&[delayed.clone()], None, &empty_heights(), Some(&grid), 33);
    assert!(
        sim.entities
            .get(1)
            .and_then(|e| e.movement_target.as_ref())
            .is_none()
    );

    let _ = sim.advance_tick(&[delayed], None, &empty_heights(), Some(&grid), 33);
    assert!(
        sim.entities
            .get(1)
            .and_then(|e| e.movement_target.as_ref())
            .is_some()
    );
}

#[test]
fn test_move_queue_command_appends_waypoint() {
    let mut sim: Simulation = Simulation::new();
    sim.spawn_from_map(
        &[MapEntity {
            owner: "Americans".to_string(),
            type_id: "MTNK".to_string(),
            health: 256,
            cell_x: 2,
            cell_y: 2,
            facing: 64,
            category: EntityCategory::Unit,
            sub_cell: 0,
            veterancy: 0,
            high: false,
        }],
        None,
        &empty_heights(),
    );
    let grid = PathGrid::new(32, 32);
    let commands = vec![
        cmd_envelope(
            &sim,
            "Americans",
            1,
            Command::Move {
                entity_id: 1,
                target_rx: 8,
                target_ry: 2,
                queue: false,
                group_id: None,
            },
        ),
        cmd_envelope(
            &sim,
            "Americans",
            1,
            Command::Move {
                entity_id: 1,
                target_rx: 12,
                target_ry: 2,
                queue: true,
                group_id: None,
            },
        ),
    ];
    let _ = sim.advance_tick(&commands, None, &empty_heights(), Some(&grid), 33);

    let ge = sim
        .entities
        .get(1)
        .expect("entity 1 should exist in EntityStore");
    let movement = ge
        .movement_target
        .as_ref()
        .expect("movement target should be set");
    assert_eq!(movement.path.last().copied(), Some((12, 2)));
}

#[test]
fn test_stop_command_clears_move_and_attack_intent() {
    let mut sim: Simulation = Simulation::new();
    sim.spawn_from_map(
        &[MapEntity {
            owner: "Americans".to_string(),
            type_id: "MTNK".to_string(),
            health: 256,
            cell_x: 4,
            cell_y: 4,
            facing: 64,
            category: EntityCategory::Unit,
            sub_cell: 0,
            veterancy: 0,
            high: false,
        }],
        None,
        &empty_heights(),
    );

    if let Some(e) = sim.entities.get_mut(1) {
        e.movement_target = Some(MovementTarget {
            path: vec![(4, 4), (5, 4)],
            path_layers: vec![MovementLayer::Ground; 2],
            next_index: 1,
            speed: SimFixed::from_num(1024),
            move_dir_x: SimFixed::from_num(256),
            move_dir_y: SIM_ZERO,
            move_dir_len: SimFixed::from_num(256),
            ..Default::default()
        });
        e.attack_target = Some(AttackTarget::new(1));
    }

    let cmd = cmd_envelope(&sim, "Americans", 1, Command::Stop { entity_id: 1 });
    let _ = sim.advance_tick(&[cmd], None, &empty_heights(), None, 33);
    assert!(
        sim.entities.get(1).unwrap().movement_target.is_none(),
        "movement target should be cleared by Stop"
    );
    assert!(
        sim.entities.get(1).unwrap().attack_target.is_none(),
        "AttackTarget should be cleared by Stop command"
    );
}

#[test]
fn test_move_command_rejects_non_owned_entity() {
    let mut sim: Simulation = Simulation::new();
    sim.spawn_from_map(
        &[MapEntity {
            owner: "Americans".to_string(),
            type_id: "MTNK".to_string(),
            health: 256,
            cell_x: 2,
            cell_y: 2,
            facing: 64,
            category: EntityCategory::Unit,
            sub_cell: 0,
            veterancy: 0,
            high: false,
        }],
        None,
        &empty_heights(),
    );
    let grid = PathGrid::new(32, 32);
    sim.interner.intern("Russians"); // Ensure "Russians" is in sim's interner for cmd_envelope lookup.
    let cmd = cmd_envelope(
        &sim,
        "Russians",
        1,
        Command::Move {
            entity_id: 1,
            target_rx: 8,
            target_ry: 2,

            queue: false,
            group_id: None,
        },
    );

    let _ = sim.advance_tick(&[cmd], None, &empty_heights(), Some(&grid), 33);
    assert!(
        sim.entities
            .get(1)
            .is_some_and(|e| e.movement_target.is_none())
    );
}

#[test]
fn test_move_command_chrono_miner_uses_ground_path() {
    let rules = teleport_command_test_rules();
    let mut sim: Simulation = Simulation::new();
    let heights = empty_heights();
    let entity = sim
        .spawn_object("CMIN", "Americans", 2, 2, 64, &rules, &heights)
        .expect("spawn chrono miner");
    let grid = PathGrid::new(32, 32);
    let cmd = cmd_envelope(
        &sim,
        "Americans",
        1,
        Command::Move {
            entity_id: entity,
            target_rx: 8,
            target_ry: 2,
            queue: false,
            group_id: None,
        },
    );

    let _ = sim.advance_tick(&[cmd], Some(&rules), &heights, Some(&grid), 33);
    assert!(
        sim.entities
            .get(entity)
            .and_then(|e| e.movement_target.as_ref())
            .is_some(),
        "Chrono Miner should path like a ground unit on normal move orders"
    );
    assert!(
        sim.entities
            .get(entity)
            .and_then(|e| e.teleport_state.as_ref())
            .is_none(),
        "Chrono Miner should not enter teleport movement on a normal move order"
    );
}

#[test]
fn test_move_command_non_harvester_teleporter_uses_teleport() {
    let rules = teleport_command_test_rules();
    let mut sim: Simulation = Simulation::new();
    let heights = empty_heights();
    let entity = sim
        .spawn_object("CHRONO", "Americans", 2, 2, 64, &rules, &heights)
        .expect("spawn teleporter");
    let grid = PathGrid::new(32, 32);
    let cmd = cmd_envelope(
        &sim,
        "Americans",
        1,
        Command::Move {
            entity_id: entity,
            target_rx: 8,
            target_ry: 2,
            queue: false,
            group_id: None,
        },
    );

    let _ = sim.advance_tick(&[cmd], Some(&rules), &heights, Some(&grid), 33);
    assert!(
        sim.entities
            .get(entity)
            .and_then(|e| e.teleport_state.as_ref())
            .is_some(),
        "Non-harvester teleporters should still use teleport movement"
    );
    assert!(
        sim.entities
            .get(entity)
            .is_some_and(|e| e.movement_target.is_none()),
        "Teleport movement should not attach a ground MovementTarget"
    );
}

#[test]
fn test_attack_move_command_chrono_miner_uses_ground_path() {
    let rules = teleport_command_test_rules();
    let mut sim: Simulation = Simulation::new();
    let heights = empty_heights();
    let entity = sim
        .spawn_object("CMIN", "Americans", 2, 2, 64, &rules, &heights)
        .expect("spawn chrono miner");
    let grid = PathGrid::new(32, 32);
    let cmd = cmd_envelope(
        &sim,
        "Americans",
        1,
        Command::AttackMove {
            entity_id: entity,
            target_rx: 8,
            target_ry: 2,
            queue: false,
        },
    );

    let _ = sim.advance_tick(&[cmd], Some(&rules), &heights, Some(&grid), 33);
    assert!(
        sim.entities
            .get(entity)
            .and_then(|e| e.movement_target.as_ref())
            .is_some(),
        "Chrono Miner should path on attack-move instead of teleporting"
    );
    assert!(
        sim.entities
            .get(entity)
            .is_some_and(|e| e.order_intent.is_some()),
        "Attack-move should still set order intent"
    );
    assert!(
        sim.entities
            .get(entity)
            .and_then(|e| e.teleport_state.as_ref())
            .is_none(),
        "Chrono Miner should not enter teleport movement on attack-move"
    );
}

#[test]
fn test_attack_command_rejects_friendly_target() {
    let mut sim: Simulation = Simulation::new();
    sim.house_alliances = alliance_map(&[
        ("Americans", &["Americans", "British"]),
        ("British", &["Americans", "British"]),
    ]);
    sim.spawn_from_map(
        &[
            MapEntity {
                owner: "Americans".to_string(),
                type_id: "MTNK".to_string(),
                health: 256,
                cell_x: 2,
                cell_y: 2,
                facing: 64,
                category: EntityCategory::Unit,
                sub_cell: 0,
                veterancy: 0,
                high: false,
            },
            MapEntity {
                owner: "British".to_string(),
                type_id: "E1".to_string(),
                health: 256,
                cell_x: 4,
                cell_y: 2,
                facing: 64,
                category: EntityCategory::Infantry,
                sub_cell: 0,
                veterancy: 0,
                high: false,
            },
        ],
        None,
        &empty_heights(),
    );
    let cmd = cmd_envelope(
        &sim,
        "Americans",
        1,
        Command::Attack {
            attacker_id: 1,
            target_id: 2,
        },
    );

    let _ = sim.advance_tick(&[cmd], None, &empty_heights(), None, 33);
    assert!(
        sim.entities.get(1).unwrap().attack_target.is_none(),
        "Attack on same-owner target should not issue"
    );
}

#[test]
fn test_attack_move_auto_acquires_enemy() {
    let rules = combat_test_rules();
    let mut sim: Simulation = Simulation::new();
    sim.spawn_from_map(
        &[
            MapEntity {
                owner: "Americans".to_string(),
                type_id: "MTNK".to_string(),
                health: 256,
                cell_x: 2,
                cell_y: 2,
                facing: 64,
                category: EntityCategory::Unit,
                sub_cell: 0,
                veterancy: 0,
                high: false,
            },
            MapEntity {
                owner: "Russians".to_string(),
                type_id: "E1".to_string(),
                health: 256,
                cell_x: 4,
                cell_y: 2,
                facing: 64,
                category: EntityCategory::Infantry,
                sub_cell: 0,
                veterancy: 0,
                high: false,
            },
        ],
        None,
        &empty_heights(),
    );
    let grid = PathGrid::new(32, 32);
    let cmd = cmd_envelope(
        &sim,
        "Americans",
        1,
        Command::AttackMove {
            entity_id: 1,
            target_rx: 8,
            target_ry: 2,
            queue: false,
        },
    );

    let _ = sim.advance_tick(&[cmd], Some(&rules), &empty_heights(), Some(&grid), 100);
    let attack = sim
        .entities
        .get(1)
        .unwrap()
        .attack_target
        .as_ref()
        .expect("attack-move should acquire target");
    assert_eq!(attack.target, 2);
    assert!(sim.entities.get(1).unwrap().order_intent.is_some());
}

#[test]
fn test_attack_move_resumes_after_kill() {
    let rules = combat_test_rules();
    let mut sim: Simulation = Simulation::new();
    sim.spawn_from_map(
        &[
            MapEntity {
                owner: "Americans".to_string(),
                type_id: "MTNK".to_string(),
                health: 256,
                cell_x: 2,
                cell_y: 2,
                facing: 64,
                category: EntityCategory::Unit,
                sub_cell: 0,
                veterancy: 0,
                high: false,
            },
            MapEntity {
                owner: "Russians".to_string(),
                type_id: "E1".to_string(),
                health: 256,
                cell_x: 4,
                cell_y: 2,
                facing: 64,
                category: EntityCategory::Infantry,
                sub_cell: 0,
                veterancy: 0,
                high: false,
            },
        ],
        None,
        &empty_heights(),
    );
    if let Some(e) = sim.entities.get_mut(2) {
        e.health.current = 50;
        e.health.max = 50;
    }
    let grid = PathGrid::new(32, 32);
    let cmd = cmd_envelope(
        &sim,
        "Americans",
        1,
        Command::AttackMove {
            entity_id: 1,
            target_rx: 8,
            target_ry: 2,
            queue: false,
        },
    );

    let _ = sim.advance_tick(&[cmd], Some(&rules), &empty_heights(), Some(&grid), 100);
    assert!(
        sim.entities.get(1).unwrap().attack_target.is_none(),
        "target should die and attack should clear"
    );
    let ge = sim.entities.get(1).expect("entity 1 should exist");
    let movement = ge
        .movement_target
        .as_ref()
        .expect("attack-move should resume movement after kill");
    assert_eq!(movement.path.last().copied(), Some((8, 2)));
}

#[test]
fn test_guard_returns_to_anchor_when_displaced() {
    let rules = combat_test_rules();
    let mut sim: Simulation = Simulation::new();
    sim.spawn_from_map(
        &[MapEntity {
            owner: "Americans".to_string(),
            type_id: "MTNK".to_string(),
            health: 256,
            cell_x: 2,
            cell_y: 2,
            facing: 64,
            category: EntityCategory::Unit,
            sub_cell: 0,
            veterancy: 0,
            high: false,
        }],
        None,
        &empty_heights(),
    );
    let guard_cmd = cmd_envelope(
        &sim,
        "Americans",
        1,
        Command::Guard {
            entity_id: 1,
            target_id: None,
        },
    );
    let grid = PathGrid::new(32, 32);
    let _ = sim.advance_tick(
        &[guard_cmd],
        Some(&rules),
        &empty_heights(),
        Some(&grid),
        100,
    );

    if let Some(e) = sim.entities.get_mut(1) {
        e.position.rx = 5;
        e.position.ry = 2;
        let (sx, sy) = terrain::iso_to_screen(5, 2, e.position.z);
        e.position.screen_x = sx;
        e.position.screen_y = sy;
        e.movement_target = None;
        e.attack_target = None;
    }

    let _ = sim.advance_tick(&[], Some(&rules), &empty_heights(), Some(&grid), 100);
    let ge = sim.entities.get(1).expect("entity 1 should exist");
    let movement = ge
        .movement_target
        .as_ref()
        .expect("guard should re-path back to its anchor");
    assert_eq!(movement.path.last().copied(), Some((2, 2)));
}

#[test]
fn test_fog_revealed_persists_after_unit_moves_away() {
    let mut sim = Simulation::new();
    let (sx, sy) = terrain::iso_to_screen(1, 1, 0);
    use crate::sim::game_entity::GameEntity;
    let americans_id = sim.interner.intern("Americans");
    let e1_id = sim.interner.intern("E1");
    let ge = GameEntity::new(
        1,
        1,
        1,
        0,
        0,
        americans_id,
        crate::sim::components::Health {
            current: 100,
            max: 100,
        },
        e1_id,
        EntityCategory::Infantry,
        0,
        0,
        false,
    );
    sim.entities.insert(ge);

    let grid = PathGrid::new(8, 8);
    let americans = sim.interner.get("Americans").expect("Americans interned");
    let _ = sim.advance_tick(&[], None, &empty_heights(), Some(&grid), 33);
    assert!(sim.fog.is_cell_visible(americans, 1, 1));
    assert!(sim.fog.is_cell_revealed(americans, 1, 1));

    let _ = (sx, sy); // suppress unused warning
    if let Some(e) = sim.entities.get_mut(1) {
        e.position.rx = 2;
        e.position.ry = 1;
        let (nx, ny) = terrain::iso_to_screen(2, 1, 0);
        e.position.screen_x = nx;
        e.position.screen_y = ny;
    }
    let _ = sim.advance_tick(&[], None, &empty_heights(), Some(&grid), 33);
    assert!(!sim.fog.is_cell_visible(americans, 1, 1));
    assert!(sim.fog.is_cell_revealed(americans, 1, 1));
    assert!(sim.fog.is_cell_visible(americans, 2, 1));
}

#[test]
fn test_undeploy_conyard_spawns_mcv() {
    let mut sim = Simulation::new();
    let rules = combat_test_rules();
    let heights = empty_heights();

    // First deploy an MCV to get a ConYard.
    let mcv = sim
        .spawn_object("AMCV", "Americans", 20, 22, 64, &rules, &heights)
        .expect("spawn MCV");
    if let Some(e) = sim.entities.get_mut(mcv) {
        e.selected = true;
    }
    let deploy_cmd = cmd_envelope(&sim, "Americans", 1, Command::DeployMcv { entity_id: mcv });
    let _ = sim.advance_tick(&[deploy_cmd], Some(&rules), &heights, None, 33);

    // Find the ConYard that was spawned.
    let yard_id: u64 = sim
        .entities
        .values()
        .find(|e| sim.interner.resolve(e.type_ref) == "GACNST")
        .map(|e| e.stable_id)
        .expect("ConYard should exist after deploy");

    // Clear building_up so we can undeploy (can't undeploy during construction).
    if let Some(e) = sim.entities.get_mut(yard_id) {
        e.building_up = None;
        e.selected = true;
    }

    // Undeploy the ConYard — starts a 30-tick reverse build-up animation.
    let undeploy_cmd = cmd_envelope(
        &sim,
        "Americans",
        2,
        Command::UndeployBuilding { entity_id: yard_id },
    );
    let _ = sim.advance_tick(&[undeploy_cmd], Some(&rules), &heights, None, 33);

    // ConYard should still exist but have building_down set.
    assert!(
        sim.entities.get(yard_id).is_some(),
        "ConYard should still exist during undeploy animation"
    );
    assert!(
        sim.entities.get(yard_id).unwrap().building_down.is_some(),
        "ConYard should have building_down component"
    );

    // Advance through the 30-tick undeploy animation.
    for _tick in 3..33 {
        let _ = sim.advance_tick(&[], Some(&rules), &heights, None, 33);
    }

    // ConYard should be gone after animation completes.
    assert!(
        sim.entities.get(yard_id).is_none(),
        "ConYard should be removed after undeploy animation"
    );

    // MCV should be spawned at center of old foundation (4x3 → center offset 2,1).
    let amcv_id = sim.interner.get("AMCV").expect("AMCV should be interned");
    let mcvs: Vec<(u16, u16, bool)> = sim
        .entities
        .values()
        .filter(|e| e.type_ref == amcv_id)
        .map(|e| (e.position.rx, e.position.ry, e.selected))
        .collect();
    assert_eq!(mcvs.len(), 1, "Exactly one MCV should exist after undeploy");
    let (rx, ry, selected) = &mcvs[0];
    // Origin was (18, 21) from deploy, foundation 4x3, center = (18+2, 21+1) = (20, 22).
    assert_eq!(*rx, 20, "MCV should spawn at foundation center X");
    assert_eq!(*ry, 22, "MCV should spawn at foundation center Y");
    assert!(*selected, "MCV should inherit selection from ConYard");
}
