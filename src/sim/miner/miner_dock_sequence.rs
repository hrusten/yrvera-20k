//! Refinery docking visual sequence — approach, enter pad, unload, exit.
//!
//! Drives the sub-state machine (`RefineryDockPhase`) when the miner is in
//! `MinerState::Dock`. Reproduces the original game's `BuildingClass::
//! DockingSequence_Update` choreography: clear path → rotate → drive onto
//! pad → 180° turn → unload → drive off.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/miner, sim/miner_dock, sim/components,
//!   sim/movement, sim/turret, rules/.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::rules::ruleset::RuleSet;
use crate::sim::miner::{MinerConfig, MinerState, RefineryDockPhase};
use crate::sim::movement;
use crate::sim::movement::turret;
use crate::sim::pathfinding::PathGrid;
use crate::sim::world::{SimSoundEvent, Simulation};
use crate::util::fixed_math::{facing_from_delta_int, SimFixed};

use super::miner_system::{player_has_purifier, MinerSnapshot};
use crate::sim::production::{credits_entry_for_owner, foundation_dimensions};

/// Helper: record a dock phase transition to the snapshot's debug buffer.
fn record_dock_phase(snap: &mut MinerSnapshot, old: RefineryDockPhase, new: RefineryDockPhase) {
    snap.debug_dock_events
        .push((format!("{:?}", old), format!("{:?}", new)));
}

/// Sim tick period in ms (15 Hz).
const TICK_MS: u32 = 67;

/// Body ROT for all harvesters during docking.
///
/// The original engine unconditionally overrides ROT=10 for Harvester/Weeder
/// units during docking, regardless of whatever the INI says (e.g. CMIN has ROT=5).
const HARVESTER_BODY_ROT: i32 = 10;

/// Exit facing from the original engine (0x47 = 71 ~ east-southeast).
///
/// The unit is ejected at offset (-0x80, +0x80) with facing 0x47.
const EXIT_FACING: u8 = 0x47;

// ---------------------------------------------------------------------------
// Cell computation helpers
// ---------------------------------------------------------------------------

/// Queue cell — where the miner waits outside the refinery (pathfindable).
///
/// Uses art.ini `QueueingCell=` when available (merged into ObjectType),
/// otherwise falls back to geometric approximation from foundation dimensions.
/// TibSun legacy: `QueueingCell=4,1` for a 4×3 foundation places the queue
/// one cell east of the building's east edge, vertically centred.
pub(super) fn refinery_queue_cell(
    rx: u16,
    ry: u16,
    width: u16,
    height: u16,
    queueing_cell: Option<(u16, u16)>,
) -> (u16, u16) {
    if let Some((qx, qy)) = queueing_cell {
        // QueueingCell is a cell offset from building origin.
        (rx + qx, ry + qy)
    } else {
        (rx + width, ry + height / 2)
    }
}

/// Pad cell — on the refinery platform inside the building footprint.
///
/// Uses art.ini `DockingOffset0=` when available (merged into ObjectType),
/// converting from lepton offset to cell offset (256 leptons per cell).
/// Otherwise falls back to rightmost foundation column, vertically centred.
pub(super) fn refinery_pad_cell(
    rx: u16,
    ry: u16,
    width: u16,
    height: u16,
    docking_offset: Option<(i32, i32, i32)>,
) -> (u16, u16) {
    if let Some((dx, dy, _)) = docking_offset {
        // DockingOffset is in leptons (256 per cell). Round to nearest cell.
        let cx = (dx + 128) / 256;
        let cy = (dy + 128) / 256;
        (
            (rx as i32 + cx).max(0) as u16,
            (ry as i32 + cy).max(0) as u16,
        )
    } else {
        (rx + width.saturating_sub(1), ry + height / 2)
    }
}

