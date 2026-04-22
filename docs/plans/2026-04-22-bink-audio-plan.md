# Bink Audio Decoder + bik-player Integration — Implementation Plan

> **For Claude:** Execute this plan task-by-task. Each task is self-contained.

**Goal:** Decode Bink Audio (DCT and RDFT variants) from `.bik` files and play it
back, in sync with video, inside the existing `bik-player` tool binary — using
zero new dependencies.

**Architecture:** New `src/assets/bink_audio.rs` decoder mirrors the existing
`bink_decode.rs` shape; FFT/RDFT/DCT primitives are hand-rolled following the
precedent of the hand-rolled video IDCT. New `src/bin/bik_player_audio.rs`
holds an SPSC ring buffer + custom `rodio::Source` that mirrors existing
`MusicPlayer` / `SfxPlayer` rodio usage but streams instead of buffering. The
existing `Playback::step()` decodes audio per video frame and pushes to the
sink; drift is corrected against `rodio::Player::get_pos()`.

**Design Doc:** [docs/plans/2026-04-22-bink-audio-design.md](docs/plans/2026-04-22-bink-audio-design.md)

---

## Grounding Summary

- **ra2-rust-game-docs/**: no Bink-audio reports (BINKW32.DLL is closed
  source and not embedded in gamemd.exe; nothing to RE).
- **Ghidra MCP**: N/A. Port source is FFmpeg's `libavcodec/binkaudio.c`,
  pinned at commit `9acd820732f0bf738bd743bbde6a5c3eadc216c2` per the PR-1
  design doc §10. DO NOT decompile BINKW32.DLL.
- **Repo patterns**:
  - Hand-rolled transform math: [src/assets/bink_decode.rs:15-139](src/assets/bink_decode.rs#L15-L139) (8×8 video IDCT, ~125 L, integer arithmetic).
  - Module structure: `bink_file.rs` (container) + `bink_decode.rs` (decoder) + `bink_bits.rs` (bit reader) + `bink_data.rs` (lookup tables). Audio extends with `bink_audio.rs` + `bink_audio_data.rs`.
  - rodio usage: [src/audio/music.rs:17-21](src/audio/music.rs#L17-L21), [src/audio/sfx.rs:23-24](src/audio/sfx.rs#L23-L24). `MixerDeviceSink` owned on the player struct, `Player::connect_new(device.mixer())` per source.
  - Test fixture: [tests/bink_first_frame.rs:14-77](tests/bink_first_frame.rs#L14-L77) SKIP-if-missing pattern. Recipe in [tests/fixtures/bink/README.md](tests/fixtures/bink/README.md).
- **INI keys**: N/A. Bink Audio is a binary format with no INI configuration.
- **Container side already done** (PR 2): [src/assets/bink_file.rs:99-123](src/assets/bink_file.rs#L99-L123) parses `AudioTrack` (sample_rate, flags, track_id), [src/assets/bink_file.rs:403-445](src/assets/bink_file.rs#L403-L445) exposes `audio_packets(i)` returning `AudioPacket { track_index, sample_count, bytes }`.
- **Unknown after grounding**: exact internal layout of FFmpeg's `AV_TX_FLOAT_DCT` (whether it consumes `frame_len` or `frame_len/2` coefficients per channel and how the second half is used). Resolved at implementation time via the oracle test — debug iteration uses the `tests/bink_audio_samples.rs` byte-close compare.

## Key Technical Decisions

- **Zero new crates.** FFT/RDFT/DCT written from scratch. — **Confidence:** high
  — **Source:** Approved in `/brainstorm` Q1=(c); precedent at `bink_decode.rs:15-139`.
- **Push from UI tick, SPSC ring buffer, rodio Source pops.** Decoder runs in `Playback::step()`. — **Confidence:** high — **Source:** Approved in `/brainstorm` Q2=(a).
- **Video FPS drives the clock; drift correction every ~10 ticks.** — **Confidence:** high — **Source:** Approved in `/brainstorm` Q3=(a).
- **Two-layer test oracle: per-primitive unit tests + FFmpeg `f32le` PCM integration test.** — **Confidence:** high — **Source:** Approved in `/brainstorm` Q4=(c); mirrors `bink_first_frame.rs`.
- **Multi-track `.bik`: decode track 0 only, warn-log others.** — **Confidence:** high — **Source:** Approved in `/brainstorm` Q5=(a); RA2 inventory all single-track per design doc §10.
- **`coeffs[]` buffer length is `frame_len + 2`** (the +2 holds the Nyquist bin moved out of `coeffs[1]` per FFmpeg's RDFT input layout). — **Confidence:** medium — **Source:** [`binkaudio.c:259-261`](C:/Users/enok/Documents/FFmpeg/libavcodec/binkaudio.c#L259-L261).
- **Bink Audio frame layout per packet** (from `binkaudio.c`):
  1. Skip 32 bits (reported size).
  2. (DCT only) skip 2 bits.
  3. Per channel: 2 floats DC (custom 29-bit `get_float()`), `num_bands × 8` band quantizer indices, then RLE-coded coefficient widths + sign-magnitude values up to `frame_len`.
  4. Inverse transform per channel.
  5. Overlap-add with previous block's tail.
  — **Confidence:** high — **Source:** [`binkaudio.c:174-281`](C:/Users/enok/Documents/FFmpeg/libavcodec/binkaudio.c#L174-L281).

## Open Questions

### Resolved During Planning

- **FFT size table for our supported sample rates**: <22050 → 512 samples; <44100 → 1024; ≥44100 → 2048. RDFT mode further multiplies by channel count for the FFT size (so stereo at <44100 = 2048-point RDFT). DCT mode uses N/2 for the inverse DCT internally. — Source: [`binkaudio.c:82-115`](C:/Users/enok/Documents/FFmpeg/libavcodec/binkaudio.c#L82-L115).
- **`s->root` scaling factor**: `RDFT: 2.0 / (sqrt(frame_len) * 32768.0)`; `DCT: frame_len / (sqrt(frame_len) * 32768.0)`. — Source: [`binkaudio.c:117-120`](C:/Users/enok/Documents/FFmpeg/libavcodec/binkaudio.c#L117-L120).
- **`quant_table[i] = expf(i * 0.15289164787221953823f) * s->root`** for i in 0..96. — Source: [`binkaudio.c:121-124`](C:/Users/enok/Documents/FFmpeg/libavcodec/binkaudio.c#L121-L124).
- **Number of bands**: smallest `n` in 1..25 such that `sample_rate_half ≤ ff_wma_critical_freqs[n-1]`. — Source: [`binkaudio.c:126-129`](C:/Users/enok/Documents/FFmpeg/libavcodec/binkaudio.c#L126-L129).
- **Band boundaries**: `bands[0]=2`, `bands[i]= (ff_wma_critical_freqs[i-1] * frame_len / sample_rate_half) & ~1` for i in 1..num_bands, `bands[num_bands]=frame_len`. — Source: [`binkaudio.c:131-135`](C:/Users/enok/Documents/FFmpeg/libavcodec/binkaudio.c#L131-L135).
- **`get_float()` decoding**: `power = 5 bits`, `mantissa = 23 bits`, `result = ldexpf(mantissa, power - 23)`, `sign_bit ? -result : result`. — Source: [`binkaudio.c:156-163`](C:/Users/enok/Documents/FFmpeg/libavcodec/binkaudio.c#L156-L163).
- **`rle_length_tab[16]`**: `{2, 3, 4, 5, 6, 8, 9, 10, 11, 12, 13, 14, 15, 16, 32, 64}`. — Source: [`binkaudio.c:165-167`](C:/Users/enok/Documents/FFmpeg/libavcodec/binkaudio.c#L165-L167).
- **`ff_wma_critical_freqs[25]`**: `{100, 200, 300, 400, 510, 630, 770, 920, 1080, 1270, 1480, 1720, 2000, 2320, 2700, 3150, 3700, 4400, 5300, 6400, 7700, 9500, 12000, 15500, 24500}`. — Source: [`wma_freqs.c:23-29`](C:/Users/enok/Documents/FFmpeg/libavcodec/wma_freqs.c#L23-L29).
- **RDFT input layout shuffle (Bink storage → FFmpeg RDFT input)**: negate imaginary halves of bins 1..N/2-1, move `coeffs[1]` (Nyquist real) to `coeffs[frame_len]`, set `coeffs[1]=0`. — Source: [`binkaudio.c:255-261`](C:/Users/enok/Documents/FFmpeg/libavcodec/binkaudio.c#L255-L261).
- **DCT input prep**: `coeffs[0] /= 0.5` (i.e. doubled) before transform call. — Source: [`binkaudio.c:252-254`](C:/Users/enok/Documents/FFmpeg/libavcodec/binkaudio.c#L252-L254).
- **Overlap-add formula**: for the first `overlap_len` samples of each block, `out[i] = (prev[i] * (count - j) + out[i] * j) / count` where `count = overlap_len * channels`, `j = ch + i*channels` (linear cross-fade). Skipped on the very first block via `s->first` flag. — Source: [`binkaudio.c:265-276`](C:/Users/enok/Documents/FFmpeg/libavcodec/binkaudio.c#L265-L276).

### Deferred to Implementation

- **Exact FFmpeg `AV_TX_FLOAT_DCT` internal coefficient ordering.** FFmpeg's tx infrastructure uses split-radix internally. Our hand-rolled inverse DCT-II of size `N/2` may need a coefficient-permutation step to match. Resolved by integration test bisection against the oracle.
- **Whether RA2 cutscenes use DCT or RDFT (or both).** Inventory from PR-1 §10 lists 141 BIK files but doesn't break down audio flag. Will inspect at Task 25 (real-cutscene smoke test) and confirm both code paths exercised in tests.
- **Sample timing of `rodio::Player::get_pos()` precision.** May not be sub-millisecond; if drift correction oscillates we tune `tick_counter % N` from 10 to higher.
- **Whether ring buffer of ~0.5 s is enough headroom under egui repaint stalls.** If under-runs occur, increase to 1.0 s. Verified at Task 25.

## File Map

| Action | Path | Responsibility |
|--------|------|----------------|
| Create | `src/assets/bink_audio.rs` | Audio decoder + private FFT/RDFT/DCT primitives |
| Create | `src/assets/bink_audio_data.rs` | WMA critical freqs + RLE length table constants |
| Create | `src/bin/bik_player_audio.rs` | SPSC ring buffer + `BinkAudioSink` + `BinkAudioSource` (`rodio::Source` impl) |
| Create | `tests/bink_audio_samples.rs` | FFmpeg `f32le` PCM oracle integration test |
| Modify | `src/assets/mod.rs` | Register `bink_audio`, `bink_audio_data` modules |
| Modify | `src/assets/error.rs` | Add `AssetError::BinkAudioError` variant |
| Modify | `src/bin/bik-player.rs` | Add `audio_decoder` + `audio_sink` fields, wire into load/seek/file-change |
| Modify | `src/bin/bik_player_playback.rs` | Push audio per frame in `step()`; drift-check; reset audio on `seek_to_frame` |
| Modify | `src/bin/bik_player_ui.rs` | Add volume slider + mute button (Task 24) |
| Modify | `tests/fixtures/bink/README.md` | Add FFmpeg PCM oracle recipe |

## Interface Changes

**New public API surface in `vera20k::assets`:**

```rust
pub struct BinkAudioDecoder { /* private */ }
impl BinkAudioDecoder {
    pub fn new(track: AudioTrack) -> Result<Self, AssetError>;
    pub fn decode_packet(&mut self, bytes: &[u8]) -> Result<Vec<f32>, AssetError>;
    pub fn sample_rate(&self) -> u32;
    pub fn channels(&self) -> u16;
    pub fn frame_len(&self) -> usize;
    pub fn use_dct(&self) -> bool;
    pub fn reset(&mut self);
}
```

**New types in the `bik-player` binary** (not exported from the library):

```rust
pub struct BinkAudioSink { /* private */ }
impl BinkAudioSink {
    pub fn new(sample_rate: u32, channels: u16) -> Option<Self>;
    pub fn push(&mut self, samples: &[f32]) -> usize;
    pub fn drain(&mut self);
    pub fn pause(&self);
    pub fn resume(&self);
    pub fn position(&self) -> std::time::Duration;
    pub fn set_volume(&self, v: f32);
}
```

**New error variant:** `AssetError::BinkAudioError { reason: String }`.

PR 5 (cutscene-in-game) will consume `BinkAudioDecoder` directly. `BinkAudioSink`
is binary-local for now; if PR 5 needs streaming audio in the main game we'll
promote it to `src/audio/streaming.rs` then.

## Sim Checklist

N/A — this PR touches no `sim/` files.

## Risk Areas

1. **FFT/RDFT/DCT correctness.** Wrong sign or scaling produces silent garbage.
   Mitigated by Tasks 4–6 unit tests (round-trip + known-answer) and the
   integration test (Task 15) catching anything that gets past unit tests.
2. **SPSC ring buffer ordering.** Atomic `head`/`tail` need correct
   `Acquire`/`Release` ordering. Mitigated by hand-rolling ~40 lines following
   the canonical SPSC pattern with explicit tests for wrap-around (Task 16).
3. **Seek state reset.** Must clear `prev[]` AND set `first=true` AND drain
   the ring buffer or the resumed audio is misaligned. Mitigated by single
   `reset()` method (Task 12) called from `seek_to_frame` (Task 22).
4. **rodio device-open failure.** `BinkAudioSink::new` returns `None`;
   `BikPlayerApp` must handle the `Option`. Tested manually at Task 25.

## Parity-Critical Items

| Task # | Item | Why it matters | Verification |
|--------|------|----------------|--------------|
| 9, 10 | Inverse-transform output matches FFmpeg bit-close | Audio that doesn't match the original is immediately audible (wrong pitch, distortion, silence) | `tests/bink_audio_samples.rs` Task 15 — peak error < 1e-4 vs FFmpeg `f32le` oracle |
| 11 | Overlap-add cross-fade exactly matches | Wrong overlap = audible click every block (~22 Hz buzz at 22050 Hz / 1024-sample blocks) | Unit test Task 13 + integration test Task 15 |
| 21 | A/V drift bounded < 1 frame | Lip-sync visibly off by tens of ms is noticed by viewers within seconds | Manual playback at Task 25 — watch a dialogue-heavy cutscene |
| 25 | Real RA2 cutscene plays correctly with audio | The whole point of the PR | Manual: pick a known cutscene (e.g., `ALLIEND1.BIK` from MOVIES01.MIX), Play, listen + watch |

---

## Tasks

### Task 1: Add `BinkAudioError` variant to `AssetError`

**Why:** Need a dedicated error variant before any audio code can return `Result<_, AssetError>`. Pattern matches existing `BinkError` variant.

**Files:**
- Modify: `src/assets/error.rs:65-71`

**Pattern:** Mirrors existing `BinkError` variant at line 66-67.

**Step 1: Add variant**

After the `BinkFrameOutOfRange` variant in [src/assets/error.rs:70-71](src/assets/error.rs#L70-L71), add:

```rust
    /// Bink audio decoder failed (truncated packet, invalid quantizer, etc.)
    #[error("Bink audio error: {reason}")]
    BinkAudioError { reason: String },
