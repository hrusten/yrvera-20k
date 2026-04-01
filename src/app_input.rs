//! In-game input handling — mouse clicks, hotkeys, sidebar interactions,
//! control groups, and selection commands.
//!
//! Context-sensitive order resolution (click → command) lives in
//! app_context_order.rs. This file handles raw input dispatch and UI state.
//!
//! ## Dependency rules
//! - Part of the app layer — may depend on everything.

use winit::event::{ElementState, MouseButton};
use winit::keyboard::KeyCode;

use crate::app::AppState;
use crate::app_commands::{
    cancel_build_by_type, cancel_last_build, cycle_active_producer, cycle_local_owner,
    place_ready_building_at_cursor, place_starter_base_for_local_owner, preferred_local_owner,
    preferred_local_owner_name, queue_build_by_type, schedule_command,
    spawn_test_units_for_local_owner, toggle_pause_build_queue,
};
use crate::app_context_order::try_queue_context_order_at_screen_point;
use crate::app_entity_pick::{compute_box_selection_snapshot, compute_click_selection_snapshot};
use crate::app_sidebar_render::current_sidebar_view;
use crate::app_types::OrderMode;
use crate::audio::events::GameSoundEvent;
use crate::map::entities::EntityCategory;
use crate::sidebar::{self, SidebarAction, SidebarTab};
use crate::sim::command::Command;
use crate::sim::selection::{DragTransition, SelectAction};

/// Click radius for single-click selection (pixels in world space).
pub(crate) const CLICK_SELECT_RADIUS: f32 = 30.0;

/// Handle mouse button press/release for selection and move commands.
pub(crate) fn handle_mouse_input(
    state: &mut AppState,
    button: MouseButton,
    btn_state: ElementState,
) {
    if btn_state.is_pressed() {
        if handle_sidebar_mouse_input(state, button) {
            return;
        }
    }
    match button {
        MouseButton::Left => {
            if btn_state.is_pressed() {
                if crate::app_sidebar_render::try_begin_minimap_drag(state) {
                    return;
                }
                if state.armed_building_placement.is_some() {
                    return;
                }
                state
                    .selection_state
                    .begin_drag(state.cursor_x, state.cursor_y);
            } else {
                if state.minimap_dragging {
                    state.minimap_dragging = false;
                    return;
                }
                if let Some(type_id) = state.armed_building_placement.clone() {
                    place_ready_building_at_cursor(state, &type_id);
                    return;
                }
                let action: SelectAction = state
                    .selection_state
                    .end_drag(state.cursor_x, state.cursor_y);
                let shift = is_shift_held(state);
                // On a single click (not drag-box), try issuing a command first.
                // If the click lands on a friendly unit/building, fall through to
                // selection instead (select_friendly_clicks=true).
                if let SelectAction::Click(_, _) = action {
                    let commanded: bool = try_queue_context_order_at_screen_point(
                        state,
                        state.cursor_x,
                        state.cursor_y,
                        true, // select_friendly_clicks: let friendly clicks fall through to selection
                    );
                    if commanded {
                        return;
                    }
                }
                let mut queued_selection: Option<Vec<u64>> = None;
                if let Some(sim) = &state.simulation {
                    match action {
                        SelectAction::Click(sx, sy) => {
                            let world_x: f32 = sx / state.zoom_level + state.camera_x;
                            let world_y: f32 = sy / state.zoom_level + state.camera_y;
                            let fog_ref = if state.sandbox_full_visibility {
                                None
                            } else {
                                Some(&sim.fog)
                            };
                            queued_selection = compute_click_selection_snapshot(
                                &sim.entities,
                                fog_ref,
                                preferred_local_owner_name(state).as_deref(),
                                world_x,
                                world_y,
                                CLICK_SELECT_RADIUS,
                                shift,
                                state.rules.as_ref(),
                                &state.height_map,
                                Some(&state.bridge_height_map),
                                Some(&sim.interner),
                            );
                        }
                        SelectAction::BoxSelect(min_x, min_y, max_x, max_y) => {
                            let fog_ref = if state.sandbox_full_visibility {
                                None
                            } else {
                                Some(&sim.fog)
                            };
                            let z = state.zoom_level;
                            queued_selection = compute_box_selection_snapshot(
                                &sim.entities,
                                fog_ref,
                                preferred_local_owner_name(state).as_deref(),
                                min_x / z + state.camera_x,
                                min_y / z + state.camera_y,
                                max_x / z + state.camera_x,
                                max_y / z + state.camera_y,
                                shift,
                                Some(&sim.interner),
                            );
                        }
                        SelectAction::None => {}
                    }
                }
                if let Some(snapshot) = queued_selection {
                    // Emit VoiceSelect for the first selected unit type.
                    emit_selection_voice(state, &snapshot);
                    queue_selection_snapshot_command(state, snapshot, shift);
                }
            }
        }
        MouseButton::Middle => {
            if btn_state.is_pressed() {
                state.middle_mouse_panning = true;
                state.middle_mouse_anchor_x = state.cursor_x;
                state.middle_mouse_anchor_y = state.cursor_y;
            } else {
                state.middle_mouse_panning = false;
            }
        }
        MouseButton::Right if btn_state.is_pressed() => {
            // Right-click = cancel / deselect only.
            if state.armed_building_placement.is_some() {
                state.armed_building_placement = None;
                state.building_placement_preview = None;
                return;
            }
            // Clear the current selection.
            queue_selection_snapshot_command(state, Vec::new(), false);
        }
        _ => {}
    }
}

