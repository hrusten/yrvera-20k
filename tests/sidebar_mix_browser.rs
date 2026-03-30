//! Browse contents of sidec01.mix, sidec02.mix, and sidec02md.mix.
//!
//! Lists every entry: hash ID, size, detected file type, SHP dimensions
//! if applicable, and matched filename from a large brute-force dictionary.
//!
//! Run with: cargo test --test sidebar_mix_browser -- --nocapture

use vera20k::assets::asset_manager::AssetManager;
use vera20k::assets::mix_archive::MixArchive;
use vera20k::assets::mix_hash::mix_hash;
use vera20k::util::config::GameConfig;

/// Build a massive dictionary of plausible sidebar filenames to reverse-lookup hashes.
fn build_hash_dictionary() -> Vec<(String, i32)> {
    let mut names: Vec<String> = Vec::new();

    // --- Known sidebar chrome ---
    for n in [
        "radar.shp",
        "side1.shp",
        "side2.shp",
        "side3.shp",
        "side2a.shp",
        "side2b.shp",
        "side3a.shp",
        "side3b.shp",
        "tabs.shp",
        "repair.shp",
        "sell.shp",
        "power.shp",
        "credits.shp",
        "clock.shp",
        "sidebar.pal",
    ] {
        names.push(n.to_string());
    }

    // tabNN.shp for 00-19
    for i in 0..20 {
        names.push(format!("tab{:02}.shp", i));
    }

    // sideN.shp, sideNa/b.shp for 1-9
    for i in 1..=9 {
        names.push(format!("side{}.shp", i));
        names.push(format!("side{}a.shp", i));
        names.push(format!("side{}b.shp", i));
    }

    // --- Palettes ---
    for name in [
        "sidebar",
        "sidebarp",
        "sidebarmd",
        "chrome",
        "cameo",
        "unit",
        "unittem",
        "unitsno",
        "uniturb",
        "unitdes",
        "unitlun",
        "temperat",
        "snow",
        "urban",
        "desert",
        "lunar",
        "newurban",
        "isotem",
        "isosno",
        "isourb",
        "isodes",
        "isolun",
        "isonurb",
        "grftxt",
        "mousepal",
        "anim",
        "lib",
        "theater",
    ] {
        names.push(format!("{}.pal", name));
    }

    // --- RA2/YR sidebar UI filenames from various references ---
    // RA2/YR sidebar UI filenames from community documentation
    let sidebar_names = [
        // Background/chrome pieces
        "sidebar.shp",
        "sidebarbg.shp",
        "sidec.shp",
        "chromeframe.shp",
        "chrome.shp",
        // Buttons and controls
        "bttn.shp",
        "button.shp",
        "btn.shp",
        "repair2.shp",
        "sell2.shp",
        "power2.shp",
        "repairon.shp",
        "sellon.shp",
        "poweron.shp",
        "repairoff.shp",
        "selloff.shp",
        "poweroff.shp",
        "pgup.shp",
        "pgdn.shp",
        "up.shp",
        "down.shp",
        "hscroll.shp",
        "vscroll.shp",
        "scroll.shp",
        "scrollup.shp",
        "scrolldn.shp",
        // Radar
        "radarbg.shp",
        "radarfr.shp",
        "radarui.shp",
        "radarframe.shp",
        "radarlogo.shp",
        // Build queue UI
        "strip.shp",
        "cameo.shp",
        "queue.shp",
        "ready.shp",
        "hold.shp",
        "onhold.shp",
        "paused.shp",
        "upgrade.shp",
        "upgrdarw.shp",
        // Misc UI
        "options.shp",
        "diplomcy.shp",
        "battle.shp",
        "mslogo.shp",
        "dialog.shp",
        "dialogs.shp",
        "menu.shp",
        "menubar.shp",
        "mfill.shp",
        "mbar.shp",
        "mbtn.shp",
        // TS/RA2 specific
        "grdylw.shp",
        "grred.shp",
        "grgrn.shp",
        "grwht.shp",
        "gryel.shp",
        "pbar.shp",
        "pbargrn.shp",
        "pbarred.shp",
        "hbar.shp",
        "hpbar.shp",
        "hpips.shp",
        "pwrbar.shp",
        "pwrbaron.shp",
        "pwrbaroff.shp",
        // Tooltip / text
        "tooltip.shp",
        "txtbg.shp",
        "text.shp",
        // Version/logo
        "version.shp",
        "logo.shp",
        "westwood.shp",
        "title.shp",
        "titlebar.shp",
        // Cursor-related
        "mouse.shp",
        "cursor.shp",
        "pointer.shp",
        // Map preview
        "preview.shp",
        "pview.shp",
        // EVA
        "eva.shp",
        "evabg.shp",
        "evabar.shp",
        // Misc small assets
        "select.shp",
        "health.shp",
        "pips.shp",
        "pips2.shp",
        "rank.shp",
        "vet.shp",
        "elite.shp",
        "spyplane.shp",
        "paradrop.shp",
        "nuke.shp",
        "lightning.shp",
        "chrono.shp",
        "iron.shp",
        "ironcurt.shp",
        // Waypoint/beacon
        "waypoint.shp",
        "beacon.shp",
        "waypointicon.shp",
        "beaconicon.shp",
        // Guard/stop/deploy
        "guard.shp",
        "stop.shp",
        "deploy.shp",
        "guardicon.shp",
        "stopicon.shp",
        "deployicon.shp",
    ];
    for n in sidebar_names {
        names.push(n.to_string());
    }

    // --- Brute-force common RA2 naming patterns ---
    // {prefix}{NN}.shp for various prefixes
    let prefixes = [
        "side", "tab", "btn", "bttn", "ctrl", "knob", "bar", "strip", "slot", "cell", "cam",
        "icon", "pip", "tic", "clock", "radar", "pbar", "hbar", "grn", "red", "yel", "wht", "gry",
        "grdylw", "grred", "grgrn", "grwht",
    ];
    for prefix in prefixes {
        for i in 0..30 {
            names.push(format!("{}{:02}.shp", prefix, i));
            names.push(format!("{}{}.shp", prefix, i));
        }
    }

    // YR/md variants
    for base in [
        "sidebar", "sidebarp", "radar", "side1", "side2", "side3", "tabs", "tab00", "tab01",
        "tab02", "tab03", "repair", "sell", "power", "credits", "strip", "cameo", "chrome",
    ] {
        names.push(format!("{}md.shp", base));
        names.push(format!("{}.shp", base));
        names.push(format!("{}md.pal", base));
        names.push(format!("{}.pal", base));
    }

    // Single-word RA2 filenames (from community file lists)
    let misc = [
        "gafscrn",
        "gafscrnmd",
        "nafscrnmd",
        "nafscreen",
        "yafscrn",
        "yafscrnmd",
        "gascren",
        "nascren",
        "yascren",
        "gaside1",
        "gaside2",
        "gaside3",
        "naside1",
        "naside2",
        "naside3",
        "yaside1",
        "yaside2",
        "yaside3",
        "gatabs",
        "natabs",
        "yatabs",
        "garadar",
        "naradar",
        "yaradar",
        "sldbkgd",
        "sldbar",
        "sldbkg",
        "bkgnd",
        "bkgd",
        "background",
        "pwrup",
        "pwrdn",
        "pwrbar",
        "credbar",
        "credbg",
        "crednum",
        "mnubtns",
        "mnubtn",
        "menubtn",
        "optbtns",
        "optbtn",
        "frame",
        "framebg",
        "framefg",
    ];
    for n in misc {
        names.push(format!("{}.shp", n));
        names.push(format!("{}.pal", n));
    }

    // Build dict with hashes, deduplicate by hash
    let mut dict: Vec<(String, i32)> = names
        .into_iter()
        .map(|n| {
            let h = mix_hash(&n);
            (n, h)
        })
        .collect();
    dict.sort_by_key(|(_, h)| *h);
    dict.dedup_by_key(|(_, h)| *h);
    dict
}

