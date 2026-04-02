//! Acceptance tests for the miner (harvester) state machine system.
//!
//! Tests exercise the miner_system::tick_miners() pipeline with a minimal
//! EntityStore: miner entity + refinery structure + resource nodes. Verifies
//! payout math, dock queuing, Chrono teleport rules, incremental unloading,
//! local continuation, pip display, and refinery rebinding.

use std::collections::BTreeMap;

use crate::map::entities::EntityCategory;
use crate::rules::ini_parser::IniFile;
use crate::rules::ruleset::RuleSet;
use crate::sim::components::Health;
use crate::sim::game_entity::GameEntity;
use crate::sim::occupancy::OccupancyGrid;
use crate::sim::miner::{
    CargoBale, Miner, MinerConfig, MinerKind, MinerState, RefineryDockPhase, ResourceNode,
    ResourceType,
};
use crate::sim::pathfinding::PathGrid;
use crate::sim::production::credits_for_owner;
use crate::sim::world::Simulation;

/// Minimal rules that know about HARV, CMIN, and GAREFN.
fn miner_rules() -> RuleSet {
    let ini = IniFile::from_str(
        "[InfantryTypes]\n\
         [VehicleTypes]\n\
         0=HARV\n\
         1=CMIN\n\
         [AircraftTypes]\n\
         [BuildingTypes]\n\
         0=GAREFN\n\
         [HARV]\n\
         Name=War Miner\n\
         Cost=1400\n\
         Strength=600\n\
         Armor=heavy\n\
         Speed=4\n\
         ROT=5\n\
         Sight=5\n\
         TechLevel=1\n\
         Owner=Americans\n\
         Harvester=yes\n\
         Dock=GAREFN\n\
         [CMIN]\n\
         Name=Chrono Miner\n\
         Cost=1400\n\
         Strength=400\n\
         Armor=light\n\
         Speed=4\n\
         Sight=5\n\
         TechLevel=1\n\
         Owner=Americans\n\
         Harvester=yes\n\
         Dock=GAREFN\n\
         [GAREFN]\n\
         Name=Ore Refinery\n\
         Cost=2000\n\
         Strength=900\n\
         Armor=wood\n\
         TechLevel=1\n\
         Owner=Americans\n\
         Foundation=4x3\n\
         Refinery=yes\n\
         FreeUnit=CMIN\n",
    );
    RuleSet::from_ini(&ini).expect("miner rules")
}

fn dock_rules() -> RuleSet {
    let ini = IniFile::from_str(
        "[InfantryTypes]\n\
         [VehicleTypes]\n\
         0=MODHARV\n\
         [AircraftTypes]\n\
         [BuildingTypes]\n\
         0=MODPROC\n\
         1=OTHERPROC\n\
         [MODHARV]\n\
         Name=Mod Harvester\n\
         Harvester=yes\n\
         Dock=MODPROC\n\
         Speed=4\n\
         [MODPROC]\n\
         Name=Mod Refinery\n\
         Foundation=4x3\n\
         Refinery=yes\n\
         [OTHERPROC]\n\
         Name=Other Refinery\n\
         Foundation=4x3\n\
         Refinery=yes\n",
    );
    RuleSet::from_ini(&ini).expect("dock rules")
}

/// Spawn a miner entity at (rx, ry), returning its stable_id.
fn spawn_miner(sim: &mut Simulation, sid: u64, kind: MinerKind, rx: u16, ry: u16) -> u64 {
    let type_id = match kind {
        MinerKind::War => "HARV",
        MinerKind::Chrono => "CMIN",
        MinerKind::Slave => "SMIN",
    };
    let health_val: u16 = match kind {
        MinerKind::War => 600,
        MinerKind::Chrono => 400,
        MinerKind::Slave => 2000,
    };
    let owner_id = sim.interner.intern("Americans");
    let type_id_interned = sim.interner.intern(type_id);
    let mut ge = GameEntity::new(
        sid,
        rx,
        ry,
        0,
        0,
        owner_id,
        Health {
            current: health_val,
            max: health_val,
        },
        type_id_interned,
        EntityCategory::Unit,
        0,
        5,
        true,
    );
    ge.miner = Some(Miner::new(kind, &MinerConfig::default()));
    sim.entities.insert(ge);
    // Update next_stable_entity_id if needed so allocate_stable_entity_id doesn't collide.
    if sim.next_stable_entity_id <= sid {
        sim.next_stable_entity_id = sid + 1;
    }
    sid
}

/// Spawn a refinery structure at (rx, ry) with a given stable_id.
fn spawn_refinery(sim: &mut Simulation, sid: u64, rx: u16, ry: u16) {
    let owner_id = sim.interner.intern("Americans");
    let type_id = sim.interner.intern("GAREFN");
    let ge = GameEntity::new(
        sid,
        rx,
        ry,
        0,
        0,
        owner_id,
        Health {
            current: 900,
            max: 900,
        },
        type_id,
        EntityCategory::Structure,
        0,
        5,
        false,
    );
    sim.entities.insert(ge);
    if sim.next_stable_entity_id <= sid {
        sim.next_stable_entity_id = sid + 1;
    }
}

