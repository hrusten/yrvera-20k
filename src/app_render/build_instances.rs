//! Per-frame sprite instance builders — grouped by rendering phase.
//!
//! Each function builds one phase of the rendering pipeline and returns a struct
//! holding the instance vectors. This makes the pipeline flow in `render_game()`
//! scannable: build world → build debug → build UI → build sidebar → build fog.
//!
//! ## Dependency rules
//! - Internal to app_render — only called from mod.rs.

use crate::app::AppState;
use crate::app_debug_overlays;
use crate::app_instances;
use crate::app_sidebar_render::{
    active_minimap_screen_rect, build_sidebar_cameo_instances, build_sidebar_chrome_instances,
    build_sidebar_instances as sidebar_inst_fn, build_sidebar_text_instances, current_sidebar_view,
};
use crate::app_ui_overlays::{
    build_building_status_instances, build_cargo_pip_instances, build_occupant_pip_instances,
    build_software_cursor_instances, build_unit_status_bg_instances,
    build_unit_status_fill_instances,
};
use crate::map::terrain::{self, TilePlacement};
use crate::map::theater::TileKey;
use crate::render::batch::SpriteInstance;
use crate::sidebar::SidebarView;

// ---------------------------------------------------------------------------
// Phase structs — group related instance vectors for clean data flow
// ---------------------------------------------------------------------------

/// Game-world sprite instances: terrain tiles, overlays, entities, bridges.
pub(super) struct WorldInstances {
    pub terrain: terrain::TerrainInstances,
    pub overlay: Vec<SpriteInstance>,
    pub bridge_detail: Vec<SpriteInstance>,
    pub bridge_body: Vec<SpriteInstance>,
    pub wall: Vec<SpriteInstance>,
    pub unit: Vec<SpriteInstance>,
    pub bridge_unit: Vec<SpriteInstance>,
    pub shp_paged: Vec<Vec<SpriteInstance>>,
    pub bridge_shp_paged: Vec<Vec<SpriteInstance>>,
    pub building_turret: Vec<SpriteInstance>,
}

/// Debug visualization overlays (toggled by hotkeys at runtime).
pub(super) struct DebugInstances {
    pub pathgrid: Vec<SpriteInstance>,
    pub cell_grid: Vec<SpriteInstance>,
    pub path: Vec<SpriteInstance>,
    pub heightmap: Vec<SpriteInstance>,
}

/// In-game UI overlays: selection brackets, health bars, placement preview, cursor.
pub(super) struct UiInstances {
    pub bracket: Vec<SpriteInstance>,
    pub building_status: Vec<SpriteInstance>,
    pub occupant_pip: Vec<SpriteInstance>,
    pub unit_status_bg: Vec<SpriteInstance>,
    pub unit_status_fill: Vec<SpriteInstance>,
    pub cargo_pip: Vec<SpriteInstance>,
    pub software_cursor: Vec<SpriteInstance>,
    pub drag: Vec<SpriteInstance>,
    pub placement_valid: Vec<SpriteInstance>,
    pub placement_invalid: Vec<SpriteInstance>,
    pub placement_ghost: Vec<SpriteInstance>,
    pub ghost_page: u8,
    pub wall_ghost: Vec<SpriteInstance>,
    pub target_line: Vec<SpriteInstance>,
}

/// Sidebar chrome, cameos, text, minimap, and radar animation.
pub(super) struct SidebarInstances {
    pub sidebar: Vec<SpriteInstance>,
    pub chrome: Vec<SpriteInstance>,
    pub cameo: Vec<SpriteInstance>,
    pub cameo_overlay: Vec<SpriteInstance>,
    pub text: Vec<SpriteInstance>,
    pub minimap: Vec<SpriteInstance>,
    pub viewport_rect: Vec<SpriteInstance>,
    pub radar_anim: Vec<SpriteInstance>,
    pub view: Option<SidebarView>,
}

// ---------------------------------------------------------------------------
// Phase 1: Game world (terrain, overlays, entities)
// ---------------------------------------------------------------------------

