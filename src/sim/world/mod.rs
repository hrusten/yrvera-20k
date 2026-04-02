//! Game simulation - owns the EntityStore and deterministic tick stepping.
//!
//! The Simulation is the authoritative game state. It spawns entities from
//! map data, executes command envelopes on fixed ticks, advances gameplay
//! systems, and exposes deterministic state hashing for replay/desync checks.
//!
//! Implementation is split across sibling files for size:
//! - `world_commands.rs` — command dispatch and selection/ownership helpers
//! - `world_hash.rs` — deterministic state hashing
//! - `world_spawn.rs` — entity spawning from map data and production
//! - `world_orders.rs` — order-intent tick systems (attack-move, guard, area-guard)

mod world_commands;
mod world_hash;
mod world_orders;
mod world_spawn;

use std::collections::BTreeMap;

use crate::map::actions::ActionMap;
use crate::map::entities::EntityCategory;
use crate::map::events::EventMap;
use crate::map::houses::HouseAllianceMap;
use crate::map::resolved_terrain::ResolvedTerrainGrid;
use crate::map::trigger_graph::TriggerGraph;
use crate::map::triggers::TriggerMap;
use crate::rules::locomotor_type::SpeedType;
use crate::rules::ruleset::RuleSet;
use crate::sim::ai::{self, AiPlayerState};
use crate::sim::bridge_state::{BridgeDamageEvent, BridgeRuntimeState, BridgeStateChange};
use crate::sim::occupancy::OccupancyGrid;
use crate::sim::combat;
use crate::sim::combat::combat_weapon::WeaponSlot;
use crate::sim::command::{Command, CommandEnvelope};
use crate::sim::components::WorldEffect;
use crate::sim::docking::aircraft_dock;
use crate::sim::docking::building_dock;
use crate::sim::entity_store::EntityStore;
use crate::sim::game_options::GameOptions;
use crate::sim::house_state::HouseState;
use crate::sim::intern::InternedId;
use crate::sim::movement;
use crate::sim::movement::air_movement;
use crate::sim::movement::droppod_movement;
use crate::sim::movement::locomotor::{GroundMovePhase, MovementLayer};
use crate::sim::movement::rocket_movement;
use crate::sim::movement::teleport_movement;
use crate::sim::movement::tunnel_movement;
use crate::sim::movement::turret;
use crate::sim::ore_growth;
use crate::sim::passenger;
use crate::sim::pathfinding::PathGrid;
use crate::sim::pathfinding::terrain_cost::TerrainCostGrid;
use crate::sim::pathfinding::terrain_speed;
use crate::sim::pathfinding::zone_map::ZoneGrid;
use crate::sim::power_system::{self, PowerState};
use crate::sim::production::{self, ProductionState};
use crate::sim::radar::{RadarEventQueue, RadarEventType};
use crate::sim::replay::ReplayLog;
use crate::sim::rng::SimRng;
use crate::sim::trigger_runtime::{TriggerEffect, TriggerRuntime};
use crate::sim::vision::{self, FogState};
use crate::util::fixed_math::SimFixed;

/// Default deterministic RNG seed for ad-hoc simulation instances.
const DEFAULT_SIM_SEED: u64 = 0x5EED_CAFE_D15E_A5E5;

/// Result of one deterministic simulation tick.
#[derive(Debug, Clone, Copy)]
pub struct TickResult {
    pub tick: u64,
    pub executed_commands: usize,
    pub state_hash: u64,
    pub spawned_entities: bool,
    /// A structure was destroyed (combat, sell, crush) — PathGrid needs rebuild
    /// to unblock the footprint.
    pub destroyed_structure: bool,
    /// An entity's owner changed (garrison transfer, engineer capture) — sprite
    /// atlas needs rebuild for the new house color.
    pub ownership_changed: bool,
    pub movement: movement::MovementTickStats,
}

/// A sound event produced during simulation (combat, death, production).
/// Pure data — no audio library dependency. Drained by the app layer each frame.
#[derive(Debug, Clone)]
pub enum SimSoundEvent {
    /// A weapon fired — play its Report= sound.
    WeaponFired {
        report_sound_id: InternedId,
        rx: u16,
        ry: u16,
    },
    /// An entity was destroyed — play its DieSound=.
    EntityDied {
        die_sound_id: InternedId,
        rx: u16,
        ry: u16,
    },
    /// A miner docked at a refinery — play the building's deploy sound.
    /// The app layer should select the healthy or damaged sound variant
    /// based on the refinery's health ratio vs ConditionYellow.
    DockDeploy { building_id: u64 },
    /// A building finished construction — play EVA "Construction complete".
    BuildingComplete { owner: InternedId },
    /// A unit finished training — play EVA "Unit ready".
    UnitComplete { owner: InternedId },
    /// A chrono miner teleported — play ChronoInSound/ChronoOutSound.
    ChronoTeleport { rx: u16, ry: u16 },
}

/// A fire event produced during combat — carries data for render-side
/// muzzle flash positioning and future projectile origin computation.
///
/// The sim emits this whenever a weapon fires. The render/app layer
/// resolves the screen-space muzzle position from the attacker's
/// ArtEntry FLH + facing.
#[derive(Debug, Clone)]
pub struct SimFireEvent {
    /// Stable ID of the entity that fired.
    pub attacker_id: u64,
    /// Which weapon slot was used (Primary or Secondary).
    pub weapon_slot: WeaponSlot,
    /// Stable ID of the target entity (for future projectile trajectory).
    pub target_id: u64,
    /// For garrison fire: which muzzle port index fired (for fire port positioning).
    /// None = normal weapon FLH, Some(idx) = garrison fire port index.
    pub garrison_muzzle_index: Option<u8>,
    /// For garrison fire: the weapon's OccupantAnim interned ID (e.g., "UCFLASH").
    /// Pushed through the event so the render layer doesn't need to re-derive the weapon.
    pub occupant_anim: Option<InternedId>,
}