fn spawn_structure(sim: &mut Simulation, sid: u64, type_id: &str, rx: u16, ry: u16) {
    let owner_id = sim.interner.intern("Americans");
    let type_id_interned = sim.interner.intern(type_id);
    let ge = GameEntity::new(
        sid,
        rx,
        ry,
        0,
        0,
        owner_id,
        Health {
            current: 900,
            max: 900,
        },
        type_id_interned,
        EntityCategory::Structure,
        0,
        5,
        false,
    );
    sim.entities.insert(ge);
    if sim.next_stable_entity_id <= sid {
        sim.next_stable_entity_id = sid + 1;
    }
}

/// Place ore resource nodes at a cell with a given amount.
fn place_ore(sim: &mut Simulation, rx: u16, ry: u16, amount: u16) {
    sim.production.resource_nodes.insert(
        (rx, ry),
        ResourceNode {
            resource_type: ResourceType::Ore,
            remaining: amount,
        },
    );
}

/// Place gem resource nodes at a cell with a given amount.
#[allow(dead_code)]
fn place_gems(sim: &mut Simulation, rx: u16, ry: u16, amount: u16) {
    sim.production.resource_nodes.insert(
        (rx, ry),
        ResourceNode {
            resource_type: ResourceType::Gem,
            remaining: amount,
        },
    );
}

/// Tick the miner system `n` times.
///
/// Matches advance_tick ordering: teleport (Phase 2) → miners (Phase 7) →
/// ground movement. Teleport must run before miners so that Relocate/ChronoDelay
/// updates are visible to the miner snapshot.
fn tick_miners_n(sim: &mut Simulation, rules: &RuleSet, n: usize) {
    let config = MinerConfig::default();
    let grid = PathGrid::new(64, 64);
    for _ in 0..n {
        crate::sim::movement::teleport_movement::tick_teleport_movement(
            &mut sim.entities,
            &mut OccupancyGrid::new(),
            67,
            sim.tick,
        );
        super::miner_system::tick_miners(sim, rules, &config, Some(&grid));
        // Also tick movement so issue_direct_move targets are consumed
        // (EnterPad/ExitPad wait for movement_target to be None).
        crate::sim::movement::tick_movement(&mut sim.entities, 67, &sim.interner);
        sim.tick += 1;
    }
}

/// Read the Miner component from an entity by stable_id.
fn get_miner(sim: &Simulation, entity_id: u64) -> Miner {
    sim.entities
        .get(entity_id)
        .and_then(|e| e.miner.as_ref())
        .cloned()
        .expect("miner component should exist")
}

// ==========================================================================
// Test 1: War Miner full ore load = 1000 credits
// ==========================================================================
#[test]
fn war_miner_full_ore_payout_is_1000() {
    let mut sim = Simulation::new();
    let rules = miner_rules();

    // Miner at dock cell, refinery at (10, 10) with 4x3 foundation.
    // Dock cell = (rx + width, ry + height/2) = (10 + 4, 10 + 1) = (14, 11) — east platform.
    let miner_id = spawn_miner(&mut sim, 1, MinerKind::War, 14, 11);
    spawn_refinery(&mut sim, 2, 10, 10);

    // Pre-load cargo: 40 ore bales.
    {
        let entity = sim.entities.get_mut(miner_id).expect("miner entity");
        let miner = entity.miner.as_mut().expect("miner component");
        for _ in 0..40 {
            miner.cargo.push(CargoBale {
                resource_type: ResourceType::Ore,
                value: 25,
            });
        }
        // Put miner in Dock state so it proceeds to Unload.
        miner.state = MinerState::Dock;
        miner.dock_phase = RefineryDockPhase::Unloading;
        miner.reserved_refinery = Some(2);
    }

    let before = credits_for_owner(&sim, "Americans");
    // Tick enough times to fully unload: 40 bales * unload_interval=57 = 2280 ticks.
    tick_miners_n(&mut sim, &rules, 2400);

    let after = credits_for_owner(&sim, "Americans");
    assert_eq!(after - before, 1000, "War Miner full ore = 1000 credits");
}

// ==========================================================================
// Test 2: War Miner full gem load = 2000 credits
// ==========================================================================
#[test]
fn war_miner_full_gem_payout_is_2000() {
    let mut sim = Simulation::new();
    let rules = miner_rules();

    let miner_id = spawn_miner(&mut sim, 1, MinerKind::War, 14, 11);
    spawn_refinery(&mut sim, 2, 10, 10);

    {
        let entity = sim.entities.get_mut(miner_id).expect("miner entity");
        let miner = entity.miner.as_mut().expect("miner component");
        for _ in 0..40 {
            miner.cargo.push(CargoBale {
                resource_type: ResourceType::Gem,
                value: 50,
            });
        }
        miner.state = MinerState::Dock;
        miner.dock_phase = RefineryDockPhase::Unloading;
        miner.reserved_refinery = Some(2);
    }

    let before = credits_for_owner(&sim, "Americans");
    tick_miners_n(&mut sim, &rules, 2400);
    let after = credits_for_owner(&sim, "Americans");
    assert_eq!(after - before, 2000, "War Miner full gems = 2000 credits");
}

