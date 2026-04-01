//! Unified entity struct replacing hecs ECS components.
//!
//! All 31 former ECS components are fields on `GameEntity`. Always-present
//! data is stored directly; optional/conditional components use `Option<T>`.
//! Zero-size markers (Selected, Repairing, VoxelModel/SpriteModel) become bools.
//!
//! ## Why plain structs?
//! - Deterministic iteration (sorted by stable_id) without per-query sorting
//! - Direct field access (`entity.position`) instead of `world.get::<&Position>(e)`
//! - No two-phase snapshot patterns needed for simple mutations
//! - Simpler borrow checker interactions than ECS archetype queries
//!
//! ## Dependency rules
//! - Part of sim/ — depends on map/ (EntityCategory), sim/components, sim/locomotor,
//!   sim/combat (AttackTarget), sim/animation, sim/miner, and special movement modules.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::map::entities::EntityCategory;
use crate::sim::aircraft::AircraftMission;
use crate::sim::animation::Animation;
use crate::sim::combat::AttackTarget;
use crate::sim::components::{
    BridgeOccupancy, BuildingAnimOverlays, BuildingDown, BuildingUp, DamageFireOverlays,
    HarvestOverlay, Health, MovementTarget, OrderIntent, Position, VoxelAnimation,
};
use crate::sim::debug_event_log::{DebugEventKind, DebugEventLog};
use crate::sim::docking::aircraft_dock::AircraftAmmo;
use crate::sim::docking::building_dock::DockState;
use crate::sim::intern::InternedId;
use crate::sim::miner::Miner;
use crate::sim::movement::drive_track::DriveTrackState;
use crate::sim::movement::droppod_movement::DropPodState;
use crate::sim::movement::locomotor::LocomotorState;
use crate::sim::movement::rocket_movement::RocketState;
use crate::sim::movement::teleport_movement::TeleportState;
use crate::sim::movement::tunnel_movement::TunnelState;
use crate::sim::passenger::PassengerRole;
use crate::sim::slave_miner::SlaveHarvester;

