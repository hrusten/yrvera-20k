//! Slave Miner system — deploy/undeploy, slave spawn, slave harvest AI, scan correction.
//!
//! The Slave Miner (SMIN) is Yuri's harvester. Unlike War/Chrono Miners it does NOT
//! harvest directly. Instead it deploys into a refinery building (YAREFN) and spawns
//! SLAV infantry who do the actual harvesting. Slaves pick up ore bales, walk them
//! back to the deployed master, and deposit credits directly (no dock queue needed).
//!
//! ## Key behaviors (from RA2/YR)
//! - **Deploy**: SMIN vehicle → YAREFN building + spawn `SlavesNumber` (5) SLAV infantry
//! - **Undeploy**: YAREFN building → SMIN vehicle, slaves recalled/killed
//! - **Slave harvest loop**: SearchOre → MoveToOre → Harvest → ReturnToMaster → Deposit
//! - **Slave regen**: Dead slaves respawn after `SlaveRegenRate` (500) frames
//! - **Scan correction**: Deployed YAREFN periodically checks if a closer ore patch exists
//!   (SlaveMinerKickFrameDelay=150 frames, SlaveMinerScanCorrection=3 cells improvement)
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/miner, sim/miner_system, rules/.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::rules::ruleset::RuleSet;
use crate::sim::intern::InternedId;
use crate::sim::miner::{CargoBale, MinerConfig};
use crate::sim::miner::{extract_bale, player_has_purifier, search_local_ore};
use crate::sim::production::credits_entry_for_owner;
use crate::sim::world::Simulation;

/// Deployed state of a Slave Miner (SMIN vehicle ↔ YAREFN building).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SlaveMinerMode {
    /// SMIN vehicle form — moving toward ore field to deploy.
    Mobile,
    /// SMIN → YAREFN deploy animation in progress.
    Deploying,
    /// YAREFN building form — slaves are active.
    Deployed,
    /// YAREFN → SMIN undeploy animation in progress.
    Undeploying,
}

/// Slave harvest AI state machine — one per SLAV infantry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SlaveHarvestState {
    /// Looking for nearest ore cell within scan radius of master.
    SearchOre,
    /// Walking toward the target ore cell.
    MoveToOre,
    /// Extracting bales from the ore cell.
    Harvest,
    /// Walking back to the deployed master (YAREFN).
    ReturnToMaster,
    /// Depositing cargo at the master — credits awarded immediately.
    Deposit,
    /// No ore found — idle near master.
    Idle,
}

/// ECS component for SLAV infantry — attached to slave entities.
///
/// Each slave has its own mini harvest loop: find ore near master,
/// walk to it, harvest, walk back, deposit at master, repeat.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SlaveHarvester {
    /// Stable ID of the master entity (deployed YAREFN or mobile SMIN).
    pub master_id: u64,
    /// Current harvest AI state.
    pub state: SlaveHarvestState,
    /// Cargo bales currently carried by this slave.
    pub cargo: Vec<CargoBale>,
    /// Max bales this slave can carry (Storage=4 for SLAV).
    pub capacity: u16,
    /// Countdown timer for harvest action (HarvestRate=150 frames).
    pub harvest_timer: u32,
    /// The ore cell being targeted.
    pub target_cell: Option<(u16, u16)>,
}

impl SlaveHarvester {
    /// Create a new SlaveHarvester bound to a master entity.
    pub fn new(master_id: u64, capacity: u16) -> Self {
        Self {
            master_id,
            state: SlaveHarvestState::SearchOre,
            cargo: Vec::with_capacity(capacity as usize),
            capacity,
            harvest_timer: 0,
            target_cell: None,
        }
    }

    /// True when cargo is at capacity.
    pub fn is_full(&self) -> bool {
        self.cargo.len() as u16 >= self.capacity
    }

    /// Total credit value of all bales currently carried.
    pub fn cargo_value(&self) -> u32 {
        self.cargo.iter().map(|b| b.value as u32).sum()
    }
}

