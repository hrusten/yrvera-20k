//! Passenger/transport system — boarding, unloading, and cargo tracking.
//!
//! Handles infantry entering transports (Passengers>0), building garrisons
//! (CanBeOccupied=yes), IFV weapon swapping (Gunner=yes), and passenger
//! death on transport destruction.
//!
//! ## Original engine reference
//! The original engine uses a linked-list at offsets +0x1D0/+0x1CC for passenger
//! storage; we use Vec<u64> for simplicity.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on rules/ (ObjectType), sim/game_entity, sim/entity_store.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::rules::object_type::ObjectType;
use crate::rules::ruleset::RuleSet;
use crate::sim::components::OrderIntent;
use crate::sim::game_entity::GameEntity;
use crate::sim::intern::StringInterner;
use crate::sim::movement;
use crate::sim::world::Simulation;
use crate::util::fixed_math::ra2_speed_to_leptons_per_second;
use crate::util::lepton;

/// Passenger cargo state, attached as `Option<PassengerCargo>` on transport entities.
///
/// Tracks which entities are currently inside this transport/garrison.
/// Passengers are stored as a Vec of stable_ids for deterministic ordering.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PassengerCargo {
    /// Stable IDs of entities currently inside, in boarding order (FIFO unload).
    pub passengers: Vec<u64>,
    /// Maximum passenger count (from Passengers= or MaxNumberOccupants= in rules.ini).
    pub capacity: u32,
    /// Maximum Size= of individual passenger allowed (from SizeLimit= in rules.ini).
    /// 0 means no size restriction.
    pub size_limit: u32,
    /// Total Size units currently occupied (sum of passenger Size= values).
    pub total_size: u32,
    /// Round-robin index for garrison fire — which occupant fires next.
    /// Matches gamemd BuildingClass+0x69C (CurrentFireIdx). Init 0, advanced
    /// by garrison combat after each shot: `(idx + 1) % occupant_count`.
    pub garrison_fire_index: u8,
}

impl PassengerCargo {
    pub fn new(capacity: u32, size_limit: u32) -> Self {
        Self {
            passengers: Vec::new(),
            capacity,
            size_limit,
            total_size: 0,
            garrison_fire_index: 0,
        }
    }

    /// Number of passengers currently inside.
    pub fn count(&self) -> u32 {
        self.passengers.len() as u32
    }

    /// Whether the transport has room for a passenger of the given size.
    pub fn can_accept(&self, passenger_size: u32) -> bool {
        self.count() < self.capacity && (self.size_limit == 0 || passenger_size <= self.size_limit)
    }

    /// Add a passenger. Returns false if full or too large.
    pub fn board(&mut self, stable_id: u64, passenger_size: u32) -> bool {
        if !self.can_accept(passenger_size) {
            return false;
        }
        self.passengers.push(stable_id);
        self.total_size += passenger_size;
        true
    }

    /// Remove a specific passenger. Returns true if found and removed.
    pub fn disembark(&mut self, stable_id: u64, passenger_size: u32) -> bool {
        if let Some(pos) = self.passengers.iter().position(|&id| id == stable_id) {
            self.passengers.remove(pos);
            self.total_size = self.total_size.saturating_sub(passenger_size);
            true
        } else {
            false
        }
    }

    /// Remove and return the first passenger (FIFO unload order).
    pub fn unload_first(&mut self) -> Option<u64> {
        if self.passengers.is_empty() {
            None
        } else {
            let id = self.passengers.remove(0);
            // total_size is corrected by the caller who knows the passenger's size
            Some(id)
        }
    }

    /// Is the cargo hold empty?
    pub fn is_empty(&self) -> bool {
        self.passengers.is_empty()
    }
}

/// Boarding intent phase — tracks a passenger's approach to a transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BoardingPhase {
    /// Moving toward the transport cell.
    Approach,
    /// Adjacent to transport, entering this tick.
    Entering,
}

