//! IronCurtain superweapon launch handler.
//!
//! Applies timed invulnerability to all techno entities in a 3×3 cell grid
//! centered on the target cell. Infantry are killed instead of protected
//! (matches InfantryClass::IronCurtain override).
//!
//! ## Dependency rules
//! - Part of sim/ — depends on rules/, sim/superweapon/{invulnerability,cell_grid},
//!   sim/game_entity, sim/components, sim/world.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::map::entities::EntityCategory;
use crate::rules::ruleset::RuleSet;
use crate::sim::components::WorldEffect;
use crate::sim::intern::InternedId;
use crate::sim::superweapon::cell_grid::iter_cells_3x3;
use crate::sim::superweapon::invulnerability::{InvulnKind, apply_invulnerability};
use crate::sim::world::{SimSoundEvent, Simulation};

/// Launch IronCurtain at (target_rx, target_ry). Applies invulnerability or
/// kills infantry in the 3×3 cell grid centered on the target.
pub fn launch(
    sim: &mut Simulation,
    rules: &RuleSet,
    owner: InternedId,
    target_rx: u16,
    target_ry: u16,
) -> bool {
    let duration = rules.general.iron_curtain_duration;
    let anim_name = rules.general.iron_curtain_invoke_anim.clone();
    let current_frame = sim.tick as u32;

    // 1. Spawn invoke animation at target.
    spawn_invoke_anim(sim, &anim_name, target_rx, target_ry);

    // 2. Collect entity IDs in the 3×3 grid (snapshot to avoid borrow conflict).
    let cells: Vec<(u16, u16)> = iter_cells_3x3(target_rx, target_ry).collect();
    let target_ids: Vec<u64> = sim
        .entities
        .values()
        .filter(|e| {
            cells
                .iter()
                .any(|(rx, ry)| e.position.rx == *rx && e.position.ry == *ry)
        })
        .filter(|e| e.health.current > 0 && !e.dying)
        .map(|e| e.stable_id)
        .collect();

    // 3. Apply effect per entity.
    for id in &target_ids {
        if let Some(entity) = sim.entities.get_mut(*id) {
            if entity.category == EntityCategory::Infantry {
                // IronCurtain kills infantry (matches binary override).
                entity.health.current = 0;
                entity.dying = true;
            } else {
                apply_invulnerability(entity, current_frame, duration, InvulnKind::IronCurtain);
            }
        }
    }

    // 4. Sound event.
    sim.sound_events.push(SimSoundEvent::SuperWeaponLaunched {
        owner,
        rx: target_rx,
        ry: target_ry,
    });

    log::info!(
        "IronCurtain launched at ({}, {}) by '{}', {} targets affected",
        target_rx,
        target_ry,
        sim.interner.resolve(owner),
        target_ids.len()
    );

    true
}

/// Spawn the invoke animation at the target cell.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::ini_parser::IniFile;
    use crate::sim::components::Health;
    use crate::sim::game_entity::GameEntity;
    use crate::sim::superweapon::invulnerability::is_invulnerable;

    fn test_rules() -> RuleSet {
        let ini = IniFile::from_str(
            "[InfantryTypes]\n0=E1\n[VehicleTypes]\n0=MTNK\n[AircraftTypes]\n[BuildingTypes]\n\
             [E1]\nStrength=125\nArmor=flak\nSpeed=4\n\
             [MTNK]\nStrength=300\nArmor=heavy\nSpeed=6\n",
        );
        RuleSet::from_ini(&ini).expect("test rules")
    }

    fn spawn(sim: &mut Simulation, id: u64, type_ref: &str, rx: u16, ry: u16, cat: EntityCategory) {
        let owner = sim.interner.intern("Americans");
        let tref = sim.interner.intern(type_ref);
        let e = GameEntity::new(
            id,
            rx,
            ry,
            0,
            0,
            owner,
            Health { current: 300, max: 300 },
            tref,
            cat,
            0,
            5,
            matches!(cat, EntityCategory::Unit),
        );
        sim.entities.insert(e);
    }

    #[test]
    fn ic_protects_vehicles_in_grid() {
        let rules = test_rules();
        let mut sim = Simulation::new();
        let owner = sim.interner.intern("Americans");
        spawn(&mut sim, 1, "MTNK", 10, 10, EntityCategory::Unit);
        assert!(launch(&mut sim, &rules, owner, 10, 10));
        let e = sim.entities.get(1).expect("tank exists");
        assert!(e.invulnerability.is_some());
        assert!(is_invulnerable(e.invulnerability.as_ref(), sim.tick as u32));
    }

    #[test]
    fn ic_kills_infantry_in_grid() {
        let rules = test_rules();
        let mut sim = Simulation::new();
        let owner = sim.interner.intern("Americans");
        spawn(&mut sim, 1, "E1", 10, 10, EntityCategory::Infantry);
        assert!(launch(&mut sim, &rules, owner, 10, 10));
        let e = sim.entities.get(1).expect("infantry exists");
        assert_eq!(e.health.current, 0);
        assert!(e.dying);
        assert!(e.invulnerability.is_none());
    }

    #[test]
    fn ic_affects_all_3x3_cells() {
        let rules = test_rules();
        let mut sim = Simulation::new();
        let owner = sim.interner.intern("Americans");
        spawn(&mut sim, 1, "MTNK", 9, 9, EntityCategory::Unit);
        spawn(&mut sim, 2, "MTNK", 10, 10, EntityCategory::Unit);
        spawn(&mut sim, 3, "MTNK", 11, 11, EntityCategory::Unit);
        launch(&mut sim, &rules, owner, 10, 10);
        assert!(sim.entities.get(1).unwrap().invulnerability.is_some());
        assert!(sim.entities.get(2).unwrap().invulnerability.is_some());
        assert!(sim.entities.get(3).unwrap().invulnerability.is_some());
    }

    #[test]
    fn ic_ignores_cells_outside_grid() {
        let rules = test_rules();
        let mut sim = Simulation::new();
        let owner = sim.interner.intern("Americans");
        spawn(&mut sim, 1, "MTNK", 15, 15, EntityCategory::Unit);
        launch(&mut sim, &rules, owner, 10, 10);
        assert!(sim.entities.get(1).unwrap().invulnerability.is_none());
    }
}

fn spawn_invoke_anim(sim: &mut Simulation, anim_name: &str, rx: u16, ry: u16) {
    let iid = sim.interner.intern(anim_name);
    let frames = sim.effect_frame_counts.get(&iid).copied().unwrap_or(20);
    sim.world_effects.push(WorldEffect {
        shp_name: iid,
        rx,
        ry,
        z: 5,
        frame: 0,
        total_frames: frames,
        rate_ms: 67,
        elapsed_ms: 0,
        translucent: false,
        delay_ms: 0,
    });
}