/// Exit cell — where the miner drives after undocking.
///
/// `exit = building_center + (-0x80, +0x80)` leptons, i.e. half a cell
/// west and half a cell south of the foundation center.
pub(super) fn refinery_exit_cell(
    rx: u16,
    ry: u16,
    width: u16,
    height: u16,
    _queueing_cell: Option<(u16, u16)>,
) -> (u16, u16) {
    // Building center in leptons: (rx*256 + (w-1)*128, ry*256 + (h-1)*128).
    // UndockUnit offset: (-128, +128) leptons from center.
    // Combined and divided by 256 for cell coordinates.
    let exit_x = (rx as i32 * 256 + (width as i32 - 2) * 128) / 256;
    let exit_y = (ry as i32 * 256 + height as i32 * 128) / 256;
    (exit_x.max(0) as u16, exit_y.max(0) as u16)
}

/// Compute the RA2 facing (0–255) from cell `a` toward cell `b`.
///
/// Convention: 0 = N, 64 = NE, 128 = S, 192 = W.
/// Delegates to deterministic `facing_from_delta_int` (no f64 atan2).
fn facing_from_to(a: (u16, u16), b: (u16, u16)) -> u8 {
    let dx: i32 = b.0 as i32 - a.0 as i32;
    let dy: i32 = b.1 as i32 - a.1 as i32;
    facing_from_delta_int(dx, dy)
}

// ---------------------------------------------------------------------------
// Rotation helper
// ---------------------------------------------------------------------------

/// Apply one tick of body rotation toward `target_facing`.
/// Returns `true` when rotation is complete.
fn apply_rotation(sim: &mut Simulation, entity_id: u64, target_facing: u8, rot: i32) -> bool {
    let Some(entity) = sim.entities.get_mut(entity_id) else {
        return true;
    };
    let max_delta: u8 = turret::rot_to_facing_delta(rot, TICK_MS);
    if max_delta == 0 {
        entity.facing = target_facing;
        return true;
    }
    let diff: i16 = turret::shortest_rotation(entity.facing, target_facing);
    if diff.unsigned_abs() <= max_delta as u16 {
        entity.facing = target_facing;
        true
    } else if diff > 0 {
        entity.facing = entity.facing.wrapping_add(max_delta);
        false
    } else {
        entity.facing = entity.facing.wrapping_sub(max_delta);
        false
    }
}

// ---------------------------------------------------------------------------
// Refinery lookup helpers
// ---------------------------------------------------------------------------

/// Resolve a refinery entity's foundation and compute queue/pad/exit cells.
/// Uses QueueingCell and DockingOffset from art.ini when available (merged into
/// ObjectType by `merge_art_data`), falling back to geometric approximation.
/// Returns `(queue, pad, exit)` or `None` if the refinery is gone.
fn resolve_refinery_cells(
    sim: &Simulation,
    rules: &RuleSet,
    ref_sid: u64,
) -> Option<((u16, u16), (u16, u16), (u16, u16))> {
    let entity = sim.entities.get(ref_sid)?;
    let obj = rules.object_case_insensitive(sim.interner.resolve(entity.type_ref));
    let (w, h) = obj
        .map(|o| foundation_dimensions(&o.foundation))
        .unwrap_or((1, 1));
    let qc = obj.and_then(|o| o.queueing_cell);
    let dock_off = obj.and_then(|o| o.docking_offset);
    let rx = entity.position.rx;
    let ry = entity.position.ry;
    Some((
        refinery_queue_cell(rx, ry, w, h, qc),
        refinery_pad_cell(rx, ry, w, h, dock_off),
        refinery_exit_cell(rx, ry, w, h, qc),
    ))
}

/// Body rotation rate for harvesters during the dock sequence.
///
/// The original engine overrides ROT to 10 for all Harvester/Weeder units
/// during docking. This function is only called from dock phase handlers
/// which only execute for miner entities, so the override is unconditional.
fn body_rot(_rules: &RuleSet, _type_id: &str) -> i32 {
    HARVESTER_BODY_ROT
}

