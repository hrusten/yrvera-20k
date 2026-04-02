//! Ground movement tick — the per-tick state machine for all ground/bridge entities.
//!
//! Contains the main `tick_movement_with_grids()` function which processes every
//! entity that has a `MovementTarget`: rotation, speed ramping, drive tracks,
//! cell boundary crossings, bridge transitions, deferred occupancy checks,
//! formation sync, and bump/crush resolution.
//!
//! This is the largest single function in the codebase (~1,300 lines) because
//! ground movement is irreducibly complex — the borrow checker constrains how
//! the per-entity loop can be decomposed, and the function already delegates to
//! 6 private submodules (movement_path, movement_blocked, movement_bridge,
//! movement_step, movement_reservation, movement_occupancy).
//!
//! ## Dependency rules
//! - Internal to sim/movement — called via re-export in mod.rs.

use std::collections::{BTreeMap, BTreeSet};

use crate::map::entities::EntityCategory;
use crate::map::houses::HouseAllianceMap;
use crate::map::resolved_terrain::ResolvedTerrainGrid;
use crate::rules::locomotor_type::{MovementZone, SpeedType};
use crate::sim::components::MovementTarget;
use crate::sim::debug_event_log::DebugEventKind;
use crate::sim::entity_store::EntityStore;
use crate::sim::pathfinding::PathGrid;
use crate::sim::pathfinding::terrain_cost::TerrainCostGrid;
use crate::sim::pathfinding::terrain_speed::{self, TerrainSpeedConfig};
use crate::sim::pathfinding::zone_map::{ZoneCategory, ZoneGrid};
use crate::sim::rng::SimRng;
use crate::util::fixed_math::{
    SIM_HALF, SIM_ONE, SIM_ZERO, SimFixed, dt_from_tick_ms, fixed_distance,
};

use super::bump_crush;
use crate::sim::occupancy::OccupancyGrid;
use super::locomotor::{GroundMovePhase, MovementLayer};
use super::movement_bridge::{
    BRIDGE_Z_OFFSET, apply_bridge_lookahead_if_needed, apply_pending_bridge_render_state,
};
use super::movement_occupancy::{DeferredCellCheck, handle_deferred_occupancy};
use super::movement_path::{find_move_path, supports_layered_bridge_pathing};
use super::movement_step;
use super::{
    INFANTRY_WOBBLE_AMPLITUDE, MIN_BRAKE_FRACTION, MovementConfig, MovementTickStats,
    MoverSnapshot, PATH_STUCK_INIT, PathfindingContext, facing_from_delta, walking_to_subcell_dest,
};

// Naval diagnostic functions moved to movement_occupancy.rs

/// Build a read-only snapshot of the mover's properties before entering the
/// inner movement loop. This avoids repeated `entities.get()` calls and keeps
/// the data available across the mutable/immutable borrow boundary.
fn snapshot_mover(entities: &EntityStore, entity_id: u64) -> Option<MoverSnapshot> {
    let e = entities.get(entity_id)?;
    Some(MoverSnapshot {
        category: e.category,
        speed_type: e.locomotor.as_ref().map(|l| l.speed_type),
        movement_zone: e
            .locomotor
            .as_ref()
            .map(|l| l.movement_zone)
            .unwrap_or(MovementZone::Normal),
        omni_crusher: e.omni_crusher,
        owner: e.owner,
        too_big_to_fit_under_bridge: e.too_big_to_fit_under_bridge,
        on_bridge: e.on_bridge,
        locomotor: e.locomotor.clone(),
        rot: e.locomotor.as_ref().map(|l| l.rot).unwrap_or(0),
    })
}

/// Result of path exhaustion check — tells the caller how to proceed.
enum PathExhaustionResult {
    /// Path is not yet exhausted — continue to rotation/movement.
    NotExhausted,
    /// Entity was repathed to the next segment — continue to rotation/movement.
    Repathed(Vec<(u32, DebugEventKind)>),
    /// Entity finished its path — caller should `continue` to next entity.
    Finished,
}

