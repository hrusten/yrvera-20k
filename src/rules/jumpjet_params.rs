//! Jumpjet-specific parameters parsed from rules.ini.
//!
//! These fields only apply to units with `JumpJet=yes` in rules.ini.
//! Stored as `Option<JumpjetParams>` on ObjectType — None for non-jumpjet units.
//!
//! Key behavior notes from the locomotor report:
//! - `JumpjetAccel` controls acceleration; deceleration = accel * 1.5
//! - `JumpjetHeight` below 208 is effectively void
//! - Crash descent speed = `JumpjetClimb + JumpjetCrash` downward
//! - Infantry with Jumpjet locomotor + JumpJet=yes + no HoverAttack use Walk for
//!   moves of 3 cells or less (TS-style jumpjet infantry)
//!
//! ## Dependency rules
//! - Part of rules/ — no dependencies on sim/, render/, ui/, etc.

use crate::rules::ini_parser::IniSection;
use crate::util::fixed_math::{SimFixed, sim_from_f32};

/// Jumpjet locomotor parameters parsed from rules.ini per-unit keys.
///
/// These control the Jumpjet locomotor's altitude, acceleration, wobble,
/// and crash behavior. Only populated for units with `JumpJet=yes`.
#[derive(Debug, Clone)]
pub struct JumpjetParams {
    /// Turning speed while airborne (JumpjetTurnRate=).
    pub turn_rate: i32,
    /// Flight speed (JumpjetSpeed=). Separate from ground Speed= value.
    pub speed: SimFixed,
    /// Climb/ascent rate per tick (JumpjetClimb=).
    pub climb: SimFixed,
    /// Extra descent speed added during crash (JumpjetCrash=).
    /// Total crash speed = climb + crash.
    pub crash: SimFixed,
    /// Target hover altitude in leptons (JumpjetHeight=). Values below 208
    /// are effectively void in the original engine.
    pub height: i32,
    /// Acceleration rate (JumpjetAccel=). Deceleration = accel * 1.5.
    pub accel: SimFixed,
    /// Wobble amplitude while hovering (JumpjetWobbles=).
    /// KEPT as f32 — only used for render-side visual wobble, not sim state.
    pub wobbles: f32,
    /// Maximum random XY deviation in leptons (JumpjetDeviation=).
    pub deviation: i32,
    /// When true, disables wobble effect entirely (JumpjetNoWobbles=).
    pub no_wobbles: bool,
}

impl JumpjetParams {
    /// Parse jumpjet parameters from a rules.ini section.
    ///
    /// Uses RA2/YR defaults for missing keys.
    pub fn from_ini_section(section: &IniSection) -> Self {
        Self {
            turn_rate: section.get_i32("JumpjetTurnRate").unwrap_or(4),
            speed: section
                .get_f32("JumpjetSpeed")
                .map(sim_from_f32)
                .unwrap_or(sim_from_f32(14.0)),
            climb: section
                .get_f32("JumpjetClimb")
                .map(sim_from_f32)
                .unwrap_or(sim_from_f32(5.0)),
            crash: section
                .get_f32("JumpjetCrash")
                .map(sim_from_f32)
                .unwrap_or(sim_from_f32(5.0)),
            height: section.get_i32("JumpjetHeight").unwrap_or(500),
            accel: section
                .get_f32("JumpjetAccel")
                .map(sim_from_f32)
                .unwrap_or(sim_from_f32(2.0)),
            wobbles: section.get_f32("JumpjetWobbles").unwrap_or(0.15),
            deviation: section.get_i32("JumpjetDeviation").unwrap_or(40),
            no_wobbles: section.get_bool("JumpjetNoWobbles").unwrap_or(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::ini_parser::IniFile;

    #[test]
    fn test_parse_jumpjet_defaults() {
        let ini = IniFile::from_str("[RTNK]\nJumpJet=yes\n");
        let section = ini.section("RTNK").unwrap();
        let params = JumpjetParams::from_ini_section(section);

        assert_eq!(params.turn_rate, 4);
        assert_eq!(params.speed, sim_from_f32(14.0));
        assert_eq!(params.climb, sim_from_f32(5.0));
        assert_eq!(params.crash, sim_from_f32(5.0));
        assert_eq!(params.height, 500);
        assert_eq!(params.accel, sim_from_f32(2.0));
        assert!((params.wobbles - 0.15).abs() < 0.01);
        assert_eq!(params.deviation, 40);
        assert!(!params.no_wobbles);
    }

    #[test]
    fn test_parse_jumpjet_custom_values() {
        let ini = IniFile::from_str(
            "[JUMPJET]\nJumpjetTurnRate=8\nJumpjetSpeed=20.0\n\
             JumpjetClimb=3.0\nJumpjetCrash=10.0\nJumpjetHeight=750\n\
             JumpjetAccel=4.0\nJumpjetWobbles=0.0\nJumpjetDeviation=0\n\
             JumpjetNoWobbles=yes\n",
        );
        let section = ini.section("JUMPJET").unwrap();
        let params = JumpjetParams::from_ini_section(section);

        assert_eq!(params.turn_rate, 8);
        assert_eq!(params.speed, sim_from_f32(20.0));
        assert_eq!(params.climb, sim_from_f32(3.0));
        assert_eq!(params.crash, sim_from_f32(10.0));
        assert_eq!(params.height, 750);
        assert_eq!(params.accel, sim_from_f32(4.0));
        assert!((params.wobbles).abs() < 0.01);
        assert_eq!(params.deviation, 0);
        assert!(params.no_wobbles);
    }
}
