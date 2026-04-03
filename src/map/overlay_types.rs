//! Overlay type registry — maps overlay ID to name and SHP filename.
//!
//! Overlay IDs in [OverlayPack] are indices into the [OverlayTypes] list
//! in rules.ini. Each overlay type has a name (e.g., "GASAND", "GEM01")
//! which is also the SHP filename prefix.
//!
//! For terrain objects from the [Terrain] section, the name IS the SHP prefix
//! (e.g., "INTREE01" → try `intree01.tem`, `intree01.shp`).
//!
//! ## Dependency rules
//! - Part of map/ — depends on rules/ (ini_parser).

use crate::rules::ini_parser::IniFile;
use std::borrow::Cow;
use std::sync::OnceLock;

/// Check if an overlay index is a bridge overlay. The original engine identifies
/// bridges by hardcoded index position in `[OverlayTypes]`, not by INI flags.
/// These indices must not be reordered without breaking bridge logic.
pub fn is_bridge_overlay_index(id: u8) -> bool {
    matches!(
        id,
        24 | 25             // BRIDGE1, BRIDGE2 (high concrete)
        | 237 | 238         // BRIDGEB1, BRIDGEB2 (high wood)
        | 74..=101          // LOBRDG01-28 (low wood)
        | 122..=125         // LOBRDGE1-4 (low wood ends, TS)
        | 205..=232         // LOBRDB01-28 (low urban)
        | 233..=236         // LOBRDGB1-4 (low urban ends)
    )
}

/// Check if a bridge overlay index is a HIGH bridge (elevated, 3-cell-wide).
pub fn is_high_bridge_index(id: u8) -> bool {
    matches!(id, 24 | 25 | 237 | 238)
}

/// Get the bridge direction from a high bridge overlay index.
/// Returns None for low bridges or non-bridge indices.
pub fn high_bridge_direction(id: u8) -> Option<u8> {
    match id {
        24 | 237 => Some(1), // Direction 1 (EW / NE-SW)
        25 | 238 => Some(2), // Direction 2 (NS / NW-SE)
        _ => None,
    }
}

/// Per-overlay-type rendering flags parsed from each type's rules.ini section.
///
/// These flags select the correct palette and Y-offset for rendering.
#[derive(Debug, Clone)]
pub struct OverlayTypeFlags {
    /// Tiberium=yes — rendered with unit palette, gets -12px Y offset.
    pub tiberium: bool,
    /// Wall=yes — rendered with unit palette, gets -12px Y offset.
    /// In RA2, Wall=yes is used for BOTH destructible walls AND road/pavement overlays.
    pub wall: bool,
    /// IsVeins=yes — rendered with unit palette, gets -12px Y offset.
    pub is_veins: bool,
    /// IsVeinholeMonster=yes — rendered with unit palette.
    pub is_veinhole_monster: bool,
    /// Crate=yes — gets -12px Y offset.
    pub crate_type: bool,
    /// Overlay name identifies a bridge deck/high-bridge overlay.
    pub bridge_deck: bool,
    /// Railroad track overlay (TRACKS01..TRACKS16). FA2 renders these +15px lower.
    pub track: bool,
    /// Land= key from INI — terrain classification for this overlay.
    /// "Road" means the overlay acts as a road surface (pavement, concrete).
    pub land: Option<String>,
    /// Strength= from rules.ini — hit points for destructible overlays.
    /// Only meaningful when wall=true. Default 1.
    pub strength: u16,
    /// DamageLevels= from art.ini — number of damage stages for walls.
    /// Only meaningful when wall=true. Default 1.
    pub damage_levels: u16,
}

impl Default for OverlayTypeFlags {
    fn default() -> Self {
        Self {
            tiberium: false,
            wall: false,
            is_veins: false,
            is_veinhole_monster: false,
            crate_type: false,
            bridge_deck: false,
            track: false,
            land: None,
            strength: 1,
            damage_levels: 1,
        }
    }
}

impl OverlayTypeFlags {
    /// Whether this overlay type should use the unit palette instead of theater palette.
    pub fn uses_unit_palette(&self) -> bool {
        self.tiberium || self.wall || self.is_veins || self.is_veinhole_monster
    }