/// Passenger/transport role for an entity. Replaces three separate Option fields
/// with a single enum that makes invalid states unrepresentable.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PassengerRole {
    /// Entity has no passenger/transport role. Most entities are this.
    None,
    /// Entity is a transport or garrisonable building that can hold passengers.
    Transport { cargo: PassengerCargo },
    /// Entity is approaching a transport to board it.
    Boarding {
        target_transport_id: u64,
        phase: BoardingPhase,
    },
    /// Entity is inside a transport (hidden from map, not targetable).
    Inside { transport_id: u64 },
}

impl PassengerRole {
    /// Returns the cargo hold if this entity is a transport.
    pub fn cargo(&self) -> Option<&PassengerCargo> {
        match self {
            Self::Transport { cargo } => Some(cargo),
            _ => Option::None,
        }
    }

    /// Returns a mutable reference to the cargo hold if this entity is a transport.
    pub fn cargo_mut(&mut self) -> Option<&mut PassengerCargo> {
        match self {
            Self::Transport { cargo } => Some(cargo),
            _ => Option::None,
        }
    }

    /// Returns the transport ID if this entity is inside one.
    pub fn inside_transport_id(&self) -> Option<u64> {
        match self {
            Self::Inside { transport_id } => Some(*transport_id),
            _ => Option::None,
        }
    }

    /// True if entity is inside a transport (hidden from map).
    pub fn is_inside_transport(&self) -> bool {
        matches!(self, Self::Inside { .. })
    }

    /// True if entity is a transport/garrison with a cargo hold.
    pub fn is_transport(&self) -> bool {
        matches!(self, Self::Transport { .. })
    }
}

/// Check whether a passenger entity can enter a specific transport.
///
/// Validates: alive, not already transported, owner compatibility,
/// size fits, transport has room.  For garrison buildings
/// (`CanBeOccupied=yes`) additional checks apply: `Occupier=yes`
/// required on the infantry, and the building must not be at red health.
pub fn can_enter_transport(
    passenger: &GameEntity,
    transport: &GameEntity,
    passenger_obj: &ObjectType,
    transport_obj: &ObjectType,
    cargo: &PassengerCargo,
    condition_red_x1000: i64, // pre-scaled integer (0.25 → 250) for deterministic comparison
    interner: &StringInterner,
) -> bool {
    // Must be alive, not dying, not already inside something
    if !passenger.is_alive() || passenger.dying {
        return false;
    }
    if passenger.passenger_role.is_inside_transport() {
        return false;
    }
    // Transport must be alive
    if !transport.is_alive() || transport.dying {
        return false;
    }
    // Owner check — garrison buildings allow neutral/special civilian buildings.
    if transport_obj.can_be_occupied {
        let same_owner = passenger.owner == transport.owner;
        let transport_owner_str = interner.resolve(transport.owner);
        let neutral_building = transport_owner_str.eq_ignore_ascii_case("neutral")
            || transport_owner_str.eq_ignore_ascii_case("special");
        if !same_owner && !neutral_building {
            return false;
        }
    } else {
        // Vehicle transports: strict same-owner.
        if passenger.owner != transport.owner {
            return false;
        }
    }
    // Garrison-specific: only Occupier infantry, and building must not be at red health.
    if transport_obj.can_be_occupied {
        if !passenger_obj.occupier {
            return false;
        }
        // Pure integer comparison: health_ratio <= condition_red
        // ↔ current * 1000 <= max * condition_red_x1000
        let current_x1000: i64 = transport.health.current as i64 * 1000;
        let threshold_x1000: i64 = transport.health.max.max(1) as i64 * condition_red_x1000;
        if current_x1000 <= threshold_x1000 {
            return false;
        }
    }
    // Size check
    cargo.can_accept(passenger_obj.size)
}

/// Maximum cell distance for a passenger to be considered "at" the transport.
/// Chebyshev distance in cells — 1 means same cell or adjacent.
const BOARD_DISTANCE: u32 = 1;

