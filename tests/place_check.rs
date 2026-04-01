use std::path::Path;
use vera20k::assets::asset_manager::AssetManager;
use vera20k::assets::pal_file::Palette;
use vera20k::assets::shp_file::ShpFile;

fn ra2_dir() -> String {
    std::env::var("RA2_DIR")
        .unwrap_or_else(|_| panic!("Set RA2_DIR env var to your RA2/YR install directory"))
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn inspect_place_shp() {
    let _ = env_logger::try_init();
    let ra2_dir_str = ra2_dir();
    let ra2_dir = Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        return;
    }
    let asset_manager = AssetManager::new(ra2_dir).expect("AssetManager");

    let shp_data = asset_manager.get("place.shp").expect("place.shp");
    let shp = ShpFile::from_bytes(&shp_data).expect("parse place.shp");

    println!(
        "\nplace.shp: {} frames, header {}x{}",
        shp.frames.len(),
        shp.width,
        shp.height
    );
    for (i, frame) in shp.frames.iter().enumerate() {
        println!(
            "  Frame {:>2}: {}x{} at=({},{})",
            i, frame.frame_width, frame.frame_height, frame.frame_x, frame.frame_y
        );
    }

    // Try different palettes
    for pal_name in &["unittem.pal", "temperat.pal", "isotem.pal", "cache.pal"] {
        if let Some(pal_data) = asset_manager.get(pal_name) {
            if let Ok(pal) = Palette::from_bytes(&pal_data) {
                if let Ok(rgba) = shp.frame_to_rgba(0, &pal) {
                    let non_transparent: usize = rgba.chunks(4).filter(|p| p[3] > 0).count();
                    let bright: usize = rgba
                        .chunks(4)
                        .filter(|p| p[3] > 0 && (p[0] > 100 || p[1] > 100 || p[2] > 100))
                        .count();
                    println!(
                        "  {} frame0: {} opaque px, {} bright px",
                        pal_name, non_transparent, bright
                    );
                }
            }
        }
    }

    // Check raw pixel indices in frame 0
    let pal_data = asset_manager.get("unittem.pal").expect("unittem.pal");
    let pal = Palette::from_bytes(&pal_data).expect("parse pal");
    let frame = &shp.frames[0];
    let mut index_histogram: std::collections::HashMap<u8, u32> = std::collections::HashMap::new();
    for &byte in &frame.pixels {
        *index_histogram.entry(byte).or_insert(0) += 1;
    }
    let mut indices: Vec<(u8, u32)> = index_histogram.into_iter().collect();
    indices.sort_by_key(|&(_, count)| std::cmp::Reverse(count));
    println!("\n  Frame 0 palette index histogram (top 10):");
    for (idx, count) in indices.iter().take(10) {
        let c = pal.colors[*idx as usize];
        println!(
            "    index={:>3} count={:>4} -> RGB({},{},{})",
            idx, count, c.r, c.g, c.b
        );
    }

    // Export frame 0 and 1 as PNG for visual inspection
    for frame_idx in 0..shp.frames.len().min(4) {
        if let Ok(rgba) = shp.frame_to_rgba(frame_idx, &pal) {
            let f = &shp.frames[frame_idx];
            let w = f.frame_width as u32;
            let h = f.frame_height as u32;
            if w > 0 && h > 0 {
                let path = format!("exported_shp_pngs/place_frame{:02}.png", frame_idx);
                let _ = image::save_buffer(&path, &rgba, w, h, image::ColorType::Rgba8);
                println!("  Exported {} ({}x{})", path, w, h);
            }
        }
    }
}
