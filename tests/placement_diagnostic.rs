//! Diagnostic test: checks building placement constraints on a real map.
//!
//! Run with: cargo test --test placement_diagnostic -- --nocapture

use std::collections::HashSet;
use std::path::Path;

use vera20k::assets::asset_manager::AssetManager;
use vera20k::map::{map_file, theater};
use vera20k::rules::art_data::ArtRegistry;
use vera20k::rules::ini_parser::IniFile;
use vera20k::rules::ruleset::RuleSet;
use vera20k::sim::pathfinding::PathGrid;

fn ra2_dir() -> String {
    std::env::var("RA2_DIR")
        .unwrap_or_else(|_| panic!("Set RA2_DIR env var to your RA2/YR install directory"))
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn diagnose_placement_constraints() {
    let _ = env_logger::try_init();

    let ra2_dir_str = ra2_dir();
    let ra2_dir = Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        println!("SKIP: RA2 dir not found");
        return;
    }

    let mut asset_manager = AssetManager::new(ra2_dir).expect("AssetManager");

    // Load rules
    let rules_data = asset_manager
        .get("rulesmd.ini")
        .or_else(|| asset_manager.get("rules.ini"))
        .expect("rules INI");
    let rules_ini = IniFile::from_bytes(&rules_data).expect("parse rules INI");
    let mut rules = RuleSet::from_ini(&rules_ini).expect("parse rules");

    // Load art.ini and merge Foundation values
    let art_data = asset_manager
        .get("artmd.ini")
        .or_else(|| asset_manager.get("art.ini"))
        .expect("art INI");
    let art_ini = IniFile::from_bytes(&art_data).expect("parse art INI");
    let art_registry = ArtRegistry::from_ini(&art_ini);
    rules.merge_art_data(&art_registry);

    // Print Adjacent + BaseNormal for key building types
    println!("\n=== Adjacent & BaseNormal for key types ===");
    let key_types = [
        "GACNST", "NACNST", "YACNST", // ConYards
        "GAPOWR", "NAPOWR", // Power
        "GAREFN", "NAREFN", // Refineries
        "GAWEAP", "NAWEAP", // War Factory
        "GAPILE", "NAHAND", // Barracks
        "GAWALL", "NAWALL", // Walls
    ];
    for type_id in &key_types {
        if let Some(obj) = rules.object(type_id) {
            println!(
                "  {:8} Adjacent={:>3}  BaseNormal={:<5}  Foundation={}",
                type_id, obj.adjacent, obj.base_normal, obj.foundation
            );
        }
    }

    // Load map
    let map_names = ["testmap1.map", "Dustbowl.mmx", "Barrel.mmx"];
    let map_data = {
        let mut result = None;
        for name in &map_names {
            let path = ra2_dir.join(name);
            if path.exists() {
                match if name.ends_with(".mmx") {
                    map_file::load_mmx(&path)
                } else {
                    let bytes = std::fs::read(&path).unwrap();
                    map_file::MapFile::from_bytes(&bytes)
                } {
                    Ok(mf) => {
                        println!("\nLoaded map: {} (theater={})", name, mf.header.theater);
                        result = Some(mf);
                        break;
                    }
                    Err(e) => println!("Failed {}: {}", name, e),
                }
            }
            // Try maps dir
            let maps_path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("maps")
                .join(name);
            if maps_path.exists() {
                let bytes = std::fs::read(&maps_path).unwrap();
                match map_file::MapFile::from_bytes(&bytes) {
                    Ok(mf) => {
                        println!(
                            "\nLoaded map: maps/{} (theater={})",
                            name, mf.header.theater
                        );
                        result = Some(mf);
                        break;
                    }
                    Err(e) => println!("Failed maps/{}: {}", name, e),
                }
            }
        }
        result.expect("No map found")
    };

    // Load theater
    let theater_name = &map_data.header.theater;
    let td = theater::load_theater(&mut asset_manager, theater_name).expect("load_theater");

    // Print tileset classification stats
    println!("\n=== Tileset SetName classification ===");
    let mut water_sets: Vec<String> = Vec::new();
    let mut cliff_sets: Vec<String> = Vec::new();
    let mut other_sets: Vec<String> = Vec::new();
    for i in 0..td.lookup.len() as u16 {
        if let Some(name) = td.lookup.set_name(i) {
            if name.is_empty() {
                continue;
            }
            let lower = name.to_ascii_lowercase();
            if lower.contains("water") {
                if !water_sets.contains(&name.to_string()) {
                    water_sets.push(name.to_string());
                }
            } else if lower.contains("cliff") {
                if !cliff_sets.contains(&name.to_string()) {
                    cliff_sets.push(name.to_string());
                }
            } else if !other_sets.contains(&name.to_string()) {
                other_sets.push(name.to_string());
            }
        }
    }
    println!("  Water tilesets ({}):", water_sets.len());
    for s in &water_sets {
        println!("    - {}", s);
    }
    println!("  Cliff tilesets ({}):", cliff_sets.len());
    for s in &cliff_sets {
        println!("    - {}", s);
    }
    println!("  Other tilesets ({}):", other_sets.len());
    for s in &other_sets {
        println!("    - {}", s);
    }

    // Build PathGrid
    let (map_w, map_h) = {
        let mut max_rx: u16 = 0;
        let mut max_ry: u16 = 0;
        for cell in &map_data.cells {
            if cell.rx > max_rx {
                max_rx = cell.rx;
            }
            if cell.ry > max_ry {
                max_ry = cell.ry;
            }
        }
        (max_rx + 1, max_ry + 1)
    };
    let grid = PathGrid::from_map_data(&map_data.cells, Some(&td.lookup), map_w, map_h);

    let total_cells = map_w as u32 * map_h as u32;
    let walkable: u32 = (0..map_h)
        .flat_map(|y| (0..map_w).map(move |x| (x, y)))
        .filter(|&(x, y)| grid.is_walkable(x, y))
        .count() as u32;
    let blocked = total_cells - walkable;

    println!("\n=== PathGrid stats ===");
    println!("  Grid size: {}x{} = {} cells", map_w, map_h, total_cells);
    println!(
        "  Walkable:  {} ({:.1}%)",
        walkable,
        walkable as f64 / total_cells as f64 * 100.0
    );
    println!(
        "  Blocked:   {} ({:.1}%)",
        blocked,
        blocked as f64 / total_cells as f64 * 100.0
    );
    println!("  Map cells from IsoMapPack5: {}", map_data.cells.len());

    // Count how many map cells are skipped vs classified
    let mut skipped_negative = 0u32;
    let mut classified_water = 0u32;
    let mut classified_cliff = 0u32;
    let mut classified_walkable = 0u32;
    for cell in &map_data.cells {
        if cell.tile_index < 0 {
            skipped_negative += 1;
            continue;
        }
        let tile_id: u16 = if cell.tile_index == 0xFFFF {
            0
        } else {
            cell.tile_index as u16
        };
        if td.lookup.is_water(tile_id) {
            classified_water += 1;
        } else if td.lookup.is_cliff(tile_id) {
            classified_cliff += 1;
        } else {
            classified_walkable += 1;
        }
    }
    println!("\n=== Cell classification from IsoMapPack5 ===");
    println!("  Total cells: {}", map_data.cells.len());
    println!("  Skipped (tile_index < 0): {}", skipped_negative);
    println!("  Classified water: {}", classified_water);
    println!("  Classified cliff: {}", classified_cliff);
    println!("  Classified walkable: {}", classified_walkable);
    println!(
        "  Grid cells with no map data (blocked by default): {}",
        total_cells - classified_walkable - classified_water - classified_cliff
    );

    // Check which cliff tile_ids are actually used on this map
    let mut cliff_tile_ids: HashSet<u16> = HashSet::new();
    for cell in &map_data.cells {
        if cell.tile_index < 0 {
            continue;
        }
        let tile_id: u16 = if cell.tile_index == 0xFFFF {
            0
        } else {
            cell.tile_index as u16
        };
        if td.lookup.is_cliff(tile_id) {
            cliff_tile_ids.insert(tile_id);
        }
    }
    if !cliff_tile_ids.is_empty() {
        println!("\n  Cliff tile_ids used on map:");
        let mut sorted: Vec<u16> = cliff_tile_ids.into_iter().collect();
        sorted.sort();
        for tid in &sorted[..sorted.len().min(20)] {
            let set_name = td
                .lookup
                .tileset_index(*tid)
                .and_then(|idx| td.lookup.set_name(idx))
                .unwrap_or("?");
            println!("    tile_id={:>4} set={}", tid, set_name);
        }
    }

    // Search raw rules.ini text for Foundation near GACNST
    println!("\n=== Looking for Foundation in rules.ini ===");
    if let Some(base_data) = asset_manager.get("rules.ini") {
        let text = std::str::from_utf8(&base_data).unwrap();
        // Find [GACNST] and print surrounding 50 lines
        for (i, line) in text.lines().enumerate() {
            if line.trim().eq_ignore_ascii_case("[GACNST]") {
                println!("  Found [GACNST] at line {}, next 50 lines:", i + 1);
                for (j, following) in text.lines().skip(i).take(50).enumerate() {
                    println!("    L{}: {}", i + 1 + j, following);
                    if j > 0 && following.trim().starts_with('[') {
                        break;
                    }
                }
                break;
            }
        }
        // Also search for any line with "Foundation" to see the pattern
        println!("\n  First 10 Foundation= lines in rules.ini:");
        let mut count = 0;
        for (i, line) in text.lines().enumerate() {
            let trimmed = line.trim().to_ascii_lowercase();
            if trimmed.starts_with("foundation=") && count < 10 {
                println!("    L{}: {}", i + 1, line.trim());
                count += 1;
            }
        }
        if count == 0 {
            println!("    NONE found! Foundation might be in art.ini instead.");
        }
    }

    // Check art.ini for Foundation
    println!("\n=== Looking for Foundation in art.ini / artmd.ini ===");
    for art_name in &["artmd.ini", "art.ini"] {
        if let Some(art_data) = asset_manager.get(art_name) {
            let text = std::str::from_utf8(&art_data).unwrap();
            let mut count = 0;
            for (i, line) in text.lines().enumerate() {
                let trimmed = line.trim().to_ascii_lowercase();
                if trimmed.starts_with("foundation=") && count < 10 {
                    println!("  {} L{}: {}", art_name, i + 1, line.trim());
                    count += 1;
                }
            }
            if count > 0 {
                println!(
                    "  Total Foundation= lines in {}: {}",
                    art_name,
                    text.lines()
                        .filter(|l| l.trim().to_ascii_lowercase().starts_with("foundation="))
                        .count()
                );
                break;
            } else {
                println!("  {} has no Foundation= lines", art_name);
            }
        }
    }

    println!("\n=== Diagnostic complete ===");
}
