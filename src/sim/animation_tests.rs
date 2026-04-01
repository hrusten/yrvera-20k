//! Tests for the sprite animation system.
//!
//! Separated from animation.rs to stay within the 400-line file limit.

use std::collections::BTreeMap;

use crate::map::entities::EntityCategory;
use crate::sim::animation::*;
use crate::sim::combat::AttackTarget;
use crate::sim::components::{Health, MovementTarget};
use crate::sim::entity_store::EntityStore;
use crate::sim::game_entity::GameEntity;
use crate::sim::intern::StringInterner;
use crate::sim::movement::locomotor::MovementLayer;
use crate::util::fixed_math::{SIM_ZERO, SimFixed};

/// Helper: create a SequenceDef for tests.
fn test_def(
    start_frame: u16,
    frame_count: u16,
    facings: u8,
    tick_ms: u32,
    loop_mode: LoopMode,
) -> SequenceDef {
    SequenceDef {
        start_frame,
        frame_count,
        facings,
        facing_multiplier: frame_count,
        tick_ms,
        loop_mode,
        clockwise_facings: false,
    }
}

// --- resolve_shp_frame tests ---

#[test]
fn test_resolve_stand_facing_north() {
    let def = test_def(0, 1, 8, 200, LoopMode::Loop);
    // Facing 0 (cell-N) → +32 adjustment for cell-N-to-screen-N offset →
    // adjusted=32, cw_index=1, ccw facing_index=7 → frame 7
    assert_eq!(resolve_shp_frame(&def, 0, 0), 7);
}

#[test]
fn test_resolve_stand_facing_south() {
    let def = test_def(0, 1, 8, 200, LoopMode::Loop);
    // Facing 128 (cell-S) → +32 adj → adjusted=160, cw_index=5, ccw facing_index=3 → frame 3
    assert_eq!(resolve_shp_frame(&def, 128, 0), 3);
}

#[test]
fn test_resolve_walk_facing_east_frame_3() {
    let def = test_def(8, 6, 8, 100, LoopMode::Loop);
    // Facing 64 (cell-E) → +32 adj → adjusted=96, cw_index=3, ccw facing_index=5 → frame = 8 + 5*6 + 3 = 41
    assert_eq!(resolve_shp_frame(&def, 64, 3), 41);
}

#[test]
fn test_resolve_non_directional() {
    let def = test_def(56, 15, 1, 120, LoopMode::Loop);
    // Non-directional: facing is ignored. Frame 7 → 56 + 7 = 63
    assert_eq!(resolve_shp_frame(&def, 128, 7), 63);
}

#[test]
fn test_resolve_frame_index_wraps() {
    let def = test_def(8, 6, 8, 100, LoopMode::Loop);
    // Frame 7 wraps: 7 % 6 = 1. Facing 0 (cell-N) → +32 adj → facing_index=7 → 8 + 7*6 + 1 = 51
    assert_eq!(resolve_shp_frame(&def, 0, 7), 51);
}

#[test]
fn test_resolve_facing_multiplier_differs_from_frame_count() {
    // Simulates a sequence with facing_multiplier=8 but frame_count=4
    let def = SequenceDef {
        start_frame: 0,
        frame_count: 4,
        facings: 8,
        facing_multiplier: 8,
        tick_ms: 100,
        loop_mode: LoopMode::Loop,
        clockwise_facings: false,
    };
    // Facing 64 (cell-E) → +32 adj → facing_index=5 → frame = 0 + 5*8 + 3 = 43
    assert_eq!(resolve_shp_frame(&def, 64, 3), 43);
}

