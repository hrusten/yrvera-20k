//! Lightning Storm state machine — bolt generation and area damage.
//!
//! Only one storm can be active globally at a time. The storm has a deferment
//! countdown before bolts begin, then generates center + scatter bolts each
//! tick for the configured duration.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on rules/, sim/components, sim/combat.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::rules::ruleset::RuleSet;
use crate::sim::components::WorldEffect;
use crate::sim::intern::InternedId;
use crate::sim::world::{SimSoundEvent, Simulation};

/// Lightning storm bolt animation names (WeatherConBolts from art.ini).
const BOLT_ANIMS: &[&str] = &["WCLBOLT1", "WCLBOLT2", "WCLBOLT3"];

/// Maximum retry attempts for scatter bolt placement (avoid infinite loop).
const MAX_SCATTER_RETRIES: u32 = 10;

/// Queued lightning storm request — activated when the current storm ends.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QueuedLightningStorm {
    pub owner: InternedId,
    pub target_rx: u16,
    pub target_ry: u16,
}

/// Active lightning storm state.
///
/// Global — only one storm at a time (per original engine).
/// Stored as `Simulation.lightning_storm: Option<LightningStormState>`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LightningStormState {
    /// House that launched the storm.
    pub owner: InternedId,
    /// Storm center cell X.
    pub target_rx: u16,
    /// Storm center cell Y.
    pub target_ry: u16,
    /// Ticks remaining before bolts begin (deferment countdown).
    pub deferment_remaining: i32,
    /// Ticks remaining for active bolt generation.
    pub duration_remaining: i32,
    /// Ticks until next center bolt.
    pub center_bolt_timer: i32,
    /// Ticks until next scatter bolt.
    pub scatter_bolt_timer: i32,
    /// Last bolt cell X (for separation enforcement).
    pub last_bolt_rx: u16,
    /// Last bolt cell Y (for separation enforcement).
    pub last_bolt_ry: u16,
}

/// Start a new lightning storm. If one is already active, queues the request
/// so it activates when the current storm ends.
pub fn start(
    sim: &mut Simulation,
    rules: &RuleSet,
    owner: InternedId,
    target_rx: u16,
    target_ry: u16,
) -> bool {
    if sim.lightning_storm.is_some() {
        log::info!(
            "Lightning Storm queued — one already active, will start when current ends"
        );
        sim.queued_lightning_storm = Some(QueuedLightningStorm {
            owner,
            target_rx,
            target_ry,
        });
        return true;
    }

    let state = LightningStormState {
        owner,
        target_rx,
        target_ry,
        deferment_remaining: rules.general.lightning_deferment,
        duration_remaining: rules.general.lightning_storm_duration,
        center_bolt_timer: rules.general.lightning_hit_delay,
        scatter_bolt_timer: rules.general.lightning_scatter_delay,
        last_bolt_rx: target_rx,
        last_bolt_ry: target_ry,
    };

    sim.lightning_storm = Some(state);

    // Sound event for EVA warning.
    sim.sound_events.push(SimSoundEvent::SuperWeaponLaunched {
        owner,
        rx: target_rx,
        ry: target_ry,
    });

    log::info!(
        "Lightning Storm started at ({}, {}) by '{}', deferment={} duration={}",
        target_rx,
        target_ry,
        sim.interner.resolve(owner),
        rules.general.lightning_deferment,
        rules.general.lightning_storm_duration,
    );

    true
}

