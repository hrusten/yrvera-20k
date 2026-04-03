//! Overlay and terrain object parsing from RA2 map files.
//!
//! RA2 maps have three overlay-related sections:
//! - `[OverlayPack]`: base64 → LCW compressed → 262,144 bytes (512×512 grid).
//!   Each byte is an overlay type ID (0xFF = no overlay).
//! - `[OverlayDataPack]`: same format. Each byte is a frame/value for the overlay
//!   (e.g., ore density 0-11, wall connection frame).
//! - `[Terrain]`: INI key-value pairs. Key = `ry * 1000 + rx`, value = object name
//!   (e.g., "INTREE01", "CACTUS01").
//!
//! ## Dependency rules
//! - Part of map/ — depends on util/ (base64, lcw), rules/ (ini_parser).

use std::collections::HashMap;

use crate::map::overlay_types::OverlayTypeRegistry;
use crate::rules::ini_parser::IniFile;
use crate::util::base64;
use crate::util::lcw;

/// Width of the overlay grid (RA2 maps are max 512×512 cells).
const OVERLAY_GRID_SIZE: usize = 512;

/// Total cells in the overlay grid (512 × 512 = 262,144).
const OVERLAY_TOTAL_CELLS: usize = OVERLAY_GRID_SIZE * OVERLAY_GRID_SIZE;

/// Sentinel value meaning "no overlay at this cell".
const NO_OVERLAY: u8 = 0xFF;

/// An overlay placed on a specific map cell.
///
/// Overlays include ore, gems, walls, fences, bridges, rocks, and other
/// decorative or gameplay objects that sit on top of terrain tiles.
#[derive(Debug, Clone)]
pub struct OverlayEntry {
    /// Isometric X coordinate.
    pub rx: u16,
    /// Isometric Y coordinate.
    pub ry: u16,
    /// Overlay type ID (index into the [OverlayTypes] registry in rules.ini).
    pub overlay_id: u8,
    /// Frame/value from OverlayDataPack (ore density, wall frame, etc.).
    pub frame: u8,
}

/// A named terrain object placed on the map (trees, rocks).
///
/// These come from the [Terrain] INI section. Unlike overlays (which are
/// ID-indexed), terrain objects are named directly (e.g., "INTREE01").
#[derive(Debug, Clone)]
pub struct TerrainObject {
    /// Isometric X coordinate.
    pub rx: u16,
    /// Isometric Y coordinate.
    pub ry: u16,
    /// Object name (e.g., "INTREE01", "CACTUS01", "ROCK01").
    pub name: String,
}

/// Parse overlay data from [OverlayPack] and [OverlayDataPack] sections.
///
/// Both sections are base64-encoded, LCW-compressed grids of 262,144 bytes.
/// Returns a list of cells where an overlay is present (type != 0xFF).
pub fn parse_overlays(ini: &IniFile) -> Vec<OverlayEntry> {
    let overlay_pack: Vec<u8> = match decode_pack_section(ini, "OverlayPack") {
        Some(data) => data,
        None => {
            log::trace!("No [OverlayPack] section in map");
            return Vec::new();
        }
    };
    let data_pack: Vec<u8> = decode_pack_section(ini, "OverlayDataPack")
        .unwrap_or_else(|| vec![0u8; OVERLAY_TOTAL_CELLS]);

    let mut entries: Vec<OverlayEntry> = Vec::new();
    let max_idx: usize = overlay_pack.len().min(OVERLAY_TOTAL_CELLS);

    for idx in 0..max_idx {
        let overlay_id: u8 = overlay_pack[idx];
        if overlay_id == NO_OVERLAY {
            continue;
        }
        let rx: u16 = (idx % OVERLAY_GRID_SIZE) as u16;
        let ry: u16 = (idx / OVERLAY_GRID_SIZE) as u16;
        let frame: u8 = if idx < data_pack.len() {
            data_pack[idx]
        } else {
            0
        };

        entries.push(OverlayEntry {
            rx,
            ry,
            overlay_id,
            frame,
        });
    }

    log::info!("Parsed {} overlay entries from OverlayPack", entries.len());
    entries
}

