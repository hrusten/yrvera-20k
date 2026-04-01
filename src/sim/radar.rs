//! Radar availability detection and event system.
//!
//! In RA2/YR, the minimap (Radar Screen) only appears when the player owns a
//! powered radar-providing building. Buildings with `Radar=yes` or `SpySat=yes`
//! provide radar. Radar goes offline when power balance is negative (produced < drained).
//!
//! Also implements the radar event (ping) system: animated rectangles that flash
//! on the minimap when combat or other events occur. Spacebar cycles through the
//! last 8 events for camera jump.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on rules/, map/ (via entity components).
//! - NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::rules::radar_event_config::RadarEventConfig;
use crate::rules::ruleset::RuleSet;
use crate::sim::world::Simulation;
use crate::util::fixed_math::{SimFixed, int_distance_to_sim};
use std::collections::VecDeque;

/// Check if the given owner has at least one operational radar-providing building.
///
/// A building provides radar if its ObjectType has `Radar=yes` OR `SpySat=yes`
/// AND the building is powered (not disabled by low-power state).
pub fn has_radar_for_owner(sim: &Simulation, rules: &RuleSet, owner: &str) -> bool {
    let Some(owner_id) = sim.interner.get(owner) else {
        return false;
    };
    crate::sim::power_system::has_active_radar(
        &sim.entities,
        &sim.power_states,
        rules,
        owner_id,
        &sim.interner,
    )
}

/// Classification of radar events — determines ping color and EVA announcement.
///
/// RA2 defines 6 event types per ModEnc's Action 55 documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum RadarEventType {
    /// Generic combat event (unit took or dealt damage).
    Combat,
    /// Non-combat event (construction complete, unit ready, etc.).
    Noncombat,
    /// Airborne unit drop zone.
    Dropzone,
    /// Player's own base structure under attack.
    BaseUnderAttack,
    /// Player's harvester under attack.
    MinerUnderAttack,
    /// Enemy unit/building detected (first sighting).
    EnemyObjectSensed,
}

impl RadarEventType {
    /// Base RGB color for the radar ping by event type.
    pub fn color(self) -> [u8; 3] {
        match self {
            Self::Combat => [255, 255, 255],          // white
            Self::Noncombat => [255, 255, 0],         // yellow
            Self::Dropzone => [0, 255, 255],          // cyan
            Self::BaseUnderAttack => [255, 255, 255], // white
            Self::MinerUnderAttack => [255, 255, 0],  // yellow
            Self::EnemyObjectSensed => [255, 255, 0], // yellow
        }
    }
}

/// A single radar event with position and age tracking.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RadarEvent {
    pub event_type: RadarEventType,
    /// Isometric cell X coordinate where the event occurred.
    pub rx: u16,
    /// Isometric cell Y coordinate where the event occurred.
    pub ry: u16,
    /// Age of this event in milliseconds (increases each tick).
    pub age_ms: u32,
    /// Total lifetime in milliseconds before the event is removed.
    pub duration_ms: u32,
    /// Current rotation angle in radians (starts at π/4 = diamond orientation).
    pub rotation: f32,
    /// Rotation speed in radians per tick.
    pub rotation_speed: f32,
}

impl RadarEvent {
    /// Normalized age (0.0 = just spawned, 1.0 = about to expire).
    pub fn progress(&self) -> f32 {
        if self.duration_ms == 0 {
            return 1.0;
        }
        (self.age_ms as f32 / self.duration_ms as f32).clamp(0.0, 1.0)
    }

    /// Whether the event has exceeded its lifetime.
    pub fn expired(&self) -> bool {
        self.age_ms >= self.duration_ms
    }
}

/// Ring-buffer queue of recent radar events for minimap display + Spacebar cycling.
///
/// Maintains up to `max_events` entries (default 8). Old events are evicted
/// when the buffer is full. Spacebar cycles through the buffer for camera jump.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RadarEventQueue {
    events: VecDeque<RadarEvent>,
    max_events: usize,
    /// Index for Spacebar cycling (wraps around).
    cycle_index: usize,
    /// Minimum Euclidean distance (in cells) between two events before suppression.
    suppression_distance: SimFixed,
}

