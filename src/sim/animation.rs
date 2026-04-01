//! Sprite animation system — tracks per-entity animation state and frame timing.
//!
//! Manages animation sequences (stand, walk, attack, die) for SHP sprite entities.
//! Each entity with an `Animation` component has a current sequence, frame index,
//! and timing accumulator. The `tick_animations()` function advances frames and
//! handles auto-transitions (e.g., stand ↔ walk based on MovementTarget).
//!
//! ## SHP frame layout
//! Infantry SHP files pack multiple sequences contiguously:
//! - Stand: 1 frame × 8 facings = frames 0–7
//! - Walk: 6 frames × 8 facings = frames 8–55
//! - Idle1: 15 frames (non-directional) = frames 56–70
//! - Die1: 15 frames (non-directional) = frames 86–100
//!
//! For directional sequences (facings > 1):
//!   `shp_frame = start + facing_index * frame_count + frame_within_sequence`
//! For non-directional sequences:
//!   `shp_frame = start + frame_within_sequence`
//!
//! ## Auto-transitions
//! - Entity gains MovementTarget → switch to Walk
//! - Entity loses MovementTarget → switch back to Stand
//! - Attack sequence finishes → switch to Stand (TransitionTo)
//! - Die sequence finishes → hold last frame (HoldLast)
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/components (MovementTarget, TypeRef).
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use std::collections::BTreeMap;

/// Standard number of facing directions for infantry animations.
const INFANTRY_FACINGS: u8 = 8;

/// Default milliseconds per frame for standing pose.
const DEFAULT_STAND_TICK_MS: u32 = 200;

/// Default milliseconds per frame for walk cycles.
const DEFAULT_WALK_TICK_MS: u32 = 100;

/// Default milliseconds per frame for idle fidget animations.
const DEFAULT_IDLE_TICK_MS: u32 = 120;

/// Default milliseconds per frame for death animations.
const DEFAULT_DIE_TICK_MS: u32 = 80;

/// Named animation sequence types.
///
/// Each corresponds to a range of frames in the SHP file, defined
/// by a `SequenceDef`. An entity plays one sequence at a time.
///
/// Maps to art.ini sequence keys: Ready/Guard → Stand, FireUp → Attack, etc.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum SequenceKind {
    /// Standing alert pose. Default state for idle entities (Ready/Guard in INI).
    Stand,
    /// Walking movement cycle. Active while entity has a MovementTarget.
    Walk,
    /// Standing still while prone (Prone= in INI).
    Prone,
    /// Moving while prone (Crawl= in INI).
    Crawl,
    /// Firing primary weapon while standing (FireUp= in INI). Transitions to Stand.
    Attack,
    /// Firing primary weapon while prone (FireProne= in INI). Transitions to Stand.
    FireProne,
    /// Transition from standing to prone (Down= in INI). Transitions to Prone.
    Down,
    /// Transition from prone to standing (Up= in INI). Transitions to Stand.
    Up,
    /// Random fidget animation while idle (Idle1= in INI).
    Idle1,
    /// Second idle fidget variant (Idle2= in INI).
    Idle2,
    /// Death animation variant 1. Plays once, holds last frame.
    Die1,
    /// Death animation variant 2. Plays once, holds last frame.
    Die2,
    /// Death animation variant 3. Plays once, holds last frame.
    Die3,
    /// Death animation variant 4. Plays once, holds last frame.
    Die4,
    /// Death animation variant 5. Plays once, holds last frame.
    Die5,
    /// Victory/celebration animation (Cheer= in INI).
    Cheer,
    /// Parachute landing animation (Paradrop= in INI).
    Paradrop,
    /// Panicked running (Panic= in INI). Uses Walk-like timing.
    Panic,
    /// Transition from standing to deployed stance (Deploy= in INI).
    Deploy,
    /// Transition from deployed back to standing (Undeploy= in INI).
    Undeploy,
    /// Standing in deployed stance (Deployed= in INI, e.g., GI sandbags).
    Deployed,
    /// Firing while deployed (DeployedFire= in INI).
    DeployedFire,
    /// Idle fidget while deployed (DeployedIdle= in INI).
    DeployedIdle,
    /// Firing secondary weapon while standing (SecondaryFire= in INI, YR only).
    SecondaryFire,
    /// Firing secondary weapon while prone (SecondaryProne= in INI, YR only).
    SecondaryProne,
    /// Swimming movement cycle (Swim= in INI, e.g., Tanya in water).
    Swim,
    /// Flying movement cycle (Fly= in INI, e.g., Rocketeer).
    Fly,
    /// Firing while flying (FireFly= in INI).
    FireFly,
    /// Hovering in place (Hover= in INI).
    Hover,
    /// Treading water / ground cycle (Tread= in INI).
    Tread,
    /// Firing while swimming (WetAttack= in INI).
    WetAttack,
    /// Idle fidget while swimming variant 1 (WetIdle1= in INI).
    WetIdle1,
    /// Idle fidget while swimming variant 2 (WetIdle2= in INI).
    WetIdle2,
}

