//! Search for RA2 cameo ICON SHP files across all loaded MIX archives.
//!
//! Cameo icons are the small building/unit portraits shown in the sidebar
//! build palette. In RA2, they follow the naming convention `<TYPE>ICON.SHP`
//! (e.g., GAPOWRICON.SHP for the Power Plant cameo). This test probes the
//! asset manager and specific MIX archives to find where they live.

use std::path::PathBuf;

use vera20k::assets::asset_manager::AssetManager;
use vera20k::assets::mix_archive::MixArchive;
use vera20k::assets::mix_hash::mix_hash;

fn ra2_dir() -> String {
    std::env::var("RA2_DIR")
        .unwrap_or_else(|_| panic!("Set RA2_DIR env var to your RA2/YR install directory"))
}

/// Well-known cameo ICON filenames to search for.
const ICON_CANDIDATES: &[&str] = &[
    "GAPOWRICON.SHP",
    "E1ICON.SHP",
    "CONSCICON.SHP",
    "GAREFNICON.SHP",
    // Additional common ones to broaden the search
    "NAHANDICON.SHP",
    "GAABORICON.SHP",
    "GABORDRICON.SHP",
    "YABORDRICON.SHP",
    "NAHEAVICON.SHP",
    "NAWEAPICON.SHP",
];

/// MIX archives that might contain cameo icons.
const CAMEO_MIX_CANDIDATES: &[&str] = &[
    "cameo.mix",
    "cameomd.mix",
    "cameo01.mix",
    "cameoe.mix",
    "cameos.mix",
    // Sidebar chrome archives (less likely, but worth checking)
    "sidec01.mix",
    "sidec02.mix",
];

