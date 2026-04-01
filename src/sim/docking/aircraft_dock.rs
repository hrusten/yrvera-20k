//! Aircraft ammo tracking and airfield docking system.
//!
//! Aircraft with finite `Ammo=` (from rules.ini) deplete ammo on each weapon
//! fire. When ammo reaches 0, the aircraft auto-returns to the nearest
//! helipad/airfield owned by the same player, descends, reloads, and
//! re-launches.
//!
//! Uses the two-phase snapshot pattern from `building_dock.rs` and follows the
//! `find_nearest_refinery()` approach from `miner_system.rs`.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on rules/, sim/components, sim/air_movement.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::map::entities::EntityCategory;
use crate::rules::ruleset::RuleSet;
use crate::sim::intern::InternedId;
use crate::sim::movement::locomotor::AirMovePhase;
use crate::sim::world::Simulation;

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// Per-entity aircraft ammo and docking state.
///
/// Present only on aircraft with `Ammo= >= 0` in rules.ini.
/// Entities with `Ammo=-1` (unlimited, the default) have `None`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AircraftAmmo {
    /// Current ammo count. 0 = depleted, triggers auto-return.
    pub current: i32,
    /// Maximum ammo (from `Ammo=` in rules.ini).
    pub max: i32,
    /// Current docking/reload lifecycle phase. None = normal flight.
    pub dock_phase: Option<AircraftDockPhase>,
    /// Stable ID of the target helipad/airfield building.
    pub target_airfield: Option<u64>,
    /// Ticks remaining until the next ammo point is restored.
    pub reload_timer: u32,
    /// Cooldown ticks before re-scanning for a helipad (prevents per-tick scans).
    pub rescan_cooldown: u16,
}

impl AircraftAmmo {
    /// Create a new ammo tracker with full ammo.
    pub fn new(max_ammo: i32) -> Self {
        Self {
            current: max_ammo,
            max: max_ammo,
            dock_phase: None,
            target_airfield: None,
            reload_timer: 0,
            rescan_cooldown: 0,
        }
    }
}

/// Docking lifecycle phases for aircraft returning to an airfield.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AircraftDockPhase {
    /// Flying toward the target airfield.
    ReturnToBase,
    /// At/near the airfield, waiting for a dock slot (FIFO queue).
    WaitForDock,
    /// Dock slot reserved, descending to land.
    Descending,
    /// On the ground, reloading ammo.
    Reloading,
    /// Fully reloaded, ascending to resume flight.
    Launching,
}

// ---------------------------------------------------------------------------
// Multi-slot dock reservations for airfields
// ---------------------------------------------------------------------------

/// Multi-slot dock reservation manager for airfields.
///
/// Unlike `DockReservations` (single occupant per building), airfields support
/// `NumberOfDocks` simultaneous occupants (e.g., 4 for Allied Air Force Command).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AirfieldDocks {
    /// Maps airfield stable_id → (occupied_count, max_slots).
    slots: BTreeMap<u64, (u8, u8)>,
    /// Maps airfield stable_id → FIFO queue of waiting aircraft stable_ids.
    queues: BTreeMap<u64, VecDeque<u64>>,
    /// Maps aircraft stable_id → airfield stable_id (reverse lookup for cancel).
    aircraft_to_airfield: BTreeMap<u64, u64>,
}

impl AirfieldDocks {
    /// Register an airfield with its max dock count.
    /// Called lazily when an aircraft first tries to dock.
    fn ensure_registered(&mut self, airfield_sid: u64, max_slots: u8) {
        self.slots.entry(airfield_sid).or_insert((0, max_slots));
    }

