//! Movement integration tests — verifies ground movement, repath behavior, blocked handling,
//! stuck recovery, and infantry sub-cell mechanics using minimal simulation setups.

use super::*;
use crate::map::terrain;
use crate::sim::components::MovementTarget;
use crate::sim::entity_store::EntityStore;
use crate::sim::game_entity::GameEntity;
use crate::sim::intern::test_interner;
use crate::sim::movement::locomotor::MovementLayer;
use crate::sim::rng::SimRng;
use crate::util::fixed_math::{SIM_ZERO, SimFixed};

// --- Facing calculation tests ---
// Cell deltas map directly to screen-relative RA2 DirStruct values:
// 0=N, 64=E, 128=S, 192=W. +dx = east, +dy = south.

#[test]
fn test_facing_iso_north() {
    // (0,-1) = north on screen → facing 0.
    let f: u8 = facing_from_delta(0, -1);
    assert_eq!(f, 0, "North (0,-1) should be facing 0");
}

#[test]
fn test_facing_iso_east() {
    // (1,0) = east on screen → facing 64.
    let f: u8 = facing_from_delta(1, 0);
    assert_eq!(f, 64, "East (1,0) should be facing 64");
}

#[test]
fn test_facing_iso_south() {
    // (0,1) = south on screen → facing 128.
    let f: u8 = facing_from_delta(0, 1);
    assert_eq!(f, 128, "South (0,1) should be facing 128");
}

#[test]
fn test_facing_iso_west() {
    // (-1,0) = west on screen → facing 192.
    let f: u8 = facing_from_delta(-1, 0);
    assert_eq!(f, 192, "West (-1,0) should be facing 192");
}

#[test]
fn test_facing_iso_northeast() {
    // (1,-1) = NE on screen → facing 32.
    let f: u8 = facing_from_delta(1, -1);
    assert_eq!(f, 32, "NE (1,-1) should be facing 32");
}

#[test]
fn test_facing_iso_southeast() {
    // (1,1) = SE on screen → facing 96.
    let f: u8 = facing_from_delta(1, 1);
    assert_eq!(f, 96, "SE (1,1) should be facing 96");
}

#[test]
fn test_facing_zero_delta() {
    let f: u8 = facing_from_delta(0, 0);
    assert_eq!(f, 0, "Zero delta should default to facing 0 (north)");
}

// --- Movement tick tests ---

#[test]
fn test_tick_movement_advances_position() {
    let mut entities = EntityStore::new();

    // Create an entity at (2, 2) with a path to (5, 2).
    let path: Vec<(u16, u16)> = vec![(2, 2), (3, 2), (4, 2), (5, 2)];

    let mut e = GameEntity::test_default(1, "HTNK", "Americans", 2, 2);
    e.movement_target = Some(MovementTarget {
        path,
        path_layers: vec![MovementLayer::Ground; 4],
        next_index: 1,
        speed: SimFixed::from_num(512), // 512 leptons/sec = 2 cells/sec.
        move_dir_x: SimFixed::from_num(256),
        move_dir_y: SIM_ZERO,
        move_dir_len: SimFixed::from_num(256),
        ..Default::default()
    });
    e.facing = 64;
    entities.insert(e);

    // Tick 500ms at 512 lep/s → 256 leptons = 1 cell → snap to (3,2).
    tick_movement(&mut entities, 500, &test_interner());

    let entity = entities.get(1).expect("entity exists");
    assert_eq!(entity.position.rx, 3);
    assert_eq!(entity.position.ry, 2);
    // Entity should still have MovementTarget (not at goal yet).
    assert!(entity.movement_target.is_some());
}

