//! Movement step helpers — cell transition mechanics, vehicle rotation, lepton advancement,
//! and cell boundary crossing detection.
//!
//! Contains the inner-loop logic extracted from `tick_movement_with_grids`: how a mover
//! rotates in place, advances sub-cell position, detects cell boundary crossings, and
//! performs the actual cell transition with occupancy/terrain checks.

use std::collections::BTreeSet;

use crate::map::entities::EntityCategory;
use crate::map::resolved_terrain::ResolvedTerrainGrid;
use crate::rules::locomotor_type::LocomotorKind;
use crate::sim::components::{MovementTarget, Position};
use crate::sim::debug_event_log::DebugEventKind;
use crate::sim::movement::bump_crush;
use crate::sim::movement::drive_track::{self, DriveTrackState};
use crate::sim::movement::locomotor::{GroundMovePhase, LocomotorState, MovementLayer};
use crate::sim::movement::movement_blocked::handle_blocked_tick;
use crate::sim::movement::movement_bridge::resolve_cell_transition_bridge_state;
use crate::sim::movement::movement_occupancy::{
    DeferredCellCheck, detect_deferred_cell_check, naval_terrain_diag,
};
use crate::sim::movement::movement_reservation::reserve_destination_after_transition;
use crate::sim::movement::turret::{rot_to_facing_delta, shortest_rotation};
use crate::sim::occupancy::OccupancyGrid;
use crate::sim::pathfinding::PathGrid;
use crate::sim::pathfinding::terrain_cost::TerrainCostGrid;
use crate::sim::rng::SimRng;
use crate::util::fixed_math::{
    SIM_HALF, SIM_ONE, SIM_ZERO, SimFixed, facing_from_delta_int as facing_from_delta,
    fixed_distance,
};
use crate::util::lepton::CELL_CENTER_LEPTON;

use super::{
    CLIFF_HEIGHT_THRESHOLD, MovementConfig, MovementTickStats, MoverSnapshot, PATH_STUCK_INIT,
    PathfindingContext,
};

pub(super) fn apply_cell_transition_remainder(
    target: &mut MovementTarget,
    position: &mut Position,
    dx_cell: i32,
    dy_cell: i32,
    nx: u16,
    ny: u16,
    is_infantry: bool,
) {
    // Infantry: clear blocking state on each cell arrival (fresh grace period).
    // Vehicles: keep both flags — once blocked, urgency escalates permanently.
    if is_infantry {
        target.blocked_delay = 0;
        target.path_blocked = false;
    }
    if dx_cell > 0 {
        position.sub_x -= crate::util::lepton::LEPTONS_PER_CELL;
    } else if dx_cell < 0 {
        position.sub_x += crate::util::lepton::LEPTONS_PER_CELL;
    }
    if dy_cell > 0 {
        position.sub_y -= crate::util::lepton::LEPTONS_PER_CELL;
    } else if dy_cell < 0 {
        position.sub_y += crate::util::lepton::LEPTONS_PER_CELL;
    }
    position.rx = nx;
    position.ry = ny;
}

