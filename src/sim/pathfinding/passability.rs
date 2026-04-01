//! Cell passability matrix — the 13×8 zone layer × terrain type table.
//!
//! Extracted from the original engine (416 bytes = 13 x 8 x 4).
//! The zone flood-fill and pathfinder use this matrix to determine whether
//! a cell's terrain type is passable for a given movement profile.
//!
//! ## How it works
//! Each cell has a **land type** (0-7) stored as a `LandType` column index.
//! Raw TMP `terrain_type` bytes (0-15) are mapped to these 8 columns via
//! `tmp_terrain_to_land_type()` during terrain resolution.
//! Each unit has a **zone layer** derived from its MovementZone/SpeedType.
//! The matrix lookup `PASSABILITY_MATRIX[zone_layer][land_type]` returns:
//! - 1 = passable
//! - 2 = blocked (dynamically, e.g. occupied)
//! - 3 = impassable (always blocked, e.g. rock)
//!
//! ## Dependency rules
//! - Part of sim/ — depends on rules/locomotor_type (SpeedType, MovementZone).
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::rules::locomotor_type::{MovementZone, SpeedType};

/// Passability values from the matrix.
pub const PASS_OK: u8 = 1;
pub const PASS_BLOCKED: u8 = 2;
pub const PASS_IMPASSABLE: u8 = 3;

// ---------------------------------------------------------------------------
// LandType enum — passability matrix column indices
// ---------------------------------------------------------------------------

/// The 8 terrain classification columns used by the passability matrix.
///
/// These are the canonical indices into `PASSABILITY_MATRIX[layer][col]`.
/// Raw TMP `terrain_type` bytes (0-15) must be mapped to these via
/// `tmp_terrain_to_land_type()` before any matrix lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum LandType {
    Clear = 0,
    Road = 1,
    Rough = 2,
    Beach = 3,
    Water = 4,
    Tiberium = 5,
    Railroad = 6,
    Rock = 7,
}

impl LandType {
    /// Convert to the raw column index for passability matrix lookups.
    pub fn as_index(self) -> u8 {
        self as u8
    }
}

/// Map a raw TMP `terrain_type` byte (0-15) to its passability matrix column.
///
/// RA2/YR TMP files encode 16 terrain types inherited from Tiberian Sun.
/// The passability matrix only has 8 columns, so multiple TMP bytes map to
/// the same LandType:
///
/// | TMP byte | Name      | LandType  |
/// |----------|-----------|-----------|
/// | 0-4, 13  | Clear/Ice | Clear (0) |
/// | 5        | Tunnel    | Railroad (6) |
/// | 6        | Railroad  | Railroad (6) |
/// | 7-8      | Rock      | Rock (7)  |
/// | 9        | Water     | Water (4) |
/// | 10       | Beach     | Beach (3) |
/// | 11-12    | Road      | Road (1)  |
/// | 14       | Rough     | Rough (2) |
/// | 15       | Cliff     | Rock (7)  |
pub fn tmp_terrain_to_land_type(tmp_terrain_type: u8) -> LandType {
    match tmp_terrain_type {
        0..=4 | 13 => LandType::Clear,
        5 | 6 => LandType::Railroad,
        7 | 8 => LandType::Rock,
        9 => LandType::Water,
        10 => LandType::Beach,
        11 | 12 => LandType::Road,
        14 => LandType::Rough,
        15 => LandType::Rock,
        // Unknown TMP bytes default to Clear (passable by all ground units).
        _ => LandType::Clear,
    }
}

/// Number of zone layers (rows) in the matrix.
pub const ZONE_LAYER_COUNT: usize = 13;

/// Number of terrain types (columns) in the matrix.
pub const TERRAIN_TYPE_COUNT: usize = 8;