#[test]
fn test_tick_movement_removes_target_at_goal() {
    let mut entities = EntityStore::new();

    // 2-cell path: (0,0) → (1,0). Speed=10 means it finishes instantly.
    let path: Vec<(u16, u16)> = vec![(0, 0), (1, 0)];
    let mut e = GameEntity::test_default(1, "HTNK", "Americans", 0, 0);
    e.movement_target = Some(MovementTarget {
        path,
        path_layers: vec![MovementLayer::Ground; 2],
        next_index: 1,
        speed: SimFixed::from_num(2560), // 10 cells/sec in leptons.
        move_dir_x: SimFixed::from_num(256),
        move_dir_y: SIM_ZERO,
        move_dir_len: SimFixed::from_num(256),
        ..Default::default()
    });
    entities.insert(e);

    // Large tick to ensure we finish the path.
    tick_movement(&mut entities, 1000, &test_interner());

    let entity = entities.get(1).expect("entity exists");
    assert_eq!(entity.position.rx, 1);
    assert_eq!(entity.position.ry, 0);
    // MovementTarget should be removed.
    assert!(
        entity.movement_target.is_none(),
        "MovementTarget should be removed when path is complete"
    );
}

#[test]
fn test_tick_movement_partial_progress() {
    let mut entities = EntityStore::new();

    let path: Vec<(u16, u16)> = vec![(0, 0), (1, 0), (2, 0)];
    let mut e = GameEntity::test_default(1, "HTNK", "Americans", 0, 0);
    e.movement_target = Some(MovementTarget {
        path,
        path_layers: vec![MovementLayer::Ground; 3],
        next_index: 1,
        speed: SimFixed::from_num(512), // 512 lep/s = 2 cells/sec.
        move_dir_x: SimFixed::from_num(256),
        move_dir_y: SIM_ZERO,
        move_dir_len: SimFixed::from_num(256),
        ..Default::default()
    });
    entities.insert(e);

    // 250ms at 512 lep/s → 128 leptons traveled. sub_x starts at 128 (center),
    // moves to 256 which is the cell boundary — entity should cross to next cell.
    // Use 125ms instead: 512 * 0.125 = 64 leptons → sub_x = 128 + 64 = 192 (mid-cell).
    tick_movement(&mut entities, 125, &test_interner());

    let entity = entities.get(1).expect("entity exists");
    assert_eq!(
        entity.position.rx, 0,
        "Should not have moved to next cell yet"
    );
    assert_eq!(entity.position.ry, 0);

    // sub_x should be ~192 (128 center + 64 leptons traveled).
    let sub_x_f32: f32 = entity.position.sub_x.to_num();
    assert!(
        (sub_x_f32 - 192.0).abs() < 2.0,
        "sub_x should be ~192, got {sub_x_f32}"
    );
}

#[test]
fn test_tick_movement_updates_screen_position() {
    let mut entities = EntityStore::new();

    let path: Vec<(u16, u16)> = vec![(5, 5), (6, 5)];
    let mut e = GameEntity::test_default(1, "HTNK", "Americans", 5, 5);
    e.movement_target = Some(MovementTarget {
        path,
        path_layers: vec![MovementLayer::Ground; 2],
        next_index: 1,
        speed: SimFixed::from_num(1280), // 5 cells/sec in leptons.
        move_dir_x: SimFixed::from_num(256),
        move_dir_y: SIM_ZERO,
        move_dir_len: SimFixed::from_num(256),
        ..Default::default()
    });
    e.facing = 64;
    entities.insert(e);

    tick_movement(&mut entities, 1000, &test_interner());

    let entity = entities.get(1).expect("entity exists");
    // lepton_to_screen = CoordsToClient(cell_center) = iso_to_screen + (30, 15).
    let (corner_sx, corner_sy): (f32, f32) = terrain::iso_to_screen(6, 5, 0);
    assert!((entity.position.screen_x - (corner_sx + 30.0)).abs() < 1.0);
    assert!((entity.position.screen_y - corner_sy).abs() < 1.0);
}

#[test]
fn test_tick_movement_updates_facing() {
    let mut entities = EntityStore::new();

    // Path goes east then south.
    let path: Vec<(u16, u16)> = vec![(0, 0), (1, 0), (1, 1)];
    let mut e = GameEntity::test_default(1, "HTNK", "Americans", 0, 0);
    e.movement_target = Some(MovementTarget {
        path,
        path_layers: vec![MovementLayer::Ground; 3],
        next_index: 1,
        speed: SimFixed::from_num(1280), // 5 cells/sec in leptons.
        move_dir_x: SimFixed::from_num(256),
        move_dir_y: SIM_ZERO,
        move_dir_len: SimFixed::from_num(256),
        ..Default::default()
    });
    e.facing = 64; // Initially facing east.
    entities.insert(e);

    // Move to (1,0). Next cell is (1,1), delta (0,1) = south → facing 128.
    tick_movement(&mut entities, 300, &test_interner());

    let entity = entities.get(1).expect("entity exists");
    assert_eq!(entity.facing, 128, "Should face south after first step");
}

