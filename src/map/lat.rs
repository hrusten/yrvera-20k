//! LAT (Lateral Terrain) automatic terrain transitions.
//!
//! RA2 theaters define "ground types" (rough, sand, pavement, green) in the
//! theater INI [General] section. Each ground type has a base tileset and a
//! 16-tile transition tileset. At map load time, tiles belonging to a ground
//! type are checked against their 4 diamond neighbors. If a neighbor belongs
//! to a different ground type, the tile is replaced with the appropriate
//! transition variant.
//!
//! The 16 transition patterns correspond to all possible combinations of
//! the 4 isometric neighbors (NE, NW, SE, SW) being the same or different
//! ground type. This is encoded as a 4-bit mask.
//!
//! ## Dependency rules
//! - Part of map/ — depends on rules/ (ini_parser), map/theater (TilesetLookup).

use std::collections::HashMap;

use crate::map::map_file::MapCell;
use crate::map::theater::TilesetLookup;
use crate::rules::ini_parser::IniFile;

/// Maps a 4-bit neighbor mask to the LAT transition tile offset (0-15).
///
/// Bit layout: bit 3 = NE is different, bit 2 = NW is different,
/// bit 1 = SE is different, bit 0 = SW is different.
///
/// Mask 0 (all neighbors same) means no transition needed.
const MASK_TO_LAT_INDEX: [u8; 16] = [
    0xFF, // 0b0000: all same → no transition (keep ground tile)
    4,    // 0b0001: SW different
    2,    // 0b0010: SE different
    6,    // 0b0011: SE+SW different
    8,    // 0b0100: NW different
    12,   // 0b0101: NW+SW different
    10,   // 0b0110: NW+SE different
    14,   // 0b0111: NW+SE+SW different
    1,    // 0b1000: NE different
    5,    // 0b1001: NE+SW different
    3,    // 0b1010: NE+SE different
    7,    // 0b1011: NE+SE+SW different
    9,    // 0b1100: NE+NW different
    13,   // 0b1101: NE+NW+SW different
    11,   // 0b1110: NE+NW+SE different
    15,   // 0b1111: all different → island
];

/// One LAT ground type definition parsed from the theater INI [General] section.
#[derive(Debug, Clone)]
pub struct LatGroundType {
    /// Display name (e.g., "Rough", "Sand").
    pub name: String,
    /// Tileset index for the base ground tiles (e.g., RoughTile=13).
    pub ground_tileset: u16,
    /// Tileset index for the 16 transition tiles (e.g., ClearToRoughLat=14).
    pub transition_tileset: u16,
    /// Start tile_id of the transition tileset (for direct tile_id replacement).
    pub transition_start_tile: u16,
    /// Tileset indices that "connect" seamlessly (no transition applied).
    pub connect_to: Vec<u16>,
}

/// LAT configuration for a theater, parsed from the [General] section.
#[derive(Debug, Clone)]
pub struct LatConfig {
    /// All ground types defined in this theater.
    pub grounds: Vec<LatGroundType>,
}

