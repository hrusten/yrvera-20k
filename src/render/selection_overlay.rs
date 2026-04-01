//! Selection overlay rendering — highlights selected entities and draws drag rectangle.
//!
//! Renders visual feedback for the selection system:
//! - Green selection brackets around selected entities.
//! - Green drag rectangle outline while the player is box-selecting.
//!
//! Uses procedurally-generated textures rendered via the batch shader.
//! The selection overlay is drawn as a separate batch pass after units/sprites.
//!
//! ## Dependency rules
//! - Part of render/ — reads from sim/ (Selected, Position) but NEVER mutates sim state.

use std::collections::BTreeMap;

use crate::assets::asset_manager::AssetManager;
use crate::assets::pal_file::Palette;
use crate::assets::shp_file::ShpFile;
use crate::map::terrain::{self, TILE_HEIGHT, TILE_WIDTH};
use crate::render::batch::{BatchRenderer, BatchTexture, SpriteInstance};
use crate::render::gpu::GpuContext;
use crate::render::sprite_atlas::{ShpSpriteKey, SpriteAtlas};
use crate::rules::house_colors::HouseColorIndex;
use crate::sim::production::BuildingPlacementPreview;
use crate::sim::selection::SelectionState;

/// Drag rectangle color — white.
const DRAG_RECT_COLOR: [u8; 4] = [255, 255, 255, 255];
const PREVIEW_VALID_COLOR: [u8; 4] = [0, 200, 4, 255];
const PREVIEW_INVALID_COLOR: [u8; 4] = [241, 3, 0, 255];
/// Depth for the drag rectangle — always in front of everything.
const DRAG_RECT_DEPTH: f32 = 0.001;

/// Thickness of the drag rectangle outline in pixels (1px = RA2 original style).
const DRAG_RECT_THICKNESS: f32 = 1.0;

/// Number of pip variants packed into the building pip atlas (empty, green, yellow, red).
const PIP_VARIANT_COUNT: u32 = 4;

/// Number of unit pip variants packed into the unit pip atlas (green, yellow, red).
const UNIT_PIP_VARIANT_COUNT: u32 = 3;

/// Manages textures and rendering for selection overlays.
pub struct SelectionOverlay {
    /// Solid color texture for drag rectangle lines.
    drag_texture: BatchTexture,
    /// Semi-transparent texture for valid building placement preview.
    preview_valid_texture: BatchTexture,
    /// Semi-transparent texture for invalid building placement preview.
    preview_invalid_texture: BatchTexture,
    /// Canvas height of the placement texture (from place.shp or TILE_WIDTH×TILE_HEIGHT fallback).
    preview_canvas_h: f32,
    /// Solid white texture reused for order markers/cursor feedback.
    white_texture: BatchTexture,
    /// Diamond outline texture for debug cell grid visualization.
    diamond_outline_texture: BatchTexture,
    /// White filled-diamond texture for debug pathgrid/path overlays.
    diamond_filled_texture: BatchTexture,
    /// Pip atlas: 4 pip frames packed horizontally (empty, green, yellow, red).
    /// Each pip is a small cube-shaped sprite from pips.shp.
    pip_texture: Option<BatchTexture>,
    /// Width of a single pip frame in the atlas (pixels).
    pip_frame_w: u32,
    /// Height of a single pip frame in the atlas (pixels).
    pip_frame_h: u32,
    /// Canvas centering adjustment for building pips (matches DrawSHP flag 0x200).
    pip_canvas_adj: (f32, f32),
    /// Unit pip atlas: 3 pip frames from pips.shp packed horizontally (green, yellow, red).
    /// Frames 16, 17, 18 — used for non-building health bars.
    unit_pip_texture: Option<BatchTexture>,
    /// Width of a single unit pip frame in the atlas (pixels).
    unit_pip_frame_w: u32,
    /// Height of a single unit pip frame in the atlas (pixels).
    unit_pip_frame_h: u32,
    /// Pre-baked pip draw offsets: game constant + canvas centering (frame_x - canvas_w/2).
    /// Infantry pips: pLoc + (-5, delta-24) + canvas_adj
    /// Vehicle pips:  pLoc + (-15, delta-25) + canvas_adj
    pip_infantry_offset: (f32, f32),
    pip_vehicle_offset: (f32, f32),
    /// pipbrd.shp atlas: 2 frames packed horizontally (vehicle bg, infantry bg).
    /// Frame 0 = vehicle/aircraft background (36×4), frame 1 = infantry background (18×4).
    pipbrd_texture: Option<BatchTexture>,
    /// Width of pipbrd frame 0 (vehicle/aircraft) in pixels.
    pipbrd_vehicle_w: u32,
    /// Width of pipbrd frame 1 (infantry) in pixels.
    pipbrd_infantry_w: u32,
    /// Height of pipbrd frames in pixels (both are 4px tall).
    pipbrd_h: u32,
    /// Total atlas width for UV computation.
    pipbrd_atlas_w: u32,
    /// Pre-baked PIPBRD draw offsets: game constant + canvas centering.
    /// Infantry PIPBRD: pLoc + (11, delta-25) + canvas_adj
    /// Vehicle PIPBRD:  pLoc + (1, delta-26) + canvas_adj
    pipbrd_infantry_offset: (f32, f32),
    pipbrd_vehicle_offset: (f32, f32),
    /// Occupant pip atlas: 7 frames from pips.shp packed horizontally.
    /// Frames 6 (empty), 7-12 (PersonGreen..PersonPurple) — used for garrison pips.
    occupant_pip_texture: Option<BatchTexture>,
    occupant_pip_frame_w: u32,
    occupant_pip_frame_h: u32,
    occupant_pip_canvas_adj: (f32, f32),
    /// Tiberium cargo pip atlas: 3 frames from pips2.shp packed horizontally.
    /// Frames 0 (empty), 2 (ore/green), 5 (gem) — used for harvester cargo display.
    tiberium_pip_texture: Option<BatchTexture>,
    tiberium_pip_frame_w: u32,
    tiberium_pip_frame_h: u32,
    tiberium_pip_canvas_adj: (f32, f32),
}