/// Build all game-world sprite instances: terrain tiles, map overlays, bridges,
/// VXL units, SHP buildings/infantry, world effects, damage fires.
/// All instance vectors are Y-sorted (depth descending) for correct draw order.
pub(super) fn build_world_instances(state: &mut AppState, sw: f32, sh: f32) -> WorldInstances {
    // Terrain tiles — look up atlas UVs with variant fallback.
    let uv_fn_closure;
    let uv_fn: Option<&dyn Fn(u16, u8, u8) -> Option<TilePlacement>> =
        if let Some(atlas) = &state.tile_atlas {
            uv_fn_closure = |tile_id: u16, sub_tile: u8, variant: u8| -> Option<TilePlacement> {
                let key = TileKey {
                    tile_id,
                    sub_tile,
                    variant,
                };
                let key_main = TileKey {
                    tile_id,
                    sub_tile,
                    variant: 0,
                };
                let uv = atlas.get_uv(key).or_else(|| atlas.get_uv(key_main))?;
                Some(TilePlacement {
                    uv_origin: uv.uv_origin,
                    uv_size: uv.uv_size,
                    pixel_size: uv.pixel_size,
                    draw_offset: uv.draw_offset,
                })
            };
            Some(&uv_fn_closure)
        } else {
            None
        };
    let terrain = if let Some(grid) = &state.terrain_grid {
        // Skip terrain for fully shrouded cells — matches gamemd which doesn't
        // render terrain under shroud. The multiply pass still darkens edges.
        let local_owner_name = crate::app_commands::preferred_local_owner_name(state);
        let fog_vis: Option<(
            crate::sim::intern::InternedId,
            &crate::sim::vision::FogState,
        )> = if state.sandbox_full_visibility {
            None
        } else if let (Some(sim), Some(owner)) = (&state.simulation, &local_owner_name) {
            sim.interner.get(owner).map(|id| (id, &sim.fog))
        } else {
            None
        };
        terrain::build_visible_instances(
            grid,
            state.camera_x,
            state.camera_y,
            sw,
            sh,
            uv_fn,
            fog_vis,
        )
    } else {
        terrain::TerrainInstances {
            normal: Vec::new(),
            cliff_redraw: Vec::new(),
        }
    };

    // Overlays: map overlays, bridges, walls — each sorted by depth descending.
    let mut overlay: Vec<SpriteInstance> = std::mem::take(&mut state.cached_overlay_instances);
    overlay.clear();
    let mut bridge_detail: Vec<SpriteInstance> = Vec::new();
    let mut bridge_body: Vec<SpriteInstance> = Vec::new();
    let mut wall: Vec<SpriteInstance> = Vec::new();
    app_instances::build_overlay_instances(
        state,
        sw,
        sh,
        &mut overlay,
        &mut bridge_detail,
        &mut bridge_body,
        &mut wall,
    );
    sort_by_depth_desc(&mut overlay);
    sort_by_depth_desc(&mut bridge_detail);
    sort_by_depth_desc(&mut bridge_body);
    sort_by_depth_desc(&mut wall);

    // SHP sprites: buildings, infantry, effects — paged across sprite atlas pages.
    let shp_page_count: usize = state
        .sprite_atlas
        .as_ref()
        .map_or(1, |a| a.page_count().max(1));
    let mut shp_paged: Vec<Vec<SpriteInstance>> = vec![Vec::new(); shp_page_count];
    let mut bridge_shp_paged: Vec<Vec<SpriteInstance>> = vec![Vec::new(); shp_page_count];

    // VXL units (ground + bridge) — sorted by depth descending.
    // shp_paged is passed in so harvest overlays (OREGATH SHP) route to the
    // correct sprite atlas page instead of the voxel unit instance list.
    let mut unit: Vec<SpriteInstance> = std::mem::take(&mut state.cached_unit_instances);
    unit.clear();
    let mut bridge_unit: Vec<SpriteInstance> = Vec::new();
    app_instances::build_unit_instances(state, &mut unit, &mut bridge_unit, &mut shp_paged);
    sort_by_depth_desc(&mut unit);
    sort_by_depth_desc(&mut bridge_unit);
    // Building turret VXLs use the unit atlas texture but must draw AFTER all
    // layer-2 objects (separate turret pass after layer 2).
    let mut building_turret: Vec<SpriteInstance> = Vec::new();
    app_instances::build_shp_instances(
        state,
        &mut shp_paged,
        &mut bridge_shp_paged,
        &mut building_turret,
    );
    app_instances::build_world_effect_instances(state, &mut shp_paged);
    // Damage fires Y-sort with buildings (Layer 2).
    app_instances::build_damage_fire_instances(state, &mut shp_paged);
    // Garrison muzzle flashes (OccupantAnim) at fire port positions.
    app_instances::build_garrison_muzzle_flash_instances(state, &mut shp_paged);
    for page in &mut shp_paged {
        sort_by_y_asc(page);
    }
    for page in &mut bridge_shp_paged {
        sort_by_y_asc(page);
    }

    // One-time first-frame statistics.
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        let total_grid: usize = state.terrain_grid.as_ref().map_or(0, |g| g.cells.len());
        log::info!(
            "First frame: {} terrain ({} cliff redraw, of {} cells) + {} overlay + {} vxl + {} shp",
            terrain.normal.len(),
            terrain.cliff_redraw.len(),
            total_grid,
            overlay.len() + bridge_detail.len() + bridge_body.len(),
            unit.len(),
            shp_paged.iter().map(|p| p.len()).sum::<usize>(),
        );
    }

    WorldInstances {
        terrain,
        overlay,
        bridge_detail,
        bridge_body,
        wall,
        unit,
        bridge_unit,
        shp_paged,
        bridge_shp_paged,
        building_turret,
    }
}