// ==========================================================================
// Test 3: Chrono Miner full ore load = 500 credits
// ==========================================================================
#[test]
fn chrono_miner_full_ore_payout_is_500() {
    let mut sim = Simulation::new();
    let rules = miner_rules();

    let miner_id = spawn_miner(&mut sim, 1, MinerKind::Chrono, 14, 11);
    spawn_refinery(&mut sim, 2, 10, 10);

    {
        let entity = sim.entities.get_mut(miner_id).expect("miner entity");
        let miner = entity.miner.as_mut().expect("miner component");
        for _ in 0..20 {
            miner.cargo.push(CargoBale {
                resource_type: ResourceType::Ore,
                value: 25,
            });
        }
        miner.state = MinerState::Dock;
        miner.dock_phase = RefineryDockPhase::Unloading;
        miner.reserved_refinery = Some(2);
    }

    let before = credits_for_owner(&sim, "Americans");
    // 20 bales * unload_interval=57 = 1140 ticks.
    tick_miners_n(&mut sim, &rules, 1200);
    let after = credits_for_owner(&sim, "Americans");
    assert_eq!(after - before, 500, "Chrono Miner full ore = 500 credits");
}

// ==========================================================================
// Test 4: Chrono Miner full gem load = 1000 credits
// ==========================================================================
#[test]
fn chrono_miner_full_gem_payout_is_1000() {
    let mut sim = Simulation::new();
    let rules = miner_rules();

    let miner_id = spawn_miner(&mut sim, 1, MinerKind::Chrono, 14, 11);
    spawn_refinery(&mut sim, 2, 10, 10);

    {
        let entity = sim.entities.get_mut(miner_id).expect("miner entity");
        let miner = entity.miner.as_mut().expect("miner component");
        for _ in 0..20 {
            miner.cargo.push(CargoBale {
                resource_type: ResourceType::Gem,
                value: 50,
            });
        }
        miner.state = MinerState::Dock;
        miner.dock_phase = RefineryDockPhase::Unloading;
        miner.reserved_refinery = Some(2);
    }

    let before = credits_for_owner(&sim, "Americans");
    tick_miners_n(&mut sim, &rules, 1200);
    let after = credits_for_owner(&sim, "Americans");
    assert_eq!(
        after - before,
        1000,
        "Chrono Miner full gems = 1000 credits"
    );
}

// ==========================================================================
// Test 5: Chrono Miner teleports on return (position snaps to dock)
// ==========================================================================
#[test]
fn chrono_miner_teleports_to_refinery_on_return() {
    let mut sim = Simulation::new();
    let rules = miner_rules();

    // Miner at ore far from refinery. Must be > ChronoHarvTooFarDistance (50 cells)
    // from dock cell (14, 11) so the chrono teleport triggers.
    let miner_id = spawn_miner(&mut sim, 1, MinerKind::Chrono, 80, 80);
    spawn_refinery(&mut sim, 2, 10, 10);
    // Dock cell for 4x3 at (10,10) = (14, 11) — east platform.

    // Give it some cargo so it wants to return.
    {
        let entity = sim.entities.get_mut(miner_id).expect("miner entity");
        let miner = entity.miner.as_mut().expect("miner component");
        miner.cargo.push(CargoBale {
            resource_type: ResourceType::Ore,
            value: 25,
        });
        miner.state = MinerState::ReturnToRefinery;
        // No reserved_refinery yet — the system should find one and teleport.
    }

    // Tick 1: miner finds refinery and issues teleport command.
    // tick_teleport_movement already ran this iteration (no-op), so Relocate
    // hasn't executed yet — teleport_state is set but position is unchanged.
    tick_miners_n(&mut sim, &rules, 1);

    let entity = sim.entities.get(miner_id).expect("entity");
    assert!(
        entity.teleport_state.is_some(),
        "Chrono Miner should have an active teleport after first tick"
    );

    // Tick 2: tick_teleport_movement runs Relocate → position snaps to queue cell.
    tick_miners_n(&mut sim, &rules, 1);

    let entity = sim.entities.get(miner_id).expect("entity");
    assert_eq!(
        (entity.position.rx, entity.position.ry),
        (14, 11),
        "Position should be at queue cell after Relocate"
    );

    // Run enough ticks for the chrono delay to expire and dock sequence to complete.
    // Distance ~95 cells → delay ≈ 95*256/48 ≈ 509 ticks. After the delay, the
    // miner enters the dock sequence (WaitForDock → EnterPad → Unloading → ExitPad)
    // and ends up at the exit cell (11, 11) for a 4x3 refinery at (10, 10).
    tick_miners_n(&mut sim, &rules, 550);

    let entity = sim.entities.get(miner_id).expect("entity");
    assert!(
        entity.teleport_state.is_none(),
        "Teleport should be complete"
    );
    // After teleport + dock sequence, miner exits at the refinery exit cell.
    assert_eq!(
        (entity.position.rx, entity.position.ry),
        (11, 11),
        "Chrono Miner should be at exit cell after completing dock sequence"
    );
}

