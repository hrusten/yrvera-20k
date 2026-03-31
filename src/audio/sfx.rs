//! Sound effect (SFX) playback using rodio.
//!
//! Plays short one-shot sounds triggered by game events: weapon fire, unit
//! voice responses, building placement, death explosions. Uses the SoundRegistry
//! (from sound.ini) to resolve sound IDs to .wav/.aud filenames, then loads
//! and plays them through rodio.
//!
//! ## Design
//! - Fire-and-forget: each sound is played via a Player and tracked only to
//!   cap the max number of concurrent sounds (prevents audio overload).
//! - Random selection: when a sound entry has multiple .wav files, one is
//!   chosen at random for variety.
//! - Volume scaling: each sound's volume from sound.ini is applied on playback.
//!
//! ## Dependency rules
//! - Part of audio/ — depends on assets/ (AssetManager for .wav/.aud loading),
//!   rules/sound_ini (SoundRegistry for ID→filename mapping).
//! - Does NOT depend on render/, ui/, sidebar/, sim/.

use std::collections::VecDeque;
use std::num::NonZero;

use rodio::buffer::SamplesBuffer;
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player};

use crate::assets::asset_manager::AssetManager;
use crate::assets::aud_file;
use crate::rules::sound_ini::SoundRegistry;

/// Maximum concurrent SFX sounds — matches original engine's 16 DirectSound buffers.
const MAX_CONCURRENT_SFX: usize = 16;

/// Range multiplier — converts VocClass Range value (cells) to pixels.
/// In the original engine: `max_distance = Range * 0x3C` where 0x3C = 60.
const RANGE_MULTIPLIER: f32 = 60.0;

/// Default audible range in cells when sound has no explicit Range.
/// The original engine's [Defaults] section uses Range=10.
pub const DEFAULT_RANGE_CELLS: u16 = 10;

/// Minimum volume cutoff — sounds below this are culled entirely (not played).
/// Matches original engine behavior (approximately 5%).
const MIN_VOLUME_CUTOFF: f32 = 0.05;

/// Calculate spatial audio volume based on screen distance from viewport center.
///
/// Algorithm:
/// 1. Compute screen-space distance from viewport center
/// 2. Subtract half viewport (on-screen = full volume)
/// 3. Double Y for isometric compensation
/// 4. Linear falloff from 1.0 at viewport edge to 0.0 at max range
///
/// `range_cells` — audible range from sound.ini Range= key (default 10).
/// `min_volume_pct` — MinVolume= floor (0-100), volume never drops below this.
pub fn calc_spatial_volume(
    sound_screen_x: f32,
    sound_screen_y: f32,
    viewport_w: f32,
    viewport_h: f32,
    camera_x: f32,
    camera_y: f32,
    range_cells: u16,
    min_volume_pct: u8,
) -> f32 {
    let center_x = camera_x + viewport_w * 0.5;
    let center_y = camera_y + viewport_h * 0.5;

    // Absolute distance from screen center.
    let mut dx = (sound_screen_x - center_x).abs();
    let mut dy = (sound_screen_y - center_y).abs();

    // Subtract half viewport — sounds on screen have zero distance.
    dx = (dx - viewport_w * 0.5).max(0.0);
    dy = (dy - viewport_h * 0.5).max(0.0);

    // Double Y for isometric compensation (Y axis is visually compressed).
    dy *= 2.0;

    // Use the larger axis as effective distance.
    let dist = dx.max(dy);

    // Max audible distance = Range (cells) * 60 (pixels per cell equivalent).
    let max_range = range_cells.max(1) as f32 * RANGE_MULTIPLIER;
    if dist >= max_range {
        return 0.0;
    }

    let mut vol = (max_range - dist) / max_range;

    // Apply MinVolume floor — volume never drops below this.
    let min_vol = min_volume_pct as f32 / 100.0;
    if vol < min_vol {
        vol = min_vol;
    }

    if vol < MIN_VOLUME_CUTOFF { 0.0 } else { vol }
}

