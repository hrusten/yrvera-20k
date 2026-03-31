//! App state transitions: map loading into InGame, screen clearing.
//!
//! Extracted from app.rs for file-size limits.

use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use crate::app_init;
use crate::app_render;
use crate::app_sim_tick;
use crate::map::basic::BasicSection;
use crate::map::houses::HouseRoster;
use crate::map::overlay_types::OverlayTypeRegistry;
use crate::map::trigger_graph::TriggerGraph;
use crate::render::minimap::MinimapRenderer;
use crate::render::selection_overlay::SelectionOverlay;
use crate::sidebar::SidebarTab;
use crate::sim::trigger_runtime::TriggerRuntime;
use crate::ui::game_screen::GameScreen;

use crate::app::AppState;

/// Background clear color for menu screens (dark blue).
const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.02,
    g: 0.04,
    b: 0.12,
    a: 1.0,
};

/// Load map data and transition to InGame state.
pub(crate) fn transition_to_in_game(state: &mut AppState) {
    log::info!("Loading map...");
    let requested_map: Option<String> = match &state.screen {
        GameScreen::Loading { map_name } => Some(map_name.clone()),
        _ => None,
    };
    let result = app_init::load_map(
        &state.gpu,
        &state.batch_renderer,
        requested_map.as_deref(),
        &state.skirmish_settings,
        state.vxl_compute.as_mut(),
    )
    .unwrap_or_else(|err| {
        log::warn!("Could not load map: {:#}", err);
        app_init::MapLoadResult {
            basic: BasicSection::default(),
            tile_atlas: None,
            terrain_grid: None,
            resolved_terrain: None,
            simulation: None,
            unit_atlas: None,
            sprite_atlas: None,
            overlay_atlas: None,
            bridge_atlas: None,
            sidebar_cameo_atlas: None,
            sidebar_chrome: None,
            software_cursor: None,
            overlays: Vec::new(),
            terrain_objects: Vec::new(),
            waypoints: HashMap::new(),
            cell_tags: HashMap::new(),
            tags: HashMap::new(),
            triggers: HashMap::new(),
            events: HashMap::new(),
            actions: HashMap::new(),
            trigger_graph: TriggerGraph::default(),
            trigger_runtime: TriggerRuntime::default(),
            overlay_names: BTreeMap::new(),
            tiberium_radar_colors: HashMap::new(),
            overlay_registry: OverlayTypeRegistry::empty(),
            house_color_map: HashMap::new(),
            house_roster: HouseRoster::default(),
            height_map: BTreeMap::new(),
            bridge_height_map: BTreeMap::new(),
            lighting_grid: HashMap::new(),
            path_grid: None,
            path_grid_base: None,
            rules: None,
            art_registry: None,
            csf: None,
            fnt_file: None,
            camera_x: 0.0,
            camera_y: 0.0,
            asset_manager: None,
            theater_name: "TEMPERATE".to_string(),
            theater_ext: "tem".to_string(),
            initial_local_owner: None,
            sandbox_full_visibility: false,
            spawn_pick_pending: false,
            infantry_sequences: HashMap::new(),
        }
    });
    state.tile_atlas = result.tile_atlas;
    state.map_basic = result.basic;
    state.terrain_grid = result.terrain_grid;
    state.resolved_terrain = result.resolved_terrain;
    state.simulation = result.simulation;
    if let Some(sim) = &mut state.simulation {
        sim.input_delay_ticks = state.configured_input_delay_ticks;
    }
    state.unit_atlas = result.unit_atlas;
    state.sprite_atlas = result.sprite_atlas;
    state.overlay_atlas = result.overlay_atlas;
    state.bridge_atlas = result.bridge_atlas;
    state.sidebar_cameo_atlas = result.sidebar_cameo_atlas;
    state.sidebar_chrome = result.sidebar_chrome;
    if let Some(ref fnt) = result.fnt_file {
        state.sidebar_text = crate::render::sidebar_text::SidebarTextRenderer::from_fnt(
            &state.gpu,
            &state.batch_renderer,
            fnt,
        );
    }

    // Initialize radar animation from the default (Allied) sidebar chrome atlas.
    // Uses pre-rendered radar.shp frames for the 33-frame open/close animation.
    // Also extract content insets derived from the transparent opening in frame 0.
    let allied_atlas = state
        .sidebar_chrome
        .as_ref()
        .and_then(|set| set.for_theme(crate::render::sidebar_chrome::SidebarTheme::Allied));
    state.radar_anim = allied_atlas.and_then(|atlas| {
        if atlas.radar_frames.is_empty() {
            return None;
        }
        let [w, h] = atlas.radar_frame_size;
        crate::render::radar_anim::RadarAnimState::new(
            &state.gpu,
            &state.batch_renderer,
            atlas.radar_frames.clone(),
            w,
            h,
        )
    });
    state.radar_content_insets = allied_atlas.map(|atlas| atlas.radar_content_insets);
    state.has_radar = false;

    state.software_cursor = result.software_cursor;
    state.overlays = result.overlays;
    state.terrain_objects = result.terrain_objects;
    state.waypoints = result.waypoints;
    state.cell_tags = result.cell_tags;
    state.tags = result.tags;
    state.triggers = result.triggers;
    state.events = result.events;
    state.actions = result.actions;
    state.trigger_graph = result.trigger_graph;
    if let Some(sim) = &mut state.simulation {
        sim.trigger_runtime = result.trigger_runtime;
    }
    state.overlay_names = result.overlay_names;
    state.tiberium_radar_colors = result.tiberium_radar_colors;
    state.overlay_registry = Some(result.overlay_registry);
    state.house_color_map = result.house_color_map;
    state.house_roster = result.house_roster;
    state.height_map = result.height_map;
    state.bridge_height_map = result.bridge_height_map;
    state.lighting_grid = result.lighting_grid;
    state.path_grid = result.path_grid;
    state.path_grid_base = result.path_grid_base;
    state.rules = result.rules;
    state.art_registry = result.art_registry;
    state.infantry_sequences = result.infantry_sequences;
    state.csf = result.csf;
    state.theater_name = result.theater_name;
    state.theater_ext = result.theater_ext;
    state.camera_x = result.camera_x;
    state.camera_y = result.camera_y;
    state.asset_manager = result.asset_manager;
    state.building_placement_preview = None;
    state.active_sidebar_tab = SidebarTab::default_active_tab();
    state.sidebar_scroll_rows = 0;
    state.mission_announcement = None;
    state.mission_announcement_deadline = None;
    let map_title: &str = state.map_basic.name.as_deref().unwrap_or("Unknown Map");
    state.window.set_title(&format!("RA2 - {}", map_title));
    state
        .window
        .set_cursor_visible(state.software_cursor.is_none());

    // Create minimap from terrain grid with overlay data.
    if let Some(grid) = &state.terrain_grid {
        let overlay_data: Vec<(
            u16,
            u16,
            crate::render::minimap::OverlayClassification,
            u8,
            Option<[u8; 4]>,
        )> = build_minimap_overlay_data(
            &state.overlays,
            &state.terrain_objects,
            &state.overlay_names,
            state.rules.as_ref(),
            &state.tiberium_radar_colors,
        );
        state.minimap = Some(MinimapRenderer::new(
            &state.gpu,
            &state.batch_renderer,
            grid,
            &overlay_data,
            &state.theater_name,
        ));
    }
    state.minimap_dragging = false;

    // Create selection overlay for rendering highlights and drag rect.
    // Pass asset_manager so it can load pips.shp for authentic health bar pips.
    state.selection_overlay = Some(SelectionOverlay::new(
        &state.gpu,
        &state.batch_renderer,
        state.asset_manager.as_ref(),
    ));

    // Create GPU ABuffer for per-pixel shroud darkening.
    // Loads SHROUD.SHP brightness data and the 256-byte edge LUT.
    if let Some(ref am) = state.asset_manager {
        if let Some(ref grid) = state.path_grid {
            if let Some(shp_data) = am.get_ref("shroud.shp") {
                if let Ok(shp) = crate::assets::shp_file::ShpFile::from_bytes(shp_data) {
                    let (frame_pixels, cw, ch) =
                        crate::render::shroud_buffer::extract_shp_brightness(&shp);
                    state.shroud_buffer = Some(crate::render::shroud_buffer::ShroudBuffer::new(
                        &state.gpu,
                        state.render_width(),
                        state.render_height(),
                        grid.width(),
                        grid.height(),
                        frame_pixels,
                        cw,
                        ch,
                        crate::render::shroud_buffer::SHROUD_EDGE_LUT,
                    ));
                }
            }
        }
    }

    // Build animation sequences for known entity types (data-driven from art.ini).
    state.animation_sequences = app_sim_tick::build_animation_sequences(
        state.simulation.as_ref(),
        state.art_registry.as_ref(),
        &state.infantry_sequences,
    );

    state.last_update_time = Instant::now();
    state.sim_accumulator_ms = 0;
    state.queued_order_mode = app_render::OrderMode::Move;
    for group in &mut state.control_groups {
        group.clear();
    }
    state.local_owner_override = result.initial_local_owner;
    state.sandbox_full_visibility = result.sandbox_full_visibility;
    state.spawn_pick_pending = result.spawn_pick_pending;

    // Load sound.ini / soundmd.ini for SFX sound ID resolution.
    if let Some(ref assets) = state.asset_manager {
        state.sound_registry = load_sound_registry(assets);
        state.audio_indices = load_audio_indices(assets);
        state.eva_registry = load_eva_registry(assets);
    }

    // Start music playback: prefer map's Theme= field, otherwise play first playlist track.
    if let (Some(player), Some(assets)) = (&mut state.music_player, &state.asset_manager) {
        let started: bool = if let Some(ref theme) = state.map_basic.theme {
            player.play_track(theme, assets)
        } else {
            false
        };
        if !started {
            let _ = player.play_next(assets);
        }
    }

    if state.spawn_pick_pending {
        state.screen = GameScreen::SpawnPick;
        log::info!("Transitioned to SpawnPick — player must choose a start location");
    } else {
        state.screen = GameScreen::InGame;
        log::info!("Transitioned to InGame");
    }
}