pub(super) fn configure_motion_after_transition(
    target: &mut MovementTarget,
    locomotor: &Option<LocomotorState>,
    drive_track: &mut Option<DriveTrackState>,
    facing: &mut u8,
    facing_target: &mut Option<u8>,
    category: EntityCategory,
    mover_rot: i32,
    current_cell: (u16, u16),
    current_sub: (SimFixed, SimFixed),
) {
    target.next_index += 1;
    if target.next_index < target.path.len() {
        let next = target.path[target.next_index];
        let ndx = next.0 as i32 - current_cell.0 as i32;
        let ndy = next.1 as i32 - current_cell.1 as i32;

        let new_face = facing_from_delta(ndx, ndy);
        let uses_drive_tracks = locomotor
            .as_ref()
            .is_some_and(|l| matches!(l.kind, LocomotorKind::Drive));
        let track_initiated = if uses_drive_tracks && new_face != *facing {
            if let Some(sel) = drive_track::select_drive_track(*facing, new_face, false) {
                *drive_track =
                    drive_track::begin_drive_track(sel.raw_track_index, sel.flags, ndx, ndy);
                drive_track.is_some()
            } else {
                false
            }
        } else {
            *drive_track = None;
            false
        };

        if track_initiated {
            *facing_target = None;
        } else if category == EntityCategory::Infantry || mover_rot <= 0 {
            *facing = new_face;
        } else {
            *facing_target = Some(new_face);
        }

        if category == EntityCategory::Infantry {
            // Infantry: direction from current sub-cell toward next cell's subcell position.
            // Use the allocated subcell offset to maintain visual spread during movement,
            // matching the WalkLocomotionClass which walks to FindSubCellDest result.
            let (sc_x, sc_y) = locomotor
                .as_ref()
                .and_then(|l| l.subcell_dest)
                .unwrap_or((CELL_CENTER_LEPTON, CELL_CENTER_LEPTON));
            let dest_x = SimFixed::from_num(ndx * 256) + sc_x;
            let dest_y = SimFixed::from_num(ndy * 256) + sc_y;
            let dx = dest_x - current_sub.0;
            let dy = dest_y - current_sub.1;
            target.move_dir_x = dx;
            target.move_dir_y = dy;
            target.move_dir_len = fixed_distance(dx, dy);
        } else {
            let (d_x, d_y, d_len) = crate::util::lepton::cell_delta_to_lepton_dir(ndx, ndy);
            target.move_dir_x = d_x;
            target.move_dir_y = d_y;
            target.move_dir_len = d_len;
        }
    } else if let Some(loco) = locomotor {
        if let Some((dest_x, dest_y)) = loco.subcell_dest {
            let dx = dest_x - current_sub.0;
            let dy = dest_y - current_sub.1;
            target.move_dir_x = dx;
            target.move_dir_y = dy;
            let len: SimFixed = fixed_distance(dx, dy);
            target.move_dir_len = if len > SIM_HALF { len } else { SIM_ONE };
        }
    }
}

/// Result of vehicle rotation — tells the caller whether to skip this tick.
pub(super) enum RotationResult {
    /// Still rotating in place — caller should `continue` (skip lepton advancement).
    StillRotating {
        debug_events: Vec<(u32, DebugEventKind)>,
    },
    /// Rotation complete or not needed — proceed with movement.
    ReadyToMove,
}

/// Handle vehicle in-place rotation before movement begins.
///
/// Vehicles rotate toward `facing_target` before advancing. If ROT > 0, gradual
/// rotation is applied; ROT = 0 means instant snap. Infantry are excluded by the
/// caller (they always turn instantly without this function).
///
/// Takes individual fields to avoid borrow conflicts with `entity.movement_target`.
pub(super) fn handle_vehicle_rotation(
    facing: &mut u8,
    facing_target: &mut Option<u8>,
    position: &mut Position,
    locomotor: &mut Option<LocomotorState>,
    rot: i32,
    tick_ms: u32,
    sim_tick: u64,
) -> RotationResult {
    let Some(target_facing) = *facing_target else {
        return RotationResult::ReadyToMove;
    };
    if rot > 0 {
        let max_delta: u8 = rot_to_facing_delta(rot, tick_ms);
        let diff: i16 = shortest_rotation(*facing, target_facing);
        if diff.unsigned_abs() <= max_delta as u16 {
            // Close enough — snap to exact facing and start moving.
            *facing = target_facing;
            *facing_target = None;
            RotationResult::ReadyToMove
        } else {
            // Still rotating — advance facing but don't move.
            if diff > 0 {
                *facing = facing.wrapping_add(max_delta);
            } else {
                *facing = facing.wrapping_sub(max_delta);
            }
            // Update screen position (entity stays in place but facing changed).
            position.refresh_screen_coords();
            // Skip lepton advancement — still rotating in place.
            let mut debug_events = Vec::new();
            if let Some(loco) = locomotor {
                let old_phase = loco.phase;
                loco.phase = GroundMovePhase::Accelerating;
                if old_phase != GroundMovePhase::Accelerating {
                    debug_events.push((
                        sim_tick as u32,
                        DebugEventKind::PhaseChange {
                            from: format!("{:?}", old_phase),
                            to: "Accelerating".into(),
                            reason: "movement started".into(),
                        },
                    ));
                }
            }
            RotationResult::StillRotating { debug_events }
        }
    } else {
        // ROT=0 — instant turn, no gradual rotation.
        *facing = target_facing;
        *facing_target = None;
        RotationResult::ReadyToMove
    }
}

