//! Deterministic simulation command model.
//!
//! All gameplay inputs are translated into explicit commands that can be
//! scheduled by tick, logged, replayed, and sent over lockstep transport.

use serde::{Deserialize, Serialize};

use crate::sim::intern::InternedId;
use crate::sim::production::ProductionCategory;

/// Queueing behavior for build/production commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueueMode {
    /// Replace existing queued path/intent.
    Replace,
    /// Append to existing queue/waypoint chain.
    Append,
}

/// One gameplay command payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    /// Selection intent (kept for replay/debug parity; not authoritative sim state yet).
    Select {
        entity_ids: Vec<u64>,
        additive: bool,
    },
    /// Move one entity to a target cell.
    /// Speed is resolved at dispatch time from rules.ini (ObjectType.speed)
    /// multiplied by the entity's locomotor speed_multiplier.
    Move {
        entity_id: u64,
        target_rx: u16,
        target_ry: u16,
        queue: bool,
        /// When multiple units are ordered together, they share a group_id.
        /// The movement system syncs their speed to the slowest member.
        group_id: Option<u32>,
    },
    /// Stop movement/combat intent on one entity.
    Stop { entity_id: u64 },
    /// Attack an explicit target.
    Attack { attacker_id: u64, target_id: u64 },
    /// Force-attack a target (ignores friendship — Ctrl+click).
    ForceAttack { attacker_id: u64, target_id: u64 },
    /// Attack-move toward a cell (logic can retarget along path).
    AttackMove {
        entity_id: u64,
        target_rx: u16,
        target_ry: u16,
        queue: bool,
    },
    /// Guard a target entity or area (target optional for area guard).
    Guard {
        entity_id: u64,
        target_id: Option<u64>,
    },
    /// Deploy a mobile construction vehicle into its construction yard.
    DeployMcv { entity_id: u64 },
    /// Undeploy a structure back into its mobile unit (e.g. ConYard → MCV).
    /// Reads UndeploysInto from rules.ini to determine the spawned unit type.
    UndeployBuilding { entity_id: u64 },
    /// Set production rally point for owner.
    SetRally { owner: InternedId, rx: u16, ry: u16 },
    /// Enqueue a production item.
    QueueProduction {
        owner: InternedId,
        type_id: InternedId,
        mode: QueueMode,
    },
    /// Pause/resume the active production item for one owner/category queue.
    TogglePauseProduction {
        owner: InternedId,
        category: ProductionCategory,
    },
    /// Cycle the active producer facility for one owner/category.
    CycleProducerFocus {
        owner: InternedId,
        category: ProductionCategory,
    },
    /// Place one completed building that is waiting for placement.
    PlaceReadyBuilding {
        owner: InternedId,
        type_id: InternedId,
        rx: u16,
        ry: u16,
    },
    /// Cancel the last queued production item for owner.
    CancelLastProduction { owner: InternedId },
    /// Cancel one queued item of a specific type (right-click cameo).
    CancelProductionByType {
        owner: InternedId,
        type_id: InternedId,
    },
    /// Sell a building, refunding a percentage of its cost and despawning it.
    SellBuilding { entity_id: u64 },
    /// Toggle repair mode on a building (spend credits to heal over time).
    ToggleRepair { entity_id: u64 },
    /// Force a miner to return to its refinery (right-click on own refinery or 'D' key).
    /// Chrono Miners teleport; War Miners drive back.
    MinerReturn { entity_id: u64 },
    /// Send a unit to a repair depot for repairs.
    /// The unit pathfinds to the depot, docks, and auto-repairs until full HP or out of credits.
    RepairAtDepot { entity_id: u64, depot_id: u64 },
    /// Order an infantry/vehicle to enter a friendly transport or garrisonable building.
    /// The passenger pathfinds to the transport's cell and boards on arrival.
    EnterTransport {
        passenger_id: u64,
        transport_id: u64,
    },
    /// Order a transport to unload all passengers to adjacent cells (one per tick).
    UnloadPassengers { transport_id: u64 },
    /// Direct a harvester to go harvest a specific ore cell.
    /// The miner will path to the cell, then enter Harvest state on arrival.
    HarvestCell {
        entity_id: u64,
        target_rx: u16,
        target_ry: u16,
    },
    /// Order an engineer to capture an enemy building.
    /// Engineer walks to the building, instantly transfers ownership on arrival,
    /// and is consumed.
    CaptureBuilding {
        engineer_id: u64,
        target_building_id: u64,
    },
}

/// Command with deterministic execution metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandEnvelope {
    pub owner: InternedId,
    pub execute_tick: u64,
    pub payload: Command,
}

impl CommandEnvelope {
    pub fn new(owner: InternedId, execute_tick: u64, payload: Command) -> Self {
        Self {
            owner,
            execute_tick,
            payload,
        }
    }
}
