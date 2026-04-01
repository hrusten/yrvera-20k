//! Warhead type definitions parsed from rules.ini.
//!
//! Warheads define HOW damage is applied: effectiveness against each armor
//! type (Verses=), splash radius (CellSpread=), and damage falloff
//! (PercentAtMax=). Weapons reference warheads via `Warhead=`.
//!
//! ## rules.ini format
//! ```ini
//! [AP]
//! CellSpread=0
//! PercentAtMax=1
//! Verses=100%,100%,90%,75%,75%,75%,60%,30%,20%,0%,0%
//! ```
//!
//! ## Verses armor order (11 types)
//! none, flak, plate, light, medium, heavy, wood, steel, concrete, special_1, special_2
//!
//! ## Dependency rules
//! - Part of rules/ — no dependencies on sim/, render/, ui/, etc.

use crate::rules::ini_parser::IniSection;
use crate::util::fixed_math::{SIM_ZERO, SimFixed, sim_from_f32};

/// A warhead definition parsed from a rules.ini section.
///
/// Warheads are the final link in the damage chain: weapon -> projectile -> warhead.
/// The Verses field determines damage effectiveness against each armor type.
#[derive(Debug, Clone)]
pub struct WarheadType {
    /// Section name in rules.ini (e.g., "AP", "HE", "SA").
    pub id: String,
    /// Damage percentage per armor type (0–200). Index order:
    /// 0=none, 1=flak, 2=plate, 3=light, 4=medium, 5=heavy,
    /// 6=wood, 7=steel, 8=concrete, 9=special_1, 10=special_2.
    /// Empty if Verses= is absent. 100 = full damage, 0 = immune.
    pub verses: Vec<u8>,
    /// Splash damage radius in cells (SIM_ZERO = direct hit only).
    pub cell_spread: SimFixed,
    /// Damage percentage at maximum spread distance (0–100).
    pub percent_at_max: u8,
    /// Whether this warhead can damage walls/bridges (Wall=yes).
    pub wall: bool,
    /// Explosion animation names indexed by damage magnitude (AnimList= in rules.ini).
    /// The original engine selects by `damage / 25`, clamped to list length.
    /// Example: ["XGRYSML1","EXPLOSML","EXPLOMED","EXPLOLRG","TWLT070"].
    pub anim_list: Vec<String>,
    /// Infantry death animation variant (InfDeath= in rules.ini, 0–10).
    /// Maps to Die1–Die5 infantry sequences. Default 1 (standard rifle death).
    pub inf_death: u8,

    // --- Bool fields (verified offsets from WarheadTypeClass::ReadINI) ---
    /// Conventional warhead — no special effects. Offset +0x14B.
    pub conventional: bool,
    /// Rocks vehicles on impact. Offset +0x14C.
    pub rocker: bool,
    /// Spawns tiberium/ore on impact. Offset +0x14E.
    pub tiberium: bool,
    /// Bright flash on detonation. Offset +0x14F.
    pub bright: bool,
    /// Extra damage to prone infantry. Offset +0x150.
    pub prone_damage: bool,
    /// Instantly destroys any wall. Offset +0x151.
    pub wall_absolute_destroyer: bool,
    /// Chrono legionnaire erase effect. Offset +0x152.
    pub temporal: bool,
    /// Changes target's locomotor (magnetron). Offset +0x153.
    pub is_locomotor: bool,
    /// Terror drone attach. Offset +0x154.
    pub parasite: bool,
    /// Mind control visual effect. Offset +0x155.
    pub psychedelic: bool,
    /// Crazy ivan bomb attach. Offset +0x174.
    pub ivan_bomb: bool,
    /// Yuri mind control. Offset +0x175.
    pub mind_control: bool,
    /// Poison damage. Offset +0x176.
    pub poison: bool,
    /// Calls in airstrike. Offset +0x177.
    pub airstrike: bool,
    /// Tesla weapon. Offset +0x178.
    pub electric: bool,
    /// Radiation contamination. Offset +0x179.
    pub radiation: bool,
    /// Kills infantry outright below threshold. Offset +0x17A.
    pub culling: bool,
    /// Spy disguise warhead. Offset +0x17B.
    pub makes_disguise: bool,

    // --- Int fields ---
    /// EMP duration in frames. Offset +0x170.
    pub em_effect: i32,
    /// Money transfer on hit. Offset +0x17C.
    pub transact_money: i32,
    /// Infantry death type for cell-level kills. Offset +0x114.
    pub cell_inf_death: i32,

    // --- List fields ---
    /// Debris animation names spawned on detonation (Debris= in rules.ini).
    pub debris: Vec<String>,
    /// Maximum count for each debris type (DebrisMaximums= in rules.ini).
    pub debris_maximums: Vec<i32>,
}

