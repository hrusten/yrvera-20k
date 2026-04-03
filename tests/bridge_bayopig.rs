//! Inspect bridge overlays in BayoPig map.
//! Run with: cargo test --test bridge_bayopig -- --nocapture

use std::path::Path;
use vera20k::assets::asset_manager::AssetManager;
use vera20k::map::map_file::MapFile;
use vera20k::map::overlay_types::OverlayTypeRegistry;
use vera20k::rules::ini_parser::IniFile;

fn ra2_dir() -> String {
    std::env::var("RA2_DIR")
        .unwrap_or_else(|_| panic!("Set RA2_DIR env var to your RA2/YR install directory"))
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn inspect_bayopig_bridges() {
    let _ = env_logger::try_init();
    let ra2_dir_str = ra2_dir();
    let ra2_dir = Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        println!("SKIP");
        return;
    }

    let mut am = AssetManager::new(ra2_dir).expect("AM");
    for mix_name in &[
        "maps01.mix",
        "maps02.mix",
        "maps03.mix",
        "mapsmd01.mix",
        "mapsmd02.mix",
        "mapsmd03.mix",
    ] {
        let _ = am.load_nested(mix_name);
    }
    let _ = am.load_all_disk_mixes();

    let rules_data = am.get("rulesmd.ini").unwrap();
    let rules_ini = IniFile::from_str(&String::from_utf8_lossy(&rules_data));
    let reg = OverlayTypeRegistry::from_ini(&rules_ini, None);

    // Try various name patterns
    let candidates = &[
        "BayoPig.mmx",
        "bayopig.mmx",
        "BAYOPIG.MMX",
        "BayoPig.map",
        "bayopig.map",
        "BAYOPIG.MAP",
        "BayoPig.mpr",
        "bayopig.mpr",
    ];

    for name in candidates {
        let Some(data) = am.get(name) else { continue };
        println!("Found: {}", name);
        let Ok(map) = MapFile::from_bytes(&data) else {
            println!("  Failed to parse as map");
            continue;
        };
        println!("  Theater: {}", map.header.theater);
        println!("  Size: {}x{}", map.header.width, map.header.height);
        println!("  Overlays: {}", map.overlays.len());

        let mut bridge_count = 0;
        for o in &map.overlays {
            let n = reg.name(o.overlay_id).unwrap_or("?");
            let u = n.to_ascii_uppercase();
            if u.contains("BRIDGE") || u.contains("BRDG") {
                println!(
                    "  ({:3},{:3}) id={:3} name={:12} frame={}",
                    o.rx, o.ry, o.overlay_id, n, o.frame
                );
                bridge_count += 1;
            }
        }
        println!("  Total bridge overlays: {}", bridge_count);
        return;
    }
    println!("BayoPig map not found in any archive");
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn dump_bridge_shp_frames() {
    let ra2_dir_str = ra2_dir();
    let ra2_dir = std::path::Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        println!("SKIP");
        return;
    }
    let mut am = AssetManager::new(ra2_dir).expect("AM");
    let _ = am.load_all_disk_mixes();
    // Load theater mixes where bridge SHPs live
    for mix in &[
        "isotemp.mix",
        "isotem.mix",
        "isourb.mix",
        "isosnow.mix",
        "isolun.mix",
        "isodes.mix",
        "temperat.mix",
        "urban.mix",
        "snow.mix",
        "lunar.mix",
        "desert.mix",
        "tem.mix",
        "urb.mix",
        "sno.mix",
        "lun.mix",
        "des.mix",
    ] {
        let _ = am.load_nested(mix);
    }

    use vera20k::assets::shp_file::ShpFile;
    // BRIDGE1/BRIDGE2 share [BRIDGE] art entry, BRIDGEB1/B2 share [BRIDGB]
    // Theater=yes means file is e.g. bridge.tem, bridgb.tem
    for name in &[
        "bridge.tem",
        "bridge.sno",
        "bridge.urb",
        "bridge.shp",
        "bridgb.tem",
        "bridgb.sno",
        "bridgb.urb",
        "bridgb.shp",
        "lobrdg01.tem",
        "lobrdg02.tem",
        "lobrdg01.shp",
        "lobrdg02.shp",
    ] {
        match am.get(name) {
            Some(data) => match ShpFile::from_bytes(&data) {
                Ok(shp) => {
                    println!("\n=== {} ===", name);
                    println!("  full size: {}x{}", shp.width, shp.height);
                    println!("  frames: {}", shp.frames.len());
                    for (i, f) in shp.frames.iter().enumerate() {
                        println!(
                            "  frame {:2}: offset=({:3},{:3}) size={:3}x{:3}",
                            i, f.frame_x, f.frame_y, f.frame_width, f.frame_height
                        );
                    }
                }
                Err(e) => println!("{}: parse error: {}", name, e),
            },
            None => println!("{}: not found in archives", name),
        }
    }
}