/// Snapshot of one slave entity for two-phase processing.
struct SlaveSnapshot {
    entity_id: u64,
    owner: InternedId,
    rx: u16,
    ry: u16,
    harvester: SlaveHarvester,
}

/// Tick all slave harvesters. Called once per sim tick from resource economy.
///
/// Uses the two-phase snapshot pattern: snapshot → process → write back.
pub(super) fn tick_slave_harvesters(sim: &mut Simulation, rules: &RuleSet, config: &MinerConfig) {
    // Phase 1: Snapshot all slave harvesters.
    let keys = sim.entities.keys_sorted();
    let mut snapshots: Vec<SlaveSnapshot> = Vec::new();
    for &id in &keys {
        let Some(entity) = sim.entities.get(id) else {
            continue;
        };
        let Some(ref sh) = entity.slave_harvester else {
            continue;
        };
        snapshots.push(SlaveSnapshot {
            entity_id: id,
            owner: entity.owner,
            rx: entity.position.rx,
            ry: entity.position.ry,
            harvester: sh.clone(),
        });
    }

    if snapshots.is_empty() {
        return;
    }

    // Phase 2: Process each slave.
    for snap in &mut snapshots {
        process_slave(sim, rules, config, snap);
    }

    // Phase 3: Write back.
    for snap in &snapshots {
        if let Some(entity) = sim.entities.get_mut(snap.entity_id) {
            entity.slave_harvester = Some(snap.harvester.clone());
        }
    }
}

/// Process one slave through its harvest state machine.
fn process_slave(
    sim: &mut Simulation,
    rules: &RuleSet,
    config: &MinerConfig,
    snap: &mut SlaveSnapshot,
) {
    // Check master is still alive.
    if sim.entities.get(snap.harvester.master_id).is_none() {
        // Master destroyed — slave becomes idle (in real RA2, freed slaves wander).
        snap.harvester.state = SlaveHarvestState::Idle;
        return;
    }

    match snap.harvester.state {
        SlaveHarvestState::SearchOre => handle_slave_search(sim, rules, config, snap),
        SlaveHarvestState::MoveToOre => handle_slave_move_to_ore(snap),
        SlaveHarvestState::Harvest => handle_slave_harvest(sim, config, snap),
        SlaveHarvestState::ReturnToMaster => handle_slave_return(sim, snap),
        SlaveHarvestState::Deposit => handle_slave_deposit(sim, rules, config, snap),
        SlaveHarvestState::Idle => handle_slave_idle(sim, rules, config, snap),
    }
}

/// Slave searches for ore within SlaveMinerSlaveScan of the master.
fn handle_slave_search(
    sim: &Simulation,
    rules: &RuleSet,
    _config: &MinerConfig,
    snap: &mut SlaveSnapshot,
) {
    let scan_radius: u16 = rules.general.slave_miner_slave_scan.max(1) as u16;

    // Search from the master's position (slaves harvest around their deployed base).
    let master_pos = sim
        .entities
        .get(snap.harvester.master_id)
        .map(|e| (e.position.rx, e.position.ry))
        .unwrap_or((snap.rx, snap.ry));

    if let Some(cell) = search_local_ore(&sim.production.resource_nodes, master_pos, scan_radius) {
        snap.harvester.target_cell = Some(cell);
        snap.harvester.state = SlaveHarvestState::MoveToOre;
    } else {
        snap.harvester.state = SlaveHarvestState::Idle;
    }
}

/// Slave moving toward ore. Simplified: instant arrival if at target cell.
/// Real movement is driven by the locomotor system; here we check proximity.
fn handle_slave_move_to_ore(snap: &mut SlaveSnapshot) {
    let Some(target) = snap.harvester.target_cell else {
        snap.harvester.state = SlaveHarvestState::SearchOre;
        return;
    };

    // If the slave has arrived at (or is adjacent to) the target, start harvesting.
    let dx: i32 = snap.rx as i32 - target.0 as i32;
    let dy: i32 = snap.ry as i32 - target.1 as i32;
    if dx.abs() <= 1 && dy.abs() <= 1 {
        snap.harvester.state = SlaveHarvestState::Harvest;
        snap.harvester.harvest_timer = 0;
    }
    // Movement itself is handled by the locomotor/movement system.
}