const FADE_MS: u32 = 3;

/// Decoded audio ready for rodio playback.
/// Holds interleaved f32 stereo samples, sample rate, and channel count.
pub(crate) struct DecodedAudio {
    /// Interleaved stereo f32 samples (L, R, L, R, ...).
    pub(crate) samples: Vec<f32>,
    pub(crate) sample_rate: u32,
    /// Always 2 (stereo) — we upmix mono sources for consistency.
    pub(crate) channels: u16,
}

/// Manages sound effect playback with separate SFX pool and voice slot.
///
/// Matches the original engine's architecture:
/// - 16-channel SFX pool for weapons, explosions, ambient
/// - 1 dedicated voice slot for unit responses (cuts off previous)
pub struct SfxPlayer {
    /// rodio mixer device sink — must be kept alive or all audio stops.
    _device: MixerDeviceSink,
    /// Active SFX players — oldest first. Capped at MAX_CONCURRENT_SFX.
    active: VecDeque<Player>,
    /// Dedicated voice player — unit responses cut off the previous voice.
    /// Separate from SFX pool so voices never compete with weapon sounds.
    voice_player: Option<Player>,
    /// SFX master volume (0.0 to 1.0).
    volume: f64,
    /// Simple counter used as seed for pseudo-random sound selection.
    /// Not cryptographic — just needs variety.
    random_counter: u32,
}

impl SfxPlayer {
    /// Create a new SfxPlayer. Returns None if audio output cannot be opened.
    pub fn new() -> Option<Self> {
        let device = DeviceSinkBuilder::open_default_sink()
            .map_err(|e| log::error!("Failed to initialize SFX audio: {}", e))
            .ok()?;

        Some(Self {
            _device: device,
            active: VecDeque::new(),
            voice_player: None,
            volume: 0.7,
            random_counter: 0,
        })
    }

    /// Play a sound by its sound.ini ID (e.g., "VGCannon1") or audio.bag name.
    ///
    /// Resolution order:
    /// 1. Look up `sound_id` in the SoundRegistry (sound.ini sections)
    /// 2. If found, pick a filename and load via audio bags then MIX assets
    /// 3. If NOT found in registry, try `sound_id` directly as an audio.bag name
    ///    (for EVA sounds and other bag-only entries)
    ///
    /// Returns true if the sound was successfully started.
    pub fn play_sound(
        &mut self,
        sound_id: &str,
        registry: &SoundRegistry,
        assets: &AssetManager,
        audio_indices: &[crate::assets::audio_bag::AudioIndex],
    ) -> bool {
        // Try SoundRegistry first (sound.ini-based sounds).
        if let Some(entry) = registry.get(sound_id) {
            if !entry.sounds.is_empty() {
                self.random_counter = self.random_counter.wrapping_add(1);
                let idx: usize = (self.random_counter as usize) % entry.sounds.len();
                let filename: &str = &entry.sounds[idx];

                if let Some(decoded) = load_sfx(filename, assets, audio_indices) {
                    let entry_volume: f64 = entry.volume as f64 / 100.0;
                    let final_volume: f32 = (entry_volume * self.volume) as f32;
                    return self.play_decoded(decoded, final_volume);
                }
            }
        }

        // Fallback: try sound_id directly as an audio.bag entry name
        // (EVA announcements and other sounds not in sound.ini).
        if let Some(decoded) = load_sfx(sound_id, assets, audio_indices) {
            let final_volume: f32 = self.volume as f32;
            return self.play_decoded(decoded, final_volume);
        }

        log::trace!("SFX: could not resolve '{}'", sound_id);
        false
    }

