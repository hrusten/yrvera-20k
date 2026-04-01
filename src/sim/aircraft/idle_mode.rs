//! Enter_Idle_Mode decision tree for aircraft.
//!
//! Determines what mission to assign when an aircraft has nothing to do.
//! Matches gamemd.exe Enter_Idle_Mode.
//!
//! ## Key behaviors
//! - AirportBound aircraft with no helipad → self-destruct (crash)
//! - Ammo depleted → RTB to nearest airfield
//! - Has target and ammo → re-engage (Attack)
//! - AI-owned → Hunt (not yet implemented, falls back to Guard)
//! - Player-owned → Guard
//!
//! ## Dependency rules
//! - Part of sim/ — depends on rules/, sim/components, sim/docking.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::sim::aircraft::AircraftMission;

/// Input snapshot for the idle mode decision.
pub struct IdleModeInput {
    pub ammo_current: i32,
    pub ammo_max: i32,
    pub has_weapon: bool,
    pub has_target: bool,
    pub airport_bound: bool,
    pub is_airborne: bool,
    /// Nearest airfield (stable_id, rx, ry) if one exists.
    pub nearest_airfield: Option<(u64, u16, u16)>,
}

/// Result of the idle mode decision.
#[derive(Debug)]
pub enum IdleModeResult {
    /// Assign this mission.
    Mission(AircraftMission),
    /// Self-destruct (AirportBound, no helipad available).
    SelfDestruct,
}

/// Decide what mission an aircraft should enter after completing its current one.
pub fn enter_idle_mode(input: &IdleModeInput) -> IdleModeResult {
    // Ammo depleted and has weapons → need to RTB.
    if input.has_weapon && input.ammo_current <= 0 && input.ammo_max > 0 {
        if let Some((af_id, _, _)) = input.nearest_airfield {
            return IdleModeResult::Mission(AircraftMission::ReturnToBase { airfield_id: af_id });
        }
        // No airfield available.
        if input.airport_bound {
            return IdleModeResult::SelfDestruct;
        }
        // Not airport-bound: just guard (hover until something happens).
        return IdleModeResult::Mission(AircraftMission::Guard);
    }

    // In flight with ammo and a target → re-engage.
    if input.is_airborne && input.ammo_current > 0 && input.has_target {
        return IdleModeResult::Mission(AircraftMission::Attack {
            sub_state: 0,
            has_fired: false,
            is_strafe: false,
        });
    }

    // AirportBound in flight with no target → RTB.
    if input.airport_bound && input.is_airborne {
        if let Some((af_id, _, _)) = input.nearest_airfield {
            return IdleModeResult::Mission(AircraftMission::ReturnToBase { airfield_id: af_id });
        }
        // AirportBound, no airfield, but has ammo → guard until airfield built.
        if input.ammo_current > 0 {
            return IdleModeResult::Mission(AircraftMission::Guard);
        }
        return IdleModeResult::SelfDestruct;
    }

    // Default: Guard.
    IdleModeResult::Mission(AircraftMission::Guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ammo_depleted_with_airfield_rtb() {
        let input = IdleModeInput {
            ammo_current: 0,
            ammo_max: 2,
            has_weapon: true,
            has_target: false,
            airport_bound: true,
            is_airborne: true,
            nearest_airfield: Some((100, 50, 50)),
        };
        match enter_idle_mode(&input) {
            IdleModeResult::Mission(AircraftMission::ReturnToBase { airfield_id }) => {
                assert_eq!(airfield_id, 100);
            }
            other => panic!("Expected ReturnToBase, got {:?}", other),
        }
    }

    #[test]
    fn test_airport_bound_no_airfield_self_destruct() {
        let input = IdleModeInput {
            ammo_current: 0,
            ammo_max: 2,
            has_weapon: true,
            has_target: false,
            airport_bound: true,
            is_airborne: true,
            nearest_airfield: None,
        };
        assert!(matches!(
            enter_idle_mode(&input),
            IdleModeResult::SelfDestruct
        ));
    }

    #[test]
    fn test_has_ammo_and_target_reengage() {
        let input = IdleModeInput {
            ammo_current: 1,
            ammo_max: 2,
            has_weapon: true,
            has_target: true,
            airport_bound: false,
            is_airborne: true,
            nearest_airfield: Some((100, 50, 50)),
        };
        assert!(matches!(
            enter_idle_mode(&input),
            IdleModeResult::Mission(AircraftMission::Attack { sub_state: 0, .. })
        ));
    }

    #[test]
    fn test_default_guard() {
        let input = IdleModeInput {
            ammo_current: 2,
            ammo_max: 2,
            has_weapon: true,
            has_target: false,
            airport_bound: false,
            is_airborne: false,
            nearest_airfield: None,
        };
        assert!(matches!(
            enter_idle_mode(&input),
            IdleModeResult::Mission(AircraftMission::Guard)
        ));
    }
}
