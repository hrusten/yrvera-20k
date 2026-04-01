//! Debug test to trace why audio.idx can't be found.

use std::path::Path;
use vera20k::assets::asset_manager::AssetManager;
use vera20k::assets::mix_hash::mix_hash;

fn ra2_dir() -> String {
    std::env::var("RA2_DIR").expect("Set RA2_DIR to your RA2/YR install directory")
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn debug_audio_idx_lookup() {
    let assets = AssetManager::new(Path::new(&ra2_dir())).expect("AssetManager");

    // Print the hash we're looking for.
    let hash_idx = mix_hash("audio.idx");
    let hash_bag = mix_hash("audio.bag");
    println!("mix_hash('audio.idx') = 0x{:08x} ({})", hash_idx, hash_idx);
    println!("mix_hash('audio.bag') = 0x{:08x} ({})", hash_bag, hash_bag);

    // Try various name variants.
    let names = [
        "audio.idx",
        "AUDIO.IDX",
        "Audio.idx",
        "audio.bag",
        "AUDIO.BAG",
        "Audio.bag",
        "audiomd.idx",
        "AUDIOMD.IDX",
        "audiomd.bag",
        "AUDIOMD.BAG",
    ];
    for name in names {
        let result = assets.get(name);
        match result {
            Some(data) => println!("FOUND: {} ({} bytes)", name, data.len()),
            None => println!("NOT FOUND: {}", name),
        }
    }

    // List all loaded archives to check if audio.mix is discovered.
    let archive_names = assets.loaded_archive_names();
    println!("\n--- All loaded archives ({}) ---", archive_names.len());
    for name in &archive_names {
        println!("  {}", name);
    }
    // Print any archive with "audio" in the name.
    let audio_archives: Vec<&String> = archive_names
        .iter()
        .filter(|n| n.to_ascii_lowercase().contains("audio"))
        .collect();
    if audio_archives.is_empty() {
        println!("WARNING: No audio-related archives found!");
    } else {
        println!("Audio archives: {:?}", audio_archives);
    }

    // Check hash of AUDIO.MIX name.
    let audio_mix_hash = mix_hash("AUDIO.MIX");
    let audiomd_mix_hash = mix_hash("AUDIOMD.MIX");
    println!("\nmix_hash('AUDIO.MIX') = {:#010X}", audio_mix_hash);
    println!("mix_hash('AUDIOMD.MIX') = {:#010X}", audiomd_mix_hash);
    // Check against unnamed archives — any of the #0x... hashes match?
    // The unnamed archive IDs from the listing are the entry hashes.
    // If AUDIO.MIX's hash matches one, it was found but not recognized as a MIX.

    // Try to get AUDIO.MIX as a raw file from any archive.
    for name in ["AUDIO.MIX", "audio.mix", "AUDIOMD.MIX", "audiomd.mix"] {
        match assets.get_with_source(name) {
            Some((data, source)) => println!(
                "RAW FOUND: {} ({} bytes, from {})",
                name,
                data.len(),
                source
            ),
            None => println!("RAW NOT FOUND: {}", name),
        }
    }

    // Check if we can find audio.mix or audiomd.mix contents.
    // Try to get with_source to see which archive things come from.
    let test_files = [
        "soundmd.ini",
        "sound.ini",
        "evamd.ini",
        "eva.ini",
        "theme.ini",
        "thememd.ini",
    ];
    for name in test_files {
        match assets.get_with_source(name) {
            Some((data, source)) => {
                println!("FOUND: {} ({} bytes) from {}", name, data.len(), source)
            }
            None => println!("NOT FOUND: {}", name),
        }
    }
}
