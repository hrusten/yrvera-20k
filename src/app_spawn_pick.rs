//! Spawn-pick phase — player sees the full map and clicks a waypoint to start.
//!
//! During SpawnPick the entire map is rendered without fog or simulation ticking.
//! Multiplayer start waypoints (0..=7) are drawn as clickable markers.
//! When the player clicks a marker, MCVs are seeded and the game transitions
//! to InGame.
//!
//! ## Dependency rules
//! - Part of the app layer — may depend on everything.

use std::time::Instant;

use crate::app::AppState;
use crate::app_init_helpers::build_entity_atlases;
use crate::app_render;
use crate::app_skirmish::seed_skirmish_opening_if_needed;
use crate::map::terrain;
use crate::map::waypoints;
use crate::ui::game_screen::GameScreen;
use crate::ui::main_menu::StartPosition;

/// Radius (in screen pixels) around a waypoint marker that counts as a click.
const WAYPOINT_CLICK_RADIUS: f32 = 40.0;

/// Check if the cursor is over a waypoint marker and return its index if so.
pub(crate) fn hovered_waypoint(state: &AppState) -> Option<usize> {
    let starts = waypoints::multiplayer_start_waypoints(&state.waypoints);
    let cx: f32 = state.cursor_x;
    let cy: f32 = state.cursor_y;

    for (i, wp) in starts.iter().enumerate() {
        let z: u8 = state.height_map.get(&(wp.rx, wp.ry)).copied().unwrap_or(0);
        let (world_x, world_y) = terrain::iso_to_screen(wp.rx, wp.ry, z);
        let screen_x: f32 = world_x - state.camera_x;
        let screen_y: f32 = world_y - state.camera_y;

        let dx: f32 = cx - screen_x;
        let dy: f32 = cy - screen_y;
        if dx * dx + dy * dy <= WAYPOINT_CLICK_RADIUS * WAYPOINT_CLICK_RADIUS {
            return Some(i);
        }
    }
    None
}

/// Handle a left-click during SpawnPick: if the player clicked a waypoint,
/// seed MCVs and transition to InGame. Returns true if a waypoint was clicked.
pub(crate) fn handle_spawn_pick_click(state: &mut AppState) -> bool {
    let Some(wp_idx) = hovered_waypoint(state) else {
        return false;
    };

    let starts = waypoints::multiplayer_start_waypoints(&state.waypoints);
    if wp_idx >= starts.len() {
        return false;
    }

    log::info!(
        "Player picked spawn waypoint {} at ({}, {})",
        starts[wp_idx].index,
        starts[wp_idx].rx,
        starts[wp_idx].ry,
    );

    // Update skirmish settings to use the chosen position, then seed MCVs.
    state.skirmish_settings.start_position = StartPosition::Position(wp_idx as u8);

    // Build temp map data before borrowing state.simulation mutably.
    let temp_map = build_temp_map_data_for_seeding(state);
    let seeded_owner: Option<String> =
        if let (Some(sim), Some(ruleset)) = (&mut state.simulation, state.rules.as_ref()) {
            seed_skirmish_opening_if_needed(
                sim,
                &temp_map,
                &state.house_roster,
                ruleset,
                &state.height_map,
                &state.skirmish_settings,
            )
        } else {
            None
        };

    // Set up AI players and rebuild entity atlases now that MCVs are spawned.
    if let Some(ref local_owner) = seeded_owner {
        if let Some(sim) = &mut state.simulation {
            setup_ai_players_from_roster(sim, &state.house_roster, local_owner);
            // Ensure the local player is marked human even if the map lacks PlayerControl=yes.
            if let Some(owner_id) = sim.interner.get(local_owner) {
                if let Some(house) = sim.houses.get_mut(&owner_id) {
                    house.is_human = true;
                }
            }
        }
        // Rebuild entity atlases to include the newly spawned MCVs.
        if let Some(sim) = &state.simulation {
            let asset_manager = state.asset_manager.as_ref();
            if let Some(assets) = asset_manager {
                let (new_unit_atlas, new_sprite_atlas) = build_entity_atlases(
                    sim,
                    assets,
                    &state.gpu,
                    &state.batch_renderer,
                    &state.theater_ext,
                    &state.theater_name,
                    state.rules.as_ref(),
                    state.art_registry.as_ref(),
                    &state.house_color_map,
                    None, // entity_unit_palette — atlas builder loads it from assets
                    &state.infantry_sequences,
                    state.vxl_compute.as_mut(),
                );
                state.unit_atlas = new_unit_atlas;
                state.sprite_atlas = new_sprite_atlas;
            }
        }
    }

    state.local_owner_override = seeded_owner;
    state.spawn_pick_pending = false;

    // Center camera on the chosen spawn position.
    let chosen_wp = starts[wp_idx];
    let z: u8 = state
        .height_map
        .get(&(chosen_wp.rx, chosen_wp.ry))
        .copied()
        .unwrap_or(0);
    let (sx, sy) = terrain::iso_to_screen(chosen_wp.rx, chosen_wp.ry, z);
    let sw: f32 = state.render_width() as f32;
    let sh: f32 = state.render_height() as f32;
    let zm = state.zoom_level;
    state.camera_x = sx - sw / (2.0 * zm);
    state.camera_y = sy - sh / (2.0 * zm);

    // Reset timing for clean InGame start.
    state.last_update_time = Instant::now();
    state.sim_accumulator_ms = 0;

    state.screen = GameScreen::InGame;
    log::info!("SpawnPick complete — transitioned to InGame");
    true
}

