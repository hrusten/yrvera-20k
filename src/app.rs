//! Application orchestrator — ties all subsystems together.
//! Implements winit's ApplicationHandler. GPU init deferred to resumed().
//! Helpers: app_init.rs (loading), app_render.rs (rendering).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use winit::application::ApplicationHandler;
use winit::event::{MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::app_init::MapMenuEntry;
use crate::app_input;
use crate::app_list_maps;
use crate::app_render;
use crate::app_sim_tick;
use crate::app_transitions;
use crate::assets::asset_manager::AssetManager;
use crate::audio::events::SoundEventQueue;
use crate::audio::music::MusicPlayer;
use crate::audio::sfx::SfxPlayer;
use crate::map::actions::ActionMap;
use crate::map::basic::BasicSection;
use crate::map::cell_tags::CellTagMap;
use crate::map::events::EventMap;
use crate::map::houses::{HouseColorMap, HouseRoster};
use crate::map::lighting::LightingGrid;
use crate::map::overlay::{OverlayEntry, TerrainObject};
use crate::map::overlay_types::OverlayTypeRegistry;
use crate::map::resolved_terrain::ResolvedTerrainGrid;
use crate::map::tags::TagMap;
use crate::map::terrain::TerrainGrid;
use crate::map::trigger_graph::TriggerGraph;
use crate::sim::trigger_runtime::TriggerRuntime;
use crate::map::triggers::TriggerMap;
use crate::map::waypoints::Waypoint;
use crate::render::batch::BatchRenderer;
use crate::render::bridge_atlas::BridgeAtlas;
use crate::render::egui_integration::EguiIntegration;
use crate::render::gpu::GpuContext;
use crate::render::minimap::MinimapRenderer;
use crate::render::overlay_atlas::OverlayAtlas;
use crate::render::selection_overlay::SelectionOverlay;
use crate::render::sidebar_cameo_atlas::SidebarCameoAtlas;
use crate::render::sidebar_chrome::SidebarChromeSet;
use crate::render::sidebar_text::SidebarTextRenderer;
use crate::render::sprite_atlas::SpriteAtlas;
use crate::render::tile_atlas::TileAtlas;
use crate::render::unit_atlas::UnitAtlas;
use crate::rules::art_data::ArtRegistry;
use crate::rules::infantry_sequence::InfantrySequenceRegistry;
use crate::rules::sound_ini::SoundRegistry;
use crate::sidebar::{SidebarChromeLayoutSpec, SidebarTab};
use crate::sim::animation::SequenceSet;
use crate::sim::command::CommandEnvelope;
use crate::sim::pathfinding::PathGrid;
use crate::sim::production::BuildingPlacementPreview;
use crate::sim::replay::ReplayLog;
use crate::sim::selection::SelectionState;
use crate::sim::world::Simulation;
use crate::ui::game_screen::GameScreen;
use crate::ui::main_menu::{self, MenuAction, SkirmishSettings};
use crate::util::config::GameConfig;

/// All initialized state. Created in `resumed()` when the window is available.
/// pub(crate) so app_render.rs can access fields.
pub(crate) struct AppState {
    pub(crate) window: Arc<Window>,
    pub(crate) gpu: GpuContext,
    pub(crate) batch_renderer: BatchRenderer,
    /// Reusable GPU instance buffers — avoids per-frame GPU buffer allocation.
    pub(crate) instance_pool: crate::render::batch::InstanceBufferPool,
    pub(crate) tile_atlas: Option<TileAtlas>,
    pub(crate) map_basic: BasicSection,
    pub(crate) terrain_grid: Option<TerrainGrid>,
    pub(crate) resolved_terrain: Option<ResolvedTerrainGrid>,
    pub(crate) simulation: Option<Simulation>,
    pub(crate) unit_atlas: Option<UnitAtlas>,
    pub(crate) vxl_compute: Option<crate::render::vxl_compute::VxlComputeRenderer>,
    pub(crate) sprite_atlas: Option<SpriteAtlas>,
    pub(crate) overlay_atlas: Option<OverlayAtlas>,
    pub(crate) bridge_atlas: Option<BridgeAtlas>,
    /// Overlay entries from map for per-frame instance generation.
    pub(crate) overlays: Vec<OverlayEntry>,
    /// Terrain objects from map for per-frame instance generation.
    pub(crate) terrain_objects: Vec<TerrainObject>,
    pub(crate) waypoints: HashMap<u32, Waypoint>,
    pub(crate) cell_tags: CellTagMap,
    pub(crate) tags: TagMap,
    pub(crate) triggers: TriggerMap,
    pub(crate) events: EventMap,
    pub(crate) actions: ActionMap,
    pub(crate) trigger_graph: TriggerGraph,
    pub(crate) trigger_runtime: TriggerRuntime,
    /// Overlay ID → type name mapping for atlas lookups at render time.
    pub(crate) overlay_names: BTreeMap<u8, String>,
    /// Precomputed average pixel color for each tiberium overlay (id, frame) pair,
    /// extracted from SHP frames for minimap radar display.
    pub(crate) tiberium_radar_colors: HashMap<(u8, u8), [u8; 3]>,
    /// Registry of overlay types from rules.ini — needed at runtime to look up
    /// overlay_id by name when a wall is placed via production.
    pub(crate) overlay_registry: Option<OverlayTypeRegistry>,
    /// GPU depth texture for back-to-front depth ordering. Recreated on window resize.
    pub(crate) depth_view: wgpu::TextureView,
    /// Optional Catmull-Rom bicubic upscale pass (render at lower res, upscale to window).
    pub(crate) upscale_pass: Option<crate::render::upscale_pass::UpscalePass>,
    pub(crate) camera_x: f32,
    pub(crate) camera_y: f32,
    /// Current zoom level for the game viewport. 1.0 = native pixel scale,
    /// >1.0 = zoomed in (world appears larger), <1.0 = zoomed out (see more map).
    /// Animated each frame toward `zoom_target`.
    pub(crate) zoom_level: f32,
    /// Target zoom level — mouse wheel sets this; `zoom_level` eases toward it.
    pub(crate) zoom_target: f32,
    /// World-space anchor point for zoom animation. The camera adjusts each frame
    /// so this world point stays at `zoom_anchor_screen` during the zoom ease.
    pub(crate) zoom_anchor_world: [f32; 2],
    /// Screen-space position of the zoom anchor (cursor position when wheel fired).
    pub(crate) zoom_anchor_screen: [f32; 2],
    pub(crate) cursor_x: f32,
    pub(crate) cursor_y: f32,
    pub(crate) keys_held: HashSet<KeyCode>,
    /// egui integration — input handling + GPU rendering.
    egui: EguiIntegration,
    /// Which screen is currently active (MainMenu, Loading, InGame).
    pub(crate) screen: GameScreen,
    /// Available maps from the RA2 directory for menu selection.
    available_maps: Vec<MapMenuEntry>,
    /// Player-configured skirmish settings (map, country, credits, etc.).
    pub(crate) skirmish_settings: SkirmishSettings,
    /// Minimap renderer — created at map load time.
    pub(crate) minimap: Option<MinimapRenderer>,
    /// True while left-dragging on minimap (camera pan mode).
    pub(crate) minimap_dragging: bool,
    /// True while middle-mouse button is held for fast camera panning.
    pub(crate) middle_mouse_panning: bool,
    /// Cursor position when middle-mouse pan started (screen pixels).
    pub(crate) middle_mouse_anchor_x: f32,
    pub(crate) middle_mouse_anchor_y: f32,
    /// Animated radar chrome — plays 33-frame open/close animation when radar gained/lost.
    pub(crate) radar_anim: Option<crate::render::radar_anim::RadarAnimState>,
    /// Animated power bar — segment-by-segment transition matching original PowerClass.
    pub(crate) power_bar_anim: crate::sidebar::PowerBarAnimState,
    /// Smoothly animated credits display per owner — ticks toward actual balance
    /// each frame (step = |diff| / 8, clamped to [1, 143]).
    pub(crate) displayed_credits: HashMap<String, i32>,
    /// Content insets [left, top, right, bottom] derived from the transparent opening
    /// in radar.shp frame 0. Used to position the minimap inside the chrome housing.
    /// Unscaled pixels — multiply by `ui_scale` at use site.
    pub(crate) radar_content_insets: Option<[u32; 4]>,
    /// Whether the local player currently has operational radar (power-gated).
    pub(crate) has_radar: bool,
    /// Selection overlay renderer — highlights and drag rectangle.
    pub(crate) selection_overlay: Option<SelectionOverlay>,
    /// Authentic SHROUD.SHP sprite-based shroud edge renderer.
    /// GPU ABuffer — screen-resolution brightness texture for per-pixel shroud darkening.
    /// SHROUD.SHP brightness pixels blitted per-cell, then a full-screen multiply pass
    /// darkens the scene.
    pub(crate) shroud_buffer: Option<crate::render::shroud_buffer::ShroudBuffer>,
    /// Packed cameo art used by the custom build sidebar.
    pub(crate) sidebar_cameo_atlas: Option<SidebarCameoAtlas>,
    /// Original side-mix shell art used to skin the custom sidebar.
    pub(crate) sidebar_chrome: Option<SidebarChromeSet>,
    /// Bitmap font atlas used by the custom sidebar text path.
    pub(crate) sidebar_text: SidebarTextRenderer,
    /// Asset-backed software cursor shown in-game when available.
    pub(crate) software_cursor: Option<app_render::SoftwareCursor>,
    /// Selection drag state — tracks mouse drag for box-select.
    pub(crate) selection_state: SelectionState,
    /// A* pathfinding grid — walkability data from terrain.
    pub(crate) path_grid: Option<PathGrid>,
    /// Terrain-only A* pathfinding grid used to rebuild dynamic structure blocking.
    pub(crate) path_grid_base: Option<PathGrid>,
    /// Sequence definitions per entity type for animation ticking.
    pub(crate) animation_sequences: BTreeMap<String, SequenceSet>,
    /// Game data from rules.ini — needed by combat system for weapon/warhead lookups.
    pub(crate) rules: Option<crate::rules::ruleset::RuleSet>,
    /// Art.ini registry — needed for building animation overlay lookups at render time.
    pub(crate) art_registry: Option<ArtRegistry>,
    /// Parsed infantry animation sequence definitions from art.ini [*Sequence] sections.
    pub(crate) infantry_sequences: InfantrySequenceRegistry,
    /// CSF string table — localized display names for units, buildings, UI text.
    pub(crate) csf: Option<crate::assets::csf_file::CsfFile>,
    /// Owner name → house color index mapping for atlas key lookups.
    pub(crate) house_color_map: HouseColorMap,
    pub(crate) house_roster: HouseRoster,
    /// Cell (rx, ry) → terrain elevation z for entity/overlay height lookup.
    pub(crate) height_map: BTreeMap<(u16, u16), u8>,
    /// Cell (rx, ry) → bridge deck elevation z. Only bridge cells present.
    /// Used by screen_to_iso to resolve clicks on high bridge surfaces.
    pub(crate) bridge_height_map: BTreeMap<(u16, u16), u8>,
    /// Cell (rx, ry) → RGB tint from map lighting. Entities/overlays look this up per-frame.
    pub(crate) lighting_grid: LightingGrid,
    /// Active map theater name (e.g., DESERT).
    pub(crate) theater_name: String,
    /// Active map theater extension (e.g., des).
    pub(crate) theater_ext: String,
    /// Timestamp of the last in-game update for delta time calculation.
    pub(crate) last_update_time: Instant,
    /// Accumulated real time waiting to be consumed by fixed simulation ticks.
    pub(crate) sim_accumulator_ms: u64,
    /// Pending gameplay commands waiting for deterministic execute tick.
    pub(crate) pending_commands: Vec<CommandEnvelope>,
    /// Target/action lines — colored lines from selected units to command destinations.
    pub(crate) target_lines: crate::app_target_lines::TargetLineState,
    /// Input delay in ticks for lockstep-style scheduling.
    pub(crate) input_delay_ticks: u64,
    /// Current in-memory replay log for this match.
    pub(crate) replay_log: Option<ReplayLog>,
    /// Pending order mode for the next right-click command.
    pub(crate) queued_order_mode: app_render::OrderMode,
    /// Control group slots (0-9) storing stable entity ids.
    pub(crate) control_groups: Vec<Vec<u64>>,
    /// Explicit local owner preference for HUD/commands (set by debug actions).
    pub(crate) local_owner_override: Option<String>,
    /// Seeded empty-map sandbox keeps full map visibility while still locking control.
    pub(crate) sandbox_full_visibility: bool,
    /// When true, computer-controlled players do nothing (no AI commands issued).
    pub(crate) disable_ai: bool,
    /// True when in SpawnPick phase — MCV seeding is deferred until the player picks a waypoint.
    pub(crate) spawn_pick_pending: bool,
    /// Ready building currently armed for left-click placement.
    pub(crate) armed_building_placement: Option<String>,
    /// Current placement preview for the armed building, if any.
    pub(crate) building_placement_preview: Option<BuildingPlacementPreview>,
    /// Active tab for the custom in-game sidebar.
    pub(crate) active_sidebar_tab: SidebarTab,
    /// Optional local override for chrome positioning loaded from sidebar_layout.ron.
    /// This is the SCALED version — multiply base by ui_scale at init/resize.
    pub(crate) sidebar_layout_spec: SidebarChromeLayoutSpec,
    /// Unscaled base layout spec (from file or stock). Kept for re-scaling on resize.
    pub(crate) sidebar_layout_spec_base: SidebarChromeLayoutSpec,
    /// Integer UI scale factor (1, 2, or 3). Auto-detected from screen height.
    /// Sidebar, minimap, and other UI elements are scaled by this factor.
    pub(crate) ui_scale: f32,
    /// Scroll offset for the current sidebar tab's item list.
    pub(crate) sidebar_scroll_rows: usize,
    /// Transient mission/script announcement shown in-game.
    pub(crate) mission_announcement: Option<String>,
    /// Absolute deadline for clearing the announcement banner.
    pub(crate) mission_announcement_deadline: Option<Instant>,
    /// Asset manager — kept alive for music track lookups.
    pub(crate) asset_manager: Option<AssetManager>,
    /// Background music player (rodio).
    pub(crate) music_player: Option<MusicPlayer>,
    /// Sound effect player (rodio) — plays one-shot SFX (weapons, voices, UI).
    pub(crate) sfx_player: Option<SfxPlayer>,
    /// sound.ini / soundmd.ini registry mapping IDs to .wav filenames.
    pub(crate) sound_registry: SoundRegistry,
    /// audio.idx/bag indices for bag-based sound lookup (voices, EVA).
    /// Searched in order (YR audiomd first, then base audio).
    pub(crate) audio_indices: Vec<crate::assets::audio_bag::AudioIndex>,
    /// EVA announcement registry from eva.ini / evamd.ini.
    /// Maps EVA event names to per-faction audio.bag sound IDs.
    pub(crate) eva_registry: crate::rules::sound_ini::EvaRegistry,
    /// Pending sound events from the current sim tick, drained each frame.
    pub(crate) sound_events: SoundEventQueue,
    /// Fire events from the current sim tick — position data for future muzzle
    /// flash rendering and projectile origin computation. Drained each frame.
    pub(crate) pending_fire_effects: Vec<crate::sim::world::SimFireEvent>,
    /// Active garrison muzzle flash animations. Short-lived one-shot entries
    /// spawned when a garrisoned building fires. Ticked each frame, removed on completion.
    pub(crate) garrison_muzzle_flashes: Vec<crate::sim::components::GarrisonMuzzleFlash>,
    /// True when the game is paused (ESC menu visible, sim frozen).
    pub(crate) paused: bool,
    /// When true, advance exactly one sim tick while paused, then clear.
    pub(crate) debug_frame_step_requested: bool,
    /// Effective simulation ticks per second — controls game speed.
    /// Default is SIM_TICK_HZ (45). Lower = slow-mo, higher = fast-forward.
    pub(crate) sim_speed_tps: u32,
    /// Hold the loading splash on screen briefly before showing the client UI.
    pub(crate) startup_splash_until: Option<Instant>,
    /// Global elapsed time for looping IdleAnim overlays (flags, smokestacks, etc.).
    pub(crate) idle_anim_elapsed_ms: u32,
    /// Debug overlay: show terrain cost / pathgrid overlay. Toggle with P / F9.
    pub(crate) debug_show_pathgrid: bool,
    /// SpeedType for terrain cost overlay. None = auto from selected unit (default Track).
    pub(crate) debug_terrain_cost_speed_type: Option<crate::rules::locomotor_type::SpeedType>,
    /// Debug overlay: show cell grid outlines (blue=terrain, yellow=overlay). Toggle with F8.
    pub(crate) debug_show_cell_grid: bool,
    /// Debug overlay: show height map elevation values. Toggle with H.
    pub(crate) debug_show_heightmap: bool,
    /// Show hotkey reference overlay. Toggle with F1.
    pub(crate) show_hotkey_help: bool,
    /// Debug unit inspector — shows event history for selected entities. Toggle with X.
    pub(crate) debug_unit_inspector: bool,
    // -- Reusable per-frame scratch buffers (avoid allocation each frame) --
    /// Overlay instance scratch vec — cleared and refilled each frame.
    pub(crate) cached_overlay_instances: Vec<crate::render::batch::SpriteInstance>,
    /// Unit (voxel) instance scratch vec — cleared and refilled each frame.
    pub(crate) cached_unit_instances: Vec<crate::render::batch::SpriteInstance>,
}

impl AppState {
    /// Effective render target width — intermediate texture when upscaling, else window.
    pub(crate) fn render_width(&self) -> u32 {
        self.upscale_pass
            .as_ref()
            .map_or(self.gpu.config.width, |u| u.src_width())
    }

    /// Effective render target height — intermediate texture when upscaling, else window.
    pub(crate) fn render_height(&self) -> u32 {
        self.upscale_pass
            .as_ref()
            .map_or(self.gpu.config.height, |u| u.src_height())
    }
}

/// Top-level application. Implements winit's ApplicationHandler.
pub struct App {
    state: Option<AppState>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self { state: None }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        log::info!("Application resumed — creating window and GPU context");
        match Self::initialize(event_loop) {
            Ok(state) => {
                self.state = Some(state);
                log::info!("Initialization complete — showing main menu");
            }
            Err(err) => {
                log::error!("Failed to initialize: {:#}", err);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = &mut self.state else { return };

        // Always let egui see the event first for input handling.
        let egui_response: egui_winit::EventResponse =
            state.egui.on_window_event(&state.window, &event);

        // In InGame mode, egui only renders non-interactive overlays
        // (mission banner). The custom sidebar handles its own hit-testing.
        // Ignore egui's `consumed` flag in-game to avoid stale UI state
        // from the Loading screen blocking mouse/keyboard input.
        // Exception: when paused, egui renders the interactive pause menu.
        let egui_consumed: bool =
            egui_response.consumed && (state.screen != GameScreen::InGame || state.paused);

        match event {
            WindowEvent::CloseRequested => {
                log::info!("Close requested");
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                state.gpu.resize(size.width, size.height);
                state.depth_view = state.gpu.create_depth_texture();
                // Recompute UI scale when window size changes.
                let new_scale = auto_detect_ui_scale(size.width, size.height);
                if (new_scale - state.ui_scale).abs() > f32::EPSILON {
                    log::info!("UI scale changed: {}x -> {}x", state.ui_scale, new_scale);
                    state.sidebar_layout_spec =
                        state.sidebar_layout_spec_base.with_scale(new_scale);
                    state.ui_scale = new_scale;
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    // ESC always reaches the handler when in-game (even when paused)
                    // so the player can toggle pause regardless of egui focus.
                    let is_escape: bool =
                        code == KeyCode::Escape && event.state.is_pressed() && !event.repeat;
                    let in_game: bool = state.screen == GameScreen::InGame;

                    if in_game && (is_escape || !egui_consumed) {
                        if event.state.is_pressed() && !event.repeat {
                            app_input::handle_hotkey_pressed(state, code);
                        }
                    }
                    // Track held keys only when not paused.
                    if in_game && !egui_consumed {
                        if event.state.is_pressed() {
                            state.keys_held.insert(code);
                        } else {
                            state.keys_held.remove(&code);
                        }
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                // When upscaling, remap window coordinates to render-target coordinates.
                let (sx, sy) = if state.upscale_pass.is_some() {
                    (
                        state.render_width() as f32 / state.gpu.config.width as f32,
                        state.render_height() as f32 / state.gpu.config.height as f32,
                    )
                } else {
                    (1.0, 1.0)
                };
                state.cursor_x = position.x as f32 * sx;
                state.cursor_y = position.y as f32 * sy;
                // Keep OS cursor hidden whenever the software cursor is active.
                if state.software_cursor.is_some() {
                    state.window.set_cursor_visible(false);
                }
                if !egui_consumed
                    && (state.screen == GameScreen::InGame || state.screen == GameScreen::SpawnPick)
                {
                    app_input::handle_cursor_moved_in_game(state);
                }
            }
            WindowEvent::MouseInput {
                state: btn_state,
                button,
                ..
            } => {
                // Keep OS cursor hidden on click events (not just CursorMoved).
                // Without this, rapid clicks without mouse movement let the OS
                // cursor flash visible between WM_SETCURSOR and the next render.
                if state.software_cursor.is_some() {
                    state.window.set_cursor_visible(false);
                }
                if !egui_consumed && state.screen == GameScreen::SpawnPick {
                    if button == MouseButton::Left && btn_state.is_pressed() {
                        crate::app_spawn_pick::handle_spawn_pick_click(state);
                    }
                } else if !egui_consumed && state.screen == GameScreen::InGame {
                    app_input::handle_mouse_input(state, button, btn_state);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if !egui_consumed
                    && (state.screen == GameScreen::InGame || state.screen == GameScreen::SpawnPick)
                {
                    let lines = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y,
                        MouseScrollDelta::PixelDelta(pos) => (pos.y as f32 / 30.0).clamp(-3.0, 3.0),
                    };
                    // Scroll sidebar when cursor is over the sidebar panel,
                    // otherwise zoom the game viewport (if enabled in settings).
                    if !app_input::try_sidebar_scroll(state, lines)
                        && state.skirmish_settings.zoom_enabled
                    {
                        crate::app_camera::apply_zoom(state, lines);
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(err) = Self::render_frame(state, event_loop) {
                    log::error!("Render: {:#}", err);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(state) = &self.state {
            state.window.request_redraw();
        }
    }
}

impl App {
    /// Create window, GPU context, and egui integration. Does NOT load a map —
    /// starts in MainMenu state. Map loading is deferred to when the user
    /// clicks "Quick Play".
    fn initialize(event_loop: &ActiveEventLoop) -> Result<AppState> {
        let window_attrs: WindowAttributes = WindowAttributes::default()
            .with_title("RA2 Engine")
            .with_inner_size(winit::dpi::LogicalSize::new(1024u32, 768u32));
        let window: Arc<Window> = Arc::new(event_loop.create_window(window_attrs)?);
        let gpu: GpuContext = GpuContext::new(window.clone())?;
        let egui: EguiIntegration = EguiIntegration::new(&gpu, &window);
        let batch_renderer: BatchRenderer = BatchRenderer::new(&gpu);
        let sidebar_text = SidebarTextRenderer::new(&gpu, &batch_renderer);
        let depth_view: wgpu::TextureView = gpu.create_depth_texture();
        let game_config = GameConfig::load().ok();
        let input_delay_ticks: u64 = game_config
            .as_ref()
            .map(|cfg| cfg.gameplay.input_delay_ticks.max(1) as u64)
            .unwrap_or(2);
        let upscale_pass = game_config
            .as_ref()
            .filter(|cfg| cfg.graphics.upscale)
            .map(|cfg| {
                let rw = cfg.graphics.render_width();
                let rh = cfg.graphics.render_height();
                log::info!(
                    "Upscale pass enabled: render at {}x{}, upscale to window",
                    rw, rh,
                );
                crate::render::upscale_pass::UpscalePass::new(&gpu, rw, rh)
            });
        let base_sidebar_layout_spec = SidebarChromeLayoutSpec::load_optional_default()
            .map(|spec| spec.unwrap_or_else(SidebarChromeLayoutSpec::stock))
            .unwrap_or_else(|err| {
                log::warn!("Could not load sidebar layout override: {:#}", err);
                SidebarChromeLayoutSpec::stock()
            });
        // Auto-detect integer UI scale from window size.
        let screen_w = window.inner_size().width;
        let screen_h = window.inner_size().height;
        let ui_scale: f32 = auto_detect_ui_scale(screen_w, screen_h);
        log::info!("UI scale: {}x ({}x{})", ui_scale, screen_w, screen_h);
        let sidebar_layout_spec = base_sidebar_layout_spec.with_scale(ui_scale);
        let vxl_compute = crate::render::vxl_compute::VxlComputeRenderer::new(&gpu.device);

        Ok(AppState {
            window,
            gpu,
            batch_renderer,
            instance_pool: crate::render::batch::InstanceBufferPool::new(),
            tile_atlas: None,
            map_basic: BasicSection::default(),
            terrain_grid: None,
            resolved_terrain: None,
            simulation: None,
            unit_atlas: None,
            vxl_compute: Some(vxl_compute),
            sprite_atlas: None,
            overlay_atlas: None,
            bridge_atlas: None,
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
            overlay_registry: None,
            depth_view,
            upscale_pass,
            camera_x: 0.0,
            camera_y: 0.0,
            zoom_level: 1.0,
            zoom_target: 1.0,
            zoom_anchor_world: [0.0, 0.0],
            zoom_anchor_screen: [0.0, 0.0],
            cursor_x: 0.0,
            cursor_y: 0.0,
            keys_held: HashSet::new(),
            egui,
            screen: if std::env::var("RA2_QUICKPLAY").is_ok() {
                GameScreen::Loading {
                    map_name: "auto".to_string(),
                }
            } else {
                GameScreen::default()
            },
            available_maps: app_list_maps::list_available_maps().unwrap_or_else(|err| {
                log::warn!("Could not list maps for menu: {:#}", err);
                Vec::new()
            }),
            skirmish_settings: SkirmishSettings::default(),
            minimap: None,
            minimap_dragging: false,
            middle_mouse_panning: false,
            middle_mouse_anchor_x: 0.0,
            middle_mouse_anchor_y: 0.0,
            radar_anim: None,
            power_bar_anim: crate::sidebar::PowerBarAnimState::new(),
            radar_content_insets: None,
            has_radar: false,
            selection_overlay: None,
            shroud_buffer: None,
            sidebar_cameo_atlas: None,
            sidebar_chrome: None,
            sidebar_text,
            software_cursor: None,
            selection_state: SelectionState::new(),
            path_grid: None,
            path_grid_base: None,
            animation_sequences: BTreeMap::new(),
            rules: None,
            art_registry: None,
            infantry_sequences: HashMap::new(),
            csf: None,
            house_color_map: HashMap::new(),
            house_roster: HouseRoster::default(),
            height_map: BTreeMap::new(),
            bridge_height_map: BTreeMap::new(),
            lighting_grid: HashMap::new(),
            theater_name: "TEMPERATE".to_string(),
            theater_ext: "tem".to_string(),
            last_update_time: Instant::now(),
            sim_accumulator_ms: 0,
            pending_commands: Vec::new(),
            target_lines: crate::app_target_lines::TargetLineState::default(),
            input_delay_ticks,
            replay_log: None,
            queued_order_mode: app_render::OrderMode::Move,
            control_groups: vec![Vec::new(); 10],
            local_owner_override: None,
            sandbox_full_visibility: false,
            disable_ai: true,
            spawn_pick_pending: false,
            armed_building_placement: None,
            building_placement_preview: None,
            active_sidebar_tab: SidebarTab::default_active_tab(),
            sidebar_layout_spec,
            sidebar_layout_spec_base: base_sidebar_layout_spec,
            ui_scale,
            sidebar_scroll_rows: 0,
            mission_announcement: None,
            mission_announcement_deadline: None,
            asset_manager: None,
            music_player: MusicPlayer::new(),
            sfx_player: SfxPlayer::new(),
            sound_registry: SoundRegistry::default(),
            audio_indices: Vec::new(),
            eva_registry: crate::rules::sound_ini::EvaRegistry::default(),
            sound_events: SoundEventQueue::new(),
            pending_fire_effects: Vec::new(),
            garrison_muzzle_flashes: Vec::new(),
            paused: false,
            debug_frame_step_requested: false,
            sim_speed_tps: app_render::SIM_TICK_HZ,
            startup_splash_until: None,
            idle_anim_elapsed_ms: 0,
            debug_show_pathgrid: false,
            debug_terrain_cost_speed_type: None,
            debug_show_cell_grid: false,
            debug_show_heightmap: false,
            show_hotkey_help: false,
            debug_unit_inspector: false,
            displayed_credits: HashMap::new(),
            cached_overlay_instances: Vec::new(),
            cached_unit_instances: Vec::new(),
        })
    }

    /// Dispatch rendering based on current GameScreen state.
    fn render_frame(state: &mut AppState, event_loop: &ActiveEventLoop) -> Result<()> {
        if let Some(until) = state.startup_splash_until {
            if Instant::now() < until {
                let output: wgpu::SurfaceTexture = state
                    .gpu
                    .surface
                    .get_current_texture()
                    .map_err(|e| anyhow::anyhow!("Surface texture: {}", e))?;
                let view: wgpu::TextureView = output.texture.create_view(&Default::default());
                let mut encoder: wgpu::CommandEncoder =
                    state
                        .gpu
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Startup Splash Frame"),
                        });
                app_transitions::clear_screen(&mut encoder, &view);
                state.egui.begin_frame(&state.window);
                main_menu::draw_loading_screen(&state.egui.ctx, "Initializing client");
                state.egui.end_frame_and_render(
                    &state.gpu,
                    &mut encoder,
                    &view,
                    &state.window,
                    state.software_cursor.is_some(),
                );
                state.gpu.queue.submit(std::iter::once(encoder.finish()));
                output.present();
                return Ok(());
            }
            state.startup_splash_until = None;
        }

        if matches!(state.screen, GameScreen::InGame) {
            let now = Instant::now();
            let elapsed_ms = app_sim_tick::update_elapsed_ms(state, now);
            app_sim_tick::advance_in_game_runtime(state, elapsed_ms);
        }

        let output: wgpu::SurfaceTexture = state
            .gpu
            .surface
            .get_current_texture()
            .map_err(|e| anyhow::anyhow!("Surface texture: {}", e))?;
        let view: wgpu::TextureView = output.texture.create_view(&Default::default());
        let mut encoder: wgpu::CommandEncoder =
            state
                .gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Frame"),
                });

        match &state.screen {
            GameScreen::MainMenu => {
                app_transitions::clear_screen(&mut encoder, &view);
                state.egui.begin_frame(&state.window);
                let action: MenuAction = main_menu::draw_main_menu_with_maps(
                    &state.egui.ctx,
                    &state.available_maps,
                    &mut state.skirmish_settings,
                );
                state.egui.end_frame_and_render(
                    &state.gpu,
                    &mut encoder,
                    &view,
                    &state.window,
                    state.software_cursor.is_some(),
                );

                // Handle menu action after rendering so the frame is visible.
                match action {
                    MenuAction::StartSelected => {
                        let map_name: String = state
                            .available_maps
                            .get(state.skirmish_settings.selected_map_idx)
                            .map(|m| m.file_name.clone())
                            .unwrap_or_else(|| "auto".to_string());
                        state.screen = GameScreen::Loading { map_name };
                        state.zoom_level = 1.0;
                        state.zoom_target = 1.0;
                    }
                    MenuAction::Exit => {
                        event_loop.exit();
                    }
                    MenuAction::None => {}
                }
            }
            GameScreen::Loading { map_name } => {
                let map_name_display: String = map_name.clone();
                app_transitions::clear_screen(&mut encoder, &view);
                state.egui.begin_frame(&state.window);
                main_menu::draw_loading_screen(&state.egui.ctx, &map_name_display);
                state.egui.end_frame_and_render(
                    &state.gpu,
                    &mut encoder,
                    &view,
                    &state.window,
                    state.software_cursor.is_some(),
                );
            }
            GameScreen::InGame => {
                let sidebar_view = if state.upscale_pass.is_some() {
                    // Render game to intermediate texture, then upscale to swapchain.
                    let up = state.upscale_pass.as_ref().unwrap();
                    let game_view = up.color_view().clone();
                    let game_depth = up.depth_view().clone();
                    let saved_depth = std::mem::replace(&mut state.depth_view, game_depth);
                    let result = app_render::render_game(state, &mut encoder, &game_view);
                    state.depth_view = saved_depth;
                    let sv = result?;
                    state.upscale_pass.as_ref().unwrap().draw(&mut encoder, &view);
                    sv
                } else {
                    app_render::render_game(state, &mut encoder, &view)?
                };
                // Always run egui in-game for sidebar text overlay (Ready labels, credits).
                state.egui.begin_frame(&state.window);
                if let Some(ref sv) = sidebar_view {
                    crate::app_sidebar_text::draw_sidebar_text_overlay(
                        &state.egui.ctx,
                        sv,
                        state.ui_scale,
                    );
                }
                if let Some(text) = state.mission_announcement.as_deref() {
                    crate::ui::mission_status::draw_mission_banner(&state.egui.ctx, text);
                }
                // Debug panels use a light/.NET theme — push light visuals
                // before rendering, then restore the original after.
                let any_debug_panel = state.debug_show_pathgrid
                    || state.debug_unit_inspector
                    || state.show_hotkey_help;
                let prev_visuals = if any_debug_panel {
                    Some(crate::app_debug_panel::push_debug_light_visuals(
                        &state.egui.ctx,
                    ))
                } else {
                    None
                };
                if state.debug_show_pathgrid {
                    crate::app_debug_panel::draw_debug_panel(&state.egui.ctx, state);
                }
                crate::app_debug_panel::draw_event_history_panel(&state.egui.ctx, state);
                if state.show_hotkey_help {
                    crate::app_debug_panel::draw_hotkey_help(&state.egui.ctx);
                }
                if let Some(prev) = prev_visuals {
                    crate::app_debug_panel::pop_debug_light_visuals(&state.egui.ctx, prev);
                }
                if state.paused {
                    Self::handle_pause_menu(state);
                }
                state.egui.end_frame_and_render(
                    &state.gpu,
                    &mut encoder,
                    &view,
                    &state.window,
                    state.software_cursor.is_some(),
                );
            }
            GameScreen::MissionResult { title, detail } => {
                app_transitions::clear_screen(&mut encoder, &view);
                state.egui.begin_frame(&state.window);
                if crate::ui::mission_status::draw_mission_result_screen(
                    &state.egui.ctx,
                    title,
                    detail,
                ) {
                    state.screen = GameScreen::MainMenu;
                    state.zoom_level = 1.0;
                    state.zoom_target = 1.0;
                }
                state.egui.end_frame_and_render(
                    &state.gpu,
                    &mut encoder,
                    &view,
                    &state.window,
                    state.software_cursor.is_some(),
                );
            }
            GameScreen::SpawnPick => {
                crate::app_spawn_pick::render_spawn_pick(state, &mut encoder, &view)?;
                state.egui.begin_frame(&state.window);
                crate::app_spawn_pick::draw_spawn_pick_overlay(&state.egui.ctx.clone(), state);
                state.egui.end_frame_and_render(
                    &state.gpu,
                    &mut encoder,
                    &view,
                    &state.window,
                    state.software_cursor.is_some(),
                );
            }
        }

        state.gpu.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        // Deferred loading: after presenting the Loading screen frame,
        // do the actual (synchronous) map load. The next frame will be InGame.
        if matches!(state.screen, GameScreen::Loading { .. }) {
            app_transitions::transition_to_in_game(state);
        }

        Ok(())
    }

    /// Draw the pause menu and handle its actions.
    fn handle_pause_menu(state: &mut AppState) {
        use crate::ui::pause_menu::{self, PauseMenuAction, PauseMenuInfo};

        let info = PauseMenuInfo {
            current_track: state.music_player.as_ref().and_then(|p| p.current_track()),
            volume: state.music_player.as_ref().map_or(0.5, |p| p.volume()),
            speed_tps: state.sim_speed_tps,
        };

        let action: PauseMenuAction = pause_menu::draw_pause_menu(&state.egui.ctx, &info);

        match action {
            PauseMenuAction::Resume => {
                state.paused = false;
                // Reset timing to prevent sim accumulator spike from pause duration.
                state.last_update_time = Instant::now();
                state.sim_accumulator_ms = 0;
                log::info!("Game resumed");
            }
            PauseMenuAction::ReturnToMenu => {
                state.paused = false;
                if let Some(ref mut player) = state.music_player {
                    player.stop();
                }
                state.screen = GameScreen::MainMenu;
                state.zoom_level = 1.0;
                state.zoom_target = 1.0;
                state.window.set_cursor_visible(true);
                log::info!("Returned to main menu");
            }
            PauseMenuAction::NextTrack => {
                if let (Some(player), Some(assets)) =
                    (&mut state.music_player, &state.asset_manager)
                {
                    if let Some(name) = player.play_next(assets) {
                        log::info!("Switched to track: {}", name);
                    }
                }
            }
            PauseMenuAction::SetMusicVolume(vol) => {
                if let Some(ref mut player) = state.music_player {
                    player.set_volume(vol);
                }
            }
            PauseMenuAction::SetGameSpeed(tps) => {
                state.sim_speed_tps = tps;
                log::info!("Game speed set to {} tps", tps);
            }
            PauseMenuAction::None => {}
        }
    }
}

/// Auto-detect UI scale from screen dimensions.
/// Returns 0.5, 1.0, or 1.5 to keep pixel art crisp at all resolutions.
/// Requires both enough height AND enough width so the sidebar doesn't
/// eat the entire screen at small window sizes.
fn auto_detect_ui_scale(screen_width: u32, screen_height: u32) -> f32 {
    // 1.5x: needs at least 2560×1441 (typical 1440p+ / 4K).
    if screen_width >= 2560 && screen_height > 1440 {
        return 1.5;
    }
    // 1.5x: needs at least 1600×900 so the sidebar leaves enough map view.
    if screen_width >= 1600 && screen_height >= 900 {
        return 1.5;
    }
    0.5
}
