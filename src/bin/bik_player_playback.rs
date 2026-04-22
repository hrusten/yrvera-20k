//! Playback state machine, frame pacing, YUV->RGBA conversion.
// Implementation in Tasks 35, 36, 38.

use std::time::Instant;
use vera20k::assets::bink_decode::BinkDecoder;
use vera20k::assets::bink_file::BinkFile;

pub struct Playback {
    pub playing: bool,
    pub last_tick: Instant,
    pub accumulator_secs: f64,
    pub speed: f32,
}

impl Default for Playback {
    fn default() -> Self {
        Self {
            playing: false,
            last_tick: Instant::now(),
            accumulator_secs: 0.0,
            speed: 1.0,
        }
    }
}

impl Playback {
    pub fn step(
        &mut self,
        file: &BinkFile,
        decoder: &mut BinkDecoder,
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
                    *current_frame += 1;
                }
                Err(e) => {
                    *status = format!("packet error: {}", e);
                    self.playing = false;
                    break;
                }
            }
        }
    }
}
