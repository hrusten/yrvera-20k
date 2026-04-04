//! Production integration tests — end-to-end tests for build completion, unit spawning,
//! harvester auto-creation, and sell/undeploy flows through the full production pipeline.

use std::collections::BTreeMap;

use super::{
    BuildQueueItem, BuildQueueState, ProductionCategory, STARTING_CREDITS, credits_for_owner,
    find_spawn_cell_for_owner, is_matching_factory, seed_resource_nodes_from_overlays,
    structure_satisfies_prerequisite,
};
use crate::map::overlay::OverlayEntry;
use crate::map::resolved_terrain::{ResolvedTerrainCell, ResolvedTerrainGrid};
use crate::rules::ini_parser::IniFile;
use crate::rules::object_type::ObjectCategory;
use crate::rules::ruleset::RuleSet;
use crate::sim::components::Health;
use crate::sim::miner::{ResourceNode, ResourceType};
use crate::sim::pathfinding::PathGrid;
use crate::sim::world::Simulation;

pub(super) fn basic_infantry_rules() -> RuleSet {
    let ini = IniFile::from_str(
        "[InfantryTypes]\n\
             0=E1\n\
             [VehicleTypes]\n\
             [AircraftTypes]\n\
             [BuildingTypes]\n\
             0=GAPILE\n\
             1=NAHAND\n\
             [E1]\n\
             Name=GI\n\
             Cost=200\n\
             Strength=100\n\
             Armor=flak\n\
             Speed=4\n\
             Sight=5\n\
             TechLevel=1\n\
             Owner=Americans,Soviet\n\
             [GAPILE]\n\
             Factory=InfantryType\n\
             [NAHAND]\n\
             Factory=InfantryType\n",
    );
    RuleSet::from_ini(&ini).expect("basic infantry rules should parse")
}

pub(super) fn basic_multi_queue_rules() -> RuleSet {
    let ini = IniFile::from_str(
        "[InfantryTypes]\n\
             0=E1\n\
             [VehicleTypes]\n\
             0=MTNK\n\
             [AircraftTypes]\n\
             [BuildingTypes]\n\
             0=GAPILE\n\
             1=GAWEAP\n\
             [E1]\n\
             Name=GI\n\
             Cost=200\n\
             Strength=100\n\
             Armor=flak\n\
             Speed=4\n\
             Sight=5\n\
             TechLevel=1\n\
             Owner=Americans\n\
             [MTNK]\n\
             Name=Tank\n\
             Cost=700\n\
             Strength=300\n\
             Armor=heavy\n\
             Speed=6\n\
             Sight=6\n\
             TechLevel=1\n\
             Owner=Americans\n\
             [GAPILE]\n\
             Factory=InfantryType\n\
             [GAWEAP]\n\
             Factory=UnitType\n",
    );
    RuleSet::from_ini(&ini).expect("basic multi queue rules should parse")
}

pub(super) fn production_modifier_rules() -> RuleSet {
    let ini = IniFile::from_str(
        "[General]\n\
             BuildSpeed=1.0\n\
             MultipleFactory=0.8\n\
             LowPowerPenaltyModifier=1.0\n\
             MinLowPowerProductionSpeed=0.5\n\
             MaxLowPowerProductionSpeed=0.9\n\
             [InfantryTypes]\n\
             0=E1\n\
             [VehicleTypes]\n\
             0=MTNK\n\
             [AircraftTypes]\n\
             [BuildingTypes]\n\
             0=GAPILE\n\
             1=NAHAND\n\
             2=GAWEAP\n\
             3=GAPOWR\n\
             [E1]\n\
             Name=GI\n\
             Cost=1000\n\
             Strength=100\n\
             Armor=flak\n\
             Speed=4\n\
             Sight=5\n\
             TechLevel=1\n\
             Owner=Americans,Soviet\n\
             [MTNK]\n\
             Name=Tank\n\
             Cost=1000\n\
             Strength=300\n\
             Armor=heavy\n\
             Speed=6\n\
             Sight=6\n\
             TechLevel=1\n\
             Owner=Americans,Soviet\n\
             [GAPILE]\n\
             Power=-20\n\
             Factory=InfantryType\n\
             [NAHAND]\n\
             Power=-20\n\
             Factory=InfantryType\n\
             [GAWEAP]\n\
             Power=-20\n\
             Factory=UnitType\n\
             [GAPOWR]\n\
             Power=200\n",
    );
    RuleSet::from_ini(&ini).expect("production modifier rules should parse")
}

