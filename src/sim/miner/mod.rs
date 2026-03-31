//! Ore miner types, configuration, and ECS component.
//!
//! Defines the Miner component (state machine), CargoBale (discrete resource
//! unit), MinerConfig (tunable defaults), and ResourceType. Attached to
//! harvester entities (CMIN = Chrono Miner, HARV = War Miner).
//!
//! ## Dependency rules
//! - Part of sim/ -- may depend on rules/ for data-driven miner detection.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

pub mod miner_dock;
mod miner_dock_sequence;
pub(crate) mod miner_system;

#[cfg(test)]
#[path = "miner_tests.rs"]
mod miner_tests;

pub(crate) use self::miner_system::{extract_bale, player_has_purifier, search_local_ore};

use std::collections::BTreeMap;

use crate::rules::object_type::ObjectType;
use crate::rules::ruleset::GeneralRules;

/// Which kind of resource a map cell or cargo bale contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ResourceType {
    Ore,
    Gem,
}

/// A resource node on the map — tracks type and remaining amount.
///
/// Replaces the old bare `u16` in `resource_nodes` so the sim knows whether
/// a cell contains ore or gems (affects bale value and palette).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResourceNode {
    pub resource_type: ResourceType,
    pub remaining: u16,
}

/// Which miner chassis this entity uses.
/// Determines movement behavior (drive vs chrono-teleport) and cargo capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MinerKind {
    /// Soviet War Miner (HARV): drives both ways, armed, large cargo.
    War,
    /// Allied Chrono Miner (CMIN): drives to ore, teleports back to refinery.
    Chrono,
    /// Yuri Slave Miner (SMIN): deploys into refinery (YAREFN), spawns slave infantry.
    /// Does not harvest directly — slaves harvest and deposit at the deployed building.
    Slave,
}

/// State machine for the miner harvest loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum MinerState {
    /// Looking for the nearest ore/gem cell to harvest.
    SearchOre,
    /// Pathing toward the target ore cell.
    MoveToOre,
    /// Extracting bales from the current cell.
    Harvest,
    /// Heading back (or teleporting) to the assigned refinery.
    ReturnToRefinery,
    /// Waiting in the dock queue outside the refinery.
    Dock,
    /// Incrementally unloading cargo bales into credits.
    Unload,
    /// No ore found anywhere on the map; idle.
    WaitNoOre,
    /// Player issued a manual return order.
    ForcedReturn,
}

/// Sub-state machine for the refinery docking visual sequence.
///
/// Active when `MinerState::Dock` is the current top-level state. Drives the
/// approach → rotate → enter pad → turn → unload → exit choreography that
/// the original game's `BuildingClass::DockingSequence_Update` performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
pub enum RefineryDockPhase {
    /// Moving toward the queue cell (QueueingCell from art.ini).
    #[default]
    Approach,
    /// At/near queue cell, waiting for dock reservation to be granted.
    WaitForDock,
    /// Dock reserved, rotating to face the pad cell (toward building).
    RotateToPad,
    /// Driving onto the refinery pad (inside building footprint).
    EnterPad,
    /// On pad, turning 180° to face outward (exit direction).
    TurnOnPad,
    /// Linked to refinery, incrementally unloading cargo bales into credits.
    Unloading,
    /// Driving off the pad to the exit cell.
    ExitPad,
}

/// One discrete cargo bale carried by a miner.
///
/// Each harvest tick pops one bale worth of resource from the map cell and
/// pushes it into the miner's cargo hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CargoBale {
    pub resource_type: ResourceType,
    pub value: u16,
}