/// Handle cursor-move behavior while in-game.
///
/// If minimap-dragging is active, camera follows the cursor on the minimap.
/// Otherwise this updates unit-selection drag rectangle.
/// Speed multiplier for middle-mouse camera panning. Each pixel of mouse movement
/// translates to this many pixels of camera scroll, making it feel fast and responsive.
const MIDDLE_MOUSE_PAN_SPEED: f32 = 3.0;

pub(crate) fn handle_cursor_moved_in_game(state: &mut AppState) {
    if state.minimap_dragging {
        crate::app_sidebar_render::update_camera_from_minimap_cursor(state);
        return;
    }
    if state.middle_mouse_panning {
        let dx: f32 = state.cursor_x - state.middle_mouse_anchor_x;
        let dy: f32 = state.cursor_y - state.middle_mouse_anchor_y;
        // Divide by zoom so screen-space mouse delta maps to correct world distance.
        state.camera_x -= dx * MIDDLE_MOUSE_PAN_SPEED / state.zoom_level;
        state.camera_y -= dy * MIDDLE_MOUSE_PAN_SPEED / state.zoom_level;
        state.middle_mouse_anchor_x = state.cursor_x;
        state.middle_mouse_anchor_y = state.cursor_y;
        // Clamp after panning so camera stays within map bounds.
        let sw: f32 = state.render_width() as f32;
        let sh: f32 = state.render_height() as f32;
        crate::app_camera::clamp_camera_to_playable_area(state, sw, sh);
        return;
    }
    // Clamp drag position to the tactical viewport (exclude sidebar area).
    let viewport_w = state.render_width() as f32;
    let viewport_h = state.render_height() as f32;
    let drag_x = state.cursor_x.clamp(0.0, viewport_w - 1.0);
    let drag_y = state.cursor_y.clamp(0.0, viewport_h - 1.0);

    let transition = state.selection_state.update_drag(drag_x, drag_y);

    // Clear selection when band-box activates (threshold crossed), not when
    // the mouse button is released.
    if transition == DragTransition::Activated {
        if let Some(sim) = &mut state.simulation {
            crate::sim::selection::deselect_all(&mut sim.entities);
        }
    }
}

