//! Diagnostic test: identifies where tile_ids are lost in the loading pipeline.
//!
//! Run with: cargo test --test tile_coverage -- --nocapture
//!
//! Pipeline: IsoMapPack5 cell → (tile_id, sub_tile) → TilesetLookup → TMP filename
//! → MIX chain → TmpFile → RGBA. Tiles can be lost at 4 points:
//! 1. Out-of-range: tile_id exceeds lookup table size
//! 2. Blank slot: FileName="" in INI (consumes tile_ids but maps to nothing)
//! 3. TMP not found: INI references a file not in any loaded MIX archive
//! 4. Empty cell: TMP loaded but sub_tile points to a None cell in the template

use std::collections::{HashMap, HashSet};
use std::path::Path;

use vera20k::assets::asset_manager::AssetManager;
use vera20k::assets::tmp_file::TmpFile;
use vera20k::map::map_file;
use vera20k::map::terrain;
use vera20k::map::theater::{self, TileKey};

fn ra2_dir() -> String {
    std::env::var("RA2_DIR")
        .unwrap_or_else(|_| panic!("Set RA2_DIR env var to your RA2/YR install directory"))
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn diagnose_tile_coverage() {
    let _ = env_logger::try_init();

    let ra2_dir_str = ra2_dir();
    let ra2_dir: &Path = Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        println!("SKIP: RA2 dir not found at {}", ra2_dir_str);
        return;
    }

    // 1. Load asset manager.
    let mut asset_manager: AssetManager = AssetManager::new(ra2_dir).expect("AssetManager::new");

    // 2. Load a map (Dustbowl is small/temperate).
    let map_names: &[&str] = &["Dustbowl.mmx", "Barrel.mmx", "Lostlake.mmx"];
    let map_data = load_first_map(ra2_dir, map_names);
    let theater_name: &str = &map_data.header.theater;
    println!("\n=== Tile Coverage Diagnostic ===");
    println!("Map theater: {}", theater_name);
    println!("Map cells: {}", map_data.cells.len());

    // 3. Load theater (lookup + palette).
    let td = theater::load_theater(&mut asset_manager, theater_name)
        .expect("load_theater should find INI + palette");
    let lookup = &td.lookup;

    println!("Tileset lookup: {} total slots", lookup.len());

    // Count blank vs non-blank slots in the lookup table.
    let blank_slots: usize = (0..lookup.len())
        .filter(|&i| lookup.filename(i as i32).is_none())
        .count();
    let non_blank_slots: usize = lookup.len() - blank_slots;
    println!("  Non-blank filename slots: {}", non_blank_slots);
    println!("  Blank filename slots: {}", blank_slots);

    // 4. Check which non-blank filenames are actually findable in MIX archives.
    let mut unique_filenames: HashSet<String> = HashSet::new();
    for i in 0..lookup.len() {
        if let Some(f) = lookup.filename(i as i32) {
            unique_filenames.insert(f.to_string());
        }
    }
    let found_count: usize = unique_filenames
        .iter()
        .filter(|f| asset_manager.contains(f))
        .count();
    let missing_files: Vec<&String> = unique_filenames
        .iter()
        .filter(|f| !asset_manager.contains(f))
        .collect();
    println!("  Unique TMP filenames: {}", unique_filenames.len());
    println!("  Found in MIX chain: {}", found_count);
    println!("  Missing from MIX chain: {}", missing_files.len());

    if !missing_files.is_empty() {
        let mut sorted: Vec<&&String> = missing_files.iter().collect();
        sorted.sort();
        let show: usize = sorted.len().min(30);
        println!("  First {} missing files:", show);
        for f in &sorted[..show] {
            println!("    {}", f);
        }
    }

    // 5. Analyze tile_index distribution BEFORE filtering.
    let neg_one_count: usize = map_data.cells.iter().filter(|c| c.tile_index == -1).count();
    let zero_count: usize = map_data.cells.iter().filter(|c| c.tile_index == 0).count();
    let positive_count: usize = map_data.cells.iter().filter(|c| c.tile_index > 0).count();
    println!(
        "\n=== tile_index distribution (all {} cells) ===",
        map_data.cells.len()
    );
    println!(
        "  tile_index == -1 (NO_TILE): {:>5} ({:.1}%)",
        neg_one_count,
        pct(neg_one_count, map_data.cells.len())
    );
    println!(
        "  tile_index ==  0 (clear):   {:>5} ({:.1}%)",
        zero_count,
        pct(zero_count, map_data.cells.len())
    );
    println!(
        "  tile_index >  0 (specific): {:>5} ({:.1}%)",
        positive_count,
        pct(positive_count, map_data.cells.len())
    );

    // Collect unique tile keys from the map.
    let grid = terrain::build_terrain_grid(&map_data, None);
    println!(
        "\nTerrain grid: {} cells (after filtering tile_index < 0)",
        grid.cells.len()
    );
    println!(
        "  Cells filtered out: {} ({:.1}% of total)",
        map_data.cells.len() - grid.cells.len(),
        pct(
            map_data.cells.len() - grid.cells.len(),
            map_data.cells.len()
        )
    );
    let cell_pairs: Vec<(i32, u8)> = grid
        .cells
        .iter()
        .map(|c| (c.tile_id as i32, c.sub_tile))
        .collect();
    let needed: HashSet<TileKey> = theater::collect_used_tiles(&cell_pairs);
    println!("Map uses {} unique tile keys", needed.len());

    // 6. Classify each needed tile key.
    let mut out_of_range: Vec<TileKey> = Vec::new();
    let mut blank_slot: Vec<TileKey> = Vec::new();
    let mut missing_file: Vec<TileKey> = Vec::new();
    let mut empty_cell: Vec<TileKey> = Vec::new();
    let mut parse_error: Vec<TileKey> = Vec::new();
    let mut loaded_ok: Vec<TileKey> = Vec::new();

    // Group by tile_id to avoid loading the same TMP multiple times.
    let mut by_tile_id: HashMap<u16, Vec<u8>> = HashMap::new();
    for key in &needed {
        by_tile_id
            .entry(key.tile_id)
            .or_default()
            .push(key.sub_tile);
    }

    for (tile_id, sub_tiles) in &by_tile_id {
        let tile_id_i32: i32 = *tile_id as i32;

        // Check if tile_id is in range.
        if tile_id_i32 >= lookup.len() as i32 {
            for &sub in sub_tiles {
                out_of_range.push(TileKey {
                    tile_id: *tile_id,
                    sub_tile: sub,
                    variant: 0,
                });
            }
            continue;
        }

        // Check if slot has a filename.
        let filename: &str = match lookup.filename(tile_id_i32) {
            Some(f) => f,
            None => {
                for &sub in sub_tiles {
                    blank_slot.push(TileKey {
                        tile_id: *tile_id,
                        sub_tile: sub,
                        variant: 0,
                    });
                }
                continue;
            }
        };

        // Check if TMP file exists in MIX archives.
        let tmp_data: Vec<u8> = match asset_manager.get(filename) {
            Some(d) => d,
            None => {
                for &sub in sub_tiles {
                    missing_file.push(TileKey {
                        tile_id: *tile_id,
                        sub_tile: sub,
                        variant: 0,
                    });
                }
                continue;
            }
        };

        // Try parsing TMP file.
        let tmp: TmpFile = match TmpFile::from_bytes(&tmp_data) {
            Ok(t) => t,
            Err(_) => {
                for &sub in sub_tiles {
                    parse_error.push(TileKey {
                        tile_id: *tile_id,
                        sub_tile: sub,
                        variant: 0,
                    });
                }
                continue;
            }
        };

        // Check each sub_tile.
        let cell_count: usize = (tmp.template_width * tmp.template_height) as usize;
        for &sub in sub_tiles {
            let key = TileKey {
                tile_id: *tile_id,
                sub_tile: sub,
                variant: 0,
            };
            if (sub as usize) >= cell_count {
                empty_cell.push(key);
            } else if tmp.tiles[sub as usize].is_none() {
                empty_cell.push(key);
            } else {
                loaded_ok.push(key);
            }
        }
    }

    // 7. Print results.
    let total: usize = needed.len();
    println!("\n=== Breakdown (out of {} unique tile keys) ===", total);
    println!(
        "  Loaded OK:       {:>4}  ({:.1}%)",
        loaded_ok.len(),
        pct(loaded_ok.len(), total)
    );
    println!(
        "  Blank slot:      {:>4}  ({:.1}%)",
        blank_slot.len(),
        pct(blank_slot.len(), total)
    );
    println!(
        "  Missing file:    {:>4}  ({:.1}%)",
        missing_file.len(),
        pct(missing_file.len(), total)
    );
    println!(
        "  Empty cell:      {:>4}  ({:.1}%)",
        empty_cell.len(),
        pct(empty_cell.len(), total)
    );
    println!(
        "  Out of range:    {:>4}  ({:.1}%)",
        out_of_range.len(),
        pct(out_of_range.len(), total)
    );
    println!(
        "  Parse error:     {:>4}  ({:.1}%)",
        parse_error.len(),
        pct(parse_error.len(), total)
    );

    // Show sample missing files for debugging.
    if !missing_file.is_empty() {
        let mut seen: HashSet<u16> = HashSet::new();
        println!("\n  Sample missing file tile_ids:");
        for key in &missing_file {
            if seen.insert(key.tile_id) && seen.len() <= 20 {
                let fname = lookup.filename(key.tile_id as i32).unwrap_or("?");
                println!("    tile_id={:>4} → {}", key.tile_id, fname);
            }
        }
    }

    if !out_of_range.is_empty() {
        let mut seen: HashSet<u16> = HashSet::new();
        println!(
            "\n  Sample out-of-range tile_ids (lookup has {} slots):",
            lookup.len()
        );
        for key in &out_of_range {
            if seen.insert(key.tile_id) && seen.len() <= 20 {
                println!("    tile_id={}", key.tile_id);
            }
        }
    }

    println!("\n=== Diagnostic complete ===");
}

fn pct(n: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        n as f64 / total as f64 * 100.0
    }
}

fn load_first_map(ra2_dir: &Path, names: &[&str]) -> vera20k::map::map_file::MapFile {
    for &name in names {
        let path = ra2_dir.join(name);
        if path.exists() {
            match map_file::load_mmx(&path) {
                Ok(mf) => {
                    println!("Loaded map: {}", name);
                    return mf;
                }
                Err(e) => println!("Failed to load {}: {}", name, e),
            }
        }
    }
    panic!("No .mmx map files found in {}", ra2_dir.display());
}