```

**Step 2: Verify**
Run: `cargo check`
Expected: clean.

**Step 3: Commit**
Message: `bink-audio: add BinkAudioError variant to AssetError`

---

### Task 2: Create `src/assets/bink_audio_data.rs` with constant tables

**Why:** Constants needed by the decoder. Isolated in their own file so the main decoder reads cleanly and so tables can be referenced without cluttering the decoder's namespace.

**Files:**
- Create: `src/assets/bink_audio_data.rs`
- Modify: `src/assets/mod.rs`

**Pattern:** Mirrors [src/assets/bink_data.rs](src/assets/bink_data.rs) (video lookup tables in their own file).

**Step 1: Create file**

`src/assets/bink_audio_data.rs`:

```rust
// Constants ported from FFmpeg's libavcodec/binkaudio.c and libavcodec/wma_freqs.c.
// Copyright (c) 2007-2011 Peter Ross (pross@xvid.org)
// Copyright (c) 2009 Daniel Verkamp (daniel@drv.nu)
// Licensed LGPL-2.1-or-later. See LICENSES/LGPL-2.1-or-later.txt in the repo root.

//! Constant tables for Bink audio decoding.
//!
//! These are pure data — no creative content. Copied verbatim from FFmpeg
//! with attribution.

/// WMA critical-band frequencies (Hz). Used to pick the number of audio
/// bands for a given sample rate.
pub const WMA_CRITICAL_FREQS: [u16; 25] = [
    100, 200, 300, 400, 510, 630, 770, 920, 1080, 1270, 1480, 1720, 2000,
    2320, 2700, 3150, 3700, 4400, 5300, 6400, 7700, 9500, 12000, 15500,
    24500,
];

/// RLE run-length lookup. Indexed by the 4-bit RLE escape value when a
/// coefficient-width run begins with bit 1.
pub const RLE_LENGTH_TAB: [u8; 16] = [
    2, 3, 4, 5, 6, 8, 9, 10, 11, 12, 13, 14, 15, 16, 32, 64,
];
```

**Step 2: Register module**

In `src/assets/mod.rs`, add `pub mod bink_audio_data;` next to `pub mod bink_data;`.

**Step 3: Verify**
Run: `cargo check`
Expected: clean.

**Step 4: Commit**
Message: `bink-audio: add bink_audio_data.rs with WMA freqs + RLE table`

---

### Task 3: Scaffold `src/assets/bink_audio.rs` with module header + LGPL attribution

**Why:** Empty file with proper header so subsequent tasks can add code in well-defined sections. Decouples the "create file" step from the "add code" steps.

**Files:**
- Create: `src/assets/bink_audio.rs`
- Modify: `src/assets/mod.rs`

**Pattern:** Mirrors [src/assets/bink_decode.rs:1-13](src/assets/bink_decode.rs#L1-L13) header.

**Step 1: Create file**

`src/assets/bink_audio.rs`:

```rust
// Ported from FFmpeg's libavcodec/binkaudio.c.
// Copyright (c) 2007-2011 Peter Ross (pross@xvid.org)
// Copyright (c) 2009 Daniel Verkamp (daniel@drv.nu)
// Licensed LGPL-2.1-or-later. See LICENSES/LGPL-2.1-or-later.txt in the repo root.

//! Bink Audio decoder (DCT and RDFT variants).
//!
//! Decodes one audio packet per call; output is interleaved f32 samples ready
//! for rodio. Maintains overlap-add state across calls. Hand-rolled radix-2
//! FFT + RDFT + inverse DCT-II primitives so we don't add audio-math crates.
//!
//! ## Dependency rules
//! - Part of assets/ — depends only on `bink_audio_data`, `bink_file::AudioTrack`,
//!   and `error::AssetError`. No game-module deps.
```

**Step 2: Register module**

In `src/assets/mod.rs`, add `pub mod bink_audio;` next to `pub mod bink_decode;`.

**Step 3: Verify**
Run: `cargo check`
Expected: clean.

**Step 4: Commit**
Message: `bink-audio: scaffold bink_audio.rs module`

---

### Task 4: Implement `Fft` struct + radix-2 forward FFT with unit test

**Why:** All other transforms (RDFT, DCT) reduce to a complex FFT. This is the foundation primitive. Round-trip-on-impulse test catches sign and bit-reverse bugs immediately.

**Files:**
- Modify: `src/assets/bink_audio.rs`

**Pattern:** New pattern (no existing FFT in repo). Standard textbook radix-2 Cooley-Tukey.

**Step 1: Add Complex32 + Fft struct**

Append to `src/assets/bink_audio.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Complex32 {
    re: f32,
    im: f32,
}

impl Complex32 {
    const fn new(re: f32, im: f32) -> Self {
        Self { re, im }
    }
}

/// Pre-computed radix-2 FFT plan. `n` must be a power of two.
struct Fft {
    n: usize,
    /// Complex twiddle factors for each stage, length n/2.
    /// twiddles[k] = (cos(-2π k / n), sin(-2π k / n))   (forward FFT sign).
    twiddles: Vec<Complex32>,
    /// Bit-reversed permutation indices, length n.
    bit_reverse: Vec<u32>,
}

impl Fft {
    fn new(n: usize) -> Self {
        assert!(n.is_power_of_two() && n >= 2, "FFT size must be power of two ≥ 2");
        let mut twiddles = Vec::with_capacity(n / 2);
        for k in 0..n / 2 {
            let theta = -2.0 * std::f32::consts::PI * (k as f32) / (n as f32);
            twiddles.push(Complex32::new(theta.cos(), theta.sin()));
        }
        let bits = n.trailing_zeros();
        let mut bit_reverse = Vec::with_capacity(n);
        for i in 0..n as u32 {
            bit_reverse.push(i.reverse_bits() >> (32 - bits));
        }
        Self { n, twiddles, bit_reverse }
    }

