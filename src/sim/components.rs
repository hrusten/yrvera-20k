//! Shared data structs used as fields of GameEntity and components.rs.
//!
//! These are plain data types with no behavior. Game logic lives in systems
//! (movement, combat, etc.). The render loop reads GameEntity fields to
//! determine what to draw and where.
//!
//! ## Design notes
//! - Position stores both isometric cell coords AND pre-computed screen coords.
//!   Screen coords are updated whenever position changes (avoids per-frame math).
//! - Some types here (Facing, Owner, VoxelModel, etc.) are legacy wrappers
//!   kept for any remaining call sites. The canonical data lives in GameEntity fields.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on map/ (EntityCategory type).
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::map::entities::EntityCategory;
use crate::sim::intern::InternedId;
use crate::sim::movement::locomotor::MovementLayer;
use crate::util::fixed_math::{SimFixed, SIM_ZERO};

/// World position in isometric cell coordinates plus sub-cell lepton offset.
///
/// RA2 uses leptons as its spatial unit (256 leptons = 1 cell). We store the
/// cell coordinate (rx, ry) plus a sub-cell lepton offset (sub_x, sub_y) to
/// get sub-cell precision without overflowing SimFixed on large maps.
///
/// Screen coords are computed via `iso_to_screen(rx, ry, z)` and cached here
/// so the render loop doesn't need to recompute every frame.
#[derive(Debug, Clone)]
pub struct Position {
    /// Isometric cell X coordinate.
    pub rx: u16,
    /// Isometric cell Y coordinate.
    pub ry: u16,
    /// Elevation level (0 = ground). Each level is 15px visual offset.
    pub z: u8,
    /// Sub-cell lepton offset X (0..256). 128 = cell center.
    /// Provides sub-cell precision for smooth movement and accurate range checks.
    pub sub_x: SimFixed,
    /// Sub-cell lepton offset Y (0..256). 128 = cell center.
    pub sub_y: SimFixed,
    /// Pre-computed screen X position (pixels, world space).
    pub screen_x: f32,
    /// Pre-computed screen Y position (pixels, world space).
    pub screen_y: f32,
}

impl Position {
    /// Shared owner for keeping cached screen coordinates in sync after any
    /// direct mutation of world position, sub-cell offset, or Z.
    pub fn refresh_screen_coords(&mut self) {
        let (sx, sy) =
            crate::util::lepton::lepton_to_screen(self.rx, self.ry, self.sub_x, self.sub_y, self.z);
        self.screen_x = sx;
        self.screen_y = sy;
    }
}

/// Facing direction (0â€“255, RA2 convention).
///
/// 0 = north, 64 = east, 128 = south, 192 = west.
/// Used for sprite/voxel rotation and movement direction.
#[derive(Debug, Clone, Copy)]
pub struct Facing(pub u8);

/// Independent turret facing direction (0–255, RA2 convention).
///
/// Only present on entities with `Turret=yes` in rules.ini (e.g., tanks, War Miner).
/// The turret rotates independently from the body: it tracks attack targets,
/// and returns to body facing when idle.
/// 0 = north, 64 = east, 128 = south, 192 = west.
#[derive(Debug, Clone, Copy)]
pub struct TurretFacing(pub u8);

/// Which player/faction owns this entity (legacy wrapper — prefer InternedId directly).
#[derive(Debug, Clone, Copy)]
pub struct Owner(pub InternedId);

/// Hit points â€” current and maximum health.
///
/// When current reaches 0, the entity is destroyed.
/// Max health comes from rules.ini Strength= value.
#[derive(Debug, Clone, Copy)]
pub struct Health {
    /// Current HP (0 = destroyed).
    pub current: u16,
    /// Maximum HP from rules.ini. Used for health bar display.
    pub max: u16,
}

/// Vision radius in grid cells used for fog/shroud reveal.
#[derive(Debug, Clone, Copy)]
pub struct Vision {
    /// Reveal/visibility radius in cells.
    pub range_cells: u16,
}

/// Marker component: this entity is rendered as a VXL voxel model.
///
/// Vehicles and aircraft use voxel models. The render loop loads the
/// corresponding VXL+HVA files and renders them via the software rasterizer.
#[derive(Debug, Clone, Copy)]
pub struct VoxelModel;

