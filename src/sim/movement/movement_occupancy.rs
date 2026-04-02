//! Movement occupancy resolution — deferred cell entry checks for crush, scatter,
//! and infantry sub-cell allocation.
//!
//! When the movement tick detects that the next cell is occupied, it defers the resolution
//! to this module (outside the mutable entity borrow) so that immutable EntityStore lookups
//! can classify blockers and decide between crush, scatter, attack, or wait.

use std::collections::{BTreeMap, BTreeSet};

use crate::map::entities::EntityCategory;
use crate::map::houses::HouseAllianceMap;
use crate::map::resolved_terrain::ResolvedTerrainGrid;
use crate::rules::locomotor_type::LocomotorKind;
use crate::sim::combat::AttackTarget;
use crate::sim::components::Position;
use crate::sim::debug_event_log::DebugEventKind;
use crate::sim::entity_store::EntityStore;
use crate::sim::movement::bump_crush;
use crate::sim::occupancy::OccupancyGrid;
use crate::sim::movement::drive_track::DriveTrackState;
use crate::sim::movement::locomotor::MovementLayer;
use crate::sim::movement::movement_blocked::handle_blocked_tick;
use crate::sim::pathfinding::PathGrid;
use crate::sim::pathfinding::cell_entry::{self, CellEntryResult};
use crate::sim::pathfinding::terrain_cost::TerrainCostGrid;
use crate::sim::rng::SimRng;