/// Check if the current path segment is exhausted and either repath to the next
/// 24-step segment toward the final goal, or mark the entity as finished.
///
/// Also handles the subcell redirect: when the path is exhausted but infantry is
/// still walking toward subcell_dest, redirects move_dir toward the destination.
///
/// Takes individual entity fields to avoid borrow conflicts.
#[allow(clippy::too_many_arguments)]
fn handle_path_exhaustion(
    target: &mut MovementTarget,
    locomotor: &Option<super::locomotor::LocomotorState>,
    position: &super::super::components::Position,
    category: EntityCategory,
    facing: &mut u8,
    facing_target: &mut Option<u8>,
    _entity_id: u64,
    active_layer: MovementLayer,
    snap: &MoverSnapshot,
    ctx: PathfindingContext<'_>,
    entity_cost_grid: Option<&TerrainCostGrid>,
    mover_entity_blocks: Option<&BTreeSet<(u16, u16)>>,
    path_delay_ticks: u16,
    sim_tick: u64,
) -> PathExhaustionResult {
    if target.next_index < target.path.len() {
        // Path not yet exhausted — check subcell redirect case and return.
        return PathExhaustionResult::NotExhausted;
    }

    // Path exhausted — check if at final goal.
    let at_final_goal: bool = target
        .final_goal
        .map_or(true, |fg| (position.rx, position.ry) == fg);
    if !at_final_goal {
        // Auto-repath: compute next 24-step segment toward final_goal.
        let fg = target.final_goal.unwrap(); // safe: at_final_goal was false
        let cur = (position.rx, position.ry);
        let layered_pathing_for_seg = snap
            .locomotor
            .as_ref()
            .zip(ctx.path_grid)
            .is_some_and(|(loco, pg)| supports_layered_bridge_pathing(loco, pg, snap.on_bridge));
        // DIAGNOSTIC: log segment repath when on bridge layer
        if active_layer == MovementLayer::Bridge {
            log::warn!(
                "BRIDGE_DIAG entity={}: path segment exhausted ON BRIDGE at ({},{}) z={} \
                 layered_pathing={} goal=({},{})",
                _entity_id,
                cur.0,
                cur.1,
                position.z,
                layered_pathing_for_seg,
                fg.0,
                fg.1,
            );
        }
        let seg_zone_cat = snap
            .locomotor
            .as_ref()
            .map(|l| ZoneCategory::from_movement_zone(l.movement_zone))
            .unwrap_or(ZoneCategory::Land);
        if ctx.path_grid.is_some() {
            if let Some((new_path, new_layers)) = find_move_path(
                ctx,
                layered_pathing_for_seg,
                cur,
                active_layer,
                fg,
                entity_cost_grid,
                mover_entity_blocks,
                None,
                None, // layer-separated entity blocks not yet wired
                seg_zone_cat,
                Some(snap.movement_zone),
                snap.too_big_to_fit_under_bridge,
            ) {
                if new_path.len() >= 2 {
                    // DIAGNOSTIC: detect layer mismatch after repath
                    if active_layer == MovementLayer::Bridge {
                        let has_bridge_step =
                            new_layers.iter().any(|l| *l == MovementLayer::Bridge);
                        if !has_bridge_step {
                            log::warn!(
                                "BRIDGE_DIAG entity={}: segment repath produced ALL-GROUND path \
                                 while on bridge! path_len={} — unit will fall through",
                                _entity_id,
                                new_path.len(),
                            );
                        } else {
                            let first_layer =
                                new_layers.get(1).copied().unwrap_or(MovementLayer::Ground);
                            log::info!(
                                "BRIDGE_DIAG entity={}: segment repath OK, first_layer={:?} path_len={}",
                                _entity_id,
                                first_layer,
                                new_path.len(),
                            );
                        }
                    }
                    let saved_speed = target.speed;
                    let saved_goal = target.final_goal;
                    let next = new_path[1];
                    let dx = next.0 as i32 - cur.0 as i32;
                    let dy = next.1 as i32 - cur.1 as i32;
                    let (d_x, d_y, d_len) = crate::util::lepton::cell_delta_to_lepton_dir(dx, dy);
                    // Preserve speed ramping state across segment repath —
                    // the unit is already moving, don't reset to zero.
                    let saved_current = target.current_speed;
                    let saved_accel = target.accel_factor;
                    let saved_decel = target.decel_factor;
                    let saved_slowdown = target.slowdown_distance;
                    let saved_group = target.group_id;
                    *target = MovementTarget {
                        path: new_path,
                        path_layers: new_layers,
                        next_index: 1,
                        speed: saved_speed,
                        current_speed: saved_current,
                        accel_factor: saved_accel,
                        decel_factor: saved_decel,
                        slowdown_distance: saved_slowdown,
                        move_dir_x: d_x,
                        move_dir_y: d_y,
                        move_dir_len: d_len,
                        movement_delay: path_delay_ticks,
                        blocked_delay: 0,
                        path_blocked: false,
                        path_stuck_counter: PATH_STUCK_INIT,
                        final_goal: saved_goal,
                        group_id: saved_group,
                        ignore_terrain_cost: false,
                    };
                    debug_assert_eq!(
                        target.path.len(),
                        target.path_layers.len(),
                        "path/path_layers desync after segment repath"
                    );
                    // Update facing toward next cell.
                    let new_face: u8 = facing_from_delta(dx, dy);
                    if category == EntityCategory::Infantry || snap.rot <= 0 {
                        *facing = new_face;
                    } else {
                        *facing_target = Some(new_face);
                    }
                    // Continue processing this entity on the new segment.
                    let mut debug_events = Vec::new();
                    debug_events.push((
                        sim_tick as u32,
                        DebugEventKind::Repath {
                            reason: "path segment exhausted".into(),
                            new_path_len: target.path.len(),
                        },
                    ));
                    // After repath, also apply subcell redirect if path is now exhausted
                    // (shouldn't happen with len>=2, but be safe).
                    apply_subcell_redirect(target, locomotor, position);
                    return PathExhaustionResult::Repathed(debug_events);
                } else if !walking_to_subcell_dest(locomotor, position.sub_x, position.sub_y) {
                    return PathExhaustionResult::Finished;
                }
            } else if !walking_to_subcell_dest(locomotor, position.sub_x, position.sub_y) {
                return PathExhaustionResult::Finished;
            }
        } else if !walking_to_subcell_dest(locomotor, position.sub_x, position.sub_y) {
            return PathExhaustionResult::Finished;
        }
    } else if !walking_to_subcell_dest(locomotor, position.sub_x, position.sub_y) {
        return PathExhaustionResult::Finished;
    }

    // Path exhausted but subcell walk still active — redirect move_dir.
    apply_subcell_redirect(target, locomotor, position);
    PathExhaustionResult::NotExhausted
}