    /// Y pixel offset for rendering.
    /// RA2 CellHeight = 15px (CellSizeY/2 = 30/2 = 15). NOT 12px (that's TS).
    pub fn y_draw_offset(&self) -> f32 {
        if self.tiberium || self.wall || self.is_veins || self.crate_type {
            -15.0
        } else {
            0.0
        }
    }
}

/// Registry of overlay type names indexed by overlay ID.
///
/// Built from the [OverlayTypes] section of rules.ini.
/// Overlay IDs 0..N map to the names listed in order.
pub struct OverlayTypeRegistry {
    /// Overlay ID -> name (indexed by preserved internal [OverlayTypes] IDs).
    names: Vec<String>,
    /// Per-type rendering flags (same indexing as names).
    flags: Vec<OverlayTypeFlags>,
}

impl OverlayTypeRegistry {
    /// Parse [OverlayTypes] from rules.ini into an indexed registry.
    ///
    /// The section lists types with numeric keys: `0=GASAND\n1=GEM01\n...`.
    /// RA2/YR may skip some numeric keys, but internal overlay ids still follow
    /// the ordered list rather than reserving holes for every missing raw key.
    /// Returns an empty registry if the section is missing.
    pub fn from_ini(ini: &IniFile, art_ini: Option<&IniFile>) -> Self {
        let section = match ini.section("OverlayTypes") {
            Some(s) => s,
            None => {
                log::warn!("[OverlayTypes] section not found in rules.ini");
                return OverlayTypeRegistry {
                    names: Vec::new(),
                    flags: Vec::new(),
                };
            }
        };

        // Collect all (numeric_key, value) pairs from the section.
        let mut pairs: Vec<(usize, String)> = Vec::new();
        for key in section.keys() {
            if let Ok(idx) = key.parse::<usize>() {
                if let Some(val) = section.get(key) {
                    if !val.is_empty() {
                        pairs.push((idx, val.to_string()));
                    }
                }
            }
        }

        if pairs.is_empty() {
            log::warn!("[OverlayTypes] present but empty");
            return OverlayTypeRegistry {
                names: Vec::new(),
                flags: Vec::new(),
            };
        }

        // Sort by numeric key to get the canonical ordering, then build a
        // 0-based sequential list. The numeric keys in [OverlayTypes] are just
        // ordering hints (may start at 0 in rules.ini or 1 in rulesmd.ini).
        // Map overlay IDs from [OverlayPack] are 0-based sequential indices
        // into this ordered list.
        pairs.sort_by_key(|(k, _)| *k);
        pairs.dedup_by(|a, b| a.0 == b.0);

        let mut names: Vec<String> = Vec::with_capacity(pairs.len());
        let mut flags: Vec<OverlayTypeFlags> = Vec::with_capacity(pairs.len());
        for (idx, (_, name)) in pairs.iter().enumerate() {
            names.push(name.clone());
            let upper_name = name.to_ascii_uppercase();
            // Bridge overlays are identified by hardcoded index position in
            // [OverlayTypes], matching the original engine's direct index checks.
            let bridge_deck = is_bridge_overlay_index(idx as u8);
            let track = upper_name.starts_with("TRACKS");
            if let Some(type_section) = ini.section(name) {
                let land = type_section.get("Land").map(|s| s.to_string());
                // Strength from rules section (e.g., [GAWALL] Strength=300).
                let strength = type_section
                    .get("Strength")
                    .and_then(|v| v.parse::<u16>().ok())
                    .unwrap_or(1);
                // DamageLevels from art section (e.g., [GASAND] DamageLevels=2 in art.ini).
                let damage_levels = art_ini
                    .and_then(|art| art.section(name))
                    .and_then(|s| s.get("DamageLevels"))
                    .and_then(|v| v.parse::<u16>().ok())
                    .unwrap_or(1);
                flags.push(OverlayTypeFlags {
                    tiberium: type_section.get_bool("Tiberium").unwrap_or(false),
                    wall: type_section.get_bool("Wall").unwrap_or(false),
                    is_veins: type_section.get_bool("IsVeins").unwrap_or(false),
                    is_veinhole_monster: type_section
                        .get_bool("IsVeinholeMonster")
                        .unwrap_or(false),
                    crate_type: type_section.get_bool("Crate").unwrap_or(false),
                    bridge_deck,
                    track,
                    land,
                    strength,
                    damage_levels,
                });
            } else {
                flags.push(OverlayTypeFlags {
                    bridge_deck,
                    track,
                    ..OverlayTypeFlags::default()
                });
            }
        }
        let max_index: usize = names.len().saturating_sub(1);

        log::info!(
            "OverlayTypeRegistry: {} types loaded (max_id={})",
            pairs.len(),
            max_index,
        );
        OverlayTypeRegistry { names, flags }
    }

