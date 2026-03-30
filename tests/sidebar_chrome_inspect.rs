//! Inspect sidebar chrome MIX archives (sidec01.mix, sidec02.mix).
//!
//! Loads each archive as a nested MIX from the AssetManager, lists all
//! entries, and parses any SHP files found to report frame dimensions.

use std::path::PathBuf;

use image::RgbaImage;
use vera20k::assets::asset_manager::AssetManager;
use vera20k::assets::mix_archive::MixArchive;
use vera20k::assets::mix_hash::mix_hash;
use vera20k::assets::pal_file::Palette;
use vera20k::assets::shp_file::ShpFile;

fn ra2_dir() -> String {
    std::env::var("RA2_DIR")
        .unwrap_or_else(|_| panic!("Set RA2_DIR env var to your RA2/YR install directory"))
}

/// Known filenames that might exist inside sidec01/sidec02.mix.
/// We hash each name and try to match against the MIX entry IDs.
const KNOWN_SIDEBAR_FILES: &[&str] = &[
    // SHP files
    "radar.shp",
    "radary.shp",
    "side1.shp",
    "side2.shp",
    "side3.shp",
    "side4.shp",
    "side1na.shp",
    "side2na.shp",
    "side3na.shp",
    "side4na.shp",
    "tab00.shp",
    "tab01.shp",
    "tab02.shp",
    "tab03.shp",
    "tab04.shp",
    "power.shp",
    "pips.shp",
    "pips2.shp",
    "clock.shp",
    "strip.shp",
    "bkgdlg.shp",
    "bkgdmd.shp",
    "bkgdsm.shp",
    "bkgdlgy.shp",
    "bkgdmdy.shp",
    "bkgdsmy.shp",
    "pwrbar.shp",
    "pwrdn.shp",
    "pwrup.shp",
    "sidebar.shp",
    "sbicons.shp",
    "sbcameos.shp",
    "tabs.shp",
    "tabsna.shp",
    "left.shp",
    "right.shp",
    "lgpanel.shp",
    "lgstrp.shp",
    "lgstick.shp",
    "lgbkgd.shp",
    "sbtabs.shp",
    "sidebarx.shp",
    "cmdbar.shp",
    "cmdbtns.shp",
    "hpips.shp",
    "vpips.shp",
    "repair.shp",
    "sell.shp",
    "power2.shp",
    "tabhigh.shp",
    "cameo.shp",
    "rstripb.shp",
    "lststub.shp",
    // Palette files
    "sidebar.pal",
    "uibkgd.pal",
    "uibkgdy.pal",
    "radaryuri.pal",
    "cameo.pal",
    "uniticon.pal",
    "grfxtd.pal",
    "grfxra.pal",
    "lib.pal",
    "ui.pal",
    // Other possible files
    "sidebar.ini",
];

fn known_sidebar_ids() -> std::collections::HashMap<i32, String> {
    let mut id_to_name: std::collections::HashMap<i32, String> = std::collections::HashMap::new();
    for &name in KNOWN_SIDEBAR_FILES {
        let id: i32 = mix_hash(name);
        id_to_name.insert(id, name.to_string());
    }
    id_to_name
}

fn save_unknown_shp_preview(
    data: &[u8],
    palette: &Palette,
    output_name: &str,
) -> Result<(), String> {
    let shp = ShpFile::from_bytes(data).map_err(|e| format!("parse shp: {e}"))?;
    if shp.frames.is_empty() {
        return Err("no frames".to_string());
    }
    let frame = &shp.frames[0];
    let rgba = shp
        .frame_to_rgba(0, palette)
        .map_err(|e| format!("decode rgba: {e}"))?;
    let image = RgbaImage::from_raw(frame.frame_width as u32, frame.frame_height as u32, rgba)
        .ok_or_else(|| "rgba size mismatch".to_string())?;
    image
        .save(output_name)
        .map_err(|e| format!("save png: {e}"))?;
    Ok(())
}

