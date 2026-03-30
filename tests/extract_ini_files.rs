//! One-shot test to extract original INI files from the RA2 MIX archives
//! into the repo's ini/ folder for easy reference.
//!
//! Run with: cargo test --test extract_ini_files -- --nocapture

use std::fs;
use std::path::Path;

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn extract_original_ini_files() {
    let game_dir = match std::env::var("RA2_DIR") {
        Ok(val) => std::path::PathBuf::from(val),
        Err(_) => match vera20k::util::config::GameConfig::load() {
            Ok(cfg) => cfg.paths.ra2_dir,
            Err(_) => {
                eprintln!("SKIPPED: RA2_DIR not set and config.toml not found");
                return;
            }
        },
    };
    if !game_dir.exists() {
        eprintln!("RA2 game directory not found, skipping extraction");
        return;
    }

    let am = vera20k::assets::asset_manager::AssetManager::new(&game_dir)
        .expect("Failed to load asset manager");

    let ini_dir = Path::new("ini");
    fs::create_dir_all(ini_dir).expect("Failed to create ini/ directory");

    let files: &[&str] = &[
        "rules.ini",
        "rulesmd.ini",
        "art.ini",
        "artmd.ini",
        "ai.ini",
        "aimd.ini",
        "sound.ini",
        "soundmd.ini",
        "eva.ini",
        "evamd.ini",
        "battle.ini",
        "battlemd.ini",
        "temperat.ini",
        "temperatmd.ini",
        "snow.ini",
        "snowmd.ini",
        "urban.ini",
        "urbanmd.ini",
        "ui.ini",
        "uimd.ini",
    ];

    for name in files {
        match am.get(name) {
            Some(data) => {
                let path = ini_dir.join(name);
                fs::write(&path, &data).expect("Failed to write file");
                println!("Extracted {:20} ({:>8} bytes)", name, data.len());
            }
            None => {
                println!("NOT FOUND: {}", name);
            }
        }
    }
}