impl SelectionOverlay {
    /// Create a new SelectionOverlay with procedurally-generated textures.
    /// If `assets` is provided, loads pips.shp for authentic RA2 health bar rendering.
    pub fn new(gpu: &GpuContext, batch: &BatchRenderer, assets: Option<&AssetManager>) -> Self {
        let drag_texture: BatchTexture =
            batch.create_texture(gpu, &generate_solid_color(DRAG_RECT_COLOR), 2, 2);
        // Use place.shp from assets for authentic RA2 placement overlay, else procedural.
        let (preview_valid_texture, preview_invalid_texture, _preview_canvas_w, preview_canvas_h) =
            load_place_textures(gpu, batch, assets).unwrap_or_else(|| {
                let valid = batch.create_texture(
                    gpu,
                    &generate_diamond_cell(PREVIEW_VALID_COLOR),
                    TILE_WIDTH as u32,
                    TILE_HEIGHT as u32,
                );
                let invalid = batch.create_texture(
                    gpu,
                    &generate_diamond_cell(PREVIEW_INVALID_COLOR),
                    TILE_WIDTH as u32,
                    TILE_HEIGHT as u32,
                );
                (valid, invalid, TILE_WIDTH, TILE_HEIGHT)
            });
        let white_texture: BatchTexture =
            batch.create_texture(gpu, &generate_solid_color([255, 255, 255, 255]), 2, 2);
        let diamond_outline_texture: BatchTexture = batch.create_texture(
            gpu,
            &generate_diamond_outline(),
            TILE_WIDTH as u32,
            TILE_HEIGHT as u32,
        );
        let diamond_filled_texture: BatchTexture = batch.create_texture(
            gpu,
            &generate_diamond_cell([255, 255, 255, 255]),
            TILE_WIDTH as u32,
            TILE_HEIGHT as u32,
        );

        // Load pips.shp and pack 4 isometric pip frames into a horizontal strip atlas.
        let (pip_texture, pip_frame_w, pip_frame_h, pip_adj_x, pip_adj_y) =
            load_pip_atlas(gpu, batch, assets).unwrap_or((None, 0, 0, 0.0, 0.0));

        // Load pips.shp frames 16-18 for unit/infantry health bar pips.
        let (
            unit_pip_texture,
            unit_pip_frame_w,
            unit_pip_frame_h,
            pip_canvas_adj_x,
            pip_canvas_adj_y,
        ) = load_unit_pip_atlas(gpu, batch, assets).unwrap_or((None, 0, 0, 0.0, 0.0));
        // Bake game offsets + canvas centering into single offset pairs.
        // Infantry pips at pLoc+(-5, -24), vehicle pips at pLoc+(-15, -25).
        let pip_infantry_offset: (f32, f32) = (-5.0 + pip_canvas_adj_x, -24.0 + pip_canvas_adj_y);
        let pip_vehicle_offset: (f32, f32) = (-15.0 + pip_canvas_adj_x, -25.0 + pip_canvas_adj_y);

        // Load pipbrd.shp for non-building health bar backgrounds.
        let (
            pipbrd_texture,
            pipbrd_vehicle_w,
            pipbrd_infantry_w,
            pipbrd_h,
            pipbrd_atlas_w,
            pipbrd_veh_adj_x,
            pipbrd_veh_adj_y,
            pipbrd_inf_adj_x,
            pipbrd_inf_adj_y,
        ) = load_pipbrd_atlas(gpu, batch, assets).unwrap_or((None, 0, 0, 0, 0, 0.0, 0.0, 0.0, 0.0));
        // Bake: infantry PIPBRD at pLoc+(11, -25), vehicle PIPBRD at pLoc+(1, -26).
        let pipbrd_infantry_offset: (f32, f32) =
            (11.0 + pipbrd_inf_adj_x, -25.0 + pipbrd_inf_adj_y);
        let pipbrd_vehicle_offset: (f32, f32) = (1.0 + pipbrd_veh_adj_x, -26.0 + pipbrd_veh_adj_y);

        // Load pips.shp frames 6-12 for occupant pips (garrison slots).
        let (
            occupant_pip_texture,
            occupant_pip_frame_w,
            occupant_pip_frame_h,
            occ_adj_x,
            occ_adj_y,
        ) = load_occupant_pip_atlas(gpu, batch, assets).unwrap_or((None, 0, 0, 0.0, 0.0));

        // Load pips2.shp frames 0, 2, 5 for tiberium cargo pips (harvesters).
        let (
            tiberium_pip_texture,
            tiberium_pip_frame_w,
            tiberium_pip_frame_h,
            tib_adj_x,
            tib_adj_y,
        ) = load_tiberium_pip_atlas(gpu, batch, assets).unwrap_or((None, 0, 0, 0.0, 0.0));

        Self {
            drag_texture,
            preview_valid_texture,
            preview_invalid_texture,
            preview_canvas_h,
            white_texture,
            diamond_outline_texture,
            diamond_filled_texture,
            pip_texture,
            pip_frame_w,
            pip_frame_h,
            pip_canvas_adj: (pip_adj_x, pip_adj_y),
            unit_pip_texture,
            unit_pip_frame_w,
            unit_pip_frame_h,
            pip_infantry_offset,
            pip_vehicle_offset,
            pipbrd_texture,
            pipbrd_vehicle_w,
            pipbrd_infantry_w,
            pipbrd_h,
            pipbrd_atlas_w,
            pipbrd_infantry_offset,
            pipbrd_vehicle_offset,
            occupant_pip_texture,
            occupant_pip_frame_w,
            occupant_pip_frame_h,
            occupant_pip_canvas_adj: (occ_adj_x, occ_adj_y),
            tiberium_pip_texture,
            tiberium_pip_frame_w,
            tiberium_pip_frame_h,
            tiberium_pip_canvas_adj: (tib_adj_x, tib_adj_y),
        }
    }

    /// Build sprite instances for the selection drag rectangle outline.
    ///
    /// Returns 4 thin line instances forming the rectangle. Positions are in
    /// screen-space with camera offset added (the shader subtracts camera_pos).
    pub fn build_drag_rect(
        &self,
        selection: &SelectionState,
        camera_x: f32,
        camera_y: f32,
    ) -> Vec<SpriteInstance> {
        let (min_x, min_y, max_x, max_y) = match selection.drag_rect() {
            Some(r) => r,
            None => return Vec::new(),
        };

        let rect_w: f32 = max_x - min_x;
        let rect_h: f32 = max_y - min_y;
        let t: f32 = DRAG_RECT_THICKNESS;

        // Offset by camera so screen-space coords survive the shader's
        // camera_pos subtraction.
        let ox: f32 = camera_x;
        let oy: f32 = camera_y;

        vec![
            // Top edge.
            SpriteInstance {
                position: [ox + min_x, oy + min_y],
                size: [rect_w, t],
                uv_origin: [0.0, 0.0],
                uv_size: [1.0, 1.0],
                depth: DRAG_RECT_DEPTH,
                tint: [1.0, 1.0, 1.0],
                alpha: 1.0,
            },
            // Bottom edge.
            SpriteInstance {
                position: [ox + min_x, oy + max_y - t],
                size: [rect_w, t],
                uv_origin: [0.0, 0.0],
                uv_size: [1.0, 1.0],
                depth: DRAG_RECT_DEPTH,
                tint: [1.0, 1.0, 1.0],
                alpha: 1.0,
            },
            // Left edge.
            SpriteInstance {
                position: [ox + min_x, oy + min_y],
                size: [t, rect_h],
                uv_origin: [0.0, 0.0],
                uv_size: [1.0, 1.0],
                depth: DRAG_RECT_DEPTH,
                tint: [1.0, 1.0, 1.0],
                alpha: 1.0,
            },
            // Right edge.
            SpriteInstance {
                position: [ox + max_x - t, oy + min_y],
                size: [t, rect_h],
                uv_origin: [0.0, 0.0],
                uv_size: [1.0, 1.0],
                depth: DRAG_RECT_DEPTH,
                tint: [1.0, 1.0, 1.0],
                alpha: 1.0,
            },
        ]
    }