    /// Create an empty registry (used as fallback when map loading fails).
    pub fn empty() -> Self {
        OverlayTypeRegistry {
            names: Vec::new(),
            flags: Vec::new(),
        }
    }

    /// Look up the name for an overlay ID. Returns None if out of range.
    pub fn name(&self, overlay_id: u8) -> Option<&str> {
        self.names
            .get(overlay_id as usize)
            .filter(|s| !s.is_empty())
            .map(|s| s.as_str())
    }

    /// Look up the rendering flags for an overlay ID.
    pub fn flags(&self, overlay_id: u8) -> Option<&OverlayTypeFlags> {
        self.flags.get(overlay_id as usize)
    }

    /// Look up flags by overlay name (case-sensitive).
    pub fn flags_by_name(&self, name: &str) -> Option<&OverlayTypeFlags> {
        self.names
            .iter()
            .position(|n| n == name)
            .and_then(|i| self.flags.get(i))
    }

    /// Look up overlay_id by name (case-insensitive). Returns None if not found.
    pub fn id_for_name(&self, name: &str) -> Option<u8> {
        self.names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(name))
            .and_then(|i| u8::try_from(i).ok())
    }

    /// Total number of registered overlay types.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

/// Generate candidate SHP filenames for an overlay name.
///
/// Returns a list of filenames to try in order, using the theater extension first
/// (e.g., for temperate: `.tem`), then the generic `.shp` extension.
/// Both lowercase and original case are tried.
pub fn overlay_shp_candidates(name: &str, theater_ext: &str) -> Vec<String> {
    let lower: String = name.to_lowercase();
    vec![
        format!("{}.{}", lower, theater_ext),
        format!("{}.shp", lower),
        format!("{}.{}", name, theater_ext),
        format!("{}.shp", name),
    ]
}

/// Generate candidate SHP filenames for a terrain object (from [Terrain] section).
///
/// Terrain objects like "INTREE01" may have theater-specific variants.
pub fn terrain_shp_candidates(name: &str, theater_ext: &str) -> Vec<String> {
    overlay_shp_candidates(name, theater_ext)
}

/// Optional debug remap for problematic resource overlays.
///
/// When `RA2_FORCE_TIB3_TO_TIB01=1`, remap `TIB3_20` to `TIB01`.
/// This is a temporary diagnostic switch to isolate rules/mapping issues.
pub fn remap_overlay_name_for_debug<'a>(name: &'a str) -> Cow<'a, str> {
    static FORCE_TIB3_TO_TIB01: OnceLock<bool> = OnceLock::new();
    let enabled: bool = *FORCE_TIB3_TO_TIB01.get_or_init(|| {
        std::env::var("RA2_FORCE_TIB3_TO_TIB01")
            .ok()
            .map(|v| {
                let n = v.trim().to_ascii_lowercase();
                n == "1" || n == "true" || n == "yes" || n == "on"
            })
            .unwrap_or(false)
    });
    if enabled && name.eq_ignore_ascii_case("TIB3_20") {
        Cow::Borrowed("TIB01")
    } else {
        Cow::Borrowed(name)
    }
}

fn is_resource_overlay_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.starts_with("TIB") || upper.starts_with("GEM")
}

fn tiberium_id_offset() -> isize {
    static TIB_ID_OFFSET: OnceLock<isize> = OnceLock::new();
    *TIB_ID_OFFSET.get_or_init(|| {
        std::env::var("RA2_TIB_ID_OFFSET")
            .ok()
            .and_then(|s| s.parse::<isize>().ok())
            .unwrap_or(0)
    })
}

