//! Superweapon system — charging, readiness, suspension, launch dispatch.
//!
//! Each player has a set of `SuperWeaponInstance`s, one per superweapon type
//! granted by their buildings. The system ticks after power (for suspend/resume)
//! and before combat. Lightning Storm is the first implemented launch handler.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on rules/, sim/power_system, sim/components.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

pub mod lightning_storm;

use crate::rules::ruleset::RuleSet;
use crate::rules::superweapon_type::SuperWeaponKind;
use crate::sim::intern::InternedId;
use crate::sim::world::Simulation;

/// Per-house, per-superweapon-type runtime state.
///
/// Tracks charging progress, readiness, and power suspension.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SuperWeaponInstance {
    /// Which SuperWeaponType this instance represents (interned ID of the INI section name).
    pub type_id: InternedId,
    /// Which house owns this instance.
    pub owner: InternedId,
    /// Whether the SW is granted (owning building exists and is alive).
    pub is_active: bool,
    /// Whether the SW is fully charged and ready to fire.
    pub is_ready: bool,
    /// Whether charging is paused due to low power.
    pub is_suspended: bool,
    /// Tick when charging began. -1 = timer stopped.
    pub charge_start_tick: i64,
    /// Total charge duration in ticks (may be adjusted on suspend/resume).
    pub charge_duration: i32,
    /// Charge/drain state: -1=N/A, 0=empty, 1=charged, 2=draining.
    /// Only used when UseChargeDrain=yes (Force Shield). -1 for all others.
    pub charge_drain_state: i32,
    /// Tick when the SW became ready. -1 = not ready yet.
    pub ready_tick: i64,
}

impl SuperWeaponInstance {
    /// Create a new inactive instance.
    pub fn new(type_id: InternedId, owner: InternedId) -> Self {
        Self {
            type_id,
            owner,
            is_active: false,
            is_ready: false,
            is_suspended: false,
            charge_start_tick: -1,
            charge_duration: 0,
            charge_drain_state: -1,
            ready_tick: -1,
        }
    }

    /// Activate (grant) this SW and start charging.
    pub fn activate(&mut self, recharge_frames: i32, current_tick: u64) {
        self.is_active = true;
        self.is_ready = false;
        self.is_suspended = false;
        self.charge_start_tick = current_tick as i64;
        self.charge_duration = recharge_frames;
        self.charge_drain_state = -1;
        self.ready_tick = -1;
    }

    /// Deactivate (revoke) this SW when the granting building is lost.
    pub fn deactivate(&mut self) {
        self.is_active = false;
        self.is_ready = false;
        self.is_suspended = false;
        self.charge_start_tick = -1;
        self.ready_tick = -1;
    }

    /// Suspend charging (low power). Saves remaining time.
    pub fn suspend(&mut self, current_tick: u64) {
        if self.charge_start_tick < 0 || self.is_suspended {
            return;
        }
        let elapsed = (current_tick as i64 - self.charge_start_tick) as i32;
        let remaining = (self.charge_duration - elapsed).max(0);
        self.charge_duration = remaining;
        self.charge_start_tick = -1;
        self.is_suspended = true;
    }

    /// Resume charging (power restored). Restarts timer with saved remaining.
    pub fn resume(&mut self, current_tick: u64) {
        if !self.is_suspended {
            return;
        }
        self.charge_start_tick = current_tick as i64;
        // charge_duration already holds the remaining frames from suspend()
        self.is_suspended = false;
    }

    /// Reset after firing — restart charge from full duration.
    pub fn reset_after_fire(&mut self, recharge_frames: i32, current_tick: u64) {
        self.is_ready = false;
        self.ready_tick = -1;
        self.charge_start_tick = current_tick as i64;
        self.charge_duration = recharge_frames;
    }

    /// Compute charge progress as 0.0–1.0 for sidebar display.
    /// Only valid when is_active and not is_ready.
    pub fn charge_progress(&self, current_tick: u64) -> f32 {
        if self.is_ready {
            return 1.0;
        }
        if self.charge_start_tick < 0 || self.charge_duration <= 0 {
            return 0.0;
        }
        let elapsed = (current_tick as i64 - self.charge_start_tick) as f32;
        (elapsed / self.charge_duration as f32).clamp(0.0, 1.0)
    }
}

/// View struct for sidebar display — no sim internals exposed.
#[derive(Debug, Clone)]
pub struct SuperWeaponView {
    pub type_id: InternedId,
    pub display_name: String,
    pub progress: f32,
    pub is_ready: bool,
    pub is_online: bool,
    pub sidebar_image: Option<String>,
    pub kind: SuperWeaponKind,
}

/// Query active superweapons for a specific owner (for sidebar rendering).
pub fn superweapon_views_for_owner(
    sim: &Simulation,
    rules: &RuleSet,
    owner: &InternedId,
) -> Vec<SuperWeaponView> {
    let Some(weapons) = sim.super_weapons.get(owner) else {
        return Vec::new();
    };
    let mut views = Vec::new();
    for (_, inst) in weapons {
        if !inst.is_active {
            continue;
        }
        let type_id_str = sim.interner.resolve(inst.type_id);
        let Some(sw_type) = rules.super_weapon(type_id_str) else {
            continue;
        };
        views.push(SuperWeaponView {
            type_id: inst.type_id,
            display_name: type_id_str.to_string(),
            progress: inst.charge_progress(sim.tick),
            is_ready: inst.is_ready,
            is_online: !inst.is_suspended,
            sidebar_image: sw_type.sidebar_image.clone(),
            kind: sw_type.kind,
        });
    }
    views
}

