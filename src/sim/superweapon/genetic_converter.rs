//! GeneticConverter superweapon launch handler.
//!
//! Mutates infantry in target area into Brutes. Two code paths depending on
//! Rules->MutateExplosion: either AoE via MutateExplosionWarhead, or per-cell
//! MutateWarhead applied to infantry in a 3×3 grid. On infantry death by
//! either warhead, spawns a Brute at the death position.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on rules/, sim/combat/combat_aoe, sim/superweapon/cell_grid,
//!   sim/game_entity, sim/components, sim/world.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::map::entities::EntityCategory;
use crate::rules::ruleset::RuleSet;
use crate::sim::combat::combat_aoe::apply_aoe_damage;
use crate::sim::components::WorldEffect;
use crate::sim::intern::InternedId;
use crate::sim::superweapon::cell_grid::iter_cells_3x3;
use crate::sim::world::{SimSoundEvent, Simulation};

/// Brute type_ref for Tier 1. Generalize to rules.general.animation_to_infantry[0]
/// when the full AnimClass death-to-infantry pipeline is implemented.
const BRUTE_TYPE_REF: &str = "BRUTE";

/// Mutate damage constant — large enough to kill any infantry in one hit.
/// Matches the design intent of the original engine's case-9 AoE path (exact
/// binary constant not yet extracted).
const MUTATE_AOE_DAMAGE: i32 = 9999;

/// Launch GeneticConverter at (target_rx, target_ry). Mutates infantry in area.
pub fn launch(
    sim: &mut Simulation,
    rules: &RuleSet,
    owner: InternedId,
    target_rx: u16,
    target_ry: u16,
) -> bool {
    // 1. Spawn invoke anim (IonBlast equivalent).
    spawn_invoke_anim(sim, "IONBLAST", target_rx, target_ry);

    // 2. Collect infantry IDs + their positions BEFORE damage (for Brute spawn).
    let (killed_infantry_cells, kill_count) = if rules.general.mutate_explosion {
        apply_mutate_explosion(sim, rules, target_rx, target_ry, owner)
    } else {
        apply_mutate_per_cell(sim, rules, target_rx, target_ry)
    };

    // 3. Spawn a Brute at each killed-infantry cell.
    let owner_name = sim.interner.resolve(owner).to_string();
    for (rx, ry) in killed_infantry_cells {
        spawn_brute(sim, rules, &owner_name, rx, ry);
    }

    // 4. Sound event.
    sim.sound_events.push(SimSoundEvent::SuperWeaponLaunched {
        owner,
        rx: target_rx,
        ry: target_ry,
    });

    log::info!(
        "GeneticConverter launched at ({}, {}) by '{}', {} infantry mutated",
        target_rx,
        target_ry,
        sim.interner.resolve(owner),
        kill_count
    );

    true
}

/// MutateExplosion path: AoE damage via MutateExplosionWarhead.
/// Returns list of (rx, ry) cell positions of infantry killed.
fn apply_mutate_explosion(
    sim: &mut Simulation,
    rules: &RuleSet,
    target_rx: u16,
    target_ry: u16,
    owner: InternedId,
) -> (Vec<(u16, u16)>, usize) {
    let warhead_id = rules.general.mutate_explosion_warhead.clone();
    let Some(warhead) = rules.warhead(&warhead_id) else {
        log::warn!(
            "MutateExplosionWarhead '{}' not found in rules",
            warhead_id
        );
        return (Vec::new(), 0);
    };
    let owner_str = sim.interner.resolve(owner).to_string();
    let base_damage: i32 = MUTATE_AOE_DAMAGE;
    let hits = apply_aoe_damage(
        &sim.entities,
        target_rx,
        target_ry,
        base_damage,
        warhead,
        rules,
        &sim.interner,
        &owner_str,
    );

    let mut killed: Vec<(u16, u16)> = Vec::new();
    for (id, dmg) in &hits {
        // Pre-snapshot category + position + HP BEFORE mutating.
        let snapshot = sim
            .entities
            .get(*id)
            .map(|e| (e.category, e.position.rx, e.position.ry, e.health.current));
        let Some((cat, rx, ry, hp)) = snapshot else {
            continue;
        };
        if cat != EntityCategory::Infantry {
            continue;
        }
        if let Some(e) = sim.entities.get_mut(*id) {
            let new_hp = hp.saturating_sub(*dmg);
            e.health.current = new_hp;
            if new_hp == 0 && !e.dying {
                e.dying = true;
                killed.push((rx, ry));
            }
        }
    }
    let count = killed.len();
    (killed, count)
}

/// Per-cell path: apply MutateWarhead to infantry in 3×3 grid.
fn apply_mutate_per_cell(
    sim: &mut Simulation,
    _rules: &RuleSet,
    target_rx: u16,
    target_ry: u16,
) -> (Vec<(u16, u16)>, usize) {
    let cells: Vec<(u16, u16)> = iter_cells_3x3(target_rx, target_ry).collect();

    // Collect infantry IDs + cell positions first (avoid borrow conflict).
    let victims: Vec<(u64, u16, u16)> = sim
        .entities
        .values()
        .filter(|e| e.category == EntityCategory::Infantry)
        .filter(|e| e.health.current > 0 && !e.dying)
        .filter(|e| {
            cells
                .iter()
                .any(|(rx, ry)| e.position.rx == *rx && e.position.ry == *ry)
        })
        .map(|e| (e.stable_id, e.position.rx, e.position.ry))
        .collect();

    let mut killed: Vec<(u16, u16)> = Vec::new();
    for (id, rx, ry) in &victims {
        if let Some(e) = sim.entities.get_mut(*id) {
            e.health.current = 0;
            e.dying = true;
            killed.push((*rx, *ry));
        }
    }
    let count = killed.len();
    (killed, count)
}

/// Spawn a Brute infantry at the given cell, owned by the launching player.
fn spawn_brute(sim: &mut Simulation, rules: &RuleSet, owner_name: &str, rx: u16, ry: u16) {
    let spawned = sim.spawn_object_at_height(
        BRUTE_TYPE_REF,
        owner_name,
        rx,
        ry,
        /* facing */ 0,
        /* z */ 0,
        rules,
    );
    if spawned.is_none() {
        log::warn!(
            "GeneticConverter: failed to spawn Brute for '{}' at ({},{})",
            owner_name,
            rx,
            ry
        );
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
