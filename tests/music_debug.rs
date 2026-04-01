//! Debug test to diagnose why music doesn't play.

use std::path::Path;
use vera20k::assets::asset_manager::AssetManager;
use vera20k::rules::ini_parser::IniFile;

fn ra2_dir() -> String {
    std::env::var("RA2_DIR").expect("Set RA2_DIR to your RA2/YR install directory")
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn debug_music_pipeline() {
    let assets = AssetManager::new(Path::new(&ra2_dir())).expect("AssetManager");

    // 1. Check if theme INI files load.
    for name in ["thememd.ini", "theme.ini"] {
        match assets.get(name) {
            Some(bytes) => {
                println!("{}: {} bytes", name, bytes.len());
                // Try strict UTF-8.
                match std::str::from_utf8(&bytes) {
                    Ok(_) => println!("  UTF-8: OK"),
                    Err(e) => println!(
                        "  UTF-8: FAILED at byte {} — lossy fallback needed",
                        e.valid_up_to()
                    ),
                }
                // Parse with lossy.
                let text = String::from_utf8_lossy(&bytes);
                let ini = IniFile::from_str(&text);
                let sections: Vec<String> = ini
                    .section_names()
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect();
                println!(
                    "  Sections: {} (first 10: {:?})",
                    sections.len(),
                    &sections[..10.min(sections.len())]
                );

                // Check for Sound= keys (theme aliases).
                let mut alias_count = 0;
                for section_name in ini.section_names() {
                    if let Some(section) = ini.section(section_name) {
                        if let Some(sound) = section.get("Sound") {
                            if !sound.is_empty() {
                                if alias_count < 5 {
                                    println!("  [{}] Sound={}", section_name, sound);
                                }
                                alias_count += 1;
                            }
                        }
                    }
                }
                println!("  Total Sound= aliases: {}", alias_count);
            }
            None => println!("{}: NOT FOUND", name),
        }
    }

    // 2. Try loading actual track files.
    let test_tracks = [
        "Grinder",
        "grinder",
        "GRINDER",
        "grinder.wav",
        "grinder.aud",
        "BIGF226M",
        "BIGF226M.wav",
        "BIGF226M.aud",
    ];
    println!("\n--- Track file lookup ---");
    for name in test_tracks {
        match assets.get(name) {
            Some(data) => println!("FOUND: {} ({} bytes)", name, data.len()),
            None => println!("NOT FOUND: {}", name),
        }
    }

    // 3. If theme.ini loaded, try resolving the first few Sound= aliases.
    if let Some(bytes) = assets.get("thememd.ini") {
        let text = String::from_utf8_lossy(&bytes);
        let ini = IniFile::from_str(&text);
        println!("\n--- Resolving theme aliases ---");
        for section_name in ini.section_names().into_iter().take(10) {
            if let Some(section) = ini.section(section_name) {
                if let Some(sound) = section.get("Sound") {
                    if !sound.is_empty() {
                        // Try loading the resolved filename.
                        for ext in [".wav", ".aud"] {
                            let filename = format!("{}{}", sound, ext);
                            if let Some(data) = assets.get(&filename) {
                                println!(
                                    "  [{}] Sound={} → {} ({} bytes) ✓",
                                    section_name,
                                    sound,
                                    filename,
                                    data.len()
                                );
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
}