#[test]
fn test_issue_move_command_sets_path() {
    let mut entities = EntityStore::new();
    let grid: PathGrid = PathGrid::new(20, 20);

    let e = GameEntity::test_default(1, "HTNK", "Americans", 2, 3);
    entities.insert(e);

    let result: bool = issue_move_command(
        &mut entities,
        &grid,
        1,
        (7, 3),
        SimFixed::from_num(768), // 3 cells/sec × 256 = 768 leptons/sec.
        false,
        None,
        None,
    );
    assert!(result, "Should find a path on open grid");

    let entity = entities.get(1).expect("entity exists");
    let target = entity
        .movement_target
        .as_ref()
        .expect("should have MovementTarget");
    assert_eq!(*target.path.first().expect("non-empty"), (2, 3));
    assert_eq!(*target.path.last().expect("non-empty"), (7, 3));
    assert_eq!(target.next_index, 1);
    assert_eq!(target.speed, SimFixed::from_num(768));
}

#[test]
fn test_issue_move_command_no_path() {
    let mut entities = EntityStore::new();
    let mut grid: PathGrid = PathGrid::new(10, 10);

    // Block column 5 completely.
    for y in 0..10 {
        grid.set_blocked(5, y, true);
    }

    let e = GameEntity::test_default(1, "HTNK", "Americans", 0, 0);
    entities.insert(e);

    let result: bool = issue_move_command(
        &mut entities,
        &grid,
        1,
        (9, 9),
        SimFixed::from_num(768),
        false,
        None,
        None,
    );
    assert!(!result, "Should fail with blocked path");
    let entity = entities.get(1).expect("entity exists");
    assert!(
        entity.movement_target.is_none(),
        "Should not have MovementTarget when no path found"
    );
}

#[test]
fn test_issue_move_command_queue_appends_waypoint_path() {
    let mut entities = EntityStore::new();
    let grid: PathGrid = PathGrid::new(32, 32);

    let e = GameEntity::test_default(1, "HTNK", "Americans", 2, 2);
    entities.insert(e);

    assert!(issue_move_command(
        &mut entities,
        &grid,
        1,
        (8, 2),
        SimFixed::from_num(768),
        false,
        None,
        None,
    ));
    assert!(issue_move_command(
        &mut entities,
        &grid,
        1,
        (12, 2),
        SimFixed::from_num(768),
        true,
        None,
        None,
    ));

    let entity = entities.get(1).expect("entity exists");
    let movement = entity
        .movement_target
        .as_ref()
        .expect("should keep movement target");
    assert_eq!(
        movement.path.last().copied(),
        Some((12, 2)),
        "Queued command should append final waypoint"
    );
    assert!(
        movement.path.len() > 7,
        "Queued command should extend path beyond initial destination"
    );
}

#[test]
fn test_tick_movement_repaths_when_next_cell_becomes_blocked() {
    let mut entities = EntityStore::new();
    let mut grid: PathGrid = PathGrid::new(8, 8);

    let e = GameEntity::test_default(1, "HTNK", "Americans", 1, 1);
    entities.insert(e);

    assert!(issue_move_command(
        &mut entities,
        &grid,
        1,
        (5, 1),
        SimFixed::from_num(1024),
        false,
        None,
        None,
    ));

    // Simulate a dynamic blocker appearing on the immediate next step.
    grid.set_blocked(2, 1, true);

    // With blockage_path_delay_ticks=60, the entity must wait 60 ticks for
    // blocked_delay to expire before a repath is attempted. After a successful
    // repath, it needs additional ticks to travel the detour to (5,1).
    for _ in 0..80 {
        tick_movement_with_grid(
            &mut entities,
            Some(&grid),
            &Default::default(),
            &Default::default(),
            &mut SimRng::new(0),
            250,
            0,
            &test_interner(),
        );
    }

    let entity = entities.get(1).expect("entity exists");
    assert_eq!(
        (entity.position.rx, entity.position.ry),
        (5, 1),
        "Entity should recover and reach destination after repath"
    );
}

