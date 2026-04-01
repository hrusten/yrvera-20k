//! Sidebar view construction, minimap interaction, chrome helpers, and render pass.
//!
//! Builds the SidebarView data model each frame, handles minimap drag/click,
//! resolves sidebar chrome theme, and creates the main wgpu render pass.
//!
//! Instance builders for sidebar layers live in app_sidebar_build.rs.
//!
//! Extracted from app_render.rs to keep files under 400 lines.
//!
//! ## Dependency rules
//! - Part of the app layer — may depend on everything.

use crate::app::AppState;
use crate::app_commands::preferred_local_owner_name;
use crate::render::batch::BatchTexture;
use crate::sidebar::{self, SidebarView};
use crate::sim::production;

// Re-export instance builders so callers don't need to know about the split.
pub(crate) use crate::app_sidebar_build::{
    build_sidebar_cameo_instances, build_sidebar_chrome_instances, build_sidebar_instances,
    build_sidebar_text_instances,
};

// ---------------------------------------------------------------------------
// Sidebar view construction
// ---------------------------------------------------------------------------

pub(crate) fn current_sidebar_view(state: &mut AppState) -> Option<SidebarView> {
    let owner_name: String =
        preferred_local_owner_name(state).unwrap_or_else(|| "Americans".to_string());
    let (sim, rules) = (state.simulation.as_ref()?, state.rules.as_ref()?);
    let mut build_options = production::build_options_for_owner(sim, rules, &owner_name);
    let mut queue_items = production::queue_view_for_owner(sim, rules, &owner_name);
    let mut ready_buildings = production::ready_buildings_for_owner(sim, rules, &owner_name);
    // Resolve CSF display names (e.g., "Name:MTNK" → "Grizzly Battle Tank").
    if let Some(csf) = &state.csf {
        for opt in &mut build_options {
            opt.display_name = resolve_csf_name(csf, &opt.display_name);
        }
        for item in &mut queue_items {
            item.display_name = resolve_csf_name(csf, &item.display_name);
        }
        for ready in &mut ready_buildings {
            ready.display_name = resolve_csf_name(csf, &ready.display_name);
        }
    }
    sync_armed_building_placement(
        &mut state.armed_building_placement,
        &mut state.building_placement_preview,
        &ready_buildings,
        state.simulation.as_ref().map(|s| &s.interner),
    );
    let producer_focus = [
        production::ProductionCategory::Building,
        production::ProductionCategory::Defense,
        production::ProductionCategory::Infantry,
        production::ProductionCategory::Vehicle,
        production::ProductionCategory::Aircraft,
    ]
    .into_iter()
    .filter_map(|category| {
        production::active_producer_for_owner_category(sim, rules, &owner_name, category)
    })
    .collect::<Vec<_>>();
    let credits = production::credits_for_owner(sim, &owner_name);
    // Smooth credits animation: step = |diff| / 8, clamped [1, 143].
    // Ticks once per render frame.
    let displayed = state
        .displayed_credits
        .entry(owner_name.clone())
        .or_insert(credits);
    if *displayed != credits {
        let diff = (credits - *displayed).unsigned_abs().max(1) as i32;
        let step = (diff / 8).clamp(1, 143);
        if credits > *displayed {
            *displayed += step;
        } else {
            *displayed -= step;
        }
    }
    let display_credits = *displayed;
    let (power_produced, power_drained) =
        production::power_balance_for_owner(sim, rules, &owner_name);
    let tab_btn_size = current_sidebar_chrome(state)
        .and_then(|atlas| atlas.tab_buttons.first())
        .map(|entry| {
            [
                entry.pixel_size[0] * state.ui_scale,
                entry.pixel_size[1] * state.ui_scale,
            ]
        });
    let interner = state.simulation.as_ref().map(|s| &s.interner);
    let mut view = sidebar::build_sidebar_view_with_spec(
        state.sidebar_layout_spec,
        state.render_width() as f32,
        state.render_height() as f32,
        state.active_sidebar_tab,
        display_credits,
        power_produced,
        power_drained,
        tab_btn_size,
        &queue_items,
        &build_options,
        &ready_buildings,
        state.armed_building_placement.as_deref(),
        &producer_focus,
        state.sidebar_scroll_rows,
        interner,
    );
    if state.sidebar_scroll_rows > view.max_scroll_rows {
        state.sidebar_scroll_rows = view.max_scroll_rows;
        view = sidebar::build_sidebar_view_with_spec(
            state.sidebar_layout_spec,
            state.render_width() as f32,
            state.render_height() as f32,
            state.active_sidebar_tab,
            credits,
            power_produced,
            power_drained,
            tab_btn_size,
            &queue_items,
            &build_options,
            &ready_buildings,
            state.armed_building_placement.as_deref(),
            &producer_focus,
            state.sidebar_scroll_rows,
            interner,
        );
    }
    if let Some(atlas) = state.sidebar_cameo_atlas.as_ref() {
        for item in &mut view.items {
            item.has_cameo_art = atlas.get(&item.type_id).is_some();
        }
    }
    Some(view)
}

