#[path = "../src/bin/mix_browser_data.rs"]
mod mix_browser_data;

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use image::RgbaImage;
use vera20k::assets::asset_manager::AssetManager;
use vera20k::assets::pal_file::Palette;
use vera20k::assets::shp_file::ShpFile;

fn ra2_dir() -> String {
    std::env::var("RA2_DIR")
        .unwrap_or_else(|_| panic!("Set RA2_DIR env var to your RA2/YR install directory"))
}
const ROOT_DIR: &str = env!("CARGO_MANIFEST_DIR");
const OUTPUT_DIR: &str = "exported_shp_pngs";
const INDEX_FILE: &str = "index.tsv";
const SHEET_PADDING: u32 = 4;
const SHEET_GAP: u32 = 4;
const CHECKER_SIZE: u32 = 8;
const CHECKER_LIGHT: [u8; 4] = [32, 32, 32, 255];
const CHECKER_DARK: [u8; 4] = [20, 20, 20, 255];
const MAX_SHEET_WIDTH: u32 = 2048;

fn save_rgba_png(path: &Path, rgba: Vec<u8>, width: u32, height: u32) {
    let image = RgbaImage::from_raw(width, height, rgba).expect("rgba image");
    image.save(path).expect("save png");
}

fn make_checkerboard(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let color = if ((x / CHECKER_SIZE) + (y / CHECKER_SIZE)).is_multiple_of(2) {
                CHECKER_LIGHT
            } else {
                CHECKER_DARK
            };
            let idx = ((y * width + x) * 4) as usize;
            rgba[idx] = color[0];
            rgba[idx + 1] = color[1];
            rgba[idx + 2] = color[2];
            rgba[idx + 3] = color[3];
        }
    }
    rgba
}

fn blit_rgba(
    dst: &mut [u8],
    dst_width: u32,
    dst_height: u32,
    dst_x: u32,
    dst_y: u32,
    src: &[u8],
    src_width: u32,
    src_height: u32,
) {
    for y in 0..src_height {
        let target_y = dst_y + y;
        if target_y >= dst_height {
            continue;
        }
        for x in 0..src_width {
            let target_x = dst_x + x;
            if target_x >= dst_width {
                continue;
            }
            let src_idx = ((y * src_width + x) * 4) as usize;
            let dst_idx = ((target_y * dst_width + target_x) * 4) as usize;
            let alpha = src[src_idx + 3];
            if alpha == 0 {
                continue;
            }
            dst[dst_idx] = src[src_idx];
            dst[dst_idx + 1] = src[src_idx + 1];
            dst[dst_idx + 2] = src[src_idx + 2];
            dst[dst_idx + 3] = alpha;
        }
    }
}

fn candidate_shp_names() -> Vec<String> {
    let mut names = BTreeSet::new();
    for (name, _) in mix_browser_data::build_hash_dictionary() {
        if name.ends_with(".shp") {
            names.insert(name.to_ascii_lowercase());
        }
    }

    for extra in [
        "pipbrd.shp",
        "place.shp",
        "mouse.sha",
        "radary.shp",
        "radar.shp",
        "pips.shp",
        "pips2.shp",
        "power.shp",
        "repair.shp",
        "sell.shp",
        "tabs.shp",
    ] {
        if extra.ends_with(".shp") {
            names.insert(extra.to_string());
        }
    }

    names.into_iter().collect()
}

fn palette_candidates(asset_name: &str, source: &str) -> Vec<&'static str> {
    let lower = asset_name.to_ascii_lowercase();
    let source_lower = source.to_ascii_lowercase();
    let mut names = Vec::new();

    if lower == "radary.shp" {
        names.push("radaryuri.pal");
    }
    if lower.contains("mouse") || lower.contains("cursor") || lower.contains("pointer") {
        names.push("mousepal.pal");
    }

    let is_sidebar_ui = lower.starts_with("side")
        || lower.starts_with("tab")
        || lower.contains("radar")
        || lower.contains("power")
        || lower.contains("repair")
        || lower.contains("sell")
        || lower.contains("clock")
        || lower.contains("credits")
        || lower.contains("dialog")
        || lower.contains("title")
        || lower.contains("button")
        || lower.contains("btn")
        || lower.contains("scroll")
        || lower.contains("tooltip")
        || lower.contains("menu")
        || lower.contains("sidebar")
        || source_lower.contains("sidec");
    if is_sidebar_ui {
        names.extend(["sidebar.pal", "uibkgd.pal", "uibkgdy.pal"]);
    }

    if lower.contains("pip")
        || lower == "place.shp"
        || lower.contains("health")
        || lower.contains("select")
        || lower.contains("rank")
        || lower.contains("vet")
        || lower.contains("elite")
        || lower.contains("beacon")
        || lower.contains("waypoint")
    {
        names.push("unittem.pal");
    }

    names.extend([
        "unittem.pal",
        "temperat.pal",
        "isotem.pal",
        "uniturb.pal",
        "unitsno.pal",
        "unitdes.pal",
        "unitlun.pal",
        "anim.pal",
        "sidebar.pal",
    ]);

    let mut deduped = Vec::new();
    for name in names {
        if !deduped.contains(&name) {
            deduped.push(name);
        }
    }
    deduped
}