    /// Try to reserve a dock slot for `aircraft_sid` at `airfield_sid`.
    ///
    /// Returns `true` if the aircraft now occupies a slot (immediately granted).
    /// Returns `false` if all slots are full — the aircraft is enqueued.
    pub fn try_reserve(&mut self, airfield_sid: u64, aircraft_sid: u64, max_slots: u8) -> bool {
        self.ensure_registered(airfield_sid, max_slots);

        // Already docked here?
        if self.aircraft_to_airfield.get(&aircraft_sid) == Some(&airfield_sid) {
            return true;
        }

        let (occupied, max) = self.slots.get_mut(&airfield_sid).unwrap();
        if *occupied < *max {
            *occupied += 1;
            self.aircraft_to_airfield.insert(aircraft_sid, airfield_sid);
            return true;
        }

        // Full — enqueue.
        let queue = self.queues.entry(airfield_sid).or_default();
        if !queue.contains(&aircraft_sid) {
            queue.push_back(aircraft_sid);
        }
        false
    }

    /// Release a dock slot for `aircraft_sid`. Returns whether a queued
    /// aircraft was promoted (and its stable_id if so).
    pub fn release(&mut self, aircraft_sid: u64) -> Option<u64> {
        let Some(airfield_sid) = self.aircraft_to_airfield.remove(&aircraft_sid) else {
            return None;
        };
        if let Some((occupied, _)) = self.slots.get_mut(&airfield_sid) {
            *occupied = occupied.saturating_sub(1);
        }
        // Promote next from queue.
        if let Some(queue) = self.queues.get_mut(&airfield_sid) {
            if let Some(next) = queue.pop_front() {
                if let Some((occupied, _)) = self.slots.get_mut(&airfield_sid) {
                    *occupied += 1;
                }
                self.aircraft_to_airfield.insert(next, airfield_sid);
                return Some(next);
            }
        }
        None
    }

    /// Check if an airfield has at least one free dock slot.
    /// Does not modify state — read-only probe.
    pub fn has_free_slot(&self, airfield_sid: u64, max_slots: u8) -> bool {
        match self.slots.get(&airfield_sid) {
            Some((occupied, max)) => *occupied < *max,
            None => max_slots > 0, // Not yet registered = all slots free.
        }
    }

    /// Cancel an aircraft's reservation or queue position.
    pub fn cancel(&mut self, aircraft_sid: u64) {
        if let Some(airfield_sid) = self.aircraft_to_airfield.remove(&aircraft_sid) {
            if let Some((occupied, _)) = self.slots.get_mut(&airfield_sid) {
                *occupied = occupied.saturating_sub(1);
            }
            // Promote next from queue.
            if let Some(queue) = self.queues.get_mut(&airfield_sid) {
                if let Some(next) = queue.pop_front() {
                    if let Some((occupied, _)) = self.slots.get_mut(&airfield_sid) {
                        *occupied += 1;
                    }
                    self.aircraft_to_airfield.insert(next, airfield_sid);
                }
            }
        } else {
            // Remove from all queues (linear scan, but rare).
            for queue in self.queues.values_mut() {
                queue.retain(|&sid| sid != aircraft_sid);
            }
        }
    }

    /// Remove dead entities (aircraft or airfields) from all tracking.
    pub fn cleanup_dead(&mut self, alive: &BTreeSet<u64>) {
        // Remove dead airfields.
        self.slots.retain(|sid, _| alive.contains(sid));
        self.queues.retain(|sid, _| alive.contains(sid));

        // Remove dead aircraft from occupant tracking and free their slots.
        let dead_aircraft: Vec<u64> = self
            .aircraft_to_airfield
            .keys()
            .filter(|sid| !alive.contains(sid))
            .copied()
            .collect();
        for sid in dead_aircraft {
            self.release(sid);
        }

        // Remove dead aircraft from queues.
        for queue in self.queues.values_mut() {
            queue.retain(|sid| alive.contains(sid));
        }
    }
}

// ---------------------------------------------------------------------------
// Find nearest airfield
// ---------------------------------------------------------------------------

