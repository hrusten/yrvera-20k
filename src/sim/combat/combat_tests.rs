//! Tests for the combat system — weapon firing, damage, and entity death.
//!
//! Extracted from combat.rs to keep it under the 400-line limit.

use std::collections::BTreeMap;

use super::*;
use crate::rules::ini_parser::IniFile;
use crate::rules::ruleset::RuleSet;
use crate::sim::components::Health;
use crate::sim::entity_store::EntityStore;
use crate::sim::game_entity::GameEntity;
use crate::sim::intern::{InternedId, test_intern, test_interner};
use crate::sim::miner::{ResourceNode, ResourceType};
use crate::sim::power_system::PowerState;
use crate::sim::vision::FogState;

/// Build a minimal RuleSet for combat testing.
fn test_rules() -> RuleSet {
    let ini_str: &str = "\
[InfantryTypes]\n0=E1\n\n\
[VehicleTypes]\n0=MTNK\n\n\
[AircraftTypes]\n\n\
[BuildingTypes]\n0=GAPOWR\n\n\
[E1]\nStrength=125\nArmor=flak\nSpeed=4\nPrimary=M60\n\n\
[MTNK]\nStrength=300\nArmor=heavy\nSpeed=6\nPrimary=105mm\n\n\
[GAPOWR]\nStrength=750\nArmor=wood\n\n\
[M60]\nDamage=25\nROF=20\nRange=5\nWarhead=SA\n\n\
[105mm]\nDamage=65\nROF=50\nRange=6\nWarhead=AP\n\n\
[SA]\nVerses=100%,100%,100%,90%,70%,25%,100%,25%,25%,0%,0%\n\n\
[AP]\nVerses=100%,100%,90%,75%,75%,75%,60%,30%,20%,0%,0%\n";
    let ini: IniFile = IniFile::from_str(ini_str);
    RuleSet::from_ini(&ini).expect("test rules should parse")
}

fn make_entity(id: u64, type_ref: &str, rx: u16, ry: u16, hp: u16) -> GameEntity {
    let mut e = GameEntity::test_default(id, type_ref, "Test", rx, ry);
    e.health = Health {
        current: hp,
        max: hp,
    };
    e
}

fn make_entity_owned(
    id: u64,
    type_ref: &str,
    rx: u16,
    ry: u16,
    hp: u16,
    owner: &str,
) -> GameEntity {
    let mut e = GameEntity::test_default(id, type_ref, owner, rx, ry);
    e.health = Health {
        current: hp,
        max: hp,
    };
    e
}

#[test]
fn test_armor_index_lookup() {
    assert_eq!(armor_index("none"), 0);
    assert_eq!(armor_index("flak"), 1);
    assert_eq!(armor_index("heavy"), 5);
    assert_eq!(armor_index("wood"), 6);
    assert_eq!(armor_index("concrete"), 8);
    assert_eq!(armor_index("unknown"), 0);
}

#[test]
fn test_issue_attack_command() {
    let mut store = EntityStore::new();
    store.insert(make_entity(1, "MTNK", 5, 5, 300));
    store.insert(make_entity(2, "MTNK", 8, 5, 300));

    let result: bool = issue_attack_command(&mut store, 1, 2, None, &test_interner());
    assert!(result, "Should succeed for valid entities");

    let attack = store.get(1).unwrap().attack_target.as_ref().unwrap();
    assert_eq!(attack.target, 2);
    assert_eq!(attack.cooldown_ticks, 0, "Initial cooldown should be 0");
}

#[test]
fn test_attack_nonexistent_target() {
    let mut store = EntityStore::new();
    store.insert(make_entity(1, "MTNK", 5, 5, 300));

    let result: bool = issue_attack_command(&mut store, 1, 99, None, &test_interner());
    assert!(!result, "Should fail for nonexistent target");
}

