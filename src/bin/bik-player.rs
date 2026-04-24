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
use std::collections::HashMap;
use std::sync::Arc;
use vera20k::assets::asset_manager::AssetManager;
use vera20k::assets::bink_audio::BinkAudioDecoder;
use vera20k::assets::bink_decode::BinkDecoder;
use vera20k::assets::bink_file::BinkFile;
use vera20k::assets::mix_hash::{mix_hash, westwood_hash};
use vera20k::assets::xcc_database::XccDatabase;
use vera20k::util::config::GameConfig;

/// One physical `.bik` entry in a loaded MIX archive.
///
/// Carries `(archive_name, entry_id)` as the fetch coordinate so
/// `AssetManager::archive_entry_data` can return that specific copy,
/// bypassing first-match-wins shadowing between archives that share
/// the same filename hash.
#[derive(Clone)]
pub struct PickerEntry {
    pub archive_name: String,
    pub entry_id: i32,
    pub display: String,
}

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
    /// Every physical `.bik` entry in every loaded archive, sorted by
    /// `(archive_name, display)`. Discovered at startup via magic-byte scan.
    pub available_entries: Vec<PickerEntry>,
    pub playback: bik_player_playback::Playback,
    pub texture: Option<egui::TextureHandle>,
}

impl BikPlayerApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        // AssetManager is optional — the player works without one (filesystem mode).
        let asset_manager = GameConfig::load()
            .ok()
            .and_then(|cfg| AssetManager::new(&cfg.paths.ra2_dir).ok());

        let available_entries = asset_manager
            .as_ref()
            .map(discover_bik_entries)
            .unwrap_or_default();
        let status = match (&asset_manager, available_entries.len()) {
            (None, _) => "No AssetManager (config.toml missing?). Use Open .bik… to load from disk.".to_string(),
            (Some(_), 0) => "No .bik entries found in loaded archives. Use Open .bik… to load from disk.".to_string(),
            (Some(_), n) => format!("{} .bik entries across loaded archives. Pick one or Open .bik… from disk.", n),
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
            available_entries,
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

    /// Load a picker entry via its precise (archive, entry_id) coordinate.
    /// Bypasses `get_ref`'s first-match-wins rule so a shadowed copy in a
    /// later archive is still reachable.
    pub fn load_picker_entry(&mut self, entry: &PickerEntry) {
        let Some(mgr) = self.asset_manager.as_ref() else {
            self.status = "No AssetManager available (config.toml missing?)".to_string();
            return;
        };
        match mgr.archive_entry_data(&entry.archive_name, entry.entry_id) {
            Some(bytes) => self.load_bytes(Arc::<[u8]>::from(bytes), entry.display.clone()),
            None => self.status = format!("missing: {}", entry.display),
        }
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

/// Enumerate every physical `.bik` entry across all loaded MIX archives.
///
/// Walks each archive, magic-byte-sniffs every entry (`BIK` or `KB2` header),
/// and resolves filenames through a reverse XCC lookup keyed by both
/// `mix_hash` and `westwood_hash`. Unknown hashes get a synthetic
/// `0x{hash:08X}.bik` label. Returned entries are sorted by
/// `(archive_name, display)` lexicographic.
fn discover_bik_entries(mgr: &AssetManager) -> Vec<PickerEntry> {
    let reverse_xcc: HashMap<i32, String> = match XccDatabase::load_from_disk() {
        Ok(xcc) => {
            let mut map = HashMap::new();
            for entry in xcc.by_extension(".bik") {
                map.insert(mix_hash(&entry.filename), entry.filename.clone());
                map.insert(westwood_hash(&entry.filename), entry.filename.clone());
            }
            map
        }
        Err(e) => {
            log::warn!(
                "XCC database not available ({}); unknown-hash fallback used for all entries",
                e
            );
            HashMap::new()
        }
    };

    let mut entries: Vec<PickerEntry> = Vec::new();
    mgr.visit_archives(|archive_name, archive| {
        for mix_entry in archive.entries() {
            let Some(data) = archive.get_by_id(mix_entry.id) else {
                continue;
            };
            if data.len() < 3 {
                continue;
            }
            if &data[..3] != b"BIK" && &data[..3] != b"KB2" {
                continue;
            }
            let filename = reverse_xcc
                .get(&mix_entry.id)
                .cloned()
                .unwrap_or_else(|| format!("0x{:08X}.bik", mix_entry.id as u32));
            let display = format!("{} / {}", archive_name, filename);
            entries.push(PickerEntry {
                archive_name: archive_name.to_string(),
                entry_id: mix_entry.id,
                display,
            });
        }
    });
    entries.sort_by(|a, b| {
        a.archive_name
            .to_ascii_lowercase()
            .cmp(&b.archive_name.to_ascii_lowercase())
            .then_with(|| a.display.cmp(&b.display))
    });
    log::info!(
        "bik-player: {} .bik entries discovered across {} archives",
        entries.len(),
        mgr.loaded_archive_names().len()
    );
    entries
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
                    if let Some(sink) = self.audio_sink.as_ref() {
                        if self.playback.playing {
                            sink.resume();
                        } else {
                            sink.pause();
                        }
                    }
                }
            });
        });
    }
}