#[test]
fn test_tick_movement_no_stacking_same_target_cell() {
    let mut entities = EntityStore::new();

    let mut e1 = GameEntity::test_default(1, "HTNK", "Americans", 1, 1);
    e1.movement_target = Some(MovementTarget {
        path: vec![(1, 1), (2, 1)],
        path_layers: vec![MovementLayer::Ground; 2],
        next_index: 1,
        speed: SimFixed::from_num(1024), // 4 cells/sec in leptons.
        move_dir_x: SimFixed::from_num(256),
        move_dir_y: SIM_ZERO,
        move_dir_len: SimFixed::from_num(256),
        ..Default::default()
    });
    e1.facing = 64;
    entities.insert(e1);

    let mut e2 = GameEntity::test_default(2, "HTNK", "Americans", 1, 2);
    e2.movement_target = Some(MovementTarget {
        path: vec![(1, 2), (2, 1)],
        path_layers: vec![MovementLayer::Ground; 2],
        next_index: 1,
        speed: SimFixed::from_num(1024), // 4 cells/sec in leptons.
        move_dir_x: SimFixed::from_num(256),
        move_dir_y: SimFixed::from_num(-256),
        move_dir_len: SimFixed::from_num(362), // ~sqrt(256^2 + 256^2)
        ..Default::default()
    });
    e2.facing = 64;
    entities.insert(e2);

    tick_movement_with_grid(
        &mut entities,
        None,
        &Default::default(),
        &Default::default(),
        &mut SimRng::new(0),
        1000,
        0,
        &test_interner(),
    );

    let ent1 = entities.get(1).expect("e1 exists");
    let ent2 = entities.get(2).expect("e2 exists");
    assert_eq!(
        (ent1.position.rx, ent1.position.ry),
        (2, 1),
        "first mover should claim destination"
    );
    assert_eq!(
        (ent2.position.rx, ent2.position.ry),
        (1, 2),
        "second mover should stay blocked"
    );
}

#[test]
fn test_repath_cooldown_prevents_thrashing_on_unrecoverable_block() {
    let mut entities = EntityStore::new();
    let mut grid: PathGrid = PathGrid::new(8, 8);

    let e = GameEntity::test_default(1, "HTNK", "Americans", 1, 1);
    entities.insert(e);

    assert!(issue_move_command(
        &mut entities,
        &grid,
        1,
        (5, 1),
        SimFixed::from_num(1024),
        false,
        None,
        None,
    ));

    // Make the route unreachable after order assignment.
    grid.set_blocked(2, 1, true);
    grid.set_blocked(2, 0, true);
    grid.set_blocked(2, 2, true);

    // With blockage_path_delay_ticks=60, the first blocked tick sets
    // blocked_delay=60. We must exhaust it before a repath is attempted.
    // Run 61 ticks: 60 to count down blocked_delay, then 1 more for the
    // repath attempt (which fails since route is fully blocked).
    for _ in 0..61 {
        tick_movement_with_grid(
            &mut entities,
            Some(&grid),
            &Default::default(),
            &Default::default(),
            &mut SimRng::new(0),
            250,
            0,
            &test_interner(),
        );
    }
    let entity = entities.get(1).expect("entity exists");
    let m1 = entity
        .movement_target
        .as_ref()
        .expect("movement target should still exist");
    // After failed repath, movement_delay should be set (PathDelay default=9).
    assert!(
        m1.movement_delay > 0,
        "movement_delay {} should be > 0 after failed repath",
        m1.movement_delay,
    );
    let delay_after_fail = m1.movement_delay;

    // Next tick: movement_delay decrements; no immediate repath retrigger.
    tick_movement_with_grid(
        &mut entities,
        Some(&grid),
        &Default::default(),
        &Default::default(),
        &mut SimRng::new(0),
        250,
        0,
        &test_interner(),
    );
    let entity = entities.get(1).expect("entity exists");
    let m2 = entity
        .movement_target
        .as_ref()
        .expect("movement target should still exist");
    assert_eq!(m2.movement_delay, delay_after_fail - 1);
}