/// Marker component: this entity is rendered as a SHP 2D sprite.
///
/// Infantry and buildings use SHP sprites. Not yet wired to rendering â€”
/// will be implemented when SHP sprite batching is added.
#[derive(Debug, Clone, Copy)]
pub struct SpriteModel;

/// Which category this entity belongs to (unit, infantry, structure, aircraft).
///
/// Wraps the map::entities::EntityCategory enum so it can be used as an ECS component.
#[derive(Debug, Clone, Copy)]
pub struct Category(pub EntityCategory);

/// Infantry sub-cell position (0–4).
///
/// RA2 uses sub-cell spots 2, 3, 4 — up to 3 infantry per cell, each at a
/// different sub-position. Only meaningful for infantry entities.
#[derive(Debug, Clone, Copy)]
pub struct SubCell(pub u8);

/// Veterancy level: 0 = rookie, 100 = veteran, 200 = elite.
///
/// Affects unit stats (damage, armor, speed bonuses) and visual indicators.
#[derive(Debug, Clone, Copy)]
pub struct Veterancy(pub u16);

/// Marker component: this building is being repaired (spending credits to heal).
///
/// Added by the ToggleRepair command. Removed when health is full or credits run out.
#[derive(Debug, Clone, Copy)]
pub struct Repairing;

/// Building construction animation state — plays the "make" SHP sequence.
///
/// When a building is first placed (from sidebar production or MCV deploy),
/// it starts with this component. Each sim tick advances `elapsed_ticks`.
/// When `elapsed_ticks >= total_ticks`, the component is removed and the
/// building switches to its normal idle appearance.
///
/// The render side maps progress (elapsed/total) to make SHP frame indices.
#[derive(Debug, Clone, Copy)]
pub struct BuildingUp {
    /// Sim ticks elapsed since placement.
    pub elapsed_ticks: u16,
    /// Total sim ticks for the build-up animation.
    pub total_ticks: u16,
}

/// Reverse build-up animation: building is undeploying back into a mobile unit.
///
/// Plays the make SHP animation in reverse (last frame → first frame).
/// When `elapsed_ticks >= total_ticks`, the building is despawned and the
/// mobile unit is spawned (e.g., ConYard → MCV).
///
/// The render side maps progress (elapsed/total) to make SHP frame indices
/// counting backwards from the last frame.
#[derive(Debug, Clone)]
pub struct BuildingDown {
    /// Sim ticks elapsed since undeploy was initiated.
    pub elapsed_ticks: u16,
    /// Total sim ticks for the undeploy animation.
    pub total_ticks: u16,
    /// Unit type to spawn when animation completes (e.g., "AMCV").
    pub spawn_type: InternedId,
    /// Owner of the unit to spawn.
    pub spawn_owner: InternedId,
    /// Map position where the mobile unit will appear.
    pub spawn_rx: u16,
    pub spawn_ry: u16,
    /// Height level for the spawned unit.
    pub spawn_z: u8,
    /// Whether the entity was selected (transfer selection to spawned unit).
    pub was_selected: bool,
}

/// Marker component: this entity is currently selected by the player.
///
/// Added/removed dynamically via `world.insert_one()` / `world.remove_one()`.
/// The render loop queries for `Selected` to draw selection indicators.
#[derive(Debug, Clone, Copy)]
pub struct Selected;