/// Look up the UnloadingClass for a miner type from rules.ini.
fn unloading_class(rules: &RuleSet, type_id: &str) -> Option<String> {
    rules
        .object_case_insensitive(type_id)
        .and_then(|obj| obj.unloading_class.clone())
}

// ---------------------------------------------------------------------------
// Main dock sequence handler
// ---------------------------------------------------------------------------

/// Process one tick of the refinery docking sequence for a single miner.
///
/// Called from `miner_system::process_miner` when `snap.miner.state == Dock`.
/// Mutates `snap.miner` (written back in phase 3) and directly mutates the
/// entity for facing/position/movement_target via `sim.entities.get_mut()`.
pub(super) fn handle_dock_sequence(
    sim: &mut Simulation,
    rules: &RuleSet,
    config: &MinerConfig,
    path_grid: Option<&PathGrid>,
    snap: &mut MinerSnapshot,
) {
    let phase_before = snap.miner.dock_phase;

    let Some(ref_sid) = snap.miner.reserved_refinery else {
        // Lost reservation — abort to SearchOre.
        snap.miner.state = MinerState::SearchOre;
        snap.miner.dock_phase = RefineryDockPhase::Approach;
        if phase_before != snap.miner.dock_phase {
            record_dock_phase(snap, phase_before, snap.miner.dock_phase);
        }
        return;
    };

    // Resolve refinery cells. If refinery is destroyed, abort.
    let Some((queue, pad, exit)) = resolve_refinery_cells(sim, rules, ref_sid) else {
        snap.miner.reserved_refinery = None;
        snap.miner.state = MinerState::SearchOre;
        snap.miner.dock_phase = RefineryDockPhase::Approach;
        if phase_before != snap.miner.dock_phase {
            record_dock_phase(snap, phase_before, snap.miner.dock_phase);
        }
        return;
    };

    match snap.miner.dock_phase {
        RefineryDockPhase::Approach => {
            phase_approach(sim, path_grid, snap, queue);
        }
        RefineryDockPhase::WaitForDock => {
            phase_wait_for_dock(sim, snap, ref_sid);
        }
        RefineryDockPhase::RotateToPad => {
            phase_rotate_to_pad(sim, rules, snap, queue, pad);
        }
        RefineryDockPhase::EnterPad => {
            phase_enter_pad(sim, snap, pad, ref_sid);
        }
        RefineryDockPhase::TurnOnPad => {
            phase_turn_on_pad(sim, rules, snap, pad, exit, ref_sid);
        }
        RefineryDockPhase::Unloading => {
            phase_unloading(sim, rules, config, snap, ref_sid);
        }
        RefineryDockPhase::ExitPad => {
            phase_exit_pad(sim, snap, pad, exit, ref_sid);
        }
    }

    // Record dock phase change if any phase handler transitioned.
    if phase_before != snap.miner.dock_phase {
        record_dock_phase(snap, phase_before, snap.miner.dock_phase);
    }
}

// ---------------------------------------------------------------------------
// Phase handlers
// ---------------------------------------------------------------------------

fn phase_approach(
    sim: &mut Simulation,
    path_grid: Option<&PathGrid>,
    snap: &mut MinerSnapshot,
    queue: (u16, u16),
) {
    if is_adjacent_or_at((snap.rx, snap.ry), queue) {
        snap.miner.dock_phase = RefineryDockPhase::WaitForDock;
        return;
    }
    // Issue pathfinding move to queue cell if not already moving there.
    if let Some(grid) = path_grid {
        issue_move_if_idle(&mut sim.entities, grid, snap.entity_id, queue, snap.speed);
    }
}

fn phase_wait_for_dock(sim: &mut Simulation, snap: &mut MinerSnapshot, ref_sid: u64) {
    if sim
        .production
        .dock_reservations
        .try_reserve(ref_sid, snap.entity_id)
    {
        snap.miner.dock_queued = false;
        snap.miner.dock_phase = RefineryDockPhase::RotateToPad;
    } else {
        snap.miner.dock_queued = true;
    }
}

