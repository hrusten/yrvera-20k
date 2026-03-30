//! Diagnostic test: print overlay IDs used by Dustbowl and their resolved names.
//!
//! Run with:
//!   cargo test --test overlay_ids -- --nocapture

use std::collections::HashMap;
use std::path::Path;

use vera20k::assets::asset_manager::AssetManager;
use vera20k::assets::pal_file::Palette;
use vera20k::assets::shp_file::ShpFile;
use vera20k::map::map_file;
use vera20k::map::overlay_types::OverlayTypeRegistry;
use vera20k::map::terrain::{self, LocalBounds};
use vera20k::rules::ini_parser::IniFile;

fn ra2_dir() -> String {
    std::env::var("RA2_DIR")
        .unwrap_or_else(|_| panic!("Set RA2_DIR env var to your RA2/YR install directory"))
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn print_overlay_id_mapping_for_dustbowl() {
    let _ = env_logger::try_init();

    let ra2_dir_str = ra2_dir();
    let ra2_dir: &Path = Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        println!("SKIP: RA2 dir not found at {}", ra2_dir_str);
        return;
    }

    let asset_manager: AssetManager = AssetManager::new(ra2_dir).expect("asset manager");
    let map_path = ra2_dir.join("Dustbowl.mmx");
    let map = map_file::load_mmx(&map_path).expect("load Dustbowl.mmx");

    let (rules_bytes, source) = asset_manager
        .get_with_source("rulesmd.ini")
        .or_else(|| asset_manager.get_with_source("rules.ini"))
        .expect("rules ini");
    println!("Rules source: {}", source);
    let rules_ini: IniFile = IniFile::from_bytes(&rules_bytes).expect("parse rules ini");
    let reg: OverlayTypeRegistry = OverlayTypeRegistry::from_ini(&rules_ini);

    let mut counts: HashMap<u8, u32> = HashMap::new();
    let mut frame_stats: HashMap<u8, HashMap<u8, u32>> = HashMap::new();
    let mut tib_frames: HashMap<u8, u32> = HashMap::new();
    let mut tib_cells: Vec<(u16, u16)> = Vec::new();
    for e in &map.overlays {
        *counts.entry(e.overlay_id).or_insert(0) += 1;
        *frame_stats
            .entry(e.overlay_id)
            .or_default()
            .entry(e.frame)
            .or_insert(0) += 1;
        if e.overlay_id == 168 {
            *tib_frames.entry(e.frame).or_insert(0) += 1;
            tib_cells.push((e.rx, e.ry));
        }
    }

    let mut by_count: Vec<(u8, u32)> = counts.into_iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    println!("Unique overlay IDs in map: {}", by_count.len());
    for (id, count) in &by_count {
        let name = reg.name(*id).unwrap_or("(unmapped)");
        let (uniq_frames, max_frame) = frame_stats
            .get(id)
            .map(|m| (m.len(), *m.keys().max().unwrap_or(&0)))
            .unwrap_or((0, 0));
        println!("  id={:3} count={:4} name={}", id, count, name);
        println!("    frames: unique={} max={}", uniq_frames, max_frame);
    }

    println!("\nResource-like overlay IDs (TIB/GEM):");
    let mut found_any = false;
    for (id, count) in &by_count {
        if let Some(name) = reg.name(*id) {
            if name.starts_with("TIB") || name.starts_with("GEM") {
                found_any = true;
                println!("  id={:3} count={:4} name={}", id, count, name);
                if let Some(sec) = rules_ini.section(name) {
                    let tib_type = sec.get("TiberiumType").unwrap_or("(none)");
                    let image = sec.get("Image").unwrap_or("(none)");
                    let tib_flag = sec.get("Tiberium").unwrap_or("(none)");
                    println!(
                        "    [{}] Tiberium={} TiberiumType={} Image={}",
                        name, tib_flag, tib_type, image
                    );
                    if let Some(tib_sec) = rules_ini.section(tib_type) {
                        let color = tib_sec.get("Color").unwrap_or("(none)");
                        println!("    [{}] Color={}", tib_type, color);
                    }
                }
            }
        }
    }
    if !found_any {
        println!("  none");
    }

    println!("\nOverlayTypes around id 168:");
    for id in 156u8..=180u8 {
        let name = reg.name(id).unwrap_or("(none)");
        println!("  id={:3} name={}", id, name);
    }

    for name in ["LOBRDG27", "FENCE21", "LOBRDG26", "FENCE20"] {
        if let Some(sec) = rules_ini.section(name) {
            let image = sec.get("Image").unwrap_or("(none)");
            let theater = sec.get("Theater").unwrap_or("(none)");
            println!("Rule [{}]: Image={} Theater={}", name, image, theater);
        } else {
            println!("Rule [{}]: (missing section)", name);
        }
    }

    if !tib_frames.is_empty() {
        let mut frames: Vec<(u8, u32)> = tib_frames.into_iter().collect();
        frames.sort_by(|a, b| a.0.cmp(&b.0));
        println!("\nFrame histogram for overlay id=168 (TIB3_20):");
        for (frame, count) in frames {
            println!("  frame={:3} count={}", frame, count);
        }
        tib_cells.sort_unstable();
        let (min_rx, max_rx) = tib_cells
            .iter()
            .fold((u16::MAX, 0u16), |acc, c| (acc.0.min(c.0), acc.1.max(c.0)));
        let (min_ry, max_ry) = tib_cells
            .iter()
            .fold((u16::MAX, 0u16), |acc, c| (acc.0.min(c.1), acc.1.max(c.1)));
        println!(
            "TIB3_20 bounds: rx=[{},{}] ry=[{},{}]",
            min_rx, max_rx, min_ry, max_ry
        );
        let show = tib_cells.len().min(20);
        println!("Sample TIB3_20 cells ({} shown):", show);
        for (rx, ry) in tib_cells.iter().take(show) {
            println!("  ({},{})", rx, ry);
        }

        let local_bounds: LocalBounds = LocalBounds::from_header(&map.header);
        let sw: f32 = 1280.0;
        let sh: f32 = 960.0;
        let cam_x: f32 = local_bounds.pixel_x + (local_bounds.pixel_w - sw) / 2.0;
        let cam_y: f32 = local_bounds.pixel_y + (local_bounds.pixel_h - sh) / 2.0;
        let mut on_screen: u32 = 0;
        let mut clipped_local: u32 = 0;
        for e in &map.overlays {
            if e.overlay_id != 168 {
                continue;
            }
            let (sx, sy) = terrain::iso_to_screen(e.rx, e.ry, 0);
            if !local_bounds.contains(sx, sy) {
                clipped_local += 1;
                continue;
            }
            if sx >= cam_x - 120.0
                && sx <= cam_x + sw + 120.0
                && sy >= cam_y - 120.0
                && sy <= cam_y + sh + 120.0
            {
                on_screen += 1;
            }
        }
        println!(
            "TIB3_20 in initial viewport: {} (clipped by LocalSize: {})",
            on_screen, clipped_local
        );
    }
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn inspect_tib20_frames() {
    let ra2_dir_str = ra2_dir();
    let ra2_dir: &Path = Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        println!("SKIP: RA2 dir not found at {}", ra2_dir_str);
        return;
    }

    let asset_manager: AssetManager = AssetManager::new(ra2_dir).expect("asset manager");
    let mut found_any = false;

    let pal_data = asset_manager
        .get("unittem.pal")
        .or_else(|| asset_manager.get("unit.pal"))
        .expect("unit palette");
    let palette = Palette::from_bytes(&pal_data).expect("parse unit palette");

    for name in ["tib20.shp", "tib20.tem", "TIB20.shp", "TIB20.tem"] {
        let Some(data) = asset_manager.get(name) else {
            continue;
        };
        found_any = true;
        println!("Found {} ({} bytes)", name, data.len());
        match ShpFile::from_bytes(&data) {
            Ok(shp) => {
                println!(
                    "  Parsed SHP: {}x{}, {} frames",
                    shp.width,
                    shp.height,
                    shp.frames.len()
                );
                let show = shp.frames.len().min(8);
                for i in 0..show {
                    let fr = &shp.frames[i];
                    println!(
                        "  frame {:2}: x={} y={} w={} h={}",
                        i, fr.frame_x, fr.frame_y, fr.frame_width, fr.frame_height
                    );
                }
                if let Ok(rgba) = shp.frame_to_rgba(0, &palette) {
                    let mut non_transparent: usize = 0;
                    let mut sum_r: u64 = 0;
                    let mut sum_g: u64 = 0;
                    let mut sum_b: u64 = 0;
                    for px in rgba.chunks_exact(4) {
                        if px[3] > 0 {
                            non_transparent += 1;
                            sum_r += px[0] as u64;
                            sum_g += px[1] as u64;
                            sum_b += px[2] as u64;
                        }
                    }
                    if non_transparent > 0 {
                        println!(
                            "  frame0 visible pixels={} avg_rgb=({},{},{})",
                            non_transparent,
                            (sum_r / non_transparent as u64),
                            (sum_g / non_transparent as u64),
                            (sum_b / non_transparent as u64)
                        );
                    } else {
                        println!("  frame0 is fully transparent");
                    }
                }
            }
            Err(e) => {
                println!("  Not SHP parseable: {}", e);
            }
        }
    }

    if !found_any {
        println!("No TIB20.(shp|tem) found in loaded archives");
    }
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn inspect_problem_overlay_assets() {
    let ra2_dir_str = ra2_dir();
    let ra2_dir: &Path = Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        println!("SKIP: RA2 dir not found at {}", ra2_dir_str);
        return;
    }

    let asset_manager: AssetManager = AssetManager::new(ra2_dir).expect("asset manager");
    for name in [
        "lobrdg27.tem",
        "LOBRDG27.tem",
        "lobrdg26.tem",
        "LOBRDG26.tem",
        "fence21.tem",
        "FENCE21.tem",
        "fence20.tem",
        "FENCE20.tem",
    ] {
        match asset_manager.get(name) {
            Some(data) => {
                let parsed = ShpFile::from_bytes(&data).is_ok();
                println!(
                    "{}: found ({} bytes), shp_parse={}",
                    name,
                    data.len(),
                    parsed
                );
            }
            None => println!("{}: missing", name),
        }
    }
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn inspect_problem_overlay_frames() {
    let ra2_dir_str = ra2_dir();
    let ra2_dir: &Path = Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        println!("SKIP: RA2 dir not found at {}", ra2_dir_str);
        return;
    }

    let asset_manager: AssetManager = AssetManager::new(ra2_dir).expect("asset manager");
    for name in ["LOBRDG27.tem", "FENCE21.tem"] {
        let data = asset_manager.get(name).expect("overlay asset");
        let shp = ShpFile::from_bytes(&data).expect("parse shp");
        println!(
            "{}: {}x{}, {} frames",
            name,
            shp.width,
            shp.height,
            shp.frames.len()
        );
        for (i, fr) in shp.frames.iter().enumerate() {
            let non_empty = fr.frame_width > 0 && fr.frame_height > 0;
            println!(
                "  frame {:2}: w={} h={} x={} y={} non_empty={}",
                i, fr.frame_width, fr.frame_height, fr.frame_x, fr.frame_y, non_empty
            );
        }
    }
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn dump_raw_overlaytypes_ranges() {
    let ra2_dir_str = ra2_dir();
    let ra2_dir: &Path = Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        println!("SKIP: RA2 dir not found at {}", ra2_dir_str);
        return;
    }

    let asset_manager: AssetManager = AssetManager::new(ra2_dir).expect("asset manager");
    let (rules_bytes, source) = asset_manager
        .get_with_source("rulesmd.ini")
        .or_else(|| asset_manager.get_with_source("rules.ini"))
        .expect("rules ini");
    println!("Rules source: {}", source);
    let rules_ini: IniFile = IniFile::from_bytes(&rules_bytes).expect("parse rules ini");
    let section = rules_ini.section("OverlayTypes").expect("OverlayTypes");

    println!("Raw OverlayTypes key->name around 95..110:");
    for i in 95..=110 {
        let k = i.to_string();
        let v = section.get(&k).unwrap_or("(none)");
        println!("  key {:3} => {}", i, v);
    }

    println!("Raw OverlayTypes key->name around 164..172:");
    for i in 164..=172 {
        let k = i.to_string();
        let v = section.get(&k).unwrap_or("(none)");
        println!("  key {:3} => {}", i, v);
    }

    // Check for missing keys in the 0..max_key range.
    let mut max_key: usize = 0;
    let mut total_keys: usize = 0;
    let mut missing_keys: Vec<usize> = Vec::new();
    for key in section.keys() {
        if let Ok(n) = key.parse::<usize>() {
            total_keys += 1;
            if n > max_key {
                max_key = n;
            }
        }
    }
    for i in 0..=max_key {
        if section.get(&i.to_string()).is_none() {
            missing_keys.push(i);
        }
    }
    println!("Total keys: {}, max_key: {}", total_keys, max_key);
    println!("Missing keys in 0..{}: {:?}", max_key, missing_keys);
    println!("Raw key 0 => {}", section.get("0").unwrap_or("(missing)"));

    let reg = OverlayTypeRegistry::from_ini(&rules_ini);
    println!("Registry len: {}", reg.len());
    println!("Current runtime mapping id->name around 95..110:");
    for i in 95u8..=110u8 {
        println!("  id {:3} => {}", i, reg.name(i).unwrap_or("(none)"));
    }
    println!("Current runtime mapping id->name around 164..172:");
    for i in 164u8..=172u8 {
        println!("  id {:3} => {}", i, reg.name(i).unwrap_or("(none)"));
    }
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn compare_overlay_index_modes() {
    let ra2_dir_str = ra2_dir();
    let ra2_dir: &Path = Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        println!("SKIP: RA2 dir not found at {}", ra2_dir_str);
        return;
    }

    let asset_manager: AssetManager = AssetManager::new(ra2_dir).expect("asset manager");
    let map_path = ra2_dir.join("Dustbowl.mmx");
    let map = map_file::load_mmx(&map_path).expect("load Dustbowl.mmx");
    let (rules_bytes, _) = asset_manager
        .get_with_source("rulesmd.ini")
        .or_else(|| asset_manager.get_with_source("rules.ini"))
        .expect("rules ini");
    let rules_ini: IniFile = IniFile::from_bytes(&rules_bytes).expect("parse rules ini");
    let section = rules_ini.section("OverlayTypes").expect("OverlayTypes");

    let mut counts: HashMap<u8, u32> = HashMap::new();
    for e in &map.overlays {
        *counts.entry(e.overlay_id).or_insert(0) += 1;
    }
    let mut ids: Vec<(u8, u32)> = counts.into_iter().collect();
    ids.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let summarize = |offset: u16| {
        let mut mapped: u32 = 0;
        let mut missing: u32 = 0;
        let mut useless: u32 = 0;
        let mut tiberium: u32 = 0;
        for (id, count) in &ids {
            let key = (*id as u16 + offset).to_string();
            match section.get(&key) {
                Some(name) => {
                    mapped += *count;
                    let up = name.to_ascii_uppercase();
                    if up.contains("USELESS") || up.contains("DUMMY") {
                        useless += *count;
                    }
                    if up.starts_with("TIB") || up.starts_with("GEM") {
                        tiberium += *count;
                    }
                }
                None => missing += *count,
            }
        }
        (mapped, missing, useless, tiberium)
    };

    let a = summarize(0);
    let b = summarize(1);
    println!(
        "Mode id->key(id):   mapped={} missing={} useless={} tiberium={}",
        a.0, a.1, a.2, a.3
    );
    println!(
        "Mode id->key(id+1): mapped={} missing={} useless={} tiberium={}",
        b.0, b.1, b.2, b.3
    );
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn yr_overlay_registry_matches_ra2_resource_families() {
    let ra2_dir_str = ra2_dir();
    let ra2_dir: &Path = Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        println!("SKIP: RA2 dir not found at {}", ra2_dir_str);
        return;
    }

    let asset_manager: AssetManager = AssetManager::new(ra2_dir).expect("asset manager");
    let (rules_bytes, _) = asset_manager
        .get_with_source("rulesmd.ini")
        .or_else(|| asset_manager.get_with_source("rules.ini"))
        .expect("rules ini");
    let rules_ini: IniFile = IniFile::from_bytes(&rules_bytes).expect("parse rules ini");
    let reg: OverlayTypeRegistry = OverlayTypeRegistry::from_ini(&rules_ini);

    assert_eq!(reg.name(27), Some("GEM01"));
    assert_eq!(reg.name(38), Some("GEM12"));
    assert_eq!(reg.name(102), Some("TIB01"));
    assert_eq!(reg.name(121), Some("TIB20"));
    assert_eq!(reg.name(127), Some("TIB2_01"));
    assert_eq!(reg.name(146), Some("TIB2_20"));
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn inspect_tib01_palette_indices() {
    use std::collections::{BTreeMap, BTreeSet};

    let ra2_dir_str = ra2_dir();
    let ra2_dir: &Path = Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        println!("SKIP: RA2 dir not found at {}", ra2_dir_str);
        return;
    }

    let asset_manager: AssetManager = AssetManager::new(ra2_dir).expect("asset manager");

    // --- Load TIB01.tem SHP ---
    let shp_data = asset_manager
        .get("tib01.tem")
        .or_else(|| asset_manager.get("TIB01.tem"))
        .expect("TIB01.tem not found in any archive");
    println!("TIB01.tem: {} bytes", shp_data.len());

    let shp: ShpFile = ShpFile::from_bytes(&shp_data).expect("parse TIB01.tem as SHP");
    println!(
        "SHP header: {}x{}, {} frames",
        shp.width,
        shp.height,
        shp.frames.len()
    );

    // --- Analyze frame 6 (mid-density tiberium) ---
    let frame_idx: usize = 6;
    assert!(
        shp.frames.len() > frame_idx,
        "TIB01.tem has fewer than {} frames (got {})",
        frame_idx + 1,
        shp.frames.len()
    );

    let frame = &shp.frames[frame_idx];
    println!(
        "\nFrame {}: {}x{} at ({},{}), {} pixels total",
        frame_idx,
        frame.frame_width,
        frame.frame_height,
        frame.frame_x,
        frame.frame_y,
        frame.pixels.len()
    );

    // Collect unique non-zero (non-transparent) palette indices
    let mut unique_indices: BTreeSet<u8> = BTreeSet::new();
    let mut index_counts: BTreeMap<u8, u32> = BTreeMap::new();
    let mut total_transparent: u32 = 0;
    let mut total_visible: u32 = 0;

    for &idx in &frame.pixels {
        if idx == 0 {
            total_transparent += 1;
        } else {
            total_visible += 1;
            unique_indices.insert(idx);
            *index_counts.entry(idx).or_insert(0) += 1;
        }
    }

    println!(
        "Transparent (idx=0): {}, Visible (idx!=0): {}",
        total_transparent, total_visible
    );
    println!("Unique non-zero palette indices: {}", unique_indices.len());

    // Print all unique indices
    println!("\nAll unique palette indices used by visible pixels:");
    for &idx in &unique_indices {
        let count = index_counts.get(&idx).copied().unwrap_or(0);
        println!("  index {:3} => {} pixels", idx, count);
    }

    // Count pixels in the house-color remap range (16-31)
    let house_color_pixels: u32 = (16u8..=31u8)
        .map(|i| index_counts.get(&i).copied().unwrap_or(0))
        .sum();
    let house_color_indices: Vec<u8> = unique_indices
        .iter()
        .copied()
        .filter(|&i| (16..=31).contains(&i))
        .collect();

    println!(
        "\nHouse-color remap range (16-31): {} pixels across {} unique indices",
        house_color_pixels,
        house_color_indices.len()
    );
    if !house_color_indices.is_empty() {
        println!("  Indices in range: {:?}", house_color_indices);
    }

    // Categorize by range
    let range_0_15: u32 = (1u8..=15u8)
        .map(|i| index_counts.get(&i).copied().unwrap_or(0))
        .sum();
    let range_32_127: u32 = (32u8..=127u8)
        .map(|i| index_counts.get(&i).copied().unwrap_or(0))
        .sum();
    let range_128_255: u32 = (128u8..=255u8)
        .map(|i| index_counts.get(&i).copied().unwrap_or(0))
        .sum();

    println!("\nPixel count by palette range:");
    println!("  indices   1-15:  {} pixels", range_0_15);
    println!(
        "  indices 16-31:   {} pixels (house color remap)",
        house_color_pixels
    );
    println!("  indices 32-127:  {} pixels", range_32_127);
    println!("  indices 128-255: {} pixels", range_128_255);

    // --- Also dump a few other frames for comparison ---
    println!("\n--- Summary for all frames ---");
    for fi in 0..shp.frames.len() {
        let fr = &shp.frames[fi];
        let vis: u32 = fr.pixels.iter().filter(|&&p| p != 0).count() as u32;
        let hc: u32 = fr
            .pixels
            .iter()
            .filter(|&&p| (16..=31).contains(&p))
            .count() as u32;
        if vis > 0 {
            println!(
                "  frame {:2}: visible={:4} house_color_range={:4} ({:.1}%)",
                fi,
                vis,
                hc,
                (hc as f64 / vis as f64) * 100.0
            );
        } else {
            println!("  frame {:2}: empty", fi);
        }
    }

    // --- Read rulesmd.ini for [Riparius] and [Tiberiums] ---
    let (rules_bytes, rules_source) = asset_manager
        .get_with_source("rulesmd.ini")
        .or_else(|| asset_manager.get_with_source("rules.ini"))
        .expect("rules ini");
    println!("\n--- Rules from: {} ---", rules_source);
    let rules_ini: IniFile = IniFile::from_bytes(&rules_bytes).expect("parse rules ini");

    // Print [Tiberiums] section
    println!("\n[Tiberiums] section:");
    if let Some(tib_sec) = rules_ini.section("Tiberiums") {
        for key in tib_sec.keys() {
            let val = tib_sec.get(key).unwrap_or("(empty)");
            println!("  {}={}", key, val);
        }
    } else {
        println!("  (section not found)");
    }

    // Print [Riparius] section fields
    println!("\n[Riparius] section:");
    if let Some(rip_sec) = rules_ini.section("Riparius") {
        for key in rip_sec.keys() {
            let val = rip_sec.get(key).unwrap_or("(empty)");
            println!("  {}={}", key, val);
        }
    } else {
        println!("  (section not found)");
    }

    // --- Check raw palette byte range ---
    let pal_data = asset_manager.get("unittem.pal").expect("unittem.pal");
    let max_byte: u8 = *pal_data.iter().max().unwrap_or(&0);
    let above_63: usize = pal_data.iter().filter(|&&v| v > 63).count();
    println!("\n--- unittem.pal raw analysis ---");
    println!(
        "Max byte: {}, bytes > 63: {} of {}",
        max_byte,
        above_63,
        pal_data.len()
    );
    if max_byte <= 63 {
        println!("Palette is 6-bit VGA (values 0-63)");
    } else {
        println!("Palette is 8-bit (values 0-255) — scaling as 6-bit would be WRONG!");
    }

    // Dump palette colors at key tiberium-used indices
    let pal = Palette::from_bytes(&pal_data).expect("parse palette");
    println!("\nUnit palette colors at tiberium-used indices (after 6-bit scaling):");
    for &idx in &[
        17u8, 20, 31, 33, 44, 50, 53, 60, 63, 64, 69, 74, 89, 99, 101, 124, 130, 151, 156, 176,
        180, 193,
    ] {
        let c = &pal.colors[idx as usize];
        println!(
            "  pal[{:3}] = ({:3}, {:3}, {:3}) a={}",
            idx, c.r, c.g, c.b, c.a
        );
    }
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn dustbowl_overlay_168_stays_non_resource() {
    let ra2_dir_str = ra2_dir();
    let ra2_dir: &Path = Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        println!("SKIP: RA2 dir not found at {}", ra2_dir_str);
        return;
    }

    let asset_manager: AssetManager = AssetManager::new(ra2_dir).expect("asset manager");
    let map_path = ra2_dir.join("Dustbowl.mmx");
    let map = map_file::load_mmx(&map_path).expect("load Dustbowl.mmx");
    let (rules_bytes, _) = asset_manager
        .get_with_source("rulesmd.ini")
        .or_else(|| asset_manager.get_with_source("rules.ini"))
        .expect("rules ini");
    let rules_ini: IniFile = IniFile::from_bytes(&rules_bytes).expect("parse rules ini");
    let reg: OverlayTypeRegistry = OverlayTypeRegistry::from_ini(&rules_ini);
    let count_168 = map.overlays.iter().filter(|e| e.overlay_id == 168).count();
    assert!(count_168 > 0, "expected overlay id 168 on Dustbowl");
    assert_eq!(reg.name(168), Some("SROCK01"));
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn print_overlay_id_mapping_for_goldst() {
    let ra2_dir_str = ra2_dir();
    let ra2_dir: &Path = Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        println!("SKIP: RA2 dir not found at {}", ra2_dir_str);
        return;
    }

    let asset_manager: AssetManager = AssetManager::new(ra2_dir).expect("asset manager");
    let map_path = ra2_dir.join("GoldSt.mmx");
    let map = match map_file::load_mmx(&map_path) {
        Ok(map) => map,
        Err(err) => {
            println!("SKIP: could not load GoldSt.mmx: {err}");
            return;
        }
    };
    let (rules_bytes, source) = asset_manager
        .get_with_source("rulesmd.ini")
        .or_else(|| asset_manager.get_with_source("rules.ini"))
        .expect("rules ini");
    println!("Rules source: {}", source);
    let rules_ini: IniFile = IniFile::from_bytes(&rules_bytes).expect("parse rules ini");
    let reg: OverlayTypeRegistry = OverlayTypeRegistry::from_ini(&rules_ini);

    let mut counts: HashMap<u8, u32> = HashMap::new();
    for e in &map.overlays {
        *counts.entry(e.overlay_id).or_insert(0) += 1;
    }
    let mut by_count: Vec<(u8, u32)> = counts.into_iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    println!("Unique overlay IDs in GoldSt: {}", by_count.len());
    for (id, count) in &by_count {
        let name = reg.name(*id).unwrap_or("(unmapped)");
        if *count >= 20 || name.starts_with("TIB") || name.starts_with("GEM") {
            println!("  id={:3} count={:4} name={}", id, count, name);
        }
    }

    println!("\nGoldSt ids in RA2 resource families:");
    for (id, count) in &by_count {
        let is_resource_family = matches!(
            *id,
            0x1B..=0x26 | 0x66..=0x79 | 0x7F..=0x92 | 0x93..=0xA6
        );
        if is_resource_family {
            println!(
                "  resource-family id={:3} count={:4} runtime_name={}",
                id,
                count,
                reg.name(*id).unwrap_or("(unmapped)")
            );
        }
    }
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn inspect_pips_shp() {
    let ra2_dir_str = ra2_dir();
    let ra2_dir: &Path = Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        println!("SKIP: RA2 dir not found at {}", ra2_dir_str);
        return;
    }

    let asset_manager: AssetManager = AssetManager::new(ra2_dir).expect("asset manager");

    // Load the unit palette to show actual RGB colors for pip indices.
    let pal = asset_manager
        .get("unittem.pal")
        .and_then(|d| Palette::from_bytes(&d).ok());

    for name in ["pips.shp", "pips2.shp"] {
        match asset_manager.get_with_source(name) {
            Some((data, source)) => {
                println!(
                    "\n=== {} ({} bytes, from: {}) ===",
                    name,
                    data.len(),
                    source
                );
                match ShpFile::from_bytes(&data) {
                    Ok(shp) => {
                        println!(
                            "  SHP: {}x{}, {} frames",
                            shp.width,
                            shp.height,
                            shp.frames.len()
                        );
                        for (i, f) in shp.frames.iter().enumerate() {
                            let non_zero = f.pixels.iter().filter(|&&p| p != 0).count();
                            let mut idx_counts: HashMap<u8, usize> = HashMap::new();
                            for &p in &f.pixels {
                                if p != 0 {
                                    *idx_counts.entry(p).or_insert(0) += 1;
                                }
                            }
                            let mut top: Vec<(u8, usize)> = idx_counts.into_iter().collect();
                            top.sort_by(|a, b| b.1.cmp(&a.1));
                            top.truncate(5);
                            // Show palette colors for the top indices.
                            let color_info: String = if let Some(ref p) = pal {
                                top.iter()
                                    .map(|(idx, _cnt)| {
                                        let c = &p.colors[*idx as usize];
                                        format!("{}=({},{},{})", idx, c.r, c.g, c.b)
                                    })
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            } else {
                                "no palette".to_string()
                            };
                            println!(
                                "  frame {:2}: {}x{} filled={:2}/{} colors: {}",
                                i,
                                f.frame_width,
                                f.frame_height,
                                non_zero,
                                f.pixels.len(),
                                color_info
                            );
                        }
                    }
                    Err(e) => println!("  parse error: {}", e),
                }
            }
            None => println!("{}: NOT FOUND", name),
        }
    }
}