/// Movement path target â€” entity is moving along a computed A* path.
///
/// Attached by `issue_move_command()` when a unit is ordered to move.
/// The movement system (`tick_movement`) advances the entity along the path
/// each tick. Removed automatically when the entity reaches its destination.
#[derive(Debug, Clone)]
pub struct MovementTarget {
    /// Sequence of (rx, ry) cells from current position to goal (inclusive).
    pub path: Vec<(u16, u16)>,
    /// Spatial layer for each path step. Matches `path.len()`.
    pub path_layers: Vec<MovementLayer>,
    /// Index of the next cell to move toward in the path.
    /// Starts at 1 (index 0 is the current position).
    pub next_index: usize,
    /// Maximum movement speed in leptons per second (from rules.ini Speed= value).
    /// 256 leptons = 1 cell. Fixed-point for deterministic multiplayer.
    pub speed: SimFixed,
    /// Actual speed this tick — ramps from 0 toward `speed` via acceleration,
    /// and brakes down near the destination. If no ramping data is set (accel=0),
    /// the movement system falls back to using `speed` directly.
    pub current_speed: SimFixed,
    /// Fraction of max speed gained per tick during acceleration (AccelerationFactor=).
    pub accel_factor: SimFixed,
    /// Fraction of max speed lost per tick during braking (DeaccelerationFactor=).
    pub decel_factor: SimFixed,
    /// Lepton distance from destination at which braking begins (SlowdownDistance=).
    pub slowdown_distance: SimFixed,
    /// Direction vector toward next cell in leptons: `dx_cells * 256`, `dy_cells * 256`.
    /// Recomputed when `next_index` advances. Cardinal = (±256, 0) or (0, ±256),
    /// diagonal = (±256, ±256).
    pub move_dir_x: SimFixed,
    /// Y component of the direction vector (see `move_dir_x`).
    pub move_dir_y: SimFixed,
    /// Lepton distance from current cell center to next cell center.
    /// 256 for cardinal moves, ~362 for diagonal. Used to normalize advancement.
    pub move_dir_len: SimFixed,
    /// Movement delay timer — ticks remaining before next Find_Path is allowed.
    /// Set after every pathfinding call. Duration from PathDelay= in [General].
    /// Original engine: CDTimerClass at FootClass+0x640, guards Process_Movement Phase 2.
    pub movement_delay: u16,
    /// Blocked delay timer — ticks remaining in the blocked wait period.
    /// Set when blocked by a moving friendly (Can_Enter_Cell code 2).
    /// Duration from BlockagePathDelay= in [General].
    /// When this timer expires, urgency escalates to 2 (aggressive scatter).
    /// Original engine: CDTimerClass at FootClass+0x668.
    pub blocked_delay: u16,
    /// Whether the unit is currently path-blocked by a friendly mover.
    /// Set on first code-2 block, cleared when the block resolves.
    /// Prevents re-starting the blocked_delay timer while still blocked.
    /// Original engine: FootClass+0x6B7 path_blocked_flag.
    pub path_blocked: bool,
    /// Retry counter — decremented on each failed Find_Path. When it reaches 0
    /// the unit gives up and stops. Reset to PATH_STUCK_INIT on new move orders.
    /// Original engine: FootClass+0x64C path_stuck_counter (init=10).
    pub path_stuck_counter: u8,
    /// Ultimate destination — preserved across 24-step segment replanning.
    /// When a path segment is exhausted before reaching this goal, the movement
    /// system auto-replans from the current position. `None` for short paths
    /// or test-only movement targets that don't need segmented replanning.
    pub final_goal: Option<(u16, u16)>,
    /// Group ID for formation speed sync. When set, the movement system caps
    /// this unit's speed to the slowest member of the group (deep_113 line 451).
    pub group_id: Option<u32>,
    /// When true, the movement tick skips terrain cost passability checks
    /// for cell entry. Used by `issue_direct_move` to let harvesters walk
    /// onto ore cells that are terrain-blocked for their SpeedType.
    pub ignore_terrain_cost: bool,
}

/// Default acceleration/deceleration values — zero means no ramping,
/// movement system falls back to using `speed` directly.
impl Default for MovementTarget {
    fn default() -> Self {
        Self {
            path: Vec::new(),
            path_layers: Vec::new(),
            next_index: 0,
            speed: SIM_ZERO,
            current_speed: SIM_ZERO,
            accel_factor: SIM_ZERO,
            decel_factor: SIM_ZERO,
            slowdown_distance: SIM_ZERO,
            move_dir_x: SIM_ZERO,
            move_dir_y: SIM_ZERO,
            move_dir_len: SIM_ZERO,
            movement_delay: 0,
            blocked_delay: 0,
            path_blocked: false,
            path_stuck_counter: 10,
            final_goal: None,
            group_id: None,
            ignore_terrain_cost: false,
        }
    }
}

impl MovementTarget {
    pub fn layer_at(&self, index: usize) -> MovementLayer {
        debug_assert_eq!(
            self.path.len(),
            self.path_layers.len(),
            "path/path_layers length mismatch: {} vs {}",
            self.path.len(),
            self.path_layers.len()
        );
        self.path_layers
            .get(index)
            .copied()
            .unwrap_or(MovementLayer::Ground)
    }
}

/// Marker component: this entity currently occupies a bridge deck cell.
#[derive(Debug, Clone, Copy)]
pub struct BridgeOccupancy {
    pub deck_level: u8,
}