// ---------------------------------------------------------------------------
// Phase 2: Debug overlays
// ---------------------------------------------------------------------------

/// Build debug visualization instances (only when toggled on via hotkeys).
pub(super) fn build_debug_instances(state: &AppState, sw: f32, sh: f32) -> DebugInstances {
    DebugInstances {
        pathgrid: if state.debug_show_pathgrid {
            app_debug_overlays::build_terrain_cost_overlay_instances(state, sw, sh)
        } else {
            Vec::new()
        },
        cell_grid: if state.debug_show_cell_grid {
            app_debug_overlays::build_cell_grid_overlay_instances(state, sw, sh)
        } else {
            Vec::new()
        },
        path: if state.debug_show_pathgrid {
            app_debug_overlays::build_path_overlay_instances(state, sw, sh)
        } else {
            Vec::new()
        },
        heightmap: if state.debug_show_heightmap {
            app_debug_overlays::build_heightmap_overlay_instances(state, sw, sh)
        } else {
            Vec::new()
        },
    }
}

// ---------------------------------------------------------------------------
// Phase 3: Minimap + UI overlays
// ---------------------------------------------------------------------------

/// Update minimap unit dots for the current frame.
pub(super) fn update_minimap(state: &mut AppState, local_owner: &Option<String>) {
    if let (Some(minimap), Some(sim)) = (&mut state.minimap, &state.simulation) {
        minimap.update_unit_dots(
            &state.gpu,
            &state.batch_renderer,
            &sim.entities,
            &state.house_color_map,
            if state.sandbox_full_visibility {
                None
            } else {
                local_owner
                    .as_deref()
                    .and_then(|owner| sim.interner.get(owner).map(|id| (id, &sim.fog)))
            },
            state.rules.as_ref(),
            Some(&sim.radar_events),
            Some(&sim.interner),
        );
    }
}

/// Build in-game UI overlay instances: selection brackets, health bars,
/// drag rectangle, building placement preview, and software cursor.
pub(super) fn build_ui_instances(state: &AppState, sw: f32, sh: f32) -> UiInstances {
    let bracket: Vec<SpriteInstance> =
        crate::app_selection_brackets::build_selection_bracket_instances(state, sw, sh);
    let mut building_status: Vec<SpriteInstance> = build_building_status_instances(state, sw, sh);
    // DEBUG: append bracket instances into building_status pool to test rendering.
    building_status.extend_from_slice(&bracket);
    let occupant_pip = build_occupant_pip_instances(state, sw, sh);
    let unit_status_bg = build_unit_status_bg_instances(state, sw, sh);
    let unit_status_fill = build_unit_status_fill_instances(state, sw, sh);
    let cargo_pip = build_cargo_pip_instances(state, sw, sh);
    let software_cursor = build_software_cursor_instances(state);
    let drag = match &state.selection_overlay {
        Some(o) => o.build_drag_rect(&state.selection_state, state.camera_x, state.camera_y),
        None => Vec::new(),
    };

    // Building placement preview: cell grid + ghost sprite (or wall ghost for wall types).
    let (placement_valid, placement_invalid, placement_ghost, ghost_page, wall_ghost) =
        build_placement_preview(state);

    // Target/action lines from selected units to command destinations.
    let target_line = crate::app_target_lines::build_target_line_instances(
        &state.target_lines,
        state.simulation.as_ref(),
        state.camera_x,
        state.camera_y,
        &state.height_map,
    );

    UiInstances {
        bracket,
        building_status,
        occupant_pip,
        unit_status_bg,
        unit_status_fill,
        cargo_pip,
        software_cursor,
        drag,
        placement_valid,
        placement_invalid,
        placement_ghost,
        ghost_page,
        wall_ghost,
        target_line,
    }
}