    /// Play a sound with an additional spatial volume multiplier.
    ///
    /// Used for positional sounds where volume is scaled by distance from camera.
    /// The spatial factor (0.0–1.0) is multiplied with the per-sound and master volumes.
    pub fn play_sound_with_volume(
        &mut self,
        sound_id: &str,
        spatial_volume: f32,
        registry: &SoundRegistry,
        assets: &AssetManager,
        audio_indices: &[crate::assets::audio_bag::AudioIndex],
    ) -> bool {
        if let Some(entry) = registry.get(sound_id) {
            if !entry.sounds.is_empty() {
                self.random_counter = self.random_counter.wrapping_add(1);
                let idx = (self.random_counter as usize) % entry.sounds.len();
                let filename = &entry.sounds[idx];

                if let Some(decoded) = load_sfx(filename, assets, audio_indices) {
                    let entry_volume = entry.volume as f64 / 100.0;
                    let final_volume = (entry_volume * self.volume) as f32 * spatial_volume;
                    return self.play_decoded(decoded, final_volume);
                }
            }
        }

        if let Some(decoded) = load_sfx(sound_id, assets, audio_indices) {
            let final_volume = self.volume as f32 * spatial_volume;
            return self.play_decoded(decoded, final_volume);
        }

        false
    }

    /// Play a sound as a unit voice response (VoiceSelect, VoiceMove, VoiceAttack).
    ///
    /// Uses a dedicated voice slot that cuts off the previous voice — unit responses
    /// don't stack, matching the original engine's behavior.
    pub fn play_voice_sound(
        &mut self,
        sound_id: &str,
        registry: &SoundRegistry,
        assets: &AssetManager,
        audio_indices: &[crate::assets::audio_bag::AudioIndex],
    ) -> bool {
        // Resolve through registry first, then fallback to bag name.
        let (decoded, entry_volume) = if let Some(entry) = registry.get(sound_id) {
            if entry.sounds.is_empty() {
                return false;
            }
            self.random_counter = self.random_counter.wrapping_add(1);
            let idx = (self.random_counter as usize) % entry.sounds.len();
            let filename = &entry.sounds[idx];
            match load_sfx(filename, assets, audio_indices) {
                Some(d) => (d, entry.volume as f64 / 100.0),
                None => return false,
            }
        } else {
            match load_sfx(sound_id, assets, audio_indices) {
                Some(d) => (d, 1.0),
                None => return false,
            }
        };

        let final_volume = (entry_volume * self.volume) as f32;
        self.play_voice(decoded, final_volume)
    }

    /// Play decoded audio on the dedicated voice slot, cutting off any current voice.
    fn play_voice(&mut self, mut decoded: DecodedAudio, final_volume: f32) -> bool {
        // Cut off previous voice immediately.
        if let Some(old) = self.voice_player.take() {
            old.stop();
        }

        apply_fade(
            &mut decoded.samples,
            decoded.sample_rate,
            decoded.channels,
            FADE_MS,
        );

        let channels = match NonZero::new(decoded.channels) {
            Some(c) => c,
            None => return false,
        };
        let sample_rate = match NonZero::new(decoded.sample_rate) {
            Some(r) => r,
            None => return false,
        };

        let source = SamplesBuffer::new(channels, sample_rate, decoded.samples);
        let player: Player = Player::connect_new(self._device.mixer());
        player.set_volume(final_volume);
        player.append(source);
        self.voice_player = Some(player);
        true
    }

    /// Play already-decoded audio on the SFX pool at the given volume.
    fn play_decoded(&mut self, mut decoded: DecodedAudio, final_volume: f32) -> bool {
        apply_fade(
            &mut decoded.samples,
            decoded.sample_rate,
            decoded.channels,
            FADE_MS,
        );

        // Evict finished sounds and enforce concurrency limit.
        self.cleanup_finished();
        if self.active.len() >= MAX_CONCURRENT_SFX {
            // Stop and evict oldest sound.
            if let Some(old) = self.active.pop_front() {
                old.stop();
            }
        }

        // NonZero is required by rodio 0.22 SamplesBuffer API.
        let channels = match NonZero::new(decoded.channels) {
            Some(c) => c,
            None => return false,
        };
        let sample_rate = match NonZero::new(decoded.sample_rate) {
            Some(r) => r,
            None => return false,
        };

        let source = SamplesBuffer::new(channels, sample_rate, decoded.samples);
        let player: Player = Player::connect_new(self._device.mixer());
        player.set_volume(final_volume);
        player.append(source);
        self.active.push_back(player);
        true
    }