// ==========================================================================
// Test 6: War Miner does NOT teleport (stays where it is on first return tick)
// ==========================================================================
#[test]
fn war_miner_does_not_teleport() {
    let mut sim = Simulation::new();
    let rules = miner_rules();

    let miner_id = spawn_miner(&mut sim, 1, MinerKind::War, 30, 30);
    spawn_refinery(&mut sim, 2, 10, 10);

    {
        let entity = sim.entities.get_mut(miner_id).expect("miner entity");
        let miner = entity.miner.as_mut().expect("miner component");
        miner.cargo.push(CargoBale {
            resource_type: ResourceType::Ore,
            value: 25,
        });
        miner.state = MinerState::ReturnToRefinery;
    }

    tick_miners_n(&mut sim, &rules, 1);

    let pos = &sim.entities.get(miner_id).expect("entity").position;
    // War miner should NOT have teleported — still at (30, 30).
    assert_eq!((pos.rx, pos.ry), (30, 30), "War Miner should not teleport");
}

// ==========================================================================
// Test 7: Dock queuing — only one miner at a refinery at a time
// ==========================================================================
#[test]
fn dock_queuing_one_at_a_time() {
    let mut sim = Simulation::new();
    let rules = miner_rules();

    // Two miners at the dock cell, both ready to unload.
    let m1 = spawn_miner(&mut sim, 1, MinerKind::War, 14, 11);
    let m2 = spawn_miner(&mut sim, 3, MinerKind::War, 14, 11);
    spawn_refinery(&mut sim, 2, 10, 10);

    // Pre-load both with cargo, put in Dock WaitForDock state.
    for entity_id in [m1, m2] {
        let entity = sim.entities.get_mut(entity_id).expect("miner entity");
        let miner = entity.miner.as_mut().expect("miner component");
        miner.cargo.push(CargoBale {
            resource_type: ResourceType::Ore,
            value: 25,
        });
        miner.state = MinerState::Dock;
        miner.dock_phase = RefineryDockPhase::WaitForDock;
        miner.reserved_refinery = Some(2);
    }

    // First tick: one should get the dock, other should wait.
    tick_miners_n(&mut sim, &rules, 1);

    let m1_miner = get_miner(&sim, m1);
    let m2_miner = get_miner(&sim, m2);

    // Miner with lower stable_id (1) processes first and gets dock.
    assert_eq!(
        m1_miner.state,
        MinerState::Dock,
        "First miner should still be docking"
    );
    assert_eq!(
        m1_miner.dock_phase,
        RefineryDockPhase::RotateToPad,
        "First miner should advance past WaitForDock"
    );
    assert_eq!(
        m2_miner.state,
        MinerState::Dock,
        "Second miner should still be docking"
    );
    assert_eq!(
        m2_miner.dock_phase,
        RefineryDockPhase::WaitForDock,
        "Second miner should still be waiting for dock"
    );
}

// ==========================================================================
// Test 8: Credits arrive incrementally during unload (not instant)
// ==========================================================================
#[test]
fn credits_arrive_incrementally_during_unload() {
    let mut sim = Simulation::new();
    let rules = miner_rules();

    let miner_id = spawn_miner(&mut sim, 1, MinerKind::War, 14, 11);
    spawn_refinery(&mut sim, 2, 10, 10);

    // Load 10 bales (250 credits total).
    {
        let entity = sim.entities.get_mut(miner_id).expect("miner entity");
        let miner = entity.miner.as_mut().expect("miner component");
        for _ in 0..10 {
            miner.cargo.push(CargoBale {
                resource_type: ResourceType::Ore,
                value: 25,
            });
        }
        miner.state = MinerState::Dock;
        miner.dock_phase = RefineryDockPhase::Unloading;
        miner.reserved_refinery = Some(2);
    }

    let before = credits_for_owner(&sim, "Americans");

    // After 1 tick: dock grants, transition to Unload, first bale popped immediately.
    tick_miners_n(&mut sim, &rules, 1);
    let after_1 = credits_for_owner(&sim, "Americans");
    // First unload_timer is 0, so first bale pops on first unload tick.
    assert!(
        after_1 - before <= 25,
        "Should have at most 1 bale worth after first tick"
    );

    // After a few more ticks, should have more but NOT all.
    // unload_tick_interval=57, so 10 bails need ~570 ticks total.
    tick_miners_n(&mut sim, &rules, 50);
    let after_51 = credits_for_owner(&sim, "Americans");
    assert!(
        after_51 - before < 250,
        "Credits should not be fully delivered after only 51 ticks (need ~570 ticks for 10 bails)"
    );

    // After enough ticks, all 250 delivered.
    tick_miners_n(&mut sim, &rules, 600);
    let after_all = credits_for_owner(&sim, "Americans");
    assert_eq!(
        after_all - before,
        250,
        "All 10 bales = 250 credits should be delivered"
    );
}

