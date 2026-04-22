//! Standalone Bink 1 video player for RA2/YR cutscenes.
//!
//! Usage:
//!   cargo run --bin bik-player <path-or-asset-name>
//!
//! If the argument is a filesystem path it's loaded directly; otherwise it's
//! looked up via AssetManager (MOVIES*.MIX + movmd03.mix).

mod bik_player_playback;
mod bik_player_ui;

use eframe::egui;
use std::sync::Arc;
use vera20k::assets::asset_manager::AssetManager;
use vera20k::assets::bink_decode::BinkDecoder;
use vera20k::assets::bink_file::BinkFile;
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
    pub current_frame: usize,
    pub status: String,
    /// Persistent input buffer for the "MIX asset" text field. Must live on
    /// the struct — declaring it inside the egui closure would reset every frame.
    pub asset_name_input: String,
    pub playback: bik_player_playback::Playback,
}

impl BikPlayerApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        // AssetManager is optional — the player works without one (filesystem mode).
        let asset_manager = GameConfig::load()
            .ok()
            .and_then(|cfg| AssetManager::new(&cfg.paths.ra2_dir).ok());
        Self {
            asset_manager,
            source_name: String::new(),
            file: None,
            decoder: None,
            current_frame: 0,
            status: String::from("Load a .bik file or a MIX asset name."),
            asset_name_input: String::new(),
            playback: bik_player_playback::Playback::default(),
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
                    self.file = Some(file);
                    self.decoder = Some(d);
                }
                Err(e) => self.status = format!("decoder error: {}", e),
            },
            Err(e) => self.status = format!("parse error: {}", e),
        }
    }
}

impl eframe::App for BikPlayerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        bik_player_ui::draw_top_panel(self, ctx);

        if let (Some(file), Some(decoder)) = (self.file.as_ref(), self.decoder.as_mut()) {
            self.playback.step(
                file,
                decoder,
                &mut self.current_frame,
                &mut self.status,
            );
        }
        ctx.request_repaint();

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(f) = &self.file {
                ui.label(format!(
                    "Frame {} / {}",
                    self.current_frame,
                    f.frame_index.len()
                ));
            } else {
                ui.label("No file loaded.");
            }
        });
    }
}
