//! Playback state machine, frame pacing, YUV->RGBA conversion.
// Implementation in Tasks 35, 36, 38.

use crate::BikPlayerApp;
use std::time::Instant;
use vera20k::assets::bink_decode::{BinkDecoder, BinkFrame, ColorRange};
use vera20k::assets::bink_file::BinkFile;

/// Audio/video drift check cadence (UI ticks per check).
const DRIFT_CHECK_INTERVAL: u32 = 10;

pub struct Playback {
    pub playing: bool,
    pub last_tick: Instant,
    pub accumulator_secs: f64,
    pub speed: f32,
    /// Counts UI ticks; drift check fires every `DRIFT_CHECK_INTERVAL` ticks.
    pub tick_counter: u32,
}

impl Default for Playback {
    fn default() -> Self {
        Self {
            playing: false,
            last_tick: Instant::now(),
            accumulator_secs: 0.0,
            speed: 1.0,
            tick_counter: 0,
        }
    }
}

impl Playback {
    pub fn step(
        &mut self,
        file: &BinkFile,
        decoder: &mut BinkDecoder,
        mut audio_decoder: Option<&mut vera20k::assets::bink_audio::BinkAudioDecoder>,
        audio_sink: Option<&crate::bik_player_audio::BinkAudioSink>,
        current_frame: &mut usize,
        status: &mut String,
    ) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64();
        self.last_tick = now;

        if !self.playing {
            return;
        }

        let fps = file.header.fps();
        if fps <= 0.0 {
            return;
        }
        let frame_dt = 1.0 / fps;
        self.accumulator_secs += dt * self.speed as f64;

        while self.accumulator_secs >= frame_dt {
            self.accumulator_secs -= frame_dt;
            if *current_frame >= file.frame_index.len() {
                self.playing = false;
                break;
            }
            match file.video_packet(*current_frame) {
                Ok(pkt) => {
                    if let Err(e) = decoder.decode_frame(pkt) {
                        *status = format!("decode error at frame {}: {}", *current_frame, e);
                        self.playing = false;
                        break;
                    }
                    if let (Some(adec), Some(sink)) = (audio_decoder.as_deref_mut(), audio_sink) {
                        match file.audio_packets(*current_frame) {
                            Ok(pkts) => {
                                for ap in pkts {
                                    if ap.track_index != 0 {
                                        continue;
                                    }
                                    match adec.decode_packet(ap.bytes) {
                                        Ok(samples) => {
                                            sink.push(&samples);
                                        }
                                        Err(e) => {
                                            log::warn!(
                                                "audio decode error frame {}: {}",
                                                *current_frame, e,
                                            );
                                        }
                                    }
                                }
                            }
                            Err(e) => log::warn!(
                                "audio packet error frame {}: {}",
                                *current_frame, e,
                            ),
                        }
                    }
                    *current_frame += 1;
                }
                Err(e) => {
                    *status = format!("packet error: {}", e);
                    self.playing = false;
                    break;
                }
            }
        }

        self.tick_counter = self.tick_counter.wrapping_add(1);
        // Drift correction disabled: rodio's get_pos() includes silence consumed
        // between sink creation and first audio push, so using it as the audio
        // clock produces a stale comparison against video time. Video is driven
        // by the UI clock at file.fps; audio packets are pushed per frame, so
        // total_audio_duration == total_video_duration over the file.
        let _ = (audio_sink, fps, frame_dt);
    }
}

