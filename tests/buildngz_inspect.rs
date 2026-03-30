//! Inspect BUILDNGZ.SHA to understand its contents for Z-buffer implementation.

use vera20k::assets::asset_manager::AssetManager;
use vera20k::assets::shp_file::ShpFile;
use std::path::Path;

fn ra2_dir() -> String {
    std::env::var("RA2_DIR").expect("Set RA2_DIR to your RA2/YR install directory")
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn inspect_buildngz() {
    let mut asset_manager = AssetManager::new(Path::new(&ra2_dir())).expect("AssetManager");
    // Try both extensions
    let data = asset_manager
        .get("buildngz.sha")
        .or_else(|| asset_manager.get("buildngz.shp"))
        .expect("BUILDNGZ.SHA not found in any MIX archive");

    println!("=== BUILDNGZ.SHA ===");
    println!("File size: {} bytes", data.len());

    let shp = ShpFile::from_bytes(&data).expect("Failed to parse BUILDNGZ.SHA as SHP");
    println!(
        "SHP header: width={}, height={}, frame_count={}",
        shp.width,
        shp.height,
        shp.frames.len()
    );
    println!();

    for (i, frame) in shp.frames.iter().enumerate() {
        let non_zero: usize = frame.pixels.iter().filter(|&&p| p != 0).count();
        let total = frame.pixels.len();

        // Value distribution for non-zero pixels
        let mut min_val: u8 = 255;
        let mut max_val: u8 = 0;
        let mut sum: u64 = 0;
        let mut count: u64 = 0;
        for &p in &frame.pixels {
            if p != 0 {
                min_val = min_val.min(p);
                max_val = max_val.max(p);
                sum += p as u64;
                count += 1;
            }
        }
        let avg = if count > 0 {
            sum as f64 / count as f64
        } else {
            0.0
        };

        // After -65 remap
        let min_remapped = min_val as i16 - 65;
        let max_remapped = max_val as i16 - 65;
        let avg_remapped = avg - 65.0;

        println!(
            "Frame {}: {}x{} at ({},{})  pixels={} non_zero={} ({:.1}%)",
            i,
            frame.frame_width,
            frame.frame_height,
            frame.frame_x,
            frame.frame_y,
            total,
            non_zero,
            if total > 0 {
                non_zero as f64 / total as f64 * 100.0
            } else {
                0.0
            }
        );
        if count > 0 {
            println!(
                "  Raw values: min={} max={} avg={:.1}",
                min_val, max_val, avg
            );
            println!(
                "  After -65 remap: min={} max={} avg={:.1}",
                min_remapped, max_remapped, avg_remapped
            );
        }

        // Print a visual slice of the middle row to see the depth gradient
        if frame.frame_height > 0 && frame.frame_width > 0 {
            let mid_row = frame.frame_height as usize / 2;
            let row_start = mid_row * frame.frame_width as usize;
            let row_end = row_start + frame.frame_width as usize;
            if row_end <= frame.pixels.len() {
                let row: Vec<i16> = frame.pixels[row_start..row_end]
                    .iter()
                    .map(|&p| if p == 0 { -999 } else { p as i16 - 65 })
                    .collect();
                // Show every 4th pixel to keep it readable
                let sampled: Vec<String> = row
                    .iter()
                    .step_by(4)
                    .map(|&v| {
                        if v == -999 {
                            "  .".to_string()
                        } else {
                            format!("{:3}", v)
                        }
                    })
                    .collect();
                println!("  Mid-row depth (every 4th px): [{}]", sampled.join(","));
            }
        }
        println!();
    }
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn inspect_buildngz_vertical() {
    let mut asset_manager = AssetManager::new(Path::new(&ra2_dir())).expect("AssetManager");
    let data = asset_manager
        .get("buildngz.sha")
        .or_else(|| asset_manager.get("buildngz.shp"))
        .expect("BUILDNGZ.SHA not found");
    let shp = ShpFile::from_bytes(&data).expect("parse");
    let frame = &shp.frames[0];
    let w = frame.frame_width as usize;

    // Sample vertical column at center of frame
    let mid_col = w / 2;
    println!("=== VERTICAL GRADIENT (center column, every 8th row) ===");
    for row in (0..frame.frame_height as usize).step_by(8) {
        let idx = row * w + mid_col;
        let raw = frame.pixels[idx];
        let remapped = if raw == 0 { -999i16 } else { raw as i16 - 65 };
        let label = if remapped == -999 {
            "  .".to_string()
        } else {
            format!("{:4}", remapped)
        };
        println!("  row {:3}: raw={:3} remapped={}", row, raw, label);
    }
}