/// The 13x8 passability matrix, adapted from the original engine (0x82A594).
///
/// Rows = MovementZone index (0-12). Columns = our LandType enum (0-7).
/// Values: 1 = passable, 2 = blocked, 3 = impassable (sentinel).
///
/// The original engine uses 8 "ZoneType" columns assigned by RecalcZoneType
/// (0x483C80). Our LandType columns don't map 1:1 to those ZoneTypes, so the
/// matrix values are remapped to produce identical passability decisions:
///
///   Our col → Original ZoneType used for values
///   0 Clear    → 0 Ground     (binary col 0)
///   1 Road     → 1 Road       (binary col 1 — for road-overlay cells)
///   2 Rough    → 0 Ground     (binary col 0 — Rough terrain = Ground in original)
///   3 Beach    → 3 Beach      (binary col 3)
///   4 Water    → 4 Water      (binary col 4)
///   5 Tiberium → 6 Impassable (binary col 6 — tib overlay = Impassable)
///   6 Railroad → 0 Ground     (binary col 0 — Railroad terrain = Ground in original)
///   7 Rock     → 6 Impassable (binary col 6 — Rock terrain = Impassable)
pub static PASSABILITY_MATRIX: [[u8; TERRAIN_TYPE_COUNT]; ZONE_LAYER_COUNT] = [
    //                               Clr Rd  Rgh Bch Wtr Tib RR  Rck
    // Row  0 Normal:
    [1, 2, 1, 2, 2, 2, 1, 2],
    // Row  1 Crusher:
    [1, 1, 1, 2, 2, 2, 1, 2],
    // Row  2 Destroyer:
    [1, 1, 1, 2, 2, 2, 1, 2],
    // Row  3 AmphibiousDestroyer:
    [1, 1, 1, 1, 1, 2, 1, 2],
    // Row  4 AmphibiousCrusher:
    [1, 1, 1, 1, 1, 2, 1, 2],
    // Row  5 Amphibious:
    [1, 2, 1, 1, 1, 2, 1, 2],
    // Row  6 Subterranean (can dig through rock and tiberium):
    [1, 1, 1, 2, 2, 1, 1, 1],
    // Row  7 Infantry:
    [1, 2, 1, 2, 2, 2, 1, 2],
    // Row  8 InfantryDestroyer:
    [1, 1, 1, 2, 2, 2, 1, 2],
    // Row  9 Fly (everything passable):
    [1, 1, 1, 1, 1, 1, 1, 1],
    // Row 10 Water:
    [2, 2, 2, 2, 1, 2, 2, 2],
    // Row 11 WaterBeach:
    [2, 2, 2, 1, 1, 2, 2, 2],
    // Row 12 CrusherAll:
    [1, 1, 1, 2, 2, 2, 1, 2],
];

/// Map a SpeedType to its zone layer index (row in the passability matrix).
///
/// Multiple SpeedTypes may share a layer. The mapping matches the original
/// engine's behavior.
pub fn zone_layer_for_speed_type(speed_type: SpeedType) -> usize {
    match speed_type {
        SpeedType::Foot => 2,       // clear + road + rough
        SpeedType::Track => 2,      // clear + road + rough
        SpeedType::Wheel => 1,      // clear + road only
        SpeedType::Float => 9,      // everything except rock (hover)
        SpeedType::FloatBeach => 4, // clear + road + beach + water
        SpeedType::Hover => 9,      // everything except rock
        SpeedType::Amphibious => 3, // land + water + beach + tiberium
        SpeedType::Winged => 9,     // everything except rock (fly)
    }
}

/// Map a MovementZone to its zone layer index (row in the passability matrix).
///
/// In the original engine, MovementZone IS the direct row index — each of the
/// 13 zones has its own unique passability profile.
pub fn zone_layer_for_movement_zone(mz: MovementZone) -> usize {
    mz as usize
}

/// Check if a terrain land type is passable for a given SpeedType.
///
/// Returns true if the matrix entry is PASS_OK (1), false for PASS_BLOCKED (2)
/// or PASS_IMPASSABLE (3).
pub fn is_passable_for_speed_type(land_type: u8, speed_type: SpeedType) -> bool {
    if land_type as usize >= TERRAIN_TYPE_COUNT {
        return false; // Out of range = impassable
    }
    let layer = zone_layer_for_speed_type(speed_type);
    PASSABILITY_MATRIX[layer][land_type as usize] == PASS_OK
}

/// Check if a terrain land type is passable for a given MovementZone.
///
/// Used by the zone flood-fill to partition the map into connectivity regions.
pub fn is_passable_for_zone(land_type: u8, mz: MovementZone) -> bool {
    if land_type as usize >= TERRAIN_TYPE_COUNT {
        return false;
    }
    let layer = zone_layer_for_movement_zone(mz);
    PASSABILITY_MATRIX[layer][land_type as usize] == PASS_OK
}