/// Unified entity struct — replaces all hecs ECS components.
///
/// Every game object (unit, infantry, building, aircraft) is one `GameEntity`.
/// Core fields are always present; optional subsystems use `Option<T>`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GameEntity {
    // --- Always present (every entity has these) ---
    /// Deterministic stable ID — primary key, used for cross-entity references,
    /// replay logs, state hashing, and networking. Never reused.
    pub stable_id: u64,
    /// World position in isometric cell coordinates + cached screen position.
    pub position: Position,
    /// Body facing direction (0–255, RA2 convention: 0=N, 64=E, 128=S, 192=W).
    pub facing: u8,
    /// Target body facing for gradual rotation (vehicles only).
    /// When `Some`, the entity is rotating in place and should not advance position.
    /// Infantry always turn instantly (RA2 behavior), so this stays `None` for them.
    pub facing_target: Option<u8>,
    /// Owning player/faction name (e.g., "Americans", "Soviet") — interned for zero-cost clones.
    pub owner: InternedId,
    /// Current and maximum hit points.
    pub health: Health,
    /// rules.ini section name (e.g., "HTNK", "E1", "GAPOWR") — interned for zero-cost clones.
    pub type_ref: InternedId,
    /// Entity category: Unit, Infantry, Aircraft, or Structure.
    pub category: EntityCategory,
    /// Veterancy level: 0 = rookie, 100 = veteran, 200 = elite.
    pub veterancy: u16,
    /// Fog-of-war sight range in cells.
    pub vision_range: u16,

    // --- Render model (mutually exclusive) ---
    /// true = VXL voxel model (vehicles/aircraft), false = SHP sprite (infantry/buildings).
    pub is_voxel: bool,

    // --- Bool markers (were zero-size ECS components) ---
    /// Whether this entity is currently selected by the local player.
    /// App-layer state — NOT part of authoritative simulation. Never read by sim logic.
    /// Mutations: `Command::Select` → `apply_selection_snapshot()` in world_commands.rs;
    /// combat.rs sets `selected = false` on death/transport entry.
    pub selected: bool,
    /// Building is being repaired (spending credits to heal).
    pub repairing: bool,

    // --- Optional subsystem components ---
    /// Locomotor state — present on movable entities (speed > 0 in rules.ini).
    pub locomotor: Option<LocomotorState>,
    /// Active movement path — present when unit is moving along an A* path.
    pub movement_target: Option<MovementTarget>,
    /// Active attack target — present when entity is firing at something.
    pub attack_target: Option<AttackTarget>,
    /// Stable ID of the last entity that dealt damage (for retaliation).
    pub last_attacker_id: Option<u64>,
    /// Independent turret facing — only on entities with Turret=yes in rules.ini.
    /// 16-bit DirStruct (0–65535), full FacingClass precision.
    pub turret_facing: Option<u16>,
    /// Building construction animation progress.
    pub building_up: Option<BuildingUp>,
    /// Reverse build-up animation — building is undeploying into a mobile unit.
    pub building_down: Option<BuildingDown>,
    /// Active one-shot building animation overlays (e.g., ConYard crane).
    pub building_anim_overlays: Option<BuildingAnimOverlays>,
    /// Persistent fire/smoke overlays on damaged buildings (health < ConditionYellow).
    pub damage_fire_overlays: Option<DamageFireOverlays>,
    /// Bridge deck occupancy marker.
    pub bridge_occupancy: Option<BridgeOccupancy>,
    /// Persistent bridge layer flag — authoritative source for "is this entity on a bridge?"
    /// Mirrors original engine's FootClass+0x8C. Survives repath operations that reset
    /// locomotor.layer. Set during spawn, updated at cell-crossing bridge transitions.
    #[serde(default)]
    pub on_bridge: bool,
    /// Infantry sprite animation state (sequence + frame + timing).
    pub animation: Option<Animation>,
    /// Voxel HVA animation state (frame cycling for multi-frame models).
    pub voxel_animation: Option<VoxelAnimation>,
    /// Harvest overlay animation (oregath.shp ore-gathering visual).
    pub harvest_overlay: Option<HarvestOverlay>,
    /// Harvester state machine (ore collection, refinery docking, cargo).
    pub miner: Option<Miner>,
    /// Slave infantry harvest AI (picks up ore, returns to master Slave Miner).
    pub slave_harvester: Option<SlaveHarvester>,
    /// Persistent high-level order (AttackMove, Guard) that survives transient state changes.
    pub order_intent: Option<OrderIntent>,
    /// Teleport movement state machine (warp out/in phases).
    pub teleport_state: Option<TeleportState>,
    /// Tunnel movement state machine (dig in/underground/dig out phases).
    pub tunnel_state: Option<TunnelState>,
    /// Rocket/missile flight state machine (launch/ascend/terminal/detonate).
    pub rocket_state: Option<RocketState>,
    /// Drop pod descent state machine (falling/landing).
    pub droppod_state: Option<DropPodState>,
    /// Active drive track curve state — present when a Drive vehicle is
    /// following a pre-computed curved path between cells.
    pub drive_track: Option<DriveTrackState>,
    /// Docking state machine — present when unit is approaching, waiting,
    /// or servicing at a repair depot.
    pub dock_state: Option<DockState>,
    /// Aircraft ammo tracking and airfield docking state.
    /// Present on aircraft with finite `Ammo=` (>= 0) from rules.ini.
    /// None for unlimited-ammo aircraft (`Ammo=-1`) and non-aircraft entities.
    pub aircraft_ammo: Option<AircraftAmmo>,
    /// Aircraft mission state machine — controls attack runs, guard, RTB, idle.
    /// Present on aircraft with Fly locomotor. None for non-aircraft and jumpjets.
    pub aircraft_mission: Option<AircraftMission>,
    /// Infantry sub-cell position (0–4). Only meaningful for infantry.
    pub sub_cell: Option<u8>,
    /// Whether this entity can be crushed by vehicles (Crushable= in rules.ini).
    /// Default false — only specific infantry and some walls are crushable.
    pub crushable: bool,
    /// Whether this entity can crush non-Crushable targets (OmniCrusher= in rules.ini).
    /// Only Battle Fortress has this in YR.
    pub omni_crusher: bool,
    /// Whether this entity is immune to ALL crush types (OmniCrushResistant= in rules.ini).
    pub omni_crush_resistant: bool,
    /// Render-only depth bias used when this entity is under or near a bridge.
    pub zfudge_bridge: i32,
    /// Prevents the unit from taking under-bridge water routes.
    pub too_big_to_fit_under_bridge: bool,
    /// Whether this entity is playing its death animation (health=0, not yet despawned).
    /// Dying entities are excluded from combat targeting, pathfinding, and selection.
    pub dying: bool,
    /// Ticks remaining before a permanently blocked infantry scatters sideways.
    /// Set when movement is stuck on a non-temporary obstacle; counts down each tick.
    /// When it reaches 0, the unit scatters to a random adjacent cell instead of
    /// endlessly repathing to the same blocked destination.
    /// Original engine: 30-frame scatter queue interval.
    pub blocked_scatter_timer: u8,

    // --- Passenger/transport system ---
    /// Original owner of a CanBeOccupied building, saved when the first garrison
    /// occupant enters. Used to revert ownership when the last occupant exits.
    /// Matches original engine's `CheckAutoSellOrCivilian` which transfers back
    /// to the Civilian house — we store the actual pre-garrison owner instead of
    /// hardcoding "Neutral".
    pub garrison_original_owner: Option<InternedId>,
    /// Combined passenger/transport role — replaces separate passenger_cargo,
    /// transport_id, and boarding_state fields. See `PassengerRole` variants.
    pub passenger_role: PassengerRole,
    /// Active IFV weapon index override. When Some(n), the transport uses
    /// weapon_list[n] instead of its default Primary weapon. Set when a
    /// Gunner=yes transport has a passenger with IFVMode=N.
    pub ifv_weapon_index: Option<u32>,
    /// Temporary VXL model override for visual-only state changes.
    /// When Some, the renderer should use this type's VXL model instead of `type_ref`.
    /// Set during refinery unloading (UnloadingClass= from rules.ini).
    pub display_type_override: Option<InternedId>,
    /// True while a miner is docked and unloading — tells the renderer to play
    /// the refinery's ActiveAnim overlays (e.g. GAREFNL1 unloading arm).
    /// Set on the *refinery* entity, not the miner.
    pub dock_active_anim: bool,
    /// Target building for engineer capture. Set by CaptureBuilding command,
    /// cleared on arrival (after capture) or if target is lost/destroyed.
    pub capture_target: Option<u64>,
    /// Debug event log — records movement/state transitions for the inspector panel.
    /// Only allocated when debug inspector is active (X hotkey). Not included in state hashing.
    #[serde(skip)]
    pub debug_log: Option<DebugEventLog>,
}

