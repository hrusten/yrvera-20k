//! Production system types, constants, and state containers.
//!
//! Shared types used across production sub-modules: queue items, build options,
//! placement previews, and the central `ProductionState` struct.

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::rules::object_type::ObjectCategory;
use crate::sim::intern::InternedId;
use crate::sim::miner::ResourceNode;
use crate::sim::miner::miner_dock::DockReservations;
use crate::sim::ore_growth::{OreGrowthConfig, OreGrowthState};

/// Initial credits for the local player.
pub const STARTING_CREDITS: i32 = 5000;
/// Fixed-point precision for dynamic production-rate application.
pub(super) const PRODUCTION_RATE_SCALE: u64 = 1_000_000;

/// One queued build item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildQueueItem {
    pub owner: InternedId,
    pub type_id: InternedId,
    pub queue_category: ProductionCategory,
    pub state: BuildQueueState,
    /// Base build time in RA2 production frames before live power/factory/wall scaling.
    pub total_base_frames: u32,
    /// Remaining base build time in RA2 production frames before live scaling.
    pub remaining_base_frames: u32,
    pub progress_carry: u64,
    pub enqueue_order: u64,
}

/// One queued item formatted for UI rendering.
#[derive(Debug, Clone)]
pub struct QueueItemView {
    pub type_id: InternedId,
    pub display_name: String,
    pub queue_category: ProductionCategory,
    pub state: BuildQueueState,
    pub remaining_ms: u32,
    pub total_ms: u32,
}

/// One completed building waiting for placement.
#[derive(Debug, Clone)]
pub struct ReadyBuildingView {
    pub type_id: InternedId,
    pub display_name: String,
    pub queue_category: ProductionCategory,
}

/// Active producer/facility focus for one queue category.
#[derive(Debug, Clone)]
pub struct ProducerFocusView {
    pub stable_id: u64,
    pub display_name: String,
    pub category: ProductionCategory,
    pub rx: u16,
    pub ry: u16,
}

/// Placement preview/evaluation for a ready building.
#[derive(Debug, Clone)]
pub struct BuildingPlacementPreview {
    pub type_id: InternedId,
    pub rx: u16,
    pub ry: u16,
    pub width: u16,
    pub height: u16,
    pub valid: bool,
    pub reason: Option<BuildingPlacementError>,
    /// Per-cell validity (row-major, width*height). True = cell is placeable.
    pub cell_valid: Vec<bool>,
}

/// Why an item cannot currently be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildDisabledReason {
    UnbuildableTechLevel,
    WrongOwner,
    WrongHouse,
    ForbiddenHouse,
    RequiresStolenTech,
    MissingPrerequisite(String),
    NoFactory,
    AtBuildLimit,
    InsufficientCredits,
    PlacementModeUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildingPlacementError {
    NotReady,
    NotBuilding,
    BlockedTerrain,
    OverlapsStructure,
    OutOfBuildArea,
}

impl BuildingPlacementError {
    pub fn label(&self) -> &'static str {
        match self {
            Self::NotReady => "Not ready for placement",
            Self::NotBuilding => "Not a building",
            Self::BlockedTerrain => "Blocked terrain",
            Self::OverlapsStructure => "Overlaps structure",
            Self::OutOfBuildArea => "Outside build radius",
        }
    }
}

pub fn disabled_reason_text(reason: &BuildDisabledReason) -> String {
    match reason {
        BuildDisabledReason::UnbuildableTechLevel => "Unbuildable (TechLevel)".to_string(),
        BuildDisabledReason::WrongOwner => "Wrong owner".to_string(),
        BuildDisabledReason::WrongHouse => "Wrong house".to_string(),
        BuildDisabledReason::ForbiddenHouse => "Forbidden for this house".to_string(),
        BuildDisabledReason::RequiresStolenTech => {
            "Requires stolen tech (spy infiltration)".to_string()
        }
        BuildDisabledReason::MissingPrerequisite(p) => format!("Missing prerequisite: {}", p),
        BuildDisabledReason::NoFactory => "No production building".to_string(),
        BuildDisabledReason::AtBuildLimit => "Build limit reached".to_string(),
        BuildDisabledReason::InsufficientCredits => "Insufficient credits".to_string(),
        BuildDisabledReason::PlacementModeUnavailable => {
            "Building placement not implemented yet".to_string()
        }
    }
}

/// Sidebar queue/category for build options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ProductionCategory {
    Building,
    Defense,
    Infantry,
    Vehicle,
    Aircraft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BuildQueueState {
    Queued,
    Building,
    Paused,
    Done,
}

impl BuildQueueState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Building => "Building",
            Self::Paused => "Paused",
            Self::Done => "Done",
        }
    }
}

impl ProductionCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::Building => "Building",
            Self::Defense => "Defense",
            Self::Infantry => "Infantry",
            Self::Vehicle => "Vehicle",
            Self::Aircraft => "Aircraft",
        }
    }
}

/// One build option exposed to UI.
#[derive(Debug, Clone)]
pub struct BuildOption {
    pub type_id: InternedId,
    pub display_name: String,
    pub cost: i32,
    pub object_category: ObjectCategory,
    pub queue_category: ProductionCategory,
    pub enabled: bool,
    pub reason: Option<BuildDisabledReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BuildMode {
    Strict,
    PrototypeRelaxed,
}

/// Player production state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductionState {
    pub queues_by_owner:
        BTreeMap<InternedId, BTreeMap<ProductionCategory, VecDeque<BuildQueueItem>>>,
    pub ready_by_owner: BTreeMap<InternedId, VecDeque<InternedId>>,
    pub active_producer_by_owner: BTreeMap<InternedId, BTreeMap<ProductionCategory, u64>>,
    pub next_enqueue_order: u64,
    /// Deterministic map resource stock by cell (ore/gem type + remaining amount).
    pub resource_nodes: BTreeMap<(u16, u16), ResourceNode>,
    /// Refinery dock reservation state — one dock per refinery, FIFO queue.
    pub dock_reservations: DockReservations,
    /// Ore growth/spread configuration resolved from merged INI sources.
    pub ore_growth_config: OreGrowthConfig,
    /// Incremental scan state for ore growth/spread system.
    pub ore_growth_state: OreGrowthState,
    /// Slave Miner bindings: master entity stable_id → vec of slave entity stable_ids.
    /// Used to track which SLAV infantry belong to which deployed SMIN/YAREFN.
    pub slave_bindings: BTreeMap<u64, Vec<u64>>,
    /// Repair depot dock reservation state — one dock per depot, FIFO queue.
    pub depot_dock_reservations: DockReservations,
    /// Airfield dock reservations — multi-slot (NumberOfDocks per airfield).
    pub airfield_docks: crate::sim::docking::aircraft_dock::AirfieldDocks,
}

impl Default for ProductionState {
    fn default() -> Self {
        Self {
            queues_by_owner: BTreeMap::new(),
            ready_by_owner: BTreeMap::new(),
            active_producer_by_owner: BTreeMap::new(),
            next_enqueue_order: 1,
            resource_nodes: BTreeMap::new(),
            dock_reservations: DockReservations::default(),
            ore_growth_config: OreGrowthConfig::disabled(),
            ore_growth_state: OreGrowthState::new(0, 0),
            slave_bindings: BTreeMap::new(),
            depot_dock_reservations: DockReservations::default(),
            airfield_docks: crate::sim::docking::aircraft_dock::AirfieldDocks::default(),
        }
    }
}
