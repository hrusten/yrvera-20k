//! Debug visualization overlays — pathgrid walkability, terrain costs, etc.
//!
//! Each overlay builds `Vec<SpriteInstance>` quads drawn with the white texture
//! at low opacity using tint colors. Toggled via hotkeys (F9 = pathgrid).
//!
//! ## Dependency rules
//! - Part of the app layer — may depend on everything.

use crate::app::AppState;
use crate::app_instances::in_view;
use crate::map::terrain::{self, TILE_HEIGHT, TILE_WIDTH};
use crate::render::batch::SpriteInstance;
use crate::rules::locomotor_type::SpeedType;

/// Semi-transparent tint alpha applied to debug overlay cells.
/// Multiplied into the RGB channels since the shader does `color.rgb * tint`.
const OVERLAY_ALPHA: f32 = 0.35;

/// Depth for debug overlays — drawn above terrain but below entities.
/// Between terrain (depth ~0.9+) and entities/overlays (depth ~0.0005-0.0007).
const DEBUG_OVERLAY_DEPTH: f32 = 0.0004;

// -- Terrain cost overlay colors --

/// Terrain cost: blocked (cost=0). Red.
const COST_BLOCKED_TINT: [f32; 3] = [OVERLAY_ALPHA, 0.0, 0.0];
/// Terrain cost: very rough (cost ~60, e.g. wheel on rough). Orange.
const COST_VERY_ROUGH_TINT: [f32; 3] = [OVERLAY_ALPHA, 0.15, 0.0];
/// Terrain cost: rough (cost ~75, e.g. track on rough). Dark yellow.
const COST_ROUGH_TINT: [f32; 3] = [OVERLAY_ALPHA, 0.25, 0.0];
/// Terrain cost: slightly slow (cost ~90, e.g. foot on rough). Yellow.
const COST_SLOW_TINT: [f32; 3] = [OVERLAY_ALPHA, OVERLAY_ALPHA, 0.0];
/// Terrain cost: normal passable (cost=100). Green.
const COST_NORMAL_TINT: [f32; 3] = [0.0, OVERLAY_ALPHA, 0.0];
/// Terrain cost: above normal (cost=106+, e.g. INI speed >100%). Cyan.
const COST_FAST_TINT: [f32; 3] = [0.0, OVERLAY_ALPHA, OVERLAY_ALPHA];

/// Map a terrain cost value to a tint color.
fn cost_to_tint(cost: u8) -> [f32; 3] {
    match cost {
        0 => COST_BLOCKED_TINT,
        1..=65 => COST_VERY_ROUGH_TINT,
        66..=80 => COST_ROUGH_TINT,
        81..=95 => COST_SLOW_TINT,
        96..=105 => COST_NORMAL_TINT,
        _ => COST_FAST_TINT,
    }
}

/// Resolve which SpeedType the terrain cost overlay should display.
///
/// Priority: manual override → first selected entity's locomotor → Track default.
pub(crate) fn resolve_debug_speed_type(state: &AppState) -> SpeedType {
    if let Some(st) = state.debug_terrain_cost_speed_type {
        return st;
    }
    state
        .simulation
        .as_ref()
        .and_then(|sim| {
            sim.entities
                .values()
                .find(|e| e.selected)
                .and_then(|e| e.locomotor.as_ref())
                .map(|l| l.speed_type)
        })
        .unwrap_or(SpeedType::Track)
}

/// Build terrain cost overlay instances for all visible cells.
///
/// Each cell gets a colored diamond quad based on the movement cost for the
/// active SpeedType: red=blocked, orange/yellow=rough, green=normal, cyan=road.
pub(crate) fn build_terrain_cost_overlay_instances(
    state: &AppState,
    sw: f32,
    sh: f32,
) -> Vec<SpriteInstance> {
    let Some(sim) = &state.simulation else {
        log::warn!("Terrain cost overlay: no simulation");
        return Vec::new();
    };
    let speed_type = resolve_debug_speed_type(state);
    let Some(cost_grid) = sim.terrain_costs.get(&speed_type) else {
        log::warn!("Terrain cost overlay: no cost grid for {:?}", speed_type);
        return Vec::new();
    };

    let width: u16 = cost_grid.width();
    let height: u16 = cost_grid.height();

    // One-shot diagnostic on first call.
    use std::sync::atomic::{AtomicBool, Ordering};
    static LOGGED: AtomicBool = AtomicBool::new(false);
    if !LOGGED.swap(true, Ordering::Relaxed) {
        // Sample a few cells to verify the grid has non-zero data.
        let sample_costs: Vec<u8> = (0..width.min(20))
            .map(|x| cost_grid.cost_at(x, height / 2))
            .collect();
        log::info!(
            "Terrain cost overlay: {:?} grid {}x{}, cam=({:.0},{:.0}), screen=({:.0},{:.0}), sample costs={:?}",
            speed_type,
            width,
            height,
            state.camera_x,
            state.camera_y,
            sw,
            sh,
            sample_costs,
        );
    }

    let mut instances: Vec<SpriteInstance> = Vec::with_capacity(2048);
    let cam_x: f32 = state.camera_x;
    let cam_y: f32 = state.camera_y;

    for ry in 0..height {
        for rx in 0..width {
            // Bridge cells use deck height so dots appear ON the bridge.
            let z: u8 = state
                .bridge_height_map
                .get(&(rx, ry))
                .copied()
                .unwrap_or_else(|| state.height_map.get(&(rx, ry)).copied().unwrap_or(0));
            let (sx, sy) = terrain::iso_to_screen(rx, ry, z);

            if !in_view(sx, sy, TILE_WIDTH, TILE_HEIGHT, cam_x, cam_y, sw, sh, 60.0) {
                continue;
            }

            let cost: u8 = cost_grid.cost_at(rx, ry);
            let tint: [f32; 3] = cost_to_tint(cost);

            instances.push(SpriteInstance {
                position: [sx, sy],
                size: [TILE_WIDTH, TILE_HEIGHT],
                uv_origin: [0.0, 0.0],
                uv_size: [1.0, 1.0],
                depth: DEBUG_OVERLAY_DEPTH,
                tint,
                alpha: 1.0,
            });
        }
    }

    instances
}