/// Chebyshev distance squared between two cell coordinates.
fn cell_dist_sq(ax: u16, ay: u16, bx: u16, by: u16) -> u32 {
    let dx = (ax as i32 - bx as i32).unsigned_abs();
    let dy = (ay as i32 - by as i32).unsigned_abs();
    dx * dx + dy * dy
}

/// Find the nearest same-owner airfield/helipad that accepts this aircraft type.
///
/// Checks: alive, same owner, `UnitReload=yes` or `Helipad=yes`, and the
/// aircraft's `Dock=` list includes the building's type_ref.
/// Returns `(stable_id, dock_cell_rx, dock_cell_ry)`.
fn find_nearest_airfield(
    sim: &Simulation,
    rules: &RuleSet,
    owner: InternedId,
    aircraft_type_id: InternedId,
    from: (u16, u16),
) -> Option<(u64, u16, u16)> {
    let aircraft_obj = rules.object(sim.interner.resolve(aircraft_type_id))?;
    let dock_list = &aircraft_obj.dock;
    if dock_list.is_empty() {
        return None;
    }

    let mut best: Option<(u64, u16, u16, u32)> = None;

    for entity in sim.entities.values() {
        if entity.category != EntityCategory::Structure {
            continue;
        }
        if entity.health.current == 0 || entity.dying {
            continue;
        }
        if entity.owner != owner {
            continue;
        }
        let entity_type_str = sim.interner.resolve(entity.type_ref);
        let Some(obj) = rules.object(entity_type_str) else {
            continue;
        };
        if !obj.unit_reload && !obj.helipad {
            continue;
        }
        // Aircraft's Dock= list must include this building's type_ref.
        if !dock_list
            .iter()
            .any(|d| d.eq_ignore_ascii_case(entity_type_str))
        {
            continue;
        }
        // Use building center as dock cell.
        let (w, h) = crate::sim::production::foundation_dimensions(&obj.foundation);
        let dock_rx = entity.position.rx + w / 2;
        let dock_ry = entity.position.ry + h / 2;
        let dist = cell_dist_sq(from.0, from.1, dock_rx, dock_ry);

        if best.is_none() || dist < best.unwrap().3 {
            best = Some((entity.stable_id, dock_rx, dock_ry, dist));
        }
    }

    best.map(|(sid, rx, ry, _)| (sid, rx, ry))
}

// ---------------------------------------------------------------------------
// Tick system
// ---------------------------------------------------------------------------

/// Chebyshev distance between two cell coordinates.
fn cell_distance(ax: u16, ay: u16, bx: u16, by: u16) -> u32 {
    let dx = (ax as i32 - bx as i32).unsigned_abs();
    let dy = (ay as i32 - by as i32).unsigned_abs();
    dx.max(dy)
}

/// Rescan cooldown in ticks before retrying helipad search after failure.
const RESCAN_COOLDOWN_TICKS: u16 = 60;