/// Build a minimal MapFile for seed_skirmish_opening_if_needed.
/// Only waypoints and ini matter for seeding; all other fields are defaults.
fn build_temp_map_data_for_seeding(state: &AppState) -> crate::map::map_file::MapFile {
    use crate::map::map_file::{MapFile, MapHeader};
    use crate::rules::ini_parser::IniFile;

    MapFile {
        header: MapHeader {
            theater: state.theater_name.clone(),
            width: 0,
            height: 0,
            local_left: 0,
            local_top: 0,
            local_width: 0,
            local_height: 0,
        },
        basic: crate::map::basic::BasicSection::default(),
        briefing: crate::map::briefing::BriefingSection::default(),
        special_flags: crate::map::basic::SpecialFlagsSection::default(),
        ini: IniFile::from_str(""),
        cells: Vec::new(),
        entities: Vec::new(),
        overlays: Vec::new(),
        terrain_objects: Vec::new(),
        waypoints: state.waypoints.clone(),
        cell_tags: std::collections::HashMap::new(),
        tags: std::collections::HashMap::new(),
        triggers: std::collections::HashMap::new(),
        events: std::collections::HashMap::new(),
        actions: std::collections::HashMap::new(),
        trigger_graph: crate::map::trigger_graph::TriggerGraph::default(),
        local_variables: std::collections::HashMap::new(),
        preview: crate::map::preview::PreviewSection::default(),
    }
}

/// Register non-local playable houses as AI opponents (same logic as app_init).
fn setup_ai_players_from_roster(
    sim: &mut crate::sim::world::Simulation,
    house_roster: &crate::map::houses::HouseRoster,
    local_owner: &str,
) {
    use crate::sim::ai::AiPlayerState;

    for house in &house_roster.houses {
        let up = house.name.to_ascii_uppercase();
        if matches!(
            up.as_str(),
            "NEUTRAL" | "SPECIAL" | "CIVILIAN" | "GOODGUY" | "BADGUY" | "JP"
        ) {
            continue;
        }
        if house.name.eq_ignore_ascii_case(local_owner) {
            continue;
        }
        sim.ai_players
            .push(AiPlayerState::new(sim.interner.intern(&house.name)));
        log::info!("AI player registered: {}", house.name);
    }
}

/// Render the SpawnPick phase: full map visible, no fog, no simulation tick.
///
/// Temporarily enables sandbox visibility so the entire map is shown.
pub(crate) fn render_spawn_pick(
    state: &mut AppState,
    encoder: &mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
) -> anyhow::Result<()> {
    // Temporarily enable sandbox visibility so the whole map is visible.
    let prev_visibility = state.sandbox_full_visibility;
    state.sandbox_full_visibility = true;
    let result = app_render::render_game(state, encoder, view);
    state.sandbox_full_visibility = prev_visibility;
    result?;
    Ok(())
}

/// Draw the SpawnPick egui overlay: instructions + hovered waypoint info.
pub(crate) fn draw_spawn_pick_overlay(ctx: &egui::Context, state: &AppState) {
    let starts = waypoints::multiplayer_start_waypoints(&state.waypoints);
    let hovered = hovered_waypoint(state);

    // Top-center banner with instructions.
    egui::Area::new(egui::Id::new("spawn_pick_banner"))
        .anchor(egui::Align2::CENTER_TOP, [0.0, 20.0])
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(egui::Color32::from_rgba_premultiplied(0, 0, 0, 200))
                .inner_margin(egui::Margin::symmetric(20, 12))
                .corner_radius(8.0)
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("Choose Your Starting Position")
                                .size(24.0)
                                .strong()
                                .color(egui::Color32::WHITE),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(
                                "Click a marker on the map to place your MCV. Scroll to explore the map.",
                            )
                            .size(14.0)
                            .color(egui::Color32::from_rgb(200, 200, 200)),
                        );
                        if let Some(idx) = hovered {
                            ui.add_space(4.0);
                            let wp = starts[idx];
                            ui.label(
                                egui::RichText::new(format!(
                                    "Position {} — Cell ({}, {})",
                                    idx + 1,
                                    wp.rx,
                                    wp.ry
                                ))
                                .size(16.0)
                                .color(egui::Color32::from_rgb(100, 255, 100)),
                            );
                        }
                    });
                });
        });
}