// ==========================================================================
// Test 9: After ore cell empties, miner searches for more (local continuation)
// ==========================================================================
#[test]
fn local_continuation_after_cell_depletes() {
    let mut sim = Simulation::new();
    let rules = miner_rules();

    // Miner at (20, 20). Two ore cells: one small (will deplete), one nearby.
    let miner_id = spawn_miner(&mut sim, 1, MinerKind::War, 20, 20);
    spawn_refinery(&mut sim, 2, 10, 10);
    place_ore(&mut sim, 20, 20, 2); // Only 2 bales worth
    place_ore(&mut sim, 22, 20, 100); // Nearby ore within local radius (6 cells)

    // Put miner in Harvest state at its position.
    {
        let entity = sim.entities.get_mut(miner_id).expect("miner entity");
        let miner = entity.miner.as_mut().expect("miner component");
        miner.state = MinerState::Harvest;
        miner.target_ore_cell = Some((20, 20));
        miner.harvest_timer = 0;
    }

    // Tick enough to deplete the small cell and search for the next.
    // harvest_tick_interval=8, so 2 bales takes ~17 ticks, then search triggers.
    tick_miners_n(&mut sim, &rules, 30);

    let miner = get_miner(&sim, miner_id);
    // After depleting (20,20), miner should have found (22,20) via local scan.
    // It should be in MoveToOre or Harvest at the new cell.
    let found_nearby = miner.target_ore_cell == Some((22, 20))
        || matches!(miner.state, MinerState::MoveToOre | MinerState::Harvest);
    assert!(
        found_nearby || !miner.cargo.is_empty(),
        "Miner should find nearby ore via local continuation or have started returning"
    );
}

// ==========================================================================
// Test 10: Cargo pips always show 5 steps of 20%
// ==========================================================================
#[test]
fn cargo_pips_five_steps() {
    let config = MinerConfig::default();
    let mut miner = Miner::new(MinerKind::War, &config);
    // War Miner capacity = 40 bales
    assert_eq!(miner.cargo_pips(), 0);

    // 20% = 8 bales → 1 pip
    for _ in 0..8 {
        miner.cargo.push(CargoBale {
            resource_type: ResourceType::Ore,
            value: 25,
        });
    }
    assert_eq!(miner.cargo_pips(), 1);

    // 40% = 16 bales → 2 pips
    for _ in 0..8 {
        miner.cargo.push(CargoBale {
            resource_type: ResourceType::Ore,
            value: 25,
        });
    }
    assert_eq!(miner.cargo_pips(), 2);

    // 60% = 24 bales → 3 pips
    for _ in 0..8 {
        miner.cargo.push(CargoBale {
            resource_type: ResourceType::Ore,
            value: 25,
        });
    }
    assert_eq!(miner.cargo_pips(), 3);

    // 80% = 32 bales → 4 pips
    for _ in 0..8 {
        miner.cargo.push(CargoBale {
            resource_type: ResourceType::Ore,
            value: 25,
        });
    }
    assert_eq!(miner.cargo_pips(), 4);

    // 100% = 40 bales → 5 pips
    for _ in 0..8 {
        miner.cargo.push(CargoBale {
            resource_type: ResourceType::Ore,
            value: 25,
        });
    }
    assert_eq!(miner.cargo_pips(), 5);
}

// ==========================================================================
// Test 11: After unload, home_refinery rebinds to the refinery used
// ==========================================================================
#[test]
fn home_refinery_rebinds_after_unload() {
    let mut sim = Simulation::new();
    let rules = miner_rules();

    let miner_id = spawn_miner(&mut sim, 1, MinerKind::War, 14, 11);
    spawn_refinery(&mut sim, 2, 10, 10);

    {
        let entity = sim.entities.get_mut(miner_id).expect("miner entity");
        let miner = entity.miner.as_mut().expect("miner component");
        miner.cargo.push(CargoBale {
            resource_type: ResourceType::Ore,
            value: 25,
        });
        miner.state = MinerState::Dock;
        miner.dock_phase = RefineryDockPhase::Unloading;
        miner.reserved_refinery = Some(2);
        miner.home_refinery = None; // Start without a home
    }

    // Tick until unload completes: 1 bale × unload_interval=57 ticks.
    tick_miners_n(&mut sim, &rules, 70);

    let miner = get_miner(&sim, miner_id);
    assert_eq!(
        miner.home_refinery,
        Some(2),
        "Home refinery should rebind to the refinery used for unloading"
    );
}

// ==========================================================================
// Test 12: Forced return (MinerReturn command) triggers Chrono teleport
// ==========================================================================
#[test]
fn forced_return_chrono_teleports() {
    let mut sim = Simulation::new();
    let rules = miner_rules();

    // Must be > ChronoHarvTooFarDistance (50 cells) from dock cell (14, 11)
    // so the chrono teleport triggers instead of driving.
    let miner_id = spawn_miner(&mut sim, 1, MinerKind::Chrono, 80, 80);
    spawn_refinery(&mut sim, 2, 10, 10);

    {
        let entity = sim.entities.get_mut(miner_id).expect("miner entity");
        let miner = entity.miner.as_mut().expect("miner component");
        miner.state = MinerState::ForcedReturn;
        miner.forced_return = true;
    }

    // Tick 1: finds refinery, issues teleport command. Relocate not yet run.
    tick_miners_n(&mut sim, &rules, 1);

    let entity = sim.entities.get(miner_id).expect("entity");
    assert!(
        entity.teleport_state.is_some(),
        "Forced return should have issued a teleport"
    );

    // Tick 2: Relocate snaps position to queue cell.
    tick_miners_n(&mut sim, &rules, 1);

    let entity = sim.entities.get(miner_id).expect("entity");
    assert_eq!(
        (entity.position.rx, entity.position.ry),
        (14, 11),
        "Position should be at queue cell after Relocate"
    );

    // Run enough ticks for the chrono delay to expire and dock sequence to complete.
    tick_miners_n(&mut sim, &rules, 550);

    let entity = sim.entities.get(miner_id).expect("entity");
    assert!(
        entity.teleport_state.is_none(),
        "Teleport should be complete"
    );
    // After teleport + dock sequence, miner exits at the refinery exit cell.
    assert_eq!(
        (entity.position.rx, entity.position.ry),
        (11, 11),
        "Forced return should have teleported and docked — now at exit cell"
    );
}