#[test]
fn test_resolve_all_8_facings() {
    let def = test_def(0, 1, 8, 200, LoopMode::Loop);
    // DirStruct (clockwise, cell-relative) → SHP frame index (counter-clockwise).
    // +32 adjustment converts cell-north (screen upper-right) to screen-north (straight up).
    // SHP CCW order: 0=screen-N, 1=NW, 2=W, 3=SW, 4=S, 5=SE, 6=E, 7=NE
    assert_eq!(resolve_shp_frame(&def, 0, 0), 7); // cell-N  → SHP 7
    assert_eq!(resolve_shp_frame(&def, 32, 0), 6); // cell-NE → SHP 6
    assert_eq!(resolve_shp_frame(&def, 64, 0), 5); // cell-E  → SHP 5
    assert_eq!(resolve_shp_frame(&def, 96, 0), 4); // cell-SE → SHP 4
    assert_eq!(resolve_shp_frame(&def, 128, 0), 3); // cell-S  → SHP 3
    assert_eq!(resolve_shp_frame(&def, 160, 0), 2); // cell-SW → SHP 2
    assert_eq!(resolve_shp_frame(&def, 192, 0), 1); // cell-W  → SHP 1
    assert_eq!(resolve_shp_frame(&def, 224, 0), 0); // cell-NW → SHP 0
}

// --- advance_animation tests ---

#[test]
fn test_advance_one_frame() {
    let def = test_def(0, 6, 8, 100, LoopMode::Loop);
    let mut anim: Animation = Animation::new(SequenceKind::Walk);
    let result: Option<SequenceKind> = advance_animation(&mut anim, &def, 100);
    assert!(result.is_none());
    assert_eq!(anim.frame_index, 1);
}

#[test]
fn test_advance_loop_wraps_to_zero() {
    let def = test_def(0, 3, 1, 100, LoopMode::Loop);
    let mut anim: Animation = Animation::new(SequenceKind::Walk);
    anim.frame_index = 2; // Last frame
    advance_animation(&mut anim, &def, 100);
    assert_eq!(anim.frame_index, 0, "Should wrap to frame 0");
}

#[test]
fn test_advance_hold_last_frame() {
    let def = test_def(86, 15, 1, 80, LoopMode::HoldLast);
    let mut anim: Animation = Animation::new(SequenceKind::Die1);
    anim.frame_index = 14; // Last frame
    advance_animation(&mut anim, &def, 80);
    assert_eq!(anim.frame_index, 14, "Should hold last frame");
    assert!(anim.finished);
}

#[test]
fn test_advance_transition_to() {
    let def = test_def(56, 3, 1, 100, LoopMode::TransitionTo(SequenceKind::Stand));
    let mut anim: Animation = Animation::new(SequenceKind::Idle1);
    anim.frame_index = 2; // Last frame
    let result: Option<SequenceKind> = advance_animation(&mut anim, &def, 100);
    assert_eq!(result, Some(SequenceKind::Stand));
}

#[test]
fn test_advance_multiple_frames_large_dt() {
    let def = test_def(0, 6, 1, 100, LoopMode::Loop);
    let mut anim: Animation = Animation::new(SequenceKind::Walk);
    // 350ms at 100ms/frame = 3 frames advanced, 50ms remainder.
    advance_animation(&mut anim, &def, 350);
    assert_eq!(anim.frame_index, 3);
    assert_eq!(anim.elapsed_ms, 50);
}

#[test]
fn test_advance_finished_does_nothing() {
    let def = test_def(86, 15, 1, 80, LoopMode::HoldLast);
    let mut anim: Animation = Animation::new(SequenceKind::Die1);
    anim.finished = true;
    anim.frame_index = 14;
    advance_animation(&mut anim, &def, 1000);
    assert_eq!(anim.frame_index, 14);
}

// --- Animation component tests ---

#[test]
fn test_switch_resets_state() {
    let mut anim: Animation = Animation::new(SequenceKind::Walk);
    anim.frame_index = 3;
    anim.elapsed_ms = 50;
    anim.switch_to(SequenceKind::Stand);
    assert_eq!(anim.sequence, SequenceKind::Stand);
    assert_eq!(anim.frame_index, 0);
    assert_eq!(anim.elapsed_ms, 0);
    assert!(!anim.finished);
}

#[test]
fn test_switch_noop_same_sequence() {
    let mut anim: Animation = Animation::new(SequenceKind::Walk);
    anim.frame_index = 3;
    anim.elapsed_ms = 50;
    anim.switch_to(SequenceKind::Walk);
    assert_eq!(anim.frame_index, 3, "Same sequence should not reset");
    assert_eq!(anim.elapsed_ms, 50);
}