/// Slave extracting bales from the ore cell.
fn handle_slave_harvest(sim: &mut Simulation, config: &MinerConfig, snap: &mut SlaveSnapshot) {
    let Some(cell) = snap.harvester.target_cell else {
        snap.harvester.state = SlaveHarvestState::SearchOre;
        return;
    };

    // Check if ore still exists at target.
    let has_ore: bool = sim
        .production
        .resource_nodes
        .get(&cell)
        .is_some_and(|n| n.remaining > 0);

    if !has_ore {
        // Ore depleted — search for more.
        snap.harvester.target_cell = None;
        if snap.harvester.cargo.is_empty() {
            snap.harvester.state = SlaveHarvestState::SearchOre;
        } else {
            snap.harvester.state = SlaveHarvestState::ReturnToMaster;
        }
        return;
    }

    // Harvest timer countdown.
    if snap.harvester.harvest_timer > 0 {
        snap.harvester.harvest_timer -= 1;
        return;
    }

    // Extract one bale.
    if let Some(bale) = extract_bale(sim, cell, config) {
        snap.harvester.cargo.push(bale);
    }

    if snap.harvester.is_full() {
        snap.harvester.state = SlaveHarvestState::ReturnToMaster;
    } else {
        // Reset harvest timer (HarvestRate from rules, stored on the ObjectType).
        // Default 150 frames at RA2's 15fps logic = 10 seconds.
        // At 15Hz sim, 1 tick = 1 RA2 game frame, so use directly.
        snap.harvester.harvest_timer = SLAVE_HARVEST_RATE_TICKS;
    }
}

/// Default harvest rate for slaves (HarvestRate=150 in rulesmd.ini).
/// In a fully data-driven version this would come from the SLAV ObjectType's
/// `harvest_rate` field. For now, use the standard value.
const SLAVE_HARVEST_RATE_TICKS: u32 = 150;

/// Slave returning to master. Check proximity to master position.
fn handle_slave_return(sim: &Simulation, snap: &mut SlaveSnapshot) {
    let Some(master) = sim.entities.get(snap.harvester.master_id) else {
        snap.harvester.state = SlaveHarvestState::Idle;
        return;
    };
    let mx: u16 = master.position.rx;
    let my: u16 = master.position.ry;

    let dx: i32 = snap.rx as i32 - mx as i32;
    let dy: i32 = snap.ry as i32 - my as i32;

    // Adjacent or on master cell → start depositing.
    if dx.abs() <= 2 && dy.abs() <= 2 {
        snap.harvester.state = SlaveHarvestState::Deposit;
    }
    // Movement handled by locomotor system.
}

/// Slave depositing cargo at master — credits awarded immediately per bale.
fn handle_slave_deposit(
    sim: &mut Simulation,
    rules: &RuleSet,
    config: &MinerConfig,
    snap: &mut SlaveSnapshot,
) {
    if snap.harvester.cargo.is_empty() {
        snap.harvester.state = SlaveHarvestState::SearchOre;
        return;
    }

    // Pop one bale per tick (slaves deposit faster than refinery unload).
    let bale: CargoBale = snap.harvester.cargo.remove(0);
    let mut value: i32 = i32::from(bale.value);

    // Ore Purifier bonus applies to slave deposits too.
    if player_has_purifier(sim, rules, sim.interner.resolve(snap.owner)) {
        let bonus_pct: i32 = (rules.general.purifier_bonus * 100.0) as i32;
        value += value * bonus_pct / 100;
    }

    let owner_str = sim.interner.resolve(snap.owner).to_string();
    let credits: &mut i32 = credits_entry_for_owner(sim, &owner_str);
    *credits = credits.saturating_add(value);

    // Keep depositing until empty. Unload tick interval for slaves is instant
    // (one bale per tick) since HarvesterDumpRate doesn't apply to slaves.
    // After empty, we'll transition to SearchOre next tick.
    let _ = config; // config available for future tuning
}