/// Tunable configuration for the miner/refinery/resource system.
///
/// Ship with RA2-like defaults; override for balance mods.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MinerConfig {
    // -- Bale values --
    /// Credits per ore bale.
    pub ore_bale_value: u16,
    /// Credits per gem bale.
    pub gem_bale_value: u16,

    // -- Cargo capacities (in bales) --
    /// War Miner bale capacity (1000 / 25 = 40 bales for ore).
    pub war_miner_capacity: u16,
    /// Chrono Miner bale capacity (500 / 25 = 20 bales for ore).
    pub chrono_miner_capacity: u16,

    // -- Timing (in sim ticks at 15Hz = RA2 game frames) --
    /// Ticks between each harvest action (extract one bale).
    pub harvest_tick_interval: u8,
    /// Ticks between each unload action (deposit one bale).
    pub unload_tick_interval: u8,

    // -- Search radii --
    /// Short scan radius: cells to scan around last harvest cell (TiberiumShortScan).
    pub local_continuation_radius: u16,
    /// Long scan radius: cells to search from current position when short scan fails
    /// (TiberiumLongScan). If this also fails, falls back to unbounded global search.
    pub long_scan_radius: u16,
    /// If the nearest ore is farther than this, the miner considers it "too far"
    /// and will try local continuation first. Standard miners (HarvesterTooFarDistance).
    pub too_far_threshold_standard: u16,
    /// Too-far threshold for Chrono Miners (much larger because they teleport back)
    /// (ChronoHarvTooFarDistance).
    pub too_far_threshold_chrono: u16,
    /// Ticks to wait before re-scanning in WaitNoOre state.
    pub rescan_cooldown_ticks: u8,
}

impl Default for MinerConfig {
    fn default() -> Self {
        Self {
            ore_bale_value: 25,
            gem_bale_value: 50,
            // War Miner: 40 bales * 25 = 1000 ore, 40 * 50 = 2000 gems
            war_miner_capacity: 40,
            // Chrono Miner: 20 bales * 25 = 500 ore, 20 * 50 = 1000 gems
            chrono_miner_capacity: 20,
            // HarvesterLoadRate=2 (frames per StepTimer step). One bale requires
            // 9 steps, so interval = 2 * 9 = 18 frames/bale at 15fps (~1.2s).
            harvest_tick_interval: 18,
            // HarvesterDumpRate=0.016 min/bale x 900 (60s x 15fps) = 14.4 frames/bale.
            // Truncated to 14 ticks. War Miner (40 bales): ~37s. Chrono Miner (20): ~19s.
            unload_tick_interval: 14,
            local_continuation_radius: 6,
            long_scan_radius: 48,
            too_far_threshold_standard: 5,
            too_far_threshold_chrono: 10,
            // TibSun legacy: 0x69 = 105 frames at 15fps logic rate (~7 seconds).
            // Prevents aggressive re-scanning when no ore exists on the map.
            rescan_cooldown_ticks: 105,
        }
    }
}

impl MinerConfig {
    /// Create a MinerConfig from parsed `[General]` rules data.
    ///
    /// Replaces hardcoded defaults with data-driven values from rules.ini.
    /// Bale values and capacities stay at defaults (not exposed in [General]).
    pub fn from_general_rules(general: &GeneralRules) -> Self {
        // HarvesterLoadRate: frames per step. 9 steps per bale.
        let load_rate = general.harvester_load_rate.max(1);
        let harvest_interval = (load_rate * 9).min(255) as u8;
        // HarvesterDumpRate: minutes per bale. Multiply by 900 (60s * 15fps) for frames.
        let dump_frames = (general.harvester_dump_rate * 900.0).clamp(0.0, 255.0) as u8;
        let unload_interval = dump_frames.max(1);

        Self {
            local_continuation_radius: general.tiberium_short_scan.max(1) as u16,
            long_scan_radius: general.tiberium_long_scan.max(1) as u16,
            too_far_threshold_standard: general.harvester_too_far_distance.max(1) as u16,
            too_far_threshold_chrono: general.chrono_harv_too_far_distance.max(1) as u16,
            harvest_tick_interval: harvest_interval,
            unload_tick_interval: unload_interval,
            ..Self::default()
        }
    }
}