/// Detect file type from header bytes and return a description string.
fn detect_file_type(data: &[u8]) -> String {
    if data.len() < 4 {
        return format!("tiny ({} bytes)", data.len());
    }

    // PAL: exactly 768 bytes, all values 0-63 (VGA 6-bit)
    if data.len() == 768 {
        let all_vga = data.iter().all(|&b| b <= 63);
        if all_vga {
            return "PAL (VGA 6-bit)".to_string();
        }
        return "PAL (768 bytes)".to_string();
    }

    let w0 = u16::from_le_bytes([data[0], data[1]]);

    // SHP(TS) new format: starts with 0x0000
    if w0 == 0 && data.len() >= 24 {
        let num_images = u16::from_le_bytes([data[2], data[3]]);
        // Bytes 4-5: unused/x, 6-7: width, 8-9: height
        let width = u16::from_le_bytes([data[6], data[7]]);
        let height = u16::from_le_bytes([data[8], data[9]]);
        if num_images > 0
            && num_images < 500
            && width > 0
            && width < 2000
            && height > 0
            && height < 2000
        {
            return format!("SHP(TS) {}x{} {} frames", width, height, num_images);
        }
    }

    // SHP old format (TD/RA1): first u16 = frame count
    if w0 > 0 && w0 < 200 && data.len() >= 14 {
        // Old SHP: after frame count, offsets table starts
        // Check if byte 2-3 looks like a reasonable offset
        let first_offset = u32::from_le_bytes([data[2], data[3], data[4], data[5]]);
        let expected_offset = 2 + (w0 as u32 + 2) * 8; // header + offsets table
        if first_offset > 0 && first_offset < data.len() as u32 {
            return format!("SHP(old?) {} frames, off0={}", w0, first_offset);
        }
    }

    // XCC-style local mix database (LMD)
    if data.len() > 52 && &data[0..4] == b"XCC " {
        return "XCC database".to_string();
    }

    // Check for ASCII text
    let ascii_count = data
        .iter()
        .take(64)
        .filter(|&&b| b >= 0x20 && b <= 0x7E)
        .count();
    if ascii_count > 48 {
        let preview: String = data
            .iter()
            .take(40)
            .map(|&b| {
                if b >= 0x20 && b <= 0x7E {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        return format!("text? \"{}...\"", preview);
    }

    format!(
        "binary ({}B, hdr={:02X}{:02X}{:02X}{:02X})",
        data.len(),
        data[0],
        data[1],
        data[2],
        data[3]
    )
}

fn browse_mix(asset_manager: &AssetManager, mix_name: &str, dict: &[(String, i32)]) {
    eprintln!("\n{}", "=".repeat(90));
    eprintln!("  MIX: {}", mix_name);
    eprintln!("{}", "=".repeat(90));

    let Some(mix_data) = asset_manager.get(mix_name) else {
        eprintln!("  NOT FOUND in asset manager");
        return;
    };
    eprintln!(
        "  Archive size: {} bytes ({:.1} KB)",
        mix_data.len(),
        mix_data.len() as f64 / 1024.0
    );

    let mix = match MixArchive::from_bytes(mix_data.to_vec()) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("  PARSE ERROR: {}", e);
            return;
        }
    };

    let entries = mix.entries();
    eprintln!("  Entries: {}", entries.len());
    eprintln!();
    eprintln!(
        "  {:>3} | {:>11} | {:>8} | {:<30} | {}",
        "#", "Hash", "Size", "Name", "Type / Details"
    );
    eprintln!(
        "  {} | {} | {} | {} | {}",
        "-".repeat(3),
        "-".repeat(11),
        "-".repeat(8),
        "-".repeat(30),
        "-".repeat(40)
    );

    let mut matched: usize = 0;

    for (i, entry) in entries.iter().enumerate() {
        let name = dict
            .iter()
            .find(|(_, h)| *h == entry.id)
            .map(|(n, _)| {
                matched += 1;
                n.clone()
            })
            .unwrap_or_else(|| format!("??? (id={:#010X})", entry.id));

        let type_info = mix
            .get_by_id(entry.id)
            .map(|data| detect_file_type(data))
            .unwrap_or_else(|| "read error".to_string());

        eprintln!(
            "  {:>3} | {:#011X} | {:>8} | {:<30} | {}",
            i, entry.id, entry.size, name, type_info
        );
    }

    eprintln!();
    eprintln!("  Identified: {}/{}", matched, entries.len());
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn browse_sidebar_mix_files() {
    let config = match GameConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Skipping: config.toml not found ({e})");
            return;
        }
    };
    let asset_manager = match AssetManager::new(&config.paths.ra2_dir) {
        Ok(am) => am,
        Err(e) => {
            eprintln!("Skipping: AssetManager init failed ({e})");
            return;
        }
    };

    let dict = build_hash_dictionary();
    eprintln!("Hash dictionary: {} candidate filenames", dict.len());

    browse_mix(&asset_manager, "sidec01.mix", &dict);
    browse_mix(&asset_manager, "sidec02.mix", &dict);
    browse_mix(&asset_manager, "sidec02md.mix", &dict);
}