    /// In-place forward FFT (sign convention: e^{-2πi k n / N}).
    /// To do an inverse FFT, conjugate input, run forward, conjugate output, divide by N.
    fn forward_inplace(&self, buf: &mut [Complex32]) {
        assert_eq!(buf.len(), self.n);

        // Bit-reverse permutation.
        for i in 0..self.n {
            let j = self.bit_reverse[i] as usize;
            if j > i {
                buf.swap(i, j);
            }
        }

        // Cooley-Tukey butterflies.
        let mut size = 2;
        while size <= self.n {
            let half = size / 2;
            let twiddle_step = self.n / size;
            let mut start = 0;
            while start < self.n {
                for k in 0..half {
                    let w = self.twiddles[k * twiddle_step];
                    let a = buf[start + k];
                    let b = buf[start + k + half];
                    let t = Complex32::new(
                        b.re * w.re - b.im * w.im,
                        b.re * w.im + b.im * w.re,
                    );
                    buf[start + k] = Complex32::new(a.re + t.re, a.im + t.im);
                    buf[start + k + half] = Complex32::new(a.re - t.re, a.im - t.im);
                }
                start += size;
            }
            size *= 2;
        }
    }
}
```

**Step 2: Add unit test**

Append to the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Forward FFT followed by inverse FFT must reproduce the input within rounding error.
    fn assert_round_trip(n: usize) {
        let fft = Fft::new(n);
        // Test signal: impulse at index 1.
        let mut buf = vec![Complex32::default(); n];
        buf[1] = Complex32::new(1.0, 0.0);
        let original = buf.clone();

        // Forward.
        fft.forward_inplace(&mut buf);

        // Inverse via conjugate trick: conj, forward, conj, /N.
        for c in buf.iter_mut() {
            c.im = -c.im;
        }
        fft.forward_inplace(&mut buf);
        let inv_n = 1.0 / n as f32;
        for c in buf.iter_mut() {
            c.re *= inv_n;
            c.im = -c.im * inv_n;
        }

        for (i, (got, want)) in buf.iter().zip(original.iter()).enumerate() {
            assert!(
                (got.re - want.re).abs() < 1e-5 && (got.im - want.im).abs() < 1e-5,
                "round-trip mismatch at i={} for n={}: got {:?}, want {:?}",
                i, n, got, want,
            );
        }
    }

    #[test]
    fn fft_round_trip_256() { assert_round_trip(256); }
    #[test]
    fn fft_round_trip_512() { assert_round_trip(512); }
    #[test]
    fn fft_round_trip_1024() { assert_round_trip(1024); }
    #[test]
    fn fft_round_trip_2048() { assert_round_trip(2048); }
}
```

**Step 3: Verify**
Run: `cargo test --lib bink_audio::tests::fft -- --nocapture`
Expected: 4 tests pass.

**Step 4: Commit**
Message: `bink-audio: radix-2 FFT primitive with round-trip tests`

---

### Task 5: Implement `inverse_rdft()` with sine-wave peak test

**Why:** Bink's RDFT mode needs an inverse RDFT. Decoder reads frequency-domain coefficients in Hermitian-symmetric layout and outputs real samples. This is the second of three transform primitives.

**Files:**
- Modify: `src/assets/bink_audio.rs`

**Pattern:** Standard "real-input IDFT via half-size complex FFT" trick. Reference: any DSP textbook chapter on real-input FFTs.

**Step 1: Add the function**

Append to `src/assets/bink_audio.rs` (above the `#[cfg(test)]` block):

```rust
/// Inverse real-output DFT.
///
/// Input layout (length `frame_len + 2`, matching FFmpeg's `AV_TX_FLOAT_RDFT`):
/// - `input[0]` = DC bin (real).
/// - `input[1]` = 0 (placeholder; Nyquist is at the end).
/// - `input[2k], input[2k+1]` for k in 1..N/2 = (real, imag) of bin k.
/// - `input[frame_len]` = Nyquist bin (real).
/// - `input[frame_len+1]` = 0.
///
/// Note: caller must have already negated `input[2k+1]` halves to flip the
/// sign convention (see binkaudio.c:255-261); this function expects the
/// post-shuffle layout.
///
/// Output: `frame_len` real samples in `output[0..frame_len]`.
fn inverse_rdft(input: &[f32], output: &mut [f32], fft: &Fft) {
    let n = fft.n;
    assert_eq!(input.len(), n + 2);
    assert_eq!(output.len(), n);

    // Build the half-size complex spectrum from the real-input layout.
    // We use a half-size complex FFT trick: the real signal of length N has
    // an N/2-point complex DFT after appropriate re-mixing.
    let half = n / 2;
    let mut buf = vec![Complex32::default(); half];

    // The pack-and-unpack-real-IFFT identity:
    //   x[2m]   + j*x[2m+1] = inverse_complex_fft(Y_m)
    // where Y_m for m in 0..N/2 is constructed from the Hermitian spectrum.
    // We compute Y_m here.

    let dc = input[0];
    let nyq = input[n];
    buf[0] = Complex32::new(dc + nyq, dc - nyq);

    for m in 1..half {
        let xr = input[2 * m];
        let xi = input[2 * m + 1];
        let yr = input[2 * (half - m)];
        let yi = input[2 * (half - m) + 1];
        // Pre-twiddle factor: w = e^{+2πi m / N}  (inverse direction).
        let theta = std::f32::consts::PI * (m as f32) / (half as f32);
        let wr = theta.cos();
        let wi = theta.sin();

        // Even part: 0.5 * ((X[m] + conj(X[N/2-m])))
        let er = 0.5 * (xr + yr);
        let ei = 0.5 * (xi - yi);
        // Odd part: 0.5j * w * (X[m] - conj(X[N/2-m]))
        let dr = 0.5 * (xr - yr);
        let di = 0.5 * (xi + yi);
        // (dr + j di) * (wr + j wi) * j = (dr + j di) * (-wi + j wr)
        //   real: -dr*wi - di*wr
        //   imag:  dr*wr - di*wi
        let or = -dr * wi - di * wr;
        let oi = dr * wr - di * wi;

        buf[m] = Complex32::new(er + or, ei + oi);
    }

    // Inverse complex FFT of size half (conjugate-forward-conjugate-/N).
    for c in buf.iter_mut() {
        c.im = -c.im;
    }
    fft.forward_inplace(&mut buf);
    let inv_h = 1.0 / half as f32;
    for c in buf.iter_mut() {
        c.re *= inv_h;
        c.im = -c.im * inv_h;
    }

    // Unpack: output[2m] = re, output[2m+1] = im.
    for m in 0..half {
        output[2 * m] = buf[m].re;
        output[2 * m + 1] = buf[m].im;
    }
}
```

Note: this `Fft` instance must be of size `n / 2`, not `n`. The caller's
responsibility — documented in the function doc.

**Step 2: Add unit test**

Inside the `#[cfg(test)] mod tests` block, append:

```rust
    /// A pure-tone spectrum (single non-zero bin) inverse-transforms to
    /// a sinusoid whose peak amplitude matches our expected scaling.
    #[test]
    fn rdft_pure_tone_round_trip() {
        let n = 512;
        let half_fft = Fft::new(n / 2);
        // Build spectrum: real sinusoid of bin index 5 amplitude 1.
        // Hermitian: X[5] = (0, -0.5), X[N-5] = (0, 0.5), all else 0.
        // Bink-storage layout has imag halves negated, so input[2*5+1] = +0.5.
        let mut input = vec![0.0f32; n + 2];
        input[2 * 5] = 0.0;
        input[2 * 5 + 1] = 0.5; // post-negate
        let mut output = vec![0.0f32; n];
        inverse_rdft(&input, &mut output, &half_fft);

        // The output should be approximately sin(2π * 5 * t / N).
        // We just check the peak amplitude is in a sane range.
        let max = output.iter().cloned().fold(0.0f32, f32::max);
        let min = output.iter().cloned().fold(0.0f32, f32::min);
        assert!(max > 0.5 && max < 1.5, "max amplitude out of range: {}", max);
        assert!(min < -0.5 && min > -1.5, "min amplitude out of range: {}", min);
    }
```

**Step 3: Verify**
Run: `cargo test --lib bink_audio::tests::rdft -- --nocapture`
Expected: 1 test passes.

**Step 4: Commit**
Message: `bink-audio: inverse RDFT via half-size complex FFT`

---

### Task 6: Implement `inverse_dct_ii()` with constant-input test

**Why:** Bink's DCT mode needs an inverse DCT-II (= DCT-III). Standard reduction: pre-permute samples, run an FFT of half size, post-twiddle. Third of three transform primitives — together with FFT (Task 4) and RDFT (Task 5) we have everything the decoder needs.

**Files:**
- Modify: `src/assets/bink_audio.rs`

**Pattern:** Standard "DCT-III via FFT" reduction. Reference: Numerical Recipes Ch. 12, or Makhoul 1980.

**Step 1: Add the function**

Append to `src/assets/bink_audio.rs` (above the test block):

```rust
/// Inverse DCT-II (= DCT-III) of length `n`, scaled to match FFmpeg's
/// `AV_TX_FLOAT_DCT` inverse transform with `scale = 1.0 / n`.
///
/// Input: `n` DCT coefficients.
/// Output: `n` time-domain samples.
///
/// Caller must pre-multiply `input[0]` by 2.0 to match the binkaudio.c
/// convention (see binkaudio.c:252-254).
fn inverse_dct_ii(input: &[f32], output: &mut [f32], fft: &Fft) {
    let n = fft.n;
    assert_eq!(input.len(), n);
    assert_eq!(output.len(), n);

    // Reduce inverse DCT-II to a length-N complex FFT via the standard
    // pre-twiddle. For an inverse DCT (DCT-III):
    //   X[k] = input[k] * exp(j * π * k / (2N))
    // then complex IFFT, then de-interleave even/odd.
    let mut buf = vec![Complex32::default(); n];
    for k in 0..n {
        let theta = std::f32::consts::PI * (k as f32) / (2.0 * n as f32);
        let cr = theta.cos();
        let ci = theta.sin();
        buf[k] = Complex32::new(input[k] * cr, input[k] * ci);
    }

    // Inverse complex FFT (conjugate-forward-conjugate-/N).
    for c in buf.iter_mut() {
        c.im = -c.im;
    }
    fft.forward_inplace(&mut buf);
    let inv_n = 1.0 / n as f32;
    for c in buf.iter_mut() {
        c.re *= inv_n;
        c.im = -c.im * inv_n;
    }

    // De-interleave: output[2k] = buf[k].re, output[2k+1] = buf[N-1-k].re
    // (standard DCT-III bit-reverse-style unscramble).
    for k in 0..n / 2 {
        output[2 * k] = buf[k].re;
        output[2 * k + 1] = buf[n - 1 - k].re;
    }
}
```