/// Advance aircraft ammo and docking systems for all entities.
///
/// Called once per tick from `advance_tick()`, after combat and building docks.
pub fn tick_aircraft_docks(sim: &mut Simulation, rules: &RuleSet) {
    // Phase 1: Cleanup dead entities from dock reservations.
    let alive: BTreeSet<u64> = sim.entities.keys_sorted().iter().copied().collect();
    sim.production.airfield_docks.cleanup_dead(&alive);

    // Phase 2: Snapshot all aircraft with aircraft_ammo.
    struct AircraftSnap {
        id: u64,
        owner: InternedId,
        type_ref: InternedId,
        rx: u16,
        ry: u16,
        current_ammo: i32,
        max_ammo: i32,
        dock_phase: Option<AircraftDockPhase>,
        target_airfield: Option<u64>,
        reload_timer: u32,
        rescan_cooldown: u16,
        has_attack_target: bool,
        air_phase: Option<AirMovePhase>,
        has_movement_target: bool,
    }

    let snapshots: Vec<AircraftSnap> = sim
        .entities
        .values()
        .filter_map(|e| {
            let ammo = e.aircraft_ammo.as_ref()?;
            // Skip aircraft managed by the mission system.
            if e.aircraft_mission.is_some() {
                return None;
            }
            let air_phase = e.locomotor.as_ref().map(|l| l.air_phase);
            Some(AircraftSnap {
                id: e.stable_id,
                owner: e.owner,
                type_ref: e.type_ref,
                rx: e.position.rx,
                ry: e.position.ry,
                current_ammo: ammo.current,
                max_ammo: ammo.max,
                dock_phase: ammo.dock_phase,
                target_airfield: ammo.target_airfield,
                reload_timer: ammo.reload_timer,
                rescan_cooldown: ammo.rescan_cooldown,
                has_attack_target: e.attack_target.is_some(),
                air_phase,
                has_movement_target: e.movement_target.is_some(),
            })
        })
        .collect();

    if snapshots.is_empty() {
        return;
    }

    // Phase 3: Process each aircraft through the ammo/dock state machine.
    struct AircraftMutation {
        id: u64,
        new_dock_phase: Option<Option<AircraftDockPhase>>, // Some(None) = clear
        new_target_airfield: Option<Option<u64>>,
        new_reload_timer: Option<u32>,
        new_rescan_cooldown: Option<u16>,
        restore_ammo: i32,
        clear_attack_target: bool,
        set_air_phase: Option<AirMovePhase>,
        air_move_to: Option<(u16, u16)>,
        clear_movement: bool,
    }

    let reload_ticks = rules.general.reload_rate_ticks;
    let mut mutations: Vec<AircraftMutation> = Vec::new();

    for snap in &snapshots {
        let mut m = AircraftMutation {
            id: snap.id,
            new_dock_phase: None,
            new_target_airfield: None,
            new_reload_timer: None,
            new_rescan_cooldown: None,
            restore_ammo: 0,
            clear_attack_target: false,
            set_air_phase: None,
            air_move_to: None,
            clear_movement: false,
        };

        match snap.dock_phase {
            None => {
                // Trigger auto-return when ammo depleted and not attacking.
                if snap.current_ammo <= 0 && !snap.has_attack_target {
                    if snap.rescan_cooldown > 0 {
                        m.new_rescan_cooldown = Some(snap.rescan_cooldown.saturating_sub(1));
                        mutations.push(m);
                        continue;
                    }
                    if let Some((af_sid, af_rx, af_ry)) = find_nearest_airfield(
                        sim,
                        rules,
                        snap.owner,
                        snap.type_ref,
                        (snap.rx, snap.ry),
                    ) {
                        m.new_dock_phase = Some(Some(AircraftDockPhase::ReturnToBase));
                        m.new_target_airfield = Some(Some(af_sid));
                        m.clear_attack_target = true;
                        m.air_move_to = Some((af_rx, af_ry));
                    } else {
                        // No airfield available — set cooldown and retry later.
                        m.new_rescan_cooldown = Some(RESCAN_COOLDOWN_TICKS);
                    }
                }
            }

            Some(AircraftDockPhase::ReturnToBase) => {
                // Verify target airfield still exists.
                let airfield_ok = snap.target_airfield.and_then(|af_sid| {
                    let af = sim.entities.get(af_sid)?;
                    if af.health.current == 0 || af.dying {
                        return None;
                    }
                    let obj = rules.object(sim.interner.resolve(af.type_ref))?;
                    let (w, h) = crate::sim::production::foundation_dimensions(&obj.foundation);
                    Some((af_sid, af.position.rx + w / 2, af.position.ry + h / 2))
                });

                match airfield_ok {
                    Some((_af_sid, dock_rx, dock_ry)) => {
                        let dist = cell_distance(snap.rx, snap.ry, dock_rx, dock_ry);
                        if dist <= 2 {
                            m.new_dock_phase = Some(Some(AircraftDockPhase::WaitForDock));
                            m.clear_movement = true;
                        } else if !snap.has_movement_target {
                            // Re-issue move if we lost movement target.
                            m.air_move_to = Some((dock_rx, dock_ry));
                        }
                    }
                    None => {
                        // Airfield destroyed — find another.
                        if let Some(old_sid) = snap.target_airfield {
                            sim.production.airfield_docks.cancel(old_sid);
                        }
                        if let Some((af_sid, af_rx, af_ry)) = find_nearest_airfield(
                            sim,
                            rules,
                            snap.owner,
                            snap.type_ref,
                            (snap.rx, snap.ry),
                        ) {
                            m.new_target_airfield = Some(Some(af_sid));
                            m.air_move_to = Some((af_rx, af_ry));
                        } else {
                            // No airfield — hover and rescan later.
                            m.new_dock_phase = Some(None);
                            m.new_target_airfield = Some(None);
                            m.new_rescan_cooldown = Some(RESCAN_COOLDOWN_TICKS);
                        }
                    }
                }
            }

            Some(AircraftDockPhase::WaitForDock) => {
                let Some(af_sid) = snap.target_airfield else {
                    m.new_dock_phase = Some(None);
                    mutations.push(m);
                    continue;
                };
                let max_slots = sim
                    .entities
                    .get(af_sid)
                    .and_then(|af| rules.object(sim.interner.resolve(af.type_ref)))
                    .map(|obj| obj.number_of_docks.max(1))
                    .unwrap_or(1);

                if sim
                    .production
                    .airfield_docks
                    .try_reserve(af_sid, snap.id, max_slots)
                {
                    m.new_dock_phase = Some(Some(AircraftDockPhase::Descending));
                    m.set_air_phase = Some(AirMovePhase::Descending);
                    m.clear_movement = true;
                }
                // Otherwise keep waiting.
            }

            Some(AircraftDockPhase::Descending) => {
                // Wait for air_movement to bring altitude to 0 (Landed).
                if snap.air_phase == Some(AirMovePhase::Landed) {
                    m.new_dock_phase = Some(Some(AircraftDockPhase::Reloading));
                    m.new_reload_timer = Some(reload_ticks);
                }
            }

            Some(AircraftDockPhase::Reloading) => {
                let timer = snap.reload_timer.saturating_sub(1);
                if timer == 0 {
                    // Restore one ammo point.
                    m.restore_ammo = 1;
                    let new_ammo = snap.current_ammo + 1;
                    if new_ammo >= snap.max_ammo {
                        // Fully reloaded — launch.
                        m.new_dock_phase = Some(Some(AircraftDockPhase::Launching));
                        m.set_air_phase = Some(AirMovePhase::Ascending);
                        // Release dock slot.
                        sim.production.airfield_docks.release(snap.id);
                    } else {
                        m.new_reload_timer = Some(reload_ticks);
                    }
                } else {
                    m.new_reload_timer = Some(timer);
                }
            }

            Some(AircraftDockPhase::Launching) => {
                // Wait for air_movement to reach cruising altitude.
                if snap.air_phase == Some(AirMovePhase::Cruising) {
                    m.new_dock_phase = Some(None);
                    m.new_target_airfield = Some(None);
                }
            }
        }

        mutations.push(m);
    }

    // Phase 4: Apply mutations.
    for m in &mutations {
        if let Some(entity) = sim.entities.get_mut(m.id) {
            if let Some(ref mut ammo) = entity.aircraft_ammo {
                if let Some(new_phase) = m.new_dock_phase {
                    ammo.dock_phase = new_phase;
                }
                if let Some(new_af) = m.new_target_airfield {
                    ammo.target_airfield = new_af;
                }
                if let Some(new_timer) = m.new_reload_timer {
                    ammo.reload_timer = new_timer;
                }
                if let Some(new_cooldown) = m.new_rescan_cooldown {
                    ammo.rescan_cooldown = new_cooldown;
                }
                ammo.current = (ammo.current + m.restore_ammo).min(ammo.max);
            }
            if m.clear_attack_target {
                entity.attack_target = None;
            }
            if let Some(phase) = m.set_air_phase {
                if let Some(ref mut loco) = entity.locomotor {
                    loco.air_phase = phase;
                }
            }
            if m.clear_movement {
                entity.movement_target = None;
            }
        }
    }

    // Phase 5: Issue air move commands (must be done after mutations to avoid
    // borrow conflicts with entities).
    let air_moves: Vec<(u64, u16, u16)> = mutations
        .iter()
        .filter_map(|m| m.air_move_to.map(|(rx, ry)| (m.id, rx, ry)))
        .collect();
    for (id, rx, ry) in air_moves {
        let speed = sim
            .entities
            .get(id)
            .and_then(|e| {
                let obj = rules.object(sim.interner.resolve(e.type_ref))?;
                Some(crate::util::fixed_math::SimFixed::from_num(
                    obj.speed.max(1),
                ))
            })
            .unwrap_or(crate::util::fixed_math::SimFixed::from_num(8));
        crate::sim::movement::air_movement::issue_air_move_command(
            &mut sim.entities,
            id,
            (rx, ry),
            speed,
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn airfield_docks_basic_reserve() {
        let mut docks = AirfieldDocks::default();
        // Airfield 100 has 2 slots.
        assert!(docks.try_reserve(100, 1, 2));
        assert!(docks.try_reserve(100, 2, 2));
        // 3rd aircraft should queue.
        assert!(!docks.try_reserve(100, 3, 2));
        assert_eq!(docks.queues[&100].len(), 1);
    }

    #[test]
    fn airfield_docks_release_promotes() {
        let mut docks = AirfieldDocks::default();
        docks.try_reserve(100, 1, 1);
        docks.try_reserve(100, 2, 1); // queued
        docks.try_reserve(100, 3, 1); // queued
        let promoted = docks.release(1);
        assert_eq!(promoted, Some(2));
        assert!(docks.aircraft_to_airfield.contains_key(&2));
    }

    #[test]
    fn airfield_docks_cancel() {
        let mut docks = AirfieldDocks::default();
        docks.try_reserve(100, 1, 2);
        docks.try_reserve(100, 2, 2);
        docks.try_reserve(100, 3, 2); // queued
        docks.cancel(1);
        // Slot freed, queued #3 should be promoted.
        assert!(docks.aircraft_to_airfield.contains_key(&3));
        assert_eq!(docks.slots[&100].0, 2); // still 2 occupied
    }

    #[test]
    fn airfield_docks_cleanup_dead() {
        let mut docks = AirfieldDocks::default();
        docks.try_reserve(100, 1, 2);
        docks.try_reserve(100, 2, 2);
        docks.try_reserve(100, 3, 2); // queued
        let alive: BTreeSet<u64> = [100, 2, 3].into_iter().collect();
        docks.cleanup_dead(&alive);
        // Aircraft 1 died — slot freed, #3 promoted.
        assert!(!docks.aircraft_to_airfield.contains_key(&1));
        assert!(docks.aircraft_to_airfield.contains_key(&3));
    }

    #[test]
    fn airfield_docks_idempotent_reserve() {
        let mut docks = AirfieldDocks::default();
        assert!(docks.try_reserve(100, 1, 2));
        assert!(docks.try_reserve(100, 1, 2)); // already occupying
        assert_eq!(docks.slots[&100].0, 1); // still only 1 occupied
    }

    #[test]
    fn aircraft_ammo_new() {
        let ammo = AircraftAmmo::new(3);
        assert_eq!(ammo.current, 3);
        assert_eq!(ammo.max, 3);
        assert!(ammo.dock_phase.is_none());
    }
}