/// How a sequence behaves when it reaches its last frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LoopMode {
    /// Restart from frame 0 when reaching the end (walk, stand).
    Loop,
    /// Play once and freeze on the last frame (death animations).
    HoldLast,
    /// Play once then switch to a different sequence (attack → stand).
    TransitionTo(SequenceKind),
}

/// Definition of one animation sequence within an SHP file.
///
/// Describes the frame range, timing, and looping behavior for a named
/// sequence. Multiple `SequenceDef`s grouped into a `SequenceSet` define
/// all animations available for one object type.
///
/// ## Frame index formula
/// For directional sequences:
///   `start_frame + facing_index * facing_multiplier + frame_within_sequence`
/// For non-directional (facings == 1):
///   `start_frame + frame_within_sequence`
///
/// ## Facing convention
/// RA2's DirStruct byte (0–255) is screen-relative clockwise (0=N, 64=E,
/// 128=S, 192=W). SHP frames are laid out counter-clockwise (0=N, 1=NW,
/// 2=W...). `resolve_shp_frame` negates the quantized index to convert.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SequenceDef {
    /// First SHP frame index for this sequence.
    pub start_frame: u16,
    /// Number of animation frames per facing direction.
    pub frame_count: u16,
    /// Number of facing directions (1 = non-directional, 8 = infantry standard).
    pub facings: u8,
    /// Frame stride between facings — the offset applied per facing increment.
    /// From art.ini's 3rd sequence field. Typically equals `frame_count` for
    /// contiguous packing. If 0, the animation is facing-independent.
    pub facing_multiplier: u16,
    /// Duration of each frame in milliseconds. Lower = faster animation.
    pub tick_ms: u32,
    /// Behavior when the sequence reaches its final frame.
    pub loop_mode: LoopMode,
    /// If true, SHP facings are laid out clockwise (0=N, 1=NE, 2=E...) as used
    /// by SHP vehicles. If false (default), facings are counter-clockwise
    /// (0=N, 1=NW, 2=W...) as used by infantry.
    pub clockwise_facings: bool,
}

/// Per-entity animation state component.
///
/// Attach to any entity that should animate. The `tick_animations()` system
/// reads and updates this each frame. The render loop reads `sequence` and
/// `frame_index` to select the correct SHP frame via `resolve_shp_frame()`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Animation {
    /// Currently playing sequence.
    pub sequence: SequenceKind,
    /// Current frame within the sequence (0 to frame_count - 1).
    pub frame_index: u16,
    /// Milliseconds accumulated since the last frame advance.
    pub elapsed_ms: u32,
    /// True if a HoldLast sequence has reached its final frame.
    pub finished: bool,
}

impl Animation {
    /// Create a new Animation starting at frame 0 of the given sequence.
    pub fn new(sequence: SequenceKind) -> Self {
        Self {
            sequence,
            frame_index: 0,
            elapsed_ms: 0,
            finished: false,
        }
    }

    /// Switch to a different sequence, resetting frame and timing.
    /// No-op if already playing the requested sequence.
    pub fn switch_to(&mut self, sequence: SequenceKind) {
        if self.sequence != sequence {
            self.sequence = sequence;
            self.frame_index = 0;
            self.elapsed_ms = 0;
            self.finished = false;
        }
    }
}

/// Collection of sequence definitions for one object type.
///
/// Maps `SequenceKind` → `SequenceDef`. Not all kinds need to be present;
/// entities hold their current frame if the active sequence is missing.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SequenceSet {
    sequences: BTreeMap<SequenceKind, SequenceDef>,
}

impl SequenceSet {
    /// Create an empty SequenceSet.
    pub fn new() -> Self {
        Self {
            sequences: BTreeMap::new(),
        }
    }

    /// Add a sequence definition.
    pub fn insert(&mut self, kind: SequenceKind, def: SequenceDef) {
        self.sequences.insert(kind, def);
    }

    /// Look up a sequence definition by kind.
    pub fn get(&self, kind: &SequenceKind) -> Option<&SequenceDef> {
        self.sequences.get(kind)
    }

    /// Number of defined sequences.
    pub fn len(&self) -> usize {
        self.sequences.len()
    }