#[test]
fn test_animation_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Animation>();
}

// --- Default sequence set tests ---

#[test]
fn test_default_infantry_has_all_sequences() {
    let set: SequenceSet = default_infantry_sequences();
    assert!(set.get(&SequenceKind::Stand).is_some());
    assert!(set.get(&SequenceKind::Walk).is_some());
    assert!(set.get(&SequenceKind::Die1).is_some());
    assert!(set.get(&SequenceKind::Die2).is_some());
    assert!(set.get(&SequenceKind::Idle1).is_some());
    assert!(set.get(&SequenceKind::Idle2).is_some());
    assert_eq!(set.len(), 6);

    let walk: &SequenceDef = set.get(&SequenceKind::Walk).expect("Walk exists");
    assert_eq!(walk.start_frame, 8);
    assert_eq!(walk.frame_count, 6);
    assert_eq!(walk.facings, 8);
    assert_eq!(walk.facing_multiplier, 6);
}

#[test]
fn test_default_building_has_stand_only() {
    let set: SequenceSet = default_building_sequences();
    assert!(set.get(&SequenceKind::Stand).is_some());
    assert_eq!(set.len(), 1);
}

// --- death_sequence_for_inf_death tests ---

#[test]
fn test_death_sequence_mapping() {
    assert_eq!(death_sequence_for_inf_death(0), SequenceKind::Die1);
    assert_eq!(death_sequence_for_inf_death(1), SequenceKind::Die1);
    assert_eq!(death_sequence_for_inf_death(2), SequenceKind::Die2);
    assert_eq!(death_sequence_for_inf_death(3), SequenceKind::Die3);
    assert_eq!(death_sequence_for_inf_death(4), SequenceKind::Die4);
    assert_eq!(death_sequence_for_inf_death(5), SequenceKind::Die5);
    // Values > 5 clamp to Die5
    assert_eq!(death_sequence_for_inf_death(10), SequenceKind::Die5);
}

// --- tick_animations integration tests ---

fn make_test_interner() -> StringInterner {
    let mut interner = StringInterner::new();
    interner.intern("Americans");
    interner.intern("E1");
    interner
}

fn make_infantry_entity(id: u64, facing: u8, interner: &mut StringInterner) -> GameEntity {
    let mut e = GameEntity::new(
        id,
        0,
        0,
        0,
        facing,
        interner.intern("Americans"),
        Health {
            current: 100,
            max: 100,
        },
        interner.intern("E1"),
        EntityCategory::Infantry,
        0,
        0,
        false,
    );
    e.animation = Some(Animation::new(SequenceKind::Stand));
    e
}

fn make_movement_target() -> MovementTarget {
    MovementTarget {
        path: vec![(0, 0), (1, 0)],
        path_layers: vec![MovementLayer::Ground; 2],
        next_index: 1,
        speed: SimFixed::from_num(512),
        move_dir_x: SimFixed::from_num(256),
        move_dir_y: SIM_ZERO,
        move_dir_len: SimFixed::from_num(256),
        ..Default::default()
    }
}

#[test]
fn test_tick_switches_to_walk_with_movement() {
    let mut interner = make_test_interner();
    let mut store = EntityStore::new();
    let mut e = make_infantry_entity(1, 0, &mut interner);
    e.movement_target = Some(make_movement_target());
    store.insert(e);

    let mut sequences: BTreeMap<String, SequenceSet> = BTreeMap::new();
    sequences.insert("E1".to_string(), default_infantry_sequences());

    tick_animations(&mut store, &sequences, 16, &interner);

    let anim = store.get(1).unwrap().animation.as_ref().unwrap();
    assert_eq!(anim.sequence, SequenceKind::Walk);
}

