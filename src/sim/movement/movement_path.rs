//! Movement path management — path computation, repath-after-block, and bridge pathing support.
//!
//! Wraps the A* pathfinder for use by the movement tick: computes initial paths,
//! retries after blockages with zone-aware corridor search, and determines whether
//! an entity's locomotor supports layered bridge pathing.

use std::collections::{BTreeSet, HashMap};

use crate::map::resolved_terrain::ResolvedTerrainGrid;
use crate::rules::locomotor_type::{LocomotorKind, MovementZone};
use crate::sim::components::MovementTarget;
use crate::sim::movement::locomotor::{LocomotorState, MovementLayer};
use crate::sim::pathfinding::path_smooth;
use crate::sim::pathfinding::terrain_cost::TerrainCostGrid;
use crate::sim::pathfinding::zone_search;
use crate::sim::pathfinding::{MAX_PATH_SEGMENT_STEPS, PathGrid, truncate_layered_path};
use crate::sim::rng::SimRng;
use crate::util::fixed_math::facing_from_delta_int as facing_from_delta;

use super::{MovementConfig, PathfindingContext};

fn is_under_bridge_blocked_cell(cell: &crate::map::resolved_terrain::ResolvedTerrainCell) -> bool {
    cell.is_elevated_bridge_cell()
}

pub(super) fn merge_path_blocks(
    entity_blocks: Option<&BTreeSet<(u16, u16)>>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
    movement_zone: Option<MovementZone>,
    too_big_to_fit_under_bridge: bool,
) -> BTreeSet<(u16, u16)> {
    let mut blocks = entity_blocks.cloned().unwrap_or_default();
    if too_big_to_fit_under_bridge && movement_zone.is_some_and(|mz| !mz.is_water_mover()) {
        // TODO(RE): The current under-bridge restriction is a hard path block. The RE notes
        // still leave open whether RA2/YR treats TooBigToFitUnderBridge as a pure parking/
        // eviction rule, a navigation restriction, or a mix depending on bridge state.
        if let Some(terrain) = resolved_terrain {
            for cell in terrain.iter().filter(|c| is_under_bridge_blocked_cell(c)) {
                blocks.insert((cell.rx, cell.ry));
            }
        }
    }
    blocks
}

pub(super) fn supports_layered_bridge_pathing(
    loco: &LocomotorState,
    grid: &PathGrid,
    on_bridge: bool,
) -> bool {
    if grid.width() == 0 || grid.height() == 0 {
        return false;
    }
    matches!(
        loco.kind,
        LocomotorKind::Drive | LocomotorKind::Walk | LocomotorKind::Mech
    ) || on_bridge
}

fn is_bridge_layer_walkable(grid: Option<&PathGrid>, cell: (u16, u16)) -> bool {
    grid.is_some_and(|g| g.is_walkable_on_layer(cell.0, cell.1, MovementLayer::Bridge))
}

fn is_bridge_only_goal(grid: &PathGrid, goal: (u16, u16)) -> bool {
    !grid.is_walkable(goal.0, goal.1) && is_bridge_layer_walkable(Some(grid), goal)
}

pub(super) fn is_move_goal_walkable(
    grid: &PathGrid,
    goal: (u16, u16),
    movement_zone: Option<MovementZone>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
) -> bool {
    if movement_zone.is_some_and(|mz| mz.is_water_mover()) {
        return crate::sim::pathfinding::is_cell_passable_for_mover(
            grid,
            goal.0,
            goal.1,
            movement_zone,
            resolved_terrain,
        );
    }
    grid.is_any_layer_walkable(goal.0, goal.1)
}