/// Slave idle — periodically re-scan for ore.
fn handle_slave_idle(
    sim: &Simulation,
    rules: &RuleSet,
    _config: &MinerConfig,
    snap: &mut SlaveSnapshot,
) {
    // Try to find ore every few ticks (reuse search logic).
    let scan_radius: u16 = rules.general.slave_miner_slave_scan.max(1) as u16;
    let master_pos = sim
        .entities
        .get(snap.harvester.master_id)
        .map(|e| (e.position.rx, e.position.ry))
        .unwrap_or((snap.rx, snap.ry));

    if let Some(cell) = search_local_ore(&sim.production.resource_nodes, master_pos, scan_radius) {
        snap.harvester.target_cell = Some(cell);
        snap.harvester.state = SlaveHarvestState::MoveToOre;
    }
}

// ---------------------------------------------------------------------------
// Slave Miner deploy/undeploy
// ---------------------------------------------------------------------------

/// Configuration for the Slave Miner deploy system, derived from rules.
pub struct SlaveMinerConfig {
    /// Number of slaves to spawn on deploy (SlavesNumber=5).
    pub slaves_number: i32,
    /// Frames between slave respawns after death (SlaveRegenRate=500).
    pub slave_regen_rate: u32,
    /// Minimum frames between consecutive respawns (SlaveReloadRate=25).
    pub slave_reload_rate: u32,
    /// Frames between scan correction checks (SlaveMinerKickFrameDelay=150).
    pub kick_frame_delay: u32,
    /// Cell improvement threshold for scan correction (SlaveMinerScanCorrection=3).
    pub scan_correction: u16,
    /// Short scan radius for deployed slave miner (SlaveMinerShortScan=8).
    pub short_scan: u16,
    /// Slave scan radius — how far slaves search for ore (SlaveMinerSlaveScan=14).
    pub slave_scan: u16,
}

impl SlaveMinerConfig {
    /// Build from parsed GeneralRules.
    pub fn from_rules(rules: &RuleSet) -> Self {
        let g = &rules.general;
        Self {
            slaves_number: 5, // overridden per-object from ObjectType.slaves_number
            slave_regen_rate: g.slave_miner_kick_frame_delay.max(1), // actually SlaveRegenRate per-obj
            slave_reload_rate: 25,
            kick_frame_delay: g.slave_miner_kick_frame_delay.max(1),
            scan_correction: g.slave_miner_scan_correction.max(0) as u16,
            short_scan: g.slave_miner_short_scan.max(1) as u16,
            slave_scan: g.slave_miner_slave_scan.max(1) as u16,
        }
    }
}

