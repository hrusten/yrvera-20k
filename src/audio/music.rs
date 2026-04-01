//! Music playback using rodio.
//!
//! Loads theme tracks from THEME.MIX / thememd.mix, decodes WAV/AUD payloads
//! to PCM, and plays them through rodio. Supports play, stop,
//! next track, and live volume control.
//!
//! ## Track resolution
//! Track names come from thememd.ini / theme.ini or the map's [Basic] Theme= field.
//! The INI `Sound=` value resolves to the actual payload stem, which is looked
//! up first as `{stem}.wav`, then as `{stem}.aud`.
//!
//! ## Dependency rules
//! - Part of audio/ — depends on assets/ (aud_file decoder, AssetManager)
//!   and rules/ini_parser for theme metadata.
//! - Does NOT depend on render/, ui/, sidebar/, sim/.

use std::collections::HashMap;
use std::num::NonZero;

use rodio::buffer::SamplesBuffer;
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player};

use crate::assets::asset_manager::AssetManager;
use crate::assets::aud_file;
use crate::audio::sfx::decode_wav;
use crate::rules::ini_parser::IniFile;

const FALLBACK_TRACKS: &[&str] = &[
    "Grinder", "Power", "Fortific", "InDeep", "Tension", "EagleHun", "Industro", "Jank",
    "200Meter", "BlowItUp", "Destroy", "Burn", "Motorize", "HM2", "Ra2-Opt", "RA2-Sco", "Drok",
    "Bully", "OptionX", "ScoreX", "BrainFre", "Deceiver", "PhatAtta", "Defend", "Tactics",
    "TranceLV",
];

/// Manages background music playback.
pub struct MusicPlayer {
    /// rodio mixer device sink — must be kept alive or all audio stops.
    _device: MixerDeviceSink,
    /// Player handle for the currently playing track, if any.
    current_player: Option<Player>,
    /// Name of the currently playing track.
    current_track: Option<String>,
    /// Playlist of track names to cycle through.
    playlist: Vec<String>,
    /// Theme alias -> actual sound stem, uppercase keys.
    aliases: HashMap<String, String>,
    /// Index into the playlist for the next track.
    playlist_index: usize,
    /// Music volume (0.0 to 1.0).
    volume: f64,
}

impl MusicPlayer {
    /// Create a new MusicPlayer. Returns None if audio output cannot be opened.
    pub fn new() -> Option<Self> {
        let device = DeviceSinkBuilder::open_default_sink()
            .map_err(|e| log::error!("Failed to initialize music audio: {}", e))
            .ok()?;

        Some(Self {
            _device: device,
            current_player: None,
            current_track: None,
            playlist: FALLBACK_TRACKS.iter().map(|s| s.to_string()).collect(),
            aliases: HashMap::new(),
            playlist_index: 0,
            volume: 0.5,
        })
    }

    /// Play a specific track by name. Loads from the AssetManager on demand.
    /// Returns true if the track was found and playback started.
    pub fn play_track(&mut self, track_name: &str, assets: &AssetManager) -> bool {
        self.ensure_theme_config(assets);
        self.stop();

        let resolved_name: String = self.resolve_track_name(track_name);
        let (samples, sample_rate) = match load_track(&resolved_name, assets) {
            Some(data) => data,
            None => {
                log::warn!(
                    "Music track not found: requested='{}', resolved='{}'",
                    track_name,
                    resolved_name
                );
                return false;
            }
        };

        let channels = match NonZero::new(2u16) {
            Some(c) => c,
            None => return false,
        };
        let rate = match NonZero::new(sample_rate) {
            Some(r) => r,
            None => return false,
        };

        let source = SamplesBuffer::new(channels, rate, samples);
        let player: Player = Player::connect_new(self._device.mixer());
        player.set_volume(self.volume as f32);
        player.append(source);
        log::info!(
            "Playing music track: requested='{}', resolved='{}'",
            track_name,
            resolved_name
        );
        self.current_player = Some(player);
        self.current_track = Some(resolved_name);
        true
    }

    /// Stop the currently playing track.
    pub fn stop(&mut self) {
        if let Some(player) = self.current_player.take() {
            player.stop();
        }
        self.current_track = None;
    }

    /// Play the next track in the playlist. Wraps around at the end.
    /// Returns the name of the track that started playing, or None if no track loaded.
    pub fn play_next(&mut self, assets: &AssetManager) -> Option<String> {
        self.ensure_theme_config(assets);

        if self.playlist.is_empty() {
            return None;
        }

        let len: usize = self.playlist.len();
        for attempt in 0..len {
            let idx: usize = (self.playlist_index + attempt) % len;
            let track_name: String = self.playlist[idx].clone();
            self.playlist_index = (idx + 1) % len;
            if self.play_track(&track_name, assets) {
                return Some(track_name);
            }
        }
        None
    }

    /// Check if the current track has finished and auto-advance to the next.
    /// Call this once per frame from the game loop.
    pub fn update(&mut self, assets: &AssetManager) {
        let finished: bool = match &self.current_player {
            Some(player) => player.empty(),
            None => self.current_track.is_some(),
        };

        if finished {
            self.current_player = None;
            self.current_track = None;
            let _ = self.play_next(assets);
        }
    }