    /// Get the drag rect texture for drawing the selection box.
    pub fn drag_texture(&self) -> &BatchTexture {
        &self.drag_texture
    }

    pub fn preview_valid_texture(&self) -> &BatchTexture {
        &self.preview_valid_texture
    }

    pub fn preview_invalid_texture(&self) -> &BatchTexture {
        &self.preview_invalid_texture
    }

    pub fn white_texture(&self) -> &BatchTexture {
        &self.white_texture
    }

    /// Get the diamond outline texture for debug cell grid visualization.
    pub fn diamond_outline_texture(&self) -> &BatchTexture {
        &self.diamond_outline_texture
    }

    /// Get the white filled-diamond texture for debug pathgrid/path overlays.
    pub fn diamond_filled_texture(&self) -> &BatchTexture {
        &self.diamond_filled_texture
    }

    /// Get the pip atlas texture for health bar rendering.
    /// Returns None if pips.shp failed to load (falls back to white_texture).
    pub fn pip_texture(&self) -> Option<&BatchTexture> {
        self.pip_texture.as_ref()
    }

    /// UV origin for a given pip variant in the horizontal strip atlas.
    /// Variants: 0=empty, 1=green, 2=yellow, 3=red.
    pub fn pip_uv_origin(&self, variant: u32) -> [f32; 2] {
        let u: f32 = variant as f32 / PIP_VARIANT_COUNT as f32;
        [u, 0.0]
    }

    /// UV size for a single pip frame in the atlas.
    pub fn pip_uv_size(&self) -> [f32; 2] {
        [1.0 / PIP_VARIANT_COUNT as f32, 1.0]
    }

    /// Pixel size of one pip frame.
    pub fn pip_frame_size(&self) -> [f32; 2] {
        [self.pip_frame_w as f32, self.pip_frame_h as f32]
    }

    /// Canvas centering adjustment for building pips.
    /// Matches gamemd DrawSHP flag 0x200: (-canvas_w/2 + frame_x, -canvas_h/2 + frame_y).
    pub fn pip_canvas_adj(&self) -> (f32, f32) {
        self.pip_canvas_adj
    }

    /// Get the unit pip atlas texture for non-building health bars.
    /// Returns None if pips.shp frames 16-18 failed to load.
    pub fn unit_pip_texture(&self) -> Option<&BatchTexture> {
        self.unit_pip_texture.as_ref()
    }

    /// UV origin for a unit pip variant: 0=green, 1=yellow, 2=red.
    pub fn unit_pip_uv_origin(&self, variant: u32) -> [f32; 2] {
        let u: f32 = variant as f32 / UNIT_PIP_VARIANT_COUNT as f32;
        [u, 0.0]
    }

    /// UV size for a single unit pip frame in the atlas.
    pub fn unit_pip_uv_size(&self) -> [f32; 2] {
        [1.0 / UNIT_PIP_VARIANT_COUNT as f32, 1.0]
    }

    /// Pixel size of one unit pip frame.
    pub fn unit_pip_frame_size(&self) -> [f32; 2] {
        [self.unit_pip_frame_w as f32, self.unit_pip_frame_h as f32]
    }

    /// Pre-baked pip offset: game constant + canvas centering.
    /// Infantry: (-5 + canvas_adj_x, -24 + canvas_adj_y)
    /// Vehicle:  (-15 + canvas_adj_x, -25 + canvas_adj_y)
    /// Add to (sx, sy + bracket_delta) to get pip draw position.
    pub fn pip_offset(&self, is_infantry: bool) -> (f32, f32) {
        if is_infantry {
            self.pip_infantry_offset
        } else {
            self.pip_vehicle_offset
        }
    }

    /// Occupant pip atlas texture (pips.shp frames 6-12).
    pub fn occupant_pip_texture(&self) -> Option<&BatchTexture> {
        self.occupant_pip_texture.as_ref()
    }

    /// Number of occupant pip variants in the atlas.
    const OCCUPANT_PIP_VARIANTS: u32 = 7;

    /// UV origin for an occupant pip by pips.shp frame index (6-12).
    /// Frame 6 → slot 0, frame 7 → slot 1, ..., frame 12 → slot 6.
    pub fn occupant_pip_uv_origin(&self, frame_index: u32) -> [f32; 2] {
        let slot: u32 = frame_index
            .saturating_sub(6)
            .min(Self::OCCUPANT_PIP_VARIANTS - 1);
        [slot as f32 / Self::OCCUPANT_PIP_VARIANTS as f32, 0.0]
    }

    /// UV size for a single occupant pip frame in the atlas.
    pub fn occupant_pip_uv_size(&self) -> [f32; 2] {
        [1.0 / Self::OCCUPANT_PIP_VARIANTS as f32, 1.0]
    }

    /// Pixel size of one occupant pip frame.
    pub fn occupant_pip_frame_size(&self) -> [f32; 2] {
        [
            self.occupant_pip_frame_w as f32,
            self.occupant_pip_frame_h as f32,
        ]
    }

    /// Canvas centering adjustment for occupant pips.
    pub fn occupant_pip_canvas_adj(&self) -> (f32, f32) {
        self.occupant_pip_canvas_adj
    }

    /// Get the tiberium cargo pip atlas texture (pips2.shp frames 0, 2, 5).
    pub fn tiberium_pip_texture(&self) -> Option<&BatchTexture> {
        self.tiberium_pip_texture.as_ref()
    }

    /// UV origin for a tiberium pip variant: 0=empty, 1=ore, 2=gem.
    pub fn tiberium_pip_uv_origin(&self, variant: u32) -> [f32; 2] {
        let v: f32 = (variant.min(TIBERIUM_PIP_VARIANT_COUNT - 1)) as f32;
        [v / TIBERIUM_PIP_VARIANT_COUNT as f32, 0.0]
    }

    /// UV size for a single tiberium pip frame in the atlas.
    pub fn tiberium_pip_uv_size(&self) -> [f32; 2] {
        [1.0 / TIBERIUM_PIP_VARIANT_COUNT as f32, 1.0]
    }

    /// Pixel size of one tiberium pip frame.
    pub fn tiberium_pip_frame_size(&self) -> [f32; 2] {
        [
            self.tiberium_pip_frame_w as f32,
            self.tiberium_pip_frame_h as f32,
        ]
    }

    /// Canvas centering adjustment for tiberium cargo pips.
    pub fn tiberium_pip_canvas_adj(&self) -> (f32, f32) {
        self.tiberium_pip_canvas_adj
    }

    /// Get the pipbrd atlas texture for non-building health bar backgrounds.
    pub fn pipbrd_texture(&self) -> Option<&BatchTexture> {
        self.pipbrd_texture.as_ref()
    }

    /// UV origin and size for the vehicle/aircraft pipbrd background (frame 0).
    pub fn pipbrd_vehicle_uv(&self) -> ([f32; 2], [f32; 2]) {
        if self.pipbrd_atlas_w == 0 {
            return ([0.0, 0.0], [1.0, 1.0]);
        }
        let u_size: f32 = self.pipbrd_vehicle_w as f32 / self.pipbrd_atlas_w as f32;
        ([0.0, 0.0], [u_size, 1.0])
    }