    /// Remove handles for sounds that have finished playing.
    fn cleanup_finished(&mut self) {
        self.active.retain(|p: &Player| !p.empty());
    }

    /// Set the SFX master volume (0.0 = silent, 1.0 = full).
    pub fn set_volume(&mut self, volume: f64) {
        self.volume = volume.clamp(0.0, 1.0);
    }

    /// Get the current SFX master volume.
    pub fn volume(&self) -> f64 {
        self.volume
    }

    /// Number of currently active (playing) sound effects.
    pub fn active_count(&self) -> usize {
        self.active.len()
    }
}

/// Load a sound effect file and decode it to interleaved f32 stereo samples.
///
/// Resolution order:
/// 1. Try audio.bag indices (most voice/EVA sounds live here)
/// 2. Try MIX asset lookup by exact name
/// 3. Try MIX asset lookup with .wav extension appended
///
/// Supports .wav (raw PCM), .aud (IMA ADPCM), and audio.bag formats.
fn load_sfx(
    filename: &str,
    assets: &AssetManager,
    audio_indices: &[crate::assets::audio_bag::AudioIndex],
) -> Option<DecodedAudio> {
    // Try audio.bag indices first (voices, EVA announcements).
    for index in audio_indices {
        if let Some((entry, data)) = index.get(filename) {
            if let Some(bag_audio) = crate::assets::audio_bag::decode_bag_audio(entry, data) {
                // Convert i16 → f32 stereo.
                let stereo = upmix_i16_to_f32_stereo(&bag_audio.samples_i16, bag_audio.channels);
                return Some(DecodedAudio {
                    samples: stereo,
                    sample_rate: bag_audio.sample_rate,
                    channels: 2,
                });
            }
        }
    }

    // Try MIX asset lookup (exact name, then with .wav extension).
    let exact_name = format!("{}.wav", filename);
    let data: &[u8] = assets
        .get_ref(filename)
        .or_else(|| assets.get_ref(&exact_name))?;

    // Try WAV first (most SFX are .wav).
    if data.len() >= 44 && &data[0..4] == b"RIFF" {
        return decode_wav(data, filename);
    }

    // Fall back to .aud format.
    let (header, samples) = aud_file::decode_aud(data)?;
    if samples.is_empty() {
        return None;
    }

    // AUD is always mono — upmix to stereo for rodio.
    let stereo = upmix_i16_to_f32_stereo(&samples, 1);
    Some(DecodedAudio {
        samples: stereo,
        sample_rate: header.sample_rate as u32,
        channels: 2,
    })
}

/// Convert i16 PCM samples to interleaved f32 stereo.
/// Mono input is duplicated to both channels.
fn upmix_i16_to_f32_stereo(samples: &[i16], channels: u16) -> Vec<f32> {
    if channels >= 2 {
        // Already stereo (or more) — just convert to f32.
        samples.iter().map(|&s| s as f32 / 32768.0).collect()
    } else {
        // Mono → stereo: duplicate each sample.
        samples
            .iter()
            .flat_map(|&s| {
                let f = s as f32 / 32768.0;
                [f, f]
            })
            .collect()
    }
}