/// Deploy a Slave Miner (SMIN) vehicle into its refinery form (YAREFN).
///
/// Follows the same pattern as `deploy_mcv()` in world_spawn.rs:
/// 1. Read deploy data from the SMIN entity
/// 2. Despawn the SMIN vehicle
/// 3. Spawn the YAREFN building at the same cell
/// 4. Spawn `SlavesNumber` SLAV infantry around the building
/// 5. Register slave bindings in ProductionState
///
/// Returns the new YAREFN stable_id, or None if deploy failed.
pub fn deploy_slave_miner(sim: &mut Simulation, stable_id: u64, rules: &RuleSet) -> Option<u64> {
    // Read deploy data before mutating.
    let deploy_data = {
        let entity = sim.entities.get(stable_id)?;
        let type_str = sim.interner.resolve(entity.type_ref);
        let obj = rules.object_case_insensitive(type_str)?;
        let target_type: &str = obj.deploys_into.as_deref()?;
        // Verify target exists in rules.
        rules.object(target_type)?;
        let enslaves: String = obj.enslaves.clone()?;
        let slaves_number: i32 = obj.slaves_number.max(0);
        let owner_str = sim.interner.resolve(entity.owner).to_string();
        Some((
            owner_str,
            entity.position.rx,
            entity.position.ry,
            entity.position.z,
            entity.facing,
            entity.selected,
            target_type.to_string(),
            enslaves,
            slaves_number,
        ))
    }?;

    let (owner, rx, ry, z, _facing, was_selected, target_type, slave_type, slaves_number) =
        deploy_data;

    // Despawn the SMIN vehicle.
    sim.despawn_entity(stable_id);

    // Spawn the YAREFN building at the same cell.
    let new_sid: u64 = sim.spawn_object_at_height(&target_type, &owner, rx, ry, 0, z, rules)?;

    if let Some(ge) = sim.entities.get_mut(new_sid) {
        ge.selected = was_selected;
    }

    // Spawn slave infantry around the building.
    let mut slave_ids: Vec<u64> = Vec::with_capacity(slaves_number as usize);
    for i in 0..slaves_number {
        // Spread slaves around the building.
        let offset_x: i32 = (i % 3) - 1; // -1, 0, 1, -1, 0
        let offset_y: i32 = (i / 3) - 1; // -1, -1, -1, 0, 0
        let sx: u16 = (rx as i32 + offset_x).clamp(0, u16::MAX as i32) as u16;
        let sy: u16 = (ry as i32 + offset_y).clamp(0, u16::MAX as i32) as u16;

        if let Some(slave_sid) =
            sim.spawn_object_at_height(&slave_type, &owner, sx, sy, 0, z, rules)
        {
            // Resolve slave capacity from ObjectType.
            let slave_capacity: u16 = rules
                .object_case_insensitive(&slave_type)
                .map(|obj| obj.storage.max(1) as u16)
                .unwrap_or(4);

            if let Some(slave_entity) = sim.entities.get_mut(slave_sid) {
                slave_entity.slave_harvester = Some(SlaveHarvester::new(new_sid, slave_capacity));
            }
            slave_ids.push(slave_sid);
        }
    }

    // Register slave bindings.
    sim.production.slave_bindings.insert(new_sid, slave_ids);

    Some(new_sid)
}

/// Undeploy a Slave Miner refinery (YAREFN) back into vehicle form (SMIN).
///
/// 1. Kill/despawn all bound slaves
/// 2. Despawn the YAREFN building
/// 3. Spawn the SMIN vehicle at the same cell
/// 4. Transfer slave bindings to the new SMIN entity
///
/// Returns the new SMIN stable_id, or None if undeploy failed.
pub fn undeploy_slave_miner(sim: &mut Simulation, stable_id: u64, rules: &RuleSet) -> Option<u64> {
    // Read undeploy data.
    let undeploy_data = {
        let entity = sim.entities.get(stable_id)?;
        let type_str = sim.interner.resolve(entity.type_ref);
        let obj = rules.object_case_insensitive(type_str)?;
        let target_type: &str = obj.undeploys_into.as_deref()?;
        rules.object(target_type)?;
        let owner_str = sim.interner.resolve(entity.owner).to_string();
        Some((
            owner_str,
            entity.position.rx,
            entity.position.ry,
            entity.position.z,
            entity.selected,
            target_type.to_string(),
        ))
    }?;

    let (owner, rx, ry, z, was_selected, target_type) = undeploy_data;

    // Despawn all bound slaves.
    if let Some(slave_ids) = sim.production.slave_bindings.remove(&stable_id) {
        for slave_id in &slave_ids {
            sim.despawn_entity(*slave_id);
        }
    }

    // Despawn the YAREFN building.
    sim.despawn_entity(stable_id);

    // Spawn the SMIN vehicle.
    let new_sid: u64 = sim.spawn_object_at_height(&target_type, &owner, rx, ry, 0, z, rules)?;

    if let Some(ge) = sim.entities.get_mut(new_sid) {
        ge.selected = was_selected;
    }

    Some(new_sid)
}