    /// UV origin and size for the infantry pipbrd background (frame 1).
    pub fn pipbrd_infantry_uv(&self) -> ([f32; 2], [f32; 2]) {
        if self.pipbrd_atlas_w == 0 {
            return ([0.0, 0.0], [1.0, 1.0]);
        }
        let u_start: f32 = self.pipbrd_vehicle_w as f32 / self.pipbrd_atlas_w as f32;
        let u_size: f32 = self.pipbrd_infantry_w as f32 / self.pipbrd_atlas_w as f32;
        ([u_start, 0.0], [u_size, 1.0])
    }

    /// Pixel size of the vehicle/aircraft pipbrd background.
    pub fn pipbrd_vehicle_size(&self) -> [f32; 2] {
        [self.pipbrd_vehicle_w as f32, self.pipbrd_h as f32]
    }

    /// Pixel size of the infantry pipbrd background.
    pub fn pipbrd_infantry_size(&self) -> [f32; 2] {
        [self.pipbrd_infantry_w as f32, self.pipbrd_h as f32]
    }

    /// Pre-baked PIPBRD offset: game constant + canvas centering.
    /// Infantry: (11 + canvas_adj_x, -25 + canvas_adj_y)
    /// Vehicle:  (1 + canvas_adj_x, -26 + canvas_adj_y)
    /// Add to (sx, sy + bracket_delta) to get PIPBRD draw position.
    pub fn pipbrd_offset(&self, is_infantry: bool) -> (f32, f32) {
        if is_infantry {
            self.pipbrd_infantry_offset
        } else {
            self.pipbrd_vehicle_offset
        }
    }

    /// Build per-cell diamond overlay instances, split by validity.
    /// Returns (valid_cells, invalid_cells).
    ///
    /// Uses the standard SHP centering formula (drawPoint + CellSize/2 - ShapeSize/2)
    /// so the place.shp diamond aligns exactly with the terrain tile underneath.
    pub fn build_building_preview(
        &self,
        preview: &BuildingPlacementPreview,
        height_map: &BTreeMap<(u16, u16), u8>,
    ) -> (Vec<SpriteInstance>, Vec<SpriteInstance>) {
        let cap: usize = (preview.width as usize) * (preview.height as usize);
        let mut valid_cells: Vec<SpriteInstance> = Vec::with_capacity(cap);
        let mut invalid_cells: Vec<SpriteInstance> = Vec::with_capacity(cap);
        let depth: f32 = DRAG_RECT_DEPTH;
        // place.shp is a per-cell diamond overlay. Draw each cell at tile size
        // (TILE_WIDTH × TILE_HEIGHT) positioned at iso_to_screen() — same anchor
        // as terrain tiles. The UV crops to the diamond region within the canvas.
        // For place.shp (60×59, frame at y=30 size 60×29), the diamond sits in
        // the bottom half: UV y from 30/59 to 59/59.
        let uv_y_start: f32 = if self.preview_canvas_h > TILE_HEIGHT {
            (self.preview_canvas_h - TILE_HEIGHT) / self.preview_canvas_h
        } else {
            0.0
        };
        let uv_y_size: f32 = TILE_HEIGHT / self.preview_canvas_h;
        for dy in 0..preview.height {
            for dx in 0..preview.width {
                let idx: usize = (dy as usize) * (preview.width as usize) + (dx as usize);
                let cell_ok: bool = preview.cell_valid.get(idx).copied().unwrap_or(false);
                let crx: u16 = preview.rx.saturating_add(dx);
                let cry: u16 = preview.ry.saturating_add(dy);
                let z: u8 = height_map.get(&(crx, cry)).copied().unwrap_or(0);
                let (sx, sy) = terrain::iso_to_screen(crx, cry, z);
                let inst = SpriteInstance {
                    position: [sx, sy],
                    size: [TILE_WIDTH, TILE_HEIGHT],
                    uv_origin: [0.0, uv_y_start],
                    uv_size: [1.0, uv_y_size],
                    depth,
                    tint: [1.0, 1.0, 1.0],
                    alpha: 1.0,
                };
                if cell_ok {
                    valid_cells.push(inst);
                } else {
                    invalid_cells.push(inst);
                }
            }
        }
        (valid_cells, invalid_cells)
    }

    /// Build valid placement diamonds for a list of extra cells (wall auto-fill).
    ///
    /// Draws the place.shp diamond on every intermediate cell between the cursor
    /// and an existing same-type wall. These green diamonds use the same UV/size
    /// as the standard building preview.
    pub fn build_wall_autofill_diamonds(
        &self,
        cells: &[(u16, u16)],
        valid: bool,
        height_map: &BTreeMap<(u16, u16), u8>,
    ) -> (Vec<SpriteInstance>, Vec<SpriteInstance>) {
        let mut valid_cells: Vec<SpriteInstance> = Vec::new();
        let mut invalid_cells: Vec<SpriteInstance> = Vec::new();
        let depth: f32 = DRAG_RECT_DEPTH;
        let uv_y_start: f32 = if self.preview_canvas_h > TILE_HEIGHT {
            (self.preview_canvas_h - TILE_HEIGHT) / self.preview_canvas_h
        } else {
            0.0
        };
        let uv_y_size: f32 = TILE_HEIGHT / self.preview_canvas_h;
        for &(rx, ry) in cells {
            let z: u8 = height_map.get(&(rx, ry)).copied().unwrap_or(0);
            let (sx, sy) = terrain::iso_to_screen(rx, ry, z);
            let inst = SpriteInstance {
                position: [sx, sy],
                size: [TILE_WIDTH, TILE_HEIGHT],
                uv_origin: [0.0, uv_y_start],
                uv_size: [1.0, uv_y_size],
                depth,
                tint: [1.0, 1.0, 1.0],
                alpha: 1.0,
            };
            if valid {
                valid_cells.push(inst);
            } else {
                invalid_cells.push(inst);
            }
        }
        (valid_cells, invalid_cells)
    }

    /// Build a semi-transparent ghost sprite of the building being placed.
    /// Returns None if the building sprite isn't in the atlas.
    pub fn build_ghost_sprite(
        preview: &BuildingPlacementPreview,
        atlas: Option<&SpriteAtlas>,
        house_color: HouseColorIndex,
        height_map: &BTreeMap<(u16, u16), u8>,
        interner: Option<&crate::sim::intern::StringInterner>,
    ) -> Option<(SpriteInstance, u8)> {
        let atlas = atlas?;
        let key = ShpSpriteKey {
            type_id: interner.map_or(String::new(), |i| i.resolve(preview.type_id).to_string()),
            facing: 0,
            frame: 0,
            house_color,
        };
        let entry = atlas.get(&key)?;
        let z: u8 = height_map
            .get(&(preview.rx, preview.ry))
            .copied()
            .unwrap_or(0);
        let (sx, sy) = terrain::iso_to_screen(preview.rx, preview.ry, z);
        let tint: [f32; 3] = if preview.valid {
            [0.5, 1.0, 0.5]
        } else {
            [1.0, 0.5, 0.5]
        };
        // iso_to_screen gives tile NW corner. Entity = iso + (TILE_WIDTH/2, 0).
        let ghost_x: f32 = sx + TILE_WIDTH / 2.0 + entry.offset_x;
        let ghost_y: f32 = sy + entry.offset_y;
        Some((
            SpriteInstance {
                position: [ghost_x, ghost_y],
                size: entry.pixel_size,
                uv_origin: entry.uv_origin,
                uv_size: entry.uv_size,
                depth: DRAG_RECT_DEPTH,
                tint,
                alpha: 1.0,
            },
            entry.page,
        ))
    }
}