fn phase_rotate_to_pad(
    sim: &mut Simulation,
    rules: &RuleSet,
    snap: &mut MinerSnapshot,
    queue: (u16, u16),
    pad: (u16, u16),
) {
    let target_facing: u8 = facing_from_to(queue, pad);
    let rot: i32 = body_rot(rules, sim.interner.resolve(snap.type_id));
    if apply_rotation(sim, snap.entity_id, target_facing, rot) {
        // Rotation complete — issue a direct move onto the pad cell.
        // The pad is inside the building footprint so A* can't reach it;
        // issue_direct_move bypasses pathfinding (matches original engine's
        // ILocomotion::MoveTo with speed 1.0).
        movement::issue_direct_move(&mut sim.entities, snap.entity_id, pad, snap.speed);
        snap.miner.dock_phase = RefineryDockPhase::EnterPad;
    }
}

fn phase_enter_pad(sim: &mut Simulation, snap: &mut MinerSnapshot, pad: (u16, u16), ref_sid: u64) {
    // TibSun legacy: activate dock door animation when unit begins entering
    // the pad area, not when it has finished turning. Original opens anim
    // slot 7 at the drive-into-building state transition.
    if let Some(refinery) = sim.entities.get_mut(ref_sid) {
        if !refinery.dock_active_anim {
            refinery.dock_active_anim = true;
        }
    }

    // Wait for the smooth movement to complete.
    let arrived = sim
        .entities
        .get(snap.entity_id)
        .is_some_and(|e| e.movement_target.is_none());
    if arrived {
        snap.rx = pad.0;
        snap.ry = pad.1;
        snap.miner.dock_phase = RefineryDockPhase::TurnOnPad;
    }
}

fn phase_turn_on_pad(
    sim: &mut Simulation,
    rules: &RuleSet,
    snap: &mut MinerSnapshot,
    _pad: (u16, u16),
    exit: (u16, u16),
    ref_sid: u64,
) {
    // 180° turn: face the exit cell direction (east for standard refineries).
    let current_pos = (snap.rx, snap.ry);
    let target_facing: u8 = facing_from_to(current_pos, exit);
    let rot: i32 = body_rot(rules, sim.interner.resolve(snap.type_id));
    if apply_rotation(sim, snap.entity_id, target_facing, rot) {
        // Set UnloadingClass override for visual model swap.
        if let Some(uc) = unloading_class(rules, sim.interner.resolve(snap.type_id)) {
            if let Some(entity) = sim.entities.get_mut(snap.entity_id) {
                entity.display_type_override = Some(sim.interner.intern(&uc));
            }
        }
        // Note: dock_active_anim is already set in phase_enter_pad (earlier timing).
        // Emit deploy sound event — the app layer resolves the actual sound
        // and picks healthy/damaged variant based on refinery health.
        sim.sound_events.push(SimSoundEvent::DockDeploy {
            building_id: ref_sid,
        });
        snap.miner.dock_phase = RefineryDockPhase::Unloading;
        snap.miner.unload_timer = 0;
    }
}

