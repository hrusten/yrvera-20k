//! Runtime locomotor state — ECS component for unit movement behavior.
//!
//! Each movable entity gets a `LocomotorState` component at spawn time, created
//! from the unit's `ObjectType` data. This component controls HOW the unit moves:
//! speed multipliers, movement layer (ground/air/underground), and phase tracking.
//!
//! `LocomotorState` works alongside `MovementTarget` (which holds the A* path).
//! The locomotor controls the interpretation of the path; `MovementTarget` holds
//! the raw path data. Entities without `LocomotorState` use legacy movement
//! (backward compatible).
//!
//! ## Phase 1 scope
//! Ground movers (Drive, Walk, Hover, Mech, Ship) are fully functional.
//!
//! ## Phase 2 scope
//! Air movers (Fly, Jumpjet) have altitude state machines.
//! Fly units move in straight lines (no A*), ascend/descend between ground and
//! cruise altitude. Jumpjet units hover at JumpjetHeight with wobble.
//! Special locomotors (Teleport, Tunnel, Rocket, DropPod) are stubbed for later.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on rules/ (LocomotorKind, ObjectType).
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::rules::jumpjet_params::JumpjetParams;
use crate::rules::locomotor_type::{LocomotorKind, MovementZone, SpeedType};
use crate::rules::object_type::ObjectType;
use crate::util::fixed_math::{SIM_ZERO, SimFixed, sim_from_f32};

/// Which spatial layer the unit currently occupies.
///
/// Affects occupancy checks, rendering, and targeting. Ground units block
/// ground cells; air units occupy the air layer and can fly over obstacles.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum MovementLayer {
    /// Standard ground surface.
    Ground,
    /// Elevated bridge deck above the ground/water layer.
    Bridge,
    /// Airborne (aircraft, jumpjets at altitude).
    Air,
    /// Burrowed underground (tunnel units).
    Underground,
}

/// Phase within a ground mover's movement cycle.
///
/// 7-state machine matching the original engine's WalkLocomotionClass (+0x50).
/// States 0-6 govern speed ramping, cell transitions, and obstacle handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GroundMovePhase {
    /// State 0: No movement order. Unit is stationary at a valid cell position.
    /// Entry: set when speed reaches 0 and unit completes all movement at cell center.
    Idle,
    /// State 1: Post-paradrop landing. Speed set to 1.0, velocity zeroed.
    /// Transitions to Accelerating when movement begins.
    Landed,
    /// State 2: Ramping up speed toward cruise. Entered when a new cell-to-cell
    /// step begins — facing is updated and speed starts increasing.
    Accelerating,
    /// State 3: At cruise speed, following path. Entered from Accelerating when
    /// unit reaches cruise speed, or from CellEntry after successful transition.
    Cruising,
    /// State 4: Core path-following tick with distance-based speed zones.
    /// Handles approach deceleration and arrival detection (< 20 leptons).
    PathFollow,
    /// State 5: Cell-to-cell transition step. Handles obstacle detection,
    /// crush logic, passability checks, and bridge-specific behaviors.
    CellEntry,
    /// State 6: Decelerating to halt. Target speed zeroed, deceleration in
    /// UpdatePosition brings speed to 0, then transitions to Idle.
    Stopping,
    /// Blocked by another entity or impassable terrain. Waiting for repath.
    /// Not a state in the original engine's +0x50 field, but tracked here
    /// for diagnostics and UI feedback.
    Blocked,
}

/// Phase within an air mover's flight cycle.
///
/// Used by Fly and Jumpjet locomotors to track altitude state transitions.
/// Fly units cycle through TakingOff → Cruising → Descending → Landed.
/// Jumpjet units ascend to hover altitude and stay in Hovering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AirMovePhase {
    /// On the ground, not yet airborne.
    Landed,
    /// Ascending from ground to cruise/hover altitude.
    Ascending,
    /// At cruise altitude, moving toward destination (Fly locomotor).
    Cruising,
    /// Hovering at fixed altitude (Jumpjet locomotor).
    Hovering,
    /// Descending from cruise altitude back to ground.
    Descending,
}

/// Default cruise altitude for Fly locomotor aircraft (in leptons).
/// RA2 aircraft fly at a fixed altitude — this value produces a visible
/// vertical offset (~3 cells worth of height).
const FLY_CRUISE_ALTITUDE: SimFixed = SimFixed::lit("600");

/// Rate at which Fly aircraft ascend/descend (leptons per second).
const FLY_CLIMB_RATE: SimFixed = SimFixed::lit("300");

