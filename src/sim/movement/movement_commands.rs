//! Move command issuing — A* pathfinding and MovementTarget attachment.
//!
//! Entry points for issuing move commands to entities. These are called from
//! `world_commands.rs`, `miner_system.rs`, and `production_queue.rs` — not
//! from the per-tick movement loop.
//!
//! ## Dependency rules
//! - Internal to sim/movement — called via re-exports in mod.rs.

use std::collections::BTreeSet;

use crate::map::entities::EntityCategory;
use crate::map::resolved_terrain::ResolvedTerrainGrid;
use crate::sim::components::MovementTarget;
use crate::sim::entity_store::EntityStore;
use crate::sim::pathfinding::PathGrid;
use crate::sim::pathfinding::terrain_cost::TerrainCostGrid;
use crate::sim::pathfinding::zone_map::ZoneCategory;
use crate::util::fixed_math::{SIM_ZERO, SimFixed};

use super::movement_path::{
    find_move_path, merge_path_blocks, resolve_requested_move_goal, supports_layered_bridge_pathing,
};
use super::{PathfindingContext, facing_from_delta};
use crate::rules::locomotor_type::MovementZone;
use crate::sim::components::OrderIntent;
use crate::sim::game_entity::GameEntity;

use super::droppod_movement::DropPodPhase;

/// Check if an entity can accept a new movement destination.
///
/// Prevents destination changes during special states: dying, deploying,
/// undeploying, falling, and unloading passengers.
fn can_accept_destination(entity: &GameEntity) -> bool {
    if entity.dying {
        return false;
    }
    if entity.building_up.is_some() || entity.building_down.is_some() {
        return false;
    }
    if entity
        .droppod_state
        .as_ref()
        .is_some_and(|s| s.phase == DropPodPhase::Falling)
    {
        return false;
    }
    if matches!(entity.order_intent, Some(OrderIntent::Unloading)) {
        return false;
    }
    true
}

/// Issue a move command: compute an A* path and attach a MovementTarget to the entity.
///
/// Returns `true` if a valid path was found and the entity is now moving.
/// Returns `false` if the entity doesn't exist, has no Position, or no path exists.
///
/// `speed` is the movement speed in cells per second (from rules.ini Speed= value).
pub fn issue_move_command(
    entities: &mut EntityStore,
    grid: &PathGrid,
    entity_id: u64,
    target: (u16, u16),
    speed: SimFixed,
    queue: bool,
    terrain_costs: Option<&TerrainCostGrid>,
    entity_blocks: Option<&BTreeSet<(u16, u16)>>,
) -> bool {
    issue_move_command_with_layered(
        entities,
        grid,
        entity_id,
        target,
        speed,
        queue,
        terrain_costs,
        entity_blocks,
        None, // resolved_terrain — per-tick repath has it
    )
}

/// Issue a direct move to a single cell without A* pathfinding.
///
/// Used for scripted movement into/out of building footprints where the target
/// cell is not pathfindable (e.g. refinery pad inside the foundation). Creates
/// a 1-step `MovementTarget` that the tick system advances smoothly via lepton
/// sub-cell interpolation — the same path the original engine's
/// `ILocomotion::MoveTo` takes.
///
/// Returns `true` if the entity was found and the move was issued.
pub fn issue_direct_move(
    entities: &mut EntityStore,
    entity_id: u64,
    target: (u16, u16),
    speed: SimFixed,
) -> bool {
    let Some(entity) = entities.get(entity_id) else {
        return false;
    };
    if !can_accept_destination(entity) {
        return false;
    }
    let start = (entity.position.rx, entity.position.ry);
    if start == target {
        return true; // Already there.
    }
    let current_layer = entity.movement_layer_or_ground();

    let dx = target.0 as i32 - start.0 as i32;
    let dy = target.1 as i32 - start.1 as i32;
    let new_facing = facing_from_delta(dx, dy);
    let (dir_x, dir_y, dir_len) = crate::util::lepton::cell_delta_to_lepton_dir(dx, dy);

    let movement = MovementTarget {
        path: vec![start, target],
        path_layers: vec![current_layer, current_layer],
        next_index: 1,
        speed,
        current_speed: speed,
        move_dir_x: dir_x,
        move_dir_y: dir_y,
        move_dir_len: dir_len,
        ignore_terrain_cost: true,
        ..Default::default()
    };

    if let Some(entity_mut) = entities.get_mut(entity_id) {
        entity_mut.movement_target = Some(movement);
        let has_rot = entity_mut.locomotor.as_ref().is_some_and(|l| l.rot > 0);
        if entity_mut.category != EntityCategory::Infantry && has_rot {
            entity_mut.facing_target = Some(new_facing);
        } else {
            entity_mut.facing = new_facing;
        }
    }
    true
}