/// Try to scroll the sidebar panel. Returns true if the cursor was over the
/// sidebar and the scroll was consumed, false if it should be handled as zoom.
pub(crate) fn try_sidebar_scroll(state: &mut AppState, delta_lines: f32) -> bool {
    let Some(view) = current_sidebar_view(state) else {
        return false;
    };
    if !view.panel_rect.contains(state.cursor_x, state.cursor_y) {
        return false;
    }
    if delta_lines > 0.0 {
        let step = delta_lines.ceil().max(1.0) as usize;
        state.sidebar_scroll_rows = state.sidebar_scroll_rows.saturating_sub(step);
    } else if delta_lines < 0.0 {
        let step = (-delta_lines).ceil().max(1.0) as usize;
        state.sidebar_scroll_rows = (state.sidebar_scroll_rows + step).min(view.max_scroll_rows);
    }
    true
}

fn handle_sidebar_mouse_input(state: &mut AppState, button: MouseButton) -> bool {
    let Some(view) = current_sidebar_view(state) else {
        return false;
    };
    let right_click = button == MouseButton::Right;
    let action = sidebar::hit_test(&view, state.cursor_x, state.cursor_y, right_click);
    if action == SidebarAction::None {
        return false;
    }
    apply_sidebar_action(state, action);
    true
}

fn apply_sidebar_action(state: &mut AppState, action: SidebarAction) {
    match action {
        SidebarAction::None => {}
        SidebarAction::SelectTab(tab) => {
            state.active_sidebar_tab = tab;
            state.sidebar_scroll_rows = 0;
        }
        SidebarAction::BuildType(type_id) => {
            queue_build_by_type(state, &type_id);
        }
        SidebarAction::ArmPlacement(type_id) => {
            state.armed_building_placement = Some(type_id);
        }
        SidebarAction::ClearPlacementMode => {
            state.armed_building_placement = None;
            state.building_placement_preview = None;
        }
        SidebarAction::TogglePauseQueue(category) => {
            toggle_pause_build_queue(state, category);
        }
        SidebarAction::CycleProducer(category) => {
            cycle_active_producer(state, category);
        }
        SidebarAction::CancelBuild(type_id) => {
            cancel_build_by_type(state, &type_id);
        }
        SidebarAction::CancelLastBuild => {
            cancel_last_build(state);
        }
        SidebarAction::CycleOwner => {
            cycle_local_owner(state);
        }
        SidebarAction::PlaceStarterBase => {
            place_starter_base_for_local_owner(state);
        }
        SidebarAction::SpawnTestUnits => {
            spawn_test_units_for_local_owner(state);
        }
        SidebarAction::Deploy => {
            queue_deploy_undeploy_for_selected(state);
        }
    }
}