/// Apply a short linear fade-in and fade-out to interleaved samples.
///
/// Prevents audible click/pop artifacts from abrupt sample transitions.
/// The fade duration is typically 2-5ms — imperceptible but eliminates clicks.
fn apply_fade(samples: &mut [f32], sample_rate: u32, channels: u16, fade_ms: u32) {
    if samples.is_empty() || fade_ms == 0 || sample_rate == 0 {
        return;
    }
    let ch = channels.max(1) as usize;
    // Number of *frames* to fade (one frame = all channels).
    let fade_frames = (sample_rate as usize * fade_ms as usize / 1000).max(1);
    let total_frames = samples.len() / ch;
    // Don't fade if the sound is shorter than 2× fade duration.
    if total_frames < fade_frames * 2 {
        return;
    }

    // Fade in: ramp from 0.0 to 1.0 over the first fade_frames.
    for frame in 0..fade_frames {
        let scale = frame as f32 / fade_frames as f32;
        for c in 0..ch {
            samples[frame * ch + c] *= scale;
        }
    }

    // Fade out: ramp from 1.0 to 0.0 over the last fade_frames.
    let fade_out_start = total_frames - fade_frames;
    for frame in 0..fade_frames {
        let scale = 1.0 - (frame as f32 / fade_frames as f32);
        let idx = (fade_out_start + frame) * ch;
        for c in 0..ch {
            samples[idx + c] *= scale;
        }
    }
}

/// Decode a WAV file into interleaved f32 stereo samples.
///
/// Supports uncompressed PCM (format tag 1) with 8-bit or 16-bit samples,
/// and IMA ADPCM (format tag 0x11) used by RA2 EVA announcements.
/// Mono or stereo. This covers all RA2 sound effects and EVA voices.
pub(crate) fn decode_wav(data: &[u8], filename: &str) -> Option<DecodedAudio> {
    if data.len() < 44 {
        return None;
    }

    // Verify RIFF/WAVE header.
    if &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        log::trace!("WAV: invalid header for {}", filename);
        return None;
    }

    // Find "fmt " and "data" chunks.
    let mut offset: usize = 12;
    let mut fmt_found: bool = false;
    let mut channels: u16 = 1;
    let mut sample_rate: u32 = 22050;
    let mut bits_per_sample: u16 = 16;
    let mut format_tag: u16 = 1;
    let mut block_align: u16 = 0;

    while offset + 8 <= data.len() {
        let chunk_id: &[u8] = &data[offset..offset + 4];
        let chunk_size: u32 = u32::from_le_bytes([
            data[offset + 4],
            data[offset + 5],
            data[offset + 6],
            data[offset + 7],
        ]);

        if chunk_id == b"fmt " && offset + 8 + chunk_size as usize <= data.len() {
            let fmt: &[u8] = &data[offset + 8..];
            format_tag = u16::from_le_bytes([fmt[0], fmt[1]]);
            channels = u16::from_le_bytes([fmt[2], fmt[3]]);
            sample_rate = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]);
            block_align = u16::from_le_bytes([fmt[12], fmt[13]]);
            bits_per_sample = u16::from_le_bytes([fmt[14], fmt[15]]);
            fmt_found = true;
        }

        if chunk_id == b"data" && fmt_found {
            let pcm_start: usize = offset + 8;
            let pcm_end: usize = (pcm_start + chunk_size as usize).min(data.len());
            let pcm: &[u8] = &data[pcm_start..pcm_end];

            let samples: Vec<f32> = match format_tag {
                1 => decode_pcm(pcm, channels, bits_per_sample),
                0x11 => decode_ima_adpcm(pcm, channels, block_align),
                _ => {
                    log::trace!(
                        "WAV: unsupported format tag {} for {}",
                        format_tag,
                        filename
                    );
                    return None;
                }
            };
            if samples.is_empty() {
                return None;
            }

            // Always output stereo — upmix mono if needed.
            let stereo: Vec<f32> = if channels == 1 {
                samples.iter().flat_map(|&s| [s, s]).collect()
            } else {
                samples
            };

            return Some(DecodedAudio {
                samples: stereo,
                sample_rate,
                channels: 2,
            });
        }

        // Advance to next chunk (chunks are word-aligned).
        offset += 8 + ((chunk_size as usize + 1) & !1);
    }

    log::trace!("WAV: no data chunk found for {}", filename);
    None
}

/// IMA ADPCM step size table — standard IMA/DVI specification.
const IMA_STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