/// Process the active lightning storm for one tick.
/// Called from `tick_superweapons()` each tick.
pub fn process(sim: &mut Simulation, rules: &RuleSet) {
    let Some(ref mut storm) = sim.lightning_storm else {
        return;
    };

    // Phase 1: deferment countdown.
    if storm.deferment_remaining > 0 {
        storm.deferment_remaining -= 1;
        return;
    }

    // Phase 2: active storm — decrement duration.
    storm.duration_remaining -= 1;
    if storm.duration_remaining <= 0 {
        log::info!("Lightning Storm ended");
        sim.lightning_storm = None;
        // Activate queued storm if one is waiting.
        if let Some(queued) = sim.queued_lightning_storm.take() {
            log::info!("Activating queued Lightning Storm");
            start(sim, rules, queued.owner, queued.target_rx, queued.target_ry);
        }
        return;
    }

    // Extract storm fields for bolt generation (avoid borrow conflict).
    let target_rx = storm.target_rx;
    let target_ry = storm.target_ry;
    let last_rx = storm.last_bolt_rx;
    let last_ry = storm.last_bolt_ry;
    let owner = storm.owner;

    // Center bolt
    storm.center_bolt_timer -= 1;
    let spawn_center = storm.center_bolt_timer <= 0;
    if spawn_center {
        storm.center_bolt_timer = rules.general.lightning_hit_delay;
    }

    // Scatter bolt
    storm.scatter_bolt_timer -= 1;
    let spawn_scatter = storm.scatter_bolt_timer <= 0;
    if spawn_scatter {
        storm.scatter_bolt_timer = rules.general.lightning_scatter_delay;
    }

    let spread = rules.general.lightning_cell_spread;
    let separation = rules.general.lightning_separation;

    if spawn_center {
        spawn_bolt(sim, rules, target_rx, target_ry, owner);
    }

    if spawn_scatter {
        let (rx, ry) = pick_scatter_cell(
            sim, target_rx, target_ry, last_rx, last_ry, spread, separation,
        );
        spawn_bolt(sim, rules, rx, ry, owner);
        // Update last bolt position on the storm state.
        if let Some(ref mut storm) = sim.lightning_storm {
            storm.last_bolt_rx = rx;
            storm.last_bolt_ry = ry;
        }
    }
}

/// Pick a random cell within `spread` of the storm center, enforcing
/// `separation` manhattan distance from the last bolt.
fn pick_scatter_cell(
    sim: &mut Simulation,
    center_rx: u16,
    center_ry: u16,
    last_rx: u16,
    last_ry: u16,
    spread: i32,
    separation: i32,
) -> (u16, u16) {
    let diameter = (spread * 2 + 1) as u32;
    for _ in 0..MAX_SCATTER_RETRIES {
        // Random offset within [-spread, +spread] for both axes.
        let dx = sim.rng.next_range_u32(diameter) as i32 - spread;
        let dy = sim.rng.next_range_u32(diameter) as i32 - spread;
        let rx = (center_rx as i32 + dx).max(0) as u16;
        let ry = (center_ry as i32 + dy).max(0) as u16;

        // Check manhattan distance from last bolt.
        let manhattan = (rx as i32 - last_rx as i32).abs() + (ry as i32 - last_ry as i32).abs();
        if manhattan >= separation {
            return (rx, ry);
        }
    }
    // Fallback: use the last attempted position (avoids infinite loop).
    let dx = sim.rng.next_range_u32(diameter) as i32 - spread;
    let dy = sim.rng.next_range_u32(diameter) as i32 - spread;
    (
        (center_rx as i32 + dx).max(0) as u16,
        (center_ry as i32 + dy).max(0) as u16,
    )
}

/// Spawn a single lightning bolt at the given cell: visual effect + area damage.
fn spawn_bolt(
    sim: &mut Simulation,
    rules: &RuleSet,
    rx: u16,
    ry: u16,
    owner: InternedId,
) {
    // 1. Pick a random bolt animation.
    let anim_idx = sim.rng.next_range_u32(BOLT_ANIMS.len() as u32) as usize;
    let anim_name = BOLT_ANIMS[anim_idx];
    let anim_iid = sim.interner.intern(anim_name);
    let frames = sim.effect_frame_counts.get(&anim_iid).copied().unwrap_or(20);

    sim.world_effects.push(WorldEffect {
        shp_name: anim_iid,
        rx,
        ry,
        z: 0,
        frame: 0,
        total_frames: frames,
        rate_ms: 67, // ~15 fps
        elapsed_ms: 0,
        translucent: true,
        delay_ms: 0,
    });

    // 2. Apply area damage via lightning warhead.
    let warhead_id = &rules.general.lightning_warhead;
    if let Some(warhead) = rules.warhead(warhead_id) {
        let owner_str = sim.interner.resolve(owner).to_string();
        let hits = crate::sim::combat::combat_aoe::apply_aoe_damage(
            &sim.entities,
            rx,
            ry,
            rules.general.lightning_damage,
            warhead,
            rules,
            &sim.interner,
            &owner_str,
        );

        // Apply damage to entities.
        for (stable_id, damage) in hits {
            if let Some(entity) = sim.entities.get_mut(stable_id) {
                entity.health.current = entity.health.current.saturating_sub(damage);
            }
        }
    } else {
        log::warn!("Lightning warhead '{}' not found in rules", warhead_id);
    }

    // 3. Sound event for the bolt strike.
    sim.sound_events.push(SimSoundEvent::SuperWeaponStrike { rx, ry });
}