/// Resolve overlay name for rendering/debug display with optional resource-only ID offset.
///
/// `RA2_TIB_ID_OFFSET=N` applies only when the base ID resolves to TIB*/GEM* and
/// the shifted target also resolves to TIB*/GEM*. This avoids shifting bridges/rocks.
pub fn resolve_overlay_name_for_render(
    reg: &OverlayTypeRegistry,
    overlay_id: u8,
) -> Option<String> {
    let base_name = reg.name(overlay_id)?;
    let mut resolved_id: u8 = overlay_id;
    let offset = tiberium_id_offset();
    if offset != 0 && is_resource_overlay_name(base_name) {
        let shifted = overlay_id as isize + offset;
        if (0..=u8::MAX as isize).contains(&shifted) {
            let shifted_id = shifted as u8;
            if let Some(candidate) = reg.name(shifted_id) {
                if is_resource_overlay_name(candidate) {
                    resolved_id = shifted_id;
                }
            }
        }
    }
    reg.name(resolved_id)
        .map(|n| remap_overlay_name_for_debug(n).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_overlay_types() {
        let text: &str = "\
[OverlayTypes]
0=GASAND
1=INTREE01
2=GAWALL
3=GEM01
";
        let ini: IniFile = IniFile::from_str(text);
        let reg: OverlayTypeRegistry = OverlayTypeRegistry::from_ini(&ini, None);
        assert_eq!(reg.len(), 4);
        assert_eq!(reg.name(0), Some("GASAND"));
        assert_eq!(reg.name(1), Some("INTREE01"));
        assert_eq!(reg.name(3), Some("GEM01"));
        assert_eq!(reg.name(255), None);
    }

    #[test]
    fn test_shp_candidates() {
        let names: Vec<String> = overlay_shp_candidates("GEM01", "tem");
        assert_eq!(names[0], "gem01.tem");
        assert_eq!(names[1], "gem01.shp");
        assert_eq!(names[2], "GEM01.tem");
        assert_eq!(names[3], "GEM01.shp");
    }

    #[test]
    fn test_sparse_overlay_types() {
        let text: &str = "\
[OverlayTypes]
0=GASAND
1=GEM01
5=BRIDGE
10=BIGFENCE
";
        let ini: IniFile = IniFile::from_str(text);
        let reg: OverlayTypeRegistry = OverlayTypeRegistry::from_ini(&ini, None);
        // Keys sorted by numeric value, compacted to 0-based sequential indices.
        assert_eq!(reg.len(), 4);
        assert_eq!(reg.name(0), Some("GASAND"));
        assert_eq!(reg.name(1), Some("GEM01"));
        assert_eq!(reg.name(2), Some("BRIDGE"));
        assert_eq!(reg.name(3), Some("BIGFENCE"));
        assert_eq!(reg.name(4), None);
    }

    #[test]
    fn test_empty_registry() {
        let text: &str = "[General]\nKey=Value\n";
        let ini: IniFile = IniFile::from_str(text);
        let reg: OverlayTypeRegistry = OverlayTypeRegistry::from_ini(&ini, None);
        assert!(reg.is_empty());
        assert_eq!(reg.name(0), None);
    }

    #[test]
    fn test_one_based_overlay_types_compacted() {
        let text: &str = "\
[OverlayTypes]
1=GASAND
2=GEM01
";
        let ini: IniFile = IniFile::from_str(text);
        let reg: OverlayTypeRegistry = OverlayTypeRegistry::from_ini(&ini, None);
        // Keys sorted and compacted to 0-based — key numbers are ordering hints only.
        assert_eq!(reg.len(), 2);
        assert_eq!(reg.name(0), Some("GASAND"));
        assert_eq!(reg.name(1), Some("GEM01"));
    }

    #[test]
    fn test_sparse_keys_compacted() {
        let text: &str = "\
[OverlayTypes]
1=GASAND
2=GEM01
6=BRIDGE
11=BIGFENCE
";
        let ini: IniFile = IniFile::from_str(text);
        let reg: OverlayTypeRegistry = OverlayTypeRegistry::from_ini(&ini, None);
        assert_eq!(reg.len(), 4);
        assert_eq!(reg.name(0), Some("GASAND"));
        assert_eq!(reg.name(1), Some("GEM01"));
        assert_eq!(reg.name(2), Some("BRIDGE"));
        assert_eq!(reg.name(3), Some("BIGFENCE"));
    }
}
