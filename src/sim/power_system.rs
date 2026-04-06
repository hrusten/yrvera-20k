//! Per-player power state tracking and low-power effects.
//!
//! RA2's power system sums each building's `Power=` value per player:
//! positive values generate power (scaled by building health), negative
//! values consume power (always at full rated value). When drain exceeds
//! output the player enters "low power", disabling `Powered=yes` buildings
//! and slowing production.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on rules/ (ObjectType, GeneralRules) and sim/ (EntityStore).
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use std::collections::BTreeMap;

use crate::map::entities::EntityCategory;
use crate::rules::ruleset::RuleSet;
use crate::sim::entity_store::EntityStore;
use crate::sim::game_entity::GameEntity;
use crate::sim::intern::InternedId;

/// Per-player power state, updated each simulation tick.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PowerState {
    /// Sum of health-scaled positive `Power=` values for all owned buildings.
    pub total_output: i32,
    /// Sum of absolute negative `Power=` values (always full rated, regardless of health).
    pub total_drain: i32,
    /// True when `total_output < total_drain` (binary threshold).
    pub is_low_power: bool,
    /// Remaining power-blackout frames. While > 0, `total_output` is forced to 0.
    /// Set by spy infiltration of power plants AND by ForceShield superweapon launch.
    #[serde(rename = "spy_blackout_remaining")]
    pub power_blackout_remaining: u32,
    /// Milliseconds accumulated toward the next degradation damage tick.
    pub degradation_accum_ms: u32,
    /// Whether the player was in low-power state on the previous tick.
    /// Used to detect transitions for EVA voice events.
    pub was_low_power: bool,
    /// Sum of absolute `|Power=|` values from TypeClass for ALL owned buildings,
    /// regardless of health, construction state, or online status. Used by the
    /// sidebar power bar fill curve (asymptotic: `400 / (total + 400)`).
    pub theoretical_total_power: i32,
}

/// Events emitted when a player's power state transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PowerEvent {
    /// Player entered low-power state (drain now exceeds output).
    EnteredLowPower { owner: InternedId },
    /// Player's power was restored after a deficit.
    PowerRestored { owner: InternedId },
}

/// Recalculate power totals for a single owner from their buildings.
///
/// Power output scales with building health using integer arithmetic:
/// `output = Power * current_hp / max_hp` (rounds down, matching RA2).
/// Drain is always the full rated `|Power|` regardless of health.
/// If spy blackout is active, output is forced to 0.
fn recalculate_power_for_owner(
    state: &mut PowerState,
    entities: &EntityStore,
    rules: &RuleSet,
    owner_id: InternedId,
    interner: &crate::sim::intern::StringInterner,
) {
    let mut produced: i32 = 0;
    let mut drained: i32 = 0;
    // Theoretical total: sum of |Power=| from TypeClass for ALL buildings,
    // including those under construction. Used by the power bar fill curve.
    let mut theoretical: i32 = 0;

    for entity in entities.values() {
        if entity.category != EntityCategory::Structure || entity.owner != owner_id {
            continue;
        }
        let power = rules
            .object(interner.resolve(entity.type_ref))
            .map(|obj| obj.power)
            .unwrap_or(0);

        // Theoretical total includes ALL buildings regardless of state.
        theoretical += power.unsigned_abs() as i32;

        // Skip buildings still under construction for operational power calc.
        if entity.building_up.is_some() {
            continue;
        }
        if power > 0 {
            // Health-scaled output: integer division rounds down.
            let hp = entity.health.current as i32;
            let max_hp = entity.health.max.max(1) as i32;
            produced += power * hp / max_hp;
        } else if power < 0 {
            // Drain is always the full rated value.
            drained += power.saturating_abs();
        }
    }

    // Spy blackout forces output to zero.
    if state.power_blackout_remaining > 0 {
        produced = 0;
    }

    state.total_output = produced;
    state.total_drain = drained;
    state.is_low_power = produced < drained;
    state.theoretical_total_power = theoretical;
}