impl Default for RadarEventQueue {
    fn default() -> Self {
        Self {
            events: VecDeque::new(),
            max_events: 8,
            cycle_index: 0,
            suppression_distance: SimFixed::from_num(8),
        }
    }
}

impl RadarEventQueue {
    /// Create a queue configured from radar event rules.
    pub fn from_config(config: &RadarEventConfig) -> Self {
        Self {
            events: VecDeque::new(),
            max_events: config.max_events,
            cycle_index: 0,
            suppression_distance: SimFixed::from_num(8),
        }
    }

    /// Push a new radar event, suppressing duplicates that are too close.
    pub fn push(&mut self, event_type: RadarEventType, rx: u16, ry: u16, duration_ms: u32) {
        // Suppress if a recent event of same type is within suppression distance.
        let dominated: bool = self.events.iter().any(|e| {
            e.event_type == event_type
                && !e.expired()
                && cell_distance(e.rx, e.ry, rx, ry) < self.suppression_distance
        });
        if dominated {
            return;
        }

        let event = RadarEvent {
            event_type,
            rx,
            ry,
            age_ms: 0,
            duration_ms,
            rotation: std::f32::consts::FRAC_PI_4,
            rotation_speed: 0.05,
        };
        if self.events.len() >= self.max_events {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    /// Advance all events by `delta_ms` and remove expired ones.
    pub fn tick(&mut self, delta_ms: u32) {
        for event in self.events.iter_mut() {
            event.age_ms = event.age_ms.saturating_add(delta_ms);
            event.rotation += event.rotation_speed;
            if event.rotation > std::f32::consts::TAU {
                event.rotation -= std::f32::consts::TAU;
            }
        }
        self.events.retain(|e| !e.expired());
        if self.cycle_index >= self.events.len() && !self.events.is_empty() {
            self.cycle_index = 0;
        }
    }

    /// Cycle to the next event and return its position for camera jump.
    /// Returns None if the queue is empty.
    pub fn cycle_event(&mut self) -> Option<(u16, u16)> {
        if self.events.is_empty() {
            return None;
        }
        let idx: usize = self.cycle_index % self.events.len();
        let event = &self.events[idx];
        let pos = (event.rx, event.ry);
        self.cycle_index = (idx + 1) % self.events.len();
        Some(pos)
    }

    /// Iterate over all active (non-expired) events.
    pub fn iter(&self) -> impl Iterator<Item = &RadarEvent> {
        self.events.iter()
    }

    /// Number of active events.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the queue has no active events.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// Euclidean distance between two cells (deterministic fixed-point).
fn cell_distance(ax: u16, ay: u16, bx: u16, by: u16) -> SimFixed {
    let dx = ax as i32 - bx as i32;
    let dy = ay as i32 - by as i32;
    int_distance_to_sim(dx, dy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::entities::EntityCategory;
    use crate::rules::ini_parser::IniFile;
    use crate::sim::game_entity::GameEntity;
    use crate::sim::world::Simulation;

    fn make_rules_with_radar() -> RuleSet {
        let ini = IniFile::from_str(
            "[InfantryTypes]\n[VehicleTypes]\n[AircraftTypes]\n\
             [BuildingTypes]\n0=GARADR\n1=GAPOWR\n\
             [GARADR]\nName=Radar\nRadar=yes\nPower=-40\nFoundation=2x2\n\
             [GAPOWR]\nName=Power Plant\nPower=200\nFoundation=2x2\n",
        );
        RuleSet::from_ini(&ini).expect("radar test rules")
    }

    fn spawn_building(sim: &mut Simulation, id: u64, owner: &str, type_id: &str) {
        let owner_id = sim.interner.intern(owner);
        let type_ref = sim.interner.intern(type_id);
        let mut e = GameEntity::new(
            id,
            0,
            0,
            0,
            0,
            owner_id,
            crate::sim::components::Health {
                current: 100,
                max: 100,
            },
            type_ref,
            EntityCategory::Structure,
            0,
            5,
            false,
        );
        sim.entities.insert(e);
    }

    #[test]
    fn no_radar_without_building() {
        let sim = Simulation::new();
        let rules = make_rules_with_radar();
        assert!(!has_radar_for_owner(&sim, &rules, "Americans"));
    }

    #[test]
    fn radar_with_powered_building() {
        let mut sim = Simulation::new();
        let rules = make_rules_with_radar();
        // Spawn power plant (Power=200)
        spawn_building(&mut sim, 1, "Americans", "GAPOWR");
        // Spawn radar building (Power=-40)
        spawn_building(&mut sim, 2, "Americans", "GARADR");
        // Tick power states so cached state reflects the buildings.
        crate::sim::power_system::tick_power_states(
            &mut sim.power_states,
            &mut sim.entities,
            &rules,
            16,
            &sim.interner,
        );
        assert!(has_radar_for_owner(&sim, &rules, "Americans"));
    }

    #[test]
    fn no_radar_when_low_power() {
        let mut sim = Simulation::new();
        let rules = make_rules_with_radar();
        // Only radar building, no power plant — drained > produced
        spawn_building(&mut sim, 1, "Americans", "GARADR");
        // Tick power states so low-power is detected.
        crate::sim::power_system::tick_power_states(
            &mut sim.power_states,
            &mut sim.entities,
            &rules,
            16,
            &sim.interner,
        );
        assert!(!has_radar_for_owner(&sim, &rules, "Americans"));
    }

    #[test]
    fn radar_event_push_and_tick() {
        let mut queue = RadarEventQueue::default();
        queue.push(RadarEventType::Combat, 10, 20, 4000);
        assert_eq!(queue.len(), 1);

        // Tick 2 seconds — event still alive.
        queue.tick(2000);
        assert_eq!(queue.len(), 1);
        let event = queue.iter().next().expect("event");
        assert_eq!(event.age_ms, 2000);
        assert!(!event.expired());

        // Tick 2 more seconds — event expires.
        queue.tick(2000);
        assert!(queue.is_empty());
    }

    #[test]
    fn radar_event_suppression() {
        let mut queue = RadarEventQueue::default();
        queue.push(RadarEventType::Combat, 10, 20, 4000);
        // Same type, close by — suppressed.
        queue.push(RadarEventType::Combat, 11, 20, 4000);
        assert_eq!(queue.len(), 1);
        // Different type at same location — NOT suppressed.
        queue.push(RadarEventType::BaseUnderAttack, 10, 20, 4000);
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn radar_event_cycle() {
        let mut queue = RadarEventQueue::default();
        queue.push(RadarEventType::Combat, 10, 20, 4000);
        queue.push(RadarEventType::Noncombat, 50, 60, 4000);

        let first = queue.cycle_event();
        assert_eq!(first, Some((10, 20)));
        let second = queue.cycle_event();
        assert_eq!(second, Some((50, 60)));
        // Wraps around.
        let third = queue.cycle_event();
        assert_eq!(third, Some((10, 20)));
    }

    #[test]
    fn radar_event_max_capacity() {
        let mut queue = RadarEventQueue::default();
        for i in 0..12u16 {
            queue.push(RadarEventType::Combat, i * 10, 0, 4000);
        }
        // Max 8 events — oldest evicted.
        assert_eq!(queue.len(), 8);
        let first = queue.iter().next().expect("first");
        assert_eq!(first.rx, 40); // events 0-3 evicted
    }

    #[test]
    fn radar_event_progress() {
        let mut queue = RadarEventQueue::default();
        queue.push(RadarEventType::Combat, 10, 20, 4000);
        queue.tick(2000);
        let event = queue.iter().next().expect("event");
        assert!((event.progress() - 0.5).abs() < 0.01);
    }
}