use super::{
    MovementConfig, MovementTickStats, MoverSnapshot, PATH_STUCK_INIT, PathfindingContext,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeferredCellCheck {
    Infantry((u16, u16), MovementLayer),
    Vehicle((u16, u16), MovementLayer),
}

pub(super) fn detect_deferred_cell_check(
    mover_category: EntityCategory,
    next_layer: MovementLayer,
    next_cell: (u16, u16),
    current_cell: (u16, u16),
    active_layer: MovementLayer,
    occupancy: &OccupancyGrid,
    reserved_infantry_sub_cells: &BTreeMap<(MovementLayer, u16, u16), Vec<u8>>,
) -> Option<DeferredCellCheck> {
    let is_self_cell =
        (next_cell.0, next_cell.1, next_layer) == (current_cell.0, current_cell.1, active_layer);
    if is_self_cell {
        return None;
    }

    let cell_occ = occupancy.get(next_cell.0, next_cell.1);
    if mover_category == EntityCategory::Infantry {
        let reserved = reserved_infantry_sub_cells
            .get(&(next_layer, next_cell.0, next_cell.1))
            .map(Vec::as_slice);
        if bump_crush::allocate_sub_cell_with_reserved(cell_occ, next_layer, reserved).is_none() {
            return Some(DeferredCellCheck::Infantry(next_cell, next_layer));
        }
    } else if cell_occ.is_some_and(|o| o.has_blockers_on(next_layer) || o.infantry(next_layer).next().is_some()) {
        return Some(DeferredCellCheck::Vehicle(next_cell, next_layer));
    }

    None
}

pub(super) fn snap_motion_to_cell_center(
    position: &mut Position,
    drive_track: &mut Option<DriveTrackState>,
) {
    position.sub_x = crate::util::lepton::CELL_CENTER_LEPTON;
    position.sub_y = crate::util::lepton::CELL_CENTER_LEPTON;
    *drive_track = None;
}

pub(super) fn naval_terrain_diag(
    terrain: Option<&ResolvedTerrainGrid>,
    cell: (u16, u16),
) -> String {
    let Some(terrain) = terrain else {
        return "terrain=<none>".into();
    };
    let Some(t) = terrain.cell(cell.0, cell.1) else {
        return format!("terrain=OOB({},{})", cell.0, cell.1);
    };
    format!(
        "terrain[water={} land_type={} cliff={} overlay_blocks={} terrain_blocks={} bridge_walkable={} bridge_deck={} level={}]",
        t.is_water,
        t.land_type,
        t.is_cliff_like,
        t.overlay_blocks,
        t.terrain_object_blocks,
        t.bridge_walkable,
        t.bridge_deck_level,
        t.level,
    )
}

pub(super) fn naval_occ_diag(occupancy: &OccupancyGrid, layer: MovementLayer, cell: (u16, u16)) -> String {
    match occupancy.get(cell.0, cell.1) {
        Some(occ) => format!(
            "occ[blockers={} infantry={}]",
            occ.blockers(layer).count(),
            occ.infantry(layer).count(),
        ),
        None => "occ[empty]".into(),
    }
}

/// Handle the deferred occupancy check — runs outside the mutable entity borrow
/// so `classify_occupied_cell()` can do immutable EntityStore lookups for blocker
/// properties. Returns debug events to be pushed onto the entity.
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_deferred_occupancy(
    entities: &mut EntityStore,
    check: DeferredCellCheck,
    entity_id: u64,
    snap: &MoverSnapshot,
    active_layer: MovementLayer,
    ctx: PathfindingContext<'_>,
    mcfg: MovementConfig,
    entity_cost_grid: Option<&TerrainCostGrid>,
    mover_entity_blocks: Option<&BTreeSet<(u16, u16)>>,
    occupancy: &OccupancyGrid,
    alliances: &HouseAllianceMap,
    path_grid: Option<&PathGrid>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
    rng: &mut SimRng,
    stats: &mut MovementTickStats,
    finished_entities: &mut Vec<u64>,
    crush_kills: &mut Vec<u64>,
    already_scattered: &mut BTreeSet<u64>,
    reserved_destinations: &BTreeSet<(MovementLayer, u16, u16)>,
    blockage_path_delay_ticks: u16,
    sim_tick: u64,
    interner: &crate::sim::intern::StringInterner,
) -> Vec<(u32, DebugEventKind)> {
    let mut debug_events: Vec<(u32, DebugEventKind)> = Vec::new();
    let (nx, ny, next_layer) = match check {
        DeferredCellCheck::Infantry((nx, ny), layer)
        | DeferredCellCheck::Vehicle((nx, ny), layer) => (nx, ny, layer),
    };
    let mover_loco_kind = snap
        .locomotor
        .as_ref()
        .map_or(LocomotorKind::Drive, |l| l.kind);
    let entry_result = cell_entry::classify_occupied_cell(
        (nx, ny),
        next_layer,
        entity_id,
        snap.movement_zone,
        snap.omni_crusher,
        interner.resolve(snap.owner),
        mover_loco_kind,
        occupancy,
        entities,
        alliances,
        interner,
    );
    if snap.movement_zone.is_water_mover() {
        log::info!(
            "NAVAL occupancy block: entity={} cur=({},{}) next=({},{}) layer={:?} result={:?} blocked_delay={} path_blocked={} {} {}",
            entity_id,
            entities.get(entity_id).map(|e| e.position.rx).unwrap_or(nx),
            entities.get(entity_id).map(|e| e.position.ry).unwrap_or(ny),
            nx,
            ny,
            next_layer,
            entry_result,
            entities
                .get(entity_id)
                .and_then(|e| e.movement_target.as_ref())
                .map(|mt| mt.blocked_delay)
                .unwrap_or(0),
            entities
                .get(entity_id)
                .and_then(|e| e.movement_target.as_ref())
                .map(|mt| mt.path_blocked)
                .unwrap_or(false),
            naval_terrain_diag(resolved_terrain, (nx, ny)),
            naval_occ_diag(occupancy, next_layer, (nx, ny)),
        );
    }

    match entry_result {
        CellEntryResult::Clear | CellEntryResult::BridgeRamp => {
            // Locomotor override (JumpJet) cleared the block.
            if let Some(entity) = entities.get_mut(entity_id) {
                snap_motion_to_cell_center(&mut entity.position, &mut entity.drive_track);
                if let Some(ref mut target) = entity.movement_target {
                    target.blocked_delay = 0;
                    target.path_blocked = false;
                }
            }
        }
        CellEntryResult::Crushable { victims } => {
            crush_kills.extend(victims);
            if let Some(entity) = entities.get_mut(entity_id) {
                snap_motion_to_cell_center(&mut entity.position, &mut entity.drive_track);
                if let Some(ref mut target) = entity.movement_target {
                    target.blocked_delay = 0;
                    target.path_blocked = false;
                }
            }
        }
        CellEntryResult::OccupiedFriendly { blocker_id } => {
            // Scatter the stationary friendly blocker out of the way.
            // Matches original engine: CellClass::Scatter_Objects with force=1
            // tells the BLOCKER to move, not the mover. The blocker receives a
            // movement command to walk to an adjacent cell.
            let mut scattered = false;
            if !already_scattered.contains(&blocker_id) {
                scattered = bump_crush::scatter_blocker(
                    entities,
                    blocker_id,
                    path_grid,
                    occupancy,
                    reserved_destinations,
                    next_layer,
                    rng,
                );
                if scattered {
                    already_scattered.insert(blocker_id);
                    stats.scatter_successes = stats.scatter_successes.saturating_add(1);
                }
            }
            // Mover waits — blocker is walking away. If scatter failed,
            // fall through to handle_blocked_tick for repath.
            if let Some(entity) = entities.get_mut(entity_id) {
                snap_motion_to_cell_center(&mut entity.position, &mut entity.drive_track);
                let cur_pos = (entity.position.rx, entity.position.ry);
                if let Some(ref mut target) = entity.movement_target {
                    if scattered {
                        // Blocker is moving — treat as temporary block, start
                        // a short wait before repath so the blocker has time to clear.
                        if !target.path_blocked {
                            target.path_blocked = true;
                            target.blocked_delay = blockage_path_delay_ticks;
                        }
                    } else {
                        let mut aborted_for_stuck = false;
                        let evts = handle_blocked_tick(
                            target,
                            &mut entity.facing,
                            &snap.locomotor,
                            entity_id,
                            cur_pos,
                            active_layer,
                            snap.on_bridge,
                            stats,
                            finished_entities,
                            &mut aborted_for_stuck,
                            ctx,
                            reserved_destinations,
                            entity_cost_grid,
                            mover_entity_blocks,
                            snap.too_big_to_fit_under_bridge,
                            mcfg,
                            rng,
                            sim_tick,
                            PATH_STUCK_INIT,
                        );
                        debug_events.extend(evts);
                    }
                }
            }
        }
        CellEntryResult::OccupiedEnemy { blocker_id } => {
            // Code 5: Attack blocker while waiting.
            if let Some(entity) = entities.get_mut(entity_id) {
                snap_motion_to_cell_center(&mut entity.position, &mut entity.drive_track);
                if entity.attack_target.is_none() {
                    entity.attack_target = Some(AttackTarget::new(blocker_id));
                }
                let cur_pos = (entity.position.rx, entity.position.ry);
                if let Some(ref mut target) = entity.movement_target {
                    let mut aborted_for_stuck = false;
                    let evts = handle_blocked_tick(
                        target,
                        &mut entity.facing,
                        &snap.locomotor,
                        entity_id,
                        cur_pos,
                        active_layer,
                        snap.on_bridge,
                        stats,
                        finished_entities,
                        &mut aborted_for_stuck,
                        ctx,
                        reserved_destinations,
                        entity_cost_grid,
                        mover_entity_blocks,
                        snap.too_big_to_fit_under_bridge,
                        mcfg,
                        rng,
                        sim_tick,
                        PATH_STUCK_INIT,
                    );
                    debug_events.extend(evts);
                }
            }
        }
        CellEntryResult::TemporaryBlock { blocker_id } => {
            // Moving friendly — wait, then scatter the BLOCKER.
            // Original engine: locomotor calls CellClass::Scatter_Objects with
            // force=1 regardless of whether blocker is moving or stationary.
            // The blocker is told to scatter; the mover waits.
            if let Some(entity) = entities.get_mut(entity_id) {
                snap_motion_to_cell_center(&mut entity.position, &mut entity.drive_track);
                if let Some(ref mut target) = entity.movement_target {
                    if !target.path_blocked {
                        target.path_blocked = true;
                        target.blocked_delay = blockage_path_delay_ticks;
                    }
                    if target.blocked_delay > 0 {
                        // Still waiting — do nothing this tick.
                    } else {
                        // Wait expired — try scattering the blocker, then repath.
                        if !already_scattered.contains(&blocker_id) {
                            let scattered = bump_crush::scatter_blocker(
                                entities,
                                blocker_id,
                                path_grid,
                                occupancy,
                                reserved_destinations,
                                next_layer,
                                rng,
                            );
                            if scattered {
                                already_scattered.insert(blocker_id);
                                stats.scatter_successes = stats.scatter_successes.saturating_add(1);
                            }
                        }
                        // Whether scatter succeeded or not, repath the mover.
                        // Re-borrow the mover since scatter_blocker released it.
                        if let Some(entity) = entities.get_mut(entity_id) {
                            let cur_pos = (entity.position.rx, entity.position.ry);
                            if let Some(ref mut target) = entity.movement_target {
                                let mut aborted_for_stuck = false;
                                let evts = handle_blocked_tick(
                                    target,
                                    &mut entity.facing,
                                    &snap.locomotor,
                                    entity_id,
                                    cur_pos,
                                    active_layer,
                                    snap.on_bridge,
                                    stats,
                                    finished_entities,
                                    &mut aborted_for_stuck,
                                    ctx,
                                    reserved_destinations,
                                    entity_cost_grid,
                                    mover_entity_blocks,
                                    snap.too_big_to_fit_under_bridge,
                                    mcfg,
                                    rng,
                                    sim_tick,
                                    PATH_STUCK_INIT,
                                );
                                debug_events.extend(evts);
                            }
                        }
                    }
                }
            }
        }
        CellEntryResult::Cliff | CellEntryResult::Impassable => {
            // Shouldn't reach here from NeedsBlockerCheck, but handle gracefully.
            if let Some(entity) = entities.get_mut(entity_id) {
                snap_motion_to_cell_center(&mut entity.position, &mut entity.drive_track);
                let cur_pos = (entity.position.rx, entity.position.ry);
                if let Some(ref mut target) = entity.movement_target {
                    let mut aborted_for_stuck = false;
                    let evts = handle_blocked_tick(
                        target,
                        &mut entity.facing,
                        &snap.locomotor,
                        entity_id,
                        cur_pos,
                        active_layer,
                        snap.on_bridge,
                        stats,
                        finished_entities,
                        &mut aborted_for_stuck,
                        ctx,
                        reserved_destinations,
                        entity_cost_grid,
                        mover_entity_blocks,
                        snap.too_big_to_fit_under_bridge,
                        mcfg,
                        rng,
                        sim_tick,
                        PATH_STUCK_INIT,
                    );
                    debug_events.extend(evts);
                }
            }
        }
    }

    debug_events
}