/// Build debug height map overlay — each cell tinted by elevation.
/// Ground cells use white brightness proportional to z level.
/// Bridge deck cells use blue tint to distinguish them from ground.
pub(crate) fn build_heightmap_overlay_instances(
    state: &AppState,
    sw: f32,
    sh: f32,
) -> Vec<SpriteInstance> {
    if state.height_map.is_empty() {
        return Vec::new();
    }
    let cam_x: f32 = state.camera_x;
    let cam_y: f32 = state.camera_y;
    let mut instances: Vec<SpriteInstance> = Vec::with_capacity(2048);

    // Find max z for normalization.
    let max_z: u8 = state.height_map.values().copied().max().unwrap_or(1).max(1);

    // One-shot diagnostic: log bridge_height_map size on first call.
    use std::sync::atomic::{AtomicBool, Ordering};
    static LOGGED: AtomicBool = AtomicBool::new(false);
    if !LOGGED.swap(true, Ordering::Relaxed) {
        let bridge_count = state.bridge_height_map.len();
        let sample: Vec<_> = state.bridge_height_map.iter().take(10).collect();
        log::info!(
            "HeightmapOverlay: bridge_height_map has {} entries, height_map has {} entries. Sample: {:?}",
            bridge_count,
            state.height_map.len(),
            sample,
        );
    }

    for (&(rx, ry), &z) in &state.height_map {
        // Bridge cells: render at deck height so the overlay aligns with the bridge surface.
        let render_z: u8 = state.bridge_height_map.get(&(rx, ry)).copied().unwrap_or(z);
        let (sx, sy) = terrain::iso_to_screen(rx, ry, render_z);
        if !in_view(sx, sy, TILE_WIDTH, TILE_HEIGHT, cam_x, cam_y, sw, sh, 60.0) {
            continue;
        }

        let is_bridge = state.bridge_height_map.contains_key(&(rx, ry));
        let tint: [f32; 3] = if is_bridge {
            // Solid blue for bridge deck cells — high alpha so the bridge
            // structure doesn't show through the overlay.
            [0.0, 0.15, 0.7]
        } else {
            // White/grey brightness by elevation.
            let intensity: f32 = (z as f32 / max_z as f32) * OVERLAY_ALPHA;
            [intensity, intensity, intensity]
        };

        // Bridge cells need a lower depth to avoid being occluded by bridge
        // overlay sprites that write to the depth buffer in the entity pass.
        let cell_depth: f32 = if is_bridge {
            0.0001
        } else {
            DEBUG_OVERLAY_DEPTH
        };
        instances.push(SpriteInstance {
            position: [sx, sy],
            size: [TILE_WIDTH, TILE_HEIGHT],
            uv_origin: [0.0, 0.0],
            uv_size: [1.0, 1.0],
            depth: cell_depth,
            tint,
            alpha: 1.0,
        });
    }
    instances
}