/// Parse terrain objects from the [Terrain] INI section.
///
/// Keys are encoded positions: `ry * 1000 + rx` (decimal string).
/// Values are object names like "INTREE01", "CACTUS01", "ROCK01".
pub fn parse_terrain_objects(ini: &IniFile) -> Vec<TerrainObject> {
    let section = match ini.section("Terrain") {
        Some(s) => s,
        None => {
            log::trace!("No [Terrain] section in map");
            return Vec::new();
        }
    };

    let mut objects: Vec<TerrainObject> = Vec::new();

    for key in section.keys() {
        let pos: u32 = match key.parse::<u32>() {
            Ok(v) => v,
            Err(_) => continue,
        };

        let name: String = match section.get(key) {
            Some(v) if !v.is_empty() => v.to_uppercase(),
            _ => continue,
        };

        let ry: u16 = (pos / 1000) as u16;
        let rx: u16 = (pos % 1000) as u16;

        objects.push(TerrainObject { rx, ry, name });
    }

    log::info!("Parsed {} terrain objects from [Terrain]", objects.len());
    objects
}

/// Decode a base64-encoded, LCW-compressed pack section (OverlayPack or OverlayDataPack).
///
/// Concatenates all numbered key values, base64 decodes, then LCW decompresses.
fn decode_pack_section(ini: &IniFile, section_name: &str) -> Option<Vec<u8>> {
    let section = ini.section(section_name)?;

    let mut b64_data: String = String::new();
    for key in section.keys() {
        if let Some(val) = section.get(key) {
            b64_data.push_str(val);
        }
    }

    if b64_data.is_empty() {
        return None;
    }

    let compressed: Vec<u8> = match base64::base64_decode(&b64_data) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("{} base64 decode failed: {}", section_name, e);
            return None;
        }
    };

    match lcw::decompress_chunks(&compressed) {
        Ok(data) => {
            log::info!(
                "{}: {} compressed → {} decompressed bytes",
                section_name,
                compressed.len(),
                data.len()
            );
            Some(data)
        }
        Err(e) => {
            log::warn!("{} LCW decompress failed: {}", section_name, e);
            None
        }
    }
}