/// The game simulation - owns all authoritative game state.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Simulation {
    /// String interner for owner/type_ref — zero-cost ID clones instead of heap Strings.
    pub interner: crate::sim::intern::StringInterner,
    /// Plain-struct entity storage.
    pub entities: EntityStore,
    /// Credits, build queue state, and rally points.
    pub production: ProductionState,
    /// Current simulation tick (starts at 0, increments after each advance_tick).
    pub tick: u64,
    /// Single explicit deterministic PRNG stream for simulation logic.
    pub rng: SimRng,
    /// Deterministic fog/shroud visibility state.
    pub fog: FogState,
    /// Static alliance graph derived from map house data.
    pub house_alliances: HouseAllianceMap,
    pub(crate) next_stable_entity_id: u64,
    /// Sound events produced during the current tick — drained by the app layer.
    #[serde(skip)]
    pub sound_events: Vec<SimSoundEvent>,
    /// Fire events produced during combat — drained by the app layer for
    /// muzzle flash rendering and future projectile origin computation.
    #[serde(skip)]
    pub fire_events: Vec<SimFireEvent>,
    /// Per-AI-owner state for computer-controlled players.
    pub ai_players: Vec<AiPlayerState>,
    /// Per-player state keyed by uppercase owner name. Deterministic iteration
    /// via BTreeMap. Equivalent to the original engine's HouseClass array.
    pub houses: BTreeMap<InternedId, HouseState>,
    /// Per-SpeedType terrain cost grids for cost-aware A* pathfinding.
    /// Built once at map load — units look up their SpeedType to pick the right grid.
    #[serde(skip)]
    pub terrain_costs: BTreeMap<SpeedType, TerrainCostGrid>,
    /// Flat per-cell height grid for height-based LOS (RevealByHeight).
    /// Built from PathGrid; indexed by `ry * width + rx`.
    #[serde(skip)]
    pub(crate) vision_height_grid: Option<Vec<u8>>,
    /// Zone-based connectivity map for instant unreachability detection.
    /// Built from terrain data; rebuilt when buildings or bridges change.
    #[serde(skip)]
    pub zone_grid: Option<ZoneGrid>,
    /// Previous PathGrid snapshot for incremental zone diffing.
    #[serde(skip)]
    prev_path_grid: Option<PathGrid>,
    #[serde(skip)]
    pub resolved_terrain: Option<ResolvedTerrainGrid>,
    pub bridge_state: Option<BridgeRuntimeState>,
    /// Persistent cell occupancy — tracks what entities occupy each cell.
    /// Maintained incrementally via add/remove at spawn, move, and death sites.
    /// Rebuilt from entities on deserialization.
    #[serde(skip)]
    pub occupancy: OccupancyGrid,
    /// SHP interned IDs for bridge destruction explosions (from rules.ini BridgeExplosions=).
    #[serde(skip)]
    pub bridge_explosions: Vec<InternedId>,
    /// Radar event queue for minimap pings and Spacebar cycling.
    #[serde(skip)]
    pub radar_events: RadarEventQueue,
    /// Per-player power state (output, drain, low-power flag, spy blackout timer).
    /// Updated each tick by `power_system::tick_power_states()`.
    pub power_states: BTreeMap<InternedId, PowerState>,
    /// Per-cell terrain speed modifier config (slope climb/descend, crowd density).
    /// Built from [General] rules at map load.
    #[serde(skip)]
    pub terrain_speed_config: terrain_speed::TerrainSpeedConfig,
    /// Distance in leptons below which a blocked unit stops instead of repathing.
    /// From CloseEnough= in [General]. Default 576 (~2.25 cells).
    pub close_enough: SimFixed,
    /// Ticks between pathfinding retry attempts (PathDelay= in [General]).
    pub path_delay_ticks: u16,
    /// Ticks to wait when blocked by a friendly before aggressive repath (BlockagePathDelay=).
    pub blockage_path_delay_ticks: u16,
    /// Temporary world-position SHP animations (warp effects, explosions, etc.).
    /// Ticked each frame, auto-removed when finished.
    #[serde(skip)]
    pub world_effects: Vec<crate::sim::components::WorldEffect>,
    /// Frame counts for world-effect SHPs, keyed by interned ID (e.g., "WARPOUT" → 20).
    /// Populated from the sprite atlas at init time so sim code can spawn effects
    /// with the correct frame count without hardcoding it.
    #[serde(skip)]
    pub effect_frame_counts: BTreeMap<InternedId, u16>,
    /// Per-match game settings (crates, short game, superweapons, etc.).
    /// Set once at game start from lobby / [MultiplayerDialogSettings], read-only during gameplay.
    pub game_options: GameOptions,
    /// When true, newly spawned entities get a `DebugEventLog` allocated.
    /// Toggled by the debug inspector hotkey (X). Debug-only — not included in state hashing.
    #[serde(skip)]
    pub debug_event_logging: bool,
    /// In-memory replay log for this match — records commands + state hashes per tick.
    /// Initialized lazily on the first tick. Observer artifact — not included in state hashing.
    #[serde(skip)]
    pub replay_log: Option<ReplayLog>,
    /// Input delay in ticks for lockstep-style command scheduling.
    /// Commands are scheduled `now_tick + input_delay_ticks` into the future.
    /// Set once from config at game start, read-only during gameplay.
    pub input_delay_ticks: u64,
    /// Pending gameplay commands waiting for their scheduled execution tick.
    /// Pushed by the app layer (user input, sidebar, AI), drained each tick
    /// in `advance_tick()` when `cmd.execute_tick <= current_tick + 1`.
    pub pending_commands: Vec<CommandEnvelope>,
    /// Map trigger runtime state — tracks global/local variables, disabled triggers,
    /// fired one-shot triggers, and elapsed scenario ticks. Initialized from map data.
    pub trigger_runtime: TriggerRuntime,
}

impl Default for Simulation {
    fn default() -> Self {
        Self::new()
    }
}

impl Simulation {
    /// Create a new empty simulation with the default deterministic seed.
    pub fn new() -> Self {
        Self::with_seed(DEFAULT_SIM_SEED)
    }