/// Hover speed multiplier — hover units are ~35% slower than Drive for the
/// same nominal Speed value (documented in ModEnc and the locomotor report).
const HOVER_SPEED_MULTIPLIER: SimFixed = SimFixed::lit("0.65");

/// Runtime locomotor state attached to each movable ECS entity.
///
/// Created from `ObjectType` at spawn time. The movement system reads this
/// to decide how to process the entity's `MovementTarget` each tick.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LocomotorState {
    /// Which locomotor class this unit uses.
    pub kind: LocomotorKind,
    /// Which spatial layer the unit currently occupies.
    pub layer: MovementLayer,
    /// Current movement phase (for ground movers).
    pub phase: GroundMovePhase,
    /// Current air movement phase (for Fly/Jumpjet locomotors).
    pub air_phase: AirMovePhase,
    /// Speed multiplier applied on top of ObjectType.speed.
    /// 1.0 for most units, 0.65 for Hover, etc.
    pub speed_multiplier: SimFixed,
    /// Mission-controlled speed fraction (0.0–1.0). Set by aircraft missions
    /// for dive bombing deceleration and speed tiers. air_movement multiplies
    /// the base speed by this fraction. Default 1.0 (full speed).
    pub speed_fraction: SimFixed,
    /// Current altitude in leptons (0 = on the ground).
    /// Fly units cruise at FLY_CRUISE_ALTITUDE; Jumpjets hover at JumpjetHeight.
    pub altitude: SimFixed,
    /// Target altitude — what the unit is ascending/descending toward.
    pub target_altitude: SimFixed,
    /// Climb rate in leptons per second.
    pub climb_rate: SimFixed,
    /// Cached jumpjet flight speed (only for Jumpjet locomotor).
    pub jumpjet_speed: SimFixed,
    /// Jumpjet wobble amplitude (0.0 if no wobble or non-jumpjet).
    /// KEPT as f32 — render-only visual wobble.
    #[serde(skip, default)]
    pub jumpjet_wobbles: f32,
    /// Jumpjet acceleration rate (JumpjetAccel). Deceleration = accel * 1.5.
    pub jumpjet_accel: SimFixed,
    /// Current speed during jumpjet flight (ramps via accel/decel).
    pub jumpjet_current_speed: SimFixed,
    /// Max lateral deviation in leptons during hover wobble (JumpjetDeviation).
    pub jumpjet_deviation: i32,
    /// Combined crash descent speed: climb + crash (leptons/sec, scaled).
    pub jumpjet_crash_speed: SimFixed,
    /// Facing change rate per tick while airborne (JumpjetTurnRate).
    pub jumpjet_turn_rate: i32,
    /// Stay airborne after reaching destination (BalloonHover=yes).
    pub balloon_hover: bool,
    /// Can attack while hovering in place (HoverAttack=yes).
    pub hover_attack: bool,
    /// Which terrain type this unit traverses (from rules.ini SpeedType=).
    /// Used to select the correct TerrainCostGrid for cost-aware pathfinding.
    pub speed_type: SpeedType,
    /// Pathfinder movement zone — determines crush capability and special routing.
    /// Cached from ObjectType at spawn to avoid per-tick RuleSet lookups.
    pub movement_zone: MovementZone,
    /// Body rotation speed — ROT value from rules.ini (degrees/frame at 15fps).
    /// Used for gradual hull turning before movement. 0 = instant turn.
    /// Infantry always turn instantly regardless of this value (RA2 behavior).
    pub rot: i32,
    /// Temporary locomotor override — saves the base locomotor while a
    /// temporary controller (Teleport, DropPod) is active. `None` means
    /// the unit is using its normal base locomotor.
    pub override_state: Option<OverrideLocomotor>,
    /// Air movement progress in cells (0.0 → 1.0 per cell step).
    /// Air movement uses cell-based progress separately from the lepton
    /// advancement used by ground movement. This field is only meaningful
    /// for air-layer entities during horizontal flight.
    pub air_progress: SimFixed,
    /// Infantry lateral wobble phase (radians). Sine wave applied perpendicular
    /// to facing direction during walking, creating natural visual sway/spacing.
    /// Original engine: WalkLocomotionClass +0x88 `LateralWobble` (double).
    /// Render-only (f32) — does not affect simulation determinism.
    #[serde(skip, default)]
    pub infantry_wobble_phase: f32,
    /// Within-cell walk destination for infantry. Set when a sub-cell is allocated
    /// during cell entry. The locomotor walks the infantry toward this point after
    /// the path is exhausted.
    pub subcell_dest: Option<(SimFixed, SimFixed)>,
}