    /// Whether no sequences are defined.
    pub fn is_empty(&self) -> bool {
        self.sequences.is_empty()
    }
}

/// Compute the SHP frame index for a given sequence, facing, and animation frame.
///
/// For directional sequences (facings > 1):
///   `start_frame + facing_index * facing_multiplier + frame_index`
/// For non-directional (facings == 1):
///   `start_frame + frame_index`
///
/// `facing` is the RA2 DirStruct byte (0–255, clockwise:
/// 0=N, 64=E, 128=S, 192=W). DirStruct is clockwise but infantry SHP frames
/// are laid out counter-clockwise (0=N, 1=NW, 2=W...), so the index is negated.
pub fn resolve_shp_frame(def: &SequenceDef, facing: u8, frame_index: u16) -> u16 {
    let clamped: u16 = if def.frame_count > 0 {
        frame_index % def.frame_count
    } else {
        0
    };

    if def.facings <= 1 {
        return def.start_frame + clamped;
    }

    // Quantize RA2 facing (0–255) to facing index (0..facings-1).
    // DirStruct is clockwise (0=N, 1=NE, 2=E...).
    // Infantry CCW sprites: +32 offset because DirStruct 0 = cell-north
    // (screen upper-right) but SHP frame 0 faces screen-north (straight up
    // = cell NW). The +32 rotates the lookup one step CW to compensate.
    let adjusted: u16 = if def.clockwise_facings {
        facing as u16
    } else {
        (facing as u16 + 32) % 256
    };
    let divisor: u16 = 256 / def.facings as u16;
    let cw_index: u16 = (adjusted / divisor) % def.facings as u16;
    // Infantry SHP frames are counter-clockwise (0=N, 1=NW, 2=W...) → negate.
    // SHP vehicle frames are clockwise (0=N, 1=NE, 2=E...) → use directly.
    let facing_index: u16 = if def.clockwise_facings {
        cw_index
    } else {
        (def.facings as u16 - cw_index) % def.facings as u16
    };

    def.start_frame + facing_index * def.facing_multiplier + clamped
}

/// Advance a single Animation by `dt_ms` milliseconds using its SequenceDef.
///
/// Returns `Some(SequenceKind)` if the sequence completed with a `TransitionTo`
/// loop mode, indicating the caller should switch to that sequence. Otherwise None.
pub fn advance_animation(
    anim: &mut Animation,
    def: &SequenceDef,
    dt_ms: u32,
) -> Option<SequenceKind> {
    if anim.finished || def.tick_ms == 0 || def.frame_count == 0 {
        return None;
    }

    anim.elapsed_ms += dt_ms;

    // Advance frame(s) while enough time has accumulated.
    while anim.elapsed_ms >= def.tick_ms {
        anim.elapsed_ms -= def.tick_ms;
        anim.frame_index += 1;

        if anim.frame_index >= def.frame_count {
            match def.loop_mode {
                LoopMode::Loop => {
                    anim.frame_index = 0;
                }
                LoopMode::HoldLast => {
                    anim.frame_index = def.frame_count.saturating_sub(1);
                    anim.finished = true;
                    anim.elapsed_ms = 0;
                    return None;
                }
                LoopMode::TransitionTo(next) => {
                    anim.frame_index = 0;
                    anim.elapsed_ms = 0;
                    return Some(next);
                }
            }
        }
    }

    None
}