/// Result of lepton position advancement.
pub(super) enum AdvanceResult {
    /// Drive track is active — caller should `continue` (skip cell crossings).
    DriveTrackActive,
    /// Drive track crossed a cell boundary — caller must handle the cell
    /// transition (update rx/ry, advance next_index, reserve destination),
    /// then continue the track on the next tick.
    DriveTrackCellJump,
    /// Drive track reached the chain_index — caller should attempt to chain
    /// into a follow-on track curve (check passability of the next-next cell,
    /// select new track if OK). If chaining fails, the current track continues
    /// on the next tick.
    DriveTrackChainReady,
    /// Normal advancement done — caller should proceed to cell crossings.
    ReadyForCrossings,
}

/// Advance sub_x/sub_y toward the next cell — either via drive track (smooth
/// curve) or straight-line lepton vector. Includes infantry wobble seeding.
///
/// Takes individual entity fields to avoid borrow conflicts with
/// `entity.movement_target` (which the caller holds as `ref mut target`).
pub(super) fn advance_lepton_position(
    target: &mut MovementTarget,
    position: &mut Position,
    facing: &mut u8,
    facing_target: &mut Option<u8>,
    drive_track_state: &mut Option<DriveTrackState>,
    locomotor: &mut Option<LocomotorState>,
    category: EntityCategory,
    effective_speed: SimFixed,
    dt: SimFixed,
    entity_id: u64,
) -> AdvanceResult {
    if let Some(track_state) = drive_track_state {
        // Drive track advancement: step through pre-computed curve points.
        // The track handles position AND facing, producing smooth turns.
        let advance = drive_track::advance_drive_track(track_state, effective_speed, dt);
        *facing = advance.facing;
        *facing_target = None; // track handles facing

        if advance.cell_jump && target.next_index < target.path.len() {
            // Coordinate-based cell crossing detected — the transformed track
            // point position landed in a different cell. The cell_offset was
            // already adjusted inside advance_drive_track. Update visual position.
            position.sub_x = advance.sub_x;
            position.sub_y = advance.sub_y;
            position.refresh_screen_coords();
            // Signal the caller to handle the actual cell transition
            // (update rx/ry, reserve destination, bridge state, etc.).
            return AdvanceResult::DriveTrackCellJump;
        }

        if advance.chain_ready && target.next_index < target.path.len() {
            // Track reached chain_index — signal caller to attempt chaining.
            // The caller will check Can_Enter_Cell on the next-next cell and
            // either replace the track state or let the current track continue.
            position.sub_x = advance.sub_x;
            position.sub_y = advance.sub_y;
            position.refresh_screen_coords();
            return AdvanceResult::DriveTrackChainReady;
        }

        if advance.finished {
            *drive_track_state = None;
            // Track complete — snap to cell center so standard movement resumes.
            position.sub_x = crate::util::lepton::CELL_CENTER_LEPTON;
            position.sub_y = crate::util::lepton::CELL_CENTER_LEPTON;
            // Fall through to ReadyForCrossings — normal movement takes over.
        } else {
            // Mid-track, no events — update visual position and continue.
            position.sub_x = advance.sub_x;
            position.sub_y = advance.sub_y;
            position.refresh_screen_coords();
            return AdvanceResult::DriveTrackActive;
        }
    } else {
        let lepton_step: SimFixed = effective_speed * dt;
        if target.move_dir_len > SIM_ZERO {
            let frac: SimFixed = lepton_step / target.move_dir_len;
            // When walking to subcell_dest (path exhausted), clamp so we
            // don't overshoot. Without this, frac > 1.0 makes the infantry
            // walk past the destination and off the cell.
            if frac >= SIM_ONE {
                if let Some(loco) = locomotor {
                    if let Some((dest_x, dest_y)) = loco.subcell_dest {
                        if target.next_index >= target.path.len() {
                            // Snap to destination — we'd overshoot this tick.
                            position.sub_x = dest_x;
                            position.sub_y = dest_y;
                            // Fall through below.
                            // The post-loop check will detect arrival and finish.
                        }
                    }
                }
                // For cell-to-cell movement, frac > 1.0 is normal — it means
                // the entity crossed a cell boundary, handled by the crossing loop.
                if target.next_index < target.path.len()
                    || locomotor.as_ref().and_then(|l| l.subcell_dest).is_none()
                {
                    position.sub_x += target.move_dir_x * frac;
                    position.sub_y += target.move_dir_y * frac;
                }
            } else {
                position.sub_x += target.move_dir_x * frac;
                position.sub_y += target.move_dir_y * frac;
            }
        }

        // Advance infantry wobble phase while walking.
        // Original engine: WalkLocomotionClass accumulates wobble each tick
        // via `wobble += 3.0 / (wobbleRate / turnRate)`.
        if category == EntityCategory::Infantry {
            if let Some(loco) = locomotor {
                // Seed phase from entity ID on first tick so group members
                // don't bob in sync — each starts at a different phase.
                if loco.infantry_wobble_phase == 0.0 {
                    loco.infantry_wobble_phase =
                        (entity_id.wrapping_mul(2654435761) & 0xFFFF) as f32 / 0xFFFF as f32
                            * std::f32::consts::TAU;
                }
                let dt_f32: f32 = dt.to_num::<f32>();
                loco.infantry_wobble_phase += super::INFANTRY_WOBBLE_RATE * dt_f32;
            }
        }
    }

    AdvanceResult::ReadyForCrossings
}