#[test]
fn test_dynamic_occupancy_repath_routes_around_stationary_blocker() {
    let mut entities = EntityStore::new();
    let grid: PathGrid = PathGrid::new(10, 10);

    // Stationary blocker at (3,4). Different owner so bump doesn't apply.
    let blocker = GameEntity::test_default(1, "HTNK", "Soviet", 3, 4);
    entities.insert(blocker);

    let mover = GameEntity::test_default(2, "HTNK", "Americans", 1, 4);
    entities.insert(mover);

    assert!(issue_move_command(
        &mut entities,
        &grid,
        2,
        (7, 4),
        SimFixed::from_num(1024),
        false,
        None,
        None,
    ));

    // With blockage_path_delay_ticks=60, the mover must wait ~60 ticks after
    // hitting the occupied cell before a repath is attempted. After repath
    // succeeds, it needs additional ticks to travel the detour to (7,4).
    let mut saw_repath_success = false;
    for _ in 0..80 {
        let stats = tick_movement_with_grid(
            &mut entities,
            Some(&grid),
            &Default::default(),
            &Default::default(),
            &mut SimRng::new(0),
            250,
            0,
            &test_interner(),
        );
        if stats.repath_successes > 0 {
            saw_repath_success = true;
        }
    }

    let entity = entities.get(2).expect("mover should still exist");
    assert_eq!(
        (entity.position.rx, entity.position.ry),
        (7, 4),
        "Mover should reach destination by routing around occupied cell"
    );
    assert!(
        saw_repath_success,
        "Should perform at least one dynamic repath"
    );
}

#[test]
fn test_stuck_recovery_clears_unreachable_movement_target() {
    let mut entities = EntityStore::new();
    let mut grid: PathGrid = PathGrid::new(7, 7);
    for y in 0..7 {
        for x in 0..7 {
            if y != 3 {
                grid.set_blocked(x, y, true);
            }
        }
    }

    // Stationary blocker at (3,3). Different owner so bump doesn't apply.
    let blocker = GameEntity::test_default(1, "HTNK", "Soviet", 3, 3);
    entities.insert(blocker);

    let mover = GameEntity::test_default(2, "HTNK", "Americans", 1, 3);
    entities.insert(mover);

    assert!(issue_move_command(
        &mut entities,
        &grid,
        2,
        (5, 3),
        SimFixed::from_num(1024),
        false,
        None,
        None,
    ));

    // path_stuck_counter starts at 10 (PATH_STUCK_INIT). Each failed repath
    // decrements it by 1 and resets blocked_delay to 60. With both
    // blocked_delay=60 and path_delay_ticks=9 counting down simultaneously,
    // each cycle takes ~61 ticks. 10 failed repaths × 61 ticks ≈ 612 ticks.
    let mut recovered = false;
    for _ in 0..700 {
        let stats = tick_movement_with_grid(
            &mut entities,
            Some(&grid),
            &Default::default(),
            &Default::default(),
            &mut SimRng::new(0),
            250,
            0,
            &test_interner(),
        );
        if stats.stuck_recoveries > 0 {
            recovered = true;
            break;
        }
    }

    assert!(
        recovered,
        "Stuck recovery should trigger for permanent deadlock"
    );
    let entity = entities.get(2).expect("mover exists");
    assert!(
        entity.movement_target.is_none(),
        "MovementTarget should be removed after stuck recovery"
    );
    assert_ne!(
        (entity.position.rx, entity.position.ry),
        (5, 3),
        "Stuck recovery should stop before unreachable destination"
    );
}

