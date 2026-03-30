//! Integration test: verify audio.idx/bag loading from retail RA2 assets.
//!
//! Tests that the full pipeline works: AssetManager → idx/bag → decode → playable samples.

use vera20k::assets::asset_manager::AssetManager;
use vera20k::assets::audio_bag::{decode_bag_audio, AudioIndex};
use vera20k::assets::mix_archive::MixArchive;
use std::path::Path;

fn ra2_dir() -> String {
    std::env::var("RA2_DIR").expect("Set RA2_DIR to your RA2/YR install directory")
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn test_load_audio_idx_from_mix() {
    let assets = AssetManager::new(Path::new(&ra2_dir())).expect("AssetManager");

    // Try YR first, then base RA2.
    let (idx_name, bag_name) = if assets.get("audiomd.idx").is_some() {
        ("audiomd.idx", "audiomd.bag")
    } else if assets.get("audio.idx").is_some() {
        ("audio.idx", "audio.bag")
    } else {
        panic!("Neither audiomd.idx nor audio.idx found in any MIX archive");
    };

    let idx_data = assets.get(idx_name).expect("idx data");
    let bag_data = assets.get(bag_name).expect("bag data");

    println!("Loaded {}: {} bytes", idx_name, idx_data.len());
    println!("Loaded {}: {} bytes", bag_name, bag_data.len());

    // Debug: print the first 20 bytes of the idx to check the header.
    println!(
        "idx header bytes: {:02X?}",
        &idx_data[..20.min(idx_data.len())]
    );
    let magic = u32::from_le_bytes([idx_data[0], idx_data[1], idx_data[2], idx_data[3]]);
    let version = u32::from_le_bytes([idx_data[4], idx_data[5], idx_data[6], idx_data[7]]);
    let count = u32::from_le_bytes([idx_data[8], idx_data[9], idx_data[10], idx_data[11]]);
    println!(
        "Header: magic={}, version={}, count={}",
        magic, version, count
    );
    println!(
        "Expected size: {} + {} * 32 = {}",
        12,
        count,
        12 + count * 32
    );
    println!("Actual size: {}", idx_data.len());

    let index = AudioIndex::from_idx_bag(&idx_data, bag_data)
        .expect("AudioIndex should parse successfully");

    println!("AudioIndex: {} entries", index.len());
    assert!(
        index.len() > 100,
        "Expected at least 100 entries, got {}",
        index.len()
    );
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn test_lookup_known_gi_voice() {
    let assets = AssetManager::new(Path::new(&ra2_dir())).expect("AssetManager");

    let idx_data = assets
        .get("audiomd.idx")
        .or_else(|| assets.get("audio.idx"))
        .expect("audio idx not found");
    let bag_data = assets
        .get("audiomd.bag")
        .or_else(|| assets.get("audio.bag"))
        .expect("audio bag not found");

    let index = AudioIndex::from_idx_bag(&idx_data, bag_data).expect("AudioIndex should parse");

    // Also try loading audiomd index.
    if let Some(md_idx_data) = assets.get("audiomd.idx") {
        if let Some(md_bag_data) = assets.get("audiomd.bag") {
            println!(
                "audiomd.idx: {} bytes, audiomd.bag: {} bytes",
                md_idx_data.len(),
                md_bag_data.len()
            );
            if let Some(md_index) = AudioIndex::from_idx_bag(&md_idx_data, md_bag_data) {
                println!("audiomd AudioIndex: {} entries", md_index.len());
                if md_index.get("ceva048").is_some() {
                    println!("ceva048 FOUND in audiomd!");
                }
            }
        }
    } else {
        println!("audiomd.idx NOT FOUND — checking what's inside audiomd.mix...");
        // Try alternate names
        for n in ["audio.idx", "audiomd.idx", "AUDIO.IDX", "AUDIOMD.IDX"] {
            if assets.get(n).is_some() {
                println!("  {} found", n);
            }
        }
    }

    // "igisea" = GI select voice A (from soundmd.ini [GISelect] Sounds= $igisea ...)
    let result = index.get("igisea");
    assert!(result.is_some(), "Expected to find 'igisea' in audio index");

    let (entry, data) = result.unwrap();
    println!(
        "igisea: offset={}, size={}, rate={}, flags=0x{:02x}",
        entry.offset, entry.size, entry.sample_rate, entry.flags
    );
    assert!(entry.size > 0, "Sound data should not be empty");
    assert!(entry.sample_rate > 0, "Sample rate should not be zero");
    assert!(!data.is_empty(), "Data slice should not be empty");

    // Decode it.
    let audio = decode_bag_audio(entry, data).expect("Should decode igisea audio data");
    println!(
        "Decoded: {} samples, {} Hz, {} ch",
        audio.samples_i16.len(),
        audio.sample_rate,
        audio.channels
    );
    assert!(
        audio.samples_i16.len() > 100,
        "Expected more than 100 samples"
    );
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn test_lookup_eva_sounds() {
    let assets = AssetManager::new(Path::new(&ra2_dir())).expect("AssetManager");

    // EVA sounds are in AUDIOMD.MIX (YR expansion). Load it explicitly
    // because both AUDIO.MIX and AUDIOMD.MIX have internal entries named "audio.idx".
    let mix_data = assets
        .get("AUDIOMD.MIX")
        .expect("AUDIOMD.MIX should be in langmd.mix");
    let mix = MixArchive::from_bytes(mix_data).expect("AUDIOMD.MIX should parse as MIX");
    let idx_data = mix
        .get_by_name("audio.idx")
        .expect("audio.idx in AUDIOMD.MIX")
        .to_vec();
    let bag_data = mix
        .get_by_name("audio.bag")
        .expect("audio.bag in AUDIOMD.MIX")
        .to_vec();

    let index = AudioIndex::from_idx_bag(&idx_data, bag_data).expect("AudioIndex should parse");
    println!("AUDIOMD AudioIndex: {} entries", index.len());

    // Debug: print entries starting with EVA prefixes.
    for prefix in ["CEVA", "CSOF", "CYUR"] {
        let matches = index.names_with_prefix(prefix);
        println!(
            "Entries with prefix '{}': {} (first 5: {:?})",
            prefix,
            matches.len(),
            &matches[..5.min(matches.len())]
        );
    }

    // Check all 3 factions' "Construction complete" EVA.
    for name in ["ceva048", "csof048", "cyur048"] {
        let result = index.get(name);
        assert!(
            result.is_some(),
            "Expected to find '{}' in audiomd index",
            name
        );
        let (entry, data) = result.unwrap();
        let audio =
            decode_bag_audio(entry, data).expect(&format!("Should decode {} audio data", name));
        println!(
            "{}: {} samples, {} Hz",
            name,
            audio.samples_i16.len(),
            audio.sample_rate
        );
        assert!(audio.samples_i16.len() > 100);
    }

    // Check "Unit ready" EVA for all factions.
    for name in ["ceva062", "csof062", "cyur062"] {
        assert!(
            index.get(name).is_some(),
            "Expected to find '{}' in audiomd index",
            name
        );
    }
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn test_sound_registry_with_real_soundmd() {
    use vera20k::rules::ini_parser::IniFile;
    use vera20k::rules::sound_ini::SoundRegistry;

    let assets = AssetManager::new(Path::new(&ra2_dir())).expect("AssetManager");

    let soundmd_bytes = assets.get("soundmd.ini").expect("soundmd.ini should exist");
    let soundmd_text = String::from_utf8(soundmd_bytes).expect("valid utf8");
    let ini = IniFile::from_str(&soundmd_text);
    let registry = SoundRegistry::from_ini(&ini);

    println!("SoundRegistry: {} entries", registry.len());
    assert!(
        registry.len() > 100,
        "Expected 100+ sound entries, got {}",
        registry.len()
    );

    // GISelect should exist and have bare names ($ stripped).
    let gi_select = registry
        .get("GISelect")
        .expect("GISelect should be in registry");
    println!("GISelect sounds: {:?}", gi_select.sounds);
    assert!(!gi_select.sounds.is_empty(), "GISelect should have sounds");
    // Verify $ was stripped.
    for s in &gi_select.sounds {
        assert!(!s.starts_with('$'), "Sound name '{}' still has $ prefix", s);
    }
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn test_eva_registry_with_real_evamd() {
    use vera20k::rules::ini_parser::IniFile;
    use vera20k::rules::sound_ini::EvaRegistry;

    let assets = AssetManager::new(Path::new(&ra2_dir())).expect("AssetManager");

    let evamd_bytes = assets.get("evamd.ini").expect("evamd.ini should exist");
    let evamd_text = String::from_utf8_lossy(&evamd_bytes).into_owned();
    let ini = IniFile::from_str(&evamd_text);
    let registry = EvaRegistry::from_ini(&ini);

    println!("EvaRegistry: {} entries", registry.len());
    assert!(
        registry.len() > 30,
        "Expected 30+ EVA entries, got {}",
        registry.len()
    );

    // Test faction-specific lookup.
    let allied = registry.get("EVA_ConstructionComplete", "Allied");
    let soviet = registry.get("EVA_ConstructionComplete", "Russian");
    let yuri = registry.get("EVA_ConstructionComplete", "Yuri");
    println!(
        "EVA_ConstructionComplete: Allied={:?}, Russian={:?}, Yuri={:?}",
        allied, soviet, yuri
    );
    assert_eq!(allied, Some("ceva048"));
    assert_eq!(soviet, Some("csof048"));
    assert_eq!(yuri, Some("cyur048"));

    let unit_allied = registry.get("EVA_UnitReady", "Allied");
    let unit_soviet = registry.get("EVA_UnitReady", "Russian");
    assert_eq!(unit_allied, Some("ceva062"));
    assert_eq!(unit_soviet, Some("csof062"));
}
