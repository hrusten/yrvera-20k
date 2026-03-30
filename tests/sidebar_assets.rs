use std::path::{Path, PathBuf};

use image::RgbaImage;
use vera20k::assets::asset_manager::AssetManager;
use vera20k::assets::pal_file::Palette;
use vera20k::assets::shp_file::ShpFile;

fn ra2_dir() -> String {
    std::env::var("RA2_DIR")
        .unwrap_or_else(|_| panic!("Set RA2_DIR env var to your RA2/YR install directory"))
}

fn save_shp_frame_png(
    asset_manager: &AssetManager,
    shp_name: &str,
    palette_name: &str,
    output_name: &str,
) {
    let Some(shp_data) = asset_manager.get(shp_name) else {
        eprintln!("  missing SHP {}", shp_name);
        return;
    };
    let Some(pal_data) = asset_manager.get(palette_name) else {
        eprintln!("  missing palette {}", palette_name);
        return;
    };

    let shp = ShpFile::from_bytes(&shp_data).expect("parse sidebar shp");
    let pal = Palette::from_bytes(&pal_data).expect("parse sidebar palette");
    let rgba = shp.frame_to_rgba(0, &pal).expect("decode sidebar frame");
    let frame = &shp.frames[0];
    let width = frame.frame_width as u32;
    let height = frame.frame_height as u32;
    let out_path = Path::new(output_name);
    let image = RgbaImage::from_raw(width, height, rgba).expect("rgba image");
    image.save(out_path).expect("save png");
    eprintln!(
        "  exported {} using {} -> {} ({}x{})",
        shp_name,
        palette_name,
        out_path.display(),
        width,
        height
    );
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn debug_probe_sidebar_assets() {
    let ra2_dir = PathBuf::from(ra2_dir());
    if !ra2_dir.exists() {
        eprintln!("SKIP: RA2 dir not found at {}", ra2_dir.display());
        return;
    }

    let asset_manager = AssetManager::new(&ra2_dir).expect("asset manager");
    let candidates = [
        "sidec01.mix",
        "sidec02.mix",
        "sidec02md.mix",
        "bkgdlg.shp",
        "bkgdmd.shp",
        "bkgdsm.shp",
        "bkgdlgy.shp",
        "bkgdmdy.shp",
        "bkgdsmy.shp",
        "radar.shp",
        "radary.shp",
        "side1.shp",
        "side2.shp",
        "side3.shp",
        "side4.shp",
        "tab00.shp",
        "tab01.shp",
        "tab02.shp",
        "tab03.shp",
        "power.shp",
        "pips.shp",
        "clock.shp",
        "uibkgd.pal",
        "uibkgdy.pal",
        "sidebar.pal",
        "radaryuri.pal",
    ];

    eprintln!("Sidebar asset probe:");
    for name in candidates {
        match asset_manager.get_with_source(name) {
            Some((data, source)) => {
                eprintln!(
                    "  FOUND {:<16} in {:<24} ({} bytes)",
                    name,
                    source,
                    data.len()
                );
            }
            None => eprintln!("  missing {}", name),
        }
    }

    save_shp_frame_png(
        &asset_manager,
        "bkgdlg.shp",
        "uibkgd.pal",
        "debug_sidebar_bkgdlg.png",
    );
    save_shp_frame_png(
        &asset_manager,
        "bkgdmd.shp",
        "uibkgd.pal",
        "debug_sidebar_bkgdmd.png",
    );
    save_shp_frame_png(
        &asset_manager,
        "bkgdsm.shp",
        "uibkgd.pal",
        "debug_sidebar_bkgdsm.png",
    );
    save_shp_frame_png(
        &asset_manager,
        "bkgdlgy.shp",
        "uibkgdy.pal",
        "debug_sidebar_bkgdlgy.png",
    );
    save_shp_frame_png(
        &asset_manager,
        "radar.shp",
        "sidebar.pal",
        "debug_sidebar_radar.png",
    );
    save_shp_frame_png(
        &asset_manager,
        "radary.shp",
        "radaryuri.pal",
        "debug_sidebar_radary.png",
    );
    save_shp_frame_png(
        &asset_manager,
        "side1.shp",
        "sidebar.pal",
        "debug_sidebar_side1.png",
    );
    save_shp_frame_png(
        &asset_manager,
        "side2.shp",
        "sidebar.pal",
        "debug_sidebar_side2.png",
    );
    save_shp_frame_png(
        &asset_manager,
        "side3.shp",
        "sidebar.pal",
        "debug_sidebar_side3.png",
    );
    save_shp_frame_png(
        &asset_manager,
        "tab00.shp",
        "sidebar.pal",
        "debug_sidebar_tab00.png",
    );
    save_shp_frame_png(
        &asset_manager,
        "power.shp",
        "sidebar.pal",
        "debug_sidebar_power.png",
    );
}