**Step 2: Add unit test**

Inside the `#[cfg(test)] mod tests` block, append:

```rust
    /// IDCT of [c, 0, 0, ..., 0] is a constant signal proportional to c.
    #[test]
    fn idct_dc_only_constant_output() {
        let n = 512;
        let fft = Fft::new(n);
        let mut input = vec![0.0f32; n];
        input[0] = 2.0; // pre-doubled per binkaudio.c convention
        let mut output = vec![0.0f32; n];
        inverse_dct_ii(&input, &mut output, &fft);

        // All samples should be equal (within rounding) since input is DC-only.
        let first = output[0];
        for (i, &s) in output.iter().enumerate() {
            assert!(
                (s - first).abs() < 1e-4,
                "non-constant output at i={}: got {}, expected {}", i, s, first,
            );
        }
        // And nonzero — DC input shouldn't yield silence.
        assert!(first.abs() > 1e-3, "DC output is silent: {}", first);
    }
```

**Step 3: Verify**
Run: `cargo test --lib bink_audio::tests::idct -- --nocapture`
Expected: 1 test passes.

**Step 4: Commit**
Message: `bink-audio: inverse DCT-II primitive`

---

### Task 7: Define `BinkAudioDecoder` struct + `new()` constructor

**Why:** Decoder owns all state — frame_len, num_bands, band boundaries, quant table, overlap tail, FFT plan. Construction is non-trivial (band boundaries depend on sample rate via WMA freqs, scaling factor differs DCT vs RDFT). Defining the type and constructor first lets later tasks fill in `decode_packet`.

**Files:**
- Modify: `src/assets/bink_audio.rs`