/// 8-directional neighbor offsets for finding unload exit cells.
const NEIGHBORS: [(i16, i16); 8] = [
    (0, -1),  // N
    (1, -1),  // NE
    (1, 0),   // E
    (1, 1),   // SE
    (0, 1),   // S
    (-1, 1),  // SW
    (-1, 0),  // W
    (-1, -1), // NW
];

/// Advance the passenger boarding/unloading system each tick.
///
/// Phase A: For entities with `boarding_state`, check if they arrived at
/// the transport's cell. If so, execute boarding. If the transport is
/// destroyed or full, cancel boarding.
///
/// Phase B: For transports with `OrderIntent::Unloading`, eject one
/// passenger per tick to an adjacent unoccupied cell. Clear the order
/// when all passengers are out.
/// Returns `true` if any entity's ownership changed this tick (garrison
/// transfer or revert), signalling that the sprite atlas needs a rebuild.
pub fn tick_passenger_system(sim: &mut Simulation, rules: &RuleSet) -> bool {
    let boarding_changed = tick_boarding(sim, rules);
    let unloading_changed = tick_unloading(sim, rules);
    boarding_changed || unloading_changed
}

/// Snapshot-then-mutate: process entities that are trying to board a transport.
/// Returns `true` if any entity ownership changed (garrison transfer).
fn tick_boarding(sim: &mut Simulation, rules: &RuleSet) -> bool {
    let mut ownership_changed = false;
    // Snapshot entities with Boarding role — must collect fully before mutating.
    let keys: Vec<u64> = sim.entities.keys_sorted();
    let boarding_snapshot: Vec<(u64, u64)> = keys
        .iter()
        .filter_map(|&id| {
            let e = sim.entities.get(id)?;
            if let PassengerRole::Boarding {
                target_transport_id,
                ..
            } = &e.passenger_role
            {
                Some((id, *target_transport_id))
            } else {
                Option::None
            }
        })
        .collect();

    for (pax_id, transport_id) in boarding_snapshot {
        // Check transport still exists and is alive.
        let transport_alive = sim
            .entities
            .get(transport_id)
            .is_some_and(|t| t.is_alive() && !t.dying);
        if !transport_alive {
            // Transport gone — cancel boarding.
            if let Some(e) = sim.entities.get_mut(pax_id) {
                e.passenger_role = PassengerRole::None;
            }
            continue;
        }

        // Get positions to check distance.
        let (pax_rx, pax_ry) = match sim.entities.get(pax_id) {
            Some(e) => (e.position.rx, e.position.ry),
            None => continue,
        };
        let (trx, try_) = match sim.entities.get(transport_id) {
            Some(e) => (e.position.rx, e.position.ry),
            None => continue,
        };

        // Chebyshev distance between passenger and transport.
        let dx = (pax_rx as i32 - trx as i32).unsigned_abs();
        let dy = (pax_ry as i32 - try_ as i32).unsigned_abs();
        let dist = dx.max(dy);

        if dist <= BOARD_DISTANCE {
            // Passenger has arrived — attempt boarding.
            let pax_type_str = sim
                .entities
                .get(pax_id)
                .map(|e| sim.interner.resolve(e.type_ref).to_string())
                .unwrap_or_default();
            let transport_type_str = sim
                .entities
                .get(transport_id)
                .map(|e| sim.interner.resolve(e.type_ref).to_string())
                .unwrap_or_default();

            let pax_size = rules.object(&pax_type_str).map(|obj| obj.size).unwrap_or(1);

            let transport_gunner = rules
                .object(&transport_type_str)
                .map(|obj| obj.gunner)
                .unwrap_or(false);

            let pax_ifv_mode = rules
                .object(&pax_type_str)
                .map(|obj| obj.ifv_mode)
                .unwrap_or(0);

            // Try to board.
            let boarded = sim
                .entities
                .get_mut(transport_id)
                .and_then(|t| t.passenger_role.cargo_mut())
                .is_some_and(|cargo| cargo.board(pax_id, pax_size));

            if boarded {
                // Garrison ownership transfer: when infantry boards a neutral/civilian
                // CanBeOccupied building, transfer building ownership to the infantry's
                // owner. Matches original engine's CheckAutoSellOrCivilian reconciliation
                // (we do it immediately rather than waiting one tick).
                let transport_can_be_occupied = rules
                    .object(&transport_type_str)
                    .map(|obj| obj.can_be_occupied)
                    .unwrap_or(false);
                if transport_can_be_occupied {
                    let pax_owner = sim.entities.get(pax_id).map(|e| e.owner);
                    if let Some(new_owner) = pax_owner {
                        if let Some(t) = sim.entities.get(transport_id) {
                            let t_owner_str = sim.interner.resolve(t.owner);
                            let is_neutral = t_owner_str.eq_ignore_ascii_case("neutral")
                                || t_owner_str.eq_ignore_ascii_case("special");
                            if is_neutral {
                                if let Some(t) = sim.entities.get_mut(transport_id) {
                                    // Save original owner before first transfer so we can
                                    // revert when the last occupant exits.
                                    if t.garrison_original_owner.is_none() {
                                        t.garrison_original_owner = Some(t.owner);
                                    }
                                    t.owner = new_owner;
                                    ownership_changed = true;
                                }
                            }
                        }
                    }
                }

                // Hide the passenger entity.
                if let Some(pax) = sim.entities.get_mut(pax_id) {
                    pax.passenger_role = PassengerRole::Inside { transport_id };
                    pax.movement_target = None;
                    pax.attack_target = None;
                    pax.order_intent = None;
                }
                // IFV weapon swap: if transport is Gunner=yes, set weapon index.
                if transport_gunner {
                    if let Some(t) = sim.entities.get_mut(transport_id) {
                        t.ifv_weapon_index = Some(pax_ifv_mode);
                    }
                }
            } else {
                // Transport full — cancel boarding.
                if let Some(pax) = sim.entities.get_mut(pax_id) {
                    pax.passenger_role = PassengerRole::None;
                }
            }
        }
        // If still approaching (movement_target present), let movement continue.
        // If movement finished but not close enough, the unit just stops.
    }
    ownership_changed
}