/// ECS component: miner state machine and cargo hold.
///
/// Attached to harvester entities alongside Position, Owner, TypeRef, etc.
/// The miner_system tick reads and mutates this each frame.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Miner {
    pub kind: MinerKind,
    pub state: MinerState,
    /// StableEntityId of the "home" refinery (may change after unloading).
    pub home_refinery: Option<u64>,
    /// StableEntityId of the refinery this miner has reserved a dock slot at.
    pub reserved_refinery: Option<u64>,
    /// The ore/gem cell we are currently targeting.
    pub target_ore_cell: Option<(u16, u16)>,
    /// Discrete cargo bales currently carried.
    pub cargo: Vec<CargoBale>,
    /// Maximum number of bales this miner can carry.
    pub capacity_bales: u16,
    /// Countdown timer for the next harvest action.
    pub harvest_timer: u8,
    /// Countdown timer for the next unload action.
    pub unload_timer: u8,
    /// Whether the player issued a manual return order.
    pub forced_return: bool,
    /// Whether this miner is queued (but not yet occupying) a dock.
    pub dock_queued: bool,
    /// Cooldown ticks before re-scanning for ore in WaitNoOre state.
    pub rescan_cooldown: u8,
    /// Last cell we successfully harvested from (for local continuation search).
    pub last_harvest_cell: Option<(u16, u16)>,
    /// Current phase of the refinery docking sequence.
    /// Only meaningful when `state == MinerState::Dock`.
    pub dock_phase: RefineryDockPhase,
    /// Accumulated base credit value of bales deposited during current unload.
    /// Used to compute purifier bonus on the total at end of unload, matching
    /// gamemd's single-pass DepositOreFromStorage (avoids per-bale truncation).
    pub unload_base_total: u32,
}

impl Miner {
    /// Create a new miner in SearchOre state with the given kind and config.
    pub fn new(kind: MinerKind, config: &MinerConfig) -> Self {
        let capacity_bales = match kind {
            MinerKind::War => config.war_miner_capacity,
            MinerKind::Chrono => config.chrono_miner_capacity,
            // Slave Miners don't carry cargo — their slave infantry harvest instead.
            MinerKind::Slave => 0,
        };
        Self {
            kind,
            state: MinerState::SearchOre,
            home_refinery: None,
            reserved_refinery: None,
            target_ore_cell: None,
            cargo: Vec::with_capacity(capacity_bales as usize),
            capacity_bales,
            harvest_timer: 0,
            unload_timer: 0,
            forced_return: false,
            dock_queued: false,
            rescan_cooldown: 0,
            last_harvest_cell: None,
            dock_phase: RefineryDockPhase::default(),
            unload_base_total: 0,
        }
    }

    /// True when cargo is at capacity.
    pub fn is_full(&self) -> bool {
        self.cargo.len() as u16 >= self.capacity_bales
    }

    /// How many of the 5 UI pips should be filled.
    /// Each pip = 20% of capacity, rounded down.
    pub fn cargo_pips(&self) -> u8 {
        if self.capacity_bales == 0 {
            return 0;
        }
        let ratio = (self.cargo.len() as u32 * 5) / self.capacity_bales as u32;
        (ratio as u8).min(5)
    }

    /// Total credit value of all bales currently in the hold.
    pub fn cargo_value(&self) -> u32 {
        self.cargo.iter().map(|b| b.value as u32).sum()
    }
}

/// Determine the miner chassis from parsed rules data.
///
/// Detection priority:
/// 1. `Enslaves=` present → Slave Miner (SMIN). Does NOT have `Harvester=yes`.
/// 2. `Harvester=yes` + `Teleporter=yes` → Chrono Miner (CMIN).
/// 3. `Harvester=yes` → War Miner (HARV).
pub fn miner_kind_for_object(object: &ObjectType) -> Option<MinerKind> {
    // Slave Miner detected via Enslaves= (SMIN does NOT have Harvester=yes).
    if object.enslaves.is_some() {
        return Some(MinerKind::Slave);
    }

    if !object.harvester {
        return None;
    }

    if object.teleporter {
        Some(MinerKind::Chrono)
    } else {
        Some(MinerKind::War)
    }
}

