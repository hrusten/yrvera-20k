//! Draw pass dispatch — creates the wgpu render pass and issues all draw calls in order.
//!
//! Separated from the instance-building phase in mod.rs so the orchestrator stays focused
//! on *what* to render while this module handles *how* to submit it to the GPU.
//!
//! ## Dependency rules
//! - Internal to app_render — only called from mod.rs via `dispatch_draw_passes()`.

use crate::app::AppState;
use crate::app_sidebar_render::{
    begin_main_pass, current_sidebar_chrome_texture, current_sidebar_gclock_texture,
};
use crate::app_ui_overlays::current_software_cursor_texture;
use crate::render::batch::{BatchRenderer, BatchTexture, InstanceBufferPool, SpriteInstance};
use crate::render::bridge_atlas::BridgeAtlas;
use crate::render::overlay_atlas::OverlayAtlas;
use crate::render::tile_atlas::TileAtlas;

use super::merge_passes;

/// Data from the instance-building phase that the draw pass needs beyond `AppState`.
///
/// These are local variables in `render_game()` that can't be accessed through `state`
/// because they're computed fresh each frame and (for the merge passes) need CPU-side
/// depth values that match the uploaded GPU buffers.
pub(super) struct DrawPassData<'a> {
    pub bridge_unit_instances: &'a [SpriteInstance],
    pub bridge_shp_paged: &'a [Vec<SpriteInstance>],
    pub unit_instances: &'a [SpriteInstance],
    pub shp_paged: &'a [Vec<SpriteInstance>],
    pub wall_instances: &'a [SpriteInstance],
    pub ghost_page: u8,
}