/// Handle one-shot gameplay hotkeys (called on key press, not held).
pub(crate) fn handle_hotkey_pressed(state: &mut AppState, code: winit::keyboard::KeyCode) {
    if let Some(group_idx) = control_group_index(code) {
        handle_control_group_hotkey(state, group_idx);
        return;
    }
    match code {
        KeyCode::Escape => {
            if state.paused {
                // Unpause — reset timing to prevent sim accumulator spike.
                state.paused = false;
                state.last_update_time = std::time::Instant::now();
                state.sim_accumulator_ms = 0;
                // Re-hide OS cursor so the software cursor takes over.
                if state.software_cursor.is_some() {
                    state.window.set_cursor_visible(false);
                }
                log::info!("Game resumed");
            } else if state.armed_building_placement.is_some() {
                state.armed_building_placement = None;
                state.building_placement_preview = None;
            } else {
                state.paused = true;
                // Show OS cursor for egui interaction.
                if state.software_cursor.is_some() {
                    state.window.set_cursor_visible(true);
                }
                log::info!("Game paused");
            }
        }
        KeyCode::KeyB => {
            state.queued_order_mode = OrderMode::Move;
        }
        KeyCode::KeyS => queue_stop_for_selected(state),
        KeyCode::KeyD => queue_deploy_undeploy_for_selected(state),
        KeyCode::KeyA => {
            state.queued_order_mode = OrderMode::AttackMove;
            log::info!("Order mode armed: AttackMove");
        }
        KeyCode::KeyG => {
            state.queued_order_mode = OrderMode::Guard;
            log::info!("Order mode armed: Guard");
        }
        KeyCode::KeyM => {
            quicksave(state);
        }
        KeyCode::KeyN => {
            quickload(state);
        }
        KeyCode::KeyQ => {
            apply_sidebar_action(state, SidebarAction::SelectTab(SidebarTab::Building))
        }
        KeyCode::KeyW => apply_sidebar_action(state, SidebarAction::SelectTab(SidebarTab::Defense)),
        KeyCode::KeyE => {
            apply_sidebar_action(state, SidebarAction::SelectTab(SidebarTab::Infantry))
        }
        KeyCode::KeyR => apply_sidebar_action(state, SidebarAction::SelectTab(SidebarTab::Vehicle)),
        KeyCode::KeyT => select_same_type(state, is_shift_held(state)),
        KeyCode::Delete => crate::app_commands::sell_selected_buildings(state),
        KeyCode::KeyL => {
            state.debug_show_cell_grid = !state.debug_show_cell_grid;
            log::info!(
                "Debug cell grid overlay: {}",
                if state.debug_show_cell_grid {
                    "ON (blue=terrain, yellow=overlay)"
                } else {
                    "OFF"
                }
            );
        }
        KeyCode::F1 => {
            state.show_hotkey_help = !state.show_hotkey_help;
        }
        KeyCode::F5 => {
            state.show_save_load_panel = !state.show_save_load_panel;
            if state.show_save_load_panel {
                state.save_list_cache.invalidate();
                // Show OS cursor for egui interaction.
                if state.software_cursor.is_some() {
                    state.window.set_cursor_visible(true);
                }
            } else if state.software_cursor.is_some() && !state.paused {
                // Re-hide OS cursor so the software cursor takes over.
                state.window.set_cursor_visible(false);
            }
        }
        KeyCode::KeyH => {
            jump_camera_to_base(state);
        }
        KeyCode::KeyK => {
            state.debug_show_heightmap = !state.debug_show_heightmap;
            log::info!(
                "Debug height map overlay: {}",
                if state.debug_show_heightmap {
                    "ON (brighter = higher elevation, blue = bridge deck)"
                } else {
                    "OFF"
                }
            );
        }
        KeyCode::F9 | KeyCode::KeyP => {
            state.debug_show_pathgrid = !state.debug_show_pathgrid;
            if !state.debug_show_pathgrid {
                state.debug_terrain_cost_speed_type = None;
            }
            log::info!(
                "Debug terrain cost overlay: {}",
                if state.debug_show_pathgrid {
                    "ON"
                } else {
                    "OFF"
                }
            );
        }
        KeyCode::BracketRight => {
            if state.debug_show_pathgrid {
                let current = crate::app_debug_overlays::resolve_debug_speed_type(state);
                let next = current.cycle_next();
                state.debug_terrain_cost_speed_type = Some(next);
                log::info!("Terrain cost overlay: {}", next.name());
            }
        }
        KeyCode::BracketLeft => {
            if state.debug_show_pathgrid {
                let current = crate::app_debug_overlays::resolve_debug_speed_type(state);
                let prev = current.cycle_prev();
                state.debug_terrain_cost_speed_type = Some(prev);
                log::info!("Terrain cost overlay: {}", prev.name());
            }
        }
        KeyCode::F10 | KeyCode::KeyV => {
            state.sandbox_full_visibility = !state.sandbox_full_visibility;
            log::info!(
                "Fog of war: {}",
                if state.sandbox_full_visibility {
                    "OFF (full visibility)"
                } else {
                    "ON"
                }
            );
        }
        KeyCode::Space => {
            // Spacebar cycles through recent radar events and jumps the camera.
            if let Some(sim) = &mut state.simulation {
                if let Some((rx, ry)) = sim.radar_events.cycle_event() {
                    let (sx, sy) = crate::map::terrain::iso_to_screen(rx, ry, 0);
                    let sw: f32 = state.render_width() as f32;
                    let sh: f32 = state.render_height() as f32;
                    let z = state.zoom_level;
                    state.camera_x = sx - sw / (2.0 * z);
                    state.camera_y = sy - sh / (2.0 * z);
                }
            }
        }
        KeyCode::KeyX => {
            state.debug_unit_inspector = !state.debug_unit_inspector;
            if let Some(sim) = &mut state.simulation {
                sim.debug_event_logging = state.debug_unit_inspector;
                if state.debug_unit_inspector {
                    // Allocate logs on all existing entities.
                    for entity in sim.entities.values_mut() {
                        if entity.debug_log.is_none() {
                            entity.debug_log =
                                Some(crate::sim::debug_event_log::DebugEventLog::new());
                        }
                    }
                    log::info!("Debug unit inspector: ON (X)");
                } else {
                    // Drop all logs to free memory.
                    for entity in sim.entities.values_mut() {
                        entity.debug_log = None;
                    }
                    log::info!("Debug unit inspector: OFF");
                }
            }
        }
        KeyCode::KeyJ => {
            state.paused = !state.paused;
            if !state.paused {
                // Reset timing to prevent sim accumulator spike after pause.
                state.last_update_time = std::time::Instant::now();
                state.sim_accumulator_ms = 0;
            }
            log::info!("Debug pause: {}", if state.paused { "ON" } else { "OFF" });
        }
        KeyCode::Period => {
            if state.paused {
                state.debug_frame_step_requested = true;
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Quick-save / quick-load
// ---------------------------------------------------------------------------

const SAVES_DIR: &str = "saves";

fn quicksave(state: &mut AppState) {
    let Some(sim) = &state.simulation else {
        log::warn!("Quicksave: no active simulation");
        return;
    };
    let rules_h = state
        .rules
        .as_ref()
        .map(crate::app_sim_tick::rules_hash)
        .unwrap_or(0);
    let map_name = &state.theater_name;
    let bytes = crate::sim::snapshot::GameSnapshot::save(sim, 0, rules_h, map_name);
    if let Err(e) = std::fs::create_dir_all(SAVES_DIR) {
        log::error!("Quicksave: failed to create saves dir: {e}");
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let filename = format!("save_tick{}_{}.bin", sim.tick, now);
    let path = format!("{SAVES_DIR}/{filename}");
    match std::fs::write(&path, &bytes) {
        Ok(()) => {
            log::info!("Quicksave: saved {} bytes to {}", bytes.len(), path);
            state.save_list_cache.invalidate();
        }
        Err(e) => log::error!("Quicksave: write failed: {e}"),
    }
}

/// Find the most recent `.bin` save file in the saves directory.
fn most_recent_save_path() -> Option<std::path::PathBuf> {
    let dir = std::fs::read_dir(SAVES_DIR).ok()?;
    dir.filter_map(|entry| {
        let entry = entry.ok()?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("bin") {
            let meta = entry.metadata().ok()?;
            Some((path, meta.modified().ok()?))
        } else {
            None
        }
    })
    .max_by_key(|(_, modified)| *modified)
    .map(|(path, _)| path)
}

fn quickload(state: &mut AppState) {
    let path = match most_recent_save_path() {
        Some(p) => p,
        None => {
            log::warn!("Quickload: no save files found in {SAVES_DIR}/");
            return;
        }
    };
    load_save_file(state, &path);
}

/// Load a save file by path. Used by both quickload and the save/load panel.
pub(crate) fn load_save_file(state: &mut AppState, path: &std::path::Path) {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            log::warn!("Load: could not read {}: {e}", path.display());
            return;
        }
    };
    let snapshot = match crate::sim::snapshot::GameSnapshot::load(&bytes) {
        Ok(s) => s,
        Err(e) => {
            log::error!("Load: {e}");
            return;
        }
    };

    // Grab cache data from the current sim (these fields are #[serde(skip)]
    // and must be restored after deserialization).
    let Some(current_sim) = &state.simulation else {
        log::warn!("Load: no active simulation to restore caches from");
        return;
    };
    let terrain_speed_config = current_sim.terrain_speed_config.clone();
    let bridge_explosions = current_sim.bridge_explosions.clone();
    let effect_frame_counts = current_sim.effect_frame_counts.clone();
    let terrain_costs = current_sim.terrain_costs.clone();

    let resolved_terrain = match state.resolved_terrain.clone() {
        Some(rt) => rt,
        None => {
            log::error!("Load: no resolved_terrain available");
            return;
        }
    };

    // Replace the simulation with the loaded one.
    let mut sim = snapshot.sim;
    sim.rebuild_caches_after_load(
        resolved_terrain,
        terrain_speed_config,
        bridge_explosions,
        effect_frame_counts,
        terrain_costs,
    );
    state.simulation = Some(sim);

    // Rebuild the app-layer dynamic path grid (building footprints + walls).
    crate::app_sim_tick::rebuild_dynamic_path_grid(state);

    // Rebuild sprite/unit atlases so all entity types in the loaded save have
    // atlas entries before the first render frame.
    crate::app_sim_tick::refresh_entity_atlases(state);

    // Reset timing to prevent a burst of ticks after the load.
    state.last_update_time = std::time::Instant::now();
    state.sim_accumulator_ms = 0;

    // Close the save/load panel after loading.
    state.show_save_load_panel = false;

    log::info!("Load: restored simulation from {}", path.display());
}

pub(crate) fn is_shift_held(state: &AppState) -> bool {
    state.keys_held.contains(&KeyCode::ShiftLeft) || state.keys_held.contains(&KeyCode::ShiftRight)
}

pub(crate) fn is_ctrl_held(state: &AppState) -> bool {
    state.keys_held.contains(&KeyCode::ControlLeft)
        || state.keys_held.contains(&KeyCode::ControlRight)
}

pub(crate) fn selected_stable_ids_sorted(
    entities: &crate::sim::entity_store::EntityStore,
) -> Vec<u64> {
    let mut ids: Vec<u64> = entities
        .values()
        .filter(|e| e.selected)
        .map(|e| e.stable_id)
        .collect();
    ids.sort_unstable();
    ids
}

fn queue_selection_snapshot_command(state: &mut AppState, selected_ids: Vec<u64>, additive: bool) {
    let owner: String = preferred_local_owner(state).unwrap_or_else(|| "Americans".to_string());
    schedule_command(
        state,
        &owner,
        Command::Select {
            entity_ids: selected_ids,
            additive,
        },
    );
}

fn queue_stop_for_selected(state: &mut AppState) {
    let Some(sim) = &state.simulation else { return };
    let mut selected_ids: Vec<u64> = selected_stable_ids_sorted(&sim.entities);
    if selected_ids.is_empty() {
        return;
    }
    selected_ids.sort_unstable();
    let owner: String = preferred_local_owner(state).unwrap_or_else(|| "Americans".to_string());
    for entity_id in selected_ids {
        schedule_command(state, &owner, Command::Stop { entity_id });
    }
}

/// Deploy or undeploy selected entities. KeyD toggles:
/// - Selected unit with `DeploysInto` → `Command::DeployMcv` (MCV → ConYard)
/// - Selected structure with `UndeploysInto` → `Command::UndeployBuilding` (ConYard → MCV)
fn queue_deploy_undeploy_for_selected(state: &mut AppState) {
    let Some(sim) = &state.simulation else { return };
    let selected_ids: Vec<u64> = selected_stable_ids_sorted(&sim.entities);
    if selected_ids.is_empty() {
        return;
    }
    let owner: String = preferred_local_owner(state).unwrap_or_else(|| "Americans".to_string());
    // Collect commands first to avoid borrow conflict with schedule_command.
    let mut commands: Vec<Command> = Vec::new();
    {
        let rules = state.rules.as_ref();
        for &entity_id in &selected_ids {
            let Some(entity) = sim.entities.get(entity_id) else {
                continue;
            };
            let obj = rules.and_then(|r| r.object(sim.interner.resolve(entity.type_ref)));
            match entity.category {
                crate::map::entities::EntityCategory::Structure => {
                    // Garrisoned building → evacuate occupants.
                    if obj.map_or(false, |o| o.can_be_occupied)
                        && entity.passenger_role.cargo().is_some_and(|c| !c.is_empty())
                    {
                        commands.push(Command::UnloadPassengers {
                            transport_id: entity_id,
                        });
                    } else if obj.map_or(false, |o| o.undeploys_into.is_some()) {
                        commands.push(Command::UndeployBuilding { entity_id });
                    }
                }
                _ => {
                    if obj.map_or(false, |o| o.deploys_into.is_some()) {
                        commands.push(Command::DeployMcv { entity_id });
                    }
                }
            }
        }
    }
    for cmd in commands {
        schedule_command(state, &owner, cmd);
    }
}

fn control_group_index(code: KeyCode) -> Option<usize> {
    match code {
        KeyCode::Digit0 => Some(0),
        KeyCode::Digit1 => Some(1),
        KeyCode::Digit2 => Some(2),
        KeyCode::Digit3 => Some(3),
        KeyCode::Digit4 => Some(4),
        KeyCode::Digit5 => Some(5),
        KeyCode::Digit6 => Some(6),
        KeyCode::Digit7 => Some(7),
        KeyCode::Digit8 => Some(8),
        KeyCode::Digit9 => Some(9),
        _ => None,
    }
}

fn handle_control_group_hotkey(state: &mut AppState, group_idx: usize) {
    if group_idx >= state.control_groups.len() {
        return;
    }
    if is_ctrl_held(state) {
        let ids = state
            .simulation
            .as_ref()
            .map(|sim| selected_stable_ids_sorted(&sim.entities))
            .unwrap_or_default();
        state.control_groups[group_idx] = ids;
        return;
    }

    let group = state.control_groups[group_idx].clone();
    if group.is_empty() {
        return;
    }
    let additive = is_shift_held(state);
    let mut final_ids = if additive {
        state
            .simulation
            .as_ref()
            .map(|sim| selected_stable_ids_sorted(&sim.entities))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    final_ids.extend(group);
    final_ids.sort_unstable();
    final_ids.dedup();
    let owner: String = preferred_local_owner(state).unwrap_or_else(|| "Americans".to_string());
    schedule_command(
        state,
        &owner,
        Command::Select {
            entity_ids: final_ids,
            additive,
        },
    );
}

fn select_same_type(state: &mut AppState, additive: bool) {
    let Some(sim) = &state.simulation else { return };
    let anchor = sim
        .entities
        .values()
        .find(|e| e.selected)
        .map(|e| (e.type_ref, e.owner));
    let Some((type_id, owner_id)) = anchor else {
        return;
    };

    let mut matching_ids: Vec<u64> = sim
        .entities
        .values()
        .filter_map(|e| (e.type_ref == type_id && e.owner == owner_id).then_some(e.stable_id))
        .collect();
    if additive {
        matching_ids.extend(selected_stable_ids_sorted(&sim.entities));
    }
    matching_ids.sort_unstable();
    matching_ids.dedup();
    let owner = sim.interner.resolve(owner_id).to_string();
    let local_owner: String = preferred_local_owner(state).unwrap_or_else(|| owner.clone());
    schedule_command(
        state,
        &local_owner,
        Command::Select {
            entity_ids: matching_ids,
            additive,
        },
    );
}

/// Emit VoiceSelect sound for the first entity in a selection snapshot.
fn emit_selection_voice(state: &mut AppState, snapshot: &[u64]) {
    let Some(first_id) = snapshot.first() else {
        return;
    };
    let Some(sim) = &state.simulation else { return };
    let Some(rules) = &state.rules else { return };

    // Find the entity's type and look up its VoiceSelect sound.
    if let Some(entity) = sim.entities.get(*first_id) {
        if let Some(obj) = rules.object(sim.interner.resolve(entity.type_ref)) {
            if let Some(ref voice_id) = obj.voice_select {
                state.sound_events.push(GameSoundEvent::UnitSelected {
                    sound_id: voice_id.clone(),
                });
            }
        }
    }
}

/// Jump camera to the local player's base.
///
/// Priority: ConYard (structure with `UndeploysInto=`) → MCV (unit with `DeploysInto=`)
/// → multiplayer start waypoint 0 as fallback.
fn jump_camera_to_base(state: &mut AppState) {
    let owner = preferred_local_owner_name(state);
    let owner_name = owner.as_deref();

    // Collect the target cell from simulation entities before mutating state.
    let target: Option<(u16, u16)> = state.simulation.as_ref().and_then(|sim| {
        let rules = state.rules.as_ref();
        // First pass: look for a ConYard (structure with UndeploysInto=).
        let conyard = sim.entities.values().find(|e| {
            e.category == EntityCategory::Structure
                && owner_name.map_or(true, |o| {
                    sim.interner.resolve(e.owner).eq_ignore_ascii_case(o)
                })
                && rules
                    .and_then(|r| r.object(sim.interner.resolve(e.type_ref)))
                    .map_or(false, |o| o.undeploys_into.is_some())
        });
        if let Some(entity) = conyard {
            log::info!(
                "H: jumping to ConYard {} at ({}, {})",
                sim.interner.resolve(entity.type_ref),
                entity.position.rx,
                entity.position.ry
            );
            return Some((entity.position.rx, entity.position.ry));
        }
        // Second pass: look for an MCV (unit with DeploysInto=).
        let mcv = sim.entities.values().find(|e| {
            e.category != EntityCategory::Structure
                && owner_name.map_or(true, |o| {
                    sim.interner.resolve(e.owner).eq_ignore_ascii_case(o)
                })
                && rules
                    .and_then(|r| r.object(sim.interner.resolve(e.type_ref)))
                    .map_or(false, |o| o.deploys_into.is_some())
        });
        if let Some(entity) = mcv {
            log::info!(
                "H: jumping to MCV {} at ({}, {})",
                sim.interner.resolve(entity.type_ref),
                entity.position.rx,
                entity.position.ry
            );
            return Some((entity.position.rx, entity.position.ry));
        }
        log::info!(
            "H: no ConYard/MCV found (owner={:?}, entities={}, rules={})",
            owner_name,
            sim.entities.len(),
            rules.is_some()
        );
        None
    });

    if let Some((rx, ry)) = target {
        crate::app_camera::center_camera_on_cell(state, rx, ry);
        return;
    }

    // Fallback: jump to the first multiplayer start waypoint.
    if let Some(wp) = crate::map::waypoints::first_multiplayer_start(&state.waypoints) {
        log::info!(
            "H: falling back to start waypoint at ({}, {})",
            wp.rx,
            wp.ry
        );
        crate::app_camera::center_camera_on_cell(state, wp.rx, wp.ry);
    } else {
        log::info!("H: no base or start waypoint found");
    }
}

/// Emit a voice sound (VoiceMove or VoiceAttack) for the first selected unit.
pub(crate) fn emit_order_voice(state: &mut AppState, voice_field: &str) {
    let Some(sim) = &state.simulation else { return };
    let Some(rules) = &state.rules else { return };

    // Find first selected entity and get its voice sound.
    let first_selected = sim.entities.values().find(|e| e.selected);
    let Some(sel_entity) = first_selected else {
        return;
    };
    if let Some(obj) = rules.object(sim.interner.resolve(sel_entity.type_ref)) {
        let voice_id: Option<&String> = match voice_field {
            "VoiceMove" => obj.voice_move.as_ref(),
            "VoiceAttack" => obj.voice_attack.as_ref(),
            _ => None,
        };
        if let Some(id) = voice_id {
            let event = match voice_field {
                "VoiceAttack" => GameSoundEvent::UnitAttackOrder {
                    sound_id: id.clone(),
                },
                _ => GameSoundEvent::UnitMoveOrder {
                    sound_id: id.clone(),
                },
            };
            state.sound_events.push(event);
        }
    }
}