fn nearest_move_goal(
    grid: &PathGrid,
    goal: (u16, u16),
    max_radius: u16,
    blocked_cells: Option<&BTreeSet<(u16, u16)>>,
    movement_zone: Option<MovementZone>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
) -> Option<(u16, u16)> {
    if !movement_zone.is_some_and(|mz| mz.is_water_mover()) {
        return grid.nearest_walkable_any_layer(goal.0, goal.1, max_radius, blocked_cells, None);
    }

    let check = |x: u16, y: u16| {
        is_move_goal_walkable(grid, (x, y), movement_zone, resolved_terrain)
            && blocked_cells.map_or(true, |blocks| !blocks.contains(&(x, y)))
    };
    if check(goal.0, goal.1) {
        return Some(goal);
    }
    for radius in 1..=max_radius {
        let r = radius as i32;
        for d in -r..=r {
            let candidates = [
                (goal.0 as i32 + d, goal.1 as i32 - r),
                (goal.0 as i32 + d, goal.1 as i32 + r),
                (goal.0 as i32 - r, goal.1 as i32 + d),
                (goal.0 as i32 + r, goal.1 as i32 + d),
            ];
            for (x, y) in candidates {
                if x < 0 || y < 0 || x >= grid.width() as i32 || y >= grid.height() as i32 {
                    continue;
                }
                let candidate = (x as u16, y as u16);
                if check(candidate.0, candidate.1) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

pub(super) fn resolve_requested_move_goal(
    grid: &PathGrid,
    goal: (u16, u16),
    blocked_cells: Option<&BTreeSet<(u16, u16)>>,
    movement_zone: Option<MovementZone>,
    resolved_terrain: Option<&ResolvedTerrainGrid>,
    max_radius: u16,
) -> Option<(u16, u16)> {
    if is_move_goal_walkable(grid, goal, movement_zone, resolved_terrain)
        && blocked_cells.map_or(true, |blocks| !blocks.contains(&goal))
    {
        return Some(goal);
    }

    nearest_move_goal(
        grid,
        goal,
        max_radius,
        blocked_cells,
        movement_zone,
        resolved_terrain,
    )
}

pub(super) fn find_move_path(
    ctx: PathfindingContext<'_>,
    layered_pathing: bool,
    start: (u16, u16),
    start_layer: MovementLayer,
    goal: (u16, u16),
    terrain_costs: Option<&TerrainCostGrid>,
    entity_blocks: Option<&BTreeSet<(u16, u16)>>,
    ground_blocks: Option<&BTreeSet<(u16, u16)>>,
    bridge_blocks: Option<&BTreeSet<(u16, u16)>>,
    zone_mz: MovementZone,
    movement_zone: Option<MovementZone>,
    too_big_to_fit_under_bridge: bool,
    entity_block_map: Option<&HashMap<(u16, u16), (u16, u16)>>,
    urgency: u8,
) -> Option<(Vec<(u16, u16)>, Vec<MovementLayer>)> {
    let grid = ctx.path_grid?;
    let zone_grid = ctx.zone_grid;
    let resolved_terrain = ctx.resolved_terrain;
    let merged_entity_blocks = merge_path_blocks(
        entity_blocks,
        resolved_terrain,
        movement_zone,
        too_big_to_fit_under_bridge,
    );
    let entity_blocks = (!merged_entity_blocks.is_empty()).then_some(&merged_entity_blocks);
    if layered_pathing {
        let layered_result = zone_search::find_layered_path_zoned(
            grid,
            ground_blocks,
            bridge_blocks,
            start,
            start_layer,
            goal,
            zone_grid,
            zone_mz,
            terrain_costs,
            movement_zone,
            entity_block_map,
            urgency,
        );
        if let Some(path) = layered_result {
            log::trace!(
                "find_move_path: layered A* succeeded ({:?}→{:?}), {} steps",
                start,
                goal,
                path.len(),
            );
            let coords: Vec<(u16, u16)> = path.iter().map(|step| (step.rx, step.ry)).collect();
            let layers: Vec<MovementLayer> = path.iter().map(|step| step.layer).collect();
            let layered_smooth_walkable = |x: u16, y: u16, layer: MovementLayer| -> bool {
                if !grid.is_walkable_on_layer(x, y, layer) {
                    return false;
                }
                match layer {
                    MovementLayer::Ground => !ground_blocks.is_some_and(|gb| gb.contains(&(x, y))),
                    MovementLayer::Bridge => !bridge_blocks.is_some_and(|bb| bb.contains(&(x, y))),
                    _ => true,
                }
            };
            let (coords, layers) =
                path_smooth::smooth_layered_path(coords, layers, &layered_smooth_walkable);
            let (coords, layers) =
                path_smooth::optimize_layered_path(coords, layers, &layered_smooth_walkable);
            let (coords, layers) = truncate_layered_path(coords, layers, MAX_PATH_SEGMENT_STEPS);
            return Some((coords, layers));
        } else {
            log::info!(
                "find_move_path: layered A* FAILED ({:?} layer={:?} → {:?}), falling back to flat A*",
                start,
                start_layer,
                goal,
            );
        }
    }

    if is_bridge_only_goal(grid, goal) {
        return None;
    }

    let path = zone_search::find_path_zoned(
        grid,
        start,
        goal,
        terrain_costs,
        entity_blocks,
        zone_grid,
        zone_mz,
        movement_zone,
        resolved_terrain,
        entity_block_map,
        urgency,
    )?;

    let smooth_walkable = |x: u16, y: u16| -> bool {
        let terrain_ok = if movement_zone.is_some_and(|mz| mz.is_water_mover()) {
            crate::sim::pathfinding::is_cell_passable_for_mover(
                grid,
                x,
                y,
                movement_zone,
                resolved_terrain,
            )
        } else {
            grid.is_walkable(x, y)
        };
        terrain_ok && !entity_blocks.is_some_and(|eb| eb.contains(&(x, y)))
    };
    let path = path_smooth::smooth_path(path, &smooth_walkable);
    let path = path_smooth::optimize_path(path, &smooth_walkable);
    let path_layers = build_flat_fallback_layers(&path, start_layer, grid);
    let (path, path_layers) = truncate_layered_path(path, path_layers, MAX_PATH_SEGMENT_STEPS);
    Some((path, path_layers))
}

/// Build per-cell movement layers for a flat A* fallback path.
///
/// If the entity starts on a bridge, preserve `MovementLayer::Bridge` for
/// contiguous bridge-walkable cells from the path start. Once the path
/// leaves the bridge deck, all remaining cells are `Ground`.
fn build_flat_fallback_layers(
    path: &[(u16, u16)],
    start_layer: MovementLayer,
    grid: &PathGrid,
) -> Vec<MovementLayer> {
    if start_layer != MovementLayer::Bridge {
        return vec![MovementLayer::Ground; path.len()];
    }
    let mut layers = Vec::with_capacity(path.len());
    let mut on_bridge = true;
    for &(x, y) in path {
        if on_bridge && grid.is_walkable_on_layer(x, y, MovementLayer::Bridge) {
            layers.push(MovementLayer::Bridge);
        } else {
            on_bridge = false;
            layers.push(MovementLayer::Ground);
        }
    }
    layers
}

#[allow(clippy::too_many_arguments)]
pub(super) fn try_repath_after_block(
    target: &mut MovementTarget,
    facing: &mut u8,
    current: (u16, u16),
    current_layer: MovementLayer,
    layered_pathing: bool,
    ctx: PathfindingContext<'_>,
    terrain_costs: Option<&TerrainCostGrid>,
    entity_blocks: Option<&BTreeSet<(u16, u16)>>,
    _rng: &mut SimRng,
    movement_zone: Option<MovementZone>,
    too_big_to_fit_under_bridge: bool,
    mcfg: MovementConfig,
    entity_block_map: Option<&HashMap<(u16, u16), (u16, u16)>>,
    urgency: u8,
) -> bool {
    let goal = target
        .final_goal
        .unwrap_or_else(|| target.path.last().copied().unwrap_or(current));
    if goal == current {
        return false;
    }
    let Some(grid) = ctx.path_grid else {
        target.movement_delay = mcfg.path_delay_ticks;
        return false;
    };

    let combined_blocks: BTreeSet<(u16, u16)> = merge_path_blocks(
        entity_blocks,
        ctx.resolved_terrain,
        movement_zone,
        too_big_to_fit_under_bridge,
    );
    let Some(effective_goal) = resolve_requested_move_goal(
        grid,
        goal,
        Some(&combined_blocks),
        movement_zone,
        ctx.resolved_terrain,
        10,
    ) else {
        target.movement_delay = mcfg.path_delay_ticks;
        return false;
    };
    if effective_goal != goal {
        log::info!(
            "Repath: goal ({},{}) blocked, redirecting to ({},{})",
            goal.0,
            goal.1,
            effective_goal.0,
            effective_goal.1,
        );
        target.final_goal = Some(effective_goal);
    }

    let zone_mz = movement_zone.unwrap_or(MovementZone::Normal);
    let path_result = find_move_path(
        ctx,
        layered_pathing,
        current,
        current_layer,
        effective_goal,
        terrain_costs,
        Some(&combined_blocks),
        None,
        None,
        zone_mz,
        movement_zone,
        too_big_to_fit_under_bridge,
        entity_block_map,
        urgency,
    );
    let Some((new_path, new_layers)) = path_result else {
        target.movement_delay = mcfg.path_delay_ticks;
        return false;
    };
    if new_path.len() < 2 {
        target.movement_delay = mcfg.path_delay_ticks;
        return false;
    }

    target.path = new_path;
    target.path_layers = new_layers;
    debug_assert_eq!(
        target.path.len(),
        target.path_layers.len(),
        "path/path_layers desync after blocked repath"
    );
    target.next_index = 1;
    target.blocked_delay = 0;
    target.path_blocked = false;
    target.movement_delay = mcfg.path_delay_ticks;
    let next = target.path[target.next_index];
    let dx = next.0 as i32 - current.0 as i32;
    let dy = next.1 as i32 - current.1 as i32;
    let (d_x, d_y, d_len) = crate::util::lepton::cell_delta_to_lepton_dir(dx, dy);
    target.move_dir_x = d_x;
    target.move_dir_y = d_y;
    target.move_dir_len = d_len;
    *facing = facing_from_delta(dx, dy);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::resolved_terrain::{ResolvedTerrainCell, ResolvedTerrainGrid};
    use crate::rules::terrain_rules::{SpeedCostProfile, TerrainClass};
    use crate::sim::pathfinding::passability::LandType;

    fn make_resolved_cell(rx: u16, ry: u16) -> ResolvedTerrainCell {
        ResolvedTerrainCell {
            rx,
            ry,
            source_tile_index: 0,
            source_sub_tile: 0,
            final_tile_index: 0,
            final_sub_tile: 0,
            level: 0,
            filled_clear: false,
            tileset_index: Some(0),
            land_type: 0,
            slope_type: 0,
            template_height: 0,
            render_offset_x: 0,
            render_offset_y: 0,
            terrain_class: TerrainClass::Clear,
            speed_costs: SpeedCostProfile::default(),
            is_water: false,
            is_cliff_like: false,
            is_rough: false,
            is_road: false,
            is_cliff_redraw: false,
            variant: 0,
            has_ramp: false,
            canonical_ramp: None,
            ground_walk_blocked: false,
            terrain_object_blocks: false,
            overlay_blocks: false,
            zone_type: 0,
            base_ground_walk_blocked: false,
            base_build_blocked: false,
            build_blocked: false,
            has_bridge_deck: false,
            bridge_walkable: false,
            bridge_transition: false,
            bridge_deck_level: 0,
            bridge_layer: None,
            radar_left: [0, 0, 0],
            radar_right: [0, 0, 0],
        }
    }

    #[test]
    fn merge_path_blocks_only_blocks_under_bridge_cells_for_land_movers() {
        let terrain = ResolvedTerrainGrid::from_cells(
            3,
            1,
            vec![
                make_resolved_cell(0, 0),
                ResolvedTerrainCell {
                    bridge_walkable: true,
                    bridge_deck_level: 1,
                    ..make_resolved_cell(1, 0)
                },
                make_resolved_cell(2, 0),
            ],
        );

        let land_blocks = merge_path_blocks(None, Some(&terrain), Some(MovementZone::Normal), true);
        assert!(
            land_blocks.contains(&(1, 0)),
            "land movers marked too large should block under-bridge cells"
        );

        let water_blocks = merge_path_blocks(None, Some(&terrain), Some(MovementZone::Water), true);
        assert!(
            !water_blocks.contains(&(1, 0)),
            "water movers should keep under-bridge cells available"
        );
    }

    #[test]
    fn water_mover_goal_redirect_stays_on_water_cells() {
        let mut cells = Vec::new();
        for ry in 0..3 {
            for rx in 0..3 {
                let is_water = matches!((rx, ry), (0, 1) | (1, 1) | (2, 1));
                cells.push(ResolvedTerrainCell {
                    land_type: if is_water {
                        LandType::Water.as_index()
                    } else {
                        LandType::Clear.as_index()
                    },
                    is_water,
                    ..make_resolved_cell(rx, ry)
                });
            }
        }
        let terrain = ResolvedTerrainGrid::from_cells(3, 3, cells);
        let grid = PathGrid::from_resolved_terrain(&terrain);
        let mut blocked = BTreeSet::new();
        blocked.insert((1, 1));

        let redirected = resolve_requested_move_goal(
            &grid,
            (1, 1),
            Some(&blocked),
            Some(MovementZone::Water),
            Some(&terrain),
            2,
        )
        .expect("water mover should find an alternate water goal");

        assert!(
            matches!(redirected, (0, 1) | (2, 1)),
            "water mover redirect should stay on water, got {:?}",
            redirected
        );
    }
}