    /// Set the music volume (0.0 = silent, 1.0 = full).
    /// Applies immediately to the currently playing track.
    pub fn set_volume(&mut self, volume: f64) {
        self.volume = volume.clamp(0.0, 1.0);
        if let Some(ref player) = self.current_player {
            player.set_volume(self.volume as f32);
        }
    }

    /// Get the current music volume.
    pub fn volume(&self) -> f64 {
        self.volume
    }

    /// Get the name of the currently playing track, if any.
    pub fn current_track(&self) -> Option<&str> {
        self.current_track.as_deref()
    }

    /// Replace the playlist with custom track names.
    pub fn set_playlist(&mut self, tracks: Vec<String>) {
        self.playlist = tracks;
        self.playlist_index = 0;
    }

    fn resolve_track_name(&self, track_name: &str) -> String {
        self.aliases
            .get(&track_name.to_ascii_uppercase())
            .cloned()
            .unwrap_or_else(|| track_name.to_string())
    }

    fn ensure_theme_config(&mut self, assets: &AssetManager) {
        if !self.aliases.is_empty() {
            return;
        }

        let base = load_theme_ini(assets, "theme.ini");
        let md = load_theme_ini(assets, "thememd.ini");

        // Build aliases from both INIs (md values override base on conflict).
        if let Some(ref ini) = base {
            merge_theme_aliases(&mut self.aliases, ini);
        }
        if let Some(ref ini) = md {
            merge_theme_aliases(&mut self.aliases, ini);
        }

        // Merge playlists from both INIs — the original game plays RA2 and YR
        // tracks together. thememd.ini comments out RA2 entries, so we need
        // theme.ini to provide those tracks.
        let mut playlist = Vec::new();
        if let Some(ref ini) = base {
            playlist = playlist_from_theme_ini(ini, &self.aliases);
        }
        if let Some(ref ini) = md {
            for track in playlist_from_theme_ini(ini, &self.aliases) {
                if !playlist.iter().any(|t| t.eq_ignore_ascii_case(&track)) {
                    playlist.push(track);
                }
            }
        }

        if !playlist.is_empty() {
            self.playlist = playlist;
            self.playlist_index = 0;
        }
    }
}

/// Load a track and decode to interleaved f32 stereo samples.
/// Returns (samples, sample_rate) or None if not found / decode fails.
fn load_track(track_name: &str, assets: &AssetManager) -> Option<(Vec<f32>, u32)> {
    for filename in [format!("{}.wav", track_name), format!("{}.aud", track_name)] {
        let Some(data) = assets.get_ref(&filename) else {
            continue;
        };

        if data.len() >= 44 && &data[0..4] == b"RIFF" {
            if let Some(decoded) = decode_wav(data, &filename) {
                return Some((decoded.samples, decoded.sample_rate));
            }
        }

        let (header, samples) = match aud_file::decode_aud(data) {
            Some(decoded) => decoded,
            None => continue,
        };
        if samples.is_empty() {
            log::warn!("Track {} decoded to 0 samples", track_name);
            return None;
        }

        // Convert i16 PCM to interleaved f32 stereo.
        let stereo: Vec<f32> = if header.is_stereo() {
            samples.iter().map(|&s| s as f32 / 32768.0).collect()
        } else {
            samples
                .iter()
                .flat_map(|&s| {
                    let f = s as f32 / 32768.0;
                    [f, f]
                })
                .collect()
        };

        log::info!(
            "Decoded track {} from {}: {}Hz, {} channels, {} frames ({:.1}s)",
            track_name,
            filename,
            header.sample_rate,
            header.channels(),
            stereo.len() / 2,
            stereo.len() as f64 / 2.0 / header.sample_rate as f64,
        );

        return Some((stereo, header.sample_rate as u32));
    }

    None
}

fn load_theme_ini(assets: &AssetManager, name: &str) -> Option<IniFile> {
    let bytes = assets.get_ref(name)?;
    IniFile::from_bytes(bytes).ok()
}

fn merge_theme_aliases(into: &mut HashMap<String, String>, ini: &IniFile) {
    for section_name in ini.section_names() {
        let Some(section) = ini.section(section_name) else {
            continue;
        };
        let Some(sound) = section.get("Sound") else {
            continue;
        };
        if sound.is_empty() {
            continue;
        }

        let sound = sound.to_string();
        into.insert(section_name.to_ascii_uppercase(), sound.clone());
        into.insert(sound.to_ascii_uppercase(), sound);
    }
}

fn playlist_from_theme_ini(ini: &IniFile, aliases: &HashMap<String, String>) -> Vec<String> {
    let Some(themes) = ini.section("Themes") else {
        return Vec::new();
    };

    themes
        .get_values()
        .into_iter()
        .filter(|value| !value.is_empty())
        .filter_map(|theme_name| {
            let sound = aliases.get(&theme_name.to_ascii_uppercase())?;
            // Skip non-Normal tracks (INTRO, SCORE, LOADING, CREDITS) —
            // they're menu/loading music, not gameplay playlist entries.
            // Normal defaults to yes if absent.
            if let Some(section) = ini.section(theme_name) {
                if section
                    .get("Normal")
                    .is_some_and(|v| v.eq_ignore_ascii_case("no"))
                {
                    return None;
                }
            }
            Some(sound.clone())
        })
        .collect()
}