/// Get the raw passability value (1/2/3) for a zone layer and terrain type.
///
/// Returns PASS_IMPASSABLE for out-of-bounds inputs.
pub fn passability_value(zone_layer: usize, land_type: u8) -> u8 {
    if zone_layer >= ZONE_LAYER_COUNT || land_type as usize >= TERRAIN_TYPE_COUNT {
        return PASS_IMPASSABLE;
    }
    PASSABILITY_MATRIX[zone_layer][land_type as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_passable_for_all_ground() {
        // Terrain type 0 (Clear) should be passable for all non-water zone layers.
        for layer in 0..10 {
            assert_eq!(
                PASSABILITY_MATRIX[layer][0], PASS_OK,
                "Zone layer {} should pass on Clear terrain",
                layer
            );
        }
    }

    #[test]
    fn rock_blocked_except_subterranean_and_fly() {
        // Rock terrain maps to Impassable ZoneType in the original engine.
        // Subterranean (row 6) and Fly (row 9) can enter; all others blocked.
        for layer in 0..ZONE_LAYER_COUNT {
            let expected = if layer == 6 || layer == 9 {
                PASS_OK
            } else {
                PASS_BLOCKED
            };
            assert_eq!(
                PASSABILITY_MATRIX[layer][7], expected,
                "Zone layer {} on Rock terrain",
                layer
            );
        }
    }

    #[test]
    fn water_only_for_ships() {
        // Zone 10 (ships) should only pass on water (col 4).
        let row = PASSABILITY_MATRIX[10];
        assert_eq!(row[4], PASS_OK);
        assert_eq!(row[0], PASS_BLOCKED); // clear = blocked for ships
        assert_eq!(row[1], PASS_BLOCKED); // road = blocked
    }

    #[test]
    fn amphibious_destroyer_passes_land_and_water() {
        // Zone 3 (AmphibiousDestroyer) passes clear, road, rough, beach, water.
        // Tiberium is Impassable in original engine — blocked for AmphibiousDestroyer.
        let row = PASSABILITY_MATRIX[3];
        assert_eq!(row[0], PASS_OK); // clear
        assert_eq!(row[1], PASS_OK); // road
        assert_eq!(row[2], PASS_OK); // rough
        assert_eq!(row[3], PASS_OK); // beach
        assert_eq!(row[4], PASS_OK); // water
        assert_eq!(row[5], PASS_BLOCKED); // tiberium = impassable zone type
        assert_eq!(row[6], PASS_OK); // railroad = ground terrain
    }

    #[test]
    fn wheel_restricted() {
        // Zone 1 (Crusher/wheel) passes clear, road, rough, railroad — all Ground-type.
        let row = PASSABILITY_MATRIX[1];
        assert_eq!(row[0], PASS_OK); // clear
        assert_eq!(row[1], PASS_OK); // road
        assert_eq!(row[2], PASS_OK); // rough = ground
    }

    #[test]
    fn speed_type_foot_uses_zone_2() {
        assert_eq!(zone_layer_for_speed_type(SpeedType::Foot), 2);
        assert!(is_passable_for_speed_type(0, SpeedType::Foot)); // clear
        assert!(is_passable_for_speed_type(2, SpeedType::Foot)); // rough
        assert!(!is_passable_for_speed_type(4, SpeedType::Foot)); // water
    }

    #[test]
    fn speed_type_float_uses_zone_9() {
        assert_eq!(zone_layer_for_speed_type(SpeedType::Float), 9);
        assert!(is_passable_for_speed_type(0, SpeedType::Float)); // clear
        assert!(is_passable_for_speed_type(4, SpeedType::Float)); // water
        // Rock maps to Impassable ZoneType — Fly/hover CAN enter (row 9 col 6 = 1).
        assert!(is_passable_for_speed_type(7, SpeedType::Float)); // rock passable for hover
    }

    #[test]
    fn movement_zone_water_is_zone_10() {
        assert_eq!(zone_layer_for_movement_zone(MovementZone::Water), 10);
        assert!(!is_passable_for_zone(0, MovementZone::Water)); // clear blocked
        assert!(is_passable_for_zone(4, MovementZone::Water)); // water OK
    }

    #[test]
    fn movement_zone_is_direct_index() {
        // MovementZone IS the passability matrix row index
        assert_eq!(zone_layer_for_movement_zone(MovementZone::Normal), 0);
        assert_eq!(zone_layer_for_movement_zone(MovementZone::Crusher), 1);
        assert_eq!(zone_layer_for_movement_zone(MovementZone::Destroyer), 2);
        assert_eq!(
            zone_layer_for_movement_zone(MovementZone::AmphibiousDestroyer),
            3
        );
        assert_eq!(
            zone_layer_for_movement_zone(MovementZone::AmphibiousCrusher),
            4
        );
        assert_eq!(zone_layer_for_movement_zone(MovementZone::Amphibious), 5);
        assert_eq!(zone_layer_for_movement_zone(MovementZone::Subterranean), 6);
        assert_eq!(zone_layer_for_movement_zone(MovementZone::Infantry), 7);
        assert_eq!(
            zone_layer_for_movement_zone(MovementZone::InfantryDestroyer),
            8
        );
        assert_eq!(zone_layer_for_movement_zone(MovementZone::Fly), 9);
        assert_eq!(zone_layer_for_movement_zone(MovementZone::CrusherAll), 12);
    }

    #[test]
    fn out_of_range_land_type_impassable() {
        assert!(!is_passable_for_speed_type(8, SpeedType::Foot));
        assert!(!is_passable_for_speed_type(255, SpeedType::Float));
    }

    // -- LandType mapping tests --

    #[test]
    fn tmp_clear_variants_map_to_clear() {
        for byte in [0, 1, 2, 3, 4, 13] {
            assert_eq!(
                tmp_terrain_to_land_type(byte),
                LandType::Clear,
                "TMP byte {}",
                byte
            );
        }
    }

    #[test]
    fn tmp_water_maps_to_water() {
        assert_eq!(tmp_terrain_to_land_type(9), LandType::Water);
    }

    #[test]
    fn tmp_beach_maps_to_beach() {
        assert_eq!(tmp_terrain_to_land_type(10), LandType::Beach);
    }

    #[test]
    fn tmp_road_variants_map_to_road() {
        assert_eq!(tmp_terrain_to_land_type(11), LandType::Road);
        assert_eq!(tmp_terrain_to_land_type(12), LandType::Road);
    }

    #[test]
    fn tmp_rough_maps_to_rough() {
        assert_eq!(tmp_terrain_to_land_type(14), LandType::Rough);
    }

    #[test]
    fn tmp_rock_and_cliff_map_to_rock() {
        assert_eq!(tmp_terrain_to_land_type(7), LandType::Rock);
        assert_eq!(tmp_terrain_to_land_type(8), LandType::Rock);
        assert_eq!(tmp_terrain_to_land_type(15), LandType::Rock);
    }

    #[test]
    fn tmp_tunnel_and_railroad_map_to_railroad() {
        assert_eq!(tmp_terrain_to_land_type(5), LandType::Railroad);
        assert_eq!(tmp_terrain_to_land_type(6), LandType::Railroad);
    }

    #[test]
    fn tmp_unknown_bytes_default_to_clear() {
        for byte in 16..=255u8 {
            assert_eq!(
                tmp_terrain_to_land_type(byte),
                LandType::Clear,
                "TMP byte {}",
                byte
            );
        }
    }

    #[test]
    fn land_type_as_index_matches_repr() {
        assert_eq!(LandType::Clear.as_index(), 0);
        assert_eq!(LandType::Road.as_index(), 1);
        assert_eq!(LandType::Rough.as_index(), 2);
        assert_eq!(LandType::Beach.as_index(), 3);
        assert_eq!(LandType::Water.as_index(), 4);
        assert_eq!(LandType::Tiberium.as_index(), 5);
        assert_eq!(LandType::Railroad.as_index(), 6);
        assert_eq!(LandType::Rock.as_index(), 7);
    }

    #[test]
    fn mapped_land_types_work_with_passability_matrix() {
        // Water cells (TMP byte 9 → LandType::Water = 4) should be passable for ships.
        let water = tmp_terrain_to_land_type(9);
        assert!(is_passable_for_speed_type(
            water.as_index(),
            SpeedType::Float
        ));
        assert!(!is_passable_for_speed_type(
            water.as_index(),
            SpeedType::Track
        ));

        // Road cells (TMP byte 11 → LandType::Road = 1) should be passable for wheels.
        let road = tmp_terrain_to_land_type(11);
        assert!(is_passable_for_speed_type(
            road.as_index(),
            SpeedType::Wheel
        ));

        // Beach cells (TMP byte 10 → LandType::Beach = 3) should be passable for amphibious.
        let beach = tmp_terrain_to_land_type(10);
        assert!(is_passable_for_speed_type(
            beach.as_index(),
            SpeedType::Amphibious
        ));
        assert!(!is_passable_for_speed_type(
            beach.as_index(),
            SpeedType::Track
        ));

        // Rock (TMP byte 7 → LandType::Rock = 7) maps to Impassable ZoneType.
        // Hover/Fly (row 9) CAN enter, but ground units cannot.
        let rock = tmp_terrain_to_land_type(7);
        assert!(is_passable_for_speed_type(
            rock.as_index(),
            SpeedType::Float // Float → row 9 (hover) → passable on Impassable terrain
        ));
        assert!(!is_passable_for_speed_type(
            rock.as_index(),
            SpeedType::Track // Track → row 2 → blocked on Impassable terrain
        ));
    }
}