/// IMA ADPCM index adjustment table for each nibble value.
const IMA_INDEX_TABLE: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

/// Decode IMA ADPCM WAV data into interleaved f32 samples.
///
/// Each block starts with a 4-byte header per channel: i16 predictor + u8 step index + u8 pad.
/// Remaining bytes contain packed 4-bit nibbles decoded with the standard IMA algorithm.
fn decode_ima_adpcm(data: &[u8], channels: u16, block_align: u16) -> Vec<f32> {
    let ch = channels as usize;
    let block_size = block_align as usize;
    if block_size == 0 || ch == 0 {
        return Vec::new();
    }
    let header_size = 4 * ch;
    if block_size < header_size {
        return Vec::new();
    }

    // Samples per block: header gives 1 sample per channel, then 2 nibbles per byte.
    let data_bytes_per_block = block_size - header_size;
    let samples_per_block = 1 + data_bytes_per_block * 2 / ch;
    let num_blocks = (data.len() + block_size - 1) / block_size;
    let mut output: Vec<f32> = Vec::with_capacity(num_blocks * samples_per_block * ch);

    for block in data.chunks(block_size) {
        if block.len() < header_size {
            break;
        }

        // Read per-channel header: initial predictor and step index.
        let mut predictor = [0i32; 2];
        let mut step_index = [0i32; 2];
        for c in 0..ch {
            let base = c * 4;
            predictor[c] = i16::from_le_bytes([block[base], block[base + 1]]) as i32;
            step_index[c] = block[base + 2] as i32;
            step_index[c] = step_index[c].clamp(0, 88);
            // First sample from header.
            if ch == 1 {
                output.push(predictor[c] as f32 / 32768.0);
            }
        }
        // For stereo, interleave the initial samples.
        if ch == 2 {
            output.push(predictor[0] as f32 / 32768.0);
            output.push(predictor[1] as f32 / 32768.0);
        }

        // Decode nibbles from the data portion.
        let payload = &block[header_size..];
        if ch == 1 {
            // Mono: straightforward sequential nibbles.
            for &byte in payload {
                for shift in [0u8, 4] {
                    let nibble = ((byte >> shift) & 0x0F) as usize;
                    let step = IMA_STEP_TABLE[step_index[0] as usize];
                    let mut diff = step >> 3;
                    if nibble & 4 != 0 {
                        diff += step;
                    }
                    if nibble & 2 != 0 {
                        diff += step >> 1;
                    }
                    if nibble & 1 != 0 {
                        diff += step >> 2;
                    }
                    if nibble & 8 != 0 {
                        predictor[0] -= diff;
                    } else {
                        predictor[0] += diff;
                    }
                    predictor[0] = predictor[0].clamp(-32768, 32767);
                    step_index[0] += IMA_INDEX_TABLE[nibble];
                    step_index[0] = step_index[0].clamp(0, 88);
                    output.push(predictor[0] as f32 / 32768.0);
                }
            }
        } else {
            // Stereo: nibbles are interleaved in 8-nibble (4-byte) chunks per channel.
            // Layout: 4 bytes for ch0 (8 nibbles), 4 bytes for ch1 (8 nibbles), repeat.
            let mut samples_buf: Vec<[f32; 2]> = Vec::new();
            let mut pos = 0;
            while pos + 8 <= payload.len() {
                for c in 0..2 {
                    for b in 0..4 {
                        let byte = payload[pos + c * 4 + b];
                        for shift in [0u8, 4] {
                            let nibble = ((byte >> shift) & 0x0F) as usize;
                            let step = IMA_STEP_TABLE[step_index[c] as usize];
                            let mut diff = step >> 3;
                            if nibble & 4 != 0 {
                                diff += step;
                            }
                            if nibble & 2 != 0 {
                                diff += step >> 1;
                            }
                            if nibble & 1 != 0 {
                                diff += step >> 2;
                            }
                            if nibble & 8 != 0 {
                                predictor[c] -= diff;
                            } else {
                                predictor[c] += diff;
                            }
                            predictor[c] = predictor[c].clamp(-32768, 32767);
                            step_index[c] += IMA_INDEX_TABLE[nibble];
                            step_index[c] = step_index[c].clamp(0, 88);
                            let sample = predictor[c] as f32 / 32768.0;
                            let sample_idx = b * 2 + shift as usize / 4;
                            if c == 0 {
                                samples_buf.push([sample, 0.0]);
                            } else if sample_idx < samples_buf.len() {
                                let last = samples_buf.len();
                                samples_buf[last - 8 + sample_idx][1] = sample;
                            }
                        }
                    }
                }
                pos += 8;
                for pair in samples_buf.drain(..) {
                    output.push(pair[0]);
                    output.push(pair[1]);
                }
            }
        }
    }

    output
}