/// If path is exhausted but infantry is walking to subcell_dest, redirect
/// move_dir toward the destination so the lepton advancement walks the
/// right direction.
fn apply_subcell_redirect(
    target: &mut MovementTarget,
    locomotor: &Option<super::locomotor::LocomotorState>,
    position: &super::super::components::Position,
) {
    if target.next_index >= target.path.len() {
        if let Some(loco) = locomotor {
            if let Some((dest_x, dest_y)) = loco.subcell_dest {
                let dx: SimFixed = dest_x - position.sub_x;
                let dy: SimFixed = dest_y - position.sub_y;
                target.move_dir_x = dx;
                target.move_dir_y = dy;
                let len: SimFixed = fixed_distance(dx, dy);
                target.move_dir_len = if len > SIM_HALF { len } else { SIM_ONE };
            }
        }
    }
}

pub fn tick_movement_with_grids(
    entities: &mut EntityStore,
    path_grid: Option<&PathGrid>,
    terrain_costs: &BTreeMap<SpeedType, TerrainCostGrid>,
    alliances: &HouseAllianceMap,
    occupancy: &OccupancyGrid,
    rng: &mut SimRng,
    tick_ms: u32,
    sim_tick: u64,
    zone_grid: Option<&ZoneGrid>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
    terrain_speed_config: &TerrainSpeedConfig,
    close_enough: SimFixed,
    path_delay_ticks: u16,
    blockage_path_delay_ticks: u16,
    interner: &crate::sim::intern::StringInterner,
) -> MovementTickStats {
    let mut stats = MovementTickStats::default();
    if tick_ms == 0 {
        return stats;
    }
    let ctx = PathfindingContext {
        path_grid,
        zone_grid,
        resolved_terrain,
    };
    let mcfg = MovementConfig {
        close_enough,
        path_delay_ticks,
        blockage_path_delay_ticks,
    };
    let dt: SimFixed = dt_from_tick_ms(tick_ms);
    // Collect entities that have finished their paths (need movement_target removal after loop).
    let mut finished_entities: Vec<u64> = Vec::new();
    let mut reserved_destinations: BTreeSet<(MovementLayer, u16, u16)> = BTreeSet::new();
    // Per-cell sub-cell reservations for infantry. Tracks which sub-cell spots have
    // been claimed by earlier movers this tick, preventing duplicate sub-cell assignment
    // and allowing up to 3 infantry to enter the same cell without false blocking.
    let mut reserved_infantry_sub_cells: BTreeMap<(MovementLayer, u16, u16), Vec<u8>> =
        BTreeMap::new();
    // Deferred effects — applied after the movement loop to avoid borrow conflicts.
    let mut crush_kills: Vec<u64> = Vec::new();
    // Track which blockers have already been told to scatter this tick,
    // preventing duplicate scatter commands from multiple movers.
    let mut already_scattered: BTreeSet<u64> = BTreeSet::new();

    // Collect movers in deterministic order: ground/bridge entities with a movement_target.
    let keys = entities.keys_sorted();
    let mut movers: Vec<u64> = Vec::new();
    let mut mover_owners: BTreeSet<crate::sim::intern::InternedId> = BTreeSet::new();
    for &id in &keys {
        if let Some(entity) = entities.get(id) {
            if entity.movement_target.is_none() {
                continue;
            }
            let layer = entity.movement_layer_or_ground();
            if !matches!(layer, MovementLayer::Air | MovementLayer::Underground) {
                movers.push(id);
                mover_owners.insert(entity.owner);
            }
        }
    }
    // Pre-build entity block sets per owner for friendly-passable pathfinding during repath.
    // RA2 optimization: moving friendly units are passable; only stationary/enemy units block.
    // InternedId is Copy, so keys are trivially cheap.
    let entity_block_sets: BTreeMap<crate::sim::intern::InternedId, BTreeSet<(u16, u16)>> =
        mover_owners
            .iter()
            .map(|&owner_id| {
                let owner_str = interner.resolve(owner_id);
                let blocks =
                    bump_crush::build_entity_block_set(entities, owner_str, alliances, interner);
                (owner_id, blocks)
            })
            .collect();

    for entity_id in movers {
        stats.movers_total = stats.movers_total.saturating_add(1);

        // Snapshot mover data before entering the inner loop so we can release the
        // mutable borrow on `entities` when needed for crush/bump immutable lookups.
        let Some(snap) = snapshot_mover(entities, entity_id) else {
            continue;
        };
        let entity_cost_grid: Option<&TerrainCostGrid> =
            snap.speed_type.and_then(|st| terrain_costs.get(&st));
        let mover_entity_blocks: Option<&BTreeSet<(u16, u16)>> = entity_block_sets.get(&snap.owner);

        let aborted_for_stuck: bool;
        let mut active_layer: MovementLayer;
        let mut debug_events: Vec<(u32, DebugEventKind)> = Vec::new();
        let mut pending_bridge_update: Option<Option<u8>> = None;
        // Vehicle crush/bump needs immutable EntityStore access, which conflicts
        // with the mutable entity borrow. When detected, we save the target cell
        // and layer, break out of the while loop, release the borrow, then handle
        // the check in a separate scope below.
        let deferred_cell_check: Option<DeferredCellCheck>;
        let mut already_finished: bool = false;

        // Scoped mutable borrow of the entity — released at block end so the
        // vehicle crush/bump check below can do immutable EntityStore lookups.
        {
            let Some(entity) = entities.get_mut(entity_id) else {
                continue;
            };
            active_layer = entity.movement_layer_or_ground();
            let Some(ref mut target) = entity.movement_target else {
                continue;
            };
            target.movement_delay = target.movement_delay.saturating_sub(1);
            target.blocked_delay = target.blocked_delay.saturating_sub(1);

            match handle_path_exhaustion(
                target,
                &entity.locomotor,
                &entity.position,
                entity.category,
                &mut entity.facing,
                &mut entity.facing_target,
                entity_id,
                active_layer,
                &snap,
                ctx,
                entity_cost_grid,
                mover_entity_blocks,
                path_delay_ticks,
                sim_tick,
            ) {
                PathExhaustionResult::Finished => {
                    finished_entities.push(entity_id);
                    continue;
                }
                PathExhaustionResult::Repathed(evts) => {
                    debug_events.extend(evts);
                }
                PathExhaustionResult::NotExhausted => {}
            }

            // Vehicles rotate in place before moving (RA2 behavior).
            // If facing_target is set and not yet reached, rotate toward it and
            // skip lepton advancement this tick. ROT=0 means instant turn.
            if snap.category != EntityCategory::Infantry {
                match movement_step::handle_vehicle_rotation(
                    &mut entity.facing,
                    &mut entity.facing_target,
                    &mut entity.position,
                    &mut entity.locomotor,
                    snap.rot,
                    tick_ms,
                    sim_tick,
                ) {
                    movement_step::RotationResult::StillRotating { debug_events: evts } => {
                        debug_events.extend(evts);
                        continue;
                    }
                    movement_step::RotationResult::ReadyToMove => {}
                }
            }

            // Per-cell terrain speed modifier: terrain type + slope + crowd density.
            // Computed from the unit's current cell and next path step. Applied to
            // both drive-track and straight-line movement below.
            let cell_speed_mod: SimFixed = {
                let next_cell = target.path.get(target.next_index).copied();
                match (
                    resolved_terrain,
                    snap.speed_type,
                    &snap.locomotor,
                    next_cell,
                ) {
                    (Some(terrain), Some(st), Some(loco), Some(nc)) => {
                        terrain_speed::compute_cell_speed_modifier(
                            st,
                            loco.kind,
                            (entity.position.rx, entity.position.ry),
                            nc,
                            terrain,
                            occupancy,
                            terrain_speed_config,
                        )
                    }
                    _ => SIM_ONE,
                }
            };
            // Speed ramping: acceleration toward max speed, deceleration near goal.
            // Matches original engine's Process_Drive_Track speed computation.
            if target.accel_factor > SIM_ZERO || target.decel_factor > SIM_ZERO {
                let goal = target.final_goal.unwrap_or_else(|| {
                    target
                        .path
                        .last()
                        .copied()
                        .unwrap_or((entity.position.rx, entity.position.ry))
                });
                let dx = (goal.0 as i32 - entity.position.rx as i32).abs();
                let dy = (goal.1 as i32 - entity.position.ry as i32).abs();
                // Chebyshev distance in leptons; bridge Z offset added below for water movers.
                let mut dist = SimFixed::from_num(dx.max(dy) * 256);

                // Ships under bridges: inflate distance by bridge Z clearance to prevent
                // premature braking.
                if snap.movement_zone.is_water_mover() {
                    if let Some(cell) =
                        path_grid.and_then(|pg| pg.cell(entity.position.rx, entity.position.ry))
                    {
                        if cell.bridge_deck_level_if_any().is_some() {
                            dist += BRIDGE_Z_OFFSET;
                        }
                    }
                }

                if dist < target.slowdown_distance && target.slowdown_distance > SIM_ZERO {
                    // Within braking distance: decelerate, floor at 30% of max speed.
                    target.current_speed -= target.decel_factor;
                    let floor = target.speed * MIN_BRAKE_FRACTION;
                    if target.current_speed < floor {
                        target.current_speed = floor;
                    }
                } else if target.current_speed < target.speed {
                    // Below max speed: accelerate.
                    target.current_speed += target.accel_factor;
                    if target.current_speed > target.speed {
                        target.current_speed = target.speed;
                    }
                }
                // Clamp to non-negative.
                if target.current_speed < SIM_ZERO {
                    target.current_speed = SIM_ZERO;
                }
            } else {
                // No ramping data — constant speed fallback.
                target.current_speed = target.speed;
            }
            let effective_speed: SimFixed = target.current_speed * cell_speed_mod;

            // Advance sub_x/sub_y toward the next cell — either via drive track
            // (smooth curve) or straight-line lepton vector.
            match movement_step::advance_lepton_position(
                target,
                &mut entity.position,
                &mut entity.facing,
                &mut entity.facing_target,
                &mut entity.drive_track,
                &mut entity.locomotor,
                entity.category,
                effective_speed,
                dt,
                entity_id,
            ) {
                movement_step::AdvanceResult::DriveTrackActive => continue,
                movement_step::AdvanceResult::DriveTrackCellJump => {
                    // Drive track crossed a cell boundary at cell_cross_index.
                    // Perform the cell transition: update rx/ry, advance next_index,
                    // reserve destination, handle bridge state.
                    if target.next_index < target.path.len() {
                        let (nx, ny) = target.path[target.next_index];
                        let dx_cell = nx as i32 - entity.position.rx as i32;
                        let dy_cell = ny as i32 - entity.position.ry as i32;
                        // DIAGNOSTIC: detect same-cell layer transition in drive track path
                        if dx_cell == 0 && dy_cell == 0 {
                            let next_layer = target.layer_at(target.next_index);
                            log::warn!(
                                "BRIDGE_DIAG entity={}: DriveTrackCellJump same-cell step! \
                                 cell=({},{}) path_layer={:?} active_layer={:?} z={} \
                                 next_index={}/{}",
                                entity_id,
                                nx,
                                ny,
                                next_layer,
                                active_layer,
                                entity.position.z,
                                target.next_index,
                                target.path.len(),
                            );
                        }
                        // Update cell coordinates.
                        entity.position.rx = nx;
                        entity.position.ry = ny;
                        // Bridge/layer resolution.
                        if let Some(pg) = path_grid {
                            let next_layer = target.layer_at(target.next_index);
                            let (resolved_layer, bridge_update) =
                                super::movement_bridge::resolve_cell_transition_bridge_state(
                                    &mut entity.position,
                                    Some(pg),
                                    next_layer,
                                    nx,
                                    ny,
                                    entity_id,
                                    "drive_track_jump",
                                );
                            pending_bridge_update = bridge_update;
                            active_layer = resolved_layer;
                            if let Some(ref mut loco) = entity.locomotor {
                                loco.layer = resolved_layer;
                            }
                            entity.on_bridge = resolved_layer == MovementLayer::Bridge;
                        }
                        // Reserve destination cell.
                        super::movement_reservation::reserve_destination_after_transition(
                            entity.category,
                            &mut entity.locomotor,
                            &mut entity.drive_track,
                            &mut entity.position,
                            &mut entity.sub_cell,
                            target,
                            active_layer,
                            nx,
                            ny,
                            occupancy,
                            &mut reserved_infantry_sub_cells,
                            &mut reserved_destinations,
                            rng,
                        );
                        stats.moved_steps = stats.moved_steps.saturating_add(1);
                        // Advance next_index and update move_dir for after track finishes.
                        // Don't initiate a new drive track — current one is still active.
                        target.next_index += 1;
                        if target.next_index < target.path.len() {
                            let next = target.path[target.next_index];
                            let ndx = next.0 as i32 - nx as i32;
                            let ndy = next.1 as i32 - ny as i32;
                            let (d_x, d_y, d_len) =
                                crate::util::lepton::cell_delta_to_lepton_dir(ndx, ndy);
                            target.move_dir_x = d_x;
                            target.move_dir_y = d_y;
                            target.move_dir_len = d_len;
                        }
                        let _ = (dx_cell, dy_cell); // used above for position update
                    }
                    // Apply bridge state and screen coords, then continue to next tick.
                    super::movement_bridge::apply_pending_bridge_render_state(
                        &mut entity.locomotor,
                        &mut entity.bridge_occupancy,
                        &mut entity.on_bridge,
                        active_layer,
                        pending_bridge_update,
                        entity_id,
                    );
                    entity.position.refresh_screen_coords();
                    continue;
                }
                movement_step::AdvanceResult::DriveTrackChainReady => {
                    // Track reached chain_index — attempt to chain into a
                    // follow-on track curve. Check passability of the next
                    // cell in the path, select a new track if the direction
                    // changes, and replace the drive track state.
                    // If chaining fails, the current track continues normally.
                    if target.next_index < target.path.len() {
                        let cur_cell = target.path[target.next_index];
                        // Need at least one more path step after the current target.
                        if target.next_index + 1 < target.path.len() {
                            let after = target.path[target.next_index + 1];
                            let ndx = after.0 as i32 - cur_cell.0 as i32;
                            let ndy = after.1 as i32 - cur_cell.1 as i32;
                            let next_face = super::facing_from_delta(ndx, ndy);
                            let cur_face = entity.facing;
                            // Only chain if the direction changes (otherwise
                            // the current track finishes into straight movement).
                            if next_face != cur_face {
                                // Check if the next cell is walkable (simplified
                                // Can_Enter_Cell — terrain + not reserved).
                                let next_walkable =
                                    path_grid.map_or(true, |g| g.is_walkable(after.0, after.1));
                                let not_reserved = !reserved_destinations.contains(&(
                                    active_layer,
                                    after.0,
                                    after.1,
                                ));
                                if next_walkable && not_reserved {
                                    if let Some(sel) = super::drive_track::select_drive_track(
                                        cur_face, next_face, false,
                                    ) {
                                        let chain_dx = after.0 as i32 - entity.position.rx as i32;
                                        let chain_dy = after.1 as i32 - entity.position.ry as i32;
                                        if let Some(new_track) =
                                            super::drive_track::begin_drive_track(
                                                sel.raw_track_index,
                                                sel.flags,
                                                chain_dx,
                                                chain_dy,
                                            )
                                        {
                                            entity.drive_track = Some(new_track);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Whether chaining succeeded or not, continue to next tick.
                    // If chaining failed, the current track continues from
                    // where it was (point_index stays at chain_index).
                    continue;
                }
                movement_step::AdvanceResult::ReadyForCrossings => {}
            }

            // Check for cell boundary crossings and handle cell transitions.
            let crossing = movement_step::process_cell_crossings(
                target,
                &mut entity.position,
                &mut entity.facing,
                &mut entity.facing_target,
                &mut entity.locomotor,
                &mut entity.drive_track,
                &mut entity.sub_cell,
                entity.category,
                entity_id,
                active_layer,
                &snap,
                path_grid,
                resolved_terrain,
                entity_cost_grid,
                mover_entity_blocks,
                occupancy,
                &mut reserved_destinations,
                &mut reserved_infantry_sub_cells,
                &mut stats,
                &mut finished_entities,
                rng,
                ctx,
                mcfg,
                sim_tick,
            );
            deferred_cell_check = crossing.deferred_cell_check;
            pending_bridge_update = crossing.pending_bridge_update;
            active_layer = crossing.active_layer;
            debug_events.extend(crossing.debug_events);
            aborted_for_stuck = crossing.aborted_for_stuck;

            // Apply bridge layer state BEFORE computing screen position, so that
            // the render frame always sees consistent state. Without this, there's
            // a one-frame window where the unit is in the bridge cell but
            // bridge_occupancy is still None, causing the renderer to use ground
            // height interpolation and briefly dip the unit to water level.
            if !aborted_for_stuck
                && !matches!(deferred_cell_check, Some(DeferredCellCheck::Vehicle(_, _)))
            {
                apply_pending_bridge_render_state(
                    &mut entity.locomotor,
                    &mut entity.bridge_occupancy,
                    &mut entity.on_bridge,
                    active_layer,
                    pending_bridge_update,
                    entity_id,
                );
            }

            // Preemptive bridge detection: if the unit is approaching a bridge
            // cell on Bridge layer, set bridge_occupancy NOW so the renderer
            // skips ground-height interpolation. Only applies when the path
            // routes ON the bridge — not when walking under it on Ground layer.
            let bridge_lookahead = if target.next_index < target.path.len() {
                Some(target.path[target.next_index])
            } else {
                None
            };
            let lookahead_layer = target.layer_at(target.next_index);
            apply_bridge_lookahead_if_needed(
                &mut entity.position,
                &mut entity.bridge_occupancy,
                &mut entity.on_bridge,
                snap.movement_zone,
                bridge_lookahead,
                lookahead_layer,
                path_grid,
            );

            // DIAGNOSTIC: detect unexpected z-drop on bridge cells.
            // If bridge_occupancy is set but z is at ground level, something
            // cleared z without clearing bridge_occupancy (or vice versa).
            if let Some(ref bocc) = entity.bridge_occupancy {
                if entity.position.z + 2 < bocc.deck_level {
                    log::error!(
                        "BRIDGE_DIAG entity={}: Z BELOW DECK! z={} deck={} \
                         cell=({},{}) layer={:?} bridge_occ={:?}",
                        entity_id,
                        entity.position.z,
                        bocc.deck_level,
                        entity.position.rx,
                        entity.position.ry,
                        active_layer,
                        entity.bridge_occupancy,
                    );
                }
            }

            // Update screen position from lepton coordinates every tick.
            entity.position.refresh_screen_coords();

            // Z handling: Z snaps discretely at cell boundaries via
            // entity.position.z (set earlier in this tick). The original engine
            // does NOT interpolate Z during sub-cell movement; track delta Z is
            // explicitly zeroed.
            // Visual smoothness on slopes comes from the body tilt system (pitch/roll),
            // not from Z interpolation. Removing the Z lerp that was here fixes a bug
            // where units on bridges visually fell to water level every cell transition
            // (the lookahead read ground_level instead of bridge_deck_level).

            // Infantry walking bob: vertical sinusoidal bounce while moving.
            // Original engine: cos(wobble) applied to Z interpolation in
            // producing an up/down bob during walking states.
            // Applied to screen_y only — doesn't affect sim determinism.
            if entity.category == EntityCategory::Infantry {
                if let Some(ref loco) = entity.locomotor {
                    if loco.infantry_wobble_phase != 0.0 {
                        let bob = loco.infantry_wobble_phase.cos() * INFANTRY_WOBBLE_AMPLITUDE;
                        // Negative = up in screen space (lower Y = higher on screen)
                        entity.position.screen_y -= bob;
                    }
                }
            }

            // Post-loop finalization (still inside mutable borrow scope).
            if !aborted_for_stuck
                && !matches!(deferred_cell_check, Some(DeferredCellCheck::Vehicle(_, _)))
            {
                if target.next_index >= target.path.len() {
                    let at_final: bool = target
                        .final_goal
                        .map_or(true, |fg| (entity.position.rx, entity.position.ry) == fg);
                    if at_final
                        && !walking_to_subcell_dest(
                            &entity.locomotor,
                            entity.position.sub_x,
                            entity.position.sub_y,
                        )
                    {
                        finished_entities.push(entity_id);
                        already_finished = true;
                    }
                }
            }
        } // mutable entity borrow released here

        if aborted_for_stuck || already_finished {
            continue;
        }

        // --- Deferred occupancy check (unified vehicle + infantry) ---
        // Runs outside the mutable entity borrow so classify_occupied_cell()
        // can do immutable EntityStore lookups for blocker properties.
        if let Some(check) = deferred_cell_check {
            let occ_evts = handle_deferred_occupancy(
                entities,
                check,
                entity_id,
                &snap,
                active_layer,
                ctx,
                mcfg,
                entity_cost_grid,
                mover_entity_blocks,
                occupancy,
                alliances,
                path_grid,
                resolved_terrain,
                rng,
                &mut stats,
                &mut finished_entities,
                &mut crush_kills,
                &mut already_scattered,
                &reserved_destinations,
                blockage_path_delay_ticks,
                sim_tick,
                interner,
            );
            debug_events.extend(occ_evts);
        }

        // Push deferred debug events onto the entity now that all borrows are released.
        if !debug_events.is_empty() {
            if let Some(entity) = entities.get_mut(entity_id) {
                for (tick, kind) in debug_events.drain(..) {
                    entity.push_debug_event(tick, kind);
                }
            }
        }
    }

    sync_formation_speeds(entities);

    // Apply deferred crush kills (instant death, then remove from EntityStore).
    for &victim_id in &crush_kills {
        if let Some(victim) = entities.get_mut(victim_id) {
            victim.health.current = 0;
        }
        entities.remove(victim_id);
        stats.crush_kills = stats.crush_kills.saturating_add(1);
    }

    finalize_finished_entities(entities, &finished_entities, sim_tick);
    update_locomotor_phases(entities, sim_tick);

    stats
}

// ---------------------------------------------------------------------------
// Post-loop helpers — extracted from tick_movement_with_grids
// ---------------------------------------------------------------------------

/// Formation speed sync (deep_113 lines 451-456).
/// Cap grouped units to the slowest member's max speed so formations stay
/// together instead of faster units pulling ahead.
fn sync_formation_speeds(entities: &mut EntityStore) {
    let mut group_min_speed: BTreeMap<u32, SimFixed> = BTreeMap::new();
    for entity in entities.values() {
        if let Some(ref mt) = entity.movement_target {
            if let Some(gid) = mt.group_id {
                let entry = group_min_speed.entry(gid).or_insert(mt.speed);
                if mt.speed < *entry {
                    *entry = mt.speed;
                }
            }
        }
    }
    if !group_min_speed.is_empty() {
        for entity in entities.values_mut() {
            if let Some(ref mut mt) = entity.movement_target {
                if let Some(gid) = mt.group_id {
                    if let Some(&min_spd) = group_min_speed.get(&gid) {
                        if mt.speed > min_spd {
                            mt.speed = min_spd;
                        }
                    }
                }
            }
        }
    }
}

/// Remove movement targets from finished entities, reset sub-cell to final
/// position, and transition locomotor to Idle.
fn finalize_finished_entities(entities: &mut EntityStore, finished: &[u64], sim_tick: u64) {
    for &entity_id in finished {
        if let Some(entity) = entities.get_mut(entity_id) {
            entity.movement_target = None;
            entity.drive_track = None; // clear any active drive track curve
            // Snap sub-cell leptons to final position. Use the locomotor's
            // subcell_dest if available (set during cell entry), otherwise fall
            // back to computing from sub_cell index. Vehicles snap to center.
            let (snap_x, snap_y) = entity
                .locomotor
                .as_ref()
                .and_then(|l| l.subcell_dest)
                .unwrap_or_else(|| crate::util::lepton::subcell_lepton_offset(entity.sub_cell));
            entity.position.sub_x = snap_x;
            entity.position.sub_y = snap_y;
            entity.position.refresh_screen_coords();
            let old_phase = entity.locomotor.as_ref().map(|l| l.phase);
            if let Some(ref mut loco) = entity.locomotor {
                loco.phase = GroundMovePhase::Idle;
                loco.infantry_wobble_phase = 0.0;
                loco.subcell_dest = None;
            }
            if let Some(old) = old_phase {
                if old != GroundMovePhase::Idle {
                    entity.push_debug_event(
                        sim_tick as u32,
                        DebugEventKind::PhaseChange {
                            from: format!("{:?}", old),
                            to: "Idle".into(),
                            reason: "movement complete".into(),
                        },
                    );
                }
            }
        }
    }
}

/// Update locomotor phases for all active movers — 7-state mapping.
/// Maps the current movement state to the appropriate WalkLocomotionClass state.
fn update_locomotor_phases(entities: &mut EntityStore, sim_tick: u64) {
    let all_keys = entities.keys_sorted();
    for &id in &all_keys {
        if let Some(entity) = entities.get_mut(id) {
            // Compute new phase and capture old phase in a scoped block to release
            // borrows before calling push_debug_event.
            let phase_change: Option<(GroundMovePhase, GroundMovePhase, &'static str)> = {
                if let (Some(target), Some(loco)) = (&entity.movement_target, &mut entity.locomotor)
                {
                    let old_phase = loco.phase;
                    let (new_phase, reason) = if target.path_blocked {
                        (GroundMovePhase::Blocked, "cell blocked")
                    } else if target.current_speed <= SIM_ZERO {
                        // Speed is zero but path remains — stopping or waiting to start.
                        (GroundMovePhase::Stopping, "decelerating to stop")
                    } else if target.current_speed < target.speed * MIN_BRAKE_FRACTION {
                        // Below 30% of max speed — still accelerating from rest.
                        (GroundMovePhase::Accelerating, "reached cruise speed")
                    } else if target.current_speed >= target.speed {
                        // At or above max speed — cruising.
                        (GroundMovePhase::Cruising, "reached cruise speed")
                    } else {
                        // Between 30% and max — path following with speed ramping.
                        (GroundMovePhase::PathFollow, "approaching next cell")
                    };
                    loco.phase = new_phase;
                    if old_phase != new_phase {
                        Some((old_phase, new_phase, reason))
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some((old, new, reason)) = phase_change {
                entity.push_debug_event(
                    sim_tick as u32,
                    DebugEventKind::PhaseChange {
                        from: format!("{:?}", old),
                        to: format!("{:?}", new),
                        reason: reason.into(),
                    },
                );
            }
        }
    }
}