pub(crate) fn sync_armed_building_placement(
    armed_building_placement: &mut Option<String>,
    building_placement_preview: &mut Option<crate::sim::production::BuildingPlacementPreview>,
    ready_buildings: &[production::ReadyBuildingView],
    interner: Option<&crate::sim::intern::StringInterner>,
) {
    if armed_building_placement.as_ref().is_some_and(|armed| {
        !ready_buildings.iter().any(|ready| {
            interner.map_or(false, |i| {
                i.resolve(ready.type_id).eq_ignore_ascii_case(armed)
            })
        })
    }) {
        *armed_building_placement = None;
        *building_placement_preview = None;
    }
}

// ---------------------------------------------------------------------------
// Minimap interaction
// ---------------------------------------------------------------------------

pub(crate) fn is_cursor_over_minimap(state: &AppState) -> bool {
    // Minimap interaction disabled when radar is not online.
    let minimap_visible: bool = state
        .radar_anim
        .as_ref()
        .map_or(true, |ra| ra.is_minimap_visible());
    if !minimap_visible {
        return false;
    }
    let Some(minimap) = &state.minimap else {
        return false;
    };
    let rect = active_minimap_screen_rect(state);
    minimap.contains_screen_point_in_rect(
        state.cursor_x,
        state.cursor_y,
        rect.x,
        rect.y,
        rect.w,
        rect.h,
    )
}

pub(crate) fn try_begin_minimap_drag(state: &mut AppState) -> bool {
    if !is_cursor_over_minimap(state) {
        return false;
    }
    // If units are selected, left-click on minimap issues a move order
    // to the clicked world position instead of dragging the camera.
    if minimap_move_order_if_selected(state) {
        return true;
    }
    state.minimap_dragging = true;
    state.selection_state.cancel_drag();
    update_camera_from_minimap_cursor(state);
    true
}

