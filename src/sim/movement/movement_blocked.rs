//! Blocked movement handling — repath attempts when a mover's next cell is occupied or impassable.
//!
//! Called from movement_tick when terrain, cliff, or occupancy checks fail.
//! Manages the blocked_delay timer and path_stuck_counter to prevent thrashing.

use std::collections::{BTreeSet, HashMap};

use crate::rules::locomotor_type::MovementZone;
use crate::sim::components::MovementTarget;
use crate::sim::debug_event_log::DebugEventKind;
use crate::sim::movement::locomotor::{LocomotorState, MovementLayer};
use crate::sim::pathfinding::terrain_cost::TerrainCostGrid;
use crate::sim::rng::SimRng;
use crate::util::fixed_math::{SIM_ZERO, SimFixed};

use super::movement_path::{supports_layered_bridge_pathing, try_repath_after_block};
use super::{MovementConfig, MovementTickStats, PathfindingContext};

/// Shared logic for handling a blocked movement tick.
///
/// Implements the original engine's two-timer system:
/// - `movement_delay` guards against calling Find_Path too often (PathDelay=)
/// - `blocked_delay` waits for friendlies to clear before escalating (BlockagePathDelay=)
/// - `path_stuck_counter` limits total retries before giving up (init=10)
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_blocked_tick(
    target: &mut MovementTarget,
    facing: &mut u8,
    locomotor: &Option<LocomotorState>,
    entity_id: u64,
    current_pos: (u16, u16),
    active_layer: MovementLayer,
    on_bridge: bool,
    stats: &mut MovementTickStats,
    finished_entities: &mut Vec<u64>,
    aborted_for_stuck: &mut bool,
    ctx: PathfindingContext<'_>,
    entity_cost_grid: Option<&TerrainCostGrid>,
    entity_blocks: Option<&BTreeSet<(u16, u16)>>,
    entity_block_map: Option<&HashMap<(u16, u16), (u16, u16)>>,
    too_big_to_fit_under_bridge: bool,
    mcfg: MovementConfig,
    rng: &mut SimRng,
    sim_tick: u64,
    path_stuck_init: u8,
) -> Vec<(u32, DebugEventKind)> {
    let mut deferred_events: Vec<(u32, DebugEventKind)> = Vec::new();
    stats.blocked_attempts = stats.blocked_attempts.saturating_add(1);
    let next_cell = target.path.get(target.next_index).copied();
    let goal = target
        .final_goal
        .unwrap_or_else(|| target.path.last().copied().unwrap_or(current_pos));

    if !target.path_blocked {
        target.path_blocked = true;
        target.blocked_delay = mcfg.blockage_path_delay_ticks;
        if let Some((nx, ny)) = next_cell {
            deferred_events.push((
                sim_tick as u32,
                DebugEventKind::Blocked {
                    by_entity: None,
                    cell: (nx, ny),
                },
            ));
        }
    }

    if mcfg.close_enough > SIM_ZERO {
        let dx = (goal.0 as i32 - current_pos.0 as i32).abs();
        let dy = (goal.1 as i32 - current_pos.1 as i32).abs();
        let dist = SimFixed::from_num((dx + dy) * 256);
        if dist < mcfg.close_enough {
            log::info!(
                "CLOSE_ENOUGH entity={} pos=({},{}) goal=({},{}) dist={} - stopping",
                entity_id,
                current_pos.0,
                current_pos.1,
                goal.0,
                goal.1,
                dist,
            );
            finished_entities.push(entity_id);
            *aborted_for_stuck = true;
            return deferred_events;
        }
    }

    if target.movement_delay > 0 {
        return deferred_events;
    }

    // Repath every tick while movement_delay == 0. Urgency escalates once the
    // blocked_delay (BlockagePathDelay) timer has expired:
    //   urgency=1 while blocked_delay > 0 → 4x traffic penalty
    //   urgency=2 once blocked_delay == 0 → 1000x route-around
    // Matches gamemd.exe DriveLocomotionClass::Process_Movement (LAB_004b3607).
    stats.repath_attempts = stats.repath_attempts.saturating_add(1);
    let urgency: u8 = if target.blocked_delay > 0 { 1 } else { 2 };
    let layered_pathing_for_repath = locomotor
        .as_ref()
        .zip(ctx.path_grid)
        .is_some_and(|(loco, pg)| supports_layered_bridge_pathing(loco, pg, on_bridge));
    let repath_mz: Option<MovementZone> = locomotor.as_ref().map(|l| l.movement_zone);
    let repath_ok = try_repath_after_block(
        target,
        facing,
        current_pos,
        active_layer,
        layered_pathing_for_repath,
        ctx,
        entity_cost_grid,
        entity_blocks,
        rng,
        repath_mz,
        too_big_to_fit_under_bridge,
        mcfg,
        entity_block_map,
        urgency,
    );
    if repath_ok {
        stats.repath_successes = stats.repath_successes.saturating_add(1);
        target.path_blocked = false;
        target.path_stuck_counter = path_stuck_init;
        deferred_events.push((
            sim_tick as u32,
            DebugEventKind::Repath {
                reason: format!("blocked repath succeeded (urgency={})", urgency),
                new_path_len: target.path.len(),
            },
        ));
    } else if urgency >= 2 {
        // Only escalated (urgency=2) repath failures count toward give-up.
        // gamemd.exe decrements path_stuck_counter in a separate "no valid
        // next cell" branch, not on every code-2 repath miss — so we don't
        // decrement during the blocked_delay grace period (urgency=1).
        target.path_stuck_counter = target.path_stuck_counter.saturating_sub(1);
        if target.path_stuck_counter == 0 {
            log::warn!(
                "STUCK ABORT entity={} pos=({},{}) - path_stuck_counter exhausted",
                entity_id,
                current_pos.0,
                current_pos.1,
            );
            deferred_events.push((
                sim_tick as u32,
                DebugEventKind::StuckAbort { blocked_ticks: 0 },
            ));
            stats.stuck_recoveries = stats.stuck_recoveries.saturating_add(1);
            finished_entities.push(entity_id);
            *aborted_for_stuck = true;
        } else {
            // Urgency=2 failed — restart blocked_delay so the next cycle
            // begins a new grace period rather than thrashing.
            target.blocked_delay = mcfg.blockage_path_delay_ticks;
        }
    } else {
        // urgency=1 grace-period failure: set a short movement_delay to
        // rate-limit A* calls while the blocked_delay counter keeps ticking.
        target.movement_delay = mcfg.path_delay_ticks;
    }
    deferred_events
}