#[test]
fn test_tick_switches_to_stand_without_movement() {
    let mut interner = make_test_interner();
    let mut store = EntityStore::new();
    let mut e = make_infantry_entity(1, 0, &mut interner);
    e.animation = Some(Animation {
        sequence: SequenceKind::Walk,
        frame_index: 3,
        elapsed_ms: 0,
        finished: false,
    });
    store.insert(e);

    let mut sequences: BTreeMap<String, SequenceSet> = BTreeMap::new();
    sequences.insert("E1".to_string(), default_infantry_sequences());

    tick_animations(&mut store, &sequences, 16, &interner);

    let anim = store.get(1).unwrap().animation.as_ref().unwrap();
    assert_eq!(anim.sequence, SequenceKind::Stand);
}

#[test]
fn test_tick_advances_walk_frame() {
    let mut interner = make_test_interner();
    let mut store = EntityStore::new();
    let mut e = make_infantry_entity(1, 64, &mut interner);
    e.animation = Some(Animation::new(SequenceKind::Walk));
    e.movement_target = Some(make_movement_target());
    store.insert(e);

    let mut sequences: BTreeMap<String, SequenceSet> = BTreeMap::new();
    sequences.insert("E1".to_string(), default_infantry_sequences());

    // Walk tick_ms = 100. Advance by 100ms → frame 0 → 1.
    tick_animations(&mut store, &sequences, 100, &interner);

    let anim = store.get(1).unwrap().animation.as_ref().unwrap();
    assert_eq!(anim.sequence, SequenceKind::Walk);
    assert_eq!(anim.frame_index, 1);
}

#[test]
fn test_tick_attack_triggers_fire_animation() {
    let mut interner = make_test_interner();
    let mut store = EntityStore::new();
    let mut e = make_infantry_entity(1, 0, &mut interner);
    e.attack_target = Some(AttackTarget::new(999));
    store.insert(e);

    // Build sequences that include Attack.
    let mut set = default_infantry_sequences();
    set.insert(
        SequenceKind::Attack,
        SequenceDef {
            start_frame: 164,
            frame_count: 6,
            facings: 8,
            facing_multiplier: 6,
            tick_ms: 80,
            loop_mode: LoopMode::TransitionTo(SequenceKind::Stand),
            clockwise_facings: false,
        },
    );
    let mut sequences: BTreeMap<String, SequenceSet> = BTreeMap::new();
    sequences.insert("E1".to_string(), set);

    tick_animations(&mut store, &sequences, 16, &interner);

    let anim = store.get(1).unwrap().animation.as_ref().unwrap();
    assert_eq!(anim.sequence, SequenceKind::Attack);
}

#[test]
fn test_tick_dying_entity_skips_transitions() {
    let mut interner = make_test_interner();
    let mut store = EntityStore::new();
    let mut e = make_infantry_entity(1, 0, &mut interner);
    e.dying = true;
    e.animation = Some(Animation::new(SequenceKind::Die1));
    e.movement_target = Some(make_movement_target());
    store.insert(e);

    let mut sequences: BTreeMap<String, SequenceSet> = BTreeMap::new();
    sequences.insert("E1".to_string(), default_infantry_sequences());

    let dead = tick_animations(&mut store, &sequences, 16, &interner);

    // Dying entity should NOT switch to Walk despite having movement_target.
    let anim = store.get(1).unwrap().animation.as_ref().unwrap();
    assert_eq!(anim.sequence, SequenceKind::Die1);
    // Not finished yet (only 16ms of an 80ms/frame * 15 frame animation).
    assert!(dead.is_empty());
}

#[test]
fn test_tick_dying_entity_returns_finished_id() {
    let mut interner = make_test_interner();
    let mut store = EntityStore::new();
    let mut e = make_infantry_entity(1, 0, &mut interner);
    e.dying = true;
    e.animation = Some(Animation {
        sequence: SequenceKind::Die1,
        frame_index: 14,
        elapsed_ms: 0,
        finished: true,
    });
    store.insert(e);

    let mut sequences: BTreeMap<String, SequenceSet> = BTreeMap::new();
    sequences.insert("E1".to_string(), default_infantry_sequences());

    let dead = tick_animations(&mut store, &sequences, 16, &interner);
    assert_eq!(dead, vec![1]);
}