pub fn issue_move_command_with_layered(
    entities: &mut EntityStore,
    grid: &PathGrid,
    entity_id: u64,
    target: (u16, u16),
    speed: SimFixed,
    queue: bool,
    terrain_costs: Option<&TerrainCostGrid>,
    entity_blocks: Option<&BTreeSet<(u16, u16)>>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
) -> bool {
    // Read the entity's current position and locomotor state.
    let Some(entity) = entities.get(entity_id) else {
        log::warn!("issue_move_command: entity {} not found", entity_id);
        return false;
    };
    if !can_accept_destination(entity) {
        return false;
    }
    let start_rx: u16 = entity.position.rx;
    let start_ry: u16 = entity.position.ry;
    let current_layer = entity.movement_layer_or_ground();
    // Derive movement_zone from the entity's locomotor — no parameter needed.
    let movement_zone: Option<MovementZone> = entity.locomotor.as_ref().map(|l| l.movement_zone);
    let too_big_to_fit_under_bridge = entity.too_big_to_fit_under_bridge;
    let layered_pathing = entity
        .locomotor
        .as_ref()
        .is_some_and(|loco| supports_layered_bridge_pathing(loco, grid, entity.on_bridge));
    let merged_entity_blocks = merge_path_blocks(
        entity_blocks,
        resolved_terrain,
        movement_zone,
        too_big_to_fit_under_bridge,
    );
    let merged_entity_blocks_ref =
        (!merged_entity_blocks.is_empty()).then_some(&merged_entity_blocks);
    let Some(effective_target) = resolve_requested_move_goal(
        grid,
        target,
        merged_entity_blocks_ref,
        movement_zone,
        resolved_terrain,
        10,
    ) else {
        log::warn!(
            "No walkable cell near ({},{}) - cannot issue move",
            target.0,
            target.1,
        );
        return false;
    };
    if effective_target != target {
        log::info!(
            "Move: goal ({},{}) blocked, redirecting to ({},{})",
            target.0,
            target.1,
            effective_target.0,
            effective_target.1,
        );
    }

    if queue {
        // Check if entity already has a movement target to append to.
        let entity_mut = entities.get_mut(entity_id);
        if let Some(entity_mut) = entity_mut {
            if let Some(ref mut movement) = entity_mut.movement_target {
                let append_start = movement
                    .path
                    .last()
                    .copied()
                    .unwrap_or((start_rx, start_ry));
                let append_layer = movement
                    .path_layers
                    .last()
                    .copied()
                    .unwrap_or(current_layer);
                let zone_cat = movement_zone.map_or(ZoneCategory::Land, |mz| {
                    ZoneCategory::from_movement_zone(mz)
                });
                let Some((appended, appended_layers)) = find_move_path(
                    PathfindingContext {
                        path_grid: Some(grid),
                        zone_grid: None,
                        resolved_terrain,
                    },
                    layered_pathing,
                    append_start,
                    append_layer,
                    effective_target,
                    terrain_costs,
                    merged_entity_blocks_ref,
                    None,
                    None, // Layer-separated blocks not available here
                    zone_cat,
                    movement_zone,
                    too_big_to_fit_under_bridge,
                ) else {
                    return false;
                };
                if appended.len() >= 2 {
                    movement.path.extend_from_slice(&appended[1..]);
                    movement
                        .path_layers
                        .extend_from_slice(&appended_layers[1..]);
                    movement.speed = speed;
                    movement.blocked_delay = 0;
                    movement.path_blocked = false;
                    debug_assert_eq!(
                        movement.path.len(),
                        movement.path_layers.len(),
                        "path/path_layers desync after queue append"
                    );
                }
                return true;
            }
        }
    }
    let zone_cat = movement_zone.map_or(ZoneCategory::Land, |mz| {
        ZoneCategory::from_movement_zone(mz)
    });
    let Some((path, path_layers)) = find_move_path(
        PathfindingContext {
            path_grid: Some(grid),
            zone_grid: None,
            resolved_terrain,
        },
        layered_pathing,
        (start_rx, start_ry),
        current_layer,
        effective_target,
        terrain_costs,
        merged_entity_blocks_ref,
        None,
        None, // Layer-separated blocks not available here
        zone_cat,
        movement_zone,
        too_big_to_fit_under_bridge,
    ) else {
        let eb_count = merged_entity_blocks_ref.map_or(0, |s| s.len());
        log::warn!(
            "No path from ({},{}) to ({},{}) [entity_blocks={}, start_walkable={}, goal_walkable={}]",
            start_rx,
            start_ry,
            effective_target.0,
            effective_target.1,
            eb_count,
            grid.is_walkable(start_rx, start_ry),
            grid.is_walkable(effective_target.0, effective_target.1),
        );
        return false;
    };

    // Log path with walkability check for each cell — helps diagnose paths
    // that go through blocked cells (indicates PathGrid mismatch).
    let path_desc: String = path
        .iter()
        .map(|&(px, py)| {
            let w = grid.is_walkable(px, py);
            if w {
                format!("({},{})", px, py)
            } else {
                format!("({},{})!BLOCKED", px, py)
            }
        })
        .collect::<Vec<_>>()
        .join("→");
    log::info!(
        "Path: grid={}x{} entity_blocks={} {}",
        grid.width(),
        grid.height(),
        merged_entity_blocks_ref.map_or(0, |s| s.len()),
        path_desc,
    );

    // Compute initial facing toward the first movement cell (path[1], since path[0] = start).
    let mut new_facing: Option<u8> = None;
    if path.len() >= 2 {
        let next: (u16, u16) = path[1];
        let dx: i32 = next.0 as i32 - start_rx as i32;
        let dy: i32 = next.1 as i32 - start_ry as i32;
        new_facing = Some(facing_from_delta(dx, dy));
    }

    // Compute initial direction vector toward the first path step.
    // No carry-forward needed — sub_x/sub_y already encode the entity's
    // exact lepton position, so it continues from wherever it is.
    let (dir_x, dir_y, dir_len) = if path.len() >= 2 {
        crate::util::lepton::cell_delta_to_lepton_dir(
            path[1].0 as i32 - path[0].0 as i32,
            path[1].1 as i32 - path[0].1 as i32,
        )
    } else {
        (SIM_ZERO, SIM_ZERO, SIM_ZERO)
    };

    // Attach the MovementTarget and update facing on the entity.
    // All units start at full speed — acceleration/deceleration is disabled.
    let movement: MovementTarget = MovementTarget {
        path,
        path_layers,
        next_index: 1, // Index 0 is the current position, 1 is the first target.
        speed,
        current_speed: speed,
        move_dir_x: dir_x,
        move_dir_y: dir_y,
        move_dir_len: dir_len,
        final_goal: Some(effective_target),
        ..Default::default()
    };
    debug_assert_eq!(
        movement.path.len(),
        movement.path_layers.len(),
        "path/path_layers desync in initial MovementTarget"
    );

    if let Some(entity_mut) = entities.get_mut(entity_id) {
        entity_mut.movement_target = Some(movement);
        if let Some(f) = new_facing {
            // Infantry always turn instantly (RA2 behavior).
            // Vehicles with ROT>0 set facing_target for gradual rotation.
            let has_rot: bool = entity_mut.locomotor.as_ref().is_some_and(|l| l.rot > 0);
            if entity_mut.category != EntityCategory::Infantry && has_rot {
                entity_mut.facing_target = Some(f);
            } else {
                entity_mut.facing = f;
            }
        }
    }

    true
}