/// Generate a 2x2 solid color texture (4 identical pixels).
fn generate_solid_color(color: [u8; 4]) -> Vec<u8> {
    let mut rgba: Vec<u8> = Vec::with_capacity(16);
    for _ in 0..4 {
        rgba.extend_from_slice(&color);
    }
    rgba
}

/// Load pips.shp (buildings) and pack 4 health pip frames into a horizontal strip atlas.
///
/// pips.shp frame indices:
///   0 = empty pip (dark/unfilled)
///   1 = green pip (healthy)
///   2 = yellow pip (medium health)
///   4 = red pip (critical health)
///
/// pips.shp is for BUILDINGS ONLY. Infantry/vehicles use pips2.shp (loaded separately).
/// Uses palette.pal for coloring.
///
/// Returns (Some(texture), frame_w, frame_h) on success, or (None, 0, 0) on failure.
fn load_pip_atlas(
    gpu: &GpuContext,
    batch: &BatchRenderer,
    assets: Option<&AssetManager>,
) -> Option<(Option<BatchTexture>, u32, u32, f32, f32)> {
    let assets = assets?;
    // pips.shp = building health bar pips.
    let shp_data = assets.get("pips.shp")?;
    let shp = ShpFile::from_bytes(&shp_data).ok()?;
    log::info!(
        "Pip atlas source: pips.shp ({}x{}, {} frames)",
        shp.width,
        shp.height,
        shp.frames.len()
    );
    for (i, f) in shp.frames.iter().enumerate() {
        log::info!(
            "  pip frame {:2}: pos=({},{}) size={}x{} pixels={}",
            i,
            f.frame_x,
            f.frame_y,
            f.frame_width,
            f.frame_height,
            f.pixels.len()
        );
    }
    // Load palette.pal for pip sprites (the general game palette, NOT unittem.pal).
    let pal_data = assets
        .get("palette.pal")
        .or_else(|| assets.get("unittem.pal"))?;
    let palette = Palette::from_bytes(&pal_data).ok()?;

    // pips.shp frame layout (buildings only):
    //   0 = empty pip (dark/unfilled)
    //   1 = green pip (healthy)
    //   2 = yellow pip (medium health)
    //   4 = red pip (critical health)
    // Atlas slot order: [empty, green, yellow, red]
    let frame_indices: [usize; PIP_VARIANT_COUNT as usize] = [0, 1, 2, 4];

    // Find max dimensions across the selected frames.
    let mut max_w: u32 = 0;
    let mut max_h: u32 = 0;
    for &fi in &frame_indices {
        if fi >= shp.frames.len() {
            log::warn!("pips.shp frame {} out of range ({})", fi, shp.frames.len());
            return Some((None, 0, 0, 0.0, 0.0));
        }
        let fw: u32 = shp.frames[fi].frame_width as u32;
        let fh: u32 = shp.frames[fi].frame_height as u32;
        if fw > max_w {
            max_w = fw;
        }
        if fh > max_h {
            max_h = fh;
        }
    }

    // Pack frames side-by-side into atlas (max_w * 4, max_h).
    let atlas_w: u32 = max_w * PIP_VARIANT_COUNT;
    let atlas_h: u32 = max_h;
    let mut rgba: Vec<u8> = vec![0u8; (atlas_w * atlas_h * 4) as usize];

    for (slot, &fi) in frame_indices.iter().enumerate() {
        let frame = &shp.frames[fi];
        let fw: u32 = frame.frame_width as u32;
        let fh: u32 = frame.frame_height as u32;
        let x_off: u32 = slot as u32 * max_w;
        // Center the frame within its slot if smaller than max.
        let cx: u32 = (max_w - fw) / 2;
        let cy: u32 = (max_h - fh) / 2;
        for py in 0..fh {
            for px in 0..fw {
                let src_idx: usize = (py * fw + px) as usize;
                let pal_idx: u8 = frame.pixels[src_idx];
                if pal_idx == 0 {
                    continue; // Transparent.
                }
                let color = palette.colors[pal_idx as usize];
                let dst_x: u32 = x_off + cx + px;
                let dst_y: u32 = cy + py;
                let dst: usize = ((dst_y * atlas_w + dst_x) * 4) as usize;
                rgba[dst] = color.r;
                rgba[dst + 1] = color.g;
                rgba[dst + 2] = color.b;
                rgba[dst + 3] = 255;
            }
        }
    }

    // Canvas centering: match gamemd DrawSHP flag 0x200 integer division.
    // actualX = drawX - (int)canvas_w/2 + frame_x
    // actualY = drawY - (int)canvas_h/2 + frame_y
    let ref_frame = &shp.frames[0]; // use frame 0 (empty pip) as reference
    let adj_x: f32 = ref_frame.frame_x as f32 - (shp.width / 2) as f32;
    let adj_y: f32 = ref_frame.frame_y as f32 - (shp.height / 2) as f32;

    let texture: BatchTexture = batch.create_texture(gpu, &rgba, atlas_w, atlas_h);
    log::info!(
        "Pip atlas: {}x{} (frame {}x{}, {} variants, SHP {}x{} canvas_adj=({:.0},{:.0}))",
        atlas_w,
        atlas_h,
        max_w,
        max_h,
        PIP_VARIANT_COUNT,
        shp.width,
        shp.height,
        adj_x,
        adj_y,
    );
    Some((Some(texture), max_w, max_h, adj_x, adj_y))
}

