//! Map waypoint parsing.
//!
//! Waypoints are numbered cell anchors used by mission logic, spawns, AI teams,
//! and other map-authored behavior. Parsing them now gives later trigger/team
//! work a stable source of truth.

use std::collections::HashMap;

use crate::rules::ini_parser::IniFile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Waypoint {
    pub index: u32,
    pub rx: u16,
    pub ry: u16,
}

/// Standard multiplayer/skirmish start waypoints in RA2/YR.
pub const MULTIPLAYER_START_WAYPOINTS: std::ops::RangeInclusive<u32> = 0..=7;

/// Parse `[Waypoints]` into a waypoint index -> cell mapping.
///
/// RA2/YR maps typically use `NewINIFormat=5`, which packs coordinates as
/// `ry * 1000 + rx`. Older formats use `ry * 128 + rx`.
pub fn parse_waypoints(ini: &IniFile) -> HashMap<u32, Waypoint> {
    let Some(section) = ini.section("Waypoints") else {
        return HashMap::new();
    };

    let coord_factor: u32 = waypoint_coord_factor(ini);
    let mut waypoints: HashMap<u32, Waypoint> = HashMap::new();
    for key in section.keys() {
        let Ok(index) = key.parse::<u32>() else {
            continue;
        };
        let Some(raw_value) = section.get(key) else {
            continue;
        };
        let Ok(coords) = raw_value.trim().parse::<u32>() else {
            continue;
        };
        let rx = (coords % coord_factor) as u16;
        let ry = (coords / coord_factor) as u16;
        waypoints.insert(index, Waypoint { index, rx, ry });
    }

    if !waypoints.is_empty() {
        log::info!("Parsed {} waypoints from [Waypoints]", waypoints.len());
    }
    waypoints
}

/// Return multiplayer start waypoints (0..=7) sorted by waypoint index.
pub fn multiplayer_start_waypoints(waypoints: &HashMap<u32, Waypoint>) -> Vec<Waypoint> {
    let mut starts: Vec<Waypoint> = waypoints
        .values()
        .copied()
        .filter(|wp| MULTIPLAYER_START_WAYPOINTS.contains(&wp.index))
        .collect();
    starts.sort_by_key(|wp| wp.index);
    starts
}

/// Return the first multiplayer/skirmish start waypoint if present.
pub fn first_multiplayer_start(waypoints: &HashMap<u32, Waypoint>) -> Option<Waypoint> {
    multiplayer_start_waypoints(waypoints).into_iter().next()
}

fn waypoint_coord_factor(ini: &IniFile) -> u32 {
    let new_ini_format = ini
        .section("Basic")
        .and_then(|section| section.get("NewINIFormat"))
        .and_then(|value| value.trim().parse::<u32>().ok())
        .unwrap_or(5);
    if new_ini_format >= 4 { 1000 } else { 128 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_waypoints_ra2_format() {
        let ini = IniFile::from_str("[Basic]\nNewINIFormat=5\n[Waypoints]\n0=140034\n99=55098\n");
        let waypoints = parse_waypoints(&ini);
        assert_eq!(waypoints.len(), 2);
        assert_eq!(
            waypoints.get(&0),
            Some(&Waypoint {
                index: 0,
                rx: 34,
                ry: 140
            })
        );
        assert_eq!(
            waypoints.get(&99),
            Some(&Waypoint {
                index: 99,
                rx: 98,
                ry: 55
            })
        );
    }

    #[test]
    fn test_parse_waypoints_old_format() {
        let ini = IniFile::from_str("[Basic]\nNewINIFormat=3\n[Waypoints]\n7=261\n");
        let waypoints = parse_waypoints(&ini);
        assert_eq!(
            waypoints.get(&7),
            Some(&Waypoint {
                index: 7,
                rx: 5,
                ry: 2
            })
        );
    }

    #[test]
    fn test_missing_waypoints_is_empty() {
        let ini = IniFile::from_str("[Map]\nTheater=TEMPERATE\n");
        assert!(parse_waypoints(&ini).is_empty());
    }

    #[test]
    fn test_multiplayer_start_waypoints_are_sorted_and_filtered() {
        let ini = IniFile::from_str(
            "[Basic]\nNewINIFormat=5\n[Waypoints]\n11=200300\n3=100200\n0=100050\n99=55098\n",
        );
        let waypoints = parse_waypoints(&ini);
        let starts = multiplayer_start_waypoints(&waypoints);
        assert_eq!(starts.len(), 2);
        assert_eq!(starts[0].index, 0);
        assert_eq!(starts[1].index, 3);
        assert_eq!(first_multiplayer_start(&waypoints), Some(starts[0]));
    }
}