/// Persistent high-level order state that survives transient combat/movement components.
///
/// This keeps intent like attack-move or guard alive while systems temporarily
/// add/remove `MovementTarget` and `AttackTarget`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderIntent {
    /// Move toward a destination but auto-acquire enemies along the way.
    AttackMove { goal_rx: u16, goal_ry: u16 },
    /// Hold position and auto-acquire nearby enemies.
    Guard { anchor_rx: u16, anchor_ry: u16 },
    /// Transport is actively unloading passengers one per tick.
    Unloading,
}

/// Which part of a multi-part voxel model an entity/atlas entry represents.
///
/// Non-turret units use `Composite` (body+turret+barrel baked together).
/// Turret units store Body/Turret/Barrel separately so the turret can
/// be drawn at a different facing than the body at render time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VxlLayer {
    /// All parts composited into one sprite (for units without independent turret).
    Composite,
    /// Body only (hull/chassis).
    Body,
    /// Turret only ({IMAGE}TUR.VXL).
    Turret,
    /// Barrel only ({IMAGE}BARL.VXL).
    Barrel,
}

/// Per-entity voxel HVA animation state.
///
/// Attached to voxel entities that cycle through HVA animation frames at runtime.
/// Used for harvesting miners (arm/turret animation), and potentially other voxel
/// units with multi-frame HVA files. The render loop reads `frame` to select
/// the correct pre-rendered atlas sprite.
#[derive(Debug, Clone, Copy)]
pub struct VoxelAnimation {
    /// Current HVA frame index (0-based).
    pub frame: u32,
    /// Total number of HVA animation frames.
    pub frame_count: u32,
    /// Milliseconds accumulated since last frame advance.
    pub elapsed_ms: u32,
    /// Milliseconds per frame (animation speed). 0 = no auto-advance.
    pub tick_ms: u32,
    /// Whether the animation is currently playing (cycling frames).
    pub playing: bool,
}

impl VoxelAnimation {
    /// Create a new VoxelAnimation in stopped state.
    pub fn new(frame_count: u32, tick_ms: u32) -> Self {
        Self {
            frame: 0,
            frame_count,
            elapsed_ms: 0,
            tick_ms,
            playing: false,
        }
    }
}

/// Harvest overlay animation state for the oregath.shp ore-gathering sprite.
///
/// Attached to harvester entities (HARV, CMIN). Shows the visual "sucking up ore"
/// animation as an SHP overlay on top of the VXL body when actively harvesting.
/// Uses the effect palette (anim.pal), independent of house colors.
#[derive(Debug, Clone, Copy)]
pub struct HarvestOverlay {
    /// Current animation frame (0..14, 15 frames per facing direction).
    pub frame: u16,
    /// Whether the overlay is currently visible and animating.
    pub visible: bool,
    /// Milliseconds accumulated since last frame advance.
    pub elapsed_ms: u32,
}

/// Visual damage state derived from health ratio.
///
/// Pure helper — not an ECS component. Computed on the fly from `Health`
/// by the render system to decide whether to show smoke/fire overlays.
/// - Green: > 50% HP (healthy)
/// - Yellow: 25–50% HP (damaged, shows smoke)
/// - Red: < 25% HP (heavily damaged, shows fire)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DamageState {
    Green,
    Yellow,
    Red,
}

impl DamageState {
    /// Compute the damage state from current and max health.
    /// Uses pure integer math for deterministic results across platforms.
    /// Green: ratio > 50%, Yellow: ratio > 25%, Red: ratio <= 25%.
    pub fn from_health(current: u16, max: u16) -> Self {
        if max == 0 {
            return Self::Green;
        }
        // current/max > 0.5  ↔  current * 2 > max (no overflow: u16 * 2 fits u32)
        let c = current as u32;
        let m = max as u32;
        if c * 2 > m {
            Self::Green
        } else if c * 4 > m {
            Self::Yellow
        } else {
            Self::Red
        }
    }
}

/// Tracks the last entity that dealt damage to this entity.
///
/// Used for retaliation: when an idle unit takes damage, it automatically
/// attacks the source. Still subject to Verses gates (0%/1% block retaliation).
#[derive(Debug, Clone, Copy)]
pub struct LastAttacker {
    /// Stable entity ID of the attacker that dealt the most recent damage.
    pub attacker: u64,
}