    /// Create a new empty simulation with an explicit deterministic seed.
    pub fn with_seed(seed: u64) -> Self {
        Self {
            interner: crate::sim::intern::StringInterner::new(),
            entities: EntityStore::new(),
            production: ProductionState::default(),
            tick: 0,
            rng: SimRng::new(seed),
            fog: FogState::default(),
            house_alliances: HouseAllianceMap::default(),
            next_stable_entity_id: 1,
            sound_events: Vec::new(),
            fire_events: Vec::new(),
            ai_players: Vec::new(),
            houses: BTreeMap::new(),
            terrain_costs: BTreeMap::new(),
            vision_height_grid: None,
            zone_grid: None,
            prev_path_grid: None,
            resolved_terrain: None,
            bridge_state: None,
            occupancy: OccupancyGrid::new(),
            bridge_explosions: Vec::new(),
            radar_events: RadarEventQueue::default(),
            power_states: BTreeMap::new(),
            terrain_speed_config: terrain_speed::TerrainSpeedConfig::default(),
            close_enough: SimFixed::from_num(576), // 2.25 cells × 256 lep/cell
            path_delay_ticks: 9,
            blockage_path_delay_ticks: 60,
            world_effects: Vec::new(),
            effect_frame_counts: BTreeMap::new(),
            game_options: GameOptions::default(),
            debug_event_logging: false,
            replay_log: None,
            input_delay_ticks: 2,
            pending_commands: Vec::new(),
            trigger_runtime: TriggerRuntime::default(),
        }
    }

    /// Resolve an InternedId back to its display string.
    #[inline]
    pub fn resolve(&self, id: crate::sim::intern::InternedId) -> &str {
        self.interner.resolve(id)
    }

    /// Intern a string, returning its InternedId.
    #[inline]
    pub fn intern(&mut self, s: &str) -> crate::sim::intern::InternedId {
        self.interner.intern(s)
    }

    /// Queue a command for future execution at its scheduled tick.
    pub fn queue_command(&mut self, cmd: CommandEnvelope) {
        self.pending_commands.push(cmd);
    }

    /// Drain commands that are due for the next tick from `pending_commands`.
    /// Returns owned commands; remaining commands stay queued.
    pub fn take_due_commands(&mut self) -> Vec<CommandEnvelope> {
        let execute_tick = self.tick.saturating_add(1);
        let mut due = Vec::new();
        let mut kept = Vec::new();
        for cmd in std::mem::take(&mut self.pending_commands) {
            if cmd.execute_tick <= execute_tick {
                due.push(cmd);
            } else {
                kept.push(cmd);
            }
        }
        self.pending_commands = kept;
        due
    }

    /// Advance map triggers by one tick. Uses `std::mem::take` to avoid
    /// self-borrow conflict (advance reads entity/interner state via `&Simulation`).
    pub fn advance_triggers(
        &mut self,
        graph: &TriggerGraph,
        triggers: &TriggerMap,
        events: &EventMap,
        actions: &ActionMap,
    ) -> Vec<TriggerEffect> {
        let mut rt = std::mem::take(&mut self.trigger_runtime);
        let effects = rt.advance(1, graph, triggers, events, actions, Some(self));
        self.trigger_runtime = rt;
        effects
    }

    /// Returns true if the given house name is human-controlled.
    /// Equivalent to the original engine's IsHumanPlayer (0x50b6f0).
    pub fn is_human_player(&self, owner: &str) -> bool {
        self.interner
            .get(owner)
            .and_then(|id| self.houses.get(&id))
            .is_some_and(|h| h.is_human)
    }

    pub(crate) fn allocate_stable_id(&mut self) -> u64 {
        let id = self.next_stable_entity_id;
        self.next_stable_entity_id = self.next_stable_entity_id.saturating_add(1);
        id
    }

    /// Increment owned count for the given owner when an entity spawns.
    pub(crate) fn increment_owned_count(&mut self, owner: &str, category: EntityCategory) {
        if let Some(house) = crate::sim::house_state::house_state_for_owner_mut(
            &mut self.houses,
            owner,
            &self.interner,
        ) {
            match category {
                EntityCategory::Structure => house.owned_building_count += 1,
                _ => house.owned_unit_count += 1,
            }
        }
    }

    /// Decrement owned count for the given owner when an entity dies or is despawned.
    pub(crate) fn decrement_owned_count(&mut self, owner: &str, category: EntityCategory) {
        if let Some(house) = crate::sim::house_state::house_state_for_owner_mut(
            &mut self.houses,
            owner,
            &self.interner,
        ) {
            match category {
                EntityCategory::Structure => {
                    house.owned_building_count = house.owned_building_count.saturating_sub(1)
                }
                _ => house.owned_unit_count = house.owned_unit_count.saturating_sub(1),
            }
        }
    }

    /// Despawn an entity by stable_id, removing it from EntityStore.
    /// Decrements owned count if the entity was not already dying (combat deaths
    /// are decremented when dying is first set, not at physical removal).
    /// Also removes the entity from the occupancy grid (origin cell only).
    pub(crate) fn despawn_entity(&mut self, stable_id: u64) {
        // Gather entity data before any mutable borrows.
        let entity_info = self.entities.get(stable_id).map(|e| {
            (
                e.dying,
                self.interner.resolve(e.owner).to_string(),
                e.category,
                e.position.rx,
                e.position.ry,
            )
        });
        if let Some((dying, owner_str, category, rx, ry)) = entity_info {
            if !dying {
                self.decrement_owned_count(&owner_str, category);
            }
            // Remove from occupancy grid (origin cell only; multi-cell structures
            // should have their foundation cells removed by the caller via
            // remove_entity_occupancy before calling despawn_entity).
            self.occupancy.remove(rx, ry, stable_id);
        }
        self.entities.remove(stable_id);
    }