/// Parse LAT ground type definitions from a theater INI's [General] section.
///
/// Standard RA2 ground types: Rough, Sand, Pave, Green.
/// Each has a base tile key, transition tile key, and optional ConnectTo list.
pub fn parse_lat_config(ini_data: &[u8], lookup: &TilesetLookup) -> LatConfig {
    let ini: IniFile = match IniFile::from_bytes(ini_data) {
        Ok(i) => i,
        Err(_) => {
            return LatConfig {
                grounds: Vec::new(),
            };
        }
    };

    let general = match ini.section("General") {
        Some(s) => s,
        None => {
            log::info!("LAT: no [General] section in theater INI");
            return LatConfig {
                grounds: Vec::new(),
            };
        }
    };

    let bounds = lookup.bounds();

    /// Helper: parse a comma-separated list of tileset indices.
    fn parse_connect_to(s: Option<&str>) -> Vec<u16> {
        match s {
            Some(val) => val
                .split(',')
                .filter_map(|v| v.trim().parse::<u16>().ok())
                .collect(),
            None => Vec::new(),
        }
    }

    // Standard RA2 LAT ground type definitions.
    let lat_defs: &[(&str, &str, &str, &str)] = &[
        ("Rough", "RoughTile", "ClearToRoughLat", "RoughConnectTo"),
        ("Sand", "SandTile", "ClearToSandLat", "SandConnectTo"),
        ("Pave", "PaveTile", "ClearToPaveLat", "PaveConnectTo"),
        ("Green", "GreenTile", "ClearToGreenLat", "GreenConnectTo"),
    ];

    let mut grounds: Vec<LatGroundType> = Vec::new();

    for &(name, ground_key, transition_key, connect_key) in lat_defs {
        let ground_ts: i32 = match general.get_i32(ground_key) {
            Some(v) => v,
            None => continue,
        };
        let transition_ts: i32 = match general.get_i32(transition_key) {
            Some(v) => v,
            None => continue,
        };

        // Look up the start tile_id for the transition tileset.
        let trans_start: u16 = match bounds.get(transition_ts as usize) {
            Some(b) => b.start,
            None => {
                log::warn!(
                    "LAT {}: transition tileset {} out of range",
                    name,
                    transition_ts
                );
                continue;
            }
        };

        let connect_to: Vec<u16> = parse_connect_to(general.get(connect_key));

        log::info!(
            "LAT {}: ground tileset={}, transition tileset={} (start_tile={}), connect_to={:?}",
            name,
            ground_ts,
            transition_ts,
            trans_start,
            connect_to
        );

        grounds.push(LatGroundType {
            name: name.to_string(),
            ground_tileset: ground_ts as u16,
            transition_tileset: transition_ts as u16,
            transition_start_tile: trans_start,
            connect_to,
        });
    }

    // LAT exemption pairs (ra2_yr_map_terrain.md §2.5):
    // Certain tileset pairs never generate transitions between each other.
    // Adding exempt tilesets to connect_to makes tile_matches_ground() treat
    // them as "same ground", preventing spurious transition tiles.
    let exemptions: &[(&str, &str)] = &[
        ("Pave", "MiscPaveTile"),
        ("Pave", "Medians"),
        ("Pave", "PavedRoads"),
        ("Green", "ShorePieces"),
        ("Green", "WaterBridge"),
    ];
    for &(ground_name, ini_key) in exemptions {
        if let Some(ts_idx) = general.get_i32(ini_key) {
            if let Some(ground) = grounds.iter_mut().find(|g| g.name == ground_name) {
                let ts: u16 = ts_idx as u16;
                if !ground.connect_to.contains(&ts) {
                    ground.connect_to.push(ts);
                    log::info!(
                        "LAT exemption: {} ↔ {} (tileset {})",
                        ground_name,
                        ini_key,
                        ts
                    );
                }
            }
        }
    }

    log::info!("LAT: {} ground types configured", grounds.len());
    LatConfig { grounds }
}

/// Check if a tile belongs to a LAT ground type (base tile, transition, or connected).
fn tile_matches_ground(tile_id: u16, ground: &LatGroundType, lookup: &TilesetLookup) -> bool {
    let ts: Option<u16> = lookup.tileset_index(tile_id);
    match ts {
        Some(idx) => {
            idx == ground.ground_tileset
                || idx == ground.transition_tileset
                || ground.connect_to.contains(&idx)
        }
        None => false,
    }
}