impl LocomotorState {
    /// Create a LocomotorState from an ObjectType's parsed rules.ini data.
    ///
    /// Sets the correct layer, phase, speed multiplier, and altitude params
    /// based on the unit's locomotor kind.
    pub fn from_object_type(obj: &ObjectType) -> Self {
        let kind: LocomotorKind = obj.locomotor;
        let sim_one: SimFixed = SimFixed::from_num(1);

        let (layer, speed_multiplier): (MovementLayer, SimFixed) = match kind {
            // Ground family — all use Ground layer
            LocomotorKind::Drive => (MovementLayer::Ground, sim_one),
            LocomotorKind::Walk => (MovementLayer::Ground, sim_one),
            LocomotorKind::Hover => (MovementLayer::Ground, HOVER_SPEED_MULTIPLIER),
            LocomotorKind::Mech => (MovementLayer::Ground, sim_one),
            LocomotorKind::Ship => (MovementLayer::Ground, sim_one),

            // Air family — use Air layer with altitude state
            LocomotorKind::Fly => (MovementLayer::Air, sim_one),
            LocomotorKind::Jumpjet => (MovementLayer::Air, sim_one),
            LocomotorKind::Rocket => (MovementLayer::Air, sim_one),

            // Special — stubbed as ground for now
            LocomotorKind::Teleport => (MovementLayer::Ground, sim_one),
            LocomotorKind::Tunnel => (MovementLayer::Ground, sim_one),
            LocomotorKind::DropPod => (MovementLayer::Air, sim_one),
        };

        // Extract jumpjet params for altitude and wobble.
        let (target_alt, climb, jj_speed, jj_wobbles) =
            Self::air_params_from_object(kind, &obj.jumpjet_params);

        // Extract extended jumpjet fields (accel, deviation, crash, turn rate).
        let jj = obj.jumpjet_params.as_ref();
        let jj_accel: SimFixed = jj.map_or(SIM_ZERO, |p| p.accel);
        let jj_deviation: i32 = jj.map_or(0, |p| p.deviation);
        let jj_crash_speed: SimFixed =
            jj.map_or(SIM_ZERO, |p| (p.climb + p.crash) * SimFixed::from_num(15));
        let jj_turn_rate: i32 = jj.map_or(4, |p| p.turn_rate);

        Self {
            kind,
            layer,
            phase: GroundMovePhase::Idle,
            air_phase: AirMovePhase::Landed,
            speed_multiplier,
            speed_fraction: sim_one,
            altitude: SIM_ZERO,
            target_altitude: target_alt,
            climb_rate: climb,
            jumpjet_speed: jj_speed,
            jumpjet_wobbles: jj_wobbles,
            jumpjet_accel: jj_accel,
            jumpjet_current_speed: SIM_ZERO,
            jumpjet_deviation: jj_deviation,
            jumpjet_crash_speed: jj_crash_speed,
            jumpjet_turn_rate: jj_turn_rate,
            balloon_hover: obj.balloon_hover,
            hover_attack: obj.hover_attack,
            speed_type: obj.speed_type,
            movement_zone: obj.movement_zone,
            rot: obj.turret_rot,
            override_state: None,
            air_progress: SIM_ZERO,
            infantry_wobble_phase: 0.0,
            subcell_dest: None,
        }
    }

    /// Compute altitude parameters from locomotor kind and optional jumpjet params.
    /// Returns (target_altitude, climb_rate, jumpjet_speed, jumpjet_wobbles).
    /// wobbles is f32 since it's render-only.
    fn air_params_from_object(
        kind: LocomotorKind,
        jumpjet_params: &Option<JumpjetParams>,
    ) -> (SimFixed, SimFixed, SimFixed, f32) {
        match kind {
            LocomotorKind::Fly | LocomotorKind::Rocket => {
                (FLY_CRUISE_ALTITUDE, FLY_CLIMB_RATE, SIM_ZERO, 0.0)
            }
            LocomotorKind::Jumpjet => {
                let jj = jumpjet_params.as_ref();
                let height: SimFixed =
                    jj.map_or(SimFixed::from_num(500), |p| SimFixed::from_num(p.height));
                let climb: SimFixed = jj.map_or(sim_from_f32(5.0), |p| p.climb);
                let speed: SimFixed = jj.map_or(sim_from_f32(14.0), |p| p.speed);
                let wobbles: f32 = jj.filter(|p| !p.no_wobbles).map_or(0.0, |p| p.wobbles);
                // Jumpjet climb rate scaled to leptons/second (original is per-tick at 15Hz).
                (height, climb * SimFixed::from_num(15), speed, wobbles)
            }
            _ => (SIM_ZERO, SIM_ZERO, SIM_ZERO, 0.0),
        }
    }