/// Main per-tick power system entry point.
///
/// For each player with structures: recalculates power totals, decrements
/// spy blackout timer, applies degradation damage during low power, and
/// returns transition events for EVA voice lines.
pub fn tick_power_states(
    power_states: &mut BTreeMap<InternedId, PowerState>,
    entities: &mut EntityStore,
    rules: &RuleSet,
    tick_ms: u32,
    interner: &crate::sim::intern::StringInterner,
) -> Vec<PowerEvent> {
    // Collect unique owners who have structures.
    let mut owners: Vec<InternedId> = Vec::new();
    for entity in entities.values() {
        if entity.category == EntityCategory::Structure && !owners.contains(&entity.owner) {
            owners.push(entity.owner);
        }
    }
    owners.sort();

    let mut events: Vec<PowerEvent> = Vec::new();

    for &owner_id in &owners {
        let state = power_states.entry(owner_id).or_default();

        // Save previous state for transition detection.
        state.was_low_power = state.is_low_power;

        // Decrement spy blackout timer (1 tick = 1 frame at game speed).
        if state.power_blackout_remaining > 0 {
            state.power_blackout_remaining = state.power_blackout_remaining.saturating_sub(1);
        }

        // Recalculate power totals with health scaling.
        recalculate_power_for_owner(state, entities, rules, owner_id, interner);

        // Detect transitions.
        if state.is_low_power && !state.was_low_power {
            events.push(PowerEvent::EnteredLowPower { owner: owner_id });
        } else if !state.is_low_power && state.was_low_power {
            events.push(PowerEvent::PowerRestored { owner: owner_id });
        }

        // Degradation damage: Powered=yes buildings take 1 HP damage periodically
        // during low power. Reset accumulator when power is restored.
        if !state.is_low_power {
            state.degradation_accum_ms = 0;
        }
    }

    // Apply degradation damage in a second pass to avoid borrow conflicts.
    // Convert f32 minutes → integer ms via fixed-point to avoid
    // platform-dependent float multiplication rounding.
    let rate_fixed =
        crate::util::fixed_math::SimFixed::saturating_from_num(rules.general.damage_delay_minutes);
    let delay_seconds = (rate_fixed * crate::util::fixed_math::SimFixed::from_num(60))
        .to_num::<i32>()
        .max(0);
    let damage_delay_ms = (delay_seconds as u32).saturating_mul(1000).max(1);
    let owners_needing_damage: Vec<InternedId> = owners
        .iter()
        .filter(|owner_id| {
            let Some(state) = power_states.get_mut(owner_id) else {
                return false;
            };
            if !state.is_low_power {
                return false;
            }
            state.degradation_accum_ms = state.degradation_accum_ms.saturating_add(tick_ms);
            if state.degradation_accum_ms >= damage_delay_ms {
                state.degradation_accum_ms -= damage_delay_ms;
                true
            } else {
                false
            }
        })
        .copied()
        .collect();

    for &owner_id in &owners_needing_damage {
        apply_degradation_damage(entities, rules, owner_id, interner);
    }

    events
}

/// Apply 1 HP degradation damage to all Powered=yes buildings with Power= <= 0
/// owned by the given player.
fn apply_degradation_damage(
    entities: &mut EntityStore,
    rules: &RuleSet,
    owner_id: InternedId,
    interner: &crate::sim::intern::StringInterner,
) {
    let ids: Vec<u64> = entities
        .values()
        .filter(|e| {
            e.category == EntityCategory::Structure
                && e.owner == owner_id
                && e.building_up.is_none()
                && rules
                    .object(interner.resolve(e.type_ref))
                    .is_some_and(|obj| obj.powered && obj.power <= 0)
        })
        .map(|e| e.stable_id)
        .collect();

    for id in ids {
        if let Some(entity) = entities.get_mut(id) {
            entity.health.current = entity.health.current.saturating_sub(1);
        }
    }
}

/// Check whether a specific building is functionally active (not disabled by low power).
///
/// Returns `false` if the owner is in low power AND the building has `Powered=yes`
/// AND consumes power (`Power= <= 0`). Power plants (positive `Power=`) are never
/// deactivated by low power.
pub fn is_building_powered(
    power_states: &BTreeMap<InternedId, PowerState>,
    rules: &RuleSet,
    entity: &GameEntity,
    interner: &crate::sim::intern::StringInterner,
) -> bool {
    if entity.category != EntityCategory::Structure {
        return true;
    }
    let Some(obj) = rules.object(interner.resolve(entity.type_ref)) else {
        return true;
    };
    // Power plants (positive Power=) are never deactivated.
    if obj.power > 0 {
        return true;
    }
    // Non-Powered buildings are never deactivated.
    if !obj.powered {
        return true;
    }
    // Check if owner is in low power.
    let is_low = power_states
        .get(&entity.owner)
        .is_some_and(|state| state.is_low_power);
    !is_low
}

