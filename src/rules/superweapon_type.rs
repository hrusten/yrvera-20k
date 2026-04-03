//! Superweapon type definitions parsed from rules(md).ini.
//!
//! Each superweapon has its own `[SwName]` section in rules.ini, listed under
//! `[SuperWeaponTypes]`. Defines charging time, cursor action, sidebar image,
//! and the dispatch kind that controls what happens on launch.
//!
//! ## rules.ini format
//! ```ini
//! [LightningStormSpecial]
//! UIName=Name:LStorm
//! Type=LightningStorm
//! RechargeTime=10
//! SidebarImage=INTICON
//! Action=LightningStorm
//! ```
//!
//! ## Dependency rules
//! - Part of rules/ — no dependencies on sim/, render/, ui/, etc.

use crate::rules::ini_parser::IniSection;

/// Maps the INI `Type=` string to an enum for launch dispatch.
///
/// Index values match gamemd.exe's SuperWeaponType enum (0x006CEA20).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SuperWeaponKind {
    /// Type=MultiMissile (index 0) — Nuclear missile.
    MultiMissile,
    /// Type=IronCurtain (index 1) — Unit invulnerability.
    IronCurtain,
    /// Type=LightningStorm (index 2) — Weather storm.
    LightningStorm,
    /// Type=ChronoSphere (index 3) — Source cell selection.
    ChronoSphere,
    /// Type=ChronoWarp (index 4) — Destination warp.
    ChronoWarp,
    /// Type=ParaDrop (index 5) — Paratroop delivery.
    ParaDrop,
    /// Type=AmerParaDrop (index 6) — American paratroop variant.
    AmerParaDrop,
    /// Type=PsychicDominator (index 7) — Area mind control.
    PsychicDominator,
    /// Type=SpyPlane (index 8) — Recon flyover.
    SpyPlane,
    /// Type=GeneticConverter (index 9) — Infantry mutation.
    GeneticConverter,
    /// Type=ForceShield (index 10) — Building invulnerability.
    ForceShield,
    /// Type=PsychicReveal (index 11) — Shroud reveal.
    PsychicReveal,
}

impl SuperWeaponKind {
    /// Parse from the INI `Type=` string value. Case-insensitive.
    pub fn from_ini_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "multimissile" => Some(Self::MultiMissile),
            "ironcurtain" => Some(Self::IronCurtain),
            "lightningstorm" => Some(Self::LightningStorm),
            "chronosphere" => Some(Self::ChronoSphere),
            "chronowarp" => Some(Self::ChronoWarp),
            "paradrop" => Some(Self::ParaDrop),
            "amerparadrop" => Some(Self::AmerParaDrop),
            "psychicdominator" => Some(Self::PsychicDominator),
            "spyplane" => Some(Self::SpyPlane),
            "geneticconverter" => Some(Self::GeneticConverter),
            "forceshield" => Some(Self::ForceShield),
            "psychicreveal" => Some(Self::PsychicReveal),
            _ => None,
        }
    }
}

/// Superweapon type definition parsed from a rules.ini section.
///
/// Each superweapon type defines charging behavior, sidebar presentation,
/// targeting cursor, and the launch dispatch kind. Runtime instances are
/// tracked per-house in `sim/superweapon/`.
#[derive(Debug, Clone)]
pub struct SuperWeaponType {
    /// Section name in rules.ini (e.g., "LightningStormSpecial").
    pub id: String,
    /// Launch dispatch type (determines what happens on fire).
    pub kind: SuperWeaponKind,
    /// Charge time in game frames. INI value is minutes × 900.
    pub recharge_time_frames: i32,
    /// Whether charging pauses when the owner has low power.
    pub is_powered: bool,
    /// Cursor action name for targeting (e.g., "LightningStorm").
    pub action: Option<String>,
    /// SHP filename for sidebar cameo (e.g., "INTICON").
    pub sidebar_image: Option<String>,
    /// Whether to show the charge timer countdown on the sidebar.
    pub show_timer: bool,
    /// Whether this SW can be disabled from the game lobby.
    pub disableable_from_shell: bool,
    /// Charge→drain cycling mode (used by Force Shield).
    pub use_charge_drain: bool,
    /// Two-click mode: first click selects source (ChronoSphere).
    pub pre_click: bool,
    /// Two-click mode: second click selects destination (ChronoWarp).
    pub post_click: bool,
    /// Prerequisite SW type name (e.g., ChronoWarp needs "ChronoSphere").
    pub pre_dependent: Option<String>,
    /// Targeting range in cells.
    pub range: f32,
    /// WeaponType reference (e.g., NukeCarrier for nuke).
    pub weapon_type: Option<String>,
    /// Required secondary building (e.g., NukeSilo for nuke).
    pub aux_building: Option<String>,
    /// Sound played when fully charged.
    pub special_sound: Option<String>,
    /// Sound played on launch/activation.
    pub start_sound: Option<String>,
    /// Sidebar tab flash duration in frames on activation.
    pub flash_sidebar_tab_frames: i32,
    /// When true, suspension doesn't auto-resume on power restore.
    pub manual_control: bool,
    /// Cursor line drawing multiplier.
    pub line_multiplier: i32,
}