/// Per-overlay one-shot animation state for a building (e.g., ConYard crane).
///
/// Each active one-shot anim overlay gets its own entry tracking frame progress.
/// Driven by art.ini LoopStart/LoopEnd/LoopCount/Rate properties from the
/// anim's own section (e.g., [GACNST_B]).
#[derive(Debug, Clone)]
pub struct AnimOverlayState {
    /// Animation type interned ID (uppercase), e.g., "GACNST_B".
    pub anim_type: InternedId,
    /// Current frame index in the animation.
    pub frame: u16,
    /// First frame of the loop range (from art.ini LoopStart=).
    pub loop_start: u16,
    /// Last frame of the loop range, exclusive (from art.ini LoopEnd=).
    pub loop_end: u16,
    /// Milliseconds per frame (from art.ini Rate=, default 200).
    pub rate_ms: u32,
    /// Milliseconds accumulated since last frame advance.
    pub elapsed_ms: u32,
    /// true = animation completed its one-shot playback.
    pub finished: bool,
}

/// Active one-shot building animation overlays (field on GameEntity).
///
/// Populated when a one-shot anim is triggered (e.g., placing a building triggers
/// the ConYard crane). Cleared when the entity is despawned via EntityStore.remove().
/// Infinite-loop anims (LoopCount=-1) are NOT stored here — they use a global timer.
#[derive(Debug, Clone)]
pub struct BuildingAnimOverlays {
    pub anims: Vec<AnimOverlayState>,
}

/// Persistent fire/smoke overlays on damaged buildings (health < ConditionYellow).
///
/// Separate from the 21-slot BuildingAnimOverlays system. Each fire loops
/// independently with a random starting frame for visual variety.
/// Created when health drops below ConditionYellow, removed when repaired above.
#[derive(Debug, Clone)]
pub struct DamageFireOverlays {
    pub fires: Vec<DamageFireAnim>,
}

/// A single looping fire/smoke animation attached to a damaged building.
#[derive(Debug, Clone)]
pub struct DamageFireAnim {
    /// SHP type interned ID (e.g., "FIRE01").
    pub shp_name: InternedId,
    /// Pixel X offset from building screen origin.
    pub pixel_x: i32,
    /// Pixel Y offset from building screen origin.
    pub pixel_y: i32,
    /// Current animation frame.
    pub frame: u16,
    /// Total frames in the SHP (loops at this boundary).
    pub total_frames: u16,
    /// Milliseconds per frame (from art.ini Rate=).
    pub rate_ms: u32,
    /// Accumulated ms since last frame advance.
    pub elapsed_ms: u32,
}

/// A one-shot muzzle flash animation at a garrison building's fire port.
///
/// Spawned when a garrisoned building fires (one per shot). Positioned at the
/// building's screen origin + MuzzleFlash pixel offset from art.ini. Auto-removed
/// when the animation completes (not looping, unlike DamageFireAnim).
#[derive(Debug, Clone)]
pub struct GarrisonMuzzleFlash {
    /// Stable ID of the building entity (to look up screen position each frame).
    pub building_id: u64,
    /// SHP type interned ID (e.g., "UCFLASH").
    pub shp_name: InternedId,
    /// Pixel X offset from building screen origin (from art.ini MuzzleFlashN).
    pub pixel_x: i32,
    /// Pixel Y offset from building screen origin (from art.ini MuzzleFlashN).
    pub pixel_y: i32,
    /// Current animation frame.
    pub frame: u16,
    /// Total frames in the SHP (one-shot: removed when frame >= total_frames).
    pub total_frames: u16,
    /// Milliseconds per frame (~67ms = 15fps, standard for RA2 muzzle flashes).
    pub rate_ms: u32,
    /// Accumulated ms since last frame advance.
    pub elapsed_ms: u32,
}

/// A temporary one-shot SHP animation playing at a fixed world position.
///
/// Used for visual effects not attached to any entity: chrono warp sparkles,
/// explosions, weapon impacts, ion storm bolts, etc. The render loop draws
/// these as flat ground-level sprites. They auto-remove when finished.
#[derive(Debug, Clone)]
pub struct WorldEffect {
    /// SHP type interned ID (uppercase), e.g., "WARPOUT", "WARPIN", "FBALL1".
    pub shp_name: InternedId,
    /// World cell where the effect plays.
    pub rx: u16,
    pub ry: u16,
    /// Height level for depth sorting.
    pub z: u8,
    /// Current frame index.
    pub frame: u16,
    /// Total frame count in the SHP.
    pub total_frames: u16,
    /// Milliseconds per frame (from art.ini Rate=, or hardcoded default).
    pub rate_ms: u32,
    /// Milliseconds accumulated since last frame advance.
    pub elapsed_ms: u32,
    /// Whether the effect renders with alpha/translucency (art.ini Translucent=yes).
    pub translucent: bool,
    /// Optional start delay in milliseconds. Counts down before animation begins.
    /// Used for staggering multiple explosions on bridge destruction.
    pub delay_ms: u32,
}