/// Trigger a spy-infiltration power blackout for the target owner.
///
/// Sets `power_blackout_remaining` to the configured duration from `[General]`.
/// While active, the owner's power output is forced to 0.
pub fn trigger_spy_blackout(
    power_states: &mut BTreeMap<InternedId, PowerState>,
    owner_id: InternedId,
    duration_frames: u32,
) {
    let state = power_states.entry(owner_id).or_default();
    state.power_blackout_remaining = duration_frames;
}

/// Check if the given owner has at least one active (powered) radar building.
pub fn has_active_radar(
    entities: &EntityStore,
    power_states: &BTreeMap<InternedId, PowerState>,
    rules: &RuleSet,
    owner_id: InternedId,
    interner: &crate::sim::intern::StringInterner,
) -> bool {
    entities.values().any(|e| {
        e.category == EntityCategory::Structure
            && e.owner == owner_id
            && e.building_up.is_none()
            && rules
                .object(interner.resolve(e.type_ref))
                .is_some_and(|obj| obj.radar || obj.spy_sat)
            && is_building_powered(power_states, rules, e, interner)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::ini_parser::IniFile;
    use crate::sim::components::Health;
    use crate::sim::game_entity::GameEntity;
    use crate::sim::intern;

    fn test_interner() -> intern::StringInterner {
        intern::test_interner()
    }

    /// Build a minimal RuleSet with the given INI text.
    fn rules_from_ini(text: &str) -> RuleSet {
        let ini = IniFile::from_str(text);
        RuleSet::from_ini(&ini).expect("test rules")
    }

    fn make_building(id: u64, type_ref: &str, owner: &str, hp: u16, max_hp: u16) -> GameEntity {
        let mut e = GameEntity::test_default(id, type_ref, owner, 10, 10);
        e.category = EntityCategory::Structure;
        e.health = Health {
            current: hp,
            max: max_hp,
        };
        e
    }

    fn test_rules() -> RuleSet {
        rules_from_ini(
            "\
[BuildingTypes]
0=GAPOWR
1=NAPOWR
2=TESLA
3=GAPILE

[GAPOWR]
Power=200
Strength=600
Powered=no

[NAPOWR]
Power=150
Strength=400
Powered=no

[TESLA]
Power=-75
Strength=400
Powered=yes

[GAPILE]
Power=-10
Strength=500
Powered=yes

[General]
DamageDelay=1.0
SpyPowerBlackout=1000
MinLowPowerProductionSpeed=0.5
MaxLowPowerProductionSpeed=0.8
LowPowerPenaltyModifier=1.25
BuildSpeed=0.02
",
        )
    }

    #[test]
    fn test_health_scaled_output() {
        let rules = test_rules();
        let mut store = EntityStore::new();
        // Power plant at 50% HP should produce 50% output.
        store.insert(make_building(1, "GAPOWR", "Allies", 300, 600));
        // Barracks at any health always drains full amount.
        store.insert(make_building(2, "GAPILE", "Allies", 50, 500));

        let mut state = PowerState::default();
        let interner = test_interner();
        let allies = intern::test_intern("Allies");
        recalculate_power_for_owner(&mut state, &store, &rules, allies, &interner);

        assert_eq!(state.total_output, 100, "200 * 300/600 = 100");
        assert_eq!(state.total_drain, 10, "|-10| = 10");
        assert!(!state.is_low_power, "100 >= 10");
    }

    #[test]
    fn test_full_health_full_output() {
        let rules = test_rules();
        let mut store = EntityStore::new();
        store.insert(make_building(1, "GAPOWR", "Allies", 600, 600));

        let mut state = PowerState::default();
        let interner = test_interner();
        let allies = intern::test_intern("Allies");
        recalculate_power_for_owner(&mut state, &store, &rules, allies, &interner);

        assert_eq!(state.total_output, 200);
        assert_eq!(state.total_drain, 0);
        assert!(!state.is_low_power);
    }

    #[test]
    fn test_low_power_detection() {
        let rules = test_rules();
        let mut store = EntityStore::new();
        // Small power plant at low health.
        store.insert(make_building(1, "NAPOWR", "Soviet", 40, 400)); // 150 * 40/400 = 15
        // Tesla Coil drains 75.
        store.insert(make_building(2, "TESLA", "Soviet", 400, 400));

        let mut state = PowerState::default();
        let interner = test_interner();
        let soviet = intern::test_intern("Soviet");
        recalculate_power_for_owner(&mut state, &store, &rules, soviet, &interner);

        assert_eq!(state.total_output, 15, "150 * 40/400 = 15");
        assert_eq!(state.total_drain, 75);
        assert!(state.is_low_power, "15 < 75");
    }

    #[test]
    fn test_drain_always_full_regardless_of_health() {
        let rules = test_rules();
        let mut store = EntityStore::new();
        // Tesla Coil at 1 HP still drains full 75.
        store.insert(make_building(1, "TESLA", "Soviet", 1, 400));

        let mut state = PowerState::default();
        let interner = test_interner();
        let soviet = intern::test_intern("Soviet");
        recalculate_power_for_owner(&mut state, &store, &rules, soviet, &interner);

        assert_eq!(state.total_drain, 75, "drain is always full rated value");
    }

    #[test]
    fn test_spy_blackout_forces_zero_output() {
        let rules = test_rules();
        let mut store = EntityStore::new();
        store.insert(make_building(1, "GAPOWR", "Allies", 600, 600));
        store.insert(make_building(2, "GAPILE", "Allies", 500, 500));

        let mut state = PowerState::default();
        state.power_blackout_remaining = 100;
        let interner = test_interner();
        let allies = intern::test_intern("Allies");
        recalculate_power_for_owner(&mut state, &store, &rules, allies, &interner);

        assert_eq!(state.total_output, 0, "blackout forces output to 0");
        assert_eq!(state.total_drain, 10);
        assert!(state.is_low_power, "0 < 10 during blackout");
    }

    #[test]
    fn test_spy_blackout_timer_decrements() {
        let rules = test_rules();
        let mut store = EntityStore::new();
        store.insert(make_building(1, "GAPOWR", "Allies", 600, 600));

        let interner = test_interner();
        let allies = intern::test_intern("Allies");
        let mut states: BTreeMap<InternedId, PowerState> = BTreeMap::new();
        trigger_spy_blackout(&mut states, allies, 5);

        // Tick 5 times — each tick decrements by 1.
        for _ in 0..5 {
            tick_power_states(&mut states, &mut store, &rules, 16, &interner);
        }

        let state = states.get(&allies).expect("state should exist");
        assert_eq!(state.power_blackout_remaining, 0, "timer should reach 0");
        assert!(!state.is_low_power, "power restored after blackout");
    }

    #[test]
    fn test_power_transition_events() {
        let rules = test_rules();
        let mut store = EntityStore::new();
        // Start with just a tesla coil (drain=75, output=0) → immediate low power.
        store.insert(make_building(1, "TESLA", "Soviet", 400, 400));

        // Pre-intern all strings that will be used (including NAPOWR for the second
        // building added later) so the interner clone has everything.
        let soviet = intern::test_intern("Soviet");
        intern::test_intern("NAPOWR");
        let interner = test_interner();
        let mut states: BTreeMap<InternedId, PowerState> = BTreeMap::new();

        let events = tick_power_states(&mut states, &mut store, &rules, 16, &interner);
        assert!(
            events.contains(&PowerEvent::EnteredLowPower { owner: soviet }),
            "should detect entering low power"
        );

        // Add a power plant → should restore power.
        store.insert(make_building(2, "NAPOWR", "Soviet", 400, 400));
        let events = tick_power_states(&mut states, &mut store, &rules, 16, &interner);
        assert!(
            events.contains(&PowerEvent::PowerRestored { owner: soviet }),
            "should detect power restored"
        );
    }

    #[test]
    fn test_is_building_powered_for_generator() {
        let rules = test_rules();
        let allies = intern::test_intern("Allies");

        // Power plant (positive Power=) is never deactivated.
        let plant = make_building(1, "GAPOWR", "Allies", 600, 600);

        // Get interner AFTER all strings are interned (make_building interns type_ref).
        let interner = test_interner();
        let mut states: BTreeMap<InternedId, PowerState> = BTreeMap::new();
        states.insert(
            allies,
            PowerState {
                is_low_power: true,
                ..PowerState::default()
            },
        );

        assert!(
            is_building_powered(&states, &rules, &plant, &interner),
            "generators are never deactivated"
        );
    }

    #[test]
    fn test_is_building_powered_for_consumer_during_low_power() {
        let rules = test_rules();
        let soviet = intern::test_intern("Soviet");
        let tesla = make_building(1, "TESLA", "Soviet", 400, 400);

        // Get interner AFTER all strings are interned.
        let interner = test_interner();
        let mut states: BTreeMap<InternedId, PowerState> = BTreeMap::new();
        states.insert(
            soviet,
            PowerState {
                is_low_power: true,
                ..PowerState::default()
            },
        );

        assert!(
            !is_building_powered(&states, &rules, &tesla, &interner),
            "Powered=yes consumer deactivated during low power"
        );
    }

    #[test]
    fn test_is_building_powered_for_consumer_during_surplus() {
        let rules = test_rules();
        let soviet = intern::test_intern("Soviet");
        let tesla = make_building(1, "TESLA", "Soviet", 400, 400);

        // Get interner AFTER all strings are interned.
        let interner = test_interner();
        let mut states: BTreeMap<InternedId, PowerState> = BTreeMap::new();
        states.insert(
            soviet,
            PowerState {
                is_low_power: false,
                ..PowerState::default()
            },
        );

        assert!(
            is_building_powered(&states, &rules, &tesla, &interner),
            "consumer active during power surplus"
        );
    }

    #[test]
    fn test_degradation_damage() {
        let rules = test_rules();
        let mut store = EntityStore::new();
        // Tesla coil (Powered=yes, Power=-75) with no power plant → low power.
        store.insert(make_building(1, "TESLA", "Soviet", 100, 400));

        let interner = test_interner();
        let mut states: BTreeMap<InternedId, PowerState> = BTreeMap::new();

        // DamageDelay=1.0 minute = 60_000 ms. Tick at 16ms per tick.
        // Need 60_000/16 = 3750 ticks to trigger degradation.
        for _ in 0..3750 {
            tick_power_states(&mut states, &mut store, &rules, 16, &interner);
        }

        let entity = store.get(1).expect("entity should exist");
        assert!(
            entity.health.current < 100,
            "degradation should have reduced HP from 100, got {}",
            entity.health.current
        );
    }

    #[test]
    fn test_building_under_construction_excluded() {
        let rules = test_rules();
        let mut store = EntityStore::new();
        let mut plant = make_building(1, "GAPOWR", "Allies", 600, 600);
        plant.building_up = Some(crate::sim::components::BuildingUp {
            elapsed_ticks: 0,
            total_ticks: 30,
        });
        store.insert(plant);

        let mut state = PowerState::default();
        let interner = test_interner();
        let allies = intern::test_intern("Allies");
        recalculate_power_for_owner(&mut state, &store, &rules, allies, &interner);

        assert_eq!(
            state.total_output, 0,
            "building under construction produces no power"
        );
    }

    #[test]
    fn test_has_active_radar_with_power() {
        // Need a radar building in the rules.
        let rules = rules_from_ini(
            "\
[BuildingTypes]
0=GAAIRC
1=GAPOWR

[GAAIRC]
Radar=yes
Power=-50
Strength=600
Powered=yes

[GAPOWR]
Power=200
Strength=600

[General]
BuildSpeed=0.02
",
        );
        let mut store = EntityStore::new();
        store.insert(make_building(1, "GAAIRC", "Allies", 600, 600));
        store.insert(make_building(2, "GAPOWR", "Allies", 600, 600));

        let interner = test_interner();
        let allies = intern::test_intern("Allies");
        let mut states: BTreeMap<InternedId, PowerState> = BTreeMap::new();
        tick_power_states(&mut states, &mut store, &rules, 16, &interner);

        assert!(
            has_active_radar(&store, &states, &rules, allies, &interner),
            "radar should be active with sufficient power"
        );

        // Remove the power plant → low power → radar disabled.
        store.remove(2);
        tick_power_states(&mut states, &mut store, &rules, 16, &interner);

        assert!(
            !has_active_radar(&store, &states, &rules, allies, &interner),
            "radar should be disabled during low power"
        );
    }
}