    /// Check each house for defeat (owned count == 0) and game completion
    /// (all remaining houses mutually allied).
    fn check_defeat(&mut self) {
        // Mark houses with zero owned objects as defeated.
        let owners: Vec<InternedId> = self.houses.keys().copied().collect();
        for &owner in &owners {
            let house = &self.houses[&owner];
            if house.is_defeated {
                continue;
            }
            let total = house.owned_building_count + house.owned_unit_count;
            if total == 0 {
                if let Some(h) = self.houses.get_mut(&owner) {
                    h.is_defeated = true;
                }
            }
        }

        // Check if all remaining alive houses are mutually allied → game over.
        let alive: Vec<InternedId> = self
            .houses
            .iter()
            .filter(|(_, h)| !h.is_defeated)
            .map(|(k, _)| *k)
            .collect();

        if alive.is_empty() {
            return;
        }

        if alive.len() == 1 {
            // Last player standing.
            if let Some(h) = self.houses.get_mut(&alive[0]) {
                h.has_won = true;
            }
            return;
        }

        // O(n^2) bidirectional alliance check.
        let all_allied = alive.iter().all(|a| {
            alive.iter().all(|b| {
                a == b
                    || crate::map::houses::are_houses_friendly(
                        &self.house_alliances,
                        self.interner.resolve(*a),
                        self.interner.resolve(*b),
                    )
            })
        });

        if all_allied {
            for &owner in &alive {
                if let Some(h) = self.houses.get_mut(&owner) {
                    h.has_won = true;
                }
            }
        }
    }

    /// Restore skipped cache fields after snapshot deserialization.
    ///
    /// The caller must provide the same map/rules data that was used to initialize
    /// the original simulation. Cache fields were `#[serde(skip)]`'d and are at
    /// their Default values after deserialization.
    ///
    /// Note: `zone_grid` is NOT rebuilt here — it requires the app layer's
    /// `PathGrid` (built from `path_grid_base` + building footprints). The caller
    /// should call `rebuild_dynamic_path_grid()` after this method, which triggers
    /// `rebuild_zone_grid()` as part of the normal tick flow.
    pub fn rebuild_caches_after_load(
        &mut self,
        resolved_terrain: ResolvedTerrainGrid,
        terrain_speed_config: terrain_speed::TerrainSpeedConfig,
        bridge_explosions: Vec<InternedId>,
        effect_frame_counts: BTreeMap<InternedId, u16>,
        terrain_costs: BTreeMap<SpeedType, TerrainCostGrid>,
    ) {
        // 1. Restore externally-derived data
        self.resolved_terrain = Some(resolved_terrain);
        self.terrain_speed_config = terrain_speed_config;
        self.bridge_explosions = bridge_explosions;
        self.effect_frame_counts = effect_frame_counts;
        self.terrain_costs = terrain_costs;

        // 2. Rebuild cached screen coords for all entities
        for entity in self.entities.values_mut() {
            entity.position.refresh_screen_coords();
        }
    }

    pub fn refresh_vision_heights(&mut self, grid: &PathGrid) {
        let w = grid.width() as usize;
        let h = grid.height() as usize;
        let mut heights = vec![0u8; w * h];
        for y in 0..grid.height() {
            for x in 0..grid.width() {
                if let Some(cell) = grid.cell(x, y) {
                    heights[y as usize * w + x as usize] = cell.ground_level;
                }
            }
        }
        self.vision_height_grid = Some(heights);
    }

    /// Rebuild the zone connectivity map from the current PathGrid and terrain costs.
    /// Call after the PathGrid has been rebuilt so that zones reflect the latest
    /// walkability state.
    ///
    /// Tries an incremental update first (diffing against the previous PathGrid).
    /// Falls back to full rebuild if too many cells changed or no previous state.
    pub fn rebuild_zone_grid(&mut self, path_grid: &PathGrid) {
        let Some(terrain) = &self.resolved_terrain else {
            return;
        };
        let width = terrain.width();
        let height = terrain.height();

        // Try incremental update if we have previous state.
        if let (Some(prev), Some(zones)) = (&self.prev_path_grid, &mut self.zone_grid) {
            if let Some(changed) = prev.diff_cells(path_grid) {
                if changed.is_empty() {
                    // No cells changed — zones are still valid.
                    self.prev_path_grid = Some(path_grid.clone());
                    return;
                }
                if crate::sim::pathfinding::zone_incremental::try_incremental_update(
                    zones,
                    &changed,
                    path_grid,
                    &self.terrain_costs,
                    self.resolved_terrain.as_ref(),
                ) {
                    log::trace!("zone: incremental update ({} cells changed)", changed.len(),);
                    self.prev_path_grid = Some(path_grid.clone());
                    return;
                }
            }
        }

        // Full rebuild fallback.
        self.zone_grid = Some(ZoneGrid::build_with_terrain(
            path_grid,
            &self.terrain_costs,
            self.resolved_terrain.as_ref(),
            width,
            height,
        ));
        self.prev_path_grid = Some(path_grid.clone());
    }

    pub(crate) fn effective_build_blocked(&self, rx: u16, ry: u16) -> Option<bool> {
        let terrain = self.resolved_terrain.as_ref()?;
        let cell = terrain.cell(rx, ry)?;
        if let Some(bridge) = self
            .bridge_state
            .as_ref()
            .and_then(|state| state.cell(rx, ry))
        {
            return Some(if bridge.destroyed {
                cell.base_build_blocked
            } else {
                true
            });
        }
        Some(cell.build_blocked)
    }

    pub(crate) fn apply_bridge_damage_events(
        &mut self,
        bridge_damage_events: &[BridgeDamageEvent],
    ) -> Vec<BridgeStateChange> {
        let mut changes = Vec::new();
        let Some(bridge_state) = self.bridge_state.as_mut() else {
            return changes;
        };
        for event in bridge_damage_events {
            if let Some(change) = bridge_state.apply_damage(*event) {
                changes.push(change);
            }
        }
        changes
    }