**Pattern:** Mirrors [src/assets/bink_decode.rs:344-400](src/assets/bink_decode.rs#L344-L400) (`BinkDecoder::new`).

**Step 1: Add use + struct + new()**

At the top of `src/assets/bink_audio.rs` (after the module header), add:

```rust
use crate::assets::bink_audio_data::{RLE_LENGTH_TAB, WMA_CRITICAL_FREQS};
use crate::assets::bink_file::AudioTrack;
use crate::assets::error::AssetError;
```

Below the existing types, add (above the FFT primitives, at the top of the impl section):

```rust
const MAX_CHANNELS: usize = 2;
const MAX_BANDS: usize = 26;

pub struct BinkAudioDecoder {
    sample_rate: u32,
    channels: u16,
    use_dct: bool,
    /// Number of samples per inverse-transform output (per channel).
    /// For RDFT mode this equals the multi-channel-folded `frame_len`.
    frame_len: usize,
    /// Cross-fade length (`frame_len / 16`).
    overlap_len: usize,
    num_bands: usize,
    /// Band boundaries in coefficient-index space. `bands[num_bands] = frame_len`.
    bands: [u32; MAX_BANDS],
    /// Dequantization multipliers, indexed by 8-bit quantizer value (clamped to 95).
    quant_table: [f32; 96],
    /// Global scaling factor; differs DCT vs RDFT.
    root: f32,
    /// True until the first block is decoded; suppresses overlap-add on first call.
    first: bool,
    /// Per-channel overlap-tail buffer. Length `channels`, each entry `overlap_len`.
    prev: Vec<Vec<f32>>,
    /// Scratch buffer of length `frame_len + 2` (the +2 holds the Nyquist
    /// bin moved out of `coeffs[1]` for RDFT input layout).
    coeffs: Vec<f32>,
    /// Per-channel inverse-transform output buffer, length `frame_len`.
    out_per_ch: Vec<Vec<f32>>,
    /// FFT plan sized for the selected transform. RDFT uses `frame_len / 2`
    /// (real-input via half-size complex FFT). DCT uses `frame_len` (our
    /// `inverse_dct_ii` operates on N coefficients producing N samples).
    fft: Fft,
}

impl BinkAudioDecoder {
    pub fn new(track: AudioTrack) -> Result<Self, AssetError> {
        let sample_rate_in = track.sample_rate as u32;
        let channels_in = if track.is_stereo() { 2u16 } else { 1u16 };
        let use_dct = track.uses_dct();

        if !(1..=MAX_CHANNELS as u16).contains(&channels_in) {
            return Err(AssetError::BinkAudioError {
                reason: format!("invalid channel count: {}", channels_in),
            });
        }
        if sample_rate_in == 0 {
            return Err(AssetError::BinkAudioError {
                reason: "zero sample rate".to_string(),
            });
        }

        // Determine frame_len from sample rate.
        let mut frame_len_bits: u32 = if sample_rate_in < 22050 {
            9
        } else if sample_rate_in < 44100 {
            10
        } else {
            11
        };

        // Per binkaudio.c:99-111: RDFT mode treats audio as interleaved single-channel
        // by multiplying sample_rate by channel count and scaling frame_len up.
        let (sample_rate, channels) = if use_dct {
            (sample_rate_in, channels_in)
        } else {
            // RDFT: fold channels into one logical stream.
            frame_len_bits += (channels_in as u32 - 1).max(0); // av_log2(channels) for channels in 1..=2
            (sample_rate_in.checked_mul(channels_in as u32).ok_or_else(|| {
                AssetError::BinkAudioError { reason: "sample-rate overflow".to_string() }
            })?, 1u16)
        };

        let frame_len = 1usize << frame_len_bits;
        let overlap_len = frame_len / 16;
        let sample_rate_half = (sample_rate as u64 + 1) / 2;

        // Scaling factor differs by transform.
        let root = if use_dct {
            (frame_len as f32) / ((frame_len as f32).sqrt() * 32768.0)
        } else {
            2.0 / ((frame_len as f32).sqrt() * 32768.0)
        };

        // Quantization table.
        let mut quant_table = [0f32; 96];
        for i in 0..96 {
            quant_table[i] = (i as f32 * 0.15289164787221953823f32).exp() * root;
        }

        // Number of bands.
        let mut num_bands: usize = 1;
        while num_bands < 25 {
            if sample_rate_half <= WMA_CRITICAL_FREQS[num_bands - 1] as u64 {
                break;
            }
            num_bands += 1;
        }

        // Band boundaries.
        let mut bands = [0u32; MAX_BANDS];
        bands[0] = 2;
        for i in 1..num_bands {
            let v = (WMA_CRITICAL_FREQS[i - 1] as u64 * frame_len as u64) / sample_rate_half;
            bands[i] = (v as u32) & !1;
        }
        bands[num_bands] = frame_len as u32;

        // FFT plan: size depends on transform variant.
        // RDFT: real-input IDFT decomposes to a complex FFT of N/2.
        // DCT:  our inverse_dct_ii uses an N-point FFT internally.
        let fft = Fft::new(if use_dct { frame_len } else { frame_len / 2 });

        let prev = (0..channels).map(|_| vec![0.0f32; overlap_len]).collect();
        let out_per_ch = (0..channels).map(|_| vec![0.0f32; frame_len]).collect();
        let coeffs = vec![0.0f32; frame_len + 2];

        Ok(Self {
            sample_rate,
            channels,
            use_dct,
            frame_len,
            overlap_len,
            num_bands,
            bands,
            quant_table,
            root,
            first: true,
            prev,
            coeffs,
            out_per_ch,
            fft,
        })
    }

    pub fn sample_rate(&self) -> u32 { self.sample_rate }
    pub fn channels(&self) -> u16 { self.channels }
    pub fn frame_len(&self) -> usize { self.frame_len }
    pub fn use_dct(&self) -> bool { self.use_dct }
    pub fn reset(&mut self) {
        self.first = true;
        for ch in &mut self.prev {
            for s in ch.iter_mut() { *s = 0.0; }
        }
    }
}
```

**Step 2: Verify**
Run: `cargo check`
Expected: clean (no warnings about unused fields — tests in later tasks will use them; if cargo warns, suppress with `#[allow(dead_code)]` on the fields temporarily — Task 8 removes the need).

**Step 3: Commit**
Message: `bink-audio: BinkAudioDecoder struct + constructor`

---

### Task 8: Implement `decode_packet()` outer loop + per-channel iteration

**Why:** Public entry point of the decoder. Sets up the bit reader, skips the 4-byte reported-size header, and iterates per channel calling the per-block decode (filled in Task 9–11). Returns the interleaved samples.

**Files:**
- Modify: `src/assets/bink_audio.rs`

**Pattern:** Mirrors FFmpeg's `binkaudio_receive_frame` ([binkaudio.c:296-360](C:/Users/enok/Documents/FFmpeg/libavcodec/binkaudio.c#L296-L360)) and `decode_block` ([binkaudio.c:174-281](C:/Users/enok/Documents/FFmpeg/libavcodec/binkaudio.c#L174-L281)) outer loop.

**Step 1: Add use for the bit reader**

At the top of `src/assets/bink_audio.rs`, ensure:

```rust
use crate::assets::bink_bits::BitReader;
```

**Step 2: Add `decode_packet` (with stub helpers)**

Append in the `impl BinkAudioDecoder` block:

```rust
    /// Decode one audio packet. Returns interleaved f32 samples (stereo: L,R,L,R,...).
    ///
    /// Per packet, exactly `(frame_len - overlap_len) * channels` samples are produced
    /// after overlap-add (the overlap_len tail is buffered for the next call).
    pub fn decode_packet(&mut self, bytes: &[u8]) -> Result<Vec<f32>, AssetError> {
        if bytes.len() < 4 {
            return Err(AssetError::BinkAudioError {
                reason: format!("audio packet too small: {} bytes", bytes.len()),
            });
        }

        let mut gb = BitReader::from_bytes(bytes);
        // Skip reported size (32 bits) per binkaudio.c:322.
        gb.read_bits(32)?;

        // Skip 2-bit header for DCT variant per binkaudio.c:183-184.
        if self.use_dct {
            gb.read_bits(2)?;
        }

        // Decode each channel's coefficients + inverse-transform into self.out_per_ch[ch].
        for ch in 0..self.channels as usize {
            self.decode_channel_block(&mut gb, ch)?;
        }

        // Apply overlap-add with previous block's tail.
        self.apply_overlap_add();

        // Interleave channels into output.
        let usable = self.frame_len - self.overlap_len;
        let mut out = Vec::with_capacity(usable * self.channels as usize);
        for i in 0..usable {
            for ch in 0..self.channels as usize {
                out.push(self.out_per_ch[ch][i]);
            }
        }

        // Stash the last `overlap_len` samples per channel for next call's cross-fade.
        for ch in 0..self.channels as usize {
            self.prev[ch].copy_from_slice(
                &self.out_per_ch[ch][self.frame_len - self.overlap_len..self.frame_len]
            );
        }

        self.first = false;
        Ok(out)
    }

    /// Stub — filled in Tasks 9-10.
    fn decode_channel_block(&mut self, _gb: &mut BitReader, _ch: usize) -> Result<(), AssetError> {
        // Placeholder: zero-fill the channel buffer so the rest of the pipeline is testable.
        for s in self.out_per_ch[_ch].iter_mut() {
            *s = 0.0;
        }
        Ok(())
    }

    /// Stub — filled in Task 11.
    fn apply_overlap_add(&mut self) {
        // Placeholder: no-op for now.
    }
```

**Step 3: Verify**
Run: `cargo check`
Expected: clean. (If `read_bits` returns a different error type than `AssetError`, wrap with `.map_err(...)`.)

**Step 4: Commit**
Message: `bink-audio: decode_packet outer loop with per-channel stubs`

---

### Task 9: Implement coefficient decoding inside `decode_channel_block`

**Why:** The biggest piece of the decoder. Reads the two leading DC samples (custom 29-bit float), the band quantizer indices, and the RLE-coded coefficient widths and signed values. Output: filled `coeffs[0..frame_len]`.

**Files:**
- Modify: `src/assets/bink_audio.rs`

**Pattern:** Direct port of `decode_block` ([binkaudio.c:186-250](C:/Users/enok/Documents/FFmpeg/libavcodec/binkaudio.c#L186-L250)).

**Step 1: Add `get_float()` helper**

Append in `impl BinkAudioDecoder`:

```rust
    /// Custom 29-bit float decode used for the two DC coefficients.
    /// Layout: 5-bit exponent (power), 23-bit mantissa, 1-bit sign.
    /// See binkaudio.c:156-163.
    fn get_float(gb: &mut BitReader) -> Result<f32, AssetError> {
        let power = gb.read_bits(5)? as i32;
        let mantissa = gb.read_bits(23)? as f32;
        let mut f = mantissa * 2.0f32.powi(power - 23);
        if gb.read_bits(1)? != 0 {
            f = -f;
        }
        Ok(f)
    }
```

**Step 2: Replace `decode_channel_block` stub with the real implementation**

Replace the body of `decode_channel_block`:

```rust
    fn decode_channel_block(&mut self, gb: &mut BitReader, ch: usize) -> Result<(), AssetError> {
        // Per-channel quantizer scratch.
        let mut quant = [0f32; 25];

        // Two leading DC samples: custom 29-bit floats, scaled by root.
        // Per binkaudio.c:192-197.
        self.coeffs[0] = Self::get_float(gb)? * self.root;
        self.coeffs[1] = Self::get_float(gb)? * self.root;

        // Band quantizer indices (one byte per band).
        // Per binkaudio.c:201-204.
        for i in 0..self.num_bands {
            let value = gb.read_bits(8)? as usize;
            let clamped = value.min(95);
            quant[i] = self.quant_table[clamped];
        }

        // RLE-coded coefficient decode loop.
        // Per binkaudio.c:206-250.
        let mut k: usize = 0;
        let mut q = quant[0];
        let mut i: usize = 2;
        while i < self.frame_len {
            // Determine run-end `j`.
            let v_bit = gb.read_bits(1)?;
            let j: usize = if v_bit != 0 {
                let v = gb.read_bits(4)? as usize;
                i + (RLE_LENGTH_TAB[v] as usize) * 8
            } else {
                i + 8
            };
            let j = j.min(self.frame_len);

            let width = gb.read_bits(4)? as u32;
            if width == 0 {
                // Zero-fill the run.
                for slot in &mut self.coeffs[i..j] {
                    *slot = 0.0;
                }
                i = j;
                while k < self.num_bands && (self.bands[k] as usize) < i {
                    k += 1;
                    if k < self.num_bands {
                        q = quant[k];
                    }
                }
            } else {
                // Decode `width`-bit magnitudes + sign per coefficient.
                while i < j {
                    if k < self.num_bands && self.bands[k] as usize == i {
                        q = quant[k];
                        k += 1;
                    }
                    let coeff = gb.read_bits(width)? as i32;
                    if coeff != 0 {
                        let sign = gb.read_bits(1)? != 0;
                        self.coeffs[i] = if sign {
                            -q * (coeff as f32)
                        } else {
                            q * (coeff as f32)
                        };
                    } else {
                        self.coeffs[i] = 0.0;
                    }
                    i += 1;
                }
            }
        }

        // Per-channel inverse transform — filled in Task 10.
        self.inverse_transform_channel(ch);
        Ok(())
    }

    /// Stub — filled in Task 10.
    fn inverse_transform_channel(&mut self, ch: usize) {
        for s in self.out_per_ch[ch].iter_mut() {
            *s = 0.0;
        }
    }
```

**Step 3: Verify**
Run: `cargo check`
Expected: clean. No new tests yet — full integration test in Task 15.

**Step 4: Commit**
Message: `bink-audio: implement coefficient + DC + band-quantizer decode`

---

### Task 10: Implement `inverse_transform_channel()` — DCT and RDFT dispatch

**Why:** Converts the decoded coefficients into time-domain samples. Two code paths: DCT and RDFT, each with the storage-layout shuffle FFmpeg's tx expects.

**Files:**
- Modify: `src/assets/bink_audio.rs`

**Pattern:** Mirrors [binkaudio.c:252-262](C:/Users/enok/Documents/FFmpeg/libavcodec/binkaudio.c#L252-L262).

**Step 1: Replace the stub**

Replace `inverse_transform_channel` with:

```rust
    fn inverse_transform_channel(&mut self, ch: usize) {
        if self.use_dct {
            // Pre-double DC per binkaudio.c:253.
            self.coeffs[0] *= 2.0;
            // Inverse DCT-II of size frame_len consumes coeffs[0..frame_len]
            // and writes frame_len samples. Task 7 sized self.fft to frame_len
            // for this mode.
            let coeffs_in: Vec<f32> = self.coeffs[..self.frame_len].to_vec();
            inverse_dct_ii(&coeffs_in, &mut self.out_per_ch[ch], &self.fft);
        } else {
            // RDFT input layout shuffle per binkaudio.c:255-261.
            // Negate imaginary halves of bins 1..N/2-1.
            let mut i = 2;
            while i < self.frame_len {
                self.coeffs[i + 1] = -self.coeffs[i + 1];
                i += 2;
            }
            // Move Nyquist (coeffs[1]) to coeffs[frame_len]; zero the placeholder.
            self.coeffs[self.frame_len] = self.coeffs[1];
            self.coeffs[self.frame_len + 1] = 0.0;
            self.coeffs[1] = 0.0;

            // Inverse RDFT writes frame_len real samples.
            let coeffs_in: Vec<f32> = self.coeffs.clone();
            inverse_rdft(&coeffs_in, &mut self.out_per_ch[ch], &self.fft);
        }
    }
```

**Step 2: Verify**
Run: `cargo check && cargo test --lib bink_audio -- --nocapture`
Expected: previous unit tests still pass; nothing new tests audio decode end-to-end yet (that's Task 15).

**Step 3: Commit**
Message: `bink-audio: dispatch DCT vs RDFT inverse transform per channel`

---

### Task 11: Implement `apply_overlap_add()` cross-fade with previous tail

**Why:** Without overlap-add, every block boundary clicks audibly (~22 Hz buzz at typical sizes). The `first` flag suppresses overlap-add on the very first call. Direct port of FFmpeg's overlap loop.

**Files:**
- Modify: `src/assets/bink_audio.rs`

**Pattern:** Direct port of [binkaudio.c:265-276](C:/Users/enok/Documents/FFmpeg/libavcodec/binkaudio.c#L265-L276).

**Step 1: Replace the stub**

Replace `apply_overlap_add`:

```rust
    fn apply_overlap_add(&mut self) {
        if self.first {
            return;
        }
        let count = (self.overlap_len as i32) * (self.channels as i32);
        for ch in 0..self.channels as usize {
            let mut j = ch as i32;
            for i in 0..self.overlap_len {
                let prev = self.prev[ch][i];
                let cur = self.out_per_ch[ch][i];
                self.out_per_ch[ch][i] = (prev * (count - j) as f32 + cur * j as f32) / count as f32;
                j += self.channels as i32;
            }
        }
    }
```

**Step 2: Verify**
Run: `cargo check`
Expected: clean.

**Step 3: Commit**
Message: `bink-audio: overlap-add cross-fade with previous block tail`

---

### Task 12: Add silent-packet + first-flag unit tests

**Why:** The decoder is complete enough now to test gross properties: a packet with all-zero coefficients should decode to silence (within numerical noise), and the `first` flag should suppress cross-fade on the initial call.

**Files:**
- Modify: `src/assets/bink_audio.rs`

**Pattern:** Mirrors unit tests in [src/assets/bink_decode.rs](src/assets/bink_decode.rs) — synthetic input → property assertion.

**Step 1: Add tests**

Inside the `#[cfg(test)] mod tests` block, append:

```rust
    fn make_track(sample_rate: u16, stereo: bool, dct: bool) -> AudioTrack {
        let mut flags = 0u16;
        if stereo { flags |= 0x2000; }
        if dct { flags |= 0x1000; }
        AudioTrack { sample_rate, flags, track_id: 0 }
    }

    /// Build the smallest possible packet that decodes to silence: 4-byte
    /// reported-size header + (DCT only: 2 padding bits) + DC = 0 + DC = 0
    /// + all bands quantizer 0 + zero-width RLE runs to fill the frame.
    fn make_silent_packet(d: &BinkAudioDecoder) -> Vec<u8> {
        let mut bits: Vec<u8> = Vec::new();
        let mut acc: u32 = 0;
        let mut nbits: u32 = 0;
        let mut push = |val: u32, n: u32, acc: &mut u32, nbits: &mut u32, bits: &mut Vec<u8>| {
            *acc |= val << *nbits;
            *nbits += n;
            while *nbits >= 8 {
                bits.push((*acc & 0xFF) as u8);
                *acc >>= 8;
                *nbits -= 8;
            }
        };

        // 4-byte reported size (placeholder).
        bits.extend_from_slice(&[0u8; 4]);

        if d.use_dct {
            push(0, 2, &mut acc, &mut nbits, &mut bits);
        }
        for _ in 0..d.channels as usize {
            // get_float = 0: power=0, mantissa=0, sign=0 → 29 bits of zero.
            push(0, 29, &mut acc, &mut nbits, &mut bits);
            push(0, 29, &mut acc, &mut nbits, &mut bits);
            // num_bands × 8-bit quantizer indices = 0.
            for _ in 0..d.num_bands {
                push(0, 8, &mut acc, &mut nbits, &mut bits);
            }
            // Fill from i=2 to frame_len with zero-width runs.
            // Each run: bit 0 (no RLE escape), then 4-bit width=0, advances by 8 coefficients.
            let runs = (d.frame_len - 2 + 7) / 8;
            for _ in 0..runs {
                push(0, 1, &mut acc, &mut nbits, &mut bits); // no escape
                push(0, 4, &mut acc, &mut nbits, &mut bits); // width = 0
            }
        }
        // Flush remaining bits.
        if nbits > 0 {
            bits.push((acc & 0xFF) as u8);
        }
        bits
    }

    #[test]
    fn decode_silent_packet_yields_silence_dct() {
        let track = make_track(22050, true, true);
        let mut dec = BinkAudioDecoder::new(track).unwrap();
        let pkt = make_silent_packet(&dec);
        let samples = dec.decode_packet(&pkt).expect("decode silent packet");
        assert!(!samples.is_empty(), "should produce some samples");
        for &s in &samples {
            assert!(s.abs() < 1e-3, "non-silent sample: {}", s);
        }
    }

    #[test]
    fn decode_silent_packet_yields_silence_rdft() {
        let track = make_track(22050, true, false);
        let mut dec = BinkAudioDecoder::new(track).unwrap();
        let pkt = make_silent_packet(&dec);
        let samples = dec.decode_packet(&pkt).expect("decode silent packet");
        assert!(!samples.is_empty());
        for &s in &samples {
            assert!(s.abs() < 1e-3, "non-silent sample: {}", s);
        }
    }

    #[test]
    fn reset_restores_first_flag() {
        let track = make_track(22050, true, false);
        let mut dec = BinkAudioDecoder::new(track).unwrap();
        let pkt = make_silent_packet(&dec);
        dec.decode_packet(&pkt).unwrap();
        assert!(!dec.first);
        dec.reset();
        assert!(dec.first);
        for ch in &dec.prev {
            for &s in ch { assert_eq!(s, 0.0); }
        }
    }
```

**Step 2: Verify**
Run: `cargo test --lib bink_audio -- --nocapture`
Expected: all bink_audio tests pass.

**Step 3: Commit**
Message: `bink-audio: silent-packet + reset() unit tests`

---

### Task 13: Add `tests/fixtures/bink/README.md` PCM oracle recipe

**Why:** Documents how developers produce the audio oracle for the integration test. Keeps the fixture pattern consistent with the existing video oracle recipe.

**Files:**
- Modify: `tests/fixtures/bink/README.md`

**Pattern:** Mirrors existing recipe in the same file.

**Step 1: Append PCM oracle section**

Append to `tests/fixtures/bink/README.md`:

```markdown

## Audio oracle

`fixture_audio.f32` is the PCM oracle for the audio decoder integration test
(`tests/bink_audio_samples.rs`). It contains interleaved 32-bit float samples
matching the audio stream in `fixture.bik`. Produce it with:

    ffmpeg -i fixture.bik -c:a pcm_f32le -f f32le fixture_audio.f32

The integration test compares our decoded samples against this file with a
peak-error tolerance of 1e-4. SKIP if the file is absent.

If `fixture.bik` has multiple audio tracks, FFmpeg's default selects track 0,
which matches our decoder's track-0-only behavior.
```

**Step 2: Verify**
None needed (docs only).

**Step 3: Commit**
Message: `bink-audio: document FFmpeg PCM oracle recipe`

---

### Task 14: Create `tests/bink_audio_samples.rs` integration test

**Why:** End-to-end verification: decode a real Bink file's audio and compare against FFmpeg's output. SKIP-if-missing pattern keeps default `cargo test` green.

**Files:**
- Create: `tests/bink_audio_samples.rs`

**Pattern:** Mirrors [tests/bink_first_frame.rs:14-77](tests/bink_first_frame.rs#L14-L77) exactly.

**Step 1: Create the file**

`tests/bink_audio_samples.rs`:

```rust
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
```

**Step 2: Verify**
Run: `cargo test --test bink_audio_samples -- --nocapture`
Expected: test runs and either passes (if fixtures present) or prints SKIP and passes (if absent).

**Step 3: Commit**
Message: `bink-audio: integration test against FFmpeg PCM oracle`

---

### Task 15: Generate fixture audio oracle locally and run the integration test

**Why:** Until we run real audio data through the decoder we don't know if our hand-rolled FFT/RDFT/DCT actually matches FFmpeg. This task is the first end-to-end correctness check. Expect to iterate on bugs found here back into Tasks 9–11.

**Files:** none (data only).

**Step 1: Produce the audio oracle**

Run on the existing `tests/fixtures/bink/fixture.bik` (must already exist from PR 2; if not, follow the recipe in `README.md` to make one):

```
ffmpeg -y -i tests/fixtures/bink/fixture.bik -c:a pcm_f32le -f f32le tests/fixtures/bink/fixture_audio.f32
```

**Step 2: Run the integration test**

Run: `cargo test --test bink_audio_samples -- --nocapture`

Expected: PASS. If FAIL, iterate on Tasks 9–11 (most likely culprits: RDFT shuffle sign, IDCT scaling, overlap-add direction). Use the divergence index reported by the test to bisect — divergence at index < `frame_len` means first-block error (suspect transform); divergence at higher indices means overlap-add error.

**Step 3: Commit when green**
Message: `bink-audio: verify decoder vs FFmpeg PCM oracle`

(If we made code fixes in this task to chase divergence, include them in a single commit `bink-audio: fix <root cause> revealed by oracle compare`.)

---

### Task 16: Create `src/bin/bik_player_audio.rs` with SPSC ring buffer

**Why:** Foundational primitive for the audio sink. Hand-rolled to avoid dep, ~50 lines, with explicit memory ordering. Built first because the rodio Source impl in Task 17 depends on it.

**Files:**
- Create: `src/bin/bik_player_audio.rs`
- Modify: `src/bin/bik-player.rs`

**Pattern:** Standard SPSC ring buffer with two `AtomicUsize` indices and `UnsafeCell` storage. References: Cargo's `crossbeam-channel` SPSC docs, `rtrb` crate's README. We don't add either crate — we reproduce the design.

**Step 1: Create the file**

`src/bin/bik_player_audio.rs`:

```rust
//! Audio sink for the bik-player binary.
//!
//! Owns a rodio `MixerDeviceSink` + `Player`, and a single-producer-single-consumer
//! ring buffer for streaming f32 samples from the decoder thread (UI tick) to
//! rodio's audio thread. Hand-rolled SPSC — no extra crates.

use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Lock-free single-producer-single-consumer ring buffer of f32 samples.
///
/// Capacity is rounded up to a power of two so head/tail wrap with masking.
pub(crate) struct SpscRing {
    capacity: usize,
    mask: usize,
    /// `UnsafeCell<f32>` lets us mutate slots from the producer side while
    /// the consumer reads them. Safety is guaranteed by the head/tail discipline.
    buffer: Box<[UnsafeCell<f32>]>,
    /// Producer-incremented; consumer reads with Acquire.
    head: AtomicUsize,
    /// Consumer-incremented; producer reads with Acquire.
    tail: AtomicUsize,
}

// SAFETY: SpscRing's UnsafeCell access is disciplined: the producer only writes
// to slots between [tail, head) (modulo wrap), the consumer only reads from
// [tail, head). The head/tail atomics provide the necessary synchronization.
unsafe impl Sync for SpscRing {}
unsafe impl Send for SpscRing {}

impl SpscRing {
    pub fn new(min_capacity: usize) -> Arc<Self> {
        let capacity = min_capacity.next_power_of_two().max(2);
        let buffer = (0..capacity)
            .map(|_| UnsafeCell::new(0.0f32))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Arc::new(Self {
            capacity,
            mask: capacity - 1,
            buffer,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        })
    }

    /// Producer-side: push as many samples as fit; returns the number pushed.
    pub fn push(&self, samples: &[f32]) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let free = self.capacity - head.wrapping_sub(tail);
        let n = samples.len().min(free);
        for i in 0..n {
            let idx = (head + i) & self.mask;
            // SAFETY: only producer writes to this slot; no concurrent reader yet
            // (consumer cannot advance past `head` until we publish the new head).
            unsafe { *self.buffer[idx].get() = samples[i]; }
        }
        self.head.store(head.wrapping_add(n), Ordering::Release);
        n
    }

    /// Consumer-side: pop a single sample, returns None if empty.
    pub fn pop(&self) -> Option<f32> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        let idx = tail & self.mask;
        // SAFETY: producer cannot write to this slot until tail is advanced.
        let v = unsafe { *self.buffer[idx].get() };
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Some(v)
    }

    /// Producer-side: clear by advancing tail to head (samples are dropped).
    /// Safe to call only when audio thread is paused; otherwise consumer races.
    pub fn drain(&self) {
        let head = self.head.load(Ordering::Acquire);
        self.tail.store(head, Ordering::Release);
    }

    pub fn capacity(&self) -> usize { self.capacity }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_pop_round_trip() {
        let r = SpscRing::new(8);
        assert_eq!(r.push(&[1.0, 2.0, 3.0]), 3);
        assert_eq!(r.pop(), Some(1.0));
        assert_eq!(r.pop(), Some(2.0));
        assert_eq!(r.pop(), Some(3.0));
        assert_eq!(r.pop(), None);
    }

    #[test]
    fn push_full_drops_overflow() {
        let r = SpscRing::new(4); // capacity 4
        let pushed = r.push(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(pushed, 4);
    }

    #[test]
    fn wrap_around_works() {
        let r = SpscRing::new(4);
        for _ in 0..3 {
            r.push(&[10.0, 20.0]);
            assert_eq!(r.pop(), Some(10.0));
            assert_eq!(r.pop(), Some(20.0));
        }
    }

    #[test]
    fn drain_empties_buffer() {
        let r = SpscRing::new(8);
        r.push(&[1.0, 2.0, 3.0, 4.0]);
        r.drain();
        assert_eq!(r.pop(), None);
    }
}
```

**Step 2: Register module in the binary**

In `src/bin/bik-player.rs`, add `mod bik_player_audio;` near the existing `mod bik_player_playback;` and `mod bik_player_ui;`.

**Step 3: Verify**
Run: `cargo test --bin bik-player -- --nocapture`
Expected: 4 SpscRing tests pass + the existing 3 YUV tests still pass.

**Step 4: Commit**
Message: `bik-player: SPSC ring buffer for audio sample streaming`

---

### Task 17: Implement `BinkAudioSource` (rodio `Source` impl)

**Why:** Rodio consumes audio via the `Source` trait. This is where the audio thread reads from the ring buffer. Returns silence when the buffer is empty (never stalls the audio thread).

**Files:**
- Modify: `src/bin/bik_player_audio.rs`

**Pattern:** First custom rodio Source in the codebase. References: rodio 0.22 docs for `rodio::Source` trait.

**Step 1: Add the Source impl**

Append to `src/bin/bik_player_audio.rs`:

```rust
use std::num::NonZero;
use std::time::Duration;

use rodio::Source;

/// rodio Source pulling samples from an `SpscRing`. Returns 0.0 (silence)
/// when the buffer is empty so the audio thread never stalls.
pub(crate) struct BinkAudioSource {
    ring: Arc<SpscRing>,
    sample_rate: NonZero<u32>,
    channels: NonZero<u16>,
}

impl BinkAudioSource {
    pub fn new(ring: Arc<SpscRing>, sample_rate: u32, channels: u16) -> Option<Self> {
        Some(Self {
            ring,
            sample_rate: NonZero::new(sample_rate)?,
            channels: NonZero::new(channels)?,
        })
    }
}

impl Iterator for BinkAudioSource {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        // Always Some — silence-fill on underrun. The Source must never end
        // unless we want rodio to drop the player.
        Some(self.ring.pop().unwrap_or(0.0))
    }
}

impl Source for BinkAudioSource {
    fn current_span_len(&self) -> Option<usize> { None }
    fn channels(&self) -> NonZero<u16> { self.channels }
    fn sample_rate(&self) -> NonZero<u32> { self.sample_rate }
    fn total_duration(&self) -> Option<Duration> { None } // streaming — unbounded
}
```

**Step 2: Verify**
Run: `cargo check --bin bik-player`
Expected: clean. If rodio 0.22's `Source` trait method signatures differ (e.g., method name `current_frame_len` instead of `current_span_len`), adjust to match the version in [Cargo.toml:97](Cargo.toml#L97). Confirm by `cargo doc --open -p rodio` if needed.

**Step 3: Commit**
Message: `bik-player: rodio Source impl backed by SPSC ring`

---

### Task 18: Implement `BinkAudioSink` lifecycle wrapper

**Why:** Owns the rodio `MixerDeviceSink` + `Player` + producer half of the ring. Provides the high-level API that `BikPlayerApp` calls: `push`, `drain`, `pause`, `resume`, `position`, `set_volume`. Mirrors the existing `MusicPlayer` / `SfxPlayer` shape.

**Files:**
- Modify: `src/bin/bik_player_audio.rs`

**Pattern:** Mirrors [src/audio/music.rs:36-69](src/audio/music.rs#L36-L69) (`MusicPlayer::new`).

**Step 1: Add the sink struct**

Append to `src/bin/bik_player_audio.rs`:

```rust
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player};

/// One-second-or-more ring buffer per channel (interleaved). Tuned at
/// startup so we have headroom against egui repaint stalls.
const RING_TARGET_SAMPLES_PER_CHANNEL: usize = 32_768; // ~1.5s @ 22050 Hz

pub struct BinkAudioSink {
    _device: MixerDeviceSink,
    player: Player,
    ring: Arc<SpscRing>,
    sample_rate: u32,
    channels: u16,
}

impl BinkAudioSink {
    pub fn new(sample_rate: u32, channels: u16) -> Option<Self> {
        let device = DeviceSinkBuilder::open_default_sink()
            .map_err(|e| log::error!("bik-player: failed to open audio device: {}", e))
            .ok()?;
        let ring = SpscRing::new(RING_TARGET_SAMPLES_PER_CHANNEL * channels as usize);
        let source = BinkAudioSource::new(ring.clone(), sample_rate, channels)?;
        let player = Player::connect_new(device.mixer());
        player.set_volume(1.0);
        player.append(source);
        Some(Self {
            _device: device,
            player,
            ring,
            sample_rate,
            channels,
        })
    }

    /// Push samples into the ring. Returns count actually pushed (caller can ignore overflow).
    pub fn push(&self, samples: &[f32]) -> usize {
        self.ring.push(samples)
    }

    /// Clear the ring (for seek). Safe to call only when paused.
    pub fn drain(&self) {
        self.ring.drain();
    }

    pub fn pause(&self) { self.player.pause(); }
    pub fn resume(&self) { self.player.play(); }

    pub fn position(&self) -> Duration { self.player.get_pos() }

    pub fn set_volume(&self, v: f32) { self.player.set_volume(v.clamp(0.0, 1.0)); }

    pub fn sample_rate(&self) -> u32 { self.sample_rate }
    pub fn channels(&self) -> u16 { self.channels }
}
```

**Step 2: Verify**
Run: `cargo check --bin bik-player`
Expected: clean. If `Player::get_pos()` doesn't exist in rodio 0.22, fall back to tracking `samples_pushed / sample_rate` manually on the sink (set a separate `AtomicU64` counter).

**Step 3: Commit**
Message: `bik-player: BinkAudioSink owning device + player + ring`

---

### Task 19: Add audio fields to `BikPlayerApp` + instantiate on file load

**Why:** Wires the new types into the app struct. Audio init happens after Bink parse so we know the track parameters; gracefully degrades to "no audio" if device open fails.

**Files:**
- Modify: `src/bin/bik-player.rs`

**Pattern:** Mirrors how `decoder` is initialized after `BinkFile::parse` succeeds.

**Step 1: Import new types**

In `src/bin/bik-player.rs` near the other `use` statements, add:

```rust
use vera20k::assets::bink_audio::BinkAudioDecoder;
```

And inside the `mod` block alongside `mod bik_player_audio;` from Task 16, you can now use it.

**Step 2: Add fields**

In `pub struct BikPlayerApp`, after `pub decoder: Option<BinkDecoder>,` add:

```rust
    pub audio_decoder: Option<BinkAudioDecoder>,
    pub audio_sink: Option<bik_player_audio::BinkAudioSink>,
    pub audio_volume: f32,
```

**Step 3: Initialize in `new()`**

In `BikPlayerApp::new`, in the struct literal (after `decoder: None,`), add:

```rust
            audio_decoder: None,
            audio_sink: None,
            audio_volume: 0.7,
```

**Step 4: Instantiate audio in `load_bytes`**

Modify [src/bin/bik-player.rs:102-128](src/bin/bik-player.rs#L102-L128) `load_bytes`. After successful `BinkDecoder::new`, add:

```rust
                    // Tear down any previous audio.
                    self.audio_decoder = None;
                    self.audio_sink = None;
                    if let Some(track) = file.header.audio_tracks.first().copied() {
                        match BinkAudioDecoder::new(track) {
                            Ok(d) => {
                                let sr = d.sample_rate();
                                let ch = d.channels();
                                self.audio_sink = bik_player_audio::BinkAudioSink::new(sr, ch);
                                if let Some(s) = &self.audio_sink {
                                    s.set_volume(self.audio_volume);
                                }
                                self.audio_decoder = Some(d);
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
```

Place this immediately after the `self.status = format!(...)` line and before `self.file = Some(file);`.

**Step 5: Verify**
Run: `cargo check --bin bik-player`
Expected: clean.

**Step 6: Commit**
Message: `bik-player: instantiate BinkAudioDecoder + sink on file load`

---

### Task 20: Push audio samples per advanced video frame in `Playback::step()`

**Why:** This is where audio actually starts playing. Each time the playback loop advances a video frame, we decode the matching audio packet and push it to the sink. Mute-friendly: if no audio decoder/sink, this is a no-op.

**Files:**
- Modify: `src/bin/bik_player_playback.rs`

**Pattern:** Extends the existing `step()` loop in [src/bin/bik_player_playback.rs:50-72](src/bin/bik_player_playback.rs#L50-L72).

**Step 1: Change `step()` signature**

Replace the current `step()` signature with one that accepts the audio decoder + sink:

```rust
    pub fn step(
        &mut self,
        file: &BinkFile,
        decoder: &mut BinkDecoder,
        audio_decoder: Option<&mut vera20k::assets::bink_audio::BinkAudioDecoder>,
        audio_sink: Option<&crate::bik_player_audio::BinkAudioSink>,
        current_frame: &mut usize,
        status: &mut String,
    ) {
```

**Step 2: Push audio inside the per-frame loop**

Inside the `while self.accumulator_secs >= frame_dt` loop, after the successful `decoder.decode_frame(pkt)` call but before `*current_frame += 1`, insert:

```rust
                    if let (Some(adec), Some(sink)) = (audio_decoder.as_deref_mut(), audio_sink) {
                        match file.audio_packets(*current_frame) {
                            Ok(pkts) => {
                                for ap in pkts {
                                    if ap.track_index != 0 { continue; }
                                    match adec.decode_packet(ap.bytes) {
                                        Ok(samples) => { sink.push(&samples); }
                                        Err(e) => {
                                            log::warn!("audio decode error frame {}: {}", *current_frame, e);
                                        }
                                    }
                                }
                            }
                            Err(e) => log::warn!("audio packet error frame {}: {}", *current_frame, e),
                        }
                    }
```

Note: the `as_deref_mut` Option chain doesn't work directly because `audio_decoder` is `Option<&mut BinkAudioDecoder>`. Use a different pattern:

```rust
                    if let Some(sink) = audio_sink {
                        if let Some(adec) = audio_decoder.as_deref_mut() {
                            match file.audio_packets(*current_frame) { /* as above */ }
                        }
                    }
```

The exact borrow-juggling will reveal itself at compile — wrap in a helper if needed.

Wait — the current `current_frame` value in the loop refers to the frame about to be decoded (it gets incremented after). The audio packet for that frame should be pushed BEFORE the increment, which is what the placement above does. Confirm by reading the loop body context.

**Step 3: Update the call site in `bik-player.rs`**

In [src/bin/bik-player.rs:156-163](src/bin/bik-player.rs#L156-L163), update the `playback.step(...)` call:

```rust
            self.playback.step(
                file,
                decoder,
                self.audio_decoder.as_mut(),
                self.audio_sink.as_ref(),
                &mut self.current_frame,
                &mut self.status,
            );
```

**Step 4: Verify**
Run: `cargo check --bin bik-player`
Expected: clean. Borrow checker may complain about overlapping `&mut self.audio_decoder` and `&mut self.current_frame` if accessed in the wrong order — destructure `self` if so, or split into local bindings.

**Step 5: Commit**
Message: `bik-player: push audio samples per advanced video frame`

---

### Task 21: Add A/V drift correction every 10 ticks

**Why:** Without correction, clock skew accumulates. The `Playback` already runs every UI tick; we add a counter and every 10th tick compare audio position to video position, skipping or stalling one frame as needed.

**Files:**
- Modify: `src/bin/bik_player_playback.rs`

**Pattern:** Pure addition — no precedent in repo for A/V sync.

**Step 1: Add tick counter to `Playback`**

In `src/bin/bik_player_playback.rs`:

```rust
pub struct Playback {
    pub playing: bool,
    pub last_tick: Instant,
    pub accumulator_secs: f64,
    pub speed: f32,
    /// Counts UI ticks; drift check fires every `DRIFT_CHECK_INTERVAL` ticks.
    pub tick_counter: u32,
}
```

Update `Default` impl to set `tick_counter: 0`.

Add constant near the top:

```rust
/// Audio/video drift check cadence (UI ticks per check).
const DRIFT_CHECK_INTERVAL: u32 = 10;
```

**Step 2: Add drift-check logic**

At the end of `step()` (after the `while` loop), insert:

```rust
        self.tick_counter = self.tick_counter.wrapping_add(1);
        if self.tick_counter % DRIFT_CHECK_INTERVAL == 0 {
            if let Some(sink) = audio_sink {
                let audio_secs = sink.position().as_secs_f64();
                let video_secs = (*current_frame as f64) / fps;
                let drift = audio_secs - video_secs;
                if drift > frame_dt && *current_frame + 1 < file.frame_index.len() {
                    // Audio is ahead — skip one video frame.
                    let skip_pkt = file.video_packet(*current_frame);
                    if let Ok(pkt) = skip_pkt {
                        let _ = decoder.decode_frame(pkt);
                        *current_frame += 1;
                    }
                } else if drift < -frame_dt {
                    // Video is ahead — stall by absorbing one frame's accumulator.
                    self.accumulator_secs -= frame_dt;
                }
            }
        }
```

**Step 3: Verify**
Run: `cargo check --bin bik-player`
Expected: clean.

**Step 4: Commit**
Message: `bik-player: A/V drift correction every 10 ticks`

---

### Task 22: Reset audio decoder + drain sink in `seek_to_frame`

**Why:** Seek must reset audio state or the post-seek audio is stale (clicks, mis-aligned overlap). Mirror video's flush-and-replay pattern.

**Files:**
- Modify: `src/bin/bik_player_playback.rs`

**Pattern:** Mirrors the existing video reset in [src/bin/bik_player_playback.rs:77-102](src/bin/bik_player_playback.rs#L77-L102).

**Step 1: Update `seek_to_frame`**

In `seek_to_frame`, after the existing `decoder.flush()` line (around line 95), add:

```rust
    if let Some(adec) = app.audio_decoder.as_mut() {
        adec.reset();
    }
    if let Some(sink) = app.audio_sink.as_ref() {
        sink.pause();
        sink.drain();
    }
```

After the for-loop that decodes video from the keyframe, before `app.current_frame = target + 1`, push the audio for the seeked-to range:

```rust
    if let (Some(adec), Some(sink)) = (app.audio_decoder.as_mut(), app.audio_sink.as_ref()) {
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
```

**Step 2: Verify**
Run: `cargo check --bin bik-player`
Expected: clean. May need to re-borrow `file` after the `app.audio_decoder.as_mut()` borrow — adjust scope or destructure as needed.

**Step 3: Commit**
Message: `bik-player: reset audio + drain sink on seek`

---

### Task 23: Wire pause/resume to the audio sink

**Why:** Without this, audio keeps playing after the user clicks Pause. Simple forward.

**Files:**
- Modify: `src/bin/bik-player.rs`

**Pattern:** None new — single-line pass-throughs.

**Step 1: Wrap the play/pause toggle**

In `src/bin/bik-player.rs:197-203`, replace the existing toggle:

```rust
                if ui
                    .button(if self.playback.playing { "Pause" } else { "Play" })
                    .clicked()
                {
                    self.playback.playing = !self.playback.playing;
                    if let Some(sink) = self.audio_sink.as_ref() {
                        if self.playback.playing { sink.resume(); } else { sink.pause(); }
                    }
                }
```

**Step 2: Verify**
Run: `cargo check --bin bik-player`
Expected: clean.

**Step 3: Commit**
Message: `bik-player: forward play/pause to audio sink`

---

### Task 24: Add volume slider + mute button to top panel

**Why:** Minimal UX so the user can confirm audio works without their system volume blasting at full.

**Files:**
- Modify: `src/bin/bik_player_ui.rs`
- Modify: `src/bin/bik-player.rs`

**Pattern:** Standard egui `ui.add(egui::Slider::new(...))` and `ui.button(...)`.

**Step 1: Add to `draw_top_panel`**

In [src/bin/bik_player_ui.rs](src/bin/bik_player_ui.rs) `draw_top_panel`, after the asset name input, add inside the same `ui.horizontal`:

```rust
            ui.separator();
            ui.label("Vol");
            let mut v = app.audio_volume;
            if ui.add(egui::Slider::new(&mut v, 0.0..=1.0).show_value(false)).changed() {
                app.audio_volume = v;
                if let Some(sink) = app.audio_sink.as_ref() {
                    sink.set_volume(v);
                }
            }
            if ui.button(if app.audio_volume > 0.0 { "Mute" } else { "Unmute" }).clicked() {
                if app.audio_volume > 0.0 {
                    app.audio_volume = 0.0;
                } else {
                    app.audio_volume = 0.7;
                }
                if let Some(sink) = app.audio_sink.as_ref() {
                    sink.set_volume(app.audio_volume);
                }
            }
```

**Step 2: Verify**
Run: `cargo check --bin bik-player`
Expected: clean.

**Step 3: Commit**
Message: `bik-player: volume slider + mute toggle in top panel`

---

### Task 25: Manual smoke test — play a real RA2 cutscene

**Why:** End-to-end validation. Until we hear audio coming out of speakers in sync with video on a real RA2 file, none of the previous tests prove the user-facing feature works.

**Files:** none.

**Step 1: Build and run**

```
cargo run --release --bin bik-player
```

**Step 2: Pick a known cutscene**

In the asset-picker ComboBox, select a known dialogue-heavy cutscene — examples from RA2:
- `ALLIEND1.BIK` (Allied ending)
- `SOVIENT.BIK` (Soviet intro)
- Any `*md*.BIK` from `movmd03.mix` for YR-specific content

Click Play.

**Step 3: Verify by ear and eye**

- ✅ Video plays at correct framerate (no obvious slow-down or speed-up).
- ✅ Audio plays at correct pitch (not chipmunk, not deep).
- ✅ Audio is NOT silent.
- ✅ Audio is NOT obviously distorted (clipping, crackling).
- ✅ Lip-sync is acceptable (within ~100 ms — drift correction should keep it tighter).
- ✅ Pause halts both audio and video; Resume continues from the same position.
- ✅ Seek (drag the timeline scrubber): video jumps to new position; audio resumes cleanly within ~half a second.
- ✅ Pick a different cutscene from the ComboBox: previous audio stops cleanly; new file plays from the start with no leftover audio.
- ✅ Volume slider audibly affects loudness; Mute silences audio without stopping video.
- ✅ Closing the window doesn't hang or crash.

If any of these fail, file a follow-up task and iterate. Most likely failure modes:
- Wrong audio (pitch/distortion): bug in transform — bisect against oracle from Task 15.
- Audio cuts out mid-playback: ring-buffer underrun — increase `RING_TARGET_SAMPLES_PER_CHANNEL` in Task 18.
- Drift visibly noticeable: tune `DRIFT_CHECK_INTERVAL` lower (more frequent checks) in Task 21.
- Seek-resume click: confirm `sink.drain()` is called before pushing new samples in Task 22.

**Step 4: No commit unless code changes**
Pure verification step.

---

### Task 26: Final cargo check + full test suite + commit

**Why:** Verify nothing else regressed. Snapshot a clean state at the end of the PR.

**Files:** none.

**Step 1: Run full suite**

```
cargo check
cargo test -p vera20k --lib
cargo test --bin bik-player
cargo test --test bink_audio_samples
cargo test --test bink_first_frame
cargo test --test bink_frame_diff
```

Expected: all green. Audio integration test passes (or SKIPs if fixture missing — should NOT SKIP if you ran Task 15).

**Step 2: Verify clean working tree**

```
git status
```

Expected: clean.

**Step 3: Push branch (do not open PR — per brief)**

The brief says "Don't push or open the PR. Stop at the last commit and report." So do NOT run `git push`. Just report completion.

---

## Sources & References

- **Design doc:** [docs/plans/2026-04-22-bink-audio-design.md](docs/plans/2026-04-22-bink-audio-design.md)
- **PR-1 design (parent):** [docs/plans/2026-04-22-bink-decoder-and-player.md](docs/plans/2026-04-22-bink-decoder-and-player.md)
- **FFmpeg sources** (read-only, pinned to commit `9acd820732f0bf738bd743bbde6a5c3eadc216c2`):
  - [`libavcodec/binkaudio.c`](C:/Users/enok/Documents/FFmpeg/libavcodec/binkaudio.c) — entire decoder
  - [`libavcodec/wma_freqs.c`](C:/Users/enok/Documents/FFmpeg/libavcodec/wma_freqs.c) — WMA critical frequencies
- **Repo patterns:**
  - [src/assets/bink_decode.rs:15-139](src/assets/bink_decode.rs#L15-L139) — hand-rolled video IDCT precedent
  - [src/audio/music.rs:36-69](src/audio/music.rs#L36-L69) — rodio `MusicPlayer` initialization pattern
  - [src/audio/sfx.rs:115-145](src/audio/sfx.rs#L115-L145) — rodio `SfxPlayer` initialization pattern
  - [tests/bink_first_frame.rs:14-77](tests/bink_first_frame.rs#L14-L77) — SKIP-if-missing integration test pattern
- **Bink format docs:**
  - [Bink Container](https://wiki.multimedia.cx/index.php?title=Bink_Container) (cited in FFmpeg headers)
  - [Bink Audio](https://wiki.multimedia.cx/index.php?title=Bink_Audio) (cited in FFmpeg headers)
- **gamemd.exe addresses:** N/A — Bink is BINKW32.DLL, closed source, not in gamemd.exe.
- **INI keys:** N/A — Bink Audio is a binary format with no INI configuration.
