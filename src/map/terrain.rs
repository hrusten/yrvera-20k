//! Isometric terrain grid: coordinate math, viewport culling, and instance generation.
//!
//! Converts map cells (isometric rx/ry coordinates) to screen-space pixel positions
//! for rendering. Provides viewport culling to only draw visible tiles.
//!
//! ## Coordinate system
//! RA2 uses isometric coordinates where each cell is a diamond shape (60x30 pixels).
//! Screen position: `sx = (rx - ry) * 30`, `sy = (rx + ry) * 15 - z * 15`.
//!
//! ## Dependency rules
//! - Part of map/ — depends on map/map_file for MapFile/MapCell.

use std::collections::BTreeMap;

use crate::map::map_file::{MapFile, MapHeader};
use crate::map::resolved_terrain::ResolvedTerrainGrid;
use crate::render::batch::SpriteInstance;

/// Isometric tile diamond width in pixels (RA2 standard).
pub const TILE_WIDTH: f32 = 60.0;

/// Isometric tile diamond height in pixels (RA2 standard).
pub const TILE_HEIGHT: f32 = 30.0;

/// Pixels per elevation level. Each z-step raises the tile by this many pixels.
/// RA2: CellHeight = CellSizeY / 2 = 30 / 2 = 15. Confirmed by ra2_yr_map_terrain.md §1.6:
/// "Each height level shifts the cell up by 15 pixels on screen (RA2)."
/// (Note: Tiberian Sun uses 24/2 = 12 — do NOT use TS values here.)
pub const HEIGHT_STEP: f32 = 15.0;

/// Margin around viewport for culling (pixels). Tiles just outside the visible
/// area are still drawn to avoid pop-in during scrolling.
const CULL_MARGIN: f32 = 120.0;

/// Playable area bounds from map `[Map] LocalSize`, in our screen pixel space.
///
/// In RA2, cells outside LocalSize are hidden by permanent shroud/fog of war.
/// We use this to clip terrain rendering so out-of-bounds cells (which are often
/// tile_index=0 green grass filler) are not drawn.
///
/// LocalSize is in "cell unit" coordinates using the TS-scale pixel grid
/// (CellSizeX=48, CellSizeY=24). We convert those pixel coords to our engine's
/// coordinate system (CellSizeX=60, CellSizeY=30) which has the same isometric
/// axes but different scale and offset.
///
/// Conversion from TS-scale pixel space to our screen space:
///   our_x = ts_x * (60/48) - (Size.X - 1) * 30
///   our_y = ts_y * (30/24) + (Size.X + 1) * 15
///   (both scale factors are 1.25)
#[derive(Debug, Clone, Copy)]
pub struct LocalBounds {
    pub pixel_x: f32,
    pub pixel_y: f32,
    pub pixel_w: f32,
    pub pixel_h: f32,
}

/// InitialHeight constant — Y padding at top for elevation headroom.
const TS_INITIAL_HEIGHT: f32 = 3.0;

/// HeightAddition constant — extra rows below for tall terrain.
const TS_HEIGHT_ADDITION: f32 = 5.0;

/// TS-scale cell pixel dimensions (used for LocalSize conversion).
const TS_CELL_SIZE_X: f32 = 48.0;
const TS_CELL_SIZE_Y: f32 = 24.0;

/// Scale factor from TS-scale pixels to our pixels (60/48 = 30/24 = 1.25).
const TS_SCALE: f32 = TILE_WIDTH / TS_CELL_SIZE_X;

impl LocalBounds {
    /// Build from MapHeader LocalSize values.
    ///
    /// Converts the TS-scale LocalSize pixel rectangle to our engine's screen coordinates.
    /// Formula: x = local_left * 48, y = (local_top - 3) * 24
    /// Our offset: x -= (Size.X - 1) * 30, y += (Size.X + 1) * 15
    pub fn from_header(header: &MapHeader) -> Self {
        let size_x: f32 = header.width as f32;

        // TS-scale pixel coordinates (no baseline — we handle offset ourselves).
        let ts_x: f32 = header.local_left as f32 * TS_CELL_SIZE_X;
        let ts_y: f32 = (header.local_top as f32 - TS_INITIAL_HEIGHT) * TS_CELL_SIZE_Y;
        let ts_w: f32 = header.local_width as f32 * TS_CELL_SIZE_X;
        let ts_h: f32 = (header.local_height as f32 + TS_HEIGHT_ADDITION) * TS_CELL_SIZE_Y;

        // Convert to our coordinate system.
        let our_x: f32 = ts_x * TS_SCALE - (size_x - 1.0) * (TILE_WIDTH / 2.0);
        let our_y: f32 = ts_y * TS_SCALE + (size_x + 1.0) * (TILE_HEIGHT / 2.0);

        LocalBounds {
            pixel_x: our_x,
            pixel_y: our_y,
            pixel_w: ts_w * TS_SCALE,
            pixel_h: ts_h * TS_SCALE,
        }
    }