/// Convert raw PCM bytes to f32 samples. Output channel count matches input.
fn decode_pcm(pcm: &[u8], channels: u16, bits_per_sample: u16) -> Vec<f32> {
    match (bits_per_sample, channels) {
        (16, _) => pcm
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            .collect(),
        (8, _) => pcm.iter().map(|&b| (b as f32 - 128.0) / 128.0).collect(),
        _ => {
            log::trace!("WAV: unsupported {}bit {}ch PCM", bits_per_sample, channels);
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_test_wav(sample_rate: u32, bits: u16, channels: u16, samples: &[u8]) -> Vec<u8> {
        let data_size: u32 = samples.len() as u32;
        let fmt_size: u32 = 16;
        let byte_rate: u32 = sample_rate * channels as u32 * bits as u32 / 8;
        let block_align: u16 = channels * bits / 8;
        let riff_size: u32 = 4 + (8 + fmt_size) + (8 + data_size);

        let mut wav: Vec<u8> = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&riff_size.to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&fmt_size.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&byte_rate.to_le_bytes());
        wav.extend_from_slice(&block_align.to_le_bytes());
        wav.extend_from_slice(&bits.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_size.to_le_bytes());
        wav.extend_from_slice(samples);
        wav
    }

    #[test]
    fn test_decode_wav_16bit_mono() {
        // 4 samples of 16-bit mono silence — upmixed to 8 stereo f32 values.
        let pcm: Vec<u8> = vec![0, 0, 0, 0, 0, 0, 0, 0];
        let wav = build_test_wav(22050, 16, 1, &pcm);
        let decoded = decode_wav(&wav, "test.wav").expect("should decode");
        assert_eq!(decoded.sample_rate, 22050);
        assert_eq!(decoded.channels, 2);
        assert_eq!(decoded.samples.len(), 8); // 4 mono → 4 stereo pairs
    }

    #[test]
    fn test_decode_wav_8bit_mono() {
        let pcm: Vec<u8> = vec![128, 128, 128, 128];
        let wav = build_test_wav(11025, 8, 1, &pcm);
        let decoded = decode_wav(&wav, "test.wav").expect("should decode");
        assert_eq!(decoded.sample_rate, 11025);
        for s in &decoded.samples {
            assert!(s.abs() < 0.01);
        }
    }

    #[test]
    fn test_decode_wav_16bit_stereo() {
        let pcm: Vec<u8> = vec![0xE8, 0x03, 0x18, 0xFC, 0x00, 0x00, 0x00, 0x00];
        let wav = build_test_wav(44100, 16, 2, &pcm);
        let decoded = decode_wav(&wav, "test.wav").expect("should decode");
        assert_eq!(decoded.samples.len(), 4); // 2 stereo frames, already 2ch
    }

    #[test]
    fn test_decode_wav_too_short() {
        assert!(decode_wav(&[0u8; 10], "short.wav").is_none());
    }

    #[test]
    fn test_decode_pcm_empty() {
        let samples = decode_pcm(&[], 1, 16);
        assert!(samples.is_empty());
    }
}