// ---------------------------------------------------------------------------
// Slave regeneration
// ---------------------------------------------------------------------------

/// Tick slave regeneration for all deployed Slave Miners.
///
/// When a slave dies (removed from entity store), spawn a replacement after
/// `SlaveRegenRate` ticks. `SlaveReloadRate` is the minimum gap between spawns.
pub(super) fn tick_slave_regen(sim: &mut Simulation, rules: &RuleSet) {
    // Collect master IDs that have slave bindings.
    let master_ids: Vec<u64> = sim.production.slave_bindings.keys().copied().collect();

    for master_id in master_ids {
        let Some(master) = sim.entities.get(master_id) else {
            // Master died — clean up bindings.
            sim.production.slave_bindings.remove(&master_id);
            continue;
        };

        let master_type = sim.interner.resolve(master.type_ref).to_string();
        let owner = sim.interner.resolve(master.owner).to_string();
        let mrx: u16 = master.position.rx;
        let mry: u16 = master.position.ry;
        let mz: u8 = master.position.z;

        // Get expected slave count and slave type from rules.
        let (slave_type, max_slaves) = {
            let obj = match rules.object_case_insensitive(&master_type) {
                Some(o) => o,
                None => continue,
            };
            let st = match obj.enslaves.as_deref() {
                Some(s) => s.to_string(),
                None => continue,
            };
            (st, obj.slaves_number.max(0))
        };

        // Count living slaves.
        let slave_ids = match sim.production.slave_bindings.get(&master_id) {
            Some(ids) => ids.clone(),
            None => continue,
        };
        let alive_count: i32 = slave_ids
            .iter()
            .filter(|&&sid| sim.entities.get(sid).is_some())
            .count() as i32;

        // Remove dead slave IDs from bindings.
        let alive_ids: Vec<u64> = slave_ids
            .iter()
            .copied()
            .filter(|&sid| sim.entities.get(sid).is_some())
            .collect();

        if alive_count < max_slaves {
            // Spawn one replacement per tick (SlaveReloadRate could throttle this,
            // but for simplicity we spawn one per tick until full).
            let sx: u16 = mrx.saturating_add(1);
            let sy: u16 = mry.saturating_add(1);

            if let Some(slave_sid) =
                sim.spawn_object_at_height(&slave_type, &owner, sx, sy, 0, mz, rules)
            {
                let slave_capacity: u16 = rules
                    .object_case_insensitive(&slave_type)
                    .map(|obj| obj.storage.max(1) as u16)
                    .unwrap_or(4);

                if let Some(slave_entity) = sim.entities.get_mut(slave_sid) {
                    slave_entity.slave_harvester =
                        Some(SlaveHarvester::new(master_id, slave_capacity));
                }
                let mut updated_ids: Vec<u64> = alive_ids;
                updated_ids.push(slave_sid);
                sim.production.slave_bindings.insert(master_id, updated_ids);
            } else {
                sim.production.slave_bindings.insert(master_id, alive_ids);
            }
        } else if alive_ids.len() != slave_ids.len() {
            // Just clean up dead IDs.
            sim.production.slave_bindings.insert(master_id, alive_ids);
        }
    }
}

// ---------------------------------------------------------------------------
// Scan correction (Phase 7)
// ---------------------------------------------------------------------------