#[test]
fn test_movement_tick_stats_report_blocked_attempts() {
    let mut entities = EntityStore::new();
    let grid: PathGrid = PathGrid::new(8, 8);

    // Stationary blocker at (2,2) owned by a different house so bump won't trigger.
    let blocker = GameEntity::test_default(1, "HTNK", "Soviets", 2, 2);
    entities.insert(blocker);

    let mover = GameEntity::test_default(2, "HTNK", "Americans", 1, 2);
    entities.insert(mover);

    assert!(issue_move_command(
        &mut entities,
        &grid,
        2,
        (4, 2),
        SimFixed::from_num(1024),
        false,
        None,
        None,
    ));

    let stats = tick_movement_with_grid(
        &mut entities,
        Some(&grid),
        &Default::default(),
        &Default::default(),
        &mut SimRng::new(0),
        250,
        0,
        &test_interner(),
    );
    assert_eq!(stats.movers_total, 1);
    assert_eq!(stats.moved_steps, 0);
    assert_eq!(stats.blocked_attempts, 1);
}

#[test]
fn test_friendly_scatter_issues_move_command() {
    // A friendly stationary blocker should receive a scatter movement
    // command — the blocker walks away instead of being teleported.
    let mut entities = EntityStore::new();
    let grid: PathGrid = PathGrid::new(8, 8);

    // Stationary friendly blocker at (2,2).
    let blocker = GameEntity::test_default(1, "HTNK", "Americans", 2, 2);
    entities.insert(blocker);

    let mover = GameEntity::test_default(2, "HTNK", "Americans", 1, 2);
    entities.insert(mover);

    assert!(issue_move_command(
        &mut entities,
        &grid,
        2,
        (4, 2),
        SimFixed::from_num(1024),
        false,
        None,
        None,
    ));

    let stats = tick_movement_with_grid(
        &mut entities,
        Some(&grid),
        &Default::default(),
        &Default::default(),
        &mut SimRng::new(0),
        250,
        0,
        &test_interner(),
    );
    assert_eq!(stats.movers_total, 1);
    // Scatter succeeded: blocker was given a movement command.
    assert_eq!(stats.scatter_successes, 1);
    // Blocker should still be at (2,2) but now has a movement_target
    // (it walks away on subsequent ticks, not teleported).
    let bl = entities.get(1).expect("blocker exists");
    assert!(
        bl.movement_target.is_some(),
        "Blocker should have a scatter movement command"
    );
    assert_eq!(
        (bl.position.rx, bl.position.ry),
        (2, 2),
        "Blocker position unchanged this tick — walks next tick"
    );
}

// --- Friendly-passable pathfinding tests ---

#[test]
fn test_friendly_passable_moving_unit_not_blocked() {
    // A moving friendly unit should NOT appear in the entity block set.
    use crate::map::houses::HouseAllianceMap;
    use crate::sim::movement::bump_crush;

    let mut entities = EntityStore::new();
    let _grid = PathGrid::new(10, 10);

    // Unit A: stationary friendly at (3, 0).
    let a = GameEntity::test_default(1, "HTNK", "Americans", 3, 0);
    entities.insert(a);

    // Unit B: moving friendly at (4, 0) — has a movement target.
    let mut b = GameEntity::test_default(2, "HTNK", "Americans", 4, 0);
    b.movement_target = Some(MovementTarget {
        path: vec![(4, 0), (5, 0), (6, 0)],
        path_layers: vec![MovementLayer::Ground; 3],
        next_index: 1,
        speed: SimFixed::from_num(1024),
        move_dir_x: SimFixed::from_num(256),
        move_dir_y: SIM_ZERO,
        move_dir_len: SimFixed::from_num(256),
        ..Default::default()
    });
    entities.insert(b);

    let alliances = HouseAllianceMap::new();
    let blocks =
        bump_crush::build_entity_block_set(&entities, "Americans", &alliances, &test_interner());

    // Stationary friendly at (3,0) should be blocked.
    assert!(blocks.contains(&(3, 0)), "Stationary friendly should block");
    // Moving friendly at (4,0) should NOT be blocked.
    assert!(
        !blocks.contains(&(4, 0)),
        "Moving friendly should be passable"
    );
}