impl WorldEffect {
    /// Advance the animation by `dt_ms` milliseconds. Returns true when finished.
    pub fn tick(&mut self, dt_ms: u32) -> bool {
        if self.delay_ms > 0 {
            self.delay_ms = self.delay_ms.saturating_sub(dt_ms);
            return false;
        }
        self.elapsed_ms += dt_ms;
        while self.elapsed_ms >= self.rate_ms && self.frame < self.total_frames {
            self.elapsed_ms -= self.rate_ms;
            self.frame += 1;
        }
        self.frame >= self.total_frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::entities::EntityCategory;

    #[test]
    fn test_position_creation() {
        let pos: Position = Position {
            rx: 30,
            ry: 40,
            z: 0,
            sub_x: crate::util::lepton::CELL_CENTER_LEPTON,
            sub_y: crate::util::lepton::CELL_CENTER_LEPTON,
            screen_x: -300.0,
            screen_y: 1050.0,
        };
        assert_eq!(pos.rx, 30);
        assert_eq!(pos.ry, 40);
    }

    #[test]
    fn test_types_are_send_sync() {
        // GameEntity fields must be Send + Sync for future multithreaded sim ticks.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Position>();
        assert_send_sync::<Facing>();
        assert_send_sync::<TurretFacing>();
        assert_send_sync::<Owner>();
        assert_send_sync::<Health>();
        assert_send_sync::<Vision>();
        assert_send_sync::<VoxelModel>();
        assert_send_sync::<SpriteModel>();
        assert_send_sync::<Category>();
        assert_send_sync::<SubCell>();
        assert_send_sync::<Veterancy>();
        assert_send_sync::<MovementTarget>();
        assert_send_sync::<BridgeOccupancy>();
        assert_send_sync::<OrderIntent>();
        assert_send_sync::<BuildingUp>();
        assert_send_sync::<Selected>();
        assert_send_sync::<LastAttacker>();
        assert_send_sync::<VoxelAnimation>();
        assert_send_sync::<HarvestOverlay>();
        assert_send_sync::<AnimOverlayState>();
        assert_send_sync::<BuildingAnimOverlays>();
        assert_send_sync::<crate::sim::movement::locomotor::LocomotorState>();
    }

    #[test]
    fn test_damage_state_thresholds() {
        assert_eq!(DamageState::from_health(100, 100), DamageState::Green);
        assert_eq!(DamageState::from_health(51, 100), DamageState::Green);
        assert_eq!(DamageState::from_health(50, 100), DamageState::Yellow);
        assert_eq!(DamageState::from_health(26, 100), DamageState::Yellow);
        assert_eq!(DamageState::from_health(25, 100), DamageState::Red);
        assert_eq!(DamageState::from_health(1, 100), DamageState::Red);
        assert_eq!(DamageState::from_health(0, 100), DamageState::Red);
        assert_eq!(DamageState::from_health(0, 0), DamageState::Green);
    }

    #[test]
    fn test_category_wraps_entity_category() {
        let cat: Category = Category(EntityCategory::Unit);
        assert_eq!(cat.0, EntityCategory::Unit);
    }

    #[test]
    fn test_world_effect_tick_advances_and_finishes() {
        use crate::sim::intern::test_intern;
        let mut fx = WorldEffect {
            shp_name: test_intern("WARPOUT"),
            rx: 10,
            ry: 10,
            z: 0,
            frame: 0,
            total_frames: 3,
            rate_ms: 100,
            elapsed_ms: 0,
            translucent: true,
            delay_ms: 0,
        };
        // Not finished yet.
        assert!(!fx.tick(50));
        assert_eq!(fx.frame, 0);
        // Advance past first frame boundary.
        assert!(!fx.tick(60));
        assert_eq!(fx.frame, 1);
        // Two more frames in one big dt.
        assert!(fx.tick(250));
        assert_eq!(fx.frame, 3);
    }
}