impl WarheadType {
    /// Parse a WarheadType from a rules.ini section.
    pub fn from_ini_section(id: &str, section: &IniSection) -> Self {
        let verses: Vec<u8> = section.get("Verses").map(parse_verses).unwrap_or_default();

        let cell_spread: SimFixed = section
            .get_f32("CellSpread")
            .map(sim_from_f32)
            .unwrap_or(SIM_ZERO);
        let percent_at_max: u8 = section
            .get_f32("PercentAtMax")
            .map(|v| (v * 100.0).round().clamp(0.0, 200.0) as u8)
            .unwrap_or(100);

        let anim_list: Vec<String> = section
            .get_list("AnimList")
            .unwrap_or_default()
            .into_iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();

        let debris: Vec<String> = section
            .get_list("Debris")
            .unwrap_or_default()
            .into_iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();

        let debris_maximums: Vec<i32> = section
            .get_list("DebrisMaximums")
            .unwrap_or_default()
            .into_iter()
            .filter_map(|s| s.trim().parse::<i32>().ok())
            .collect();

        Self {
            id: id.to_string(),
            verses,
            cell_spread,
            percent_at_max,
            wall: section.get_bool("Wall").unwrap_or(false),
            anim_list,
            inf_death: section.get_i32("InfDeath").unwrap_or(1).clamp(0, 10) as u8,

            // Bool fields — all default false
            conventional: section.get_bool("Conventional").unwrap_or(false),
            rocker: section.get_bool("Rocker").unwrap_or(false),
            tiberium: section.get_bool("Tiberium").unwrap_or(false),
            bright: section.get_bool("Bright").unwrap_or(false),
            prone_damage: section.get_bool("ProneDamage").unwrap_or(false),
            wall_absolute_destroyer: section.get_bool("WallAbsoluteDestroyer").unwrap_or(false),
            temporal: section.get_bool("Temporal").unwrap_or(false),
            is_locomotor: section.get_bool("IsLocomotor").unwrap_or(false),
            parasite: section.get_bool("Parasite").unwrap_or(false),
            psychedelic: section.get_bool("Psychedelic").unwrap_or(false),
            ivan_bomb: section.get_bool("IvanBomb").unwrap_or(false),
            mind_control: section.get_bool("MindControl").unwrap_or(false),
            poison: section.get_bool("Poison").unwrap_or(false),
            airstrike: section.get_bool("Airstrike").unwrap_or(false),
            electric: section.get_bool("Electric").unwrap_or(false),
            radiation: section.get_bool("Radiation").unwrap_or(false),
            culling: section.get_bool("Culling").unwrap_or(false),
            makes_disguise: section.get_bool("MakesDisguise").unwrap_or(false),

            // Int fields — all default 0
            em_effect: section.get_i32("EMEffect").unwrap_or(0),
            transact_money: section.get_i32("TransactMoney").unwrap_or(0),
            cell_inf_death: section.get_i32("CellInfDeath").unwrap_or(0),

            // List fields
            debris,
            debris_maximums,
        }
    }
}

/// Parse the Verses= value into a vec of u8 percentages (0–200).
///
/// Format: "100%,100%,90%,75%,..." — percentages separated by commas.
/// Values without a '%' suffix are treated as raw percentages (e.g., "100" = 100).
/// Result: 100 = full damage, 0 = immune, 200 = double damage.
fn parse_verses(raw: &str) -> Vec<u8> {
    raw.split(',')
        .map(|s| {
            let s: &str = s.trim();
            let pct: f32 = if let Some(stripped) = s.strip_suffix('%') {
                stripped.trim().parse::<f32>().unwrap_or(100.0)
            } else {
                // Some mods/versions use raw percentages without '%'.
                s.parse::<f32>().unwrap_or(100.0)
            };
            pct.round().clamp(0.0, 200.0) as u8
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::ini_parser::IniFile;

    #[test]
    fn test_parse_warhead() {
        let ini: IniFile = IniFile::from_str(
            "[AP]\nCellSpread=0.5\nPercentAtMax=0.25\nWall=yes\n\
             Verses=100%,100%,90%,75%,75%,75%,60%,30%,20%,0%,0%\n",
        );
        let section: &IniSection = ini.section("AP").unwrap();
        let wh: WarheadType = WarheadType::from_ini_section("AP", section);

        assert_eq!(wh.id, "AP");
        assert_eq!(wh.cell_spread, sim_from_f32(0.5));
        assert_eq!(wh.percent_at_max, 25); // 0.25 * 100 = 25
        assert!(wh.wall);
        assert_eq!(wh.verses.len(), 11);
        assert_eq!(wh.verses[0], 100); // none: 100%
        assert_eq!(wh.verses[2], 90); // plate: 90%
        assert_eq!(wh.verses[6], 60); // wood: 60%
        assert_eq!(wh.verses[10], 0); // special_2: 0%
    }

    #[test]
    fn test_warhead_defaults() {
        let ini: IniFile = IniFile::from_str("[Empty]\n");
        let section: &IniSection = ini.section("Empty").unwrap();
        let wh: WarheadType = WarheadType::from_ini_section("Empty", section);

        assert!(wh.verses.is_empty());
        assert_eq!(wh.cell_spread, SIM_ZERO);
        assert_eq!(wh.percent_at_max, 100);
        assert!(!wh.wall);
    }

    #[test]
    fn test_warhead_mind_control() {
        let ini: IniFile = IniFile::from_str(
            "[YOUREWEAK]\nMindControl=yes\nCellSpread=0\nPercentAtMax=1\n\
             Verses=0%,0%,0%,0%,0%,0%,0%,0%,0%,0%,0%\n",
        );
        let section: &IniSection = ini.section("YOUREWEAK").unwrap();
        let wh: WarheadType = WarheadType::from_ini_section("YOUREWEAK", section);

        assert!(wh.mind_control);
        assert!(!wh.temporal);
        assert!(!wh.electric);
        assert!(!wh.ivan_bomb);
    }

    #[test]
    fn test_parse_verses_without_percent() {
        // Some formats omit the '%' suffix.
        let result: Vec<u8> = parse_verses("100,50,25");
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], 100);
        assert_eq!(result[1], 50);
        assert_eq!(result[2], 25);
    }
}