/// Build the building placement preview: valid/invalid cell markers, ghost sprite,
/// and wall connectivity ghost for wall-type buildings.
fn build_placement_preview(
    state: &AppState,
) -> (
    Vec<SpriteInstance>,
    Vec<SpriteInstance>,
    Vec<SpriteInstance>,
    u8,
    Vec<SpriteInstance>,
) {
    match (&state.selection_overlay, &state.building_placement_preview) {
        (Some(o), Some(preview)) => {
            let preview_type_str = state
                .simulation
                .as_ref()
                .map(|s| s.interner.resolve(preview.type_id).to_string())
                .unwrap_or_default();
            let is_wall: bool = state
                .rules
                .as_ref()
                .and_then(|r| r.object(&preview_type_str))
                .map(|obj| obj.wall)
                .unwrap_or(false);

            if is_wall {
                // Walls show the cursor cell + auto-fill cells toward existing walls.
                // Draws place.shp on every intermediate cell between cursor and
                // nearest same-type wall.
                let (mut valid, mut invalid) = o.build_building_preview(preview, &state.height_map);
                let autofill = compute_wall_autofill_cells(state, preview);
                if !autofill.is_empty() {
                    let (av, ai) =
                        o.build_wall_autofill_diamonds(&autofill, preview.valid, &state.height_map);
                    valid.extend(av);
                    invalid.extend(ai);
                }
                (valid, invalid, Vec::new(), 0, Vec::new())
            } else {
                let (valid, invalid) = o.build_building_preview(preview, &state.height_map);
                let hc: crate::rules::house_colors::HouseColorIndex = state
                    .house_color_map
                    .get(
                        &crate::app_commands::preferred_local_owner(state)
                            .unwrap_or_else(|| "Americans".to_string()),
                    )
                    .copied()
                    .unwrap_or_default();
                let ghost_result =
                    crate::render::selection_overlay::SelectionOverlay::build_ghost_sprite(
                        preview,
                        state.sprite_atlas.as_ref(),
                        hc,
                        &state.height_map,
                        state.simulation.as_ref().map(|s| &s.interner),
                    );
                let (ghost, page) = match ghost_result {
                    Some((inst, p)) => (vec![inst], p),
                    None => (Vec::new(), 0),
                };
                (valid, invalid, ghost, page, Vec::new())
            }
        }
        _ => (Vec::new(), Vec::new(), Vec::new(), 0, Vec::new()),
    }
}

/// Compute auto-fill cells for wall placement: intermediate cells between the
/// cursor and the nearest existing same-type wall in each cardinal direction.
///
/// Walks each cardinal direction from the cursor until it hits a same-type
/// wall, then fills the gap.
fn compute_wall_autofill_cells(
    state: &AppState,
    preview: &crate::sim::production::BuildingPlacementPreview,
) -> Vec<(u16, u16)> {
    let Some(overlay_registry) = state.overlay_registry.as_ref() else {
        return Vec::new();
    };
    let preview_type_str_wall = state
        .simulation
        .as_ref()
        .map(|s| s.interner.resolve(preview.type_id).to_string())
        .unwrap_or_default();
    let Some(overlay_id) = overlay_registry.id_for_name(&preview_type_str_wall) else {
        return Vec::new();
    };
    let sim = state.simulation.as_ref();
    let rules = state.rules.as_ref();

    let cursor_rx = preview.rx;
    let cursor_ry = preview.ry;
    let mut cells: Vec<(u16, u16)> = Vec::new();
    let directions: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)];
    for (drx, dry) in directions {
        let mut cx = cursor_rx as i32 + drx;
        let mut cy = cursor_ry as i32 + dry;
        let mut line: Vec<(u16, u16)> = Vec::new();
        loop {
            if cx < 0 || cy < 0 || cx > 511 || cy > 511 {
                break;
            }
            let cell = (cx as u16, cy as u16);
            // Stop if a non-wall building occupies this cell (can't build through it).
            if let (Some(s), Some(r)) = (sim, rules) {
                if crate::sim::production::structure_occupies_cell(
                    &s.entities,
                    r,
                    cell.0,
                    cell.1,
                    &s.interner,
                ) {
                    break;
                }
            }
            let has_wall = state
                .overlays
                .iter()
                .any(|e| e.rx == cell.0 && e.ry == cell.1 && e.overlay_id == overlay_id);
            if has_wall {
                cells.extend_from_slice(&line);
                break;
            }
            line.push(cell);
            cx += drx;
            cy += dry;
        }
    }
    cells
}

