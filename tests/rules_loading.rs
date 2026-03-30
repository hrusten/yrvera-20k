//! Integration test: loads real rules.ini from retail RA2 MIX archives.
//!
//! Run with: cargo test --test rules_loading -- --nocapture

use std::path::Path;

use vera20k::assets::asset_manager::AssetManager;
use vera20k::rules::ini_parser::IniFile;
use vera20k::rules::object_type::ObjectCategory;
use vera20k::rules::ruleset::RuleSet;

fn ra2_dir() -> String {
    std::env::var("RA2_DIR")
        .unwrap_or_else(|_| panic!("Set RA2_DIR env var to your RA2/YR install directory"))
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn test_load_real_rules_ini() {
    let _ = env_logger::try_init();

    let ra2_dir_str = ra2_dir();
    let ra2_dir: &Path = Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        println!("SKIP: RA2 dir not found at {}", ra2_dir_str);
        return;
    }

    // Load asset manager and extract rules.ini.
    let asset_manager: AssetManager = AssetManager::new(ra2_dir).expect("AssetManager");
    let rules_data: Vec<u8> = asset_manager
        .get("rules.ini")
        .expect("rules.ini should exist in MIX chain");

    println!(
        "rules.ini: {} bytes ({:.0} KB)",
        rules_data.len(),
        rules_data.len() as f64 / 1024.0
    );

    // Parse INI file.
    let ini: IniFile = IniFile::from_bytes(&rules_data).expect("Valid UTF-8");
    println!("INI sections: {}", ini.section_count());

    // Build RuleSet.
    let rules: RuleSet = RuleSet::from_ini(&ini).expect("RuleSet::from_ini");

    // Verify reasonable type counts.
    println!("\n=== RuleSet Summary ===");
    println!("Infantry:  {}", rules.infantry_ids.len());
    println!("Vehicles:  {}", rules.vehicle_ids.len());
    println!("Aircraft:  {}", rules.aircraft_ids.len());
    println!("Buildings: {}", rules.building_ids.len());
    println!("Objects:   {}", rules.object_count());
    println!("Weapons:   {}", rules.weapon_count());
    println!("Warheads:  {}", rules.warhead_count());

    // RA2 has significant numbers of each type.
    assert!(rules.infantry_ids.len() > 20, "Expected 20+ infantry types");
    assert!(rules.vehicle_ids.len() > 20, "Expected 20+ vehicle types");
    assert!(rules.aircraft_ids.len() > 5, "Expected 5+ aircraft types");
    assert!(rules.building_ids.len() > 50, "Expected 50+ building types");
    assert!(rules.weapon_count() > 30, "Expected 30+ weapons");
    assert!(rules.warhead_count() > 5, "Expected 5+ warheads");

    // Spot-check specific units.
    let e1 = rules.object("E1").expect("E1 (GI) should exist");
    assert_eq!(e1.category, ObjectCategory::Infantry);
    assert!(e1.cost > 0, "E1 should have a cost");
    assert!(e1.strength > 0, "E1 should have health");
    println!(
        "\nE1 (GI): cost={}, hp={}, armor={}",
        e1.cost, e1.strength, e1.armor
    );

    let mtnk = rules.object("MTNK");
    if let Some(mtnk) = mtnk {
        assert_eq!(mtnk.category, ObjectCategory::Vehicle);
        println!(
            "MTNK (Grizzly): cost={}, hp={}, primary={:?}",
            mtnk.cost, mtnk.strength, mtnk.primary
        );
    }

    let gaweap = rules.object("GAWEAP");
    if let Some(gaweap) = gaweap {
        assert_eq!(gaweap.category, ObjectCategory::Building);
        println!(
            "GAWEAP (War Factory): cost={}, hp={}, foundation={}",
            gaweap.cost, gaweap.strength, gaweap.foundation
        );
    }

    // Spot-check a weapon if E1 has one.
    if let Some(ref weapon_id) = e1.primary {
        if let Some(weapon) = rules.weapon(weapon_id) {
            println!(
                "\nWeapon '{}': damage={}, range={:.1}, rof={}, warhead={:?}",
                weapon.id, weapon.damage, weapon.range, weapon.rof, weapon.warhead
            );

            // Check the warhead chain.
            if let Some(ref wh_id) = weapon.warhead {
                if let Some(wh) = rules.warhead(wh_id) {
                    println!(
                        "Warhead '{}': {} armor multipliers, spread={:.1}",
                        wh.id,
                        wh.verses.len(),
                        wh.cell_spread
                    );
                }
            }
        }
    }

    println!("\n=== Rules loading test passed ===");
}