/// Reduce ore/gem density on a cell by `amount` density levels.
///
/// Returns the number of density levels actually removed. If the cell is
/// fully depleted, removes the resource node entirely.
///
/// Mirrors `CellClass::Reduce_Tiberium` (0x00480a80) in gamemd.exe.
/// Called by the combat system after warhead detonation.
pub(crate) fn reduce_tiberium(
    resource_nodes: &mut BTreeMap<(u16, u16), ResourceNode>,
    cell: (u16, u16),
    amount: u16,
) -> u16 {
    if amount == 0 {
        return 0;
    }
    // Read type and density before deciding partial vs full removal.
    let (base, density_levels) = match resource_nodes.get(&cell) {
        Some(node) => {
            let base: u16 = match node.resource_type {
                ResourceType::Ore => 120,
                ResourceType::Gem => 180,
            };
            (base, node.remaining / base)
        }
        None => return 0,
    };
    if density_levels == 0 {
        return 0;
    }

    if amount < density_levels {
        // Partial reduction: reduce remaining by amount × base.
        resource_nodes.get_mut(&cell).unwrap().remaining -= amount * base;
        amount
    } else {
        // Full removal: destroy the resource node entirely.
        resource_nodes.remove(&cell);
        density_levels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_war_miner_ore_payout() {
        let cfg = MinerConfig::default();
        // War Miner full ore: capacity * ore_bale_value = 40 * 25 = 1000
        assert_eq!(
            cfg.war_miner_capacity as u32 * cfg.ore_bale_value as u32,
            1000
        );
    }

    #[test]
    fn default_config_war_miner_gem_payout() {
        let cfg = MinerConfig::default();
        // War Miner full gems: 40 * 50 = 2000
        assert_eq!(
            cfg.war_miner_capacity as u32 * cfg.gem_bale_value as u32,
            2000
        );
    }

    #[test]
    fn default_config_chrono_miner_ore_payout() {
        let cfg = MinerConfig::default();
        // Chrono Miner full ore: 20 * 25 = 500
        assert_eq!(
            cfg.chrono_miner_capacity as u32 * cfg.ore_bale_value as u32,
            500
        );
    }

    #[test]
    fn default_config_chrono_miner_gem_payout() {
        let cfg = MinerConfig::default();
        // Chrono Miner full gems: 20 * 50 = 1000
        assert_eq!(
            cfg.chrono_miner_capacity as u32 * cfg.gem_bale_value as u32,
            1000
        );
    }

    #[test]
    fn cargo_pips_shows_five_steps() {
        let cfg = MinerConfig::default();
        let mut miner = Miner::new(MinerKind::War, &cfg);
        assert_eq!(miner.cargo_pips(), 0);
        // Fill 20% (8 of 40 bales)
        for _ in 0..8 {
            miner.cargo.push(CargoBale {
                resource_type: ResourceType::Ore,
                value: 25,
            });
        }
        assert_eq!(miner.cargo_pips(), 1);
        // Fill 40%
        for _ in 0..8 {
            miner.cargo.push(CargoBale {
                resource_type: ResourceType::Ore,
                value: 25,
            });
        }
        assert_eq!(miner.cargo_pips(), 2);
        // Fill 100%
        while !miner.is_full() {
            miner.cargo.push(CargoBale {
                resource_type: ResourceType::Ore,
                value: 25,
            });
        }
        assert_eq!(miner.cargo_pips(), 5);
    }

    #[test]
    fn miner_kind_detection_is_data_driven() {
        let mut war = ObjectType::from_ini_section(
            "MODHARV",
            &crate::rules::ini_parser::IniFile::from_str("[MODHARV]\nHarvester=yes\n")
                .section("MODHARV")
                .expect("section"),
            crate::rules::object_type::ObjectCategory::Vehicle,
        );
        assert_eq!(miner_kind_for_object(&war), Some(MinerKind::War));

        war.teleporter = true;
        assert_eq!(miner_kind_for_object(&war), Some(MinerKind::Chrono));

        let non_harvester = ObjectType::from_ini_section(
            "E1",
            &crate::rules::ini_parser::IniFile::from_str("[E1]\n")
                .section("E1")
                .expect("section"),
            crate::rules::object_type::ObjectCategory::Infantry,
        );
        assert_eq!(miner_kind_for_object(&non_harvester), None);
    }

    #[test]
    fn from_general_rules_overrides_scan_radii() {
        let mut general = GeneralRules::default();
        general.tiberium_short_scan = 10;
        general.tiberium_long_scan = 60;
        general.harvester_too_far_distance = 8;
        general.chrono_harv_too_far_distance = 40;

        let cfg = MinerConfig::from_general_rules(&general);
        assert_eq!(cfg.local_continuation_radius, 10);
        assert_eq!(cfg.long_scan_radius, 60);
        assert_eq!(cfg.too_far_threshold_standard, 8);
        assert_eq!(cfg.too_far_threshold_chrono, 40);
        // Bale values stay at defaults.
        assert_eq!(cfg.ore_bale_value, 25);
        assert_eq!(cfg.gem_bale_value, 50);
    }

    #[test]
    fn reduce_tiberium_partial_ore() {
        let mut nodes = BTreeMap::new();
        // 6 density levels of ore: remaining = 6 * 120 = 720.
        nodes.insert((5, 5), ResourceNode { resource_type: ResourceType::Ore, remaining: 720 });
        let removed = reduce_tiberium(&mut nodes, (5, 5), 2);
        assert_eq!(removed, 2);
        assert_eq!(nodes.get(&(5, 5)).unwrap().remaining, 720 - 2 * 120);
    }

    #[test]
    fn reduce_tiberium_full_removal_ore() {
        let mut nodes = BTreeMap::new();
        // 3 density levels: remaining = 360.
        nodes.insert((5, 5), ResourceNode { resource_type: ResourceType::Ore, remaining: 360 });
        let removed = reduce_tiberium(&mut nodes, (5, 5), 12);
        assert_eq!(removed, 3, "should return old density_levels");
        assert!(nodes.get(&(5, 5)).is_none(), "node should be removed");
    }

    #[test]
    fn reduce_tiberium_exact_density_is_full_removal() {
        let mut nodes = BTreeMap::new();
        // 5 density levels: remaining = 600.
        nodes.insert((5, 5), ResourceNode { resource_type: ResourceType::Ore, remaining: 600 });
        // amount(5) >= density_levels(5) → full removal (amount < density is false).
        let removed = reduce_tiberium(&mut nodes, (5, 5), 5);
        assert_eq!(removed, 5);
        assert!(nodes.get(&(5, 5)).is_none(), "exact match = full removal");
    }

    #[test]
    fn reduce_tiberium_empty_cell() {
        let mut nodes: BTreeMap<(u16, u16), ResourceNode> = BTreeMap::new();
        let removed = reduce_tiberium(&mut nodes, (5, 5), 10);
        assert_eq!(removed, 0);
    }

    #[test]
    fn reduce_tiberium_zero_amount() {
        let mut nodes = BTreeMap::new();
        nodes.insert((5, 5), ResourceNode { resource_type: ResourceType::Ore, remaining: 720 });
        let removed = reduce_tiberium(&mut nodes, (5, 5), 0);
        assert_eq!(removed, 0);
        assert_eq!(nodes.get(&(5, 5)).unwrap().remaining, 720, "unchanged");
    }

    #[test]
    fn reduce_tiberium_gem_base_rate() {
        let mut nodes = BTreeMap::new();
        // 4 density levels of gems: remaining = 4 * 180 = 720.
        nodes.insert((5, 5), ResourceNode { resource_type: ResourceType::Gem, remaining: 720 });
        let removed = reduce_tiberium(&mut nodes, (5, 5), 2);
        assert_eq!(removed, 2);
        assert_eq!(nodes.get(&(5, 5)).unwrap().remaining, 720 - 2 * 180);
    }
}