    /// Whether this locomotor is in the ground family (Drive/Walk/Hover/Mech/Ship).
    pub fn is_ground_mover(&self) -> bool {
        matches!(
            self.kind,
            LocomotorKind::Drive
                | LocomotorKind::Walk
                | LocomotorKind::Hover
                | LocomotorKind::Mech
                | LocomotorKind::Ship
        )
    }

    /// Whether this locomotor is an air mover (Fly/Jumpjet/Rocket).
    pub fn is_air_mover(&self) -> bool {
        matches!(
            self.kind,
            LocomotorKind::Fly | LocomotorKind::Jumpjet | LocomotorKind::Rocket
        )
    }

    /// Whether this unit is currently airborne (altitude > 0).
    pub fn is_airborne(&self) -> bool {
        self.altitude > SIM_ZERO
    }

    /// Whether this locomotor currently has a temporary override active.
    pub fn is_overridden(&self) -> bool {
        self.override_state.is_some()
    }

    /// Begin a temporary locomotor override (e.g., Teleport or DropPod).
    ///
    /// Saves the current state so it can be restored when the override ends.
    /// Sets `kind` to the override's locomotor kind and adjusts the layer.
    pub fn begin_override(&mut self, override_kind: OverrideKind) {
        if self.override_state.is_some() {
            log::warn!("begin_override called while already overridden — replacing saved state");
        }
        let saved = Box::new(self.clone());
        let (new_kind, new_layer) = match override_kind {
            OverrideKind::Teleport => (LocomotorKind::Teleport, MovementLayer::Ground),
            OverrideKind::DropPod => (LocomotorKind::DropPod, MovementLayer::Air),
        };
        self.override_state = Some(OverrideLocomotor {
            saved,
            override_kind,
        });
        self.kind = new_kind;
        self.layer = new_layer;
        self.phase = GroundMovePhase::Idle;
        self.air_phase = AirMovePhase::Landed;
    }

    /// End a temporary override — restore the saved locomotor state.
    ///
    /// Returns the `OverrideKind` that was active, or `None` if no override was set.
    pub fn end_override(&mut self) -> Option<OverrideKind> {
        let Some(overridden) = self.override_state.take() else {
            log::warn!("end_override called but no override is active");
            return None;
        };
        let kind = overridden.override_kind;
        let saved = *overridden.saved;
        // Restore the entire base locomotor state.
        self.kind = saved.kind;
        self.layer = saved.layer;
        self.phase = GroundMovePhase::Idle;
        self.air_phase = saved.air_phase;
        self.speed_multiplier = saved.speed_multiplier;
        self.altitude = saved.altitude;
        self.target_altitude = saved.target_altitude;
        self.climb_rate = saved.climb_rate;
        self.jumpjet_speed = saved.jumpjet_speed;
        self.jumpjet_wobbles = saved.jumpjet_wobbles;
        self.jumpjet_accel = saved.jumpjet_accel;
        self.jumpjet_current_speed = saved.jumpjet_current_speed;
        self.jumpjet_deviation = saved.jumpjet_deviation;
        self.jumpjet_crash_speed = saved.jumpjet_crash_speed;
        self.jumpjet_turn_rate = saved.jumpjet_turn_rate;
        self.balloon_hover = saved.balloon_hover;
        self.hover_attack = saved.hover_attack;
        self.speed_type = saved.speed_type;
        // override_state is already None from take().
        Some(kind)
    }
}

/// Which kind of temporary locomotor override is active.
///
/// Used by the piggyback mechanism — some locomotors (Teleport, DropPod) act
/// as temporary overlays on a unit's base locomotor and restore it when done.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OverrideKind {
    /// Chrono teleport override — restores base locomotor after warp-in.
    Teleport,
    /// Drop pod entry — restores base locomotor after landing.
    DropPod,
}

/// Saved base locomotor state for temporary override restoration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OverrideLocomotor {
    /// The saved base locomotor state to restore when the override ends.
    pub saved: Box<LocomotorState>,
    /// What kind of override is active.
    pub override_kind: OverrideKind,
}

#[cfg(test)]
#[path = "locomotor_tests.rs"]
mod locomotor_tests;