fn choose_palette(
    asset_manager: &AssetManager,
    asset_name: &str,
    source: &str,
) -> Option<(Palette, String)> {
    for palette_name in palette_candidates(asset_name, source) {
        let Some(palette_bytes) = asset_manager.get(palette_name) else {
            continue;
        };
        let Ok(palette) = Palette::from_bytes(&palette_bytes) else {
            continue;
        };
        return Some((palette, palette_name.to_string()));
    }
    None
}

fn build_sheet_rgba(shp: &ShpFile, palette: &Palette) -> Option<(Vec<u8>, u32, u32)> {
    let cell_width = shp.width as u32;
    let cell_height = shp.height as u32;
    if cell_width == 0 || cell_height == 0 || shp.frames.is_empty() {
        return None;
    }

    let frame_count = shp.frames.len() as u32;
    let preferred_cols = (frame_count as f64).sqrt().ceil() as u32;
    let cell_span = cell_width + SHEET_GAP;
    let max_cols_by_width = ((MAX_SHEET_WIDTH - SHEET_PADDING * 2) / cell_span.max(1)).max(1);
    let cols = preferred_cols.clamp(1, max_cols_by_width.max(1));
    let rows = frame_count.div_ceil(cols);

    let sheet_width = SHEET_PADDING * 2 + cols * cell_width + cols.saturating_sub(1) * SHEET_GAP;
    let sheet_height = SHEET_PADDING * 2 + rows * cell_height + rows.saturating_sub(1) * SHEET_GAP;
    let mut sheet_rgba = make_checkerboard(sheet_width, sheet_height);

    for (frame_idx, frame) in shp.frames.iter().enumerate() {
        let rgba = shp.frame_to_rgba(frame_idx, palette).ok()?;
        let frame_col = frame_idx as u32 % cols;
        let frame_row = frame_idx as u32 / cols;
        let dst_x = SHEET_PADDING + frame_col * (cell_width + SHEET_GAP) + frame.frame_x as u32;
        let dst_y = SHEET_PADDING + frame_row * (cell_height + SHEET_GAP) + frame.frame_y as u32;
        blit_rgba(
            &mut sheet_rgba,
            sheet_width,
            sheet_height,
            dst_x,
            dst_y,
            &rgba,
            frame.frame_width as u32,
            frame.frame_height as u32,
        );
    }

    Some((sheet_rgba, sheet_width, sheet_height))
}

fn output_name(asset_name: &str) -> String {
    asset_name
        .trim_end_matches(".shp")
        .to_ascii_lowercase()
        .replace('.', "_")
        + ".png"
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn export_identified_shp_pngs() {
    let ra2_dir = PathBuf::from(ra2_dir());
    if !ra2_dir.exists() {
        eprintln!("SKIP: RA2 dir not found at {}", ra2_dir.display());
        return;
    }

    let root_dir = PathBuf::from(ROOT_DIR);
    let output_dir = root_dir.join(OUTPUT_DIR);
    if output_dir.exists() {
        fs::remove_dir_all(&output_dir).expect("clear previous export dir");
    }
    fs::create_dir_all(&output_dir).expect("create export dir");

    let asset_manager = AssetManager::new(&ra2_dir).expect("asset manager");
    let candidate_names = candidate_shp_names();

    let mut exported = 0usize;
    let mut found = 0usize;
    let mut skipped_parse = 0usize;
    let mut skipped_palette = 0usize;
    let mut manifest = String::from(
        "asset\tpalette\tsource\tframes\tcell_width\tcell_height\tsheet_width\tsheet_height\n",
    );

    for asset_name in candidate_names {
        let Some((shp_bytes, source)) = asset_manager.get_with_source(&asset_name) else {
            continue;
        };
        found += 1;

        let shp = match ShpFile::from_bytes(&shp_bytes) {
            Ok(shp) => shp,
            Err(err) => {
                skipped_parse += 1;
                eprintln!("SKIP parse {} from {}: {}", asset_name, source, err);
                continue;
            }
        };

        let Some((palette, palette_name)) = choose_palette(&asset_manager, &asset_name, &source)
        else {
            skipped_palette += 1;
            eprintln!("SKIP palette {} from {}", asset_name, source);
            continue;
        };

        let Some((sheet_rgba, sheet_width, sheet_height)) = build_sheet_rgba(&shp, &palette) else {
            eprintln!("SKIP empty {}", asset_name);
            continue;
        };

        let output_path = output_dir.join(output_name(&asset_name));
        save_rgba_png(&output_path, sheet_rgba, sheet_width, sheet_height);
        let _ = writeln!(
            manifest,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            asset_name,
            palette_name,
            source,
            shp.frames.len(),
            shp.width,
            shp.height,
            sheet_width,
            sheet_height
        );
        exported += 1;
        eprintln!(
            "  exported {:<20} frames={:<3} palette={:<14} -> {}",
            asset_name,
            shp.frames.len(),
            palette_name,
            output_path.display()
        );
    }

    let index_path = output_dir.join(INDEX_FILE);
    fs::write(&index_path, manifest).expect("write index");
    eprintln!(
        "Exported {} SHP sheets ({} found, {} parse skips, {} palette skips) -> {}",
        exported,
        found,
        skipped_parse,
        skipped_palette,
        output_dir.display()
    );

    assert!(exported > 0, "should export at least one SHP sheet");
    assert!(output_dir.join("pipbrd.png").exists());
    assert!(index_path.exists());
}
