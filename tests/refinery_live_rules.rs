use std::collections::{BTreeMap, VecDeque};
use std::path::Path;

use vera20k::assets::asset_manager::AssetManager;
use vera20k::map::entities::EntityCategory;
use vera20k::rules::art_data::ArtRegistry;
use vera20k::rules::ini_parser::IniFile;
use vera20k::rules::ruleset::RuleSet;
use vera20k::sim::pathfinding::PathGrid;
use vera20k::sim::production::place_ready_building;
use vera20k::sim::world::Simulation;

fn ra2_dir() -> String {
    std::env::var("RA2_DIR")
        .unwrap_or_else(|_| panic!("Set RA2_DIR env var to your RA2/YR install directory"))
}

fn load_merged_rules(asset_manager: &AssetManager) -> RuleSet {
    let mut rules_ini = IniFile::from_bytes(
        &asset_manager
            .get("rules.ini")
            .expect("rules.ini should exist in MIX chain"),
    )
    .expect("rules.ini should parse");
    if let Some(rulesmd) = asset_manager.get("rulesmd.ini") {
        let patch = IniFile::from_bytes(&rulesmd).expect("rulesmd.ini should parse");
        rules_ini.merge(&patch);
    }

    let mut rules = RuleSet::from_ini(&rules_ini).expect("merged rules should build");

    let mut art_ini = IniFile::from_bytes(
        &asset_manager
            .get("art.ini")
            .expect("art.ini should exist in MIX chain"),
    )
    .expect("art.ini should parse");
    if let Some(artmd) = asset_manager.get("artmd.ini") {
        let patch = IniFile::from_bytes(&artmd).expect("artmd.ini should parse");
        art_ini.merge(&patch);
    }
    let art = ArtRegistry::from_ini(&art_ini);
    rules.merge_art_data(&art);
    rules
}

fn load_merged_rules_ini(asset_manager: &AssetManager) -> IniFile {
    let mut rules_ini = IniFile::from_bytes(
        &asset_manager
            .get("rules.ini")
            .expect("rules.ini should exist in MIX chain"),
    )
    .expect("rules.ini should parse");
    if let Some(rulesmd) = asset_manager.get("rulesmd.ini") {
        let patch = IniFile::from_bytes(&rulesmd).expect("rulesmd.ini should parse");
        rules_ini.merge(&patch);
    }
    rules_ini
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn real_garefn_resolves_chrono_miner() {
    let _ = env_logger::try_init();

    let ra2_dir_str = ra2_dir();
    let ra2_dir = Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        println!("SKIP: RA2 dir not found at {}", ra2_dir.display());
        return;
    }

    let asset_manager = AssetManager::new(ra2_dir).expect("AssetManager");
    let rules_ini = load_merged_rules_ini(&asset_manager);
    let rules = load_merged_rules(&asset_manager);

    let vehicle_types = rules_ini
        .section("VehicleTypes")
        .expect("VehicleTypes should exist");
    let cmin_index = (0..600_u32).find(|i| {
        vehicle_types
            .get(&i.to_string())
            .is_some_and(|value| value.eq_ignore_ascii_case("CMIN"))
    });
    assert!(
        rules_ini.section("CMIN").is_some(),
        "raw merged INI should contain [CMIN]"
    );
    assert_eq!(
        rules_ini
            .section("GAREFN")
            .and_then(|section| section.get("FreeUnit")),
        Some("CMIN"),
        "raw merged INI should point GAREFN FreeUnit at CMIN"
    );
    assert!(
        cmin_index.is_some(),
        "VehicleTypes should include a numeric entry for CMIN"
    );

    let cmin = rules
        .object_case_insensitive("CMIN")
        .expect("RuleSet should contain CMIN");

    assert!(rules.is_refinery_type("GAREFN"));
    assert_eq!(rules.refinery_free_unit("GAREFN"), Some("CMIN"));
    assert!(cmin.harvester);
    assert!(cmin.teleporter);
    assert!(rules.harvester_can_dock_at("CMIN", "GAREFN"));
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn real_garefn_placement_spawns_cmin() {
    let _ = env_logger::try_init();

    let ra2_dir_str = ra2_dir();
    let ra2_dir = Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        println!("SKIP: RA2 dir not found at {}", ra2_dir.display());
        return;
    }

    let asset_manager = AssetManager::new(ra2_dir).expect("AssetManager");
    let rules = load_merged_rules(&asset_manager);

    let mut sim = Simulation::new();
    let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();
    let grid = PathGrid::new(64, 64);

    sim.spawn_object("GACNST", "Americans", 18, 18, 0, &rules, &height_map)
        .expect("construction yard should spawn");
    sim.production.ready_by_owner.insert(
        vera20k::sim::intern::test_intern("Americans"),
        VecDeque::from([vera20k::sim::intern::test_intern("GAREFN")]),
    );

    assert!(place_ready_building(
        &mut sim,
        &rules,
        "Americans",
        "GAREFN",
        24,
        20,
        Some(&grid),
        &height_map,
    ));

    let americans_id = sim
        .interner
        .get("Americans")
        .expect("Americans should be interned");
    let cmin_id = sim.interner.get("CMIN").expect("CMIN should be interned");

    let cmins: Vec<(u16, u16)> = sim
        .entities
        .values()
        .filter(|e| e.owner == americans_id && e.category == EntityCategory::Unit)
        .filter(|e| e.type_ref == cmin_id)
        .map(|e| (e.position.rx, e.position.ry))
        .collect();
    assert_eq!(
        cmins.len(),
        1,
        "expected one free CMIN after GAREFN placement"
    );

    let (rx, ry) = cmins[0];
    assert!(
        !(24..=27).contains(&rx) || !(20..=22).contains(&ry),
        "free CMIN spawned inside refinery footprint at ({rx},{ry})"
    );
}