/// Load pips.shp frames 16-18 for unit/infantry health bars into a 3-variant atlas.
///
/// pips.shp frame indices:
///   16 = green pip (healthy)
///   17 = yellow pip (medium health)
///   18 = red pip (critical health)
///
/// Returns (Some(texture), frame_w, frame_h, canvas_adj_x, canvas_adj_y) on success.
fn load_unit_pip_atlas(
    gpu: &GpuContext,
    batch: &BatchRenderer,
    assets: Option<&AssetManager>,
) -> Option<(Option<BatchTexture>, u32, u32, f32, f32)> {
    let assets = assets?;
    let shp_data = assets.get("pips.shp")?;
    let shp = ShpFile::from_bytes(&shp_data).ok()?;
    if shp.frames.len() < 19 {
        log::warn!(
            "pips.shp has fewer than 19 frames ({}), cannot load unit pip atlas",
            shp.frames.len()
        );
        return Some((None, 0, 0, 0.0, 0.0));
    }
    let pal_data = assets
        .get("palette.pal")
        .or_else(|| assets.get("unittem.pal"))?;
    let palette = Palette::from_bytes(&pal_data).ok()?;

    // Atlas slot order: [green(16), yellow(17), red(18)]
    let frame_indices: [usize; UNIT_PIP_VARIANT_COUNT as usize] = [16, 17, 18];
    let mut max_w: u32 = 0;
    let mut max_h: u32 = 0;
    for &fi in &frame_indices {
        let fw: u32 = shp.frames[fi].frame_width as u32;
        let fh: u32 = shp.frames[fi].frame_height as u32;
        if fw > max_w {
            max_w = fw;
        }
        if fh > max_h {
            max_h = fh;
        }
    }

    let atlas_w: u32 = max_w * UNIT_PIP_VARIANT_COUNT;
    let atlas_h: u32 = max_h;
    let mut rgba: Vec<u8> = vec![0u8; (atlas_w * atlas_h * 4) as usize];

    for (slot, &fi) in frame_indices.iter().enumerate() {
        let frame = &shp.frames[fi];
        let fw: u32 = frame.frame_width as u32;
        let fh: u32 = frame.frame_height as u32;
        let x_off: u32 = slot as u32 * max_w;
        let cx: u32 = (max_w - fw) / 2;
        let cy: u32 = (max_h - fh) / 2;
        for py in 0..fh {
            for px in 0..fw {
                let src_idx: usize = (py * fw + px) as usize;
                let pal_idx: u8 = frame.pixels[src_idx];
                if pal_idx == 0 {
                    continue;
                }
                let color = palette.colors[pal_idx as usize];
                let dst_x: u32 = x_off + cx + px;
                let dst_y: u32 = cy + py;
                let dst: usize = ((dst_y * atlas_w + dst_x) * 4) as usize;
                rgba[dst] = color.r;
                rgba[dst + 1] = color.g;
                rgba[dst + 2] = color.b;
                rgba[dst + 3] = 255;
            }
        }
    }

    // Canvas centering adjustment: RA2 DrawSHP uses INTEGER division for centering.
    // Original: actualX = drawX - (int)canvas_w / 2 + frame_x
    // We must match integer division to avoid sub-pixel drift.
    let ref_frame = &shp.frames[16];
    let adj_x: f32 = ref_frame.frame_x as f32 - (shp.width / 2) as f32;
    let adj_y: f32 = ref_frame.frame_y as f32 - (shp.height / 2) as f32;

    let texture: BatchTexture = batch.create_texture(gpu, &rgba, atlas_w, atlas_h);
    log::info!(
        "Unit pip atlas: {}x{} (frame {}x{}, canvas {}x{}, adj ({:.0},{:.0}), frames 16-18)",
        atlas_w,
        atlas_h,
        max_w,
        max_h,
        shp.width,
        shp.height,
        adj_x,
        adj_y,
    );
    Some((Some(texture), max_w, max_h, adj_x, adj_y))
}

/// Load pips.shp frames 6-12 for occupant (garrison) pips into a 7-variant atlas.
///
/// pips.shp frame indices:
///   6 = empty occupant slot (gray)
///   7 = PersonGreen, 8 = PersonYellow, 9 = PersonWhite
///   10 = PersonRed, 11 = PersonBlue, 12 = PersonPurple
///
/// Returns (Some(texture), frame_w, frame_h, canvas_adj_x, canvas_adj_y) on success.
fn load_occupant_pip_atlas(
    gpu: &GpuContext,
    batch: &BatchRenderer,
    assets: Option<&AssetManager>,
) -> Option<(Option<BatchTexture>, u32, u32, f32, f32)> {
    let assets = assets?;
    let shp_data = assets.get("pips.shp")?;
    let shp = ShpFile::from_bytes(&shp_data).ok()?;
    if shp.frames.len() < 13 {
        log::warn!(
            "pips.shp has fewer than 13 frames ({}), cannot load occupant pip atlas",
            shp.frames.len()
        );
        return Some((None, 0, 0, 0.0, 0.0));
    }
    let pal_data = assets
        .get("palette.pal")
        .or_else(|| assets.get("unittem.pal"))?;
    let palette = Palette::from_bytes(&pal_data).ok()?;

    const VARIANT_COUNT: u32 = 7;
    let frame_indices: [usize; VARIANT_COUNT as usize] = [6, 7, 8, 9, 10, 11, 12];
    let mut max_w: u32 = 0;
    let mut max_h: u32 = 0;
    for &fi in &frame_indices {
        let fw: u32 = shp.frames[fi].frame_width as u32;
        let fh: u32 = shp.frames[fi].frame_height as u32;
        if fw > max_w {
            max_w = fw;
        }
        if fh > max_h {
            max_h = fh;
        }
    }

    let atlas_w: u32 = max_w * VARIANT_COUNT;
    let atlas_h: u32 = max_h;
    let mut rgba: Vec<u8> = vec![0u8; (atlas_w * atlas_h * 4) as usize];

    for (slot, &fi) in frame_indices.iter().enumerate() {
        let frame = &shp.frames[fi];
        let fw: u32 = frame.frame_width as u32;
        let fh: u32 = frame.frame_height as u32;
        let x_off: u32 = slot as u32 * max_w;
        let cx: u32 = (max_w - fw) / 2;
        let cy: u32 = (max_h - fh) / 2;
        for py in 0..fh {
            for px in 0..fw {
                let src_idx: usize = (py * fw + px) as usize;
                let pal_idx: u8 = frame.pixels[src_idx];
                if pal_idx == 0 {
                    continue;
                }
                let color = palette.colors[pal_idx as usize];
                let dst_x: u32 = x_off + cx + px;
                let dst_y: u32 = cy + py;
                let dst: usize = ((dst_y * atlas_w + dst_x) * 4) as usize;
                rgba[dst] = color.r;
                rgba[dst + 1] = color.g;
                rgba[dst + 2] = color.b;
                rgba[dst + 3] = 255;
            }
        }
    }

    let ref_frame = &shp.frames[6];
    let adj_x: f32 = ref_frame.frame_x as f32 - (shp.width / 2) as f32;
    let adj_y: f32 = ref_frame.frame_y as f32 - (shp.height / 2) as f32;

    let texture: BatchTexture = batch.create_texture(gpu, &rgba, atlas_w, atlas_h);
    log::info!(
        "Occupant pip atlas: {}x{} (frame {}x{}, {} variants, frames 6-12, adj ({:.0},{:.0}))",
        atlas_w,
        atlas_h,
        max_w,
        max_h,
        VARIANT_COUNT,
        adj_x,
        adj_y,
    );
    Some((Some(texture), max_w, max_h, adj_x, adj_y))
}

/// Number of tiberium pip variants packed into the tiberium pip atlas (empty, ore, gem).
const TIBERIUM_PIP_VARIANT_COUNT: u32 = 3;