/// Load sound.ini / soundmd.ini and build a SoundRegistry.
/// YR-first: soundmd.ini takes precedence, sound.ini fills gaps.
fn load_sound_registry(
    assets: &crate::assets::asset_manager::AssetManager,
) -> crate::rules::sound_ini::SoundRegistry {
    use crate::rules::ini_parser::IniFile;
    use crate::rules::sound_ini::SoundRegistry;

    // Try YR sound.ini first (soundmd.ini).
    let mut registry: Option<SoundRegistry> = None;
    for name in ["soundmd.ini", "sound.ini"] {
        if let Some(bytes) = assets.get(name) {
            if let Ok(text) = String::from_utf8(bytes) {
                let ini: IniFile = IniFile::from_str(&text);
                match &mut registry {
                    None => {
                        registry = Some(SoundRegistry::from_ini(&ini));
                        log::info!("Loaded {} for SFX", name);
                    }
                    Some(reg) => {
                        reg.merge_fallback(&ini);
                        log::info!("Merged fallback {} for SFX", name);
                    }
                }
            }
        }
    }
    registry.unwrap_or_default()
}

/// Load audio.idx/bag indices for bag-based sound playback (voices, EVA).
///
/// Tries YR (audiomd) first, then base RA2 (audio). Both are loaded if present
/// so YR sounds take priority but base RA2 sounds are still available.
fn load_audio_indices(
    assets: &crate::assets::asset_manager::AssetManager,
) -> Vec<crate::assets::audio_bag::AudioIndex> {
    use crate::assets::audio_bag::AudioIndex;

    let mut indices = Vec::new();

    // Both AUDIO.MIX and AUDIOMD.MIX contain entries named "audio.idx" and "audio.bag"
    // internally. We need to load each MIX explicitly and extract from within, because
    // the generic first-match lookup would conflate the shared internal filenames.
    // YR (AUDIOMD.MIX) is loaded first so its sounds take priority in the search.
    for mix_name in ["AUDIOMD.MIX", "AUDIO.MIX"] {
        let Some(mix) = assets.archive(mix_name) else {
            continue;
        };
        let idx_data = match mix.get_by_name("audio.idx") {
            Some(d) => d,
            None => {
                log::warn!("{} has no audio.idx entry", mix_name);
                continue;
            }
        };
        let bag_data = match mix.get_by_name("audio.bag") {
            Some(d) => d.to_vec(),
            None => {
                log::warn!("{} has audio.idx but no audio.bag", mix_name);
                continue;
            }
        };
        match AudioIndex::from_idx_bag(idx_data, bag_data) {
            Some(index) => {
                log::info!(
                    "Loaded audio.idx/bag from {}: {} entries",
                    mix_name,
                    index.len()
                );
                indices.push(index);
            }
            None => {
                log::warn!("Failed to parse audio.idx from {}", mix_name);
            }
        }
    }

    if indices.is_empty() {
        log::warn!("No audio.idx/bag found — bag-based sounds (voices, EVA) will be silent");
    }
    indices
}