#[test]
fn test_tick_combat_applies_damage() {
    let rules: RuleSet = test_rules();
    let mut store = EntityStore::new();

    // MTNK attacks another MTNK (heavy armor).
    // 105mm: damage=65, warhead=AP, AP verses[heavy(5)] = 75%.
    // Integer math: 65 * 75 / 100 = 48.
    store.insert(make_entity(1, "MTNK", 5, 5, 300));
    store.insert(make_entity(2, "MTNK", 8, 5, 300));
    let mut interner = test_interner();
    issue_attack_command(&mut store, 1, 2, None, &interner);

    tick_combat(&mut store, &rules, &mut interner, &mut BTreeMap::new(), 100);

    let target_health = store.get(2).expect("target alive").health.current;
    assert_eq!(
        target_health,
        300 - 48,
        "Should take 48 damage (65 * 75 / 100)"
    );
}

#[test]
fn test_tick_combat_only_emits_bridge_damage_for_wall_warheads() {
    let mut store = EntityStore::new();
    let rules_without_wall = test_rules();
    store.insert(make_entity(1, "MTNK", 5, 5, 300));
    store.insert(make_entity(2, "MTNK", 8, 5, 300));
    let mut interner = test_interner();
    issue_attack_command(&mut store, 1, 2, None, &interner);
    let result = tick_combat_with_fog(
        &mut store,
        &rules_without_wall,
        &mut interner,
        None,
        &BTreeMap::<InternedId, PowerState>::new(),
        None,
        &mut BTreeMap::new(),
        100,
    );
    assert!(
        result.bridge_damage_events.is_empty(),
        "non-wall warheads must not emit bridge damage"
    );

    let bridge_rules = RuleSet::from_ini(&IniFile::from_str(
        "[InfantryTypes]\n\
         [VehicleTypes]\n0=MTNK\n\n\
         [AircraftTypes]\n\n\
         [BuildingTypes]\n\n\
         [MTNK]\nStrength=300\nArmor=heavy\nSpeed=6\nPrimary=105mm\n\n\
         [105mm]\nDamage=65\nROF=50\nRange=6\nWarhead=AP\n\n\
         [AP]\nWall=yes\nVerses=100%,100%,90%,75%,75%,75%,60%,30%,20%,0%,0%\n",
    ))
    .expect("bridge combat rules should parse");
    let mut wall_store = EntityStore::new();
    wall_store.insert(make_entity(3, "MTNK", 5, 5, 300));
    wall_store.insert(make_entity(4, "MTNK", 8, 5, 300));
    issue_attack_command(&mut wall_store, 3, 4, None, &interner);
    let wall_result = tick_combat_with_fog(
        &mut wall_store,
        &bridge_rules,
        &mut interner,
        None,
        &BTreeMap::<InternedId, PowerState>::new(),
        None,
        &mut BTreeMap::new(),
        100,
    );
    assert_eq!(
        wall_result.bridge_damage_events,
        vec![BridgeDamageEvent {
            rx: 8,
            ry: 5,
            damage: 65
        }]
    );
}

#[test]
fn test_tick_combat_respects_cooldown() {
    let rules: RuleSet = test_rules();
    let mut store = EntityStore::new();
    store.insert(make_entity(1, "MTNK", 5, 5, 300));
    store.insert(make_entity(2, "MTNK", 8, 5, 300));
    let mut interner = test_interner();
    issue_attack_command(&mut store, 1, 2, None, &interner);

    // First shot fires immediately (cooldown=0).
    tick_combat(&mut store, &rules, &mut interner, &mut BTreeMap::new(), 100);
    let h1: u16 = store.get(2).unwrap().health.current;

    // Next tick should not fire again immediately.
    tick_combat(&mut store, &rules, &mut interner, &mut BTreeMap::new(), 100);
    let h2: u16 = store.get(2).unwrap().health.current;
    assert_eq!(h1, h2, "Should not fire during cooldown");

    // After enough ticks, should fire again.
    for _ in 0..40 {
        tick_combat(&mut store, &rules, &mut interner, &mut BTreeMap::new(), 100);
    }
    let h3: u16 = store.get(2).unwrap().health.current;
    assert!(h3 < h2, "Should fire after cooldown expires");
}