// ---------------------------------------------------------------------------
// Phase 4: Sidebar
// ---------------------------------------------------------------------------

/// Build sidebar UI instances: chrome, cameos, text, minimap, viewport rect, radar animation.
pub(super) fn build_sidebar_instances(state: &mut AppState) -> SidebarInstances {
    let view = current_sidebar_view(state);
    let minimap_rect = active_minimap_screen_rect(state);
    let (sw, sh) = (state.render_width() as f32, state.render_height() as f32);

    // Only show minimap when radar is online (or no radar_anim = legacy fallback).
    let minimap_visible: bool = state
        .radar_anim
        .as_ref()
        .map_or(true, |ra| ra.is_minimap_visible());

    let minimap = if minimap_visible {
        match &state.minimap {
            Some(mm) => vec![mm.build_minimap_instance_in_rect(
                state.camera_x,
                state.camera_y,
                minimap_rect.x,
                minimap_rect.y,
                minimap_rect.w,
                minimap_rect.h,
            )],
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };
    let viewport_rect = if minimap_visible {
        // Viewport rect shows the visible world area — shrinks when zoomed in.
        let z = state.zoom_level;
        match &state.minimap {
            Some(mm) => mm.build_viewport_rect_in_rect(
                state.camera_x,
                state.camera_y,
                sw / z,
                sh / z,
                minimap_rect.x,
                minimap_rect.y,
                minimap_rect.w,
                minimap_rect.h,
            ),
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };

    let sidebar = view
        .as_ref()
        .map(|v| sidebar_inst_fn(state, v))
        .unwrap_or_default();
    let chrome = view
        .as_ref()
        .map(|v| build_sidebar_chrome_instances(state, v))
        .unwrap_or_default();

    let ready_text: &str = state
        .csf
        .as_ref()
        .and_then(|csf| csf.get("TXT_READY"))
        .unwrap_or("Ready");
    let ready_tint = {
        let theme = crate::app_sidebar_render::current_sidebar_theme(state);
        crate::app_sidebar_text::ready_color_for_theme(theme)
    };
    let (cameo, cameo_overlay) = view
        .as_ref()
        .map(|v| build_sidebar_cameo_instances(state, v, ready_text))
        .unwrap_or_default();
    let text = view
        .as_ref()
        .map(|v| build_sidebar_text_instances(state, v, ready_text, ready_tint))
        .unwrap_or_default();

    let radar_anim = build_radar_anim_instance(state);

    SidebarInstances {
        sidebar,
        chrome,
        cameo,
        cameo_overlay,
        text,
        minimap,
        viewport_rect,
        radar_anim,
        view,
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// Specialized instance builders
// ---------------------------------------------------------------------------

/// Build a SpriteInstance for the animated radar chrome overlay.
/// Positioned at the same location as the static radar.shp in the sidebar chrome.
fn build_radar_anim_instance(state: &AppState) -> Vec<SpriteInstance> {
    let ra = match &state.radar_anim {
        Some(ra) => ra,
        None => return Vec::new(),
    };
    if ra.phase() == crate::render::radar_anim::RadarAnimPhase::Offline {
        return Vec::new();
    }

    let sw: f32 = state.render_width() as f32;
    let sh: f32 = state.render_height() as f32;
    let spec = state.sidebar_layout_spec;
    let layout = crate::sidebar::compute_layout_with_spec(spec, sw, sh, 0);

    let s = state.ui_scale;
    vec![SpriteInstance {
        position: [
            state.camera_x + layout.sidebar_x,
            state.camera_y + layout.radar_y,
        ],
        size: [ra.width as f32 * s, ra.height as f32 * s],
        uv_origin: [0.0, 0.0],
        uv_size: [1.0, 1.0],
        depth: 0.00048,
        tint: [1.0, 1.0, 1.0],
        alpha: 1.0,
    }]
}

// ---------------------------------------------------------------------------
// Sort helpers
// ---------------------------------------------------------------------------

/// Sort instances by depth descending (furthest-back first for back-to-front draw).
fn sort_by_depth_desc(instances: &mut [SpriteInstance]) {
    instances.sort_by(|a, b| {
        b.depth
            .partial_cmp(&a.depth)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// Sort instances by Y position ascending (used for SHP pages where Y = screen row).
fn sort_by_y_asc(instances: &mut [SpriteInstance]) {
    instances.sort_by(|a, b| {
        a.position[1]
            .partial_cmp(&b.position[1])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}