/// Output from the cell boundary crossing loop.
pub(super) struct CrossingOutput {
    /// If set, the caller must handle deferred occupancy outside the entity borrow.
    pub deferred_cell_check: Option<DeferredCellCheck>,
    /// Bridge render state to apply after the loop.
    pub pending_bridge_update: Option<Option<u8>>,
    /// The resolved movement layer after all crossings.
    pub active_layer: MovementLayer,
    /// Debug events accumulated during crossing checks.
    pub debug_events: Vec<(u32, DebugEventKind)>,
    /// Whether the entity was marked as stuck and should abort.
    pub aborted_for_stuck: bool,
}

/// Process cell boundary crossings — the inner loop that checks whether
/// sub_x/sub_y have crossed cell boundaries, validates terrain walkability,
/// cliff height, and occupancy, then performs cell transitions with lepton
/// remainder carry-over.
///
/// Takes individual entity fields to avoid borrow conflicts with
/// `entity.movement_target` (which the caller holds as `ref mut target`).
#[allow(clippy::too_many_arguments)]
pub(super) fn process_cell_crossings(
    target: &mut MovementTarget,
    position: &mut Position,
    facing: &mut u8,
    facing_target: &mut Option<u8>,
    locomotor: &mut Option<LocomotorState>,
    drive_track_state: &mut Option<DriveTrackState>,
    sub_cell: &mut Option<u8>,
    category: EntityCategory,
    entity_id: u64,
    mut active_layer: MovementLayer,
    snap: &MoverSnapshot,
    path_grid: Option<&PathGrid>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
    entity_cost_grid: Option<&TerrainCostGrid>,
    mover_entity_blocks: Option<&BTreeSet<(u16, u16)>>,
    mover_entity_block_map: Option<&std::collections::HashMap<(u16, u16), crate::sim::pathfinding::EntityBlockEntry>>,
    occupancy: &mut OccupancyGrid,
    stats: &mut MovementTickStats,
    finished_entities: &mut Vec<u64>,
    rng: &mut SimRng,
    ctx: PathfindingContext<'_>,
    mcfg: MovementConfig,
    sim_tick: u64,
) -> CrossingOutput {
    let mut debug_events: Vec<(u32, DebugEventKind)> = Vec::new();
    let mut deferred_cell_check: Option<DeferredCellCheck> = None;
    let mut pending_bridge_update: Option<Option<u8>> = None;
    let mut aborted_for_stuck: bool = false;

    loop {
        if target.next_index >= target.path.len() {
            break;
        }
        let old_rx = position.rx;
        let old_ry = position.ry;
        let (nx, ny): (u16, u16) = target.path[target.next_index];
        let dx_cell: i32 = nx as i32 - position.rx as i32;
        let dy_cell: i32 = ny as i32 - position.ry as i32;

        // Check if sub_x/sub_y have crossed cell boundaries on each axis.
        let crossed_x: bool = match dx_cell.signum() {
            1 => position.sub_x >= crate::util::lepton::LEPTONS_PER_CELL,
            -1 => position.sub_x <= SIM_ZERO,
            _ => true, // No X movement needed for this step.
        };
        let crossed_y: bool = match dy_cell.signum() {
            1 => position.sub_y >= crate::util::lepton::LEPTONS_PER_CELL,
            -1 => position.sub_y <= SIM_ZERO,
            _ => true,
        };
        if !(crossed_x && crossed_y) {
            break;
        }

        let mut next_layer = target.layer_at(target.next_index);
        let mut layer_grid_ok: Option<bool> = None;
        let mut layer_terrain_ok: Option<bool> = None;

        // --- Terrain walkability check (static map data) ---
        let layer_walkable = match next_layer {
            MovementLayer::Ground => {
                // Water movers (ships) bypass PathGrid — water cells are
                // marked non-walkable for land units but ships need them.
                // Use passability matrix directly, same as the pathfinder.
                let is_water_mover = snap.movement_zone.is_water_mover();
                let grid_ok: bool = if is_water_mover {
                    match (path_grid, resolved_terrain) {
                        (Some(grid), rt) => crate::sim::pathfinding::is_cell_passable_for_mover(
                            grid,
                            nx,
                            ny,
                            Some(snap.movement_zone),
                            rt,
                        ),
                        _ => true,
                    }
                } else {
                    path_grid.map_or(true, |grid| grid.is_walkable(nx, ny))
                };
                let terrain_ok: bool = target.ignore_terrain_cost
                    || is_water_mover
                    || entity_cost_grid.map_or(true, |cg| cg.cost_at(nx, ny) > 0);
                layer_grid_ok = Some(grid_ok);
                layer_terrain_ok = Some(terrain_ok);
                grid_ok && terrain_ok
            }
            MovementLayer::Bridge => path_grid.is_some_and(|grid| {
                if !grid.is_walkable_on_layer(nx, ny, MovementLayer::Bridge) {
                    return false;
                }
                // Bridgehead gate:
                // entering bridge from ground requires a bridgehead cell (0x200).
                // Already on bridge → any bridge cell is fine.
                if active_layer != MovementLayer::Bridge {
                    grid.can_enter_bridge_layer_from_ground(nx, ny)
                } else {
                    true
                }
            }),
            MovementLayer::Air | MovementLayer::Underground => false,
        };
        if !layer_walkable {
            if snap.movement_zone.is_water_mover() {
                log::info!(
                    "NAVAL transition blocked: entity={} cur=({},{}) next=({},{}) layer={:?} grid_ok={:?} terrain_ok={:?} blocked_delay={} path_blocked={} {}",
                    entity_id,
                    position.rx,
                    position.ry,
                    nx,
                    ny,
                    next_layer,
                    layer_grid_ok,
                    layer_terrain_ok,
                    target.blocked_delay,
                    target.path_blocked,
                    naval_terrain_diag(resolved_terrain, (nx, ny)),
                );
            }
            // Undo lepton advancement — entity stays at cell center.
            position.sub_x = crate::util::lepton::CELL_CENTER_LEPTON;
            position.sub_y = crate::util::lepton::CELL_CENTER_LEPTON;
            *drive_track_state = None;
            // Terrain-blocked (building/cliff) — the path is stale.
            // Force immediate repath by clearing movement_delay.
            target.movement_delay = 0;
            let mover_is_crusher = snap.omni_crusher
                || matches!(
                    snap.locomotor.as_ref().map(|l| l.movement_zone),
                    Some(crate::rules::locomotor_type::MovementZone::Crusher | crate::rules::locomotor_type::MovementZone::AmphibiousCrusher | crate::rules::locomotor_type::MovementZone::CrusherAll)
                );
            let evts = handle_blocked_tick(
                target,
                facing,
                &snap.locomotor,
                entity_id,
                (position.rx, position.ry),
                active_layer,
                snap.on_bridge,
                stats,
                finished_entities,
                &mut aborted_for_stuck,
                ctx,
                entity_cost_grid,
                mover_entity_blocks,
                mover_entity_block_map,
                snap.too_big_to_fit_under_bridge,
                mcfg,
                rng,
                sim_tick,
                PATH_STUCK_INIT,
                mover_is_crusher,
                category == EntityCategory::Infantry,
            );
            debug_events.extend(evts);
            break;
        }

        // --- Cliff detection (Can_Enter_Cell code 6) ---
        // Original engine: if height difference >= 3 levels and not a
        // bridge ramp, treat as cliff. Catches stale paths after terrain
        // changes, bump/scatter toward cliff edges, etc.
        if let Some(pg) = path_grid {
            if let Some(next_cell) = pg.cell(nx, ny) {
                let next_level = next_cell.effective_cell_z_for_layer(next_layer);
                let diff = (position.z as i16 - next_level as i16).unsigned_abs();
                let is_bridge_ramp =
                    next_cell.is_bridge_transition_cell() || next_cell.is_elevated_bridge_cell();
                if diff >= CLIFF_HEIGHT_THRESHOLD && !is_bridge_ramp {
                    position.sub_x = crate::util::lepton::CELL_CENTER_LEPTON;
                    position.sub_y = crate::util::lepton::CELL_CENTER_LEPTON;
                    *drive_track_state = None;
                    target.movement_delay = 0;
                    let mover_is_crusher = snap.omni_crusher
                        || matches!(
                            snap.locomotor.as_ref().map(|l| l.movement_zone),
                            Some(crate::rules::locomotor_type::MovementZone::Crusher | crate::rules::locomotor_type::MovementZone::AmphibiousCrusher | crate::rules::locomotor_type::MovementZone::CrusherAll)
                        );
                    let evts = handle_blocked_tick(
                        target,
                        facing,
                        &snap.locomotor,
                        entity_id,
                        (position.rx, position.ry),
                        active_layer,
                        snap.on_bridge,
                        stats,
                        finished_entities,
                        &mut aborted_for_stuck,
                        ctx,
                        entity_cost_grid,
                        mover_entity_blocks,
                        mover_entity_block_map,
                        snap.too_big_to_fit_under_bridge,
                        mcfg,
                        rng,
                        sim_tick,
                        PATH_STUCK_INIT,
                        mover_is_crusher,
                        category == EntityCategory::Infantry,
                    );
                    debug_events.extend(evts);
                    break;
                }
            }
        }

        // --- Occupancy check (entity-aware: sub-cell, crush, bump) ---
        // Occupancy check: vehicles defer to crush/bump/attack handler,
        // infantry defer to sub-cell/attack handler. Both break out of the
        // loop to release the mutable entity borrow for blocker lookups.
        if let Some(check) = detect_deferred_cell_check(
            snap.category,
            next_layer,
            (nx, ny),
            (position.rx, position.ry),
            active_layer,
            occupancy,
        ) {
            deferred_cell_check = Some(check);
            break;
        }

        // --- Cell transition: carry over lepton remainder ---
        // Only adjust the axes that actually crossed a boundary.
        // Do NOT snap the perpendicular axis to center — that causes
        // a visible position jump when transitioning from diagonal
        // to cardinal movement (e.g., sub_x=51 → 128 = ~9px snap).
        apply_cell_transition_remainder(target, position, dx_cell, dy_cell, nx, ny, category == EntityCategory::Infantry);
        // Update occupancy grid: move entity from old cell to new cell.
        // Uses current sub_cell (from old cell). For infantry, reserve_destination
        // below may allocate a new sub-cell and correct it via update_sub_cell.
        occupancy.move_entity(
            old_rx, old_ry, nx, ny,
            entity_id, active_layer, *sub_cell,
        );
        // Bridge/layer resolution stays in one helper so cell transitions
        // don't duplicate deck/ground height rules across the tick loop.
        let (resolved_layer, bridge_update) = resolve_cell_transition_bridge_state(
            position,
            path_grid,
            next_layer,
            nx,
            ny,
            entity_id,
            "cell_crossing",
        );
        next_layer = resolved_layer;
        pending_bridge_update = bridge_update;
        active_layer = next_layer;
        if let Some(loco) = locomotor {
            loco.layer = next_layer;
        }
        if !reserve_destination_after_transition(
            category,
            locomotor,
            drive_track_state,
            position,
            sub_cell,
            target,
            next_layer,
            nx,
            ny,
            occupancy,
            rng,
        ) {
            break;
        }
        // After reservation, infantry sub_cell may have changed.
        if category == EntityCategory::Infantry {
            occupancy.update_sub_cell(nx, ny, entity_id, *sub_cell);
        }
        stats.moved_steps = stats.moved_steps.saturating_add(1);

        configure_motion_after_transition(
            target,
            locomotor,
            drive_track_state,
            facing,
            facing_target,
            category,
            snap.rot,
            (nx, ny),
            (position.sub_x, position.sub_y),
        );

        // Pre-allocate subcell in the NEXT path cell for infantry direction targeting.
        // FindSubCellDest reserves a subcell in the destination cell before walking,
        // so each infantry targets its own subcell position based on the destination
        // cell's occupancy rather than carrying the current cell's.
        if category == EntityCategory::Infantry && target.next_index < target.path.len() {
            let next_cell = target.path[target.next_index];
            if let Some(pre_sub) = bump_crush::allocate_sub_cell_with_preference(
                occupancy.get(next_cell.0, next_cell.1),
                active_layer,
                None,
                position.sub_x,
                position.sub_y,
                rng,
            ) {
                let (sc_x, sc_y) = crate::util::lepton::subcell_lepton_offset(Some(pre_sub));
                if let Some(loco) = locomotor {
                    loco.subcell_dest = Some((sc_x, sc_y));
                }
                // Recompute direction toward the destination cell's subcell.
                let ndx = next_cell.0 as i32 - nx as i32;
                let ndy = next_cell.1 as i32 - ny as i32;
                let dest_x = SimFixed::from_num(ndx * 256) + sc_x;
                let dest_y = SimFixed::from_num(ndy * 256) + sc_y;
                let dx = dest_x - position.sub_x;
                let dy = dest_y - position.sub_y;
                target.move_dir_x = dx;
                target.move_dir_y = dy;
                target.move_dir_len = fixed_distance(dx, dy);
            }
        }
    }

    CrossingOutput {
        deferred_cell_check,
        pending_bridge_update,
        active_layer,
        debug_events,
        aborted_for_stuck,
    }
}