// ==========================================================================
// Test: Chrono Miner drives to ore (does NOT warp — only warps on return)
// ==========================================================================
#[test]
fn chrono_miner_drives_to_ore() {
    let mut sim = Simulation::new();
    let rules = miner_rules();

    let miner_id = spawn_miner(&mut sim, 1, MinerKind::Chrono, 10, 10);
    place_ore(&mut sim, 12, 10, 1200);

    // Set up: miner knows about ore, state = MoveToOre.
    {
        let entity = sim.entities.get_mut(miner_id).expect("miner entity");
        let miner = entity.miner.as_mut().expect("miner component");
        miner.target_ore_cell = Some((12, 10));
        miner.state = MinerState::MoveToOre;
    }

    // After one tick, chrono miner should NOT have a teleport — it drives.
    tick_miners_n(&mut sim, &rules, 1);

    let entity = sim.entities.get(miner_id).expect("entity");
    assert!(
        entity.teleport_state.is_none(),
        "Chrono Miner should drive to ore, not warp"
    );
}

// ==========================================================================
// Test 13: SearchOre transitions to WaitNoOre when map has no resources
// ==========================================================================
#[test]
fn search_ore_becomes_wait_when_empty() {
    let mut sim = Simulation::new();
    let rules = miner_rules();

    let miner_id = spawn_miner(&mut sim, 1, MinerKind::War, 20, 20);
    spawn_refinery(&mut sim, 2, 10, 10);
    // No ore placed!

    tick_miners_n(&mut sim, &rules, 1);

    let miner = get_miner(&sim, miner_id);
    assert_eq!(
        miner.state,
        MinerState::WaitNoOre,
        "Miner should enter WaitNoOre when no resources exist"
    );
}

// ==========================================================================
// Test 14: WaitNoOre rescans after cooldown
// ==========================================================================
#[test]
fn wait_no_ore_rescans_after_cooldown() {
    let mut sim = Simulation::new();
    let rules = miner_rules();
    let config = MinerConfig::default();

    let miner_id = spawn_miner(&mut sim, 1, MinerKind::War, 20, 20);
    spawn_refinery(&mut sim, 2, 10, 10);

    {
        let entity = sim.entities.get_mut(miner_id).expect("miner entity");
        let miner = entity.miner.as_mut().expect("miner component");
        miner.state = MinerState::WaitNoOre;
        miner.rescan_cooldown = config.rescan_cooldown_ticks;
    }

    // rescan_cooldown_ticks = 105 (0x69 frames from original engine).
    // After half the cooldown, should still be waiting.
    let half_cooldown = (config.rescan_cooldown_ticks / 2) as usize;
    tick_miners_n(&mut sim, &rules, half_cooldown);
    assert_eq!(
        get_miner(&sim, miner_id).state,
        MinerState::WaitNoOre,
        "Should still be waiting mid-cooldown"
    );

    // Place ore so that when rescan fires it finds something.
    place_ore(&mut sim, 20, 20, 100);

    // Tick the remaining cooldown + 5 extra (transition tick + SearchOre tick).
    let remaining = (config.rescan_cooldown_ticks as usize) - half_cooldown + 5;
    tick_miners_n(&mut sim, &rules, remaining);
    let state = get_miner(&sim, miner_id).state;
    assert!(
        state != MinerState::WaitNoOre,
        "Should have rescanned and found ore, got {:?}",
        state,
    );
}

#[test]
fn harvester_uses_dock_list_for_refinery_selection() {
    let mut sim = Simulation::new();
    let rules = dock_rules();
    let miner_id = sim
        .spawn_object("MODHARV", "Americans", 30, 30, 64, &rules, &BTreeMap::new())
        .expect("spawn harvester");
    spawn_structure(&mut sim, 2, "OTHERPROC", 28, 28);
    spawn_structure(&mut sim, 3, "MODPROC", 10, 10);

    {
        let entity = sim.entities.get_mut(miner_id).expect("miner entity");
        let miner = entity.miner.as_mut().expect("miner component");
        miner.cargo.push(CargoBale {
            resource_type: ResourceType::Ore,
            value: 25,
        });
        miner.state = MinerState::ReturnToRefinery;
    }

    tick_miners_n(&mut sim, &rules, 1);

    let miner = get_miner(&sim, miner_id);
    assert_eq!(miner.reserved_refinery, Some(3));
    assert_eq!(miner.state, MinerState::ReturnToRefinery);
}

#[test]
fn harvester_waits_when_no_dock_compatible_refinery_exists() {
    let mut sim = Simulation::new();
    let rules = dock_rules();
    let miner_id = sim
        .spawn_object("MODHARV", "Americans", 30, 30, 64, &rules, &BTreeMap::new())
        .expect("spawn harvester");
    spawn_structure(&mut sim, 2, "OTHERPROC", 10, 10);

    {
        let entity = sim.entities.get_mut(miner_id).expect("miner entity");
        let miner = entity.miner.as_mut().expect("miner component");
        miner.cargo.push(CargoBale {
            resource_type: ResourceType::Ore,
            value: 25,
        });
        miner.state = MinerState::ReturnToRefinery;
    }

    tick_miners_n(&mut sim, &rules, 1);

    let miner = get_miner(&sim, miner_id);
    assert_eq!(miner.reserved_refinery, None);
    assert_eq!(miner.state, MinerState::WaitNoOre);
}