#[test]
fn test_enemy_unit_always_blocks_even_when_moving() {
    use crate::map::houses::HouseAllianceMap;
    use crate::sim::movement::bump_crush;

    let mut entities = EntityStore::new();

    // Enemy unit moving at (3, 0).
    let mut enemy = GameEntity::test_default(1, "HTNK", "Russians", 3, 0);
    enemy.movement_target = Some(MovementTarget {
        path: vec![(3, 0), (4, 0)],
        path_layers: vec![MovementLayer::Ground; 2],
        next_index: 1,
        speed: SimFixed::from_num(1024),
        move_dir_x: SimFixed::from_num(256),
        move_dir_y: SIM_ZERO,
        move_dir_len: SimFixed::from_num(256),
        ..Default::default()
    });
    entities.insert(enemy);

    let alliances = HouseAllianceMap::new();
    let blocks =
        bump_crush::build_entity_block_set(&entities, "Americans", &alliances, &test_interner());

    // Enemy at (3,0) should block even though it's moving.
    assert!(blocks.contains(&(3, 0)), "Moving enemy should still block");
}

#[test]
fn test_friendly_passable_path_goes_through_moving_friendly() {
    // Unit should be able to pathfind THROUGH a moving friendly's cell.
    use crate::sim::pathfinding::find_path_with_costs;
    use std::collections::BTreeSet;

    let grid = PathGrid::new(10, 3);
    // Only block (3,1) — force path through row 0.
    let mut blocks: BTreeSet<(u16, u16)> = BTreeSet::new();
    // (3,0) has a moving friendly — NOT in blocks.
    // (3,1) is a stationary friendly — in blocks.
    blocks.insert((3, 1));

    let path = find_path_with_costs(&grid, (0, 0), (6, 0), None, Some(&blocks), None, None);
    assert!(
        path.is_some(),
        "Should find path through moving-friendly cell"
    );
    let path = path.unwrap();
    // Path can go through (3,0) since it's not blocked (moving friendly).
    assert_eq!(path.last(), Some(&(6, 0)));
}

// --- 24-step path segmentation tests ---

#[test]
fn test_short_path_no_truncation() {
    // A 5-step path (well under 24) should be delivered intact.
    let mut entities = EntityStore::new();
    let grid: PathGrid = PathGrid::new(32, 32);

    let e = GameEntity::test_default(1, "HTNK", "Americans", 0, 0);
    entities.insert(e);

    assert!(issue_move_command(
        &mut entities,
        &grid,
        1,
        (5, 0),
        SimFixed::from_num(1024),
        false,
        None,
        None,
    ));

    let entity = entities.get(1).expect("entity exists");
    let target = entity.movement_target.as_ref().expect("has target");
    assert_eq!(
        target.path.len(),
        6,
        "5-step path = 6 entries (start + 5 moves)"
    );
    assert_eq!(target.final_goal, Some((5, 0)));
}

#[test]
fn test_long_path_truncated_to_24_steps() {
    // A path longer than 24 steps should be truncated to 25 entries.
    let mut entities = EntityStore::new();
    let grid: PathGrid = PathGrid::new(50, 1);

    let e = GameEntity::test_default(1, "HTNK", "Americans", 0, 0);
    entities.insert(e);

    assert!(issue_move_command(
        &mut entities,
        &grid,
        1,
        (40, 0),
        SimFixed::from_num(1024),
        false,
        None,
        None,
    ));

    let entity = entities.get(1).expect("entity exists");
    let target = entity.movement_target.as_ref().expect("has target");
    // Path truncated: 24 steps + start = 25 entries.
    assert_eq!(
        target.path.len(),
        25,
        "Long path should be truncated to 25 entries"
    );
    assert_eq!(target.path[0], (0, 0), "Path starts at origin");
    assert_eq!(target.path[24], (24, 0), "Path ends at 24th step");
    assert_eq!(target.final_goal, Some((40, 0)), "Final goal preserved");
}