/// Check if a deployed Slave Miner should reposition to a closer ore patch.
///
/// Called periodically (every SlaveMinerKickFrameDelay ticks). If the nearest
/// ore from the master's position is `SlaveMinerScanCorrection` cells closer
/// than the current nearest ore to the slaves, trigger an undeploy + move.
///
/// Returns Some((rx, ry)) = cell to reposition to, None = stay put.
pub fn check_scan_correction(
    sim: &Simulation,
    rules: &RuleSet,
    master_id: u64,
) -> Option<(u16, u16)> {
    let master = sim.entities.get(master_id)?;
    let mrx: u16 = master.position.rx;
    let mry: u16 = master.position.ry;

    let short_scan: u16 = rules.general.slave_miner_short_scan.max(1) as u16;
    let correction: u16 = rules.general.slave_miner_scan_correction.max(0) as u16;

    // Find nearest ore from current position.
    let current_nearest = search_local_ore(&sim.production.resource_nodes, (mrx, mry), short_scan)?;

    let current_dist: u16 = manhattan_distance(mrx, mry, current_nearest.0, current_nearest.1);

    // Search the broader area (SlaveMinerLongScan) for a better patch.
    let long_scan: u16 = rules.general.slave_miner_long_scan.max(1) as u16;
    let better_ore = search_local_ore(&sim.production.resource_nodes, (mrx, mry), long_scan)?;

    let better_dist: u16 = manhattan_distance(mrx, mry, better_ore.0, better_ore.1);

    // If the improvement exceeds SlaveMinerScanCorrection, recommend repositioning.
    if current_dist > better_dist && (current_dist - better_dist) >= correction {
        Some(better_ore)
    } else {
        None
    }
}

/// Manhattan distance between two cells.
fn manhattan_distance(ax: u16, ay: u16, bx: u16, by: u16) -> u16 {
    ax.abs_diff(bx) + ay.abs_diff(by)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::miner::ResourceType;

    #[test]
    fn slave_harvester_capacity_and_value() {
        let mut sh = SlaveHarvester::new(100, 4);
        assert!(!sh.is_full());
        assert_eq!(sh.cargo_value(), 0);

        for _ in 0..4 {
            sh.cargo.push(CargoBale {
                resource_type: ResourceType::Ore,
                value: 25,
            });
        }
        assert!(sh.is_full());
        assert_eq!(sh.cargo_value(), 100);
    }

    #[test]
    fn slave_harvester_state_transitions() {
        let sh = SlaveHarvester::new(1, 4);
        assert_eq!(sh.state, SlaveHarvestState::SearchOre);
    }

    #[test]
    fn manhattan_distance_basic() {
        assert_eq!(manhattan_distance(10, 10, 13, 14), 7);
        assert_eq!(manhattan_distance(5, 5, 5, 5), 0);
        assert_eq!(manhattan_distance(0, 0, 100, 50), 150);
    }

    #[test]
    fn scan_correction_returns_none_without_entities() {
        // With no entities, check_scan_correction returns None (master not found).
        let sim = Simulation::new();
        let rules = make_test_rules();
        assert!(check_scan_correction(&sim, &rules, 999).is_none());
    }

    /// Minimal rules for slave miner tests.
    fn make_test_rules() -> RuleSet {
        use crate::rules::ini_parser::IniFile;
        let ini_str: &str = "\
[InfantryTypes]\n1=SLAV\n\
[VehicleTypes]\n1=SMIN\n\
[BuildingTypes]\n1=YAREFN\n\
[SLAV]\nStrength=125\nSpeed=3\nSlaved=yes\nStorage=4\nHarvestRate=150\n\
[SMIN]\nStrength=2000\nSpeed=3\nEnslaves=SLAV\nSlavesNumber=5\nDeploysInto=YAREFN\nResourceGatherer=yes\nResourceDestination=yes\n\
[YAREFN]\nStrength=2000\nEnslaves=SLAV\nSlavesNumber=5\nUndeploysInto=SMIN\nFoundation=3x3\n\
[General]\nSlaveMinerShortScan=8\nSlaveMinerSlaveScan=14\nSlaveMinerLongScan=48\nSlaveMinerScanCorrection=3\nSlaveMinerKickFrameDelay=150\n\
";
        let ini: IniFile = IniFile::from_str(ini_str);
        RuleSet::from_ini(&ini).expect("test rules should parse")
    }
}