    pub(crate) fn resolve_bridge_state_changes(
        &mut self,
        changes: &[BridgeStateChange],
    ) -> Vec<u64> {
        use std::collections::BTreeSet;

        if changes.is_empty() {
            return Vec::new();
        }

        let destroyed_cells: BTreeSet<(u16, u16)> = changes
            .iter()
            .flat_map(|change| change.destroyed_cells.iter().copied())
            .collect();
        let fallout_ground_grid = self.resolved_terrain.as_ref().map(|terrain| {
            PathGrid::from_resolved_terrain_with_bridges(terrain, self.bridge_state.as_ref())
        });

        // Spawn bridge destruction explosions (matches original BlowUpBridge logic):
        // ~95% chance per destroyed cell, random anim from BridgeExplosions list,
        // plus a second delayed explosion at 50% chance.
        self.spawn_bridge_explosions(&destroyed_cells);

        // Collect entity IDs that need bridge state changes — then mutate below.
        let mut to_snap: Vec<(u64, u8)> = Vec::new();
        let mut to_stop: Vec<u64> = Vec::new();
        let mut to_despawn: Vec<u64> = Vec::new();

        for entity in self.entities.values() {
            let pos = &entity.position;
            let on_bridge = entity.is_on_bridge_layer();
            if !on_bridge || !destroyed_cells.contains(&(pos.rx, pos.ry)) {
                continue;
            }
            let ground_walkable = fallout_ground_grid.as_ref().is_some_and(|grid| {
                grid.is_walkable_on_layer(pos.rx, pos.ry, MovementLayer::Ground)
            });
            if ground_walkable {
                let ground_level = self
                    .resolved_terrain
                    .as_ref()
                    .and_then(|terrain| terrain.cell(pos.rx, pos.ry))
                    .map(|cell| cell.level)
                    .unwrap_or(0);
                to_snap.push((entity.stable_id, ground_level));
                to_stop.push(entity.stable_id);
            } else {
                to_despawn.push(entity.stable_id);
            }
        }

        for (sid, ground_level) in to_snap {
            if let Some(entity) = self.entities.get_mut(sid) {
                entity.bridge_occupancy = None;
                entity.on_bridge = false;
                entity.position.z = ground_level;
                entity.position.refresh_screen_coords();
                if let Some(ref mut loco) = entity.locomotor {
                    loco.layer = MovementLayer::Ground;
                    loco.phase = GroundMovePhase::Idle;
                }
            }
        }
        for sid in to_stop {
            if let Some(entity) = self.entities.get_mut(sid) {
                entity.movement_target = None;
            }
        }

        let mut despawned_ids = Vec::new();
        for sid in to_despawn {
            despawned_ids.push(sid);
            self.despawn_entity(sid);
        }
        despawned_ids
    }

    /// Spawn explosion effects on destroyed bridge cells.
    ///
    /// Per the original engine's `BlowUpBridge()`:
    /// - ~95% of cells get at least one explosion from `BridgeExplosions=`
    /// - 50% chance for a second explosion with a random delay (1-5 frames)
    fn spawn_bridge_explosions(
        &mut self,
        destroyed_cells: &std::collections::BTreeSet<(u16, u16)>,
    ) {
        if self.bridge_explosions.is_empty() {
            return;
        }
        let explosion_count = self.bridge_explosions.len() as u32;

        for &(rx, ry) in destroyed_cells {
            // ~95% chance to spawn any explosion on this cell.
            if self.rng.next_range_u32(20) == 0 {
                continue;
            }

            let deck_level = self
                .resolved_terrain
                .as_ref()
                .and_then(|t| t.cell(rx, ry))
                .map(|c| c.bridge_deck_level_if_any().unwrap_or(c.level))
                .unwrap_or(0);

            // First explosion — always spawned (when we pass the 95% check).
            let idx = self.rng.next_range_u32(explosion_count) as usize;
            let anim_name = &self.bridge_explosions[idx];
            let frames = self
                .effect_frame_counts
                .get(anim_name)
                .copied()
                .unwrap_or(20);

            self.world_effects.push(WorldEffect {
                shp_name: anim_name.clone(),
                rx,
                ry,
                z: deck_level,
                frame: 0,
                total_frames: frames,
                rate_ms: 67, // ~15 fps (Normalized=yes anims)
                elapsed_ms: 0,
                translucent: true,
                delay_ms: 0,
            });

            // 50% chance for a second explosion with a random start delay (1-5 frames).
            if self.rng.next_range_u32(2) == 0 {
                let idx2 = self.rng.next_range_u32(explosion_count) as usize;
                let anim_name2 = &self.bridge_explosions[idx2];
                let frames2 = self
                    .effect_frame_counts
                    .get(anim_name2)
                    .copied()
                    .unwrap_or(20);
                let delay_frames = self.rng.next_range_u32(5) + 1;

                self.world_effects.push(WorldEffect {
                    shp_name: anim_name2.clone(),
                    rx,
                    ry,
                    z: deck_level,
                    frame: 0,
                    total_frames: frames2,
                    rate_ms: 67,
                    elapsed_ms: 0,
                    translucent: true,
                    delay_ms: delay_frames * 67,
                });
            }
        }
    }

    pub(crate) fn default_vision_range_for_category(category: EntityCategory) -> u16 {
        match category {
            EntityCategory::Infantry => 5,
            EntityCategory::Unit => 6,
            EntityCategory::Aircraft => 8,
            EntityCategory::Structure => 7,
        }
    }