/// Apply LAT transitions to map cells in-place.
///
/// For each cell that belongs to a LAT ground type's base tileset:
/// 1. Check 4 diamond neighbors (NE, NW, SE, SW)
/// 2. Build a 4-bit mask of which neighbors are NOT the same ground type
/// 3. If mask != 0, replace tile_id with the transition tile at the
///    appropriate offset
///
/// Cells are modified in-place. Only cells whose tile belongs to a
/// LAT base tileset are candidates for replacement.
pub fn apply_lat(cells: &mut [MapCell], lat_config: &LatConfig, lookup: &TilesetLookup) {
    if lat_config.grounds.is_empty() {
        return;
    }

    // Build spatial lookup: (rx, ry) → tile_index for neighbor checking.
    let mut tile_map: HashMap<(u16, u16), i32> = HashMap::with_capacity(cells.len());
    for cell in cells.iter() {
        tile_map.insert((cell.rx, cell.ry), cell.tile_index);
    }

    let mut changes: u32 = 0;

    for cell in cells.iter_mut() {
        // Skip empty/no-tile cells.
        if cell.tile_index < 0 || cell.tile_index == 0xFFFF {
            continue;
        }

        let tile_id: u16 = cell.tile_index as u16;
        let ts_idx: Option<u16> = lookup.tileset_index(tile_id);

        // Find which ground type this tile's base tileset belongs to.
        let ground: Option<&LatGroundType> = lat_config
            .grounds
            .iter()
            .find(|g| ts_idx == Some(g.ground_tileset));
        let ground: &LatGroundType = match ground {
            Some(g) => g,
            None => continue, // Not a LAT ground tile — skip.
        };

        // Check 4 diamond neighbors.
        // NE = (rx, ry-1), NW = (rx-1, ry), SE = (rx+1, ry), SW = (rx, ry+1)
        let neighbors: [(i32, i32); 4] = [
            (cell.rx as i32, cell.ry as i32 - 1), // NE (bit 3)
            (cell.rx as i32 - 1, cell.ry as i32), // NW (bit 2)
            (cell.rx as i32 + 1, cell.ry as i32), // SE (bit 1)
            (cell.rx as i32, cell.ry as i32 + 1), // SW (bit 0)
        ];

        let mut mask: u8 = 0;
        for (bit_pos, &(nx, ny)) in [3u8, 2, 1, 0].iter().zip(neighbors.iter()) {
            // Out-of-bounds or negative coords treated as "same" (no transition at edges).
            if nx < 0 || ny < 0 {
                continue; // Same ground → bit stays 0.
            }
            let neighbor_tile: i32 = tile_map.get(&(nx as u16, ny as u16)).copied().unwrap_or(-1);

            if neighbor_tile < 0 || neighbor_tile == 0xFFFF {
                // No tile / empty → treat as different ground type.
                mask |= 1 << bit_pos;
                continue;
            }

            // Preserve map-authored shoreline tiles: do not force LAT transitions
            // against water neighbors, which otherwise creates harsh beach seams.
            if lookup.is_water(neighbor_tile as u16) {
                continue;
            }

            if !tile_matches_ground(neighbor_tile as u16, ground, lookup) {
                mask |= 1 << bit_pos;
            }
        }

        // Apply transition if any neighbor differs.
        if mask != 0 {
            let lat_idx: u8 = MASK_TO_LAT_INDEX[mask as usize];
            if lat_idx != 0xFF {
                let new_tile: u16 = ground.transition_start_tile + lat_idx as u16;
                cell.tile_index = new_tile as i32;
                cell.sub_tile = 0;
                changes += 1;
            }
        }
    }

    log::info!(
        "LAT: applied {} tile transitions across {} cells",
        changes,
        cells.len(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_to_lat_index() {
        assert_eq!(MASK_TO_LAT_INDEX[0b0000], 0xFF); // All same → no transition.
        assert_eq!(MASK_TO_LAT_INDEX[0b1000], 1); // NE different → index 1.
        assert_eq!(MASK_TO_LAT_INDEX[0b0001], 4); // SW different → index 4.
        assert_eq!(MASK_TO_LAT_INDEX[0b0011], 6); // SE+SW → index 6.
        assert_eq!(MASK_TO_LAT_INDEX[0b1111], 15); // All different → island.
    }

    #[test]
    fn test_parse_lat_config_empty() {
        let ini_data: &[u8] = b"[General]\nKey=Value\n";
        let lookup_data: &[u8] = b"[TileSet0000]\nSetName=Clear\nFileName=clear\nTilesInSet=1\n";
        let lookup = crate::map::theater::parse_tileset_ini(lookup_data, "tem").unwrap();
        let config: LatConfig = parse_lat_config(ini_data, &lookup);
        assert!(config.grounds.is_empty());
    }

    #[test]
    fn test_parse_lat_config_with_ground_types() {
        // Minimal theater INI with tileset definitions + LAT config.
        let ini_text: &str = "\
[General]
RoughTile=2
ClearToRoughLat=3
SandTile=4
ClearToSandLat=5

[TileSet0000]
SetName=Clear
FileName=clear
TilesInSet=10

[TileSet0001]
SetName=Blank
FileName=
TilesInSet=1

[TileSet0002]
SetName=Rough
FileName=rough
TilesInSet=5

[TileSet0003]
SetName=ClearToRough
FileName=crgh
TilesInSet=16

[TileSet0004]
SetName=Sand
FileName=sand
TilesInSet=5

[TileSet0005]
SetName=ClearToSand
FileName=csnd
TilesInSet=16
";
        let lookup = crate::map::theater::parse_tileset_ini(ini_text.as_bytes(), "tem").unwrap();
        let config: LatConfig = parse_lat_config(ini_text.as_bytes(), &lookup);

        assert_eq!(config.grounds.len(), 2); // Rough + Sand (no Pave/Green).
        assert_eq!(config.grounds[0].name, "Rough");
        assert_eq!(config.grounds[0].ground_tileset, 2);
        assert_eq!(config.grounds[0].transition_tileset, 3);
        // TileSet0: 10 tiles, TileSet1: 1 tile, TileSet2: 5 tiles → start of TileSet3 = 16.
        assert_eq!(config.grounds[0].transition_start_tile, 16);

        assert_eq!(config.grounds[1].name, "Sand");
        assert_eq!(config.grounds[1].ground_tileset, 4);
        // TileSet0..3 = 10+1+5+16 = 32 tiles → start of TileSet4 = 32.
        // But Sand uses TileSet5 for transitions → start of TileSet5 = 32+5 = 37.
        assert_eq!(config.grounds[1].transition_start_tile, 37);
    }

    #[test]
    fn test_apply_lat_simple() {
        // Build a minimal scenario: 3x3 grid where center cell is "rough" ground,
        // and some neighbors are "clear" (tileset 0).
        let ini_text: &str = "\
[General]
RoughTile=1
ClearToRoughLat=2

[TileSet0000]
SetName=Clear
FileName=clear
TilesInSet=5

[TileSet0001]
SetName=Rough
FileName=rough
TilesInSet=5

[TileSet0002]
SetName=ClearToRough
FileName=crgh
TilesInSet=16
";
        let lookup = crate::map::theater::parse_tileset_ini(ini_text.as_bytes(), "tem").unwrap();
        let config: LatConfig = parse_lat_config(ini_text.as_bytes(), &lookup);

        // Center cell (5,5) is rough (tile_id=5, tileset 1).
        // Neighbor NE (5,4) is clear (tile_id=0, tileset 0).
        // Neighbor NW (4,5) is rough (tile_id=6, tileset 1).
        // Neighbor SE (6,5) is clear (tile_id=1, tileset 0).
        // Neighbor SW (5,6) is rough (tile_id=7, tileset 1).
        let mut cells: Vec<MapCell> = vec![
            MapCell {
                rx: 5,
                ry: 5,
                tile_index: 5,
                sub_tile: 0,
                z: 0,
            }, // center: rough
            MapCell {
                rx: 5,
                ry: 4,
                tile_index: 0,
                sub_tile: 0,
                z: 0,
            }, // NE: clear
            MapCell {
                rx: 4,
                ry: 5,
                tile_index: 6,
                sub_tile: 0,
                z: 0,
            }, // NW: rough
            MapCell {
                rx: 6,
                ry: 5,
                tile_index: 1,
                sub_tile: 0,
                z: 0,
            }, // SE: clear
            MapCell {
                rx: 5,
                ry: 6,
                tile_index: 7,
                sub_tile: 0,
                z: 0,
            }, // SW: rough
        ];

        apply_lat(&mut cells, &config, &lookup);

        // Center cell should now be a transition tile.
        // NE different (bit 3) + SE different (bit 1) → mask = 0b1010 = 10 → LAT index 3.
        // Transition start = tileset2.start = 5+5 = 10. New tile = 10 + 3 = 13.
        assert_eq!(cells[0].tile_index, 13);
        // Non-ground cells should be unchanged.
        assert_eq!(cells[1].tile_index, 0);
        assert_eq!(cells[3].tile_index, 1);
    }

    #[test]
    fn test_lat_exemption_pairs_parsed() {
        // Verify that MiscPaveTile is added to Pave's connect_to when present in INI.
        let ini_text: &str = "\
[General]
PaveTile=1
ClearToPaveLat=2
MiscPaveTile=3
ShorePieces=4
GreenTile=5
ClearToGreenLat=6
WaterBridge=7

[TileSet0000]
SetName=Clear
FileName=clear
TilesInSet=5

[TileSet0001]
SetName=Pave
FileName=pave
TilesInSet=5

[TileSet0002]
SetName=ClearToPave
FileName=cpav
TilesInSet=16

[TileSet0003]
SetName=MiscPave
FileName=mpav
TilesInSet=5

[TileSet0004]
SetName=Shore
FileName=shore
TilesInSet=5

[TileSet0005]
SetName=Green
FileName=green
TilesInSet=5

[TileSet0006]
SetName=ClearToGreen
FileName=cgrn
TilesInSet=16

[TileSet0007]
SetName=WaterBridge
FileName=wbrd
TilesInSet=5
";
        let lookup = crate::map::theater::parse_tileset_ini(ini_text.as_bytes(), "tem").unwrap();
        let config: LatConfig = parse_lat_config(ini_text.as_bytes(), &lookup);

        // Should have Pave and Green ground types.
        assert_eq!(config.grounds.len(), 2);

        let pave: &LatGroundType = config.grounds.iter().find(|g| g.name == "Pave").unwrap();
        assert!(
            pave.connect_to.contains(&3),
            "MiscPaveTile (3) should be in Pave connect_to"
        );

        let green: &LatGroundType = config.grounds.iter().find(|g| g.name == "Green").unwrap();
        assert!(
            green.connect_to.contains(&4),
            "ShorePieces (4) should be in Green connect_to"
        );
        assert!(
            green.connect_to.contains(&7),
            "WaterBridge (7) should be in Green connect_to"
        );
    }

    #[test]
    fn test_lat_exemption_prevents_transition() {
        // Pave tile next to MiscPaveTile should NOT generate a transition.
        let ini_text: &str = "\
[General]
PaveTile=1
ClearToPaveLat=2
MiscPaveTile=3

[TileSet0000]
SetName=Clear
FileName=clear
TilesInSet=5

[TileSet0001]
SetName=Pave
FileName=pave
TilesInSet=5

[TileSet0002]
SetName=ClearToPave
FileName=cpav
TilesInSet=16

[TileSet0003]
SetName=MiscPave
FileName=mpav
TilesInSet=5
";
        let lookup = crate::map::theater::parse_tileset_ini(ini_text.as_bytes(), "tem").unwrap();
        let config: LatConfig = parse_lat_config(ini_text.as_bytes(), &lookup);

        // Center (5,5) is Pave (tile_id=5, tileset 1).
        // All 4 neighbors are MiscPave (tileset 3, tile_ids 26-29).
        // With exemption, no transition should occur.
        let mut cells: Vec<MapCell> = vec![
            MapCell {
                rx: 5,
                ry: 5,
                tile_index: 5,
                sub_tile: 0,
                z: 0,
            },
            MapCell {
                rx: 5,
                ry: 4,
                tile_index: 26,
                sub_tile: 0,
                z: 0,
            }, // NE: MiscPave
            MapCell {
                rx: 4,
                ry: 5,
                tile_index: 27,
                sub_tile: 0,
                z: 0,
            }, // NW: MiscPave
            MapCell {
                rx: 6,
                ry: 5,
                tile_index: 28,
                sub_tile: 0,
                z: 0,
            }, // SE: MiscPave
            MapCell {
                rx: 5,
                ry: 6,
                tile_index: 29,
                sub_tile: 0,
                z: 0,
            }, // SW: MiscPave
        ];

        apply_lat(&mut cells, &config, &lookup);

        // Center tile should remain unchanged — exemption prevents transition.
        assert_eq!(
            cells[0].tile_index, 5,
            "Pave adjacent to MiscPave should not transition"
        );
    }
}