/// Compute wall connectivity frames for all wall overlays.
///
/// RA2 walls use a 4-bit cardinal-direction bitmask to select their SHP frame:
///   bit 0 (1) = North neighbor (Y − 1, screen top-right)
///   bit 1 (2) = East  neighbor (X + 1, screen bottom-right)
///   bit 2 (4) = South neighbor (Y + 1, screen bottom-left)
///   bit 3 (8) = West  neighbor (X − 1, screen top-left)
///
/// Frame 0 = isolated pillar, 5 = NE–SW straight, 10 = NW–SE straight, 15 = cross.
/// Connectivity is same-type only (GAWALL connects to GAWALL, not NAWALL).
/// Modifies `entries` in-place, setting `entry.frame` for every wall overlay.
/// Returns the number of wall entries updated.
pub fn compute_wall_connectivity(
    entries: &mut [OverlayEntry],
    overlay_registry: &OverlayTypeRegistry,
) -> u32 {
    // Build spatial index: (rx, ry) → overlay_id for wall cells only.
    let mut wall_cells: HashMap<(u16, u16), u8> = HashMap::new();
    for entry in entries.iter() {
        let is_wall: bool = overlay_registry
            .flags(entry.overlay_id)
            .map(|f| f.wall)
            .unwrap_or(false);
        if is_wall {
            wall_cells.insert((entry.rx, entry.ry), entry.overlay_id);
        }
    }

    if wall_cells.is_empty() {
        return 0;
    }

    let mut updated: u32 = 0;
    for entry in entries.iter_mut() {
        let is_wall: bool = overlay_registry
            .flags(entry.overlay_id)
            .map(|f| f.wall)
            .unwrap_or(false);
        if !is_wall {
            continue;
        }

        let (rx, ry) = (entry.rx, entry.ry);
        let mut bitmask: u8 = 0;

        // Bit 0: North (Y − 1, screen top-right).
        if ry > 0 {
            if wall_cells.get(&(rx, ry - 1)) == Some(&entry.overlay_id) {
                bitmask |= 1;
            }
        }
        // Bit 1: East (X + 1, screen bottom-right).
        if let Some(&id) = wall_cells.get(&(rx.wrapping_add(1), ry)) {
            if id == entry.overlay_id {
                bitmask |= 2;
            }
        }
        // Bit 2: South (Y + 1, screen bottom-left).
        if let Some(&id) = wall_cells.get(&(rx, ry.wrapping_add(1))) {
            if id == entry.overlay_id {
                bitmask |= 4;
            }
        }
        // Bit 3: West (X − 1, screen top-left).
        if rx > 0 {
            if wall_cells.get(&(rx - 1, ry)) == Some(&entry.overlay_id) {
                bitmask |= 8;
            }
        }

        entry.frame = bitmask;
        if updated < 10 {
            log::info!(
                "  Wall[{}] at ({},{}) id={} → bitmask={} (N={} E={} S={} W={})",
                updated,
                rx,
                ry,
                entry.overlay_id,
                bitmask,
                bitmask & 1 != 0,
                bitmask & 2 != 0,
                bitmask & 4 != 0,
                bitmask & 8 != 0,
            );
        }
        updated += 1;
    }

    log::info!(
        "Wall connectivity: {} entries updated ({} unique wall cells)",
        updated,
        wall_cells.len()
    );
    updated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_terrain_objects() {
        let text: &str = "\
[Terrain]
35042=INTREE01
35043=CACTUS01
72015=ROCK01
";
        let ini: IniFile = IniFile::from_str(text);
        let objects: Vec<TerrainObject> = parse_terrain_objects(&ini);
        assert_eq!(objects.len(), 3);

        // Key 35042 → ry=35, rx=42
        let tree = objects.iter().find(|o| o.name == "INTREE01").unwrap();
        assert_eq!(tree.rx, 42);
        assert_eq!(tree.ry, 35);

        // Key 72015 → ry=72, rx=15
        let rock = objects.iter().find(|o| o.name == "ROCK01").unwrap();
        assert_eq!(rock.rx, 15);
        assert_eq!(rock.ry, 72);
    }

    #[test]
    fn test_no_overlay_section() {
        let text: &str = "[Map]\nTheater=TEMPERATE\n";
        let ini: IniFile = IniFile::from_str(text);
        let overlays: Vec<OverlayEntry> = parse_overlays(&ini);
        assert!(overlays.is_empty());
    }

    #[test]
    fn test_no_terrain_section() {
        let text: &str = "[Map]\nTheater=TEMPERATE\n";
        let ini: IniFile = IniFile::from_str(text);
        let objects: Vec<TerrainObject> = parse_terrain_objects(&ini);
        assert!(objects.is_empty());
    }

    /// Build a minimal OverlayTypeRegistry where overlay_id 0 = GAWALL (wall)
    /// and overlay_id 1 = NAWALL (wall), overlay_id 2 = GEM01 (non-wall).
    fn test_wall_registry() -> OverlayTypeRegistry {
        let text: &str = "\
[OverlayTypes]
0=GAWALL
1=NAWALL
2=GEM01
[GAWALL]
Wall=yes
[NAWALL]
Wall=yes
[GEM01]
Tiberium=yes
";
        let ini: IniFile = IniFile::from_str(text);
        OverlayTypeRegistry::from_ini(&ini, None)
    }

    #[test]
    fn wall_connectivity_isolated() {
        let reg = test_wall_registry();
        let mut entries = vec![OverlayEntry {
            rx: 10,
            ry: 10,
            overlay_id: 0,
            frame: 99,
        }];
        let updated = compute_wall_connectivity(&mut entries, &reg);
        assert_eq!(updated, 1);
        assert_eq!(entries[0].frame, 0, "isolated wall = frame 0 (pillar)");
    }

    #[test]
    fn wall_connectivity_north_neighbor() {
        let reg = test_wall_registry();
        let mut entries = vec![
            OverlayEntry {
                rx: 10,
                ry: 10,
                overlay_id: 0,
                frame: 0,
            },
            OverlayEntry {
                rx: 10,
                ry: 9,
                overlay_id: 0,
                frame: 0,
            }, // North
        ];
        compute_wall_connectivity(&mut entries, &reg);
        // (10,10) has neighbor at (10,9) = North → bit 0
        assert_eq!(entries[0].frame, 1);
        // (10,9) has neighbor at (10,10) = South → bit 2
        assert_eq!(entries[1].frame, 4);
    }

    #[test]
    fn wall_connectivity_straight_ns() {
        let reg = test_wall_registry();
        // Three walls in a North–South line (same X, consecutive Y).
        let mut entries = vec![
            OverlayEntry {
                rx: 5,
                ry: 4,
                overlay_id: 0,
                frame: 0,
            },
            OverlayEntry {
                rx: 5,
                ry: 5,
                overlay_id: 0,
                frame: 0,
            },
            OverlayEntry {
                rx: 5,
                ry: 6,
                overlay_id: 0,
                frame: 0,
            },
        ];
        compute_wall_connectivity(&mut entries, &reg);
        assert_eq!(entries[0].frame, 4, "top end: South only = bit 2");
        assert_eq!(entries[1].frame, 5, "middle: N+S = bits 0+2 = 5");
        assert_eq!(entries[2].frame, 1, "bottom end: North only = bit 0");
    }

    #[test]
    fn wall_connectivity_straight_ew() {
        let reg = test_wall_registry();
        let mut entries = vec![
            OverlayEntry {
                rx: 4,
                ry: 5,
                overlay_id: 0,
                frame: 0,
            },
            OverlayEntry {
                rx: 5,
                ry: 5,
                overlay_id: 0,
                frame: 0,
            },
            OverlayEntry {
                rx: 6,
                ry: 5,
                overlay_id: 0,
                frame: 0,
            },
        ];
        compute_wall_connectivity(&mut entries, &reg);
        assert_eq!(entries[0].frame, 2, "west end: East only = bit 1");
        assert_eq!(entries[1].frame, 10, "middle: E+W = bits 1+3 = 10");
        assert_eq!(entries[2].frame, 8, "east end: West only = bit 3");
    }

    #[test]
    fn wall_connectivity_cross() {
        let reg = test_wall_registry();
        let mut entries = vec![
            OverlayEntry {
                rx: 5,
                ry: 5,
                overlay_id: 0,
                frame: 0,
            }, // center
            OverlayEntry {
                rx: 5,
                ry: 4,
                overlay_id: 0,
                frame: 0,
            }, // North
            OverlayEntry {
                rx: 6,
                ry: 5,
                overlay_id: 0,
                frame: 0,
            }, // East
            OverlayEntry {
                rx: 5,
                ry: 6,
                overlay_id: 0,
                frame: 0,
            }, // South
            OverlayEntry {
                rx: 4,
                ry: 5,
                overlay_id: 0,
                frame: 0,
            }, // West
        ];
        compute_wall_connectivity(&mut entries, &reg);
        assert_eq!(entries[0].frame, 15, "center with all 4 neighbors = cross");
    }

    #[test]
    fn wall_connectivity_same_type_only() {
        let reg = test_wall_registry();
        // GAWALL (id=0) at (5,5) with NAWALL (id=1) neighbors — should NOT connect.
        let mut entries = vec![
            OverlayEntry {
                rx: 5,
                ry: 5,
                overlay_id: 0,
                frame: 0,
            },
            OverlayEntry {
                rx: 5,
                ry: 4,
                overlay_id: 1,
                frame: 0,
            }, // NAWALL North
            OverlayEntry {
                rx: 6,
                ry: 5,
                overlay_id: 1,
                frame: 0,
            }, // NAWALL East
        ];
        compute_wall_connectivity(&mut entries, &reg);
        assert_eq!(entries[0].frame, 0, "GAWALL ignores NAWALL neighbors");
        assert_eq!(entries[1].frame, 0, "NAWALL ignores GAWALL neighbors");
    }

    #[test]
    fn wall_connectivity_skips_non_walls() {
        let reg = test_wall_registry();
        let mut entries = vec![
            OverlayEntry {
                rx: 5,
                ry: 5,
                overlay_id: 2,
                frame: 7,
            }, // GEM01, not wall
        ];
        let updated = compute_wall_connectivity(&mut entries, &reg);
        assert_eq!(updated, 0);
        assert_eq!(entries[0].frame, 7, "non-wall frame unchanged");
    }

    #[test]
    fn wall_connectivity_no_walls() {
        let reg = test_wall_registry();
        let mut entries: Vec<OverlayEntry> = Vec::new();
        let updated = compute_wall_connectivity(&mut entries, &reg);
        assert_eq!(updated, 0);
    }
}