pub(super) fn build_catalog_rules() -> RuleSet {
    let ini = IniFile::from_str(
        "[InfantryTypes]\n\
             0=E1\n\
             [VehicleTypes]\n\
             0=MTNK\n\
             1=HARV\n\
             [AircraftTypes]\n\
             0=ORCA\n\
             [BuildingTypes]\n\
             0=GACNST\n\
             1=GTUR\n\
             2=GAREFN\n\
             3=GAPILE\n\
             4=GAWEAP\n\
             5=GAAIRC\n\
             [E1]\n\
             Name=GI\n\
             Cost=200\n\
             Strength=100\n\
             Armor=flak\n\
             Speed=4\n\
             Sight=5\n\
             TechLevel=1\n\
             Owner=Americans,Alliance\n\
             [MTNK]\n\
             Name=Tank\n\
             Cost=900\n\
             Strength=300\n\
             Armor=heavy\n\
             Speed=6\n\
             Sight=6\n\
             TechLevel=1\n\
             Owner=Americans,Alliance\n\
             [HARV]\n\
             Name=Harvester\n\
             Harvester=yes\n\
             Dock=GAREFN\n\
             Cost=1400\n\
             Strength=600\n\
             Armor=heavy\n\
             Speed=4\n\
             Sight=5\n\
             TechLevel=1\n\
             Owner=Americans,Alliance\n\
             [ORCA]\n\
             Name=Orca\n\
             Cost=1200\n\
             Strength=250\n\
             Armor=light\n\
             Speed=12\n\
             Sight=8\n\
             TechLevel=1\n\
             Owner=Americans,Alliance\n\
             Prerequisite=GAAIRC\n\
             [GACNST]\n\
             Name=Construction Yard\n\
             Cost=3000\n\
             Strength=1000\n\
             Armor=wood\n\
             TechLevel=1\n\
             Owner=Americans,Alliance\n\
             BuildCat=Tech\n\
             Factory=BuildingType\n\
             [GTUR]\n\
             Name=Guardian GI\n\
             Cost=600\n\
             Strength=400\n\
             Armor=wood\n\
             TechLevel=1\n\
             Owner=Americans,Alliance\n\
             RequiredHouses=Americans\n\
             BuildCat=Combat\n\
             [GAREFN]\n\
             Name=Ore Refinery\n\
             Cost=2000\n\
             Strength=900\n\
             Armor=wood\n\
             TechLevel=1\n\
             Owner=Americans,Alliance\n\
             BuildCat=Tech\n\
             Refinery=yes\n\
             FreeUnit=HARV\n\
             Foundation=3x3\n\
             [GAPILE]\n\
             TechLevel=-1\n\
             Factory=InfantryType\n\
             [GAWEAP]\n\
             TechLevel=-1\n\
             Factory=UnitType\n\
             [GAAIRC]\n\
             TechLevel=-1\n\
             Factory=AircraftType\n",
    );
    RuleSet::from_ini(&ini).expect("build catalog rules should parse")
}

pub(super) fn naval_production_rules() -> RuleSet {
    let ini = IniFile::from_str(
        "[InfantryTypes]\n\
         [VehicleTypes]\n\
         0=DEST\n\
         [AircraftTypes]\n\
         [BuildingTypes]\n\
         0=GAYARD\n\
         [DEST]\n\
         Name=Destroyer\n\
         Cost=1000\n\
         Strength=600\n\
         Armor=heavy\n\
         Speed=6\n\
         ROT=5\n\
         Naval=yes\n\
         SpeedType=Float\n\
         MovementZone=Water\n\
         Locomotor={2BEA74E1-7CCA-11d3-BE14-00104B62A16C}\n\
         TechLevel=1\n\
         Owner=Americans\n\
         [GAYARD]\n\
         Name=Naval Yard\n\
         Factory=UnitType\n\
         SpeedType=Float\n\
         WaterBound=yes\n\
         ExitCoord=512,256,0\n",
    );
    RuleSet::from_ini(&ini).expect("naval production rules should parse")
}