/// Build debug cell grid overlay — diamond outlines for terrain cells (blue)
/// and overlay cells (yellow) so alignment between the two can be verified visually.
///
/// Both use `iso_to_screen()` with the same height map. If they misalign,
/// the outlines will visually separate — making coordinate bugs obvious.
pub(crate) fn build_cell_grid_overlay_instances(
    state: &AppState,
    sw: f32,
    sh: f32,
) -> Vec<SpriteInstance> {
    let mut instances: Vec<SpriteInstance> = Vec::with_capacity(4096);
    let cam_x: f32 = state.camera_x;
    let cam_y: f32 = state.camera_y;

    /// Terrain cell grid: bright cyan outlines.
    const TERRAIN_TINT: [f32; 3] = [0.0, 0.9, 0.9];
    /// Overlay cell grid: bright yellow outlines.
    const OVERLAY_TINT: [f32; 3] = [1.0, 1.0, 0.0];
    /// Depth — just above terrain but below entities.
    const GRID_DEPTH: f32 = 0.0003;

    // Terrain grid cells.
    if let Some(grid) = &state.terrain_grid {
        for cell in &grid.cells {
            if !in_view(
                cell.screen_x,
                cell.screen_y,
                TILE_WIDTH,
                TILE_HEIGHT,
                cam_x,
                cam_y,
                sw,
                sh,
                60.0,
            ) {
                continue;
            }
            instances.push(SpriteInstance {
                position: [cell.screen_x, cell.screen_y],
                size: [TILE_WIDTH, TILE_HEIGHT],
                uv_origin: [0.0, 0.0],
                uv_size: [1.0, 1.0],
                depth: GRID_DEPTH,
                tint: TERRAIN_TINT,
                alpha: 1.0,
            });
        }
    }

    // Overlay cells — re-compute iso_to_screen for each overlay entry so the
    // diamond outline uses the exact same formula as overlay rendering.
    for entry in &state.overlays {
        let z: u8 = state
            .height_map
            .get(&(entry.rx, entry.ry))
            .copied()
            .unwrap_or(0);
        let (sx, sy) = terrain::iso_to_screen(entry.rx, entry.ry, z);
        if !in_view(sx, sy, TILE_WIDTH, TILE_HEIGHT, cam_x, cam_y, sw, sh, 60.0) {
            continue;
        }
        instances.push(SpriteInstance {
            position: [sx, sy],
            size: [TILE_WIDTH, TILE_HEIGHT],
            uv_origin: [0.0, 0.0],
            uv_size: [1.0, 1.0],
            depth: GRID_DEPTH - 0.0001,
            tint: OVERLAY_TINT,
            alpha: 1.0,
        });
    }

    instances
}

/// Build path visualization overlay for selected entities.
///
/// Draws the current computed path as colored diamond cells:
/// - Cyan: current/upcoming path step
/// - Magenta: next step (immediate target)
/// - Yellow: final goal
pub(crate) fn build_path_overlay_instances(
    state: &AppState,
    sw: f32,
    sh: f32,
) -> Vec<SpriteInstance> {
    let Some(sim) = &state.simulation else {
        return Vec::new();
    };
    let cam_x: f32 = state.camera_x;
    let cam_y: f32 = state.camera_y;
    let mut instances: Vec<SpriteInstance> = Vec::with_capacity(128);

    /// Path step color: bright cyan.
    const PATH_TINT: [f32; 3] = [0.0, 0.6, 0.6];
    /// Next immediate step: bright magenta.
    const NEXT_TINT: [f32; 3] = [0.7, 0.0, 0.7];
    /// Final goal: bright yellow.
    const GOAL_TINT: [f32; 3] = [0.7, 0.7, 0.0];
    const PATH_DEPTH: f32 = 0.0002;

    for entity in sim.entities.values() {
        if !entity.selected {
            continue;
        }
        let Some(ref mt) = entity.movement_target else {
            continue;
        };
        for (i, &(px, py)) in mt.path.iter().enumerate() {
            let z: u8 = state.height_map.get(&(px, py)).copied().unwrap_or(0);
            let (sx, sy) = terrain::iso_to_screen(px, py, z);
            if !in_view(sx, sy, TILE_WIDTH, TILE_HEIGHT, cam_x, cam_y, sw, sh, 60.0) {
                continue;
            }
            let tint: [f32; 3] = if i == mt.next_index {
                NEXT_TINT
            } else if i == mt.path.len() - 1 {
                GOAL_TINT
            } else {
                PATH_TINT
            };
            instances.push(SpriteInstance {
                position: [sx, sy],
                size: [TILE_WIDTH, TILE_HEIGHT],
                uv_origin: [0.0, 0.0],
                uv_size: [1.0, 1.0],
                depth: PATH_DEPTH,
                tint,
                alpha: 1.0,
            });
        }
        // Also draw final_goal if it's not the last path step.
        if let Some(goal) = mt.final_goal {
            let last = mt.path.last().copied().unwrap_or((0, 0));
            if goal != last {
                let z: u8 = state
                    .height_map
                    .get(&(goal.0, goal.1))
                    .copied()
                    .unwrap_or(0);
                let (sx, sy) = terrain::iso_to_screen(goal.0, goal.1, z);
                if in_view(sx, sy, TILE_WIDTH, TILE_HEIGHT, cam_x, cam_y, sw, sh, 60.0) {
                    instances.push(SpriteInstance {
                        position: [sx, sy],
                        size: [TILE_WIDTH, TILE_HEIGHT],
                        uv_origin: [0.0, 0.0],
                        uv_size: [1.0, 1.0],
                        depth: PATH_DEPTH,
                        tint: GOAL_TINT,
                        alpha: 1.0,
                    });
                }
            }
        }
    }

    instances
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tint_colors_are_distinct() {
        // Blocked and normal cost tints must differ so the overlay is useful.
        assert_ne!(COST_BLOCKED_TINT, COST_NORMAL_TINT);
    }

    #[test]
    fn overlay_alpha_is_reasonable() {
        // Alpha should be between 0 and 1, and visibly translucent.
        assert!(OVERLAY_ALPHA > 0.1);
        assert!(OVERLAY_ALPHA < 0.8);
    }
}