fn phase_unloading(
    sim: &mut Simulation,
    rules: &RuleSet,
    config: &MinerConfig,
    snap: &mut MinerSnapshot,
    ref_sid: u64,
) {
    // Timer countdown.
    if snap.miner.unload_timer > 0 {
        snap.miner.unload_timer -= 1;
        return;
    }

    // Pop one bale and award base credits. Accumulate total for purifier bonus.
    if let Some(bale) = snap.miner.cargo.pop() {
        let value: i32 = i32::from(bale.value);
        snap.miner.unload_base_total += value as u32;
        let owner_str = sim.interner.resolve(snap.owner).to_string();
        let credits = credits_entry_for_owner(sim, &owner_str);
        *credits = credits.saturating_add(value);
        snap.miner.unload_timer = config.unload_tick_interval;
        return;
    }

    // Cargo empty — apply purifier bonus on the accumulated total, then finish.
    if snap.miner.unload_base_total > 0
        && player_has_purifier(sim, rules, sim.interner.resolve(snap.owner))
    {
        let bonus_pct: i32 = (rules.general.purifier_bonus * 100.0) as i32;
        let bonus: i32 = snap.miner.unload_base_total as i32 * bonus_pct / 100;
        let owner_str = sim.interner.resolve(snap.owner).to_string();
        let credits = credits_entry_for_owner(sim, &owner_str);
        *credits = credits.saturating_add(bonus);
    }
    snap.miner.unload_base_total = 0;

    // Release dock and transition to exit.
    sim.production.dock_reservations.release(ref_sid);
    snap.miner.home_refinery = Some(ref_sid);

    // Clear UnloadingClass override.
    if let Some(entity) = sim.entities.get_mut(snap.entity_id) {
        entity.display_type_override = None;
    }

    // Deactivate the refinery's ActiveAnim now that unloading is done.
    if let Some(refinery) = sim.entities.get_mut(ref_sid) {
        refinery.dock_active_anim = false;
    }

    snap.miner.dock_phase = RefineryDockPhase::ExitPad;
}

fn phase_exit_pad(
    sim: &mut Simulation,
    snap: &mut MinerSnapshot,
    _pad: (u16, u16),
    exit: (u16, u16),
    _ref_sid: u64,
) {
    // First call: issue direct move to exit cell with the original engine's
    // exit facing (0x47 ≈ east-southeast).
    let moving = sim
        .entities
        .get(snap.entity_id)
        .is_some_and(|e| e.movement_target.is_some());
    let at_exit = (snap.rx, snap.ry) == exit;

    if !moving && !at_exit {
        // Issue the exit move and set facing to match original engine.
        movement::issue_direct_move(&mut sim.entities, snap.entity_id, exit, snap.speed);
        if let Some(entity) = sim.entities.get_mut(snap.entity_id) {
            entity.facing_target = Some(EXIT_FACING);
        }
        return;
    }

    if !moving && at_exit {
        // Arrived at exit — finish docking.
        snap.miner.reserved_refinery = None;
        snap.miner.dock_queued = false;
        snap.miner.forced_return = false;
        snap.miner.dock_phase = RefineryDockPhase::Approach;
        snap.miner.state = MinerState::SearchOre;
        return;
    }

    // Still moving — wait.
    if let Some(entity) = sim.entities.get(snap.entity_id) {
        snap.rx = entity.position.rx;
        snap.ry = entity.position.ry;
    }
}

// ---------------------------------------------------------------------------
// Utility (re-exported from miner_system for shared use)
// ---------------------------------------------------------------------------

/// True if `pos` is at `target` or cardinally/diagonally adjacent (1 cell away).
fn is_adjacent_or_at(pos: (u16, u16), target: (u16, u16)) -> bool {
    let dx = (pos.0 as i32 - target.0 as i32).unsigned_abs();
    let dy = (pos.1 as i32 - target.1 as i32).unsigned_abs();
    dx <= 1 && dy <= 1
}

/// Issue a move command only if the entity isn't already pathing to this target.
fn issue_move_if_idle(
    entities: &mut crate::sim::entity_store::EntityStore,
    grid: &PathGrid,
    entity_id: u64,
    target: (u16, u16),
    speed: SimFixed,
) {
    let already = entities
        .get(entity_id)
        .and_then(|e| e.movement_target.as_ref())
        .and_then(|mt| mt.path.last().copied())
        .is_some_and(|goal| goal == target);
    if !already {
        let _ = movement::issue_move_command(
            entities, grid, entity_id, target, speed, false, None, None,
        );
    }
}