    fn refresh_fog(
        &mut self,
        path_grid: Option<&PathGrid>,
        config: &vision::VisionConfig,
        rules: Option<&RuleSet>,
    ) {
        // Recompute visibility in-place: clears FLAG_VISIBLE on existing grids
        // (preserving FLAG_REVEALED) then re-reveals from entity positions.
        // No allocation or merge_revealed_from pass needed.
        vision::recompute_owner_visibility_in_place(
            &mut self.fog,
            &self.entities,
            path_grid,
            &self.house_alliances,
            config,
            self.vision_height_grid.as_deref(),
            &self.interner,
        );

        // Apply SpySat and Gap Generator effects if rules are available.
        if let Some(rules) = rules {
            let mut spy_sat_owners: Vec<InternedId> = Vec::new();
            let mut gap_generators: Vec<(InternedId, u16, u16)> = Vec::new();

            for entity in self.entities.values() {
                if entity.category != EntityCategory::Structure {
                    continue;
                }
                if let Some(obj) = rules.object(self.interner.resolve(entity.type_ref)) {
                    let active = power_system::is_building_powered(
                        &self.power_states,
                        rules,
                        entity,
                        &self.interner,
                    ) && entity.building_up.is_none();
                    if obj.spy_sat && active {
                        spy_sat_owners.push(entity.owner);
                    }
                    if obj.gap_generator && active {
                        gap_generators.push((entity.owner, entity.position.rx, entity.position.ry));
                    }
                }
            }

            // Apply in order: SpySat first, then Gap Generator (gap wins in contested areas).
            if !spy_sat_owners.is_empty() {
                vision::apply_spy_sat(&mut self.fog, &spy_sat_owners, &self.interner);
            }
            if !gap_generators.is_empty() {
                vision::apply_gap_generators(
                    &mut self.fog,
                    &gap_generators,
                    rules.general.gap_radius,
                    &self.interner,
                );
            }
        }

        // Diagnostic: log fog grid stats on first tick to debug coverage issues.
        if self.tick == 1 {
            log::info!(
                "Fog grid: {}x{}, {} owners",
                self.fog.width,
                self.fog.height,
                self.fog.by_owner.len()
            );
            for (owner, vis) in &self.fog.by_owner {
                let total = vis.width() as u32 * vis.height() as u32;
                let visible_count = vis.cells_raw().iter().filter(|c| **c & 0x02 != 0).count();
                let revealed_count = vis.cells_raw().iter().filter(|c| **c & 0x01 != 0).count();
                log::info!(
                    "  Owner '{}': {}/{} visible, {}/{} revealed",
                    owner,
                    visible_count,
                    total,
                    revealed_count,
                    total
                );
            }
            use std::collections::BTreeMap as DiagMap;
            let mut entity_stats: DiagMap<String, (u32, u16, u16, u16, u16)> = DiagMap::new();
            for entity in self.entities.values() {
                let entry = entity_stats
                    .entry(self.interner.resolve(entity.owner).to_string())
                    .or_insert((0, u16::MAX, u16::MAX, 0, 0));
                entry.0 += 1;
                entry.1 = entry.1.min(entity.position.rx);
                entry.2 = entry.2.min(entity.position.ry);
                entry.3 = entry.3.max(entity.position.rx);
                entry.4 = entry.4.max(entity.position.ry);
            }
            for (owner, (count, min_rx, min_ry, max_rx, max_ry)) in &entity_stats {
                log::info!(
                    "  Entities '{}': {} units, rx={}..{}, ry={}..{}",
                    owner,
                    count,
                    min_rx,
                    max_rx,
                    min_ry,
                    max_ry
                );
            }
        }
    }

    /// Advance build-up animations: increment elapsed ticks, remove when done.
    fn tick_building_up(&mut self) {
        // Collect keys first to allow &mut iteration via get_mut().
        let keys = self.entities.keys_sorted();
        let mut finished: Vec<u64> = Vec::new();
        for &sid in &keys {
            if let Some(entity) = self.entities.get_mut(sid) {
                if let Some(ref mut bu) = entity.building_up {
                    bu.elapsed_ticks = bu.elapsed_ticks.saturating_add(1);
                    if bu.elapsed_ticks >= bu.total_ticks {
                        finished.push(sid);
                    }
                }
            }
        }
        for sid in finished {
            if let Some(entity) = self.entities.get_mut(sid) {
                entity.building_up = None;
            }
        }
    }

    /// Advance building-down (undeploy) animations. When done, despawn the
    /// building and spawn the mobile unit (e.g., ConYard → MCV).
    /// Returns true if any entities were spawned (triggers atlas refresh).
    fn tick_building_down(&mut self, rules: Option<&RuleSet>) -> bool {
        let keys = self.entities.keys_sorted();
        let mut finished: Vec<u64> = Vec::new();
        for &sid in &keys {
            if let Some(entity) = self.entities.get_mut(sid) {
                if let Some(ref mut bd) = entity.building_down {
                    bd.elapsed_ticks = bd.elapsed_ticks.saturating_add(1);
                    if bd.elapsed_ticks >= bd.total_ticks {
                        finished.push(sid);
                    }
                }
            }
        }
        let any_finished = !finished.is_empty();
        for sid in finished {
            // Extract spawn data before despawning.
            let spawn_data = self.entities.get(sid).and_then(|e| {
                e.building_down.as_ref().map(|bd| {
                    (
                        bd.spawn_type,
                        bd.spawn_owner,
                        bd.spawn_rx,
                        bd.spawn_ry,
                        bd.spawn_z,
                        bd.was_selected,
                    )
                })
            });
            let Some((unit_type_id, owner_id, rx, ry, z, was_selected)) = spawn_data else {
                continue;
            };
            self.despawn_entity(sid);
            let rules = match rules {
                Some(r) => r,
                None => continue,
            };
            let unit_type_str = self.interner.resolve(unit_type_id).to_string();
            let owner_str = self.interner.resolve(owner_id).to_string();
            if let Some(new_sid) =
                self.spawn_object_at_height(&unit_type_str, &owner_str, rx, ry, 0, z, rules)
            {
                if let Some(ge) = self.entities.get_mut(new_sid) {
                    ge.selected = was_selected;
                }
            }
        }
        any_finished
    }