    /// Check if a screen position is within the playable area.
    pub fn contains(&self, screen_x: f32, screen_y: f32) -> bool {
        screen_x >= self.pixel_x
            && screen_x < self.pixel_x + self.pixel_w
            && screen_y >= self.pixel_y
            && screen_y < self.pixel_y + self.pixel_h
    }
}

/// Per-tile rendering placement data returned by the UV lookup function.
/// Carries atlas UV coordinates, actual pixel size, and draw offset.
#[derive(Debug, Clone, Copy)]
pub struct TilePlacement {
    /// UV origin in the atlas texture (0.0..1.0).
    pub uv_origin: [f32; 2],
    /// UV extent in the atlas texture (0.0..1.0).
    pub uv_size: [f32; 2],
    /// Actual pixel dimensions of this tile (may differ from 60×30 for cliff/shore tiles).
    pub pixel_size: [f32; 2],
    /// Draw offset from the standard diamond origin (pixels).
    /// Negative values shift the sprite left/up to accommodate extra data regions.
    pub draw_offset: [f32; 2],
}

/// UV lookup function type: maps (tile_id, sub_tile) → optional placement data.
/// Returns None for tiles not in the atlas (empty template cells), which are skipped.
/// UV lookup: (tile_id, sub_tile, variant) → placement data.
pub type UvLookupFn<'a> = Option<&'a dyn Fn(u16, u8, u8) -> Option<TilePlacement>>;

/// A single terrain cell with pre-computed screen position.
#[derive(Debug, Clone)]
pub struct TerrainCell {
    /// Screen X position (top-left of diamond bounding box).
    pub screen_x: f32,
    /// Screen Y position (top-left of diamond bounding box).
    pub screen_y: f32,
    /// Tile index for atlas lookup (truncated from i32 to u16; -1 filtered out).
    pub tile_id: u16,
    /// Sub-tile index within the template.
    pub sub_tile: u8,
    /// Elevation level (0 = ground). Used for depth buffer computation.
    pub z: u8,
    /// Isometric cell X coordinate (preserved for lighting lookups).
    pub rx: u16,
    /// Isometric cell Y coordinate (preserved for lighting lookups).
    pub ry: u16,
    /// True when the resolved terrain classifies the cell as water.
    pub is_water: bool,
    /// FinalAlert2 cliff redraw flag — this tile is drawn a second time after
    /// entities so cliff face pixels occlude units behind them.
    pub is_cliff_redraw: bool,
    /// Tile visual variant index (FA2 bRNDImage): 0 = main tile, 1-4 = replacement a-d.
    pub variant: u8,
    /// RGB color tint from map lighting. [1,1,1] = full brightness (default).
    pub tint: [f32; 3],
    /// Per-tile radar minimap color (left half of isometric diamond), from TMP header.
    pub radar_left: [u8; 3],
    /// Per-tile radar minimap color (right half of isometric diamond), from TMP header.
    pub radar_right: [u8; 3],
}

/// Pre-computed terrain grid ready for rendering.
///
/// Cells are sorted by screen_y for correct back-to-front draw order.
/// World bounds are computed from all cell positions.
#[derive(Debug)]
pub struct TerrainGrid {
    /// All terrain cells, sorted by screen_y (draw order).
    pub cells: Vec<TerrainCell>,
    /// Total world width in pixels.
    pub world_width: f32,
    /// Total world height in pixels.
    pub world_height: f32,
    /// Minimum screen_x across all cells (world origin x).
    pub origin_x: f32,
    /// Minimum screen_y across all cells (world origin y).
    pub origin_y: f32,
    /// Playable area bounds (from LocalSize). Used to clip overlays/entities too.
    pub local_bounds: Option<LocalBounds>,
}

