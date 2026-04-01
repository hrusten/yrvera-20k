//! Terrain land-type semantics parsed from rules.ini / rulesmd.ini.
//!
//! This module maps raw TMP `LandType` bytes onto a small verified subset of
//! RA2/YR terrain semantics and, when available, parses the corresponding
//! land-type sections from rules data for buildability and per-SpeedType costs.

use std::collections::HashMap;

use crate::rules::ini_parser::{IniFile, IniSection};
use crate::rules::locomotor_type::SpeedType;
use crate::util::fixed_math::{SIM_HALF, SIM_ONE, SimFixed};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TerrainClass {
    Clear,
    Rough,
    Road,
    Water,
    Rock,
    Cliff,
    Beach,
    Ice,
    Tiberium,
    Weeds,
    Wall,
    Railroad,
    Tunnel,
    Unknown,
}

impl Default for TerrainClass {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpeedCostProfile {
    pub foot: Option<u8>,
    pub track: Option<u8>,
    pub wheel: Option<u8>,
    pub float: Option<u8>,
    pub amphibious: Option<u8>,
    pub float_beach: Option<u8>,
    pub hover: Option<u8>,
}

impl SpeedCostProfile {
    pub fn cost_for_speed_type(&self, speed_type: SpeedType) -> Option<u8> {
        match speed_type {
            SpeedType::Foot => self.foot,
            SpeedType::Track => self.track,
            SpeedType::Wheel => self.wheel,
            SpeedType::Float => self.float,
            SpeedType::Amphibious => self.amphibious,
            SpeedType::FloatBeach => self.float_beach.or(self.float),
            SpeedType::Hover => self.hover,
            SpeedType::Winged => Some(100),
        }
    }