/// Seek the decoder to `target`: flush state, re-decode from the nearest
/// preceding keyframe up to and including `target`.
pub fn seek_to_frame(app: &mut BikPlayerApp, target: usize) -> Result<(), String> {
    if app.file.is_none() {
        return Err("no file".to_string());
    }
    if app.decoder.is_none() {
        return Err("no decoder".to_string());
    }

    let file = app.file.as_ref().unwrap();
    let n = file.frame_index.len();
    if target >= n {
        return Err(format!("target {} >= {}", target, n));
    }

    // Walk backwards to find the nearest keyframe at or before `target`.
    let mut kf = target;
    while kf > 0 && !file.frame_index[kf].is_keyframe {
        kf -= 1;
    }

    // Reset audio state before video flush so a post-seek audio push starts clean.
    if let Some(adec) = app.audio_decoder.as_mut() {
        adec.reset();
    }
    if let Some(sink) = app.audio_sink.as_ref() {
        sink.pause();
        sink.drain();
    }

    let decoder = app.decoder.as_mut().unwrap();
    decoder.flush();
    let file = app.file.as_ref().unwrap();
    for i in kf..=target {
        let pkt = file.video_packet(i).map_err(|e| e.to_string())?;
        decoder.decode_frame(pkt).map_err(|e| e.to_string())?;
    }

    // Push audio for [kf..=target] so playback resumes in sync.
    if let (Some(adec), Some(sink)) = (app.audio_decoder.as_mut(), app.audio_sink.as_ref()) {
        let file = app.file.as_ref().unwrap();
        for i in kf..=target {
            if let Ok(pkts) = file.audio_packets(i) {
                for ap in pkts {
                    if ap.track_index == 0 {
                        if let Ok(samples) = adec.decode_packet(ap.bytes) {
                            sink.push(&samples);
                        }
                    }
                }
            }
        }
        sink.resume();
    }

    app.current_frame = target + 1; // next frame to play
    Ok(())
}

/// Convert a YUV420P frame to interleaved RGBA bytes (width*height*4 bytes).
pub fn frame_to_rgba(frame: &BinkFrame) -> Vec<u8> {
    let w = frame.width as usize;
    let h = frame.height as usize;
    let mut out = vec![0u8; w * h * 4];
    let stride_y = frame.stride_y;
    let stride_uv = frame.stride_uv;

    for y in 0..h {
        for x in 0..w {
            let yv = frame.y[y * stride_y + x] as i32;
            let uv_off = (y / 2) * stride_uv + (x / 2);
            let u = frame.u[uv_off] as i32;
            let v = frame.v[uv_off] as i32;
            let (r, g, b) = match frame.color_range {
                ColorRange::Mpeg => yuv_to_rgb_mpeg(yv, u, v),
                ColorRange::Jpeg => yuv_to_rgb_jpeg(yv, u, v),
            };
            let base = (y * w + x) * 4;
            out[base] = r;
            out[base + 1] = g;
            out[base + 2] = b;
            out[base + 3] = 255;
        }
    }
    out
}

#[inline]
fn clip(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

#[inline]
fn yuv_to_rgb_mpeg(y: i32, u: i32, v: i32) -> (u8, u8, u8) {
    let c = (y - 16) * 298;
    let d = u - 128;
    let e = v - 128;
    (
        clip((c + 409 * e + 128) >> 8),
        clip((c - 100 * d - 208 * e + 128) >> 8),
        clip((c + 516 * d + 128) >> 8),
    )
}

#[inline]
fn yuv_to_rgb_jpeg(y: i32, u: i32, v: i32) -> (u8, u8, u8) {
    let d = u - 128;
    let e = v - 128;
    (
        clip(y + ((359 * e + 128) >> 8)),
        clip(y + ((-88 * d - 183 * e + 128) >> 8)),
        clip(y + ((454 * d + 128) >> 8)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mpeg_black_and_white() {
        assert_eq!(yuv_to_rgb_mpeg(16, 128, 128), (0, 0, 0));
        assert_eq!(yuv_to_rgb_mpeg(235, 128, 128), (255, 255, 255));
    }

    #[test]
    fn jpeg_black_and_white() {
        assert_eq!(yuv_to_rgb_jpeg(0, 128, 128), (0, 0, 0));
        assert_eq!(yuv_to_rgb_jpeg(255, 128, 128), (255, 255, 255));
    }

    #[test]
    fn jpeg_mid_grey() {
        assert_eq!(yuv_to_rgb_jpeg(128, 128, 128), (128, 128, 128));
    }
}