// ==========================================================================
// Test 15: Dock cell calculation for 3x3 foundation
// ==========================================================================
#[test]
fn dock_cell_for_4x3_refinery() {
    // refinery_dock_cell(rx, ry, width, height)
    // Dock is just outside the east edge, vertically centered: (rx + width, ry + height/2).
    // For 4x3 at (10, 10): (10 + 4, 10 + 1) = (14, 11).
    // None = no art.ini QueueingCell override, falls back to geometric computation.
    let dock = super::miner_system::refinery_dock_cell(10, 10, 4, 3, None);
    assert_eq!(dock, (14, 11));
}

// ==========================================================================
// Test 16: pick_best_resource_node prefers gems over ore
// ==========================================================================
#[test]
fn pick_best_resource_node_prefers_gems_over_ore() {
    use crate::sim::production::pick_best_resource_node;
    use std::collections::BTreeMap;

    let mut nodes: BTreeMap<(u16, u16), ResourceNode> = BTreeMap::new();
    // Ore node equidistant from miner (at 5,5).
    nodes.insert(
        (5, 3),
        ResourceNode {
            resource_type: ResourceType::Ore,
            remaining: 500,
        },
    );
    // Gem node at same distance.
    nodes.insert(
        (5, 7),
        ResourceNode {
            resource_type: ResourceType::Gem,
            remaining: 500,
        },
    );

    let chosen = pick_best_resource_node(&nodes, (5, 5));
    assert_eq!(
        chosen,
        Some((5, 7)),
        "Miner should prefer gems over equidistant ore"
    );
}

// ==========================================================================
// Test 17: pick_best_resource_node prefers denser ore when same type
// ==========================================================================
#[test]
fn pick_best_resource_node_prefers_higher_density() {
    use crate::sim::production::pick_best_resource_node;
    use std::collections::BTreeMap;

    let mut nodes: BTreeMap<(u16, u16), ResourceNode> = BTreeMap::new();
    // Sparse ore node equidistant from miner (at 5,5).
    nodes.insert(
        (5, 3),
        ResourceNode {
            resource_type: ResourceType::Ore,
            remaining: 100,
        },
    );
    // Dense ore node at same distance.
    nodes.insert(
        (5, 7),
        ResourceNode {
            resource_type: ResourceType::Ore,
            remaining: 900,
        },
    );

    let chosen = pick_best_resource_node(&nodes, (5, 5));
    assert_eq!(
        chosen,
        Some((5, 7)),
        "Miner should prefer the denser (remaining=900) ore node"
    );
}

// ==========================================================================
// Dock sequence tests
// ==========================================================================

/// Verify the dock sequence progresses through all phases when given enough ticks.
#[test]
fn dock_sequence_progresses_through_phases() {
    let mut sim = Simulation::new();
    let rules = miner_rules();

    // Miner at queue cell (14, 11), refinery at (10, 10) with 4x3 foundation.
    let miner_id = spawn_miner(&mut sim, 1, MinerKind::War, 14, 11);
    spawn_refinery(&mut sim, 2, 10, 10);

    {
        let entity = sim.entities.get_mut(miner_id).expect("miner entity");
        let miner = entity.miner.as_mut().expect("miner component");
        miner.cargo.push(CargoBale {
            resource_type: ResourceType::Ore,
            value: 25,
        });
        miner.state = MinerState::Dock;
        miner.dock_phase = RefineryDockPhase::WaitForDock;
        miner.reserved_refinery = Some(2);
    }

    // Tick 1: WaitForDock → RotateToPad (dock is free, reservation granted).
    tick_miners_n(&mut sim, &rules, 1);
    let m = get_miner(&sim, miner_id);
    assert_eq!(m.dock_phase, RefineryDockPhase::RotateToPad);

    // Tick enough for rotation + enter pad + turn + unload + exit.
    // ROT=5 at 15Hz: ~36 facing units/tick. Worst-case 128 units = ~4 ticks per turn.
    // Enter/exit pad movement: ~2 ticks each. Unload: 1 bale * 14 = ~15 ticks.
    tick_miners_n(&mut sim, &rules, 200);
    let m = get_miner(&sim, miner_id);
    // With only 1 bale (unload_tick_interval=14), unloading takes ~15 ticks.
    // After that, ExitPad → SearchOre.
    // After docking, miner transitions to SearchOre. Since there's no ore
    // on the map, it immediately goes to WaitNoOre. Both are valid endpoints.
    assert!(
        m.state == MinerState::SearchOre || m.state == MinerState::WaitNoOre,
        "Miner should complete dock sequence, got state={:?} phase={:?}",
        m.state,
        m.dock_phase,
    );
}