#[test]
fn test_tick_combat_kills_target() {
    let rules: RuleSet = test_rules();
    let mut store = EntityStore::new();
    store.insert(make_entity(1, "MTNK", 5, 5, 300));
    store.insert(make_entity(2, "MTNK", 8, 5, 10));
    let mut interner = test_interner();
    issue_attack_command(&mut store, 1, 2, None, &interner);

    tick_combat(&mut store, &rules, &mut interner, &mut BTreeMap::new(), 100);

    assert!(store.get(2).is_none(), "Dead entity should be removed");
    assert!(
        store.get(1).unwrap().attack_target.is_none(),
        "AttackTarget removed after target dies"
    );
}

#[test]
fn test_tick_combat_out_of_range() {
    let rules: RuleSet = test_rules();
    let mut store = EntityStore::new();
    // 105mm range = 6 cells. Target at distance 10.
    store.insert(make_entity(1, "MTNK", 0, 0, 300));
    store.insert(make_entity(2, "MTNK", 10, 0, 300));
    let mut interner = test_interner();
    issue_attack_command(&mut store, 1, 2, None, &interner);

    tick_combat(&mut store, &rules, &mut interner, &mut BTreeMap::new(), 100);

    let target_health = store.get(2).unwrap().health.current;
    assert_eq!(
        target_health, 300,
        "Out-of-range target should not take damage"
    );
    assert!(
        store.get(1).unwrap().attack_target.is_none(),
        "AttackTarget removed when out of range"
    );
}

#[test]
fn test_infantry_vs_heavy_armor() {
    let rules: RuleSet = test_rules();
    let mut store = EntityStore::new();
    // E1 (M60) attacks MTNK (heavy armor).
    // M60: damage=25, warhead=SA, SA verses[heavy(5)] = 25%.
    // Integer math: 25 * 25 / 100 = 6.
    store.insert(make_entity(1, "E1", 5, 5, 125));
    store.insert(make_entity(2, "MTNK", 8, 5, 300));
    let mut interner = test_interner();
    issue_attack_command(&mut store, 1, 2, None, &interner);

    tick_combat(&mut store, &rules, &mut interner, &mut BTreeMap::new(), 100);

    let h: u16 = store.get(2).unwrap().health.current;
    assert_eq!(
        h,
        300 - 6,
        "Infantry vs heavy armor should do 6 damage (25 * 25 / 100)"
    );
}

#[test]
fn test_cell_distance() {
    assert!((cell_distance(0, 0, 3, 4) - 5.0).abs() < 0.01);
    assert!((cell_distance(5, 5, 5, 5) - 0.0).abs() < f32::EPSILON);
    assert!((cell_distance(0, 0, 1, 0) - 1.0).abs() < f32::EPSILON);
}

#[test]
fn test_tick_combat_visibility_blocks_fire() {
    let rules: RuleSet = test_rules();
    let mut store = EntityStore::new();
    store.insert(make_entity_owned(1, "MTNK", 5, 5, 300, "Americans"));
    store.insert(make_entity_owned(2, "MTNK", 8, 5, 300, "Soviet"));
    let mut interner = test_interner();
    issue_attack_command(&mut store, 1, 2, None, &interner);

    let fog = FogState::default();
    tick_combat_with_fog(
        &mut store,
        &rules,
        &mut interner,
        Some(&fog),
        &BTreeMap::<InternedId, PowerState>::new(),
        None,
        &mut BTreeMap::new(),
        100,
    );

    let target_health = store.get(2).expect("target alive").health.current;
    assert_eq!(target_health, 300, "Hidden target should not be damaged");
    assert!(
        store.get(1).unwrap().attack_target.is_none(),
        "AttackTarget removed when target is not visible and no replacement exists"
    );
}

