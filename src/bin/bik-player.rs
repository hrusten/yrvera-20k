//! Standalone Bink 1 video player for RA2/YR cutscenes.
//!
//! Usage:
//!   cargo run --bin bik-player <path-or-asset-name>
//!
//! If the argument is a filesystem path it's loaded directly; otherwise it's
//! looked up via AssetManager (MOVIES*.MIX + movmd03.mix).

mod bik_player_audio;
mod bik_player_playback;
mod bik_player_ui;

use eframe::egui;
use std::sync::Arc;
use vera20k::assets::asset_manager::AssetManager;
use vera20k::assets::bink_audio::BinkAudioDecoder;
use vera20k::assets::bink_decode::BinkDecoder;
use vera20k::assets::bink_file::BinkFile;
use vera20k::assets::xcc_database::XccDatabase;
use vera20k::util::config::GameConfig;

fn main() -> Result<(), eframe::Error> {
    env_logger::init();
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([960.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        "vera20k bik-player",
        native_options,
        Box::new(|cc| Ok(Box::new(BikPlayerApp::new(cc)))),
    )
}

pub struct BikPlayerApp {
    pub asset_manager: Option<AssetManager>,
    pub source_name: String,
    pub file: Option<BinkFile>,
    pub decoder: Option<BinkDecoder>,
    pub audio_decoder: Option<BinkAudioDecoder>,
    pub audio_sink: Option<bik_player_audio::BinkAudioSink>,
    pub audio_volume: f32,
    pub current_frame: usize,
    pub status: String,
    /// Persistent input buffer for the "MIX asset" text field. Must live on
    /// the struct — declaring it inside the egui closure would reset every frame.
    pub asset_name_input: String,
    /// `.bik` asset names resolvable via AssetManager, sorted. Populated at
    /// startup by intersecting the XCC filename database with loaded archives.
    pub available_assets: Vec<String>,
    pub playback: bik_player_playback::Playback,
    pub texture: Option<egui::TextureHandle>,
}

impl BikPlayerApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        // AssetManager is optional — the player works without one (filesystem mode).
        let asset_manager = GameConfig::load()
            .ok()
            .and_then(|cfg| AssetManager::new(&cfg.paths.ra2_dir).ok());

        let available_assets = asset_manager
            .as_ref()
            .map(discover_bik_assets)
            .unwrap_or_default();
        let status = match (&asset_manager, available_assets.len()) {
            (None, _) => "No AssetManager (config.toml missing?). Use Open .bik… to load from disk.".to_string(),
            (Some(_), 0) => "No .bik assets found in loaded archives. Use Open .bik… to load from disk.".to_string(),
            (Some(_), n) => format!("{} .bik assets available. Pick one or Open .bik… from disk.", n),
        };

        Self {
            asset_manager,
            source_name: String::new(),
            file: None,
            decoder: None,
            audio_decoder: None,
            audio_sink: None,
            audio_volume: 0.7,
            current_frame: 0,
            status,
            asset_name_input: String::new(),
            available_assets,
            playback: bik_player_playback::Playback::default(),
            texture: None,
        }
    }

    /// Load a source by filesystem path.
    pub fn load_path(&mut self, path: &std::path::Path) {
        match std::fs::read(path) {
            Ok(bytes) => self.load_bytes(Arc::<[u8]>::from(bytes), path.display().to_string()),
            Err(e) => self.status = format!("read error: {}", e),
        }
    }

    /// Load a source by MIX asset name.
    pub fn load_asset(&mut self, name: &str) {
        let Some(mgr) = self.asset_manager.as_ref() else {
            self.status = "No AssetManager available (config.toml missing?)".to_string();
            return;
        };
        let Some(bytes) = mgr.get_ref(name) else {
            self.status = format!("asset not found: {}", name);
            return;
        };
        self.load_bytes(Arc::<[u8]>::from(bytes), name.to_string());
    }

    fn load_bytes(&mut self, bytes: Arc<[u8]>, name: String) {
        // Clear old state up front so a mid-load failure never leaves a stale
        // decoder paired with no file (or vice versa).
        self.file = None;
        self.decoder = None;
        self.audio_decoder = None;
        self.audio_sink = None;
        self.current_frame = 0;
        self.source_name = name;

        match BinkFile::parse(bytes) {
            Ok(file) => match BinkDecoder::new(&file.header) {
                Ok(d) => {
                    self.status = format!(
                        "loaded {}: {}x{} @ {:.2} fps, {} frames",
                        self.source_name,
                        file.header.width,
                        file.header.height,
                        file.header.fps(),
                        file.header.num_frames
                    );
                    if let Some(track) = file.header.audio_tracks.first().copied() {
                        match BinkAudioDecoder::new(track) {
                            Ok(ad) => {
                                let sr = ad.sample_rate();
                                let ch = ad.channels();
                                self.audio_sink = bik_player_audio::BinkAudioSink::new(sr, ch);
                                if let Some(s) = &self.audio_sink {
                                    s.set_volume(self.audio_volume);
                                }
                                self.audio_decoder = Some(ad);
                            }
                            Err(e) => log::warn!("bik-player: audio init failed: {}", e),
                        }
                        if file.header.audio_tracks.len() > 1 {
                            log::warn!(
                                "bik-player: {} audio tracks; using track 0 only",
                                file.header.audio_tracks.len(),
                            );
                        }
                    }
                    self.file = Some(file);
                    self.decoder = Some(d);
                }
                Err(e) => self.status = format!("decoder error: {}", e),
            },
            Err(e) => self.status = format!("parse error: {}", e),
        }
    }
}