#[test]
fn test_segment_exhaustion_triggers_auto_repath() {
    // Walk a truncated 24-step segment, verify auto-repath continues to final goal.
    let mut entities = EntityStore::new();
    let grid: PathGrid = PathGrid::new(50, 1);

    let e = GameEntity::test_default(1, "HTNK", "Americans", 0, 0);
    entities.insert(e);

    assert!(issue_move_command(
        &mut entities,
        &grid,
        1,
        (30, 0),
        SimFixed::from_num(15360), // Very fast — finishes segment quickly.
        false,
        None,
        None,
    ));

    // Tick enough times to exhaust the first 24-step segment and auto-repath.
    for _ in 0..30 {
        tick_movement_with_grid(
            &mut entities,
            Some(&grid),
            &Default::default(),
            &Default::default(),
            &mut SimRng::new(0),
            250,
            0,
            &test_interner(),
        );
    }

    let entity = entities.get(1).expect("entity exists");
    assert_eq!(
        (entity.position.rx, entity.position.ry),
        (30, 0),
        "Entity should reach final destination via auto-repath"
    );
    assert!(
        entity.movement_target.is_none(),
        "Movement should be complete"
    );
}

#[test]
fn test_exact_24_step_path_no_repath_needed() {
    // A path of exactly 24 steps should complete without needing auto-repath.
    let mut entities = EntityStore::new();
    let grid: PathGrid = PathGrid::new(50, 1);

    let e = GameEntity::test_default(1, "HTNK", "Americans", 0, 0);
    entities.insert(e);

    assert!(issue_move_command(
        &mut entities,
        &grid,
        1,
        (24, 0),
        SimFixed::from_num(15360),
        false,
        None,
        None,
    ));

    let entity = entities.get(1).expect("entity exists");
    let target = entity.movement_target.as_ref().expect("has target");
    assert_eq!(target.path.len(), 25, "24-step path = 25 entries");

    // Walk the full path.
    for _ in 0..20 {
        tick_movement_with_grid(
            &mut entities,
            Some(&grid),
            &Default::default(),
            &Default::default(),
            &mut SimRng::new(0),
            250,
            0,
            &test_interner(),
        );
    }

    let entity = entities.get(1).expect("entity exists");
    assert_eq!(
        (entity.position.rx, entity.position.ry),
        (24, 0),
        "Should reach destination"
    );
    assert!(entity.movement_target.is_none(), "Movement should be done");
}

#[test]
fn test_auto_repath_fails_entity_stops() {
    // If auto-repath fails (goal unreachable after segment), entity should stop.
    let mut entities = EntityStore::new();
    let mut grid: PathGrid = PathGrid::new(50, 3);

    let e = GameEntity::test_default(1, "HTNK", "Americans", 0, 1);
    entities.insert(e);

    assert!(issue_move_command(
        &mut entities,
        &grid,
        1,
        (40, 1),
        SimFixed::from_num(15360),
        false,
        None,
        None,
    ));

    // After the path is issued, block column 25 completely so repath fails.
    for y in 0..3 {
        grid.set_blocked(25, y, true);
    }

    // Tick enough to exhaust the first segment (reaches cell 24) and attempt repath.
    for _ in 0..30 {
        tick_movement_with_grid(
            &mut entities,
            Some(&grid),
            &Default::default(),
            &Default::default(),
            &mut SimRng::new(0),
            250,
            0,
            &test_interner(),
        );
    }

    let entity = entities.get(1).expect("entity exists");
    // Entity should have stopped — either at segment end or earlier.
    assert!(
        entity.movement_target.is_none(),
        "Movement should be cleared when repath fails"
    );
    assert!(
        entity.position.rx <= 24,
        "Entity should not pass the blocked column"
    );
}

#[test]
fn test_blocked_repath_uses_final_goal_not_segment_end() {
    // When blocked mid-segment, repath should target final_goal, not segment end.
    let mut entities = EntityStore::new();
    let grid: PathGrid = PathGrid::new(50, 5);

    let e = GameEntity::test_default(1, "HTNK", "Americans", 0, 2);
    entities.insert(e);

    assert!(issue_move_command(
        &mut entities,
        &grid,
        1,
        (40, 2),
        SimFixed::from_num(1024),
        false,
        None,
        None,
    ));

    let entity = entities.get(1).expect("entity exists");
    let target = entity.movement_target.as_ref().expect("has target");
    assert_eq!(target.final_goal, Some((40, 2)));
    // The segment path ends at (24, 2), but final_goal is (40, 2).
    assert_eq!(target.path.last(), Some(&(24, 2)));
}