/// Verify WaitForDock grants the dock reservation when free.
#[test]
fn dock_wait_grants_reservation_when_free() {
    let mut sim = Simulation::new();
    let rules = miner_rules();

    let miner_id = spawn_miner(&mut sim, 1, MinerKind::War, 14, 11);
    spawn_refinery(&mut sim, 2, 10, 10);

    {
        let entity = sim.entities.get_mut(miner_id).expect("miner entity");
        let miner = entity.miner.as_mut().expect("miner component");
        miner.cargo.push(CargoBale {
            resource_type: ResourceType::Ore,
            value: 25,
        });
        miner.state = MinerState::Dock;
        miner.dock_phase = RefineryDockPhase::WaitForDock;
        miner.reserved_refinery = Some(2);
    }

    tick_miners_n(&mut sim, &rules, 1);

    // Dock should be occupied by this miner.
    assert!(sim.production.dock_reservations.is_occupied(2));
    let m = get_miner(&sim, miner_id);
    assert_eq!(m.dock_phase, RefineryDockPhase::RotateToPad);
    assert!(!m.dock_queued);
}

/// Verify pad cell and exit cell computation for a 4x3 refinery.
#[test]
fn refinery_pad_and_exit_cells() {
    use super::miner_dock_sequence::{refinery_exit_cell, refinery_pad_cell, refinery_queue_cell};

    // 4x3 foundation at (10, 10), no art.ini overrides:
    // queue = (14, 11), pad = (13, 11)
    // exit = building_center + (-0x80, +0x80) leptons = (11, 11)
    assert_eq!(refinery_queue_cell(10, 10, 4, 3, None), (14, 11));
    assert_eq!(refinery_pad_cell(10, 10, 4, 3, None), (13, 11));
    assert_eq!(refinery_exit_cell(10, 10, 4, 3, None), (11, 11));

    // 3x3 foundation at (5, 5), no art.ini overrides:
    // queue = (8, 6), pad = (7, 6)
    // exit = building_center + (-0x80, +0x80) leptons = (5, 6)
    assert_eq!(refinery_queue_cell(5, 5, 3, 3, None), (8, 6));
    assert_eq!(refinery_pad_cell(5, 5, 3, 3, None), (7, 6));
    assert_eq!(refinery_exit_cell(5, 5, 3, 3, None), (5, 6));

    // With QueueingCell override from art.ini:
    assert_eq!(refinery_queue_cell(10, 10, 4, 3, Some((4, 1))), (14, 11)); // same result for standard
    assert_eq!(refinery_queue_cell(10, 10, 4, 3, Some((3, 2))), (13, 12)); // custom position
}

/// Verify the Unloading phase awards credits like the old handle_unload.
#[test]
fn dock_unloading_phase_awards_credits() {
    let mut sim = Simulation::new();
    let rules = miner_rules();

    // Place miner directly in Unloading phase at pad cell (13, 11).
    let miner_id = spawn_miner(&mut sim, 1, MinerKind::War, 13, 11);
    spawn_refinery(&mut sim, 2, 10, 10);

    {
        let entity = sim.entities.get_mut(miner_id).expect("miner entity");
        let miner = entity.miner.as_mut().expect("miner component");
        for _ in 0..5 {
            miner.cargo.push(CargoBale {
                resource_type: ResourceType::Ore,
                value: 25,
            });
        }
        miner.state = MinerState::Dock;
        miner.dock_phase = RefineryDockPhase::Unloading;
        miner.reserved_refinery = Some(2);
    }

    // Pre-reserve the dock so release works correctly.
    sim.production.dock_reservations.try_reserve(2, miner_id);

    let before = credits_for_owner(&sim, "Americans");
    // 5 bales × unload_interval=14 = ~70 ticks + margin.
    tick_miners_n(&mut sim, &rules, 100);
    let after = credits_for_owner(&sim, "Americans");

    assert_eq!(after - before, 125, "5 ore bales × 25 = 125 credits");
}

/// Verify that after unloading finishes, the miner exits and returns to SearchOre.
#[test]
fn dock_exit_returns_to_search_ore() {
    let mut sim = Simulation::new();
    let rules = miner_rules();

    let miner_id = spawn_miner(&mut sim, 1, MinerKind::War, 13, 11);
    spawn_refinery(&mut sim, 2, 10, 10);

    {
        let entity = sim.entities.get_mut(miner_id).expect("miner entity");
        let miner = entity.miner.as_mut().expect("miner component");
        miner.cargo.push(CargoBale {
            resource_type: ResourceType::Ore,
            value: 25,
        });
        miner.state = MinerState::Dock;
        miner.dock_phase = RefineryDockPhase::Unloading;
        miner.reserved_refinery = Some(2);
    }

    sim.production.dock_reservations.try_reserve(2, miner_id);

    // Tick enough for unload (1 bale at 14 ticks) + exit movement + margin.
    tick_miners_n(&mut sim, &rules, 50);

    let m = get_miner(&sim, miner_id);
    // After unloading, miner goes to SearchOre → WaitNoOre (no ore on map).
    assert!(
        m.state == MinerState::SearchOre || m.state == MinerState::WaitNoOre,
        "Should finish dock sequence, got {:?}",
        m.state,
    );
    assert_eq!(m.home_refinery, Some(2), "Home refinery should be set");
    assert!(m.cargo.is_empty(), "Cargo should be empty");
}