/// If there are selected mobile units, issue a move command to the minimap
/// click location and return true. Otherwise return false (caller does camera drag).
fn minimap_move_order_if_selected(state: &mut AppState) -> bool {
    let Some(sim) = &state.simulation else {
        return false;
    };
    let selected_ids = crate::app_input::selected_stable_ids_sorted(&sim.entities);
    if selected_ids.is_empty() {
        return false;
    }
    // Convert minimap cursor position to world iso coordinates.
    let (target_rx, target_ry) = match minimap_cursor_to_iso(state) {
        Some(coords) => coords,
        None => return false,
    };
    let owner = crate::app_commands::preferred_local_owner_name(state)
        .unwrap_or_else(|| "Americans".to_string());
    let owner_id = sim.interner.get(&owner).unwrap_or_default();
    let execute_tick = sim.tick.saturating_add(sim.input_delay_ticks);
    let order_mode = state.queued_order_mode;
    let shift_held: bool = crate::app_input::is_shift_held(state);
    let mut queued: Vec<crate::sim::command::CommandEnvelope> = Vec::new();
    for &entity_id in &selected_ids {
        let Some(entity) = sim.entities.get(entity_id) else {
            continue;
        };
        // Only issue move to non-structure entities.
        if entity.category == crate::map::entities::EntityCategory::Structure {
            continue;
        }
        let mut goal = (target_rx, target_ry);
        if let Some(grid) = state.path_grid.as_ref() {
            let layered = state
                .simulation
                .as_ref()
                .and_then(|s| s.layered_path_grid.as_ref());
            if !crate::app_sim_tick::is_any_layer_walkable(grid, layered, goal.0, goal.1) {
                if let Some(nearest) =
                    crate::app_sim_tick::nearest_walkable_cell_layered(grid, layered, goal, 12)
                {
                    goal = nearest;
                }
            }
        }
        let command = match order_mode {
            crate::app_render::OrderMode::AttackMove => crate::sim::command::Command::AttackMove {
                entity_id,
                target_rx: goal.0,
                target_ry: goal.1,
                queue: shift_held,
            },
            _ => crate::sim::command::Command::Move {
                entity_id,
                target_rx: goal.0,
                target_ry: goal.1,
                queue: shift_held,
                group_id: None,
            },
        };
        queued.push(crate::sim::command::CommandEnvelope::new(
            owner_id,
            execute_tick,
            command,
        ));
    }
    if queued.is_empty() {
        return false;
    }
    // Reset order mode after issuing the command (like the main viewport does).
    if order_mode != crate::app_render::OrderMode::Move {
        state.queued_order_mode = crate::app_render::OrderMode::Move;
    }
    if let Some(sim) = &mut state.simulation {
        sim.pending_commands.extend(queued);
    }
    true
}

/// Convert the current minimap cursor position to iso (rx, ry) coordinates.
/// Returns None if no minimap is available.
fn minimap_cursor_to_iso(state: &AppState) -> Option<(u16, u16)> {
    let minimap = state.minimap.as_ref()?;
    let sw = state.render_width() as f32;
    let sh = state.render_height() as f32;
    let z = state.zoom_level;
    let rect = active_minimap_screen_rect(state);
    // camera_top_left_for_screen_point_in_rect returns the camera top-left that
    // would center the viewport on the clicked point. We want the world center point.
    // Visible world area = screen / zoom.
    let (cam_x, cam_y) = minimap.camera_top_left_for_screen_point_in_rect(
        state.cursor_x,
        state.cursor_y,
        sw / z,
        sh / z,
        rect.x,
        rect.y,
        rect.w,
        rect.h,
    );
    // The center of the viewport is what was clicked.
    let world_x = cam_x + sw / (2.0 * z);
    let world_y = cam_y + sh / (2.0 * z);
    Some(crate::app_sim_tick::world_point_to_cell(
        world_x,
        world_y,
        &state.height_map,
        Some(&state.bridge_height_map),
    ))
}

pub(crate) fn update_camera_from_minimap_cursor(state: &mut AppState) {
    let Some(minimap) = &state.minimap else {
        return;
    };
    let sw = state.render_width() as f32;
    let sh = state.render_height() as f32;
    let z = state.zoom_level;
    let rect = active_minimap_screen_rect(state);
    let (cx, cy) = minimap.camera_top_left_for_screen_point_in_rect(
        state.cursor_x,
        state.cursor_y,
        sw / z,
        sh / z,
        rect.x,
        rect.y,
        rect.w,
        rect.h,
    );
    state.camera_x = cx;
    state.camera_y = cy;
    crate::app_camera::clamp_camera_to_playable_area(state, sw, sh);
}