/// Create the main render pass and dispatch all draw calls in the correct order.
///
/// Draw order follows the original engine's layered rendering:
/// 1. Terrain (zdepth) → 2. Bridge body (zdepth) → 3. Overlays (passthrough) →
/// 4. Bridge entities (merge) → 5. Ground objects (merge) → 6. Turrets →
/// 7. Cliff redraw (zdepth) → 8. Debug → 9. Shroud/fog → 10. UI/sidebar
pub(super) fn dispatch_draw_passes(
    state: &AppState,
    encoder: &mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    data: &DrawPassData<'_>,
) {
    let pool: &InstanceBufferPool = &state.instance_pool;
    let mut pass = begin_main_pass(encoder, view, &state.depth_view);

    // --- Step 1: Terrain (Z-depth pipeline for per-pixel depth from TMP Z-data) ---
    draw_pooled_zdepth(
        &mut pass,
        &state.batch_renderer,
        pool,
        state.tile_atlas.as_ref(),
        "terrain",
    );

    // --- Step 2: Bridge body (Z-depth pipeline) ---
    draw_pooled_bridge_zdepth(
        &mut pass,
        &state.batch_renderer,
        pool,
        state.bridge_atlas.as_ref(),
        "overlay_bridge_body",
    );

    // --- Step 3: Overlays (no depth test — passthrough) ---
    // Overlays don't read the Z-buffer — the tile blitter skips Z-testing
    // for tiles without Z-data (flag 0x02 clear at cell header byte 36).
    // Overlays paint unconditionally over terrain. Without
    // passthrough, adjacent terrain tiles from closer iso rows would
    // occlude overlays via LessEqual depth test ("sinking into ground").
    // Cliff occlusion for overlays comes from the cliff redraw pass (step 7).
    //
    // Walls are NOT drawn here — they participate in the Y-sorted merge
    // (step 5) so they correctly interleave with buildings by depth.

    draw_pooled_passthrough_overlay(
        &mut pass,
        &state.batch_renderer,
        pool,
        state.overlay_atlas.as_ref(),
        "overlay_bridge_detail",
    );
    // Non-wall overlays (ore, trees, terrain objects) — no depth test.
    draw_pooled_passthrough_overlay(
        &mut pass,
        &state.batch_renderer,
        pool,
        state.overlay_atlas.as_ref(),
        "overlay",
    );

    // --- Step 4: Bridge entities (multi-way Y-merge) ---
    merge_passes::draw_merged_bridge_occluded_pass(
        &mut pass,
        &state.batch_renderer,
        pool,
        data.bridge_unit_instances,
        data.bridge_shp_paged,
        state.unit_atlas.as_ref(),
        state.sprite_atlas.as_ref(),
    );

    // --- Step 5: Ground objects (unified multi-way Y-merge) ---
    // All ground objects Y-sorted together (Layer 2).
    // Walls are included so they correctly appear in front of units at
    // closer iso rows (walls render in both terrain pass and object pass --
    // the object pass rendering provides Y-sorted priority).
    merge_passes::draw_merged_object_pass(
        &mut pass,
        &state.batch_renderer,
        pool,
        data.unit_instances,
        data.shp_paged,
        data.wall_instances,
        state.unit_atlas.as_ref(),
        state.sprite_atlas.as_ref(),
        state.overlay_atlas.as_ref(),
    );

    // --- Step 6: Building turrets ---
    // Drawn AFTER all layer-2 objects (separate turret pass).
    if let Some(unit_atlas) = state.unit_atlas.as_ref() {
        if let Some((buf, count)) = pool.get("building_turret") {
            state.batch_renderer.draw_passthrough_range(
                &mut pass,
                &unit_atlas.texture,
                buf,
                0,
                count,
            );
        }
    }

    // --- Step 7: Cliff redraw ---
    // Cliff terrain tiles redrawn AFTER sprites using zdepth shader + Less compare.
    // Only cliff face pixels (z_sample > 0) pass the depth test — their frag_depth
    // is pushed closer than the terrain depth written in step 1. Flat ground pixels
    // (z_sample = 0) have equal frag_depth and fail Less, preserving sprites near
    // cliff edges.
    draw_pooled_zdepth(
        &mut pass,
        &state.batch_renderer,
        pool,
        state.tile_atlas.as_ref(),
        "terrain_cliff",
    );

    // --- Step 8: Debug overlays ---
    // Drawn above entities, below fog and UI.
    // Use filled-diamond texture so cells appear as isometric diamonds, not rectangles.
    let debug_diamond_tex = state
        .selection_overlay
        .as_ref()
        .map(|o| o.diamond_filled_texture());
    draw_pooled_no_depth(
        &mut pass,
        &state.batch_renderer,
        pool,
        debug_diamond_tex,
        "debug_pathgrid",
    );
    let grid_tex = state
        .selection_overlay
        .as_ref()
        .map(|o| o.diamond_outline_texture());
    draw_pooled_no_depth(
        &mut pass,
        &state.batch_renderer,
        pool,
        grid_tex,
        "debug_cell_grid",
    );
    draw_pooled_no_depth(
        &mut pass,
        &state.batch_renderer,
        pool,
        debug_diamond_tex,
        "debug_path",
    );
    draw_pooled_no_depth(
        &mut pass,
        &state.batch_renderer,
        pool,
        debug_diamond_tex,
        "debug_heightmap",
    );

    // --- Step 9: Shroud (GPU ABuffer multiply pass) ---
    // Darkens every scene pixel by the shroud brightness value via
    // per-pixel ABuffer lookup.
    // Fully shrouded areas → black, edge cells → gradient, explored → no change.
    if let Some(ref buf) = state.shroud_buffer {
        if !state.sandbox_full_visibility {
            buf.draw(&mut pass);
        }
    }

    // --- Step 10: UI elements ---
    // Target/action lines from selected units to command destinations.
    let bracket_tex = state.selection_overlay.as_ref().map(|o| o.white_texture());
    draw_pooled_no_depth(
        &mut pass,
        &state.batch_renderer,
        pool,
        bracket_tex,
        "target_lines",
    );
    // Isometric selection brackets for buildings: white 1px stub lines at 3 roof corners.
    draw_pooled_no_depth(
        &mut pass,
        &state.batch_renderer,
        pool,
        bracket_tex,
        "selection_brackets",
    );
    // Building health pips: discrete pips from pips.shp atlas.
    let building_status_tex = state
        .selection_overlay
        .as_ref()
        .map(|o| o.pip_texture().unwrap_or_else(|| o.white_texture()));
    draw_pooled_no_depth(
        &mut pass,
        &state.batch_renderer,
        pool,
        building_status_tex,
        "status_building",
    );
    // Occupant pips for garrisoned buildings (pips.shp frames 6-12).
    let occupant_pip_tex = state.selection_overlay.as_ref().map(|o| {
        o.occupant_pip_texture()
            .unwrap_or_else(|| o.white_texture())
    });
    draw_pooled_no_depth(
        &mut pass,
        &state.batch_renderer,
        pool,
        occupant_pip_tex,
        "occupant_pips",
    );
    // Non-building health bar backgrounds: pipbrd.shp bracket sprites.
    let unit_bg_tex = state
        .selection_overlay
        .as_ref()
        .and_then(|o| o.pipbrd_texture());
    draw_pooled_no_depth(
        &mut pass,
        &state.batch_renderer,
        pool,
        unit_bg_tex,
        "status_unit_bg",
    );
    // Non-building health bar fills: individual pip sprites from pips.shp (or white_texture fallback).
    let unit_fill_tex = state
        .selection_overlay
        .as_ref()
        .map(|o| o.unit_pip_texture().unwrap_or_else(|| o.white_texture()));
    draw_pooled_no_depth(
        &mut pass,
        &state.batch_renderer,
        pool,
        unit_fill_tex,
        "status_unit_fill",
    );
    // Tiberium cargo pips for harvesters (pips2.shp frames 0, 2, 5).
    let cargo_pip_tex = state.selection_overlay.as_ref().map(|o| {
        o.tiberium_pip_texture()
            .unwrap_or_else(|| o.white_texture())
    });
    draw_pooled_no_depth(
        &mut pass,
        &state.batch_renderer,
        pool,
        cargo_pip_tex,
        "cargo_pips",
    );
    // Drag rectangle — screen-fixed, use UI camera (zoom=1.0).
    let drag_tex = state.selection_overlay.as_ref().map(|o| o.drag_texture());
    draw_pooled_ui(&mut pass, &state.batch_renderer, pool, drag_tex, "drag");
    // Placement preview — world-space, uses world camera (zoom).
    let ghost_tex = state
        .sprite_atlas
        .as_ref()
        .and_then(|a| a.page(data.ghost_page as usize))
        .map(|p| &p.texture);
    draw_pooled_no_depth(
        &mut pass,
        &state.batch_renderer,
        pool,
        ghost_tex,
        "placement_ghost",
    );
    let wall_ghost_tex = state.overlay_atlas.as_ref().map(|a| &a.texture);
    draw_pooled_no_depth(
        &mut pass,
        &state.batch_renderer,
        pool,
        wall_ghost_tex,
        "placement_wall_ghost",
    );
    let valid_tex = state
        .selection_overlay
        .as_ref()
        .map(|o| o.preview_valid_texture());
    draw_pooled_no_depth(
        &mut pass,
        &state.batch_renderer,
        pool,
        valid_tex,
        "placement_valid",
    );
    let invalid_tex = state
        .selection_overlay
        .as_ref()
        .map(|o| o.preview_invalid_texture());
    draw_pooled_no_depth(
        &mut pass,
        &state.batch_renderer,
        pool,
        invalid_tex,
        "placement_invalid",
    );
    // --- Screen-fixed UI: sidebar, minimap, cursor — use UI camera (zoom=1.0) ---
    draw_pooled_ui(
        &mut pass,
        &state.batch_renderer,
        pool,
        state.minimap.as_ref().map(|m| m.map_texture()),
        "minimap",
    );
    draw_pooled_ui(
        &mut pass,
        &state.batch_renderer,
        pool,
        state.minimap.as_ref().map(|m| m.white_texture()),
        "viewport_rect",
    );
    draw_pooled_ui(
        &mut pass,
        &state.batch_renderer,
        pool,
        state.minimap.as_ref().map(|m| m.white_texture()),
        "sidebar",
    );
    draw_pooled_ui(
        &mut pass,
        &state.batch_renderer,
        pool,
        current_sidebar_chrome_texture(state),
        "sidebar_chrome",
    );
    draw_pooled_ui(
        &mut pass,
        &state.batch_renderer,
        pool,
        state.radar_anim.as_ref().map(|ra| ra.texture()),
        "radar_anim",
    );
    draw_pooled_ui(
        &mut pass,
        &state.batch_renderer,
        pool,
        state
            .sidebar_cameo_atlas
            .as_ref()
            .map(|atlas| &atlas.texture),
        "sidebar_cameo",
    );
    draw_pooled_ui(
        &mut pass,
        &state.batch_renderer,
        pool,
        current_sidebar_gclock_texture(state),
        "sidebar_gclock",
    );
    let cameo_overlay_tex = state
        .sidebar_text
        .darken_texture()
        .or_else(|| state.selection_overlay.as_ref().map(|o| o.white_texture()));
    draw_pooled_ui(
        &mut pass,
        &state.batch_renderer,
        pool,
        cameo_overlay_tex,
        "sidebar_cameo_overlay",
    );
    draw_pooled_ui(
        &mut pass,
        &state.batch_renderer,
        pool,
        Some(state.sidebar_text.texture()),
        "sidebar_text",
    );
    draw_pooled_ui(
        &mut pass,
        &state.batch_renderer,
        pool,
        current_software_cursor_texture(state),
        "software_cursor",
    );
}