/// Advance all animated entities in the ECS world by `dt_ms` milliseconds.
///
/// 1. Dying entities: skip auto-transitions, only advance death animation.
///    Returns IDs of dying entities whose death animation has finished.
/// 2. Auto-transitions: entities with MovementTarget switch to Walk;
///    entities without MovementTarget switch back to Stand.
/// 3. Attack transitions: stationary entities with attack_target switch to
///    Attack (or FireProne/DeployedFire depending on stance).
/// 4. Advances frame timing for each entity's current sequence.
///
/// `sequences` maps type_id → SequenceSet for frame timing lookup.
/// Entities whose type_id isn't in the map are skipped (no animation advance).
pub fn tick_animations(
    entities: &mut crate::sim::entity_store::EntityStore,
    sequences: &BTreeMap<String, SequenceSet>,
    dt_ms: u32,
    interner: &crate::sim::intern::StringInterner,
) -> Vec<u64> {
    let mut dying_finished: Vec<u64> = Vec::new();
    let keys: Vec<u64> = entities.keys_sorted();

    for &id in &keys {
        let Some(entity) = entities.get_mut(id) else {
            continue;
        };
        let Some(anim) = entity.animation.as_mut() else {
            // Dying entity with no animation → ready for despawn.
            if entity.dying {
                dying_finished.push(id);
            }
            continue;
        };

        // Dying entities: only advance the death animation, skip all transitions.
        if entity.dying {
            if anim.finished {
                dying_finished.push(id);
                continue;
            }
            let Some(seq_set) = sequences.get(interner.resolve(entity.type_ref)) else {
                dying_finished.push(id);
                continue;
            };
            let Some(def) = seq_set.get(&anim.sequence) else {
                dying_finished.push(id);
                continue;
            };
            advance_animation(anim, def, dt_ms);
            continue;
        }

        let has_movement: bool = entity.movement_target.is_some();
        let has_attack: bool = entity.attack_target.is_some();

        // Look up this type's sequence definitions for transition checks.
        let seq_set: Option<&SequenceSet> = sequences.get(interner.resolve(entity.type_ref));

        // Auto-transition: stand ↔ walk based on MovementTarget presence.
        if has_movement && anim.sequence == SequenceKind::Stand {
            anim.switch_to(SequenceKind::Walk);
        } else if !has_movement && anim.sequence == SequenceKind::Walk {
            anim.switch_to(SequenceKind::Stand);
        }

        // Attack animation: when entity is stationary with an attack target,
        // switch to the appropriate fire sequence based on current stance.
        if has_attack && !has_movement {
            if let Some(set) = seq_set {
                match anim.sequence {
                    SequenceKind::Stand if set.get(&SequenceKind::Attack).is_some() => {
                        anim.switch_to(SequenceKind::Attack);
                    }
                    SequenceKind::Prone if set.get(&SequenceKind::FireProne).is_some() => {
                        anim.switch_to(SequenceKind::FireProne);
                    }
                    SequenceKind::Deployed if set.get(&SequenceKind::DeployedFire).is_some() => {
                        anim.switch_to(SequenceKind::DeployedFire);
                    }
                    SequenceKind::Swim if set.get(&SequenceKind::WetAttack).is_some() => {
                        anim.switch_to(SequenceKind::WetAttack);
                    }
                    SequenceKind::Fly if set.get(&SequenceKind::FireFly).is_some() => {
                        anim.switch_to(SequenceKind::FireFly);
                    }
                    _ => {}
                }
            }
        }

        // Advance frame timing.
        let Some(set) = seq_set else {
            continue;
        };
        let Some(def) = set.get(&anim.sequence) else {
            continue;
        };

        if let Some(next) = advance_animation(anim, def, dt_ms) {
            anim.switch_to(next);
        }
    }

    dying_finished
}

/// Number of animation frames in oregath.shp per facing direction.
const HARVEST_OVERLAY_FRAMES: u16 = 15;

/// Milliseconds per harvest overlay frame — one frame per sim tick (15 Hz = 67ms).
/// oregath.shp is a hardcoded engine asset with no INI-configurable Rate, so the
/// animation advances once per game logic tick like other engine-driven overlays.
const HARVEST_OVERLAY_FRAME_MS: u32 = 67;

/// Advance all HarvestOverlay components by `dt_ms` milliseconds.
///
/// Cycles through the 15-frame oregath.shp animation for harvesters that are
/// actively gathering ore. When not visible, the overlay is skipped.
pub fn tick_harvest_overlays(entities: &mut crate::sim::entity_store::EntityStore, dt_ms: u32) {
    if dt_ms == 0 {
        return;
    }
    let keys: Vec<u64> = entities.keys_sorted();
    for &id in &keys {
        let Some(entity) = entities.get_mut(id) else {
            continue;
        };
        let Some(overlay) = entity.harvest_overlay.as_mut() else {
            continue;
        };
        if !overlay.visible {
            continue;
        }
        overlay.elapsed_ms += dt_ms;
        while overlay.elapsed_ms >= HARVEST_OVERLAY_FRAME_MS {
            overlay.elapsed_ms -= HARVEST_OVERLAY_FRAME_MS;
            overlay.frame = (overlay.frame + 1) % HARVEST_OVERLAY_FRAMES;
        }
    }
}

/// Advance all VoxelAnimation components by `dt_ms` milliseconds.
///
/// Cycles through HVA frames for voxel entities that have `playing == true`.
/// Frame wraps around to 0 when reaching frame_count (looping animation).
pub fn tick_voxel_animations(entities: &mut crate::sim::entity_store::EntityStore, dt_ms: u32) {
    if dt_ms == 0 {
        return;
    }
    let keys: Vec<u64> = entities.keys_sorted();
    for &id in &keys {
        let Some(entity) = entities.get_mut(id) else {
            continue;
        };
        let Some(anim) = entity.voxel_animation.as_mut() else {
            continue;
        };
        if !anim.playing || anim.frame_count <= 1 || anim.tick_ms == 0 {
            continue;
        }
        anim.elapsed_ms += dt_ms;
        while anim.elapsed_ms >= anim.tick_ms {
            anim.elapsed_ms -= anim.tick_ms;
            anim.frame = (anim.frame + 1) % anim.frame_count;
        }
    }
}