/// Tick all superweapon instances: advance charge timers, handle power
/// suspend/resume, and process active lightning storm.
pub fn tick_superweapons(sim: &mut Simulation, rules: &RuleSet) {
    let current_tick = sim.tick;

    // One-time initialization: scan all owners' buildings for SW grants.
    // Handles map-pre-placed buildings that bypass production placement hooks.
    if !sim.super_weapons_initialized {
        sim.super_weapons_initialized = true;
        let owners: Vec<InternedId> = sim
            .entities
            .values()
            .filter(|e| e.category == crate::map::entities::EntityCategory::Structure && !e.dying)
            .map(|e| e.owner)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        for owner_id in owners {
            refresh_super_weapons_for_owner(sim, rules, owner_id);
        }
    }

    // Phase 1: Charge/suspend lifecycle for all instances.
    // Collect owners to avoid borrow conflict on sim.super_weapons.
    let owners: Vec<InternedId> = sim.super_weapons.keys().copied().collect();
    for owner_id in owners {
        let is_low_power = sim
            .power_states
            .get(&owner_id)
            .map_or(false, |ps| ps.is_low_power);

        let Some(weapons) = sim.super_weapons.get_mut(&owner_id) else {
            continue;
        };
        for (_, inst) in weapons.iter_mut() {
            if !inst.is_active || inst.is_ready {
                continue;
            }
            let type_id_str = sim.interner.resolve(inst.type_id);
            let sw_powered = rules
                .super_weapon(type_id_str)
                .map_or(true, |sw| sw.is_powered);

            // Power suspend/resume
            if sw_powered {
                if is_low_power && !inst.is_suspended {
                    inst.suspend(current_tick);
                } else if !is_low_power && inst.is_suspended {
                    inst.resume(current_tick);
                }
            }

            // Charge advancement
            if inst.charge_start_tick >= 0 && !inst.is_suspended {
                let elapsed = current_tick as i64 - inst.charge_start_tick;
                if elapsed >= inst.charge_duration as i64 {
                    inst.is_ready = true;
                    inst.ready_tick = current_tick as i64;
                }
            }
        }
    }

    // Phase 2: Process active lightning storm.
    lightning_storm::process(sim, rules);
}

/// Refresh superweapon grants for a specific owner by scanning their buildings.
///
/// Call when a building is completed, sold, or destroyed. Activates new grants
/// and deactivates revoked ones.
pub fn refresh_super_weapons_for_owner(sim: &mut Simulation, rules: &RuleSet, owner: InternedId) {
    use std::collections::HashSet;

    let owner_str = sim.interner.resolve(owner).to_string();

    // Collect all SW type IDs (as strings) granted by living buildings of this owner.
    let mut granted_strs: Vec<String> = Vec::new();
    for (_, entity) in sim.entities.iter_sorted() {
        if entity.owner != owner {
            continue;
        }
        if entity.category != crate::map::entities::EntityCategory::Structure {
            continue;
        }
        if entity.dying {
            continue;
        }
        let type_str = sim.interner.resolve(entity.type_ref);
        if let Some(obj) = rules.object(type_str) {
            if let Some(ref sw_id) = obj.super_weapon {
                if rules.super_weapon(sw_id).is_some() {
                    granted_strs.push(sw_id.clone());
                }
            }
            if let Some(ref sw2_id) = obj.super_weapon2 {
                if rules.super_weapon(sw2_id).is_some() {
                    granted_strs.push(sw2_id.clone());
                }
            }
        }
    }

    // Intern all granted SW IDs.
    let granted: HashSet<InternedId> = granted_strs
        .iter()
        .map(|s| sim.interner.intern(s))
        .collect();

    let weapons = sim.super_weapons.entry(owner).or_default();

    // Activate new grants.
    for &sw_iid in &granted {
        if !weapons.contains_key(&sw_iid) {
            let sw_str = sim.interner.resolve(sw_iid).to_string();
            let recharge = rules
                .super_weapon(&sw_str)
                .map_or(4500, |sw| sw.recharge_time_frames);
            let mut inst = SuperWeaponInstance::new(sw_iid, owner);
            inst.activate(recharge, sim.tick);
            log::info!("SuperWeapon '{}' granted to '{}'", sw_str, owner_str);
            weapons.insert(sw_iid, inst);
        }
    }

    // Deactivate revoked (building destroyed, no other provides it).
    let revoke_ids: Vec<InternedId> = weapons
        .iter()
        .filter(|(sw_iid, inst)| inst.is_active && !granted.contains(sw_iid))
        .map(|(sw_iid, _)| *sw_iid)
        .collect();
    for sw_iid in revoke_ids {
        let sw_str = sim.interner.resolve(sw_iid).to_string();
        log::info!("SuperWeapon '{}' revoked from '{}'", sw_str, owner_str);
        if let Some(inst) = weapons.get_mut(&sw_iid) {
            inst.deactivate();
        }
    }
}
