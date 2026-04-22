//! Integration test: decode all audio packets from a real Bink fixture
//! and compare against FFmpeg's f32le PCM oracle within tight tolerance.
//!
//! The fixture files are not committed — see `tests/fixtures/bink/README.md`
//! for the production recipe. When the fixture is absent the test prints a
//! SKIP message and passes, so default `cargo test` stays green.

use vera20k::assets::bink_audio::BinkAudioDecoder;
use vera20k::assets::bink_file::BinkFile;

const BIK_PATH: &str = "tests/fixtures/bink/fixture.bik";
const PCM_PATH: &str = "tests/fixtures/bink/fixture_audio.f32";

const PEAK_TOLERANCE: f32 = 1e-4;
const RMS_TOLERANCE: f32 = 1e-5;

#[test]
fn decodes_fixture_audio_within_tolerance() {
    let bik_bytes = match std::fs::read(BIK_PATH) {
        Ok(b) => b,
        Err(_) => {
            eprintln!("SKIP: {} missing — see tests/fixtures/bink/README.md", BIK_PATH);
            return;
        }
    };
    let oracle_bytes = match std::fs::read(PCM_PATH) {
        Ok(b) => b,
        Err(_) => {
            eprintln!("SKIP: {} missing — see tests/fixtures/bink/README.md", PCM_PATH);
            return;
        }
    };

    let file = BinkFile::parse_from_slice(&bik_bytes).expect("parse fixture");
    if file.header.audio_tracks.is_empty() {
        eprintln!("SKIP: fixture has no audio tracks");
        return;
    }
    let track = file.header.audio_tracks[0];
    let mut decoder = BinkAudioDecoder::new(track).expect("audio decoder init");

    // Collect all audio packets across all frames for track 0.
    let mut ours: Vec<f32> = Vec::new();
    for frame_idx in 0..file.frame_index.len() {
        for ap in file.audio_packets(frame_idx).expect("audio packets") {
            if ap.track_index == 0 {
                let samples = decoder.decode_packet(ap.bytes).expect("decode audio packet");
                ours.extend_from_slice(&samples);
            }
        }
    }

    // Parse oracle as little-endian f32.
    assert_eq!(oracle_bytes.len() % 4, 0, "oracle file length not multiple of 4");
    let oracle: Vec<f32> = oracle_bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    // Length compare: allow ±2 blocks of slack (FFmpeg's flush + first-block convention).
    let slack = 2 * decoder.frame_len();
    assert!(
        (ours.len() as isize - oracle.len() as isize).abs() <= slack as isize,
        "sample-count mismatch: ours={}, oracle={}, slack={}",
        ours.len(), oracle.len(), slack,
    );

    // Compare common prefix.
    let n = ours.len().min(oracle.len());
    let mut peak: f32 = 0.0;
    let mut sse: f64 = 0.0;
    for i in 0..n {
        let d = (ours[i] - oracle[i]).abs();
        if d > peak { peak = d; }
        sse += (d as f64) * (d as f64);
    }
    let rms = (sse / n as f64).sqrt() as f32;

    assert!(peak < PEAK_TOLERANCE, "peak error too large: {} (limit {})", peak, PEAK_TOLERANCE);
    assert!(rms < RMS_TOLERANCE, "RMS error too large: {} (limit {})", rms, RMS_TOLERANCE);
}