/// Create the default infantry sequence set matching RA2's standard frame layout.
///
/// Based on RA2's standard infantry sequence defaults:
/// - Stand: frames 0–7 (1 frame × 8 facings)
/// - Walk: frames 8–55 (6 frames × 8 facings, 100ms/frame)
/// - Idle1: frames 56–70 (15 frames, non-directional, 120ms/frame)
/// - Idle2: frames 71–85 (15 frames, non-directional, 120ms/frame)
/// - Die1: frames 86–100 (15 frames, non-directional, 80ms/frame)
/// - Die2: frames 101–115 (15 frames, non-directional, 80ms/frame)
pub fn default_infantry_sequences() -> SequenceSet {
    let mut set: SequenceSet = SequenceSet::new();

    set.insert(
        SequenceKind::Stand,
        SequenceDef {
            start_frame: 0,
            frame_count: 1,
            facings: INFANTRY_FACINGS,
            facing_multiplier: 1,
            tick_ms: DEFAULT_STAND_TICK_MS,
            loop_mode: LoopMode::Loop,
            clockwise_facings: false,
        },
    );
    set.insert(
        SequenceKind::Walk,
        SequenceDef {
            start_frame: 8,
            frame_count: 6,
            facings: INFANTRY_FACINGS,
            facing_multiplier: 6,
            tick_ms: DEFAULT_WALK_TICK_MS,
            loop_mode: LoopMode::Loop,
            clockwise_facings: false,
        },
    );
    set.insert(
        SequenceKind::Idle1,
        SequenceDef {
            start_frame: 56,
            frame_count: 15,
            facings: 1,
            facing_multiplier: 0,
            tick_ms: DEFAULT_IDLE_TICK_MS,
            loop_mode: LoopMode::TransitionTo(SequenceKind::Stand),
            clockwise_facings: false,
        },
    );
    set.insert(
        SequenceKind::Idle2,
        SequenceDef {
            start_frame: 71,
            frame_count: 15,
            facings: 1,
            facing_multiplier: 0,
            tick_ms: DEFAULT_IDLE_TICK_MS,
            loop_mode: LoopMode::TransitionTo(SequenceKind::Stand),
            clockwise_facings: false,
        },
    );
    set.insert(
        SequenceKind::Die1,
        SequenceDef {
            start_frame: 86,
            frame_count: 15,
            facings: 1,
            facing_multiplier: 0,
            tick_ms: DEFAULT_DIE_TICK_MS,
            loop_mode: LoopMode::HoldLast,
            clockwise_facings: false,
        },
    );
    set.insert(
        SequenceKind::Die2,
        SequenceDef {
            start_frame: 101,
            frame_count: 15,
            facings: 1,
            facing_multiplier: 0,
            tick_ms: DEFAULT_DIE_TICK_MS,
            loop_mode: LoopMode::HoldLast,
            clockwise_facings: false,
        },
    );

    set
}

/// Create default building sequence set (single idle frame, no animation).
///
/// Buildings typically use frame 0 as their idle state. Animated buildings
/// (e.g., power plant, radar dome) will need per-type overrides later.
pub fn default_building_sequences() -> SequenceSet {
    let mut set: SequenceSet = SequenceSet::new();

    set.insert(
        SequenceKind::Stand,
        SequenceDef {
            start_frame: 0,
            frame_count: 1,
            facings: 1,
            facing_multiplier: 0,
            tick_ms: DEFAULT_STAND_TICK_MS,
            loop_mode: LoopMode::Loop,
            clockwise_facings: false,
        },
    );

    set
}

/// Map warhead InfDeath value to the appropriate death SequenceKind.
///
/// InfDeath: 1→Die1, 2→Die2, 3→Die3, 4→Die4, 5→Die5. 0 defaults to Die1.
pub fn death_sequence_for_inf_death(inf_death: u8) -> SequenceKind {
    match inf_death.min(5) {
        2 => SequenceKind::Die2,
        3 => SequenceKind::Die3,
        4 => SequenceKind::Die4,
        5 => SequenceKind::Die5,
        _ => SequenceKind::Die1,
    }
}

#[cfg(test)]
#[path = "animation_tests.rs"]
mod tests;