pub(crate) fn active_minimap_screen_rect(state: &AppState) -> crate::sidebar::Rect {
    let sw = state.render_width() as f32;
    let sh = state.render_height() as f32;
    if current_sidebar_chrome(state).is_some() {
        // These position the minimap content exactly inside the BKGDLG.SHP chrome border.
        const MINIMAP_LEFT: f32 = 13.0;
        const MINIMAP_TOP: f32 = 0.0;
        const MINIMAP_WIDTH: f32 = 140.0;
        const MINIMAP_HEIGHT: f32 = 120.0;

        let spec = state.sidebar_layout_spec;
        let s = state.ui_scale;
        let sidebar_x = sw - spec.sidebar_width + spec.x_offset;
        crate::sidebar::Rect {
            x: sidebar_x + MINIMAP_LEFT * s,
            y: spec.top_inset + MINIMAP_TOP * s,
            w: MINIMAP_WIDTH * s,
            h: MINIMAP_HEIGHT * s,
        }
    } else {
        let (x, y, w, h) = crate::render::minimap::default_minimap_rect(sh);
        crate::sidebar::Rect { x, y, w, h }
    }
}

// ---------------------------------------------------------------------------
// Chrome / theme helpers
// ---------------------------------------------------------------------------

pub(crate) fn current_sidebar_chrome_texture(state: &AppState) -> Option<&BatchTexture> {
    current_sidebar_chrome(state).map(|atlas| &atlas.texture)
}

pub(crate) fn current_sidebar_theme(
    state: &AppState,
) -> crate::render::sidebar_chrome::SidebarTheme {
    preferred_local_owner_name(state)
        .and_then(|owner| sidebar_theme_for_owner(state, &owner))
        .unwrap_or(crate::render::sidebar_chrome::SidebarTheme::Allied)
}

pub(crate) fn current_sidebar_chrome(
    state: &AppState,
) -> Option<&crate::render::sidebar_chrome::SidebarChromeAtlas> {
    let set = state.sidebar_chrome.as_ref()?;
    let theme = current_sidebar_theme(state);
    set.for_theme(theme)
}

fn sidebar_theme_for_owner(
    state: &AppState,
    owner: &str,
) -> Option<crate::render::sidebar_chrome::SidebarTheme> {
    let house = state
        .house_roster
        .houses
        .iter()
        .find(|house| house.name.eq_ignore_ascii_case(owner))?;

    let side = house
        .side
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let country = house
        .country
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();

    if side.contains("yuri") || country.contains("yuri") {
        return Some(crate::render::sidebar_chrome::SidebarTheme::Yuri);
    }
    if side.contains("soviet")
        || matches!(
            country.as_str(),
            "russia" | "iraq" | "cuba" | "libya" | "soviet"
        )
    {
        return Some(crate::render::sidebar_chrome::SidebarTheme::Soviet);
    }
    Some(crate::render::sidebar_chrome::SidebarTheme::Allied)
}

// ---------------------------------------------------------------------------
// CSF display name resolution
// ---------------------------------------------------------------------------

/// Resolve a display name through the CSF string table.
///
/// RA2 rules.ini `Name=` values are CSF keys (e.g., `"Name:MTNK"`).
/// If the name matches a CSF key, return the localized string.
/// Otherwise return the original name unchanged.
fn resolve_csf_name(csf: &crate::assets::csf_file::CsfFile, name: &str) -> String {
    // Try the name directly as a CSF key (e.g., "Name:MTNK").
    if let Some(resolved) = csf.get(name) {
        return resolved.to_string();
    }
    // No match — keep original name.
    name.to_string()
}

// ---------------------------------------------------------------------------
// Render pass creation
// ---------------------------------------------------------------------------

/// Create the main render pass with depth buffer and clear.
pub(crate) fn begin_main_pass<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    view: &'a wgpu::TextureView,
    depth_view: &'a wgpu::TextureView,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("Main Pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(crate::app_render::CLEAR_COLOR),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: depth_view,
            stencil_ops: None,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(1.0),
                store: wgpu::StoreOp::Store,
            }),
        }),
        timestamp_writes: None,
        occlusion_query_set: None,
    })
}