/// Enumerate `.bik` filenames that AssetManager can resolve. Uses the XCC
/// global mix database as the candidate name source; falls back to an empty
/// list if XCC isn't installed.
fn discover_bik_assets(mgr: &AssetManager) -> Vec<String> {
    let Ok(xcc) = XccDatabase::load_from_disk() else {
        log::warn!("XCC database not available — no asset picker population");
        return Vec::new();
    };
    let mut names: Vec<String> = xcc
        .by_extension(".bik")
        .into_iter()
        .map(|e| e.filename.clone())
        .filter(|n| mgr.get_ref(n).is_some())
        .collect();
    names.sort_unstable_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()));
    names.dedup();
    log::info!("bik-player: {} .bik assets discovered in loaded archives", names.len());
    names
}

impl eframe::App for BikPlayerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        bik_player_ui::draw_top_panel(self, ctx);
        bik_player_ui::draw_timeline(self, ctx);

        if let (Some(file), Some(decoder)) = (self.file.as_ref(), self.decoder.as_mut()) {
            self.playback.step(
                file,
                decoder,
                self.audio_decoder.as_mut(),
                self.audio_sink.as_ref(),
                &mut self.current_frame,
                &mut self.status,
            );
        }
        ctx.request_repaint();

        if let Some(decoder) = &self.decoder {
            let rgba = bik_player_playback::frame_to_rgba(&decoder.cur);
            let img = egui::ColorImage::from_rgba_unmultiplied(
                [decoder.width() as usize, decoder.height() as usize],
                &rgba,
            );
            let handle = match self.texture.as_mut() {
                Some(h) => {
                    h.set(img, egui::TextureOptions::LINEAR);
                    h.clone()
                }
                None => ctx.load_texture("bink-frame", img, egui::TextureOptions::LINEAR),
            };
            self.texture = Some(handle);
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(tex) = &self.texture {
                let size = tex.size_vec2();
                ui.image((tex.id(), size));
            }
            ui.horizontal(|ui| {
                if let Some(f) = &self.file {
                    ui.label(format!(
                        "Frame {} / {}",
                        self.current_frame,
                        f.frame_index.len()
                    ));
                } else {
                    ui.label("No file loaded.");
                }
                if ui
                    .button(if self.playback.playing { "Pause" } else { "Play" })
                    .clicked()
                {
                    self.playback.playing = !self.playback.playing;
                }
            });
        });
    }
}