// ---------------------------------------------------------------------------
// Draw helpers — thin wrappers around BatchRenderer methods with atlas lookup
// ---------------------------------------------------------------------------

/// Draw a pooled buffer with the Z-depth pipeline (per-pixel frag_depth).
/// Uses the tile atlas's pre-built zdepth bind group (color + R8 depth textures).
fn draw_pooled_zdepth<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    batch: &'a BatchRenderer,
    pool: &'a InstanceBufferPool,
    atlas: Option<&'a TileAtlas>,
    key: &'static str,
) {
    if let (Some(a), Some((buf, count))) = (atlas, pool.get(key)) {
        batch.draw_with_buffer_zdepth(pass, &a.zdepth_bind_group, buf, count);
    }
}

fn draw_pooled_bridge_zdepth<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    batch: &'a BatchRenderer,
    pool: &'a InstanceBufferPool,
    atlas: Option<&'a BridgeAtlas>,
    key: &'static str,
) {
    if let (Some(a), Some((buf, count))) = (atlas, pool.get(key)) {
        batch.draw_with_buffer_zdepth(pass, &a.zdepth_bind_group, buf, count);
    }
}

/// Draw a pooled buffer with LessEqual depth test, depth write ON.
/// Used for cliff redraw (must write depth) and UI/debug passes.
fn draw_pooled_no_depth<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    batch: &'a BatchRenderer,
    pool: &'a InstanceBufferPool,
    tex: Option<&'a BatchTexture>,
    key: &'static str,
) {
    if let (Some(t), Some((buf, count))) = (tex, pool.get(key)) {
        batch.draw_with_buffer_no_depth(pass, t, buf, count);
    }
}