/// Convert isometric cell coordinates to screen-space pixel position.
///
/// Returns the top-left corner of the tile's diamond bounding box.
/// The original engine passes cell CENTER coords to its coordinate transform
/// for tile positioning, placing the tile NW corner at the diamond center's
/// screen Y:
///   X = 30*(rx-ry) - 30
///   Y = 15*(rx+ry) + 15 - z*15
pub fn iso_to_screen(rx: u16, ry: u16, z: u8) -> (f32, f32) {
    let sx: f32 = (rx as f32 - ry as f32) * TILE_WIDTH / 2.0 - TILE_WIDTH / 2.0;
    let sy: f32 =
        (rx as f32 + ry as f32) * TILE_HEIGHT / 2.0 + TILE_HEIGHT / 2.0 - z as f32 * HEIGHT_STEP;
    (sx, sy)
}

/// Convert screen-space pixel position back to isometric cell coordinates.
///
/// Inverse of `iso_to_screen`. Assumes z=0 (ground level). Returns floating-point
/// coordinates; caller should round to get the nearest cell.
///
/// `iso_to_screen` maps (rx,ry) to `((rx-ry)*30 - 30, (rx+ry)*15 + 15)`.
/// The tile center is at NW + (30, 15). To map tile centers back to integer
/// cell coords, shift by half tile before dividing.
///
/// Derivation (clicking at tile center):
///   tile_center_X = (rx-ry)*30,  tile_center_Y = (rx+ry)*15 + 30
///   col = center_X / 30 = screen_x / 30  (after shifting click left by 30)
///   row = (center_Y - 30) / 15 = (screen_y - 30) / 15
pub fn screen_to_iso(screen_x: f32, screen_y: f32) -> (f32, f32) {
    let half_w: f32 = TILE_WIDTH / 2.0;
    let col: f32 = screen_x / half_w;
    let row: f32 = (screen_y - TILE_HEIGHT) / (TILE_HEIGHT / 2.0);
    let rx: f32 = (col + row) / 2.0;
    let ry: f32 = (row - col) / 2.0;
    (rx, ry)
}

/// Convert screen-space pixel position to isometric cell, accounting for terrain elevation.
///
/// Iteratively refines the cell guess by looking up the actual terrain height at each
/// candidate cell and re-solving with the corrected screen Y. This fixes the z=0
/// assumption in `screen_to_iso` which causes clicks on elevated terrain to resolve
/// to the wrong cell.
///
/// Converges in 1-3 iterations on typical RA2 terrain gradients.
pub fn screen_to_iso_with_height(
    screen_x: f32,
    screen_y: f32,
    height_map: &BTreeMap<(u16, u16), u8>,
) -> (f32, f32) {
    screen_to_iso_with_height_and_bridges(screen_x, screen_y, height_map, None)
}

/// Convert screen coordinates to isometric cell coordinates, accounting for
/// terrain height and optionally bridge deck height.
///
/// When `bridge_height_map` is provided and the resolved cell has a bridge deck,
/// the bridge deck elevation is used instead of the ground elevation. This makes
/// clicks on high bridge surfaces resolve to the correct cell.
pub fn screen_to_iso_with_height_and_bridges(
    screen_x: f32,
    screen_y: f32,
    height_map: &BTreeMap<(u16, u16), u8>,
    bridge_height_map: Option<&BTreeMap<(u16, u16), u8>>,
) -> (f32, f32) {
    // First pass: resolve using ground height (existing behavior).
    let (mut rx, mut ry) = screen_to_iso(screen_x, screen_y);
    for _ in 0..3 {
        let cell_rx: u16 = rx.round().max(0.0) as u16;
        let cell_ry: u16 = ry.round().max(0.0) as u16;
        let z: u8 = height_map.get(&(cell_rx, cell_ry)).copied().unwrap_or(0);
        if z == 0 {
            break;
        }
        let corrected_y: f32 = screen_y + z as f32 * HEIGHT_STEP;
        let (new_rx, new_ry) = screen_to_iso(screen_x, corrected_y);
        if (new_rx - rx).abs() < 0.01 && (new_ry - ry).abs() < 0.01 {
            break;
        }
        rx = new_rx;
        ry = new_ry;
    }

    // Second pass: bridge deck click resolution. The bridge surface is elevated
    // (deck_level = ground + 4), so the ground resolution above can shift the
    // result by up to ~3 cells away from the actual bridge cell. We search a
    // neighborhood around the ground-resolved cell for any bridge entries and
    // test each at its deck height. The closest match wins.
    if let Some(bridge_map) = bridge_height_map {
        let cell_rx: u16 = rx.round().max(0.0) as u16;
        let cell_ry: u16 = ry.round().max(0.0) as u16;
        let mut best: Option<(f32, f32)> = None;
        let mut best_dist: f32 = f32::MAX;
        for dy in -3i32..=3 {
            for dx in -3i32..=3 {
                let bx_i: i32 = cell_rx as i32 + dx;
                let by_i: i32 = cell_ry as i32 + dy;
                if bx_i < 0 || by_i < 0 {
                    continue;
                }
                let bx: u16 = bx_i as u16;
                let by: u16 = by_i as u16;
                if let Some(&bridge_z) = bridge_map.get(&(bx, by)) {
                    let corrected_y: f32 = screen_y + bridge_z as f32 * HEIGHT_STEP;
                    let (new_rx, new_ry) = screen_to_iso(screen_x, corrected_y);
                    let dist: f32 = (new_rx - bx as f32).abs() + (new_ry - by as f32).abs();
                    if dist < 0.7 && dist < best_dist {
                        best = Some((new_rx, new_ry));
                        best_dist = dist;
                    }
                }
            }
        }
        if let Some((brx, bry)) = best {
            rx = brx;
            ry = bry;
        }
    }

    (rx, ry)
}