impl GameEntity {
    /// Create a new entity with all required fields. Optional fields default to None/false.
    pub fn new(
        stable_id: u64,
        rx: u16,
        ry: u16,
        z: u8,
        facing: u8,
        owner: InternedId,
        health: Health,
        type_ref: InternedId,
        category: EntityCategory,
        veterancy: u16,
        vision_range: u16,
        is_voxel: bool,
    ) -> Self {
        // Infantry spawn at sub-cell 2 (top of diamond) instead of cell center
        // so they don't overlap with other units at the same position.
        let (init_sub_x, init_sub_y) = if category == EntityCategory::Infantry {
            crate::util::lepton::subcell_lepton_offset(Some(2))
        } else {
            (
                crate::util::lepton::CELL_CENTER_LEPTON,
                crate::util::lepton::CELL_CENTER_LEPTON,
            )
        };
        let (screen_x, screen_y) =
            crate::util::lepton::lepton_to_screen(rx, ry, init_sub_x, init_sub_y, z);
        Self {
            stable_id,
            position: Position {
                rx,
                ry,
                z,
                sub_x: init_sub_x,
                sub_y: init_sub_y,
                screen_x,
                screen_y,
            },
            facing,
            facing_target: None,
            owner,
            health,
            type_ref,
            category,
            veterancy,
            vision_range,
            is_voxel,
            selected: false,
            repairing: false,
            locomotor: None,
            movement_target: None,
            attack_target: None,
            last_attacker_id: None,
            turret_facing: None,
            building_up: None,
            building_down: None,
            building_anim_overlays: None,
            damage_fire_overlays: None,
            bridge_occupancy: None,
            on_bridge: false,
            animation: None,
            voxel_animation: None,
            harvest_overlay: None,
            miner: None,
            slave_harvester: None,
            order_intent: None,
            teleport_state: None,
            tunnel_state: None,
            rocket_state: None,
            droppod_state: None,
            drive_track: None,
            dock_state: None,
            aircraft_ammo: None,
            aircraft_mission: None,
            // Infantry get sub-cell 2 (first distinct position) at spawn so
            // they don't all pile up at cell center when multiple are created.
            sub_cell: if category == EntityCategory::Infantry {
                Some(2)
            } else {
                None
            },
            crushable: false,
            omni_crusher: false,
            omni_crush_resistant: false,
            zfudge_bridge: 7,
            too_big_to_fit_under_bridge: false,
            dying: false,
            blocked_scatter_timer: 0,
            garrison_original_owner: None,
            passenger_role: PassengerRole::None,
            ifv_weapon_index: None,
            display_type_override: None,
            dock_active_anim: false,
            capture_target: None,
            debug_log: None,
        }
    }