pub(super) fn water_terrain(width: u16, height: u16) -> ResolvedTerrainGrid {
    let mut cells = Vec::new();
    for y in 0..height {
        for x in 0..width {
            cells.push(ResolvedTerrainCell {
                rx: x,
                ry: y,
                source_tile_index: 0,
                source_sub_tile: 0,
                final_tile_index: 0,
                final_sub_tile: 0,
                level: 0,
                filled_clear: false,
                tileset_index: Some(0),
                land_type: 4,
                slope_type: 0,
                template_height: 0,
                render_offset_x: 0,
                render_offset_y: 0,
                terrain_class: crate::rules::terrain_rules::TerrainClass::Clear,
                speed_costs: crate::rules::terrain_rules::SpeedCostProfile::default(),
                is_water: true,
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
                zone_type: 4,
                base_ground_walk_blocked: false,
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

pub(super) fn placement_radius_rules() -> RuleSet {
    let ini = IniFile::from_str(
        "[InfantryTypes]\n\
             [VehicleTypes]\n\
             [AircraftTypes]\n\
             [BuildingTypes]\n\
             0=GACNST\n\
             1=GAPOWR\n\
             2=GAGAP\n\
             [GACNST]\n\
             Name=Construction Yard\n\
             Cost=3000\n\
             Strength=1000\n\
             Armor=wood\n\
             TechLevel=1\n\
             Owner=Americans\n\
             Foundation=2x2\n\
             BaseNormal=yes\n\
             [GAPOWR]\n\
             Name=Power Plant\n\
             Cost=800\n\
             Strength=750\n\
             Armor=wood\n\
             TechLevel=1\n\
             Owner=Americans\n\
             Foundation=2x2\n\
             Adjacent=0\n\
             [GAGAP]\n\
             Name=Gap Generator\n\
             Cost=1000\n\
             Strength=900\n\
             Armor=wood\n\
             TechLevel=1\n\
             Owner=Americans\n\
             Foundation=2x2\n\
             BaseNormal=no\n\
             Adjacent=0\n",
    );
    RuleSet::from_ini(&ini).expect("placement radius rules should parse")
}

pub(super) fn sell_rules() -> RuleSet {
    let ini = IniFile::from_str(
        "[InfantryTypes]\n\
             0=E1\n\
             1=E2\n\
             [VehicleTypes]\n\
             [AircraftTypes]\n\
             [BuildingTypes]\n\
             0=GAPOWR\n\
             1=NAHAND\n\
             [E1]\n\
             Name=GI\n\
             Cost=200\n\
             Strength=100\n\
             Armor=flak\n\
             Speed=4\n\
             Sight=5\n\
             TechLevel=1\n\
             Owner=Americans,Alliance\n\
             [E2]\n\
             Name=Conscript\n\
             Cost=100\n\
             Strength=100\n\
             Armor=flak\n\
             Speed=4\n\
             Sight=5\n\
             TechLevel=1\n\
             Owner=Russians,Soviet\n\
             [GAPOWR]\n\
             Name=Power Plant\n\
             Cost=800\n\
             Strength=750\n\
             Armor=wood\n\
             TechLevel=1\n\
             Owner=Americans,Alliance\n\
             Foundation=2x2\n\
             Crewed=yes\n\
             [NAHAND]\n\
             Name=Barracks\n\
             Cost=500\n\
             Strength=500\n\
             Armor=wood\n\
             TechLevel=1\n\
             Owner=Russians,Soviet\n\
             Foundation=2x2\n\
             Crewed=yes\n",
    );
    RuleSet::from_ini(&ini).expect("sell rules should parse")
}

/// Rules with Factory= keys for testing data-driven factory matching.
/// Includes standard RA2 factories plus custom modded ones (MYBARR, XAIRFLD).
pub(super) fn factory_rules() -> RuleSet {
    let ini = IniFile::from_str(
        "[InfantryTypes]\n\
             [VehicleTypes]\n\
             [AircraftTypes]\n\
             [BuildingTypes]\n\
             0=GACNST\n\
             1=GAPILE\n\
             2=GAWEAP\n\
             3=GAWEAT\n\
             4=GAAIRC\n\
             5=MYBARR\n\
             6=XAIRFLD\n\
             [GACNST]\n\
             Factory=BuildingType\n\
             [GAPILE]\n\
             Factory=InfantryType\n\
             [GAWEAP]\n\
             Factory=UnitType\n\
             Foundation=5x3\n\
             ExitCoord=512,256,0\n\
             [GAWEAT]\n\
             Factory=UnitType\n\
             Foundation=5x3\n\
             ExitCoord=512,256,0\n\
             [GAAIRC]\n\
             Factory=AircraftType\n\
             Foundation=3x2\n\
             ExitCoord=384,128,0\n\
             [MYBARR]\n\
             Factory=InfantryType\n\
             ExitCoord=-64,64,0\n\
             [XAIRFLD]\n\
             Factory=AircraftType\n\
             ExitCoord=384,128,0\n",
    );
    RuleSet::from_ini(&ini).expect("factory rules should parse")
}

/// Rules with [General] PrerequisiteXxx groups for testing data-driven
/// prerequisite alias resolution.
pub(super) fn prerequisite_group_rules() -> RuleSet {
    let ini = IniFile::from_str(
        "[General]\n\
         PrerequisitePower=GAPOWR,NAPOWR,NANRCT\n\
         PrerequisiteProc=GAREFN,NAREFN\n\
         PrerequisiteRadar=GAAIRC,NARADR\n\
         PrerequisiteTech=GATECH,NATECH\n\
         PrerequisiteBarracks=GAPILE,NAHAND\n\
         PrerequisiteFactory=GAWEAP,NAWEAP\n\
         [InfantryTypes]\n\
         [VehicleTypes]\n\
         [AircraftTypes]\n\
         [BuildingTypes]\n\
         0=GAPOWR\n\
         1=NAPOWR\n\
         2=NANRCT\n\
         3=GAREFN\n\
         4=NAREFN\n\
         5=GAAIRC\n\
         6=NARADR\n\
         7=GATECH\n\
         8=NATECH\n\
         9=GAPILE\n\
         10=NAHAND\n\
         11=GAWEAP\n\
         12=NAWEAP\n\
         [GAPOWR]\n\
         [NAPOWR]\n\
         [NANRCT]\n\
         [GAREFN]\n\
         [NAREFN]\n\
         [GAAIRC]\n\
         [NARADR]\n\
         [GATECH]\n\
         [NATECH]\n\
         [GAPILE]\n\
         [NAHAND]\n\
         [GAWEAP]\n\
         [NAWEAP]\n",
    );
    RuleSet::from_ini(&ini).expect("prerequisite group rules should parse")
}

pub(super) fn spawn_structure(
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
        crate::map::entities::EntityCategory::Structure,
        0,
        5,
        false,
    );
    sim.entities.insert(ge);
    // Register structure in occupancy grid (single cell — test structures
    // don't have foundation data, so just register the origin cell).
    sim.occupancy.add(
        rx,
        ry,
        sid,
        crate::sim::movement::locomotor::MovementLayer::Ground,
        None,
    );
    if sim.next_stable_entity_id <= sid {
        sim.next_stable_entity_id = sid + 1;
    }
}

/// Create a BuildQueueItem with IDs interned via the given interner so the IDs
/// are resolvable by sim code at runtime.
pub(super) fn queued_item_via(
    interner: &mut crate::sim::intern::StringInterner,
    owner: &str,
    type_id: &str,
    queue_category: ProductionCategory,
    total_base_frames: u32,
    remaining_base_frames: u32,
) -> BuildQueueItem {
    BuildQueueItem {
        owner: interner.intern(owner),
        type_id: interner.intern(type_id),
        queue_category,
        state: BuildQueueState::Queued,
        total_base_frames,
        remaining_base_frames,
        progress_carry: 0,
        enqueue_order: 1,
    }
}

#[test]
fn structure_satisfies_prerequisite_with_groups() {
    let rules = prerequisite_group_rules();
    // Direct match: GAWEAP satisfies GAWEAP.
    assert!(structure_satisfies_prerequisite(&rules, "GAWEAP", "GAWEAP"));
    // Alias match: NAWEAP is in PrerequisiteFactory list, which also maps to WARFACTORY.
    assert!(structure_satisfies_prerequisite(
        &rules,
        "NAWEAP",
        "WARFACTORY"
    ));
    assert!(structure_satisfies_prerequisite(
        &rules, "GAWEAP", "FACTORY"
    ));
    // Power alias.
    assert!(structure_satisfies_prerequisite(&rules, "GAPOWR", "POWER"));
    assert!(structure_satisfies_prerequisite(&rules, "NANRCT", "POWER"));
    // Barracks alias + TENT secondary alias.
    assert!(structure_satisfies_prerequisite(
        &rules, "GAPILE", "BARRACKS"
    ));
    assert!(structure_satisfies_prerequisite(&rules, "NAHAND", "TENT"));
    // Unknown alias: not in any group and not a direct match.
    assert!(!structure_satisfies_prerequisite(&rules, "GAPOWR", "RADAR"));
}

#[test]
fn custom_modded_prerequisite_group_recognized() {
    let ini = IniFile::from_str(
        "[General]\n\
         PrerequisitePower=MODPOWR,MODSOLR\n\
         [InfantryTypes]\n\
         [VehicleTypes]\n\
         [AircraftTypes]\n\
         [BuildingTypes]\n\
         0=MODPOWR\n\
         1=MODSOLR\n\
         [MODPOWR]\n\
         Name=Mod Power\n\
         [MODSOLR]\n\
         Name=Mod Solar\n",
    );
    let rules = RuleSet::from_ini(&ini).expect("modded rules should parse");
    assert!(structure_satisfies_prerequisite(&rules, "MODPOWR", "POWER"));
    assert!(structure_satisfies_prerequisite(&rules, "MODSOLR", "POWER"));
    assert!(!structure_satisfies_prerequisite(&rules, "GAPOWR", "POWER"));
}

#[test]
fn weat_factory_matches_vehicle_category() {
    let rules = factory_rules();
    assert!(is_matching_factory(
        &rules,
        "GAWEAT",
        ObjectCategory::Vehicle
    ));
    assert!(!is_matching_factory(
        &rules,
        "GAWEAT",
        ObjectCategory::Infantry
    ));
}

#[test]
fn custom_modded_factory_recognized_via_factory_key() {
    let rules = factory_rules();
    // MYBARR has Factory=InfantryType — should be recognized without hardcoding its name.
    assert!(is_matching_factory(
        &rules,
        "MYBARR",
        ObjectCategory::Infantry
    ));
    assert!(!is_matching_factory(
        &rules,
        "MYBARR",
        ObjectCategory::Vehicle
    ));
    // XAIRFLD has Factory=AircraftType.
    assert!(is_matching_factory(
        &rules,
        "XAIRFLD",
        ObjectCategory::Aircraft
    ));
}

#[test]
fn exit_coord_parsed_and_used_for_spawn() {
    let rules = factory_rules();
    // GAWEAP has ExitCoord=512,256,0 → 2 cells right, 1 cell down.
    let gaweap = rules.object("GAWEAP").expect("GAWEAP exists");
    assert_eq!(gaweap.exit_coord, Some((512, 256, 0)));

    // MYBARR has ExitCoord=-64,64,0 → rounds to (0, 0) cell offset.
    let mybarr = rules.object("MYBARR").expect("MYBARR exists");
    assert_eq!(mybarr.exit_coord, Some((-64, 64, 0)));

    // GACNST has no ExitCoord.
    let gacnst = rules.object("GACNST").expect("GACNST exists");
    assert_eq!(gacnst.exit_coord, None);

    // Spawn test: GAWEAP at (20,20), ExitCoord→primary cell (22,21).
    let mut sim = Simulation::new();
    spawn_structure(&mut sim, 1, "Americans", "GAWEAP", 20, 20);
    let spawn = find_spawn_cell_for_owner(
        &mut sim,
        &rules,
        "Americans",
        ObjectCategory::Vehicle,
        None,
        false,
    )
    .expect("should find spawn cell");
    assert_eq!(
        spawn,
        (22, 21),
        "primary exit cell from ExitCoord=512,256,0"
    );
}

#[test]
fn naval_factory_spawn_uses_water_exit_cells() {
    let rules = naval_production_rules();
    let mut sim = Simulation::new();
    let terrain = water_terrain(32, 32);
    let grid = PathGrid::from_resolved_terrain(&terrain);
    sim.resolved_terrain = Some(terrain);

    spawn_structure(&mut sim, 1, "Americans", "GAYARD", 20, 20);
    let spawn = find_spawn_cell_for_owner(
        &mut sim,
        &rules,
        "Americans",
        ObjectCategory::Vehicle,
        Some(&grid),
        true,
    )
    .expect("naval factory should find a water exit cell");

    assert_eq!(spawn, (22, 21), "naval yard should use its water exit cell");
}

#[test]
fn custom_exit_coord_modded_factory() {
    let ini = IniFile::from_str(
        "[InfantryTypes]\n\
         [VehicleTypes]\n\
         [AircraftTypes]\n\
         [BuildingTypes]\n\
         0=MODFACT\n\
         [MODFACT]\n\
         Factory=UnitType\n\
         ExitCoord=768,512,0\n",
    );
    let rules = RuleSet::from_ini(&ini).expect("modded rules should parse");
    let modfact = rules.object("MODFACT").expect("MODFACT exists");
    // 768/256=3 cells right, 512/256=2 cells down.
    assert_eq!(modfact.exit_coord, Some((768, 512, 0)));

    let mut sim = Simulation::new();
    spawn_structure(&mut sim, 1, "Americans", "MODFACT", 10, 10);
    let spawn = find_spawn_cell_for_owner(
        &mut sim,
        &rules,
        "Americans",
        ObjectCategory::Vehicle,
        None,
        false,
    )
    .expect("should find spawn cell");
    assert_eq!(
        spawn,
        (13, 12),
        "exit at (10+3, 10+2) from ExitCoord=768,512,0"
    );
}

#[test]
fn seed_resource_nodes_from_overlay_detects_ore_and_gems() {
    let mut sim = Simulation::new();
    let overlays = vec![
        OverlayEntry {
            rx: 5,
            ry: 6,
            overlay_id: 1,
            frame: 3,
        },
        OverlayEntry {
            rx: 7,
            ry: 9,
            overlay_id: 2,
            frame: 11,
        },
        OverlayEntry {
            rx: 2,
            ry: 2,
            overlay_id: 3,
            frame: 4,
        },
    ];
    let names: BTreeMap<u8, String> = BTreeMap::from([
        (1, "TIB01".to_string()),
        (2, "GEM01".to_string()),
        (3, "GAWALL".to_string()),
    ]);

    let added = seed_resource_nodes_from_overlays(&mut sim, &overlays, &names);
    assert_eq!(added, 2);
    let ore_node = sim.production.resource_nodes.get(&(5, 6)).unwrap();
    assert_eq!(ore_node.remaining, 480);
    assert_eq!(ore_node.resource_type, ResourceType::Ore);
    let gem_node = sim.production.resource_nodes.get(&(7, 9)).unwrap();
    assert_eq!(gem_node.remaining, 2160);
    assert_eq!(gem_node.resource_type, ResourceType::Gem);
    assert!(sim.production.resource_nodes.get(&(2, 2)).is_none());
}

#[test]
fn harvester_moves_to_ore_and_back_with_path_grid() {
    let mut sim = Simulation::new();
    let rules = build_catalog_rules();
    let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();
    let grid = PathGrid::new(64, 64);

    let harvester_sid = sim
        .spawn_object("HARV", "Americans", 10, 10, 64, &rules, &height_map)
        .expect("spawn harvester");
    spawn_structure(&mut sim, 2, "Americans", "GAREFN", 8, 10);
    sim.production.resource_nodes.insert(
        (12, 10),
        ResourceNode {
            resource_type: ResourceType::Ore,
            remaining: 800,
        },
    );

    let before = credits_for_owner(&sim, "Americans");
    let mut moved = false;
    // Run enough ticks for a full harvest cycle: search → move to ore →
    // harvest bales → return to refinery → dock → unload. HarvestRate=37
    // ticks/bale, 40 bales = ~1500 ticks harvest + movement + dock time.
    for _ in 0..3000 {
        let _ = sim.advance_tick(&[], Some(&rules), &height_map, Some(&grid), 33);
        let pos = sim
            .entities
            .get(harvester_sid)
            .map(|e| (e.position.rx, e.position.ry))
            .expect("harvester position");
        if pos != (10, 10) {
            moved = true;
        }
    }

    assert!(
        moved,
        "harvester should physically move toward ore/refinery"
    );
    // With the new Miner-component system, credits are earned via bale
    // unloading (25 per ore bale) rather than the legacy flat-140 system.
    let earned = credits_for_owner(&sim, "Americans") - before;
    assert!(
        earned > 0,
        "harvester should have earned some credits, got {earned}"
    );
}

#[test]
fn owner_credits_are_isolated() {
    let mut sim = Simulation::new();
    *super::credits_entry_for_owner(&mut sim, "Americans") -= 750;
    assert_eq!(credits_for_owner(&sim, "Americans"), STARTING_CREDITS - 750);
    assert_eq!(credits_for_owner(&sim, "Soviet"), STARTING_CREDITS);
}