#[test]
fn test_tick_combat_retargets_by_distance_then_stable_id() {
    let rules: RuleSet = test_rules();
    let mut store = EntityStore::new();
    store.insert(make_entity_owned(10, "MTNK", 5, 5, 300, "Americans"));
    store.insert(make_entity_owned(99, "MTNK", 6, 5, 0, "Soviet")); // dead
    store.insert(make_entity_owned(20, "MTNK", 7, 5, 300, "Soviet"));
    store.insert(make_entity_owned(3, "MTNK", 7, 5, 300, "Soviet"));
    let mut interner = test_interner();
    issue_attack_command(&mut store, 10, 99, None, &interner);

    let mut fog = FogState::default();
    fog.mark_visible_for_owner(test_intern("Americans"), 7, 5);
    tick_combat_with_fog(
        &mut store,
        &rules,
        &mut interner,
        Some(&fog),
        &BTreeMap::<InternedId, PowerState>::new(),
        None,
        &mut BTreeMap::new(),
        100,
    );

    let attack = store
        .get(10)
        .unwrap()
        .attack_target
        .as_ref()
        .expect("attacker should retarget");
    assert_eq!(
        attack.target, 3,
        "Tie should resolve to lower stable entity id"
    );
    assert_ne!(attack.target, 20u64, "Should not target enemy_a (sid=20)");
}

#[test]
fn test_tick_combat_retargets_prefers_threat_class_when_distance_equal() {
    let rules: RuleSet = test_rules();
    let mut store = EntityStore::new();
    store.insert(make_entity_owned(10, "MTNK", 5, 5, 300, "Americans"));
    store.insert(make_entity_owned(99, "MTNK", 6, 5, 0, "Soviet")); // dead
    let mut building = make_entity_owned(1, "GAPOWR", 7, 5, 750, "Soviet");
    building.category = crate::map::entities::EntityCategory::Structure;
    store.insert(building);
    store.insert(make_entity_owned(200, "MTNK", 7, 5, 300, "Soviet"));
    let mut interner = test_interner();
    issue_attack_command(&mut store, 10, 99, None, &interner);

    let mut fog = FogState::default();
    fog.mark_visible_for_owner(test_intern("Americans"), 7, 5);
    tick_combat_with_fog(
        &mut store,
        &rules,
        &mut interner,
        Some(&fog),
        &BTreeMap::<InternedId, PowerState>::new(),
        None,
        &mut BTreeMap::new(),
        100,
    );

    let attack = store
        .get(10)
        .unwrap()
        .attack_target
        .as_ref()
        .expect("attacker should retarget");
    assert_eq!(
        attack.target, 200,
        "Combat unit should rank above building at equal distance"
    );
    assert_ne!(attack.target, 1u64, "Should not target building (sid=1)");
}

// --- Ore destruction integration tests ---

/// Build a RuleSet with a CellSpread=2 AoE weapon for ore destruction testing.
fn test_rules_with_spread() -> RuleSet {
    let ini_str: &str = "\
[InfantryTypes]\n\n\
[VehicleTypes]\n0=MTNK\n\n\
[AircraftTypes]\n\n\
[BuildingTypes]\n\n\
[MTNK]\nStrength=300\nArmor=heavy\nSpeed=6\nPrimary=120mm\n\n\
[120mm]\nDamage=120\nROF=50\nRange=6\nWarhead=HE\n\n\
[HE]\nCellSpread=2\nVerses=100%,100%,100%,100%,100%,100%,100%,100%,100%,100%,100%\n";
    let ini = IniFile::from_str(ini_str);
    RuleSet::from_ini(&ini).expect("test rules should parse")
}