/// Build a TerrainGrid from a parsed map file.
///
/// Converts all map cells to screen coordinates, computes world bounds,
/// and sorts by screen_y for correct draw order. Cells outside the
/// LocalSize playable area are clipped (they are filler tiles hidden
/// by shroud in the original RA2 engine).
pub fn build_terrain_grid(map: &MapFile, local_bounds: Option<LocalBounds>) -> TerrainGrid {
    let mut cells: Vec<TerrainCell> = Vec::with_capacity(map.cells.len());
    let mut min_x: f32 = f32::MAX;
    let mut min_y: f32 = f32::MAX;
    let mut max_x: f32 = f32::MIN;
    let mut max_y: f32 = f32::MIN;
    let mut clipped: u32 = 0;

    for cell in &map.cells {
        // Skip true "no tile" entries: -1 (0xFFFFFFFF).
        // Some maps use 0x0000FFFF as "clear ground" (legacy 16-bit sentinel).
        // Treat that as tile 0 so we don't render black holes in otherwise valid cells.
        if cell.tile_index < 0 {
            continue;
        }
        let tile_id: u16 = if cell.tile_index == 0xFFFF {
            0
        } else {
            cell.tile_index as u16
        };

        let (sx, sy): (f32, f32) = iso_to_screen(cell.rx, cell.ry, cell.z);

        // Clip cells outside the playable area (LocalSize bounds).
        // In RA2, these border cells are hidden under permanent shroud.
        if let Some(ref bounds) = local_bounds {
            if !bounds.contains(sx, sy) {
                clipped += 1;
                continue;
            }
        }

        cells.push(TerrainCell {
            screen_x: sx,
            screen_y: sy,
            tile_id,
            sub_tile: cell.sub_tile,
            z: cell.z,
            rx: cell.rx,
            ry: cell.ry,
            is_water: tile_id == 0,
            is_cliff_redraw: false,
            variant: 0,
            tint: [1.0, 1.0, 1.0],
            radar_left: [0, 0, 0],
            radar_right: [0, 0, 0],
        });

        min_x = min_x.min(sx);
        min_y = min_y.min(sy);
        max_x = max_x.max(sx + TILE_WIDTH);
        max_y = max_y.max(sy + TILE_HEIGHT);
    }

    if clipped > 0 {
        log::info!(
            "LocalSize clip: {} cells kept, {} clipped (outside playable area)",
            cells.len(),
            clipped,
        );
    }

    // Sort by screen_y for back-to-front draw order.
    cells.sort_by(|a, b| {
        a.screen_y
            .partial_cmp(&b.screen_y)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    TerrainGrid {
        cells,
        world_width: max_x - min_x,
        world_height: max_y - min_y,
        origin_x: min_x,
        origin_y: min_y,
        local_bounds,
    }
}

/// Build a TerrainGrid from the resolved terrain stage.
///
/// Unlike `build_terrain_grid()`, this consumes the final LAT-adjusted tile
/// choice and retains water classification from resolved terrain metadata.
pub fn build_terrain_grid_from_resolved(
    resolved: &ResolvedTerrainGrid,
    local_bounds: Option<LocalBounds>,
) -> TerrainGrid {
    let mut cells: Vec<TerrainCell> = Vec::with_capacity(resolved.cells.len());
    let mut min_x: f32 = f32::MAX;
    let mut min_y: f32 = f32::MAX;
    let mut max_x: f32 = f32::MIN;
    let mut max_y: f32 = f32::MIN;
    let mut clipped: u32 = 0;

    for cell in resolved.iter() {
        if cell.final_tile_index < 0 {
            continue;
        }
        let tile_id = if cell.final_tile_index == 0xFFFF {
            0
        } else {
            cell.final_tile_index as u16
        };
        let (sx, sy) = iso_to_screen(cell.rx, cell.ry, cell.level);
        if let Some(ref bounds) = local_bounds {
            if !bounds.contains(sx, sy) {
                clipped += 1;
                continue;
            }
        }
        cells.push(TerrainCell {
            screen_x: sx,
            screen_y: sy,
            tile_id,
            sub_tile: cell.final_sub_tile,
            z: cell.level,
            rx: cell.rx,
            ry: cell.ry,
            is_water: cell.is_water,
            is_cliff_redraw: cell.is_cliff_redraw,
            variant: cell.variant,
            tint: [1.0, 1.0, 1.0],
            radar_left: cell.radar_left,
            radar_right: cell.radar_right,
        });
        min_x = min_x.min(sx);
        min_y = min_y.min(sy);
        max_x = max_x.max(sx + TILE_WIDTH);
        max_y = max_y.max(sy + TILE_HEIGHT);
    }

    if clipped > 0 {
        log::info!(
            "LocalSize clip: {} resolved cells kept, {} clipped (outside playable area)",
            cells.len(),
            clipped,
        );
    }

    cells.sort_by(|a, b| {
        a.screen_y
            .partial_cmp(&b.screen_y)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    TerrainGrid {
        cells,
        world_width: max_x - min_x,
        world_height: max_y - min_y,
        origin_x: min_x,
        origin_y: min_y,
        local_bounds,
    }
}

/// Terrain instance sets: normal terrain drawn behind entities, and cliff-redraw
/// terrain drawn after entities so cliff face pixels occlude units behind them.
/// The cliff-redraw set contains copies of flagged tiles with a depth bias that
/// places them in front of entities in the depth buffer.
pub struct TerrainInstances {
    /// Normal terrain — drawn in the first pass (behind entities).
    pub normal: Vec<SpriteInstance>,
    /// Cliff-redraw terrain — drawn after entities (cliff occlusion pass).
    pub cliff_redraw: Vec<SpriteInstance>,
}

/// Generate SpriteInstance data for all tiles visible in the current viewport.
///
/// Single-layer rendering: each cell draws exactly one tile. LAT transition
/// tiles are fully opaque inside the diamond shape (confirmed via diagnostics),
/// so no base clear-ground layer is needed. Missing tiles are skipped —
/// the caller's UV lookup should provide fallbacks if desired.
pub fn build_visible_instances(
    grid: &TerrainGrid,
    camera_x: f32,
    camera_y: f32,
    screen_width: f32,
    screen_height: f32,
    uv_fn: UvLookupFn<'_>,
    fog: Option<(
        crate::sim::intern::InternedId,
        &crate::sim::vision::FogState,
    )>,
) -> TerrainInstances {
    let view_left: f32 = camera_x - CULL_MARGIN;
    let view_right: f32 = camera_x + screen_width + CULL_MARGIN;
    let view_top: f32 = camera_y - CULL_MARGIN;
    let view_bottom: f32 = camera_y + screen_height + CULL_MARGIN;

    let mut instances = TerrainInstances {
        normal: Vec::with_capacity(grid.cells.len() / 2),
        cliff_redraw: Vec::new(),
    };

    for cell in &grid.cells {
        // AABB visibility test against viewport.
        let right: f32 = cell.screen_x + TILE_WIDTH;
        let bottom: f32 = cell.screen_y + TILE_HEIGHT;

        if right < view_left || cell.screen_x > view_right {
            continue;
        }
        if bottom < view_top || cell.screen_y > view_bottom {
            continue;
        }

        // Skip fully shrouded cells — matches gamemd which doesn't render terrain
        // for unexplored cells at all (ZBuffer cleared to 0xFFFF prevents drawing).
        if let Some((owner, fog_state)) = fog {
            if !fog_state.is_cell_revealed(owner, cell.rx, cell.ry) {
                continue;
            }
        }

        // Depth: reconstruct elevation-free iso row, then normalize.
        // Lower screen_y → larger depth (drawn behind). Elevation bias ensures
        // elevated tiles draw in front of same-row ground tiles.
        let iso_row: f32 = cell.screen_y + cell.z as f32 * HEIGHT_STEP;
        let normalized: f32 = ((iso_row - grid.origin_y) / grid.world_height).clamp(0.0, 1.0);
        let z_bias: f32 = cell.z as f32 * 0.0001;
        let depth: f32 = (1.0 - normalized - z_bias).clamp(0.001, 0.999);

        let placement: Option<TilePlacement> = match &uv_fn {
            Some(f) => f(cell.tile_id, cell.sub_tile, cell.variant),
            None => Some(TilePlacement {
                uv_origin: [0.0, 0.0],
                uv_size: [1.0, 1.0],
                pixel_size: [TILE_WIDTH, TILE_HEIGHT],
                draw_offset: [0.0, 0.0],
            }),
        };

        if let Some(p) = placement {
            let inst = SpriteInstance {
                position: [
                    cell.screen_x + p.draw_offset[0],
                    cell.screen_y + p.draw_offset[1],
                ],
                size: p.pixel_size,
                uv_origin: p.uv_origin,
                uv_size: p.uv_size,
                depth,
                tint: cell.tint,
                alpha: 1.0,
            };
            instances.normal.push(inst);
            // Cliff-redraw: same tile redrawn AFTER sprites using zdepth shader
            // with Less compare. Only cliff face pixels (z_sample > 0) pass the
            // test — flat ground pixels have equal depth and fail Less, preserving
            // sprites near cliff edges.
            if cell.is_cliff_redraw {
                instances.cliff_redraw.push(inst);
            }
        }
    }

    instances
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_iso_to_screen_origin() {
        // (0,0,0) → X = 0-30 = -30, Y = 0+15 = 15
        let (sx, sy): (f32, f32) = iso_to_screen(0, 0, 0);
        assert!((sx - (-30.0)).abs() < f32::EPSILON);
        assert!((sy - 15.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_iso_to_screen_positive() {
        // rx=10, ry=0, z=0 → sx = 300-30 = 270, sy = 150+15 = 165
        let (sx, sy): (f32, f32) = iso_to_screen(10, 0, 0);
        assert!((sx - 270.0).abs() < f32::EPSILON);
        assert!((sy - 165.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_iso_to_screen_diagonal() {
        // rx=5, ry=5, z=0 → sx = 0-30 = -30, sy = 150+15 = 165
        let (sx, sy): (f32, f32) = iso_to_screen(5, 5, 0);
        assert!((sx - (-30.0)).abs() < f32::EPSILON);
        assert!((sy - 165.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_iso_to_screen_elevation() {
        // rx=0, ry=0, z=2 → sx = -30, sy = 15 - 30 = -15
        let (sx, sy): (f32, f32) = iso_to_screen(0, 0, 2);
        assert!((sx - (-30.0)).abs() < f32::EPSILON);
        assert!((sy - (-15.0)).abs() < f32::EPSILON);
    }

    #[test]
    fn test_local_bounds_from_header_dustbowl() {
        // Dustbowl: Size=70x76, LocalSize=2,8,65,62
        let header = MapHeader {
            theater: "TEMPERATE".to_string(),
            width: 70,
            height: 76,
            local_left: 2,
            local_top: 8,
            local_width: 65,
            local_height: 62,
        };
        let bounds: LocalBounds = LocalBounds::from_header(&header);
        // TS-scale pixel rect: x=2*48=96, y=(8-3)*24=120, w=65*48=3120, h=(62+5)*24=1608
        // Our coords: x = 96*1.25 - 69*30 = -1950, y = 120*1.25 + 71*15 = 1215
        // w = 3120*1.25 = 3900, h = 1608*1.25 = 2010
        assert!((bounds.pixel_x - (-1950.0)).abs() < 1.0);
        assert!((bounds.pixel_y - 1215.0).abs() < 1.0);
        assert!((bounds.pixel_w - 3900.0).abs() < 1.0);
        assert!((bounds.pixel_h - 2010.0).abs() < 1.0);
    }

    #[test]
    fn test_local_bounds_contains() {
        let bounds = LocalBounds {
            pixel_x: -1950.0,
            pixel_y: 1215.0,
            pixel_w: 3900.0,
            pixel_h: 2010.0,
        };
        assert!(bounds.contains(-1950.0, 1215.0)); // top-left (inclusive)
        assert!(bounds.contains(0.0, 2000.0)); // center
        assert!(!bounds.contains(-1951.0, 1215.0)); // just left
        assert!(!bounds.contains(-1950.0, 1214.0)); // just above
        assert!(!bounds.contains(1950.0, 1215.0)); // at right edge (exclusive)
        assert!(!bounds.contains(-1950.0, 3225.0)); // at bottom edge (exclusive)
    }

    #[test]
    fn test_build_visible_instances_culling() {
        // Create a small grid manually.
        let grid: TerrainGrid = TerrainGrid {
            cells: vec![
                TerrainCell {
                    screen_x: 0.0,
                    screen_y: 0.0,
                    tile_id: 0,
                    sub_tile: 0,
                    z: 0,
                    rx: 0,
                    ry: 0,
                    is_water: false,
                    is_cliff_redraw: false,
                    variant: 0,
                    tint: [1.0, 1.0, 1.0],
                    radar_left: [0, 0, 0],
                    radar_right: [0, 0, 0],
                },
                TerrainCell {
                    screen_x: 5000.0,
                    screen_y: 5000.0,
                    tile_id: 0,
                    sub_tile: 0,
                    z: 0,
                    rx: 100,
                    ry: 100,
                    is_water: false,
                    is_cliff_redraw: false,
                    variant: 0,
                    tint: [1.0, 1.0, 1.0],
                    radar_left: [0, 0, 0],
                    radar_right: [0, 0, 0],
                },
            ],
            world_width: 5060.0,
            world_height: 5030.0,
            origin_x: 0.0,
            origin_y: 0.0,
            local_bounds: None,
        };

        // Camera at origin, 1024x768 viewport — only first cell should be visible.
        let result: TerrainInstances =
            build_visible_instances(&grid, 0.0, 0.0, 1024.0, 768.0, None, None);
        assert_eq!(result.normal.len(), 1);
        assert_eq!(result.cliff_redraw.len(), 0);
    }

    #[test]
    fn test_screen_to_iso_with_height_flat_terrain() {
        // On flat terrain (z=0 everywhere), result matches plain screen_to_iso.
        let height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();
        let (rx, ry) = screen_to_iso_with_height(300.0, 150.0, &height_map);
        let (rx0, ry0) = screen_to_iso(300.0, 150.0);
        assert!((rx - rx0).abs() < 0.01);
        assert!((ry - ry0).abs() < 0.01);
    }

    #[test]
    fn test_screen_to_iso_with_height_elevated() {
        // Cell (10, 5) at z=4:
        //   iso_to_screen = ((10-5)*30-30, (10+5)*15+15-4*15) = (120, 165)
        //   Tile center = (120+30, 165+15) = (150, 180)
        let mut height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();
        for rx in 8..=12 {
            for ry in 3..=7 {
                height_map.insert((rx, ry), 4);
            }
        }
        let (rx, ry) = screen_to_iso_with_height(150.0, 180.0, &height_map);
        assert!((rx - 10.0).abs() < 0.6, "rx={rx}, expected ~10");
        assert!((ry - 5.0).abs() < 0.6, "ry={ry}, expected ~5");
    }

    #[test]
    fn test_screen_to_iso_with_height_convergence() {
        // Verify the function converges and doesn't overshoot on steep terrain.
        let mut height_map: BTreeMap<(u16, u16), u8> = BTreeMap::new();
        // A ridge: cells with ry < 10 are at z=6, ry >= 10 at z=0.
        for rx in 0..30 {
            for ry in 0..10 {
                height_map.insert((rx, ry), 6);
            }
        }
        // Click on the elevated part: cell (15, 5) at z=6.
        // iso_to_screen = ((15-5)*30-30, (15+5)*15+15-6*15) = (270, 225)
        // Center: (270+30, 225+15) = (300, 240).
        let (rx, ry) = screen_to_iso_with_height(300.0, 240.0, &height_map);
        assert!((rx - 15.0).abs() < 0.6, "rx={rx}, expected ~15");
        assert!((ry - 5.0).abs() < 0.6, "ry={ry}, expected ~5");
    }
}
