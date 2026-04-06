//! PsychicReveal superweapon launch handler.
//!
//! Reveals shroud in a radius around the target cell for the owning house.
//! Matches binary's double-call to MapClass::RevealAroundCell (verified).
//!
//! ## Dependency rules
//! - Part of sim/ — depends on rules/, sim/vision, sim/world.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::rules::ruleset::RuleSet;
use crate::sim::intern::InternedId;
use crate::sim::vision;
use crate::sim::world::{SimSoundEvent, Simulation};

/// Launch PsychicReveal at (target_rx, target_ry). Reveals shroud in radius.
pub fn launch(
    sim: &mut Simulation,
    rules: &RuleSet,
    owner: InternedId,
    target_rx: u16,
    target_ry: u16,
) -> bool {
    let radius = rules.general.psychic_reveal_radius as u16;

    // Double call matches binary (verified). Both calls pass identical args.
    vision::reveal_radius(&mut sim.fog, owner, target_rx, target_ry, radius);
    vision::reveal_radius(&mut sim.fog, owner, target_rx, target_ry, radius);

    sim.sound_events.push(SimSoundEvent::SuperWeaponLaunched {
        owner,
        rx: target_rx,
        ry: target_ry,
    });

    log::info!(
        "PsychicReveal launched at ({}, {}) by '{}', radius={}",
        target_rx,
        target_ry,
        sim.interner.resolve(owner),
        radius
    );

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::ini_parser::IniFile;

    fn minimal_rules() -> RuleSet {
        let ini = IniFile::from_str(
            "[InfantryTypes]\n[VehicleTypes]\n[AircraftTypes]\n[BuildingTypes]\n\
             [General]\nPsychicRevealRadius=5\n",
        );
        RuleSet::from_ini(&ini).expect("test rules")
    }

    #[test]
    fn pr_reveals_cells_in_radius() {
        let rules = minimal_rules();
        let mut sim = Simulation::new();
        sim.fog.width = 30;
        sim.fog.height = 30;
        let owner = sim.interner.intern("Americans");
        assert!(launch(&mut sim, &rules, owner, 10, 10));
        let vis = sim.fog.by_owner.get(&owner).expect("owner fog exists");
        assert!(vis.is_visible(10, 10));
    }

    #[test]
    fn pr_does_not_reveal_beyond_radius() {
        let rules = minimal_rules();
        let mut sim = Simulation::new();
        sim.fog.width = 30;
        sim.fog.height = 30;
        let owner = sim.interner.intern("Americans");
        launch(&mut sim, &rules, owner, 10, 10);
        let vis = sim.fog.by_owner.get(&owner).expect("owner fog exists");
        // Radius=5, so (25, 25) is well outside.
        assert!(!vis.is_visible(25, 25));
    }
}