    /// Advance one deterministic simulation tick.
    pub fn advance_tick(
        &mut self,
        commands: &[CommandEnvelope],
        rules: Option<&RuleSet>,
        height_map: &BTreeMap<(u16, u16), u8>,
        path_grid: Option<&PathGrid>,
        tick_ms: u32,
    ) -> TickResult {
        let execute_tick = self.tick.saturating_add(1);
        let mut executed_commands = 0usize;
        let mut spawned_entities = false;
        let mut destroyed_structure = false;
        let mut passenger_ownership_changed = false;

        let mut due: Vec<&CommandEnvelope> = commands
            .iter()
            .filter(|c| c.execute_tick <= execute_tick)
            .collect();
        due.sort_by(|a, b| {
            a.execute_tick
                .cmp(&b.execute_tick)
                .then_with(|| a.owner.cmp(&b.owner))
        });

        for cmd in due {
            let cmd_owner_str = self.interner.resolve(cmd.owner).to_string();
            let applied =
                self.apply_command(&cmd_owner_str, &cmd.payload, rules, path_grid, height_map);
            if applied {
                if matches!(
                    cmd.payload,
                    Command::PlaceReadyBuilding { .. }
                        | Command::DeployMcv { .. }
                        | Command::UndeployBuilding { .. }
                ) {
                    spawned_entities = true;
                }
                if matches!(
                    cmd.payload,
                    Command::SellBuilding { .. } | Command::UndeployBuilding { .. }
                ) {
                    destroyed_structure = true;
                }
            }
            executed_commands += 1;
        }

        // --- Phase 1: Ground movement ---
        // DEPENDS ON: commands (may set movement_target), entity positions from prior tick.
        // PRODUCES: updated entity positions, crush/bump effects, drive track state.
        let movement_stats = movement::tick_movement_with_grids(
            &mut self.entities,
            path_grid,
            &self.terrain_costs,
            &self.house_alliances,
            &self.occupancy,
            &mut self.rng,
            tick_ms,
            self.tick,
            self.zone_grid.as_ref(),
            self.resolved_terrain.as_ref(),
            &self.terrain_speed_config,
            self.close_enough,
            self.path_delay_ticks,
            self.blockage_path_delay_ticks,
            &self.interner,
        );
        // --- Phase 2: Air + special movement ---
        // DEPENDS ON: commands (may set movement targets for air/special units).
        // INDEPENDENT OF: ground movement (air units bypass A* and occupancy).
        air_movement::tick_air_movement(&mut self.entities, tick_ms, self.tick);
        teleport_movement::tick_teleport_movement(
            &mut self.entities,
            &mut self.occupancy,
            tick_ms,
            self.tick,
        );
        tunnel_movement::tick_tunnel_movement(
            &mut self.entities,
            &mut self.occupancy,
            tick_ms,
            self.tick,
        );
        let _rocket_detonations =
            rocket_movement::tick_rocket_movement(&mut self.entities, tick_ms, self.tick);
        droppod_movement::tick_droppod_movement(&mut self.entities, tick_ms, self.tick);

        // Aircraft mission state machines — between movement and combat.
        // Reads updated positions, controls firing and RTB decisions.
        if let Some(rules) = rules {
            crate::sim::aircraft::tick_aircraft_missions(self, rules);
        }

        // Spawn wake effects behind moving ships on water (every 8 ticks).
        if self.tick & 7 == 0 {
            if let Some(rules) = rules {
                let wake_name_str = &rules.general.wake.name;
                let wake_rate = rules.general.wake.rate_ms;
                let wake_name_id = self.interner.get(&wake_name_str.to_uppercase());
                let wake_frames = wake_name_id
                    .and_then(|id| self.effect_frame_counts.get(&id).copied())
                    .unwrap_or(8);
                // Collect positions to avoid borrow conflict (read entities, write world_effects).
                let wake_positions: Vec<(u16, u16, u8)> = self
                    .entities
                    .keys_sorted()
                    .iter()
                    .filter_map(|id| {
                        let e = self.entities.get(*id)?;
                        if e.movement_target.is_none() {
                            return None;
                        }
                        let loco = e.locomotor.as_ref()?;
                        let is_water_mover = loco.movement_zone.is_water_mover();
                        if !is_water_mover {
                            return None;
                        }
                        Some((e.position.rx, e.position.ry, e.position.z))
                    })
                    .collect();
                if let Some(wake_id) = wake_name_id {
                    for (rx, ry, z) in wake_positions {
                        self.world_effects.push(WorldEffect {
                            shp_name: wake_id,
                            rx,
                            ry,
                            z,
                            frame: 0,
                            total_frames: wake_frames,
                            rate_ms: wake_rate,
                            elapsed_ms: 0,
                            translucent: true,
                            delay_ms: 0,
                        });
                    }
                }
            }
        }

        // --- Phase 3: Vision refresh ---
        // DEPENDS ON: movement (positions updated), spawn (new entities need LOS).
        // PRODUCES: fog state used by combat targeting (phase 5).
        let vision_config = vision::VisionConfig {
            veteran_sight_bonus: rules.map_or(0, |r| r.general.veteran_sight),
            leptons_per_sight_increase: rules.map_or(0, |r| r.general.leptons_per_sight_increase),
            reveal_by_height: rules.map_or(true, |r| r.general.reveal_by_height),
        };
        self.refresh_fog(path_grid, &vision_config, rules);

        if let Some(rules) = rules {
            // --- Phase 4: Power ---
            // DEPENDS ON: entity health (damaged buildings produce less power).
            // PRODUCES: power_states used by combat (cloaking) and production (build speed).
            let _power_events = power_system::tick_power_states(
                &mut self.power_states,
                &mut self.entities,
                rules,
                tick_ms,
                &self.interner,
            );
            // --- Phase 5: Turrets + Combat ---
            // DEPENDS ON: vision/fog (targeting uses fog state), power (cloaking),
            //   turret rotation MUST run before combat so turrets are aligned when firing.
            // PRODUCES: damage, deaths, bridge damage, fire events, last_attacker_id.
            turret::tick_turret_rotation(&mut self.entities, rules, tick_ms, &self.interner);
            spawned_entities |= self.tick_capture_orders();
            self.tick_order_intents_pre_combat(rules);
            let combat_result = combat::tick_combat_with_fog(
                &mut self.entities,
                &mut self.occupancy,
                rules,
                &mut self.interner,
                Some(&self.fog),
                &self.power_states,
                Some(&mut self.sound_events),
                &mut self.production.resource_nodes,
                tick_ms,
            );
            destroyed_structure |= combat_result.structure_destroyed;
            // Decrement owned counts for entities killed in combat (dying=true set this tick).
            for &dead_id in &combat_result.despawned_ids {
                if let Some(entity) = self.entities.get(dead_id) {
                    let owner_str = self.interner.resolve(entity.owner).to_string();
                    let category = entity.category;
                    self.decrement_owned_count(&owner_str, category);
                }
            }
            let bridge_changes =
                self.apply_bridge_damage_events(&combat_result.bridge_damage_events);
            // resolve_bridge_state_changes calls despawn_entity() internally.
            let _bridge_fallout_ids = self.resolve_bridge_state_changes(&bridge_changes);
            // Apply RevealOnFire events from combat.
            for ev in &combat_result.reveal_events {
                vision::reveal_radius(&mut self.fog, ev.owner, ev.rx, ev.ry, ev.radius);
            }
            // SpySat reshroud: when a SpySat building is destroyed, fully reshroud
            // its owner. Current LOS will re-reveal on the next vision tick.
            for &owner_id in &combat_result.spy_sat_reshroud_owners {
                self.fog.reset_explored_for_owner(owner_id);
            }
            // Eject survivors from crewed buildings destroyed in combat.
            for bldg in &combat_result.destroyed_crewed_buildings {
                production::eject_destruction_survivors(
                    self,
                    rules,
                    bldg.type_id,
                    bldg.owner,
                    bldg.rx,
                    bldg.ry,
                    bldg.z,
                );
            }
            // Spawn explosion animations from combat deaths.
            for fx in &combat_result.explosion_effects {
                let frames = self
                    .effect_frame_counts
                    .get(&fx.shp_name)
                    .copied()
                    .unwrap_or(20);
                self.world_effects.push(WorldEffect {
                    shp_name: fx.shp_name,
                    rx: fx.rx,
                    ry: fx.ry,
                    z: fx.z,
                    frame: 0,
                    total_frames: frames,
                    rate_ms: 67, // ~15fps, standard for Normalized=yes explosion anims
                    elapsed_ms: 0,
                    translucent: true,
                    delay_ms: 0,
                });
            }
            // Collect fire events for render-side muzzle flash / projectile origin.
            self.fire_events.extend(combat_result.fire_events);
            // Emit radar events for combat occurrences.
            let event_dur: u32 = rules.radar_event_config.event_duration_ms;
            for ev in &combat_result.reveal_events {
                self.radar_events
                    .push(RadarEventType::Combat, ev.rx, ev.ry, event_dur);
            }
            // --- Phase 6: Retaliation + Passengers ---
            // DEPENDS ON: combat (sets last_attacker_id read by retaliation).
            combat::tick_retaliation(&mut self.entities, rules, &self.interner);
            passenger_ownership_changed = passenger::tick_passenger_system(self, rules);
            self.tick_order_intents_post_combat(path_grid, Some(rules));
            // --- Phase 7: Scatter + Production + Repairs + Docks + Ore ---
            // DEPENDS ON: combat (dead entities removed), movement (positions stable).
            // PRODUCES: new entities (spawned units), credit changes, ore growth.
            // Idle scatter disabled — units were moving on their own after reaching
            // destination. Needs further RE to match original engine conditions before
            // re-enabling.
            // scatter::tick_idle_scatter(
            //     &mut self.entities,
            //     Some(rules),
            //     path_grid,
            //     &self.terrain_costs,
            //     &mut self.rng,
            //     self.tick,
            //     &self.interner,
            // );
            spawned_entities |=
                production::tick_production(self, rules, height_map, path_grid, tick_ms);
            production::tick_repairs(self, rules);
            building_dock::tick_building_docks(self, rules);
            aircraft_dock::tick_aircraft_docks(self, rules);
            // Ore growth/spread: incremental scan driven by rules.ini GrowthRate.
            ore_growth::tick_ore_growth(
                &self.production.ore_growth_config,
                &mut self.production.ore_growth_state,
                &mut self.production.resource_nodes,
                path_grid,
                &mut self.rng,
            );
            if spawned_entities {
                self.refresh_fog(path_grid, &vision_config, Some(rules));
            }
        }

        // --- Phase 8: AI ---
        // DEPENDS ON: all prior phases (AI reads full game state to make decisions).
        // PRODUCES: commands applied immediately in the same tick.
        // AI decision loop: generate commands for computer players.
        // Temporarily take ai_players out to avoid borrow conflict with &self.
        if rules.is_some() && !self.ai_players.is_empty() {
            let mut ai_state = std::mem::take(&mut self.ai_players);
            let ai_commands = ai::tick_ai(
                self,
                &mut ai_state,
                rules.expect("rules checked above"),
                path_grid,
                height_map,
            );
            self.ai_players = ai_state;
            for cmd in &ai_commands {
                let cmd_owner_str = self.interner.resolve(cmd.owner).to_string();
                let applied =
                    self.apply_command(&cmd_owner_str, &cmd.payload, rules, path_grid, height_map);
                if applied
                    && matches!(
                        cmd.payload,
                        Command::PlaceReadyBuilding { .. }
                            | Command::DeployMcv { .. }
                            | Command::UndeployBuilding { .. }
                    )
                {
                    spawned_entities = true;
                }
            }
        }

        // --- Phase 8.5: Defeat detection ---
        // DEPENDS ON: combat (deaths processed), production (spawns), AI (commands applied).
        // Runs after all game-state mutations so owned counts are final for this tick.
        if self.tick > 0 {
            self.check_defeat();
        }

        // --- Phase 9: Building animations + cleanup ---
        // DEPENDS ON: production (newly placed buildings start build-up).
        self.tick_building_up();
        // Advance building-down (undeploy) animations; spawn units when done.
        spawned_entities |= self.tick_building_down(rules);

        // Tick radar event aging (remove expired pings).
        self.radar_events.tick(tick_ms);

        // Tick world-effect animations and remove finished ones.
        self.world_effects.retain_mut(|fx| !fx.tick(tick_ms));

        // Debug-mode safety net: rebuild occupancy from scratch and compare
        // with the persistent grid. Catches missed add/remove calls.
        // Note: rebuild() only registers single cells (no multi-cell foundations),
        // so this check is conservative — extra cells from foundations are expected.
        // Enable via OCCUPANCY_DEBUG=1 environment variable for focused debugging.
        #[cfg(debug_assertions)]
        if std::env::var("OCCUPANCY_DEBUG").is_ok() {
            let expected = OccupancyGrid::rebuild(&self.entities);
            self.occupancy.debug_assert_matches(&expected);
        }

        self.tick = execute_tick;
        let state_hash = self.state_hash();
        TickResult {
            tick: self.tick,
            executed_commands,
            state_hash,
            spawned_entities,
            destroyed_structure,
            ownership_changed: passenger_ownership_changed,
            movement: movement_stats,
        }
    }
}

#[cfg(test)]
#[path = "world_tests.rs"]
mod tests;