#[test]
fn test_weapon_fire_destroys_ore_in_spread() {
    let rules = test_rules_with_spread();
    let mut store = EntityStore::new();
    store.insert(make_entity(1, "MTNK", 5, 5, 300));
    store.insert(make_entity(2, "MTNK", 8, 5, 300));
    let mut interner = test_interner();
    issue_attack_command(&mut store, 1, 2, None, &interner);

    // Place ore at the target cell and a neighbor within CellSpread=2.
    let mut resource_nodes = BTreeMap::new();
    // 6 density levels of ore at target (8,5): remaining = 6 * 120 = 720.
    resource_nodes.insert(
        (8, 5),
        ResourceNode {
            resource_type: ResourceType::Ore,
            remaining: 720,
        },
    );
    // 3 density levels at (9,5): remaining = 3 * 120 = 360.
    resource_nodes.insert(
        (9, 5),
        ResourceNode {
            resource_type: ResourceType::Ore,
            remaining: 360,
        },
    );

    tick_combat_with_fog(
        &mut store,
        &rules,
        &mut interner,
        None,
        &BTreeMap::<InternedId, PowerState>::new(),
        None,
        &mut resource_nodes,
        100,
    );

    // Damage=120, ore_damage = 120/10 = 12 density levels.
    // Cell (8,5) had 6 levels — 12 >= 6, so fully removed.
    assert!(
        resource_nodes.get(&(8, 5)).is_none(),
        "target cell ore should be fully destroyed (12 >= 6)"
    );
    // Cell (9,5) had 3 levels — 12 >= 3, so fully removed.
    assert!(
        resource_nodes.get(&(9, 5)).is_none(),
        "neighbor cell ore should be fully destroyed (12 >= 3)"
    );
}

#[test]
fn test_direct_hit_weapon_destroys_center_ore() {
    let rules = test_rules(); // AP warhead has CellSpread=0.
    let mut store = EntityStore::new();
    store.insert(make_entity(1, "MTNK", 5, 5, 300));
    store.insert(make_entity(2, "MTNK", 8, 5, 300));
    let mut interner = test_interner();
    issue_attack_command(&mut store, 1, 2, None, &interner);

    let mut resource_nodes = BTreeMap::new();
    resource_nodes.insert(
        (8, 5),
        ResourceNode {
            resource_type: ResourceType::Ore,
            remaining: 720,
        },
    );
    // Ore at adjacent cell (9,5) should NOT be affected (CellSpread=0 = center only).
    resource_nodes.insert(
        (9, 5),
        ResourceNode {
            resource_type: ResourceType::Ore,
            remaining: 720,
        },
    );

    tick_combat_with_fog(
        &mut store,
        &rules,
        &mut interner,
        None,
        &BTreeMap::<InternedId, PowerState>::new(),
        None,
        &mut resource_nodes,
        100,
    );

    // 105mm damage=65, ore_damage = 65/10 = 6.
    // Cell (8,5) had 6 density levels — 6 >= 6 → fully removed.
    assert!(
        resource_nodes.get(&(8, 5)).is_none(),
        "center cell ore should be destroyed (6 >= 6)"
    );
    // Cell (9,5) should be untouched.
    assert_eq!(
        resource_nodes.get(&(9, 5)).unwrap().remaining,
        720,
        "adjacent cell should be untouched with CellSpread=0"
    );
}

#[test]
fn test_weak_weapon_partial_ore_reduction() {
    let rules = test_rules(); // M60 damage=25.
    let mut store = EntityStore::new();
    // E1 attacks MTNK — E1's primary is M60 (damage=25, SA warhead, CellSpread=0).
    store.insert(make_entity(1, "E1", 5, 5, 125));
    store.insert(make_entity(2, "MTNK", 8, 5, 300));
    let mut interner = test_interner();
    issue_attack_command(&mut store, 1, 2, None, &interner);

    let mut resource_nodes = BTreeMap::new();
    // 10 density levels of ore: remaining = 10 * 120 = 1200.
    resource_nodes.insert(
        (8, 5),
        ResourceNode {
            resource_type: ResourceType::Ore,
            remaining: 1200,
        },
    );

    tick_combat_with_fog(
        &mut store,
        &rules,
        &mut interner,
        None,
        &BTreeMap::<InternedId, PowerState>::new(),
        None,
        &mut resource_nodes,
        100,
    );

    // M60 damage=25, ore_damage = 25/10 = 2.
    // 10 density levels, remove 2 → 8 remaining → 8 * 120 = 960.
    assert_eq!(
        resource_nodes.get(&(8, 5)).unwrap().remaining,
        960,
        "should reduce by 2 density levels (25/10=2)"
    );
}
