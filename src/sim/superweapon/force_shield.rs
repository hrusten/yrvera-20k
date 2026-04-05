//! ForceShield superweapon launch handler.
//!
//! Applies timed invulnerability to all allied buildings within
//! ForceShieldRadius cells of the target. Also triggers a power blackout
//! on the owning house (shared mechanism with spy infiltration).
//!
//! ## Dependency rules
//! - Part of sim/ — depends on rules/, map/houses, sim/superweapon/invulnerability,
//!   sim/power_system, sim/game_entity, sim/components, sim/world.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::map::entities::EntityCategory;
use crate::map::houses::are_houses_friendly;
use crate::rules::ruleset::RuleSet;
use crate::sim::components::WorldEffect;
use crate::sim::intern::InternedId;
use crate::sim::superweapon::invulnerability::{InvulnKind, apply_invulnerability};
use crate::sim::world::{SimSoundEvent, Simulation};

const LEPTONS_PER_CELL: i64 = 256;
const CELL_CENTER_LEPTON: i64 = 128;

/// Launch ForceShield at (target_rx, target_ry). Protects allied buildings
/// in radius, triggers owner power blackout.
pub fn launch(
    sim: &mut Simulation,
    rules: &RuleSet,
    owner: InternedId,
    target_rx: u16,
    target_ry: u16,
) -> bool {
    let duration = rules.general.force_shield_duration;
    let radius_cells = rules.general.force_shield_radius as i64;
    let radius_leptons = radius_cells * LEPTONS_PER_CELL;
    let radius_sq = radius_leptons * radius_leptons;
    let blackout = rules.general.force_shield_blackout_duration;
    let anim_name = rules.general.force_shield_invoke_anim.clone();
    let current_frame = sim.tick as u32;

    // 1. Spawn invoke animation.
    spawn_invoke_anim(sim, &anim_name, target_rx, target_ry);

    // 2. Trigger power blackout on owner (take max to never shorten existing).
    if let Some(power_state) = sim.power_states.get_mut(&owner) {
        power_state.power_blackout_remaining = power_state.power_blackout_remaining.max(blackout);
    } else {
        log::warn!(
            "ForceShield: no PowerState for owner '{}', skipping blackout",
            sim.interner.resolve(owner)
        );
    }

    // 3. Find allied buildings within radius.
    let owner_str = sim.interner.resolve(owner).to_string();
    let target_x_leptons: i64 = target_rx as i64 * LEPTONS_PER_CELL + CELL_CENTER_LEPTON;
    let target_y_leptons: i64 = target_ry as i64 * LEPTONS_PER_CELL + CELL_CENTER_LEPTON;

    let target_ids: Vec<u64> = sim
        .entities
        .values()
        .filter(|e| e.category == EntityCategory::Structure)
        .filter(|e| e.health.current > 0 && !e.dying)
        .filter(|e| {
            let other = sim.interner.resolve(e.owner);
            are_houses_friendly(&sim.house_alliances, &owner_str, other)
        })
        .filter(|e| {
            rules
                .object(sim.interner.resolve(e.type_ref))
                .map(|o| !o.no_force_shield)
                .unwrap_or(true)
        })
        .filter(|e| {
            let ex: i64 = e.position.rx as i64 * LEPTONS_PER_CELL
                + e.position.sub_x.to_num::<i64>();
            let ey: i64 = e.position.ry as i64 * LEPTONS_PER_CELL
                + e.position.sub_y.to_num::<i64>();
            let dx = ex - target_x_leptons;
            let dy = ey - target_y_leptons;
            dx * dx + dy * dy <= radius_sq
        })
        .map(|e| e.stable_id)
        .collect();

    // 4. Apply invulnerability.
    for id in &target_ids {
        if let Some(entity) = sim.entities.get_mut(*id) {
            apply_invulnerability(entity, current_frame, duration, InvulnKind::ForceShield);
        }
    }

    // 5. Sound event.
    sim.sound_events.push(SimSoundEvent::SuperWeaponLaunched {
        owner,
        rx: target_rx,
        ry: target_ry,
    });

    log::info!(
        "ForceShield launched at ({}, {}) by '{}', {} buildings protected",
        target_rx,
        target_ry,
        owner_str,
        target_ids.len()
    );

    true
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