impl SuperWeaponType {
    /// Parse a SuperWeaponType from a rules.ini section.
    pub fn from_ini_section(id: &str, section: &IniSection) -> Option<Self> {
        let kind_str = section.get("Type")?;
        let kind = SuperWeaponKind::from_ini_str(kind_str)?;
        let recharge_minutes = section.get_f32("RechargeTime").unwrap_or(5.0);
        Some(Self {
            id: id.to_string(),
            kind,
            recharge_time_frames: (recharge_minutes * 900.0) as i32,
            is_powered: section.get_bool("IsPowered").unwrap_or(true),
            action: section.get("Action").map(|s| s.to_string()),
            sidebar_image: section.get("SidebarImage").map(|s| s.to_string()),
            show_timer: section.get_bool("ShowTimer").unwrap_or(false),
            disableable_from_shell: section.get_bool("DisableableFromShell").unwrap_or(false),
            use_charge_drain: section.get_bool("UseChargeDrain").unwrap_or(false),
            pre_click: section.get_bool("PreClick").unwrap_or(false),
            post_click: section.get_bool("PostClick").unwrap_or(false),
            pre_dependent: section.get("PreDependent").map(|s| s.to_string()),
            range: section.get_f32("Range").unwrap_or(0.0),
            weapon_type: section.get("WeaponType").map(|s| s.to_string()),
            aux_building: section.get("AuxBuilding").map(|s| s.to_string()),
            special_sound: section.get("SpecialSound").map(|s| s.to_string()),
            start_sound: section.get("StartSound").map(|s| s.to_string()),
            flash_sidebar_tab_frames: section.get_i32("FlashSidebarTabFrames").unwrap_or(0),
            manual_control: section.get_bool("ManualControl").unwrap_or(false),
            line_multiplier: section.get_i32("LineMultiplier").unwrap_or(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::ini_parser::IniFile;

    #[test]
    fn parse_lightning_storm_special() {
        let ini_text = "\
[LightningStormSpecial]
UIName=Name:LStorm
Name=Lightning Storm
Type=LightningStorm
RechargeTime=10
SidebarImage=INTICON
Action=LightningStorm
IsPowered=yes
ShowTimer=yes
DisableableFromShell=yes
";
        let ini = IniFile::from_str(ini_text);
        let section = ini.section("LightningStormSpecial").unwrap();
        let sw = SuperWeaponType::from_ini_section("LightningStormSpecial", section).unwrap();
        assert_eq!(sw.kind, SuperWeaponKind::LightningStorm);
        assert_eq!(sw.recharge_time_frames, 9000); // 10 min × 900
        assert!(sw.is_powered);
        assert!(sw.show_timer);
        assert!(sw.disableable_from_shell);
    }

    #[test]
    fn parse_unknown_type_returns_none() {
        let ini_text = "\
[BogusWeapon]
Type=BogusType
";
        let ini = IniFile::from_str(ini_text);
        let section = ini.section("BogusWeapon").unwrap();
        assert!(SuperWeaponType::from_ini_section("BogusWeapon", section).is_none());
    }

    #[test]
    fn kind_from_ini_str_case_insensitive() {
        assert_eq!(
            SuperWeaponKind::from_ini_str("LightningStorm"),
            Some(SuperWeaponKind::LightningStorm)
        );
        assert_eq!(
            SuperWeaponKind::from_ini_str("lightningstorm"),
            Some(SuperWeaponKind::LightningStorm)
        );
        assert_eq!(
            SuperWeaponKind::from_ini_str("MULTIMISSILE"),
            Some(SuperWeaponKind::MultiMissile)
        );
        assert_eq!(SuperWeaponKind::from_ini_str("bogus"), None);
    }
}