/// Load eva.ini / evamd.ini and build an EvaRegistry.
/// YR-first: evamd.ini takes precedence, eva.ini fills gaps.
fn load_eva_registry(
    assets: &crate::assets::asset_manager::AssetManager,
) -> crate::rules::sound_ini::EvaRegistry {
    use crate::rules::ini_parser::IniFile;
    use crate::rules::sound_ini::EvaRegistry;

    let mut registry: Option<EvaRegistry> = None;
    for name in ["evamd.ini", "eva.ini"] {
        if let Some(bytes) = assets.get(name) {
            // EVA INI files from MIX archives may contain non-UTF8 bytes (Windows-1252).
            let text = String::from_utf8_lossy(&bytes);
            let ini: IniFile = IniFile::from_str(&text);
            match &mut registry {
                None => {
                    registry = Some(EvaRegistry::from_ini(&ini));
                    log::info!("Loaded {} for EVA", name);
                }
                Some(reg) => {
                    reg.merge_fallback(&ini);
                    log::info!("Merged fallback {} for EVA", name);
                }
            }
        }
    }
    registry.unwrap_or_default()
}

/// Build overlay classification data for the minimap from map overlay entries.
///
/// Classifies each overlay by name pattern (TIB* = ore, GEM* = gem, WALL/FENCE = wall,
/// BRIDGE/BRDG = bridge) and each terrain object as TerrainObject.
fn build_minimap_overlay_data(
    overlays: &[crate::map::overlay::OverlayEntry],
    terrain_objects: &[crate::map::overlay::TerrainObject],
    overlay_names: &BTreeMap<u8, String>,
    _rules: Option<&crate::rules::ruleset::RuleSet>,
    tiberium_colors: &HashMap<(u8, u8), [u8; 3]>,
) -> Vec<(
    u16,
    u16,
    crate::render::minimap::OverlayClassification,
    u8,
    Option<[u8; 4]>,
)> {
    use crate::render::minimap::OverlayClassification;

    let mut data: Vec<(u16, u16, OverlayClassification, u8, Option<[u8; 4]>)> =
        Vec::with_capacity(overlays.len() + terrain_objects.len());

    for entry in overlays {
        let name: &str = match overlay_names.get(&entry.overlay_id) {
            Some(n) => n.as_str(),
            None => continue,
        };
        let upper: String = name.to_ascii_uppercase();
        let classification: OverlayClassification = if upper.starts_with("GEM") {
            OverlayClassification::Gem
        } else if upper.starts_with("TIB") {
            OverlayClassification::Ore
        } else if upper.contains("WALL") || upper.contains("FENCE") {
            OverlayClassification::Wall
        } else if upper.contains("BRIDGE") || upper.contains("BRDG") {
            OverlayClassification::Bridge
        } else {
            OverlayClassification::Other
        };
        // For tiberium overlays (Ore/Gem), look up the precomputed SHP-derived color.
        let precomputed: Option<[u8; 4]> = match classification {
            OverlayClassification::Ore | OverlayClassification::Gem => tiberium_colors
                .get(&(entry.overlay_id, entry.frame))
                .map(|&[r, g, b]| [r, g, b, 255]),
            _ => None,
        };
        data.push((entry.rx, entry.ry, classification, entry.frame, precomputed));
    }

    for obj in terrain_objects {
        data.push((
            obj.rx,
            obj.ry,
            OverlayClassification::TerrainObject,
            0,
            None,
        ));
    }

    data
}

/// Clear the screen to the background color (no depth buffer).
pub(crate) fn clear_screen(encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
    let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("Clear Pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(CLEAR_COLOR),
                store: wgpu::StoreOp::Store,
            },
            depth_slice: None,
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
    });
}
