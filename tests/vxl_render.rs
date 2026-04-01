//! Integration test: loads a real VXL model from RA2 MIX archives and renders to PNG.
//!
//! Run with: cargo test --test vxl_render -- --nocapture

use std::path::Path;

use vera20k::assets::asset_manager::AssetManager;
use vera20k::assets::hva_file::HvaFile;
use vera20k::assets::pal_file::Palette;
use vera20k::assets::vpl_file::VplFile;
use vera20k::assets::vxl_file::VxlFile;
use vera20k::render::vxl_raster::{VxlRenderParams, VxlSprite, render_vxl};

fn ra2_dir() -> String {
    std::env::var("RA2_DIR")
        .unwrap_or_else(|_| panic!("Set RA2_DIR env var to your RA2/YR install directory"))
}

/// Vehicle VXL names to try (in priority order).
const VXL_CANDIDATES: &[&str] = &["htnk.vxl", "mtnk.vxl", "harv.vxl", "sref.vxl"];

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn render_real_vxl_to_png() {
    let _ = env_logger::try_init();

    let ra2_dir_str = ra2_dir();
    let ra2_dir: &Path = Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        println!("SKIP: RA2 dir not found at {}", ra2_dir_str);
        return;
    }

    let asset_manager: AssetManager = AssetManager::new(ra2_dir).expect("AssetManager");

    // Find a VXL file in the MIX chain.
    let mut vxl_name: Option<&str> = None;
    let mut vxl_data: Vec<u8> = Vec::new();
    for name in VXL_CANDIDATES {
        if let Some(data) = asset_manager.get(name) {
            vxl_name = Some(name);
            vxl_data = data;
            break;
        }
    }
    let vxl_name: &str = match vxl_name {
        Some(n) => n,
        None => {
            println!(
                "SKIP: No VXL found in MIX chain (tried {:?})",
                VXL_CANDIDATES
            );
            return;
        }
    };

    let vxl: VxlFile = VxlFile::from_bytes(&vxl_data).expect("VXL parse");
    println!(
        "\nVXL '{}': {} limbs, {} total voxels",
        vxl_name,
        vxl.limb_count,
        vxl.limbs.iter().map(|l| l.voxels.len()).sum::<usize>()
    );

    // Try to load matching HVA.
    let hva_name: String = vxl_name.replace(".vxl", ".hva");
    let hva: Option<HvaFile> = asset_manager
        .get(&hva_name)
        .and_then(|data| HvaFile::from_bytes(&data).ok());
    if let Some(ref h) = hva {
        println!(
            "HVA '{}': {} frames, {} sections",
            hva_name, h.frame_count, h.section_count
        );
    } else {
        println!("HVA '{}': not found (using default transforms)", hva_name);
    }

    // Load unit palette (unittem.pal is the RA2 unit palette for voxels).
    let pal_names: &[&str] = &["unittem.pal", "unit.pal", "temperat.pal"];
    let palette: Palette = pal_names
        .iter()
        .find_map(|name| {
            asset_manager
                .get(name)
                .and_then(|data| Palette::from_bytes(&data).ok())
        })
        .expect("Should find at least one palette");

    // Load VPL for Blinn-Phong lighting (voxels.vpl contains palette shading tables).
    let vpl: Option<VplFile> = asset_manager
        .get("voxels.vpl")
        .and_then(|data| VplFile::from_bytes(&data).ok());
    if let Some(ref v) = vpl {
        println!(
            "VPL: loaded ({} sections, firstRemap={}, lastRemap={})",
            v.num_sections, v.first_remap, v.last_remap
        );
    } else {
        println!("VPL: NOT FOUND");
    }

    // Render at 8 facings.
    let stem: &str = vxl_name.trim_end_matches(".vxl");
    let facings: &[u8] = &[0, 32, 64, 96, 128, 160, 192, 224];

    for &facing in facings {
        let params: VxlRenderParams = VxlRenderParams {
            facing,
            ..Default::default()
        };

        let sprite: VxlSprite = render_vxl(&vxl, hva.as_ref(), &palette, &params, vpl.as_ref());

        assert!(sprite.width > 0, "Sprite width should be > 0");
        assert!(sprite.height > 0, "Sprite height should be > 0");

        // Check that at least some pixels are opaque.
        let opaque: usize = sprite.rgba.chunks(4).filter(|p| p[3] > 0).count();
        assert!(
            opaque > 0,
            "Facing {} produced an all-transparent sprite",
            facing
        );

        // Save as PNG for visual inspection.
        let filename: String = format!("debug_vxl_{}_{}.png", stem, facing);
        save_rgba_png(&filename, &sprite.rgba, sprite.width, sprite.height);
        println!(
            "  facing={:3}: {}x{} px, {} opaque pixels, offset=({:.1},{:.1}) → {}",
            facing, sprite.width, sprite.height, opaque, sprite.offset_x, sprite.offset_y, filename
        );
    }

    println!("\n=== VXL render test passed ===");
}

/// Save RGBA data as a PNG file in the project root.
fn save_rgba_png(filename: &str, rgba: &[u8], width: u32, height: u32) {
    let img: image::RgbaImage = image::RgbaImage::from_raw(width, height, rgba.to_vec())
        .expect("RGBA dimensions should match");
    let path: String = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), filename);
    img.save(&path).expect("PNG save should succeed");
}