/// Inspect a single sidebar chrome MIX archive by name.
fn inspect_sidebar_mix(asset_manager: &AssetManager, mix_name: &str) {
    eprintln!("\n{}", "=".repeat(60));
    eprintln!("  Inspecting: {}", mix_name);
    eprintln!("{}", "=".repeat(60));

    // Get the raw MIX data from the asset manager
    let mix_data: Vec<u8> = match asset_manager.get(mix_name) {
        Some(data) => {
            eprintln!("  Loaded {} ({} bytes)", mix_name, data.len());
            data
        }
        None => {
            eprintln!("  NOT FOUND: {} — skipping", mix_name);
            return;
        }
    };

    // Parse it as a nested MIX archive
    let archive: MixArchive = match MixArchive::from_bytes(mix_data) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("  PARSE ERROR for {}: {}", mix_name, e);
            return;
        }
    };

    let entries = archive.entries();
    eprintln!("  Entry count: {}", entries.len());

    // Build a reverse lookup: hash ID -> known filename
    let id_to_name = known_sidebar_ids();

    // List all entries, tagging known ones
    eprintln!("\n  --- All entries in {} ---", mix_name);
    let mut matched_shps: Vec<(String, Vec<u8>)> = Vec::new();
    let mut matched_pals: Vec<(String, Vec<u8>)> = Vec::new();

    for (i, entry) in entries.iter().enumerate() {
        let label: String = match id_to_name.get(&entry.id) {
            Some(name) => {
                // Extract the file data for later SHP parsing
                if let Some(data) = archive.get_by_id(entry.id) {
                    let name_lower = name.to_lowercase();
                    if name_lower.ends_with(".shp") {
                        matched_shps.push((name.clone(), data.to_vec()));
                    } else if name_lower.ends_with(".pal") {
                        matched_pals.push((name.clone(), data.to_vec()));
                    }
                }
                format!("  => {}", name)
            }
            None => String::new(),
        };
        eprintln!(
            "  [{:3}] id={:#010X}  offset={:8}  size={:8}{}",
            i, entry.id as u32, entry.offset, entry.size, label
        );
    }

    // Report matched palettes
    if !matched_pals.is_empty() {
        eprintln!("\n  --- Palette files found ---");
        for (name, data) in &matched_pals {
            eprintln!("  {} — {} bytes", name, data.len());
        }
    }

    // Parse and report SHP files
    if !matched_shps.is_empty() {
        eprintln!("\n  --- SHP file details ---");
        for (name, data) in &matched_shps {
            match ShpFile::from_bytes(data) {
                Ok(shp) => {
                    eprintln!(
                        "\n  {}: {} frames, overall {}x{}",
                        name,
                        shp.frames.len(),
                        shp.width,
                        shp.height
                    );
                    for (fi, frame) in shp.frames.iter().enumerate() {
                        // Print first 5 frames individually, then summarize
                        if fi < 5 || fi == shp.frames.len() - 1 {
                            eprintln!(
                                "    frame {:3}: {}x{} at offset ({}, {}), {} pixels",
                                fi,
                                frame.frame_width,
                                frame.frame_height,
                                frame.frame_x,
                                frame.frame_y,
                                frame.pixels.len()
                            );
                        } else if fi == 5 {
                            eprintln!("    ... ({} more frames) ...", shp.frames.len() - 6);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("  {}: PARSE ERROR — {}", name, e);
                }
            }
        }
    }

    // Count unmatched entries
    let unmatched: usize = entries
        .iter()
        .filter(|e| !id_to_name.contains_key(&e.id))
        .count();
    eprintln!(
        "\n  Summary: {} total entries, {} matched by name, {} unknown",
        entries.len(),
        entries.len() - unmatched,
        unmatched
    );
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn sidebar_unknown_entries_probe() {
    let ra2_dir: PathBuf = PathBuf::from(ra2_dir());
    if !ra2_dir.exists() {
        eprintln!("SKIP: RA2 dir not found at {}", ra2_dir.display());
        return;
    }

    let asset_manager: AssetManager =
        AssetManager::new(&ra2_dir).expect("Failed to create AssetManager");
    let known_ids = known_sidebar_ids();

    for mix_name in ["sidec01.mix", "sidec02.mix", "sidec02md.mix"] {
        let Some(mix_data) = asset_manager.get(mix_name) else {
            eprintln!("SKIP: {} missing", mix_name);
            continue;
        };
        let archive = MixArchive::from_bytes(mix_data).expect("parse sidebar mix");
        eprintln!("\n{}", "=".repeat(60));
        eprintln!("  Unknown entry probe: {}", mix_name);
        eprintln!("{}", "=".repeat(60));

        for entry in archive.entries() {
            if known_ids.contains_key(&entry.id) {
                continue;
            }
            let Some(data) = archive.get_by_id(entry.id) else {
                continue;
            };
            if let Ok(shp) = ShpFile::from_bytes(data) {
                eprintln!(
                    "  UNKNOWN SHP id={:#010X} size={} frames={} overall={}x{}",
                    entry.id as u32,
                    entry.size,
                    shp.frames.len(),
                    shp.width,
                    shp.height
                );
                for (fi, frame) in shp.frames.iter().take(3).enumerate() {
                    eprintln!(
                        "    frame {:3}: {}x{} at ({}, {}) pixels={}",
                        fi,
                        frame.frame_width,
                        frame.frame_height,
                        frame.frame_x,
                        frame.frame_y,
                        frame.pixels.len()
                    );
                }
                if shp.frames.len() > 3 {
                    let last = shp.frames.last().expect("last frame");
                    eprintln!(
                        "    frame {:3}: {}x{} at ({}, {}) pixels={}",
                        shp.frames.len() - 1,
                        last.frame_width,
                        last.frame_height,
                        last.frame_x,
                        last.frame_y,
                        last.pixels.len()
                    );
                }
            }
        }
    }
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn export_suspicious_unknown_sidebar_shps() {
    let ra2_dir: PathBuf = PathBuf::from(ra2_dir());
    if !ra2_dir.exists() {
        eprintln!("SKIP: RA2 dir not found at {}", ra2_dir.display());
        return;
    }

    let asset_manager: AssetManager =
        AssetManager::new(&ra2_dir).expect("Failed to create AssetManager");
    let sidec01 = MixArchive::from_bytes(
        asset_manager
            .get("sidec01.mix")
            .expect("sidec01.mix not found"),
    )
    .expect("parse sidec01.mix");
    let sidec02 = MixArchive::from_bytes(
        asset_manager
            .get("sidec02.mix")
            .expect("sidec02.mix not found"),
    )
    .expect("parse sidec02.mix");
    let sidebar_pal = Palette::from_bytes(
        &asset_manager
            .get("sidebar.pal")
            .expect("sidebar.pal not found"),
    )
    .expect("parse sidebar.pal");
    let uibkgd_pal = Palette::from_bytes(
        &asset_manager
            .get("uibkgd.pal")
            .expect("uibkgd.pal not found"),
    )
    .expect("parse uibkgd.pal");

    let suspicious: &[(i32, &str, &Palette)] = &[
        (
            0xD508C1A4u32 as i32,
            "debug_unknown_allied_856x32_uibkgd.png",
            &uibkgd_pal,
        ),
        (
            0xF0F1CE8Du32 as i32,
            "debug_unknown_allied_168x32_uibkgd.png",
            &uibkgd_pal,
        ),
        (
            0x7AEBAE6Bu32 as i32,
            "debug_unknown_allied_168x63_sidebar.png",
            &sidebar_pal,
        ),
        (
            0xB0259C24u32 as i32,
            "debug_unknown_allied_168x50_sidebar.png",
            &sidebar_pal,
        ),
        (
            0x7637D6E1u32 as i32,
            "debug_unknown_allied_168x16_uibkgd.png",
            &uibkgd_pal,
        ),
    ];

    for (id, name, palette) in suspicious {
        let data = sidec01
            .get_by_id(*id)
            .unwrap_or_else(|| panic!("missing suspicious id {id:#010X}"));
        save_unknown_shp_preview(data, palette, name)
            .unwrap_or_else(|e| panic!("failed to export {name}: {e}"));
        eprintln!("exported {} from id {:#010X}", name, *id as u32);
    }

    let suspicious_soviet: &[(i32, &str, &Palette)] = &[
        (
            0xD508C1A4u32 as i32,
            "debug_unknown_soviet_856x32_uibkgd.png",
            &uibkgd_pal,
        ),
        (
            0xF0F1CE8Du32 as i32,
            "debug_unknown_soviet_168x32_uibkgd.png",
            &uibkgd_pal,
        ),
        (
            0x7AEBAE6Bu32 as i32,
            "debug_unknown_soviet_168x63_sidebar.png",
            &sidebar_pal,
        ),
        (
            0xB0259C24u32 as i32,
            "debug_unknown_soviet_168x50_sidebar.png",
            &sidebar_pal,
        ),
        (
            0x7637D6E1u32 as i32,
            "debug_unknown_soviet_168x16_uibkgd.png",
            &uibkgd_pal,
        ),
    ];

    for (id, name, palette) in suspicious_soviet {
        let data = sidec02
            .get_by_id(*id)
            .unwrap_or_else(|| panic!("missing suspicious Soviet id {id:#010X}"));
        save_unknown_shp_preview(data, palette, name)
            .unwrap_or_else(|e| panic!("failed to export {name}: {e}"));
        eprintln!("exported {} from sidec02 id {:#010X}", name, *id as u32);
    }
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn sidebar_chrome_inspect() {
    let ra2_dir: PathBuf = PathBuf::from(ra2_dir());
    if !ra2_dir.exists() {
        eprintln!("SKIP: RA2 dir not found at {}", ra2_dir.display());
        return;
    }

    let asset_manager: AssetManager =
        AssetManager::new(&ra2_dir).expect("Failed to create AssetManager");

    // Inspect Allied sidebar chrome
    inspect_sidebar_mix(&asset_manager, "sidec01.mix");

    // Inspect Soviet sidebar chrome
    inspect_sidebar_mix(&asset_manager, "sidec02.mix");

    // Also check for YR variants
    inspect_sidebar_mix(&asset_manager, "sidec01md.mix");
    inspect_sidebar_mix(&asset_manager, "sidec02md.mix");
}

/// Scan side2.shp frame 0 for dark rectangular regions (cameo slot positions).
///
/// A "dark" pixel: R < 40 && G < 40 && B < 40 && A > 200.
/// For each column x, finds the vertical span (min_y, max_y) of dark pixels.
/// For each row y, finds the horizontal span (min_x, max_x) of dark pixels.
/// Groups contiguous dark columns into bounding boxes to identify cameo slots.
#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn sidebar_chrome_slot_positions() {
    let ra2_dir: PathBuf = PathBuf::from(ra2_dir());
    if !ra2_dir.exists() {
        eprintln!("SKIP: RA2 dir not found at {}", ra2_dir.display());
        return;
    }

    let asset_manager: AssetManager =
        AssetManager::new(&ra2_dir).expect("Failed to create AssetManager");

    // Load side2.shp from the asset manager (it lives inside sidec01.mix)
    let shp_data: Vec<u8> = asset_manager
        .get("side2.shp")
        .expect("side2.shp not found in asset manager");
    let pal_data: Vec<u8> = asset_manager
        .get("sidebar.pal")
        .expect("sidebar.pal not found in asset manager");

    let shp: ShpFile = ShpFile::from_bytes(&shp_data).expect("parse side2.shp");
    let pal: Palette = Palette::from_bytes(&pal_data).expect("parse sidebar.pal");

    let frame = &shp.frames[0];
    let width: u32 = frame.frame_width as u32;
    let height: u32 = frame.frame_height as u32;

    eprintln!("\nside2.shp frame 0: {}x{}", width, height);

    let rgba: Vec<u8> = shp.frame_to_rgba(0, &pal).expect("decode frame 0");

    // Save debug PNG
    let image: RgbaImage =
        RgbaImage::from_raw(width, height, rgba.clone()).expect("build rgba image");
    image.save("debug_side2.png").expect("save debug_side2.png");
    eprintln!("Saved debug_side2.png");

    // Helper: test if pixel at (x, y) is "dark"
    let is_dark = |x: u32, y: u32| -> bool {
        let idx: usize = ((y * width + x) * 4) as usize;
        let r: u8 = rgba[idx];
        let g: u8 = rgba[idx + 1];
        let b: u8 = rgba[idx + 2];
        let a: u8 = rgba[idx + 3];
        r < 40 && g < 40 && b < 40 && a > 200
    };

    // --- Per-column analysis: for each x, find (min_y, max_y) of dark pixels ---
    eprintln!("\n--- Per-column dark pixel spans ---");
    // Store (min_y, max_y) per column; None if no dark pixels in that column
    let mut col_spans: Vec<Option<(u32, u32)>> = Vec::with_capacity(width as usize);
    for x in 0..width {
        let mut min_y: Option<u32> = None;
        let mut max_y: Option<u32> = None;
        for y in 0..height {
            if is_dark(x, y) {
                match min_y {
                    None => {
                        min_y = Some(y);
                        max_y = Some(y);
                    }
                    Some(_) => {
                        max_y = Some(y);
                    }
                }
            }
        }
        let span: Option<(u32, u32)> = match (min_y, max_y) {
            (Some(mn), Some(mx)) => Some((mn, mx)),
            _ => None,
        };
        col_spans.push(span);
        if let Some((mn, mx)) = span {
            eprintln!(
                "  col {:3}: dark y=[{}, {}]  height={}",
                x,
                mn,
                mx,
                mx - mn + 1
            );
        }
    }

    // --- Per-row analysis: for each y, find (min_x, max_x) of dark pixels ---
    eprintln!("\n--- Per-row dark pixel spans ---");
    let mut row_spans: Vec<Option<(u32, u32)>> = Vec::with_capacity(height as usize);
    for y in 0..height {
        let mut min_x: Option<u32> = None;
        let mut max_x: Option<u32> = None;
        for x in 0..width {
            if is_dark(x, y) {
                match min_x {
                    None => {
                        min_x = Some(x);
                        max_x = Some(x);
                    }
                    Some(_) => {
                        max_x = Some(x);
                    }
                }
            }
        }
        let span: Option<(u32, u32)> = match (min_x, max_x) {
            (Some(mn), Some(mx)) => Some((mn, mx)),
            _ => None,
        };
        row_spans.push(span);
        if let Some((mn, mx)) = span {
            eprintln!(
                "  row {:3}: dark x=[{}, {}]  width={}",
                y,
                mn,
                mx,
                mx - mn + 1
            );
        }
    }

    // --- Group contiguous dark columns into rectangular regions ---
    // A dark column group is a run of consecutive x values that all have dark spans.
    // Within each group, the bounding box is: x_start..x_end, min(all min_y)..max(all max_y).
    eprintln!("\n--- Detected dark rectangular regions (cameo slots) ---");
    let mut regions: Vec<(u32, u32, u32, u32)> = Vec::new(); // (x, y, w, h)
    let mut group_start: Option<u32> = None;
    let mut group_min_y: u32 = u32::MAX;
    let mut group_max_y: u32 = 0;

    for x in 0..=width {
        let has_dark: bool = if x < width {
            col_spans[x as usize].is_some()
        } else {
            false // sentinel to flush last group
        };

        if has_dark {
            let (mn, mx) = col_spans[x as usize].unwrap();
            if group_start.is_none() {
                group_start = Some(x);
                group_min_y = mn;
                group_max_y = mx;
            } else {
                if mn < group_min_y {
                    group_min_y = mn;
                }
                if mx > group_max_y {
                    group_max_y = mx;
                }
            }
        } else if let Some(start) = group_start {
            // End of a contiguous group
            let region_w: u32 = x - start;
            let region_h: u32 = group_max_y - group_min_y + 1;
            regions.push((start, group_min_y, region_w, region_h));
            group_start = None;
            group_min_y = u32::MAX;
            group_max_y = 0;
        }
    }

    // Report all regions found
    for (i, &(x, y, w, h)) in regions.iter().enumerate() {
        eprintln!(
            "  Region {}: x={}, y={}, w={}, h={}  (x_end={}, y_end={})",
            i,
            x,
            y,
            w,
            h,
            x + w - 1,
            y + h - 1
        );
    }

    // Also detect separate horizontal spans per row to see if there are two
    // side-by-side slots (left cameo + right cameo).
    eprintln!("\n--- Per-row horizontal gap analysis (looking for two slots) ---");
    for y in 0..height {
        // Find all dark pixel runs in this row
        let mut runs: Vec<(u32, u32)> = Vec::new();
        let mut run_start: Option<u32> = None;
        for x in 0..=width {
            let dark: bool = if x < width { is_dark(x, y) } else { false };
            if dark {
                if run_start.is_none() {
                    run_start = Some(x);
                }
            } else if let Some(start) = run_start {
                runs.push((start, x - 1));
                run_start = None;
            }
        }
        if runs.len() >= 2 {
            let run_strs: Vec<String> = runs
                .iter()
                .map(|(s, e)| format!("[{}-{} w={}]", s, e, e - s + 1))
                .collect();
            eprintln!(
                "  row {:3}: {} dark runs: {}",
                y,
                runs.len(),
                run_strs.join(", ")
            );
        }
    }

    // Final summary
    eprintln!("\n=== SUMMARY: {} dark region(s) found ===", regions.len());
    for (i, &(x, y, w, h)) in regions.iter().enumerate() {
        eprintln!("  Slot {}: (x={}, y={}, w={}, h={})", i, x, y, w, h);
    }
}

/// Inspect tab00-03.shp, repair.shp, sell.shp, and power.shp from sidec01.mix.
///
/// For each SHP file: prints frame count, overall width/height, and per-frame
/// dimensions (frame_width, frame_height, frame_x, frame_y).
#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn inspect_tab_and_button_frames() {
    let ra2_dir: PathBuf = PathBuf::from(ra2_dir());
    if !ra2_dir.exists() {
        eprintln!("SKIP: RA2 dir not found at {}", ra2_dir.display());
        return;
    }

    let asset_manager: AssetManager =
        AssetManager::new(&ra2_dir).expect("Failed to create AssetManager");

    // Load sidec01.mix as a nested MIX to extract files from it
    let mix_data: Vec<u8> = asset_manager
        .get("sidec01.mix")
        .expect("sidec01.mix not found");
    let archive: MixArchive =
        MixArchive::from_bytes(mix_data).expect("Failed to parse sidec01.mix");

    let files_to_inspect: &[&str] = &[
        "tab00.shp",
        "tab01.shp",
        "tab02.shp",
        "tab03.shp",
        "repair.shp",
        "sell.shp",
        "power.shp",
    ];

    eprintln!("\n{}", "=".repeat(70));
    eprintln!("  Tab & button SHP inspection from sidec01.mix");
    eprintln!("{}", "=".repeat(70));

    for &filename in files_to_inspect {
        let hash_id: i32 = mix_hash(filename);
        let data: Vec<u8> = match archive.get_by_id(hash_id) {
            Some(d) => d.to_vec(),
            None => {
                eprintln!(
                    "\n  {} — NOT FOUND in sidec01.mix (hash={:#010X})",
                    filename, hash_id as u32
                );
                continue;
            }
        };

        match ShpFile::from_bytes(&data) {
            Ok(shp) => {
                eprintln!(
                    "\n  {}: {} frames, overall {}x{}, data={} bytes",
                    filename,
                    shp.frames.len(),
                    shp.width,
                    shp.height,
                    data.len()
                );
                for (fi, frame) in shp.frames.iter().enumerate() {
                    eprintln!(
                        "    frame {:3}: {}x{} at ({}, {}), {} pixels",
                        fi,
                        frame.frame_width,
                        frame.frame_height,
                        frame.frame_x,
                        frame.frame_y,
                        frame.pixels.len()
                    );
                }
            }
            Err(e) => {
                eprintln!("\n  {}: PARSE ERROR — {}", filename, e);
            }
        }
    }

    eprintln!("\n{}", "=".repeat(70));
}

/// Export all frames of unknown SHP entries that could be power bar pieces.
/// Also tries "powerp.shp" name hash against sidec01.mix entries.
#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn export_power_bar_candidates() {
    let ra2_dir: PathBuf = PathBuf::from(ra2_dir());
    if !ra2_dir.exists() {
        eprintln!("SKIP: RA2 dir not found at {}", ra2_dir.display());
        return;
    }

    let asset_manager: AssetManager =
        AssetManager::new(&ra2_dir).expect("Failed to create AssetManager");
    let mix_data: Vec<u8> = asset_manager
        .get("sidec01.mix")
        .expect("sidec01.mix not found");
    let archive: MixArchive =
        MixArchive::from_bytes(mix_data).expect("Failed to parse sidec01.mix");
    let sidebar_pal: Palette = Palette::from_bytes(
        &asset_manager
            .get("sidebar.pal")
            .expect("sidebar.pal not found"),
    )
    .expect("parse sidebar.pal");

    // Try powerp.shp hash
    let powerp_hash: i32 = mix_hash("powerp.shp");
    eprintln!(
        "powerp.shp hash = {:#010X}, found = {}",
        powerp_hash as u32,
        archive.get_by_id(powerp_hash).is_some()
    );

    // Also try other power-related name hashes
    let power_names: &[&str] = &[
        "powerp.shp",
        "powerp.shp",
        "pwrbar.shp",
        "pbar.shp",
        "pbargrn.shp",
        "pbarred.shp",
        "pwrbaron.shp",
        "pwrbaroff.shp",
        "power2.shp",
        "pwrup.shp",
        "pwrdn.shp",
        "powerbar.shp",
    ];
    for name in power_names {
        let h: i32 = mix_hash(name);
        let found: bool = archive.get_by_id(h).is_some();
        if found {
            eprintln!("  FOUND: {} (hash={:#010X})", name, h as u32);
        }
    }

    // Export all frames of suspicious unknown SHPs
    let candidates: &[(u32, &str)] = &[
        (0xA281ADD0, "66x122_20f"), // tall narrow — power bar?
        (0xB9B633F1, "8x2_1f"),     // tiny strip
        (0xC33D9320, "12x2_5f"),    // 5-frame strip
        (0xEE4B62BD, "75x15_2f"),   // 75x15
        (0xC5C7F91C, "46x25_3f"),   // 46x25
        (0xD29D01C1, "46x25_3f_b"), // another 46x25
        (0xBC1580C2, "28x32_1f"),   // 28x32
    ];

    for (id, label) in candidates {
        let data: &[u8] = match archive.get_by_id(*id as i32) {
            Some(d) => d,
            None => {
                eprintln!("MISSING {label} id={id:#010X}");
                continue;
            }
        };
        let shp: ShpFile = match ShpFile::from_bytes(data) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("PARSE FAIL {label}: {e}");
                continue;
            }
        };
        eprintln!(
            "\n{label} (id={id:#010X}): {}x{}, {} frames",
            shp.width,
            shp.height,
            shp.frames.len()
        );
        for i in 0..shp.frames.len() {
            let f = &shp.frames[i];
            let fw: u16 = f.frame_width;
            let fh: u16 = f.frame_height;
            if fw == 0 || fh == 0 {
                eprintln!("  frame {i}: empty");
                continue;
            }
            // Render onto full canvas
            let rgba: Vec<u8> = match shp.frame_to_rgba(i, &sidebar_pal) {
                Ok(r) => r,
                Err(_) => continue,
            };
            // Blit frame onto canvas-sized image
            let cw: u32 = shp.width as u32;
            let ch: u32 = shp.height as u32;
            let mut canvas: Vec<u8> = vec![0u8; (cw * ch * 4) as usize];
            let fx: u32 = f.frame_x as u32;
            let fy: u32 = f.frame_y as u32;
            for row in 0..fh as u32 {
                let src_off: usize = (row * fw as u32 * 4) as usize;
                let dst_off: usize = (((fy + row) * cw + fx) * 4) as usize;
                let len: usize = (fw as u32 * 4) as usize;
                if src_off + len <= rgba.len() && dst_off + len <= canvas.len() {
                    canvas[dst_off..dst_off + len].copy_from_slice(&rgba[src_off..src_off + len]);
                }
            }
            if let Some(img) = RgbaImage::from_raw(cw, ch, canvas) {
                let name: String = format!("debug_pwr_{label}_f{i}.png");
                let _ = img.save(&name);
                eprintln!(
                    "  frame {i}: {fw}x{fh} at ({},{}) -> {name}",
                    f.frame_x, f.frame_y
                );
            }
        }
    }
}