/// Draw with the UI camera (zoom=1.0) for screen-fixed elements.
/// Uses the overlay pipeline (no depth) but sets bind group 0 to the UI camera
/// so sidebar, minimap, and cursor stay at fixed screen positions regardless of zoom.
fn draw_pooled_ui<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    batch: &'a BatchRenderer,
    pool: &'a InstanceBufferPool,
    tex: Option<&'a BatchTexture>,
    key: &'static str,
) {
    if let (Some(t), Some((buf, count))) = (tex, pool.get(key)) {
        if count == 0 {
            return;
        }
        pass.set_pipeline(batch.overlay_pipeline());
        pass.set_bind_group(0, batch.ui_camera_bind_group(), &[]);
        pass.set_bind_group(1, &t.bind_group, &[]);
        pass.set_vertex_buffer(0, buf.slice(..));
        pass.draw(0..6, 0..count);
    }
}

/// Draw non-wall overlays with depth test bypassed (Always compare).
/// Tiles without embedded Z-data skip Z-testing.
/// Uses the overlay atlas's regular texture bind group (not zdepth_bind_group).
fn draw_pooled_passthrough_overlay<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    batch: &'a BatchRenderer,
    pool: &'a InstanceBufferPool,
    atlas: Option<&'a OverlayAtlas>,
    key: &'static str,
) {
    if let (Some(a), Some((buf, count))) = (atlas, pool.get(key)) {
        batch.draw_with_buffer_passthrough(pass, &a.texture, buf, count);
    }
}