/// Load pips2.shp frames 0, 2, 5 for tiberium/cargo pip display on harvesters.
///
/// pips2.shp frame layout (non-building pip display):
///   0 = empty pip (dark)
///   2 = ore pip (green)
///   5 = gem pip (yellow/blue)
///
/// Uses palette.pal for coloring.
fn load_tiberium_pip_atlas(
    gpu: &GpuContext,
    batch: &BatchRenderer,
    assets: Option<&AssetManager>,
) -> Option<(Option<BatchTexture>, u32, u32, f32, f32)> {
    let assets = assets?;
    let shp_data = assets.get("pips2.shp")?;
    let shp = ShpFile::from_bytes(&shp_data).ok()?;
    if shp.frames.len() < 6 {
        log::warn!(
            "pips2.shp has fewer than 6 frames ({}), cannot load tiberium pip atlas",
            shp.frames.len()
        );
        return Some((None, 0, 0, 0.0, 0.0));
    }
    let pal_data = assets
        .get("palette.pal")
        .or_else(|| assets.get("unittem.pal"))?;
    let palette = Palette::from_bytes(&pal_data).ok()?;

    // Atlas slot order: [empty, ore, gem] from pips2.shp frames [0, 2, 5].
    let frame_indices: [usize; TIBERIUM_PIP_VARIANT_COUNT as usize] = [0, 2, 5];
    let mut max_w: u32 = 0;
    let mut max_h: u32 = 0;
    for &fi in &frame_indices {
        let fw: u32 = shp.frames[fi].frame_width as u32;
        let fh: u32 = shp.frames[fi].frame_height as u32;
        if fw > max_w {
            max_w = fw;
        }
        if fh > max_h {
            max_h = fh;
        }
    }

    let atlas_w: u32 = max_w * TIBERIUM_PIP_VARIANT_COUNT;
    let atlas_h: u32 = max_h;
    let mut rgba: Vec<u8> = vec![0u8; (atlas_w * atlas_h * 4) as usize];

    for (slot, &fi) in frame_indices.iter().enumerate() {
        let frame = &shp.frames[fi];
        let fw: u32 = frame.frame_width as u32;
        let fh: u32 = frame.frame_height as u32;
        let x_off: u32 = slot as u32 * max_w;
        let cx: u32 = (max_w - fw) / 2;
        let cy: u32 = (max_h - fh) / 2;
        for py in 0..fh {
            for px in 0..fw {
                let src_idx: usize = (py * fw + px) as usize;
                let pal_idx: u8 = frame.pixels[src_idx];
                if pal_idx == 0 {
                    continue;
                }
                let color = palette.colors[pal_idx as usize];
                let dst_x: u32 = x_off + cx + px;
                let dst_y: u32 = cy + py;
                let dst: usize = ((dst_y * atlas_w + dst_x) * 4) as usize;
                rgba[dst] = color.r;
                rgba[dst + 1] = color.g;
                rgba[dst + 2] = color.b;
                rgba[dst + 3] = 255;
            }
        }
    }

    let ref_frame = &shp.frames[0];
    let adj_x: f32 = ref_frame.frame_x as f32 - (shp.width / 2) as f32;
    let adj_y: f32 = ref_frame.frame_y as f32 - (shp.height / 2) as f32;

    let texture: BatchTexture = batch.create_texture(gpu, &rgba, atlas_w, atlas_h);
    log::info!(
        "Tiberium pip atlas: {}x{} (frame {}x{}, {} variants, pips2.shp frames [0,2,5], adj ({:.0},{:.0}))",
        atlas_w,
        atlas_h,
        max_w,
        max_h,
        TIBERIUM_PIP_VARIANT_COUNT,
        adj_x,
        adj_y,
    );
    Some((Some(texture), max_w, max_h, adj_x, adj_y))
}

/// Load pipbrd.shp and pack 2 health bar background frames into a horizontal strip atlas.
///
/// pipbrd.shp frame layout:
///   0 = vehicle/aircraft health bar background (36×4)
///   1 = infantry health bar background (18×4)
///
/// Uses palette.pal for coloring.
/// Returns (Some(texture), vehicle_w, infantry_w, height, atlas_w,
///          veh_adj_x, veh_adj_y, inf_adj_x, inf_adj_y) or failure tuple.
fn load_pipbrd_atlas(
    gpu: &GpuContext,
    batch: &BatchRenderer,
    assets: Option<&AssetManager>,
) -> Option<(Option<BatchTexture>, u32, u32, u32, u32, f32, f32, f32, f32)> {
    let assets = assets?;
    let shp_data = assets.get("pipbrd.shp")?;
    let shp = ShpFile::from_bytes(&shp_data).ok()?;
    if shp.frames.len() < 2 {
        log::warn!("pipbrd.shp has fewer than 2 frames ({})", shp.frames.len());
        return Some((None, 0, 0, 0, 0, 0.0, 0.0, 0.0, 0.0));
    }
    log::info!(
        "pipbrd.shp: {}x{}, {} frames",
        shp.width,
        shp.height,
        shp.frames.len()
    );

    let pal_data = assets
        .get("palette.pal")
        .or_else(|| assets.get("unittem.pal"))?;
    let palette = Palette::from_bytes(&pal_data).ok()?;

    let f0 = &shp.frames[0];
    let f1 = &shp.frames[1];
    let w0: u32 = f0.frame_width as u32;
    let h0: u32 = f0.frame_height as u32;
    let w1: u32 = f1.frame_width as u32;
    let h1: u32 = f1.frame_height as u32;
    let atlas_w: u32 = w0 + w1;
    let atlas_h: u32 = h0.max(h1);
    let mut rgba: Vec<u8> = vec![0u8; (atlas_w * atlas_h * 4) as usize];

    // Blit frame 0 (vehicle) at x=0.
    for py in 0..h0 {
        for px in 0..w0 {
            let src: usize = (py * w0 + px) as usize;
            let pal_idx: u8 = f0.pixels[src];
            if pal_idx == 0 {
                continue;
            }
            let color = palette.colors[pal_idx as usize];
            let dst: usize = ((py * atlas_w + px) * 4) as usize;
            rgba[dst] = color.r;
            rgba[dst + 1] = color.g;
            rgba[dst + 2] = color.b;
            rgba[dst + 3] = 255;
        }
    }

    // Blit frame 1 (infantry) at x=w0.
    for py in 0..h1 {
        for px in 0..w1 {
            let src: usize = (py * w1 + px) as usize;
            let pal_idx: u8 = f1.pixels[src];
            if pal_idx == 0 {
                continue;
            }
            let color = palette.colors[pal_idx as usize];
            let dst_x: u32 = w0 + px;
            let dst: usize = ((py * atlas_w + dst_x) * 4) as usize;
            rgba[dst] = color.r;
            rgba[dst + 1] = color.g;
            rgba[dst + 2] = color.b;
            rgba[dst + 3] = 255;
        }
    }

    // Canvas centering adjustments: RA2 DrawSHP uses INTEGER division for centering.
    // Original: actualX = drawX - (int)canvas_w / 2 + frame_x
    let canvas_cx: f32 = (shp.width / 2) as f32;
    let canvas_cy: f32 = (shp.height / 2) as f32;
    let veh_adj_x: f32 = f0.frame_x as f32 - canvas_cx;
    let veh_adj_y: f32 = f0.frame_y as f32 - canvas_cy;
    let inf_adj_x: f32 = f1.frame_x as f32 - canvas_cx;
    let inf_adj_y: f32 = f1.frame_y as f32 - canvas_cy;

    let texture: BatchTexture = batch.create_texture(gpu, &rgba, atlas_w, atlas_h);
    log::info!(
        "pipbrd atlas: {}x{} (vehicle {}x{} adj({:.0},{:.0}), infantry {}x{} adj({:.0},{:.0}), canvas {}x{})",
        atlas_w,
        atlas_h,
        w0,
        h0,
        veh_adj_x,
        veh_adj_y,
        w1,
        h1,
        inf_adj_x,
        inf_adj_y,
        shp.width,
        shp.height,
    );
    Some((
        Some(texture),
        w0,
        w1,
        atlas_h,
        atlas_w,
        veh_adj_x,
        veh_adj_y,
        inf_adj_x,
        inf_adj_y,
    ))
}