/// Process transports with `OrderIntent::Unloading` — eject one passenger per tick.
/// Returns `true` if any entity ownership changed (garrison revert to Neutral).
fn tick_unloading(sim: &mut Simulation, rules: &RuleSet) -> bool {
    let mut ownership_changed = false;
    // Snapshot transports that are unloading — must collect fully before mutating.
    let keys: Vec<u64> = sim.entities.keys_sorted();
    let unload_snapshot: Vec<u64> = keys
        .iter()
        .filter_map(|&id| {
            let e = sim.entities.get(id)?;
            if matches!(e.order_intent, Some(OrderIntent::Unloading)) {
                Some(id)
            } else {
                None
            }
        })
        .collect();

    for transport_id in unload_snapshot {
        let (trx, try_, tz) = match sim.entities.get(transport_id) {
            Some(e) => (e.position.rx, e.position.ry, e.position.z),
            None => continue,
        };

        // Collect occupied cell positions (skip transported/dying entities).
        let occupied_cells: Vec<(u16, u16)> = {
            let all_keys: Vec<u64> = sim.entities.keys_sorted();
            all_keys
                .iter()
                .filter_map(|&eid| {
                    let e = sim.entities.get(eid)?;
                    if !e.passenger_role.is_inside_transport() && !e.dying && e.is_alive() {
                        Some((e.position.rx, e.position.ry))
                    } else {
                        None
                    }
                })
                .collect()
        };

        // Find an adjacent free cell for the passenger to exit to.
        // Simple search: first unoccupied neighbor in 8 directions.
        let exit_cell = NEIGHBORS.iter().find_map(|&(dx, dy)| {
            let nx = trx as i16 + dx;
            let ny = try_ as i16 + dy;
            if nx < 0 || ny < 0 {
                return None;
            }
            let (nx, ny) = (nx as u16, ny as u16);
            let occupied = occupied_cells.iter().any(|&(ox, oy)| ox == nx && oy == ny);
            if occupied { None } else { Some((nx, ny)) }
        });

        let Some((exit_rx, exit_ry)) = exit_cell else {
            // No free cell — skip this tick, try again next tick.
            continue;
        };

        // Pop the first passenger from the cargo.
        let pax_id = sim
            .entities
            .get_mut(transport_id)
            .and_then(|t| t.passenger_role.cargo_mut())
            .and_then(|cargo| cargo.unload_first());

        let Some(pax_id) = pax_id else {
            // Cargo empty — clear unload order.
            if let Some(t) = sim.entities.get_mut(transport_id) {
                t.order_intent = None;
            }
            continue;
        };

        // Get passenger size for total_size bookkeeping.
        let pax_type_str = sim
            .entities
            .get(pax_id)
            .map(|e| sim.interner.resolve(e.type_ref).to_string())
            .unwrap_or_default();
        let pax_size = rules.object(&pax_type_str).map(|obj| obj.size).unwrap_or(1);

        // Adjust total_size on the cargo.
        if let Some(cargo) = sim
            .entities
            .get_mut(transport_id)
            .and_then(|t| t.passenger_role.cargo_mut())
        {
            cargo.total_size = cargo.total_size.saturating_sub(pax_size);
        }

        // Restore the passenger entity to the map.
        if let Some(pax) = sim.entities.get_mut(pax_id) {
            pax.passenger_role = PassengerRole::None;
            pax.position.rx = exit_rx;
            pax.position.ry = exit_ry;
            pax.position.z = tz;
            // Recalculate sub-cell offsets and screen position.
            let (sub_x, sub_y) = lepton::subcell_lepton_offset(pax.sub_cell);
            pax.position.sub_x = sub_x;
            pax.position.sub_y = sub_y;
            pax.position.refresh_screen_coords();
        }

        // Scatter: issue a short move to a random adjacent cell so ejected
        // infantry flee the building footprint (gamemd mission 0xF / Scatter).
        let scatter_speed = rules
            .object(&pax_type_str)
            .map(|obj| ra2_speed_to_leptons_per_second(obj.speed))
            .unwrap_or(ra2_speed_to_leptons_per_second(4));
        let start_dir = sim.rng.next_u32() as usize % 8;
        for i in 0..8 {
            let (dx, dy) = NEIGHBORS[(start_dir + i) % 8];
            let sx = exit_rx as i32 + dx as i32;
            let sy = exit_ry as i32 + dy as i32;
            if sx >= 0 && sy >= 0 {
                let dest = (sx as u16, sy as u16);
                let occupied = occupied_cells
                    .iter()
                    .any(|&(ox, oy)| ox == dest.0 && oy == dest.1);
                if !occupied {
                    movement::issue_direct_move(&mut sim.entities, pax_id, dest, scatter_speed);
                    break;
                }
            }
        }

        // If transport is Gunner=yes and now empty, revert weapon.
        let transport_type_str = sim
            .entities
            .get(transport_id)
            .map(|e| sim.interner.resolve(e.type_ref).to_string())
            .unwrap_or_default();
        let transport_gunner = rules
            .object(&transport_type_str)
            .map(|obj| obj.gunner)
            .unwrap_or(false);
        if transport_gunner {
            let is_empty = sim
                .entities
                .get(transport_id)
                .and_then(|t| t.passenger_role.cargo())
                .is_some_and(|c| c.is_empty());
            if is_empty {
                if let Some(t) = sim.entities.get_mut(transport_id) {
                    t.ifv_weapon_index = None;
                }
            }
        }

        // If cargo is now empty, clear the unload order and revert garrison ownership.
        let cargo_empty = sim
            .entities
            .get(transport_id)
            .and_then(|t| t.passenger_role.cargo())
            .is_some_and(|c| c.is_empty());
        if cargo_empty {
            // Garrison ownership revert: when last occupant leaves a CanBeOccupied
            // building, revert ownership to the building's original (pre-garrison)
            // owner. Matches original engine's CheckAutoSellOrCivilian which
            // transfers back to the Civilian house identified by side index.
            let is_garrison_building = rules
                .object(&transport_type_str)
                .map(|obj| obj.can_be_occupied)
                .unwrap_or(false);
            // Pre-intern "Neutral" as fallback for garrison ownership revert.
            let neutral_id = sim.interner.intern("Neutral");
            if let Some(t) = sim.entities.get_mut(transport_id) {
                t.order_intent = None;
                if is_garrison_building {
                    let revert_owner = t.garrison_original_owner.take().unwrap_or(neutral_id);
                    t.owner = revert_owner;
                    ownership_changed = true;
                }
            }
        }
    }
    ownership_changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cargo_new() {
        let cargo = PassengerCargo::new(5, 2);
        assert_eq!(cargo.capacity, 5);
        assert_eq!(cargo.size_limit, 2);
        assert_eq!(cargo.count(), 0);
        assert!(cargo.is_empty());
        assert_eq!(cargo.total_size, 0);
    }

    #[test]
    fn test_board_and_count() {
        let mut cargo = PassengerCargo::new(3, 0);
        assert!(cargo.board(100, 1));
        assert!(cargo.board(101, 1));
        assert!(cargo.board(102, 1));
        assert_eq!(cargo.count(), 3);
        assert!(!cargo.is_empty());
        assert_eq!(cargo.total_size, 3);
        // Full — cannot board more
        assert!(!cargo.board(103, 1));
        assert_eq!(cargo.count(), 3);
    }

    #[test]
    fn test_size_limit_rejection() {
        let mut cargo = PassengerCargo::new(5, 2);
        // Size 1 fits
        assert!(cargo.can_accept(1));
        assert!(cargo.board(100, 1));
        // Size 2 fits
        assert!(cargo.can_accept(2));
        assert!(cargo.board(101, 2));
        // Size 3 rejected by SizeLimit=2
        assert!(!cargo.can_accept(3));
        assert!(!cargo.board(102, 3));
        assert_eq!(cargo.count(), 2);
        assert_eq!(cargo.total_size, 3);
    }

    #[test]
    fn test_size_limit_zero_means_no_restriction() {
        let mut cargo = PassengerCargo::new(5, 0);
        assert!(cargo.can_accept(100)); // Any size fits
        assert!(cargo.board(1, 50));
        assert_eq!(cargo.total_size, 50);
    }

    #[test]
    fn test_disembark() {
        let mut cargo = PassengerCargo::new(5, 0);
        cargo.board(100, 1);
        cargo.board(101, 2);
        cargo.board(102, 1);

        assert!(cargo.disembark(101, 2));
        assert_eq!(cargo.count(), 2);
        assert_eq!(cargo.total_size, 2);
        assert_eq!(cargo.passengers, vec![100, 102]);

        // Disembarking non-existent ID returns false
        assert!(!cargo.disembark(999, 1));
    }

    #[test]
    fn test_unload_first_fifo() {
        let mut cargo = PassengerCargo::new(5, 0);
        cargo.board(100, 1);
        cargo.board(101, 1);
        cargo.board(102, 1);

        assert_eq!(cargo.unload_first(), Some(100));
        assert_eq!(cargo.unload_first(), Some(101));
        assert_eq!(cargo.unload_first(), Some(102));
        assert_eq!(cargo.unload_first(), None);
        assert!(cargo.is_empty());
    }

    #[test]
    fn test_can_accept_when_full() {
        let mut cargo = PassengerCargo::new(1, 1);
        assert!(cargo.can_accept(1));
        cargo.board(100, 1);
        assert!(!cargo.can_accept(1));
    }
}