    /// Runtime speed multiplier for a given SpeedType.
    ///
    /// Converts the INI percentage (0–100+) to a SimFixed fraction (0.0–1.0).
    /// Matches original engine behavior: 0% is boosted to 50% (never fully
    /// immobile on passable terrain), values >100% are clamped to 1.0, and
    /// missing INI data defaults to full speed.
    pub fn speed_multiplier_for(&self, speed_type: SpeedType) -> SimFixed {
        match self.cost_for_speed_type(speed_type) {
            Some(0) => SIM_HALF,
            Some(pct) => {
                let clamped = pct.min(100);
                SimFixed::from_num(clamped) / SimFixed::from_num(100u8)
            }
            None => SIM_ONE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LandTypeSemantics {
    pub section_name: &'static str,
    pub terrain_class: TerrainClass,
    pub buildable: bool,
    pub ground_blocked: bool,
    pub rough: bool,
    pub road: bool,
    pub water: bool,
    pub cliff_like: bool,
    pub speed_costs: SpeedCostProfile,
}

impl LandTypeSemantics {
    pub fn cost_for_speed_type(&self, speed_type: SpeedType) -> Option<u8> {
        self.speed_costs.cost_for_speed_type(speed_type)
    }
}

#[derive(Debug, Clone, Default)]
pub struct TerrainRules {
    by_land_type: HashMap<u8, LandTypeSemantics>,
    by_name: HashMap<String, LandTypeSemantics>,
}

impl TerrainRules {
    pub fn from_ini(ini: &IniFile) -> Self {
        let mut by_land_type: HashMap<u8, LandTypeSemantics> = HashMap::new();
        let mut by_name: HashMap<String, LandTypeSemantics> = HashMap::new();

        for &(land_type, section_name) in KNOWN_LAND_TYPES {
            let semantics = build_semantics(section_name, ini.section(section_name));
            by_land_type.insert(land_type, semantics);
            by_name.insert(section_name.to_ascii_lowercase(), semantics);
        }

        // Overlay-derived terrain types (Tiberium, Weeds) don't come from TMP bytes
        // but are applied when ore/gem/weed overlays change a cell's land type.
        // Parse their INI sections so speed costs are available for terrain cost grids.
        for section_name in ["Tiberium", "Weeds"] {
            if !by_name.contains_key(&section_name.to_ascii_lowercase()) {
                let semantics = build_semantics(section_name, ini.section(section_name));
                by_name.insert(section_name.to_ascii_lowercase(), semantics);
            }
        }

        Self {
            by_land_type,
            by_name,
        }
    }

    pub fn semantics_for_land_type(&self, land_type: u8) -> Option<&LandTypeSemantics> {
        self.by_land_type.get(&land_type)
    }

    pub fn semantics_by_name(&self, name: &str) -> Option<&LandTypeSemantics> {
        self.by_name.get(&name.to_ascii_lowercase())
    }
}

/// Maps every TMP `terrain_type` byte (0-15) to a rules.ini terrain section name.
///
/// RA2/YR inherits the TS TMP byte layout. Bytes 2-4 are TS ice variants
/// (Clear-equivalent in RA2). Byte 5 = Tunnel, byte 6 = Railroad, byte 10 = Beach.
/// All 16 bytes are mapped so no "Unknown TMP LandType" warnings are emitted.
const KNOWN_LAND_TYPES: &[(u8, &str)] = &[
    (0, "Clear"),
    (1, "Clear"),
    (2, "Clear"), // TS Ice1 — Clear-equivalent in RA2
    (3, "Clear"), // TS Ice2 — Clear-equivalent in RA2
    (4, "Clear"), // TS Ice3 — Clear-equivalent in RA2
    (5, "Tunnel"),
    (6, "Railroad"),
    (7, "Rock"),
    (8, "Rock"),
    (9, "Water"),
    (10, "Beach"),
    (11, "Road"),
    (12, "Road"),
    (13, "Clear"),
    (14, "Rough"),
    (15, "Cliff"),
];

fn build_semantics(section_name: &'static str, section: Option<&IniSection>) -> LandTypeSemantics {
    let mut semantics = built_in_semantics(section_name);
    let Some(section) = section else {
        return semantics;
    };

    semantics.buildable = section.get_bool("Buildable").unwrap_or(semantics.buildable);
    semantics.speed_costs = parse_speed_costs(section);
    semantics
}

fn built_in_semantics(section_name: &'static str) -> LandTypeSemantics {
    match section_name {
        "Clear" => LandTypeSemantics {
            section_name,
            terrain_class: TerrainClass::Clear,
            buildable: true,
            ground_blocked: false,
            rough: false,
            road: false,
            water: false,
            cliff_like: false,
            speed_costs: SpeedCostProfile::default(),
        },
        "Rough" => LandTypeSemantics {
            section_name,
            terrain_class: TerrainClass::Rough,
            buildable: true,
            ground_blocked: false,
            rough: true,
            road: false,
            water: false,
            cliff_like: false,
            speed_costs: SpeedCostProfile::default(),
        },
        "Road" => LandTypeSemantics {
            section_name,
            terrain_class: TerrainClass::Road,
            buildable: true,
            ground_blocked: false,
            rough: false,
            road: true,
            water: false,
            cliff_like: false,
            speed_costs: SpeedCostProfile::default(),
        },
        "Water" => LandTypeSemantics {
            section_name,
            terrain_class: TerrainClass::Water,
            buildable: false,
            ground_blocked: true,
            rough: false,
            road: false,
            water: true,
            cliff_like: false,
            speed_costs: SpeedCostProfile::default(),
        },
        "Rock" => LandTypeSemantics {
            section_name,
            terrain_class: TerrainClass::Rock,
            buildable: false,
            ground_blocked: true,
            rough: false,
            road: false,
            water: false,
            cliff_like: true,
            speed_costs: SpeedCostProfile::default(),
        },
        "Cliff" => LandTypeSemantics {
            section_name,
            terrain_class: TerrainClass::Cliff,
            buildable: false,
            ground_blocked: true,
            rough: false,
            road: false,
            water: false,
            cliff_like: true,
            speed_costs: SpeedCostProfile::default(),
        },
        "Beach" => LandTypeSemantics {
            section_name,
            terrain_class: TerrainClass::Beach,
            buildable: false,
            ground_blocked: false,
            rough: false,
            road: false,
            water: false,
            cliff_like: false,
            speed_costs: SpeedCostProfile::default(),
        },
        "Ice" => LandTypeSemantics {
            section_name,
            terrain_class: TerrainClass::Ice,
            buildable: false,
            ground_blocked: false,
            rough: false,
            road: false,
            water: false,
            cliff_like: false,
            speed_costs: SpeedCostProfile::default(),
        },
        "Tiberium" => LandTypeSemantics {
            section_name,
            terrain_class: TerrainClass::Tiberium,
            buildable: false,
            ground_blocked: false,
            rough: false,
            road: false,
            water: false,
            cliff_like: false,
            speed_costs: SpeedCostProfile::default(),
        },
        "Weeds" => LandTypeSemantics {
            section_name,
            terrain_class: TerrainClass::Weeds,
            buildable: false,
            ground_blocked: false,
            rough: false,
            road: false,
            water: false,
            cliff_like: false,
            speed_costs: SpeedCostProfile::default(),
        },
        "Wall" => LandTypeSemantics {
            section_name,
            terrain_class: TerrainClass::Wall,
            buildable: false,
            ground_blocked: true,
            rough: false,
            road: false,
            water: false,
            cliff_like: false,
            speed_costs: SpeedCostProfile::default(),
        },
        "Railroad" => LandTypeSemantics {
            section_name,
            terrain_class: TerrainClass::Railroad,
            buildable: false,
            ground_blocked: false,
            rough: false,
            road: false,
            water: false,
            cliff_like: false,
            speed_costs: SpeedCostProfile::default(),
        },
        "Tunnel" => LandTypeSemantics {
            section_name,
            terrain_class: TerrainClass::Tunnel,
            buildable: false,
            ground_blocked: false,
            rough: false,
            road: false,
            water: false,
            cliff_like: false,
            speed_costs: SpeedCostProfile::default(),
        },
        _ => LandTypeSemantics {
            section_name,
            terrain_class: TerrainClass::Unknown,
            buildable: false,
            ground_blocked: true,
            rough: false,
            road: false,
            water: false,
            cliff_like: true,
            speed_costs: SpeedCostProfile::default(),
        },
    }
}

fn parse_speed_costs(section: &IniSection) -> SpeedCostProfile {
    let mut costs = SpeedCostProfile {
        foot: parse_cost(section, "Foot"),
        track: parse_cost(section, "Track"),
        wheel: parse_cost(section, "Wheel"),
        float: parse_cost(section, "Float"),
        amphibious: parse_cost(section, "Amphibious"),
        float_beach: parse_cost(section, "FloatBeach"),
        hover: parse_cost(section, "Hover"),
    };

    if costs.track == Some(0) {
        costs.foot = Some(0);
    }

    costs
}

fn parse_cost(section: &IniSection, key: &str) -> Option<u8> {
    let raw = section.get(key)?.trim().trim_end_matches('%').trim();
    let value = raw.parse::<i32>().ok()?;
    Some(value.clamp(0, 255) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terrain_rules_parse_known_sections_and_buildability() {
        let ini = IniFile::from_str(
            "[Clear]\nBuildable=yes\nFoot=100%\nTrack=100%\nWheel=100%\n\
             [Rough]\nBuildable=yes\nFoot=90%\nTrack=75%\nWheel=60%\n\
             [Water]\nBuildable=no\nFoot=0%\nTrack=0%\nFloat=100%\nHover=100%\n\
             [Cliff]\nBuildable=no\nFoot=0%\nTrack=0%\n",
        );
        let terrain_rules = TerrainRules::from_ini(&ini);

        let clear = terrain_rules
            .semantics_for_land_type(0)
            .expect("clear semantics");
        assert_eq!(clear.terrain_class, TerrainClass::Clear);
        assert!(clear.buildable);
        assert_eq!(clear.cost_for_speed_type(SpeedType::Track), Some(100));

        let rough = terrain_rules
            .semantics_for_land_type(14)
            .expect("rough semantics");
        assert!(rough.rough);
        assert_eq!(rough.cost_for_speed_type(SpeedType::Wheel), Some(60));

        let water = terrain_rules
            .semantics_for_land_type(9)
            .expect("water semantics");
        assert!(water.water);
        assert!(!water.buildable);
        assert_eq!(water.cost_for_speed_type(SpeedType::Float), Some(100));
        assert_eq!(water.cost_for_speed_type(SpeedType::Track), Some(0));

        let cliff = terrain_rules
            .semantics_for_land_type(15)
            .expect("cliff semantics");
        assert!(cliff.cliff_like);
        assert_eq!(cliff.cost_for_speed_type(SpeedType::Foot), Some(0));
    }

    #[test]
    fn terrain_rules_keep_verified_fallbacks_when_section_is_missing() {
        let terrain_rules = TerrainRules::from_ini(&IniFile::from_str(""));

        let road = terrain_rules
            .semantics_for_land_type(11)
            .expect("road semantics");
        assert_eq!(road.terrain_class, TerrainClass::Road);
        assert!(road.road);
        assert!(road.buildable);
        assert_eq!(road.cost_for_speed_type(SpeedType::Track), None);

        // Byte 10 is now mapped to Beach.
        let beach = terrain_rules
            .semantics_for_land_type(10)
            .expect("beach semantics");
        assert_eq!(beach.terrain_class, TerrainClass::Beach);
        assert!(!beach.buildable);
    }

    #[test]
    fn all_16_land_type_bytes_resolve() {
        let terrain_rules = TerrainRules::from_ini(&IniFile::from_str(""));
        for byte in 0u8..=15 {
            assert!(
                terrain_rules.semantics_for_land_type(byte).is_some(),
                "LandType byte {} should have semantics",
                byte,
            );
        }
    }

    #[test]
    fn ts_ice_bytes_are_clear_equivalent() {
        let terrain_rules = TerrainRules::from_ini(&IniFile::from_str(""));
        let clear = terrain_rules.semantics_for_land_type(0).expect("clear");
        for byte in [2, 3, 4] {
            let ice_clear = terrain_rules
                .semantics_for_land_type(byte)
                .expect("ts ice byte");
            assert_eq!(ice_clear.terrain_class, clear.terrain_class);
            assert_eq!(ice_clear.buildable, clear.buildable);
            assert_eq!(ice_clear.ground_blocked, clear.ground_blocked);
        }
    }

    #[test]
    fn beach_costs_from_ini() {
        let ini = IniFile::from_str(
            "[Beach]\nFoot=0%\nTrack=0%\nWheel=0%\nFloat=0%\n\
             FloatBeach=100%\nHover=75%\nAmphibious=60%\nBuildable=no\n",
        );
        let terrain_rules = TerrainRules::from_ini(&ini);
        let beach = terrain_rules
            .semantics_for_land_type(10)
            .expect("beach semantics");
        assert_eq!(beach.terrain_class, TerrainClass::Beach);
        assert!(!beach.buildable);
        assert_eq!(beach.cost_for_speed_type(SpeedType::Foot), Some(0));
        assert_eq!(beach.cost_for_speed_type(SpeedType::Track), Some(0));
        assert_eq!(beach.cost_for_speed_type(SpeedType::FloatBeach), Some(100));
        assert_eq!(beach.cost_for_speed_type(SpeedType::Hover), Some(75));
        assert_eq!(beach.cost_for_speed_type(SpeedType::Amphibious), Some(60));
    }

    #[test]
    fn tunnel_and_railroad_bytes_map_correctly() {
        let ini = IniFile::from_str(
            "[Tunnel]\nFoot=100%\nTrack=100%\nBuildable=no\n\
             [Railroad]\nFoot=90%\nTrack=100%\nBuildable=no\n",
        );
        let terrain_rules = TerrainRules::from_ini(&ini);
        let tunnel = terrain_rules
            .semantics_for_land_type(5)
            .expect("tunnel semantics");
        assert_eq!(tunnel.terrain_class, TerrainClass::Tunnel);
        assert_eq!(tunnel.cost_for_speed_type(SpeedType::Foot), Some(100));

        let railroad = terrain_rules
            .semantics_for_land_type(6)
            .expect("railroad semantics");
        assert_eq!(railroad.terrain_class, TerrainClass::Railroad);
        assert_eq!(railroad.cost_for_speed_type(SpeedType::Foot), Some(90));
    }

    #[test]
    fn terrain_rules_force_foot_block_when_track_is_zero() {
        let ini = IniFile::from_str("[Rock]\nTrack=0%\nFoot=50%\n");
        let terrain_rules = TerrainRules::from_ini(&ini);
        let rock = terrain_rules
            .semantics_for_land_type(7)
            .expect("rock semantics");
        assert_eq!(rock.cost_for_speed_type(SpeedType::Track), Some(0));
        assert_eq!(rock.cost_for_speed_type(SpeedType::Foot), Some(0));
    }
}