/// Extended list of known cameo filenames to match against MIX entry hashes.
/// If any of these hash-match an entry inside a cameo MIX, we can identify it.
const KNOWN_CAMEO_NAMES: &[&str] = &[
    // Allied buildings
    "GAPOWRICON.SHP",
    "GAREFNICON.SHP",
    "GAABORICON.SHP",
    "GABORDRICON.SHP",
    "GAABORICON.SHP",
    "GAPILEICON.SHP",
    "GADEPTICON.SHP",
    "GAABORICON.SHP",
    "GAWALLICON.SHP",
    "GAAIRLICON.SHP",
    "GASPYRICON.SHP",
    "GAORMNICON.SHP",
    "GASANDICON.SHP",
    "GAROBOICON.SHP",
    "NACONSICON.SHP",
    "NABORDRICON.SHP",
    // Soviet buildings
    "NAPOWR.SHP",
    "NAPOWRICON.SHP",
    "NAREFNICON.SHP",
    "NAHANDICON.SHP",
    "NAHEAVICON.SHP",
    "NAWEAPICON.SHP",
    "NARADRICON.SHP",
    "NAFLAKICON.SHP",
    "NATESLICON.SHP",
    "NAWALLICON.SHP",
    // Units
    "E1ICON.SHP",
    "E2ICON.SHP",
    "E3ICON.SHP",
    "E4ICON.SHP",
    "CONSCICON.SHP",
    "IVANICON.SHP",
    "FLAKTICON.SHP",
    "TANYAICON.SHP",
    "SHOCKTICON.SHP",
    "DESOICON.SHP",
    "SPYICON.SHP",
    "ENGINEICON.SHP",
    "DTRKICON.SHP",
    "MTNKICON.SHP",
    "FVICON.SHP",
    "HTRKICON.SHP",
    "APOCICON.SHP",
    "HTNKICON.SHP",
    "RHINOICON.SHP",
    "TERRICON.SHP",
    "V3ICON.SHP",
    "TTNKICON.SHP",
    "HARVRICON.SHP",
    "MCVICON.SHP",
    "HORNETICON.SHP",
    "AEGISICON.SHP",
    "DREDICON.SHP",
    "CARRIERICON.SHP",
    "BSUBICON.SHP",
    "INTRLICON.SHP",
    // YR additions
    "YABORDRICON.SHP",
    "YAPPICON.SHP",
    "YAREFNICON.SHP",
    "YACOMDICON.SHP",
    // Superweapons
    "NABORDRICON.SHP",
    "GAABORICON.SHP",
    // PCX cameo variants (YR uses PCX for some cameos)
    "GAPOWRICON.PCX",
    "E1ICON.PCX",
    "CONSCICON.PCX",
    "GAREFNICON.PCX",
];

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn find_cameo_icon_files() {
    let ra2_dir: PathBuf = PathBuf::from(ra2_dir());
    if !ra2_dir.exists() {
        eprintln!("SKIP: RA2 dir not found at {}", ra2_dir.display());
        return;
    }

    let asset_manager: AssetManager =
        AssetManager::new(&ra2_dir).expect("Failed to create AssetManager");

    // ---------------------------------------------------------------
    // Step 1: Direct lookup of ICON SHP files via asset_manager.get()
    // ---------------------------------------------------------------
    eprintln!("\n{}", "=".repeat(60));
    eprintln!("  Step 1: Direct asset_manager.get() for ICON SHP files");
    eprintln!("{}", "=".repeat(60));

    for &name in ICON_CANDIDATES {
        match asset_manager.get_with_source(name) {
            Some((data, source)) => {
                eprintln!(
                    "  FOUND  {:<22} in {:<30} ({} bytes)",
                    name,
                    source,
                    data.len()
                );
            }
            None => {
                eprintln!("  NOT FOUND  {}", name);
            }
        }
    }

    // Also try lowercase variants
    eprintln!("\n  (Trying lowercase variants...)");
    for &name in &["gapowricon.shp", "e1icon.shp", "conscicon.shp"] {
        match asset_manager.get_with_source(name) {
            Some((data, source)) => {
                eprintln!(
                    "  FOUND  {:<22} in {:<30} ({} bytes)",
                    name,
                    source,
                    data.len()
                );
            }
            None => {
                eprintln!("  NOT FOUND  {}", name);
            }
        }
    }

    // ---------------------------------------------------------------
    // Step 2: Find and open cameo MIX archives
    // ---------------------------------------------------------------
    eprintln!("\n{}", "=".repeat(60));
    eprintln!("  Step 2: Search for cameo MIX archives");
    eprintln!("{}", "=".repeat(60));

    // Build reverse lookup: hash -> known cameo filename
    let mut id_to_name: std::collections::HashMap<i32, String> = std::collections::HashMap::new();
    for &name in KNOWN_CAMEO_NAMES {
        let id: i32 = mix_hash(name);
        id_to_name.insert(id, name.to_string());
    }

    for &mix_name in CAMEO_MIX_CANDIDATES {
        eprintln!("\n  --- {} ---", mix_name);
        let mix_data: Vec<u8> = match asset_manager.get(mix_name) {
            Some(data) => {
                eprintln!("  FOUND {} ({} bytes)", mix_name, data.len());
                data
            }
            None => {
                eprintln!("  NOT FOUND: {}", mix_name);
                continue;
            }
        };

        // Parse as nested MIX archive
        let archive: MixArchive = match MixArchive::from_bytes(mix_data) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("  PARSE ERROR for {}: {}", mix_name, e);
                continue;
            }
        };

        let entries = archive.entries();
        eprintln!("  Entry count: {}", entries.len());

        // List all entries, matching against known cameo names
        let mut matched_count: usize = 0;
        for (i, entry) in entries.iter().enumerate() {
            let label: String = match id_to_name.get(&entry.id) {
                Some(name) => {
                    matched_count += 1;
                    format!("  => {}", name)
                }
                None => String::new(),
            };
            // Print first 20 entries always, then only matched ones
            if i < 20 || !label.is_empty() {
                eprintln!(
                    "  [{:4}] id={:#010X}  offset={:8}  size={:8}{}",
                    i, entry.id as u32, entry.offset, entry.size, label
                );
            } else if i == 20 {
                eprintln!("  ... (showing only matched entries from here) ...");
            }
        }
        eprintln!(
            "  Summary: {} total, {} matched known cameo names",
            entries.len(),
            matched_count
        );

        // Try to find GAPOWRICON.SHP inside this MIX
        let gapowricon_id: i32 = mix_hash("GAPOWRICON.SHP");
        if let Some(data) = archive.get_by_id(gapowricon_id) {
            eprintln!(
                "  ** GAPOWRICON.SHP FOUND inside {} ({} bytes) **",
                mix_name,
                data.len()
            );
        } else {
            eprintln!("  GAPOWRICON.SHP NOT inside {}", mix_name);
        }
    }

    // ---------------------------------------------------------------
    // Step 3: Check sidec01.mix specifically
    // ---------------------------------------------------------------
    eprintln!("\n{}", "=".repeat(60));
    eprintln!("  Step 3: Check sidec01.mix for cameo ICONs");
    eprintln!("{}", "=".repeat(60));

    if let Some(mix_data) = asset_manager.get("sidec01.mix") {
        eprintln!("  sidec01.mix loaded ({} bytes)", mix_data.len());
        match MixArchive::from_bytes(mix_data) {
            Ok(archive) => {
                // Try each icon candidate
                for &name in ICON_CANDIDATES {
                    match archive.get_by_name(name) {
                        Some(data) => {
                            eprintln!(
                                "  FOUND  {:<22} in sidec01.mix ({} bytes)",
                                name,
                                data.len()
                            );
                        }
                        None => {
                            eprintln!("  NOT FOUND  {} in sidec01.mix", name);
                        }
                    }
                }

                // Also list all sidec01.mix entries matching cameo hashes
                let mut matched: usize = 0;
                for entry in archive.entries() {
                    if let Some(name) = id_to_name.get(&entry.id) {
                        eprintln!(
                            "  sidec01 match: id={:#010X} => {} ({} bytes)",
                            entry.id as u32, name, entry.size
                        );
                        matched += 1;
                    }
                }
                eprintln!(
                    "  sidec01.mix: {} entries, {} matched cameo names",
                    archive.entry_count(),
                    matched
                );
            }
            Err(e) => {
                eprintln!("  PARSE ERROR for sidec01.mix: {}", e);
            }
        }
    } else {
        eprintln!("  sidec01.mix NOT FOUND in asset manager");
    }

    // ---------------------------------------------------------------
    // Step 4: Broader search — try common container MIX names
    // ---------------------------------------------------------------
    eprintln!("\n{}", "=".repeat(60));
    eprintln!("  Step 4: Broader MIX search for cameo containers");
    eprintln!("{}", "=".repeat(60));

    let broader_mixes: &[&str] = &[
        "local.mix",
        "localmd.mix",
        "conquer.mix",
        "conqmd.mix",
        "cache.mix",
        "cachemd.mix",
        "cameo.mix",
        "cameomd.mix",
    ];

    for &mix_name in broader_mixes {
        let mix_data: Vec<u8> = match asset_manager.get(mix_name) {
            Some(data) => data,
            None => {
                eprintln!("  {} — NOT FOUND", mix_name);
                continue;
            }
        };

        let archive: MixArchive = match MixArchive::from_bytes(mix_data) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("  {} — PARSE ERROR: {}", mix_name, e);
                continue;
            }
        };

        // Check for GAPOWRICON.SHP as representative cameo
        let has_gapowr: bool = archive.get_by_name("GAPOWRICON.SHP").is_some();
        let has_e1: bool = archive.get_by_name("E1ICON.SHP").is_some();
        let has_consc: bool = archive.get_by_name("CONSCICON.SHP").is_some();

        // Count total cameo matches
        let cameo_matches: usize = KNOWN_CAMEO_NAMES
            .iter()
            .filter(|&&name| archive.get_by_name(name).is_some())
            .count();

        eprintln!(
            "  {:<16} — {} entries, GAPOWR={}, E1={}, CONSC={}, total cameo matches={}",
            mix_name,
            archive.entry_count(),
            if has_gapowr { "YES" } else { "no" },
            if has_e1 { "YES" } else { "no" },
            if has_consc { "YES" } else { "no" },
            cameo_matches
        );

        // If we found cameos, list them
        if cameo_matches > 0 {
            eprintln!("    ** CAMEO ICONS FOUND in {} **", mix_name);
            for &name in KNOWN_CAMEO_NAMES {
                if let Some(data) = archive.get_by_name(name) {
                    eprintln!("    {} — {} bytes", name, data.len());
                }
            }
        }
    }

    eprintln!("\n{}", "=".repeat(60));
    eprintln!("  Search complete.");
    eprintln!("{}", "=".repeat(60));
}