    /// Record a debug event if the event log is active. No-op when `debug_log` is `None`.
    pub fn push_debug_event(&mut self, tick: u32, kind: DebugEventKind) {
        if let Some(log) = &mut self.debug_log {
            log.push(tick, kind);
        }
    }

    /// Shared owner for "which movement layer should this entity be treated as on?"
    ///
    /// `on_bridge` is the authoritative source for bridge layer state — it survives
    /// repath operations that may reset `locomotor.layer`. Ground is the default
    /// when no locomotor is attached.
    pub fn movement_layer_or_ground(&self) -> crate::sim::movement::locomotor::MovementLayer {
        if self.on_bridge {
            return crate::sim::movement::locomotor::MovementLayer::Bridge;
        }
        self.locomotor.as_ref().map_or(
            crate::sim::movement::locomotor::MovementLayer::Ground,
            |l| l.layer,
        )
    }

    /// Whether this entity is currently on a bridge deck.
    pub fn is_on_bridge_layer(&self) -> bool {
        self.on_bridge
    }

    /// Create a minimal entity for testing. Fills sensible defaults for most fields.
    #[cfg(test)]
    /// Create a minimal test entity with the given owner and type_ref strings.
    /// Uses a shared test interner via `test_intern()` for consistent IDs.
    pub fn test_default(stable_id: u64, type_ref: &str, owner: &str, rx: u16, ry: u16) -> Self {
        Self::new(
            stable_id,
            rx,
            ry,
            0, // z = ground level
            0, // facing = north
            crate::sim::intern::test_intern(owner),
            Health {
                current: 100,
                max: 100,
            },
            crate::sim::intern::test_intern(type_ref),
            EntityCategory::Unit,
            0, // veterancy = rookie
            5, // vision_range = 5 cells
            true,
        )
    }

    /// Whether this entity is alive (health > 0).
    pub fn is_alive(&self) -> bool {
        self.health.current > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::terrain;

    #[test]
    fn test_new_entity_defaults() {
        let e = GameEntity::test_default(1, "HTNK", "Americans", 30, 40);
        assert_eq!(e.stable_id, 1);
        assert_eq!(e.type_ref, crate::sim::intern::test_intern("HTNK"));
        assert_eq!(e.owner, crate::sim::intern::test_intern("Americans"));
        assert_eq!(e.position.rx, 30);
        assert_eq!(e.position.ry, 40);
        assert_eq!(e.position.z, 0);
        assert_eq!(e.facing, 0);
        assert_eq!(e.health.current, 100);
        assert_eq!(e.health.max, 100);
        assert_eq!(e.category, EntityCategory::Unit);
        assert_eq!(e.veterancy, 0);
        assert_eq!(e.vision_range, 5);
        assert!(e.is_voxel);
        assert!(!e.selected);
        assert!(!e.repairing);
        assert!(e.locomotor.is_none());
        assert!(e.movement_target.is_none());
        assert!(e.attack_target.is_none());
        assert!(e.last_attacker_id.is_none());
        assert!(e.turret_facing.is_none());
        assert!(e.miner.is_none());
        assert!(e.order_intent.is_none());
        assert!(!e.on_bridge);
    }

    #[test]
    fn test_is_alive() {
        let mut e = GameEntity::test_default(1, "E1", "Soviet", 10, 10);
        assert!(e.is_alive());
        e.health.current = 0;
        assert!(!e.is_alive());
    }

    #[test]
    fn test_screen_coords_computed() {
        let e = GameEntity::new(
            1,
            30,
            40,
            2, // z=2 for elevation
            0,
            crate::sim::intern::test_intern("Americans"),
            Health {
                current: 100,
                max: 100,
            },
            crate::sim::intern::test_intern("HTNK"),
            EntityCategory::Unit,
            0,
            5,
            true,
        );
        // lepton_to_screen = CoordsToClient(cell_center) = iso_to_screen + (30, 15)
        let (corner_sx, corner_sy) = terrain::iso_to_screen(30, 40, 2);
        assert!((e.position.screen_x - (corner_sx + 30.0)).abs() < 0.01);
        assert!((e.position.screen_y - corner_sy).abs() < 0.01);
    }
}