/// Dump raw pixel RGBA values for each powerp.shp frame to see transparency/spacing.
#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn dump_powerp_pixels() {
    let ra2_dir: PathBuf = PathBuf::from(ra2_dir());
    if !ra2_dir.exists() {
        eprintln!("SKIP: RA2 dir not found");
        return;
    }
    let asset_manager: AssetManager =
        AssetManager::new(&ra2_dir).expect("Failed to create AssetManager");
    let mix_data: Vec<u8> = asset_manager
        .get("sidec01.mix")
        .expect("sidec01.mix not found");
    let archive: MixArchive =
        MixArchive::from_bytes(mix_data).expect("Failed to parse sidec01.mix");
    let sidebar_pal: Palette = Palette::from_bytes(
        &asset_manager
            .get("sidebar.pal")
            .expect("sidebar.pal not found"),
    )
    .expect("parse sidebar.pal");

    let powerp_hash: i32 = mix_hash("powerp.shp");
    let data: &[u8] = archive
        .get_by_id(powerp_hash)
        .expect("powerp.shp not found");
    let shp: ShpFile = ShpFile::from_bytes(data).expect("parse powerp.shp");
    eprintln!(
        "powerp.shp: {}x{}, {} frames",
        shp.width,
        shp.height,
        shp.frames.len()
    );

    for i in 0..shp.frames.len() {
        let f = &shp.frames[i];
        let fw: u32 = f.frame_width as u32;
        let fh: u32 = f.frame_height as u32;
        eprintln!("\nFrame {i}: {fw}x{fh} at ({},{})", f.frame_x, f.frame_y);
        if fw == 0 || fh == 0 {
            eprintln!("  (empty)");
            continue;
        }
        let rgba: Vec<u8> = shp.frame_to_rgba(i, &sidebar_pal).expect("render frame");
        for y in 0..fh {
            let mut row_str = String::new();
            for x in 0..fw {
                let off: usize = ((y * fw + x) * 4) as usize;
                let r: u8 = rgba[off];
                let g: u8 = rgba[off + 1];
                let b: u8 = rgba[off + 2];
                let a: u8 = rgba[off + 3];
                if a == 0 {
                    row_str.push_str("______ ");
                } else {
                    row_str.push_str(&format!("{r:02x}{g:02x}{b:02x} "));
                }
            }
            eprintln!("  row{y}: {row_str}");
        }
    }

    // Also dump pwrlvl.shp pixels
    let pwrlvl_hash: i32 = mix_hash("pwrlvl.shp");
    match archive.get_by_id(pwrlvl_hash) {
        Some(data) => match ShpFile::from_bytes(data) {
            Ok(shp) => {
                eprintln!(
                    "\npwrlvl.shp: {}x{}, {} frames",
                    shp.width,
                    shp.height,
                    shp.frames.len()
                );
                for i in 0..shp.frames.len() {
                    let f = &shp.frames[i];
                    let fw: u32 = f.frame_width as u32;
                    let fh: u32 = f.frame_height as u32;
                    eprintln!("\n  Frame {i}: {fw}x{fh} at ({},{})", f.frame_x, f.frame_y);
                    if fw == 0 || fh == 0 {
                        continue;
                    }
                    let rgba: Vec<u8> = shp.frame_to_rgba(i, &sidebar_pal).expect("render");
                    for y in 0..fh {
                        let mut row_str = String::new();
                        for x in 0..fw {
                            let off: usize = ((y * fw + x) * 4) as usize;
                            let r: u8 = rgba[off];
                            let g: u8 = rgba[off + 1];
                            let b: u8 = rgba[off + 2];
                            let a: u8 = rgba[off + 3];
                            if a == 0 {
                                row_str.push_str("______ ");
                            } else {
                                row_str.push_str(&format!("{r:02x}{g:02x}{b:02x} "));
                            }
                        }
                        eprintln!("    row{y}: {row_str}");
                    }
                }
            }
            Err(e) => eprintln!("\npwrlvl.shp: parse error: {e}"),
        },
        None => eprintln!("\npwrlvl.shp: NOT FOUND in sidec01.mix"),
    }
}