/// Load place.shp (building placement overlay) from RA2 assets.
///
/// Frame 0 = valid placement cell, frame 1 = invalid placement cell.
/// The SHP uses palette index 125 for the diamond shape — we extract the shape
/// as an alpha mask and tint it green (valid) or red (invalid) so it's visible
/// regardless of which palette is loaded.
/// Returns (valid_texture, invalid_texture, canvas_w, canvas_h) or None if not available.
fn load_place_textures(
    gpu: &GpuContext,
    batch: &BatchRenderer,
    assets: Option<&AssetManager>,
) -> Option<(BatchTexture, BatchTexture, f32, f32)> {
    let assets = assets?;
    let shp_data = assets.get("place.shp")?;
    let shp = ShpFile::from_bytes(&shp_data).ok()?;
    if shp.frames.len() < 2 {
        log::warn!("place.shp has fewer than 2 frames ({})", shp.frames.len());
        return None;
    }

    // Render a place.shp frame into its full canvas (shp.width × shp.height),
    // then use the standard SHP centering formula so the diamond aligns with
    // the cell the same way buildings do. The caller draws it at iso_to_screen()
    // with size = (canvas_w, canvas_h) and offset matching the original engine's formula:
    //   position = cellTopLeft + (CellSizeX/2 - canvasW/2 + frameX, CellSizeY/2 - canvasH/2 + frameY)
    // But since we blit into the full canvas, frameX/Y are baked in, and the
    // offset simplifies to (CellSizeX/2 - canvasW/2, CellSizeY/2 - canvasH/2).
    let canvas_w: u32 = shp.width as u32;
    let canvas_h: u32 = shp.height as u32;
    let render_tinted = |frame_idx: usize, color: [u8; 4]| -> Option<BatchTexture> {
        let frame = &shp.frames[frame_idx];
        let fw: u32 = frame.frame_width as u32;
        let fh: u32 = frame.frame_height as u32;
        if fw == 0 || fh == 0 {
            return None;
        }
        let mut rgba: Vec<u8> = vec![0u8; (canvas_w * canvas_h * 4) as usize];
        let fx: u32 = frame.frame_x as u32;
        let fy: u32 = frame.frame_y as u32;
        for py in 0..fh {
            for px in 0..fw {
                let src_idx: usize = (py * fw + px) as usize;
                let palette_index: u8 = frame.pixels[src_idx];
                if palette_index == 0 {
                    continue;
                }
                let dst_x: u32 = fx + px;
                let dst_y: u32 = fy + py;
                if dst_x >= canvas_w || dst_y >= canvas_h {
                    continue;
                }
                let dst_idx: usize = ((dst_y * canvas_w + dst_x) * 4) as usize;
                rgba[dst_idx] = color[0];
                rgba[dst_idx + 1] = color[1];
                rgba[dst_idx + 2] = color[2];
                rgba[dst_idx + 3] = color[3];
            }
        }
        Some(batch.create_texture(gpu, &rgba, canvas_w, canvas_h))
    };

    let valid_tex = render_tinted(0, PREVIEW_VALID_COLOR)?;
    let invalid_tex = render_tinted(1, PREVIEW_INVALID_COLOR)?;
    log::info!(
        "Loaded place.shp for building placement overlay ({}x{}, {} frames)",
        shp.width,
        shp.height,
        shp.frames.len()
    );
    Some((valid_tex, invalid_tex, canvas_w as f32, canvas_h as f32))
}

/// Generate a 1px-thick diamond outline texture (TILE_WIDTH x TILE_HEIGHT).
/// White outline on transparent background — tinted per-instance for debug coloring.
fn generate_diamond_outline() -> Vec<u8> {
    let w: u32 = TILE_WIDTH as u32;
    let h: u32 = TILE_HEIGHT as u32;
    let cx: f32 = w as f32 / 2.0;
    let cy: f32 = h as f32 / 2.0;
    let mut rgba: Vec<u8> = vec![0u8; (w * h * 4) as usize];
    for py in 0..h {
        for px in 0..w {
            let dist: f32 = (px as f32 - cx).abs() / cx + (py as f32 - cy).abs() / cy;
            // Draw pixels near the diamond edge (within ~1.5px of dist=1.0).
            let edge_dist: f32 = (dist - 1.0).abs() * cx;
            if edge_dist < 1.5 {
                let alpha: f32 = (1.5 - edge_dist).min(1.0);
                let idx: usize = ((py * w + px) * 4) as usize;
                rgba[idx] = 255;
                rgba[idx + 1] = 255;
                rgba[idx + 2] = 255;
                rgba[idx + 3] = (alpha * 255.0) as u8;
            }
        }
    }
    rgba
}

/// Generate an isometric diamond-shaped cell texture (TILE_WIDTH x TILE_HEIGHT).
/// The diamond inscribes the tile rectangle with 1px anti-aliased edges.
fn generate_diamond_cell(color: [u8; 4]) -> Vec<u8> {
    let w: u32 = TILE_WIDTH as u32;
    let h: u32 = TILE_HEIGHT as u32;
    let cx: f32 = (w as f32) / 2.0;
    let cy: f32 = (h as f32) / 2.0;
    let mut rgba: Vec<u8> = vec![0u8; (w * h * 4) as usize];
    for py in 0..h {
        for px in 0..w {
            // Normalized diamond distance: 0 at center, 1 at edge.
            let dist: f32 = (px as f32 - cx).abs() / cx + (py as f32 - cy).abs() / cy;
            if dist <= 1.0 {
                // Anti-alias: fade out over the last ~1px before the edge.
                let edge_fade: f32 = ((1.0 - dist) * cx).min(1.0);
                let alpha: u8 = (color[3] as f32 * edge_fade) as u8;
                let idx: usize = ((py * w + px) * 4) as usize;
                rgba[idx] = color[0];
                rgba[idx + 1] = color[1];
                rgba[idx + 2] = color[2];
                rgba[idx + 3] = alpha;
            }
        }
    }
    rgba
}
