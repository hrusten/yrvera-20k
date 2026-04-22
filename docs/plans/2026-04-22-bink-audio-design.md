# Bink Audio Decoder + bik-player Integration — Design

## Goal

Decode Bink Audio (DCT and RDFT variants) from `.bik` files and play it back, in
sync with video, inside the existing `bik-player` tool binary — using zero new
dependencies.

## Architecture Context

The Bink container side is already done in PR 2/3:

- [src/assets/bink_file.rs:99-123](src/assets/bink_file.rs#L99-L123) parses
  `AudioTrack { sample_rate, flags, track_id }`. Flag bits give us
  `is_stereo()`, `is_16bit()`, `uses_dct()`.
- [src/assets/bink_file.rs:403-445](src/assets/bink_file.rs#L403-L445) exposes
  `audio_packets(i) -> Vec<AudioPacket<'_>>` — per-frame, per-track audio
  bytes plus a 4-byte `sample_count` header.
- [src/bin/bik_player_playback.rs:28-72](src/bin/bik_player_playback.rs#L28-L72)
  drives the playback loop: `accumulator_secs += dt`, decode video frames at
  the file's nominal FPS, set `current_frame += 1`. Audio hooks in here.

The existing rodio usage pattern in
[src/audio/music.rs:17-21](src/audio/music.rs#L17-L21) and
[src/audio/sfx.rs:23-24](src/audio/sfx.rs#L23-L24) is:
`MixerDeviceSink` owned on the player struct, `Player::connect_new(device.mixer())`
per source, `Player::append(impl Source)`. Both currently use the all-at-once
`SamplesBuffer<f32>`. For movie playback we need a streaming `rodio::Source`.

**Test fixture pattern** ([tests/bink_first_frame.rs:14-77](tests/bink_first_frame.rs#L14-L77)):
SKIP-if-missing, byte-exact compare against an FFmpeg-generated YUV oracle. We
mirror this for audio with an FFmpeg-generated `f32le` PCM oracle and a
tight-epsilon compare.

**FFmpeg reference** is `libavcodec/binkaudio.c` (398 lines, ~250 lines of
meat after stripping codec-registration boilerplate). The transforms it calls
into (`AV_TX_FLOAT_RDFT`, `AV_TX_FLOAT_DCT`) live one layer below in
`libavutil/tx_float.c` — a heavily-templated SIMD/split-radix implementation
that does *not* port cleanly. We write our own radix-2 FFT plus standard
RDFT/DCT-II reductions instead.

**Project precedent** for hand-rolled transforms:
[src/assets/bink_decode.rs:15-139](src/assets/bink_decode.rs#L15-L139) already
implements the 8×8 video IDCT from scratch (~125 lines of integer arithmetic).
The audio transforms extend that pattern.

## Impact Analysis

**New files**

| File | Purpose | Est. size |
|---|---|---|
| `src/assets/bink_audio.rs` | Decoder + FFT/RDFT/DCT primitives | ~550 L |
| `src/assets/bink_audio_data.rs` | WMA critical freqs, RLE length table | ~20 L |
| `src/bin/bik_player_audio.rs` | rodio sink + SPSC ring buffer + `Source` impl | ~200 L |
| `tests/bink_audio_samples.rs` | FFmpeg PCM oracle integration test | ~70 L |

**Modified files**

| File | Change |
|---|---|
| `src/assets/mod.rs` | Register `bink_audio`, `bink_audio_data` |
| `src/assets/error.rs` | Add `AssetError::BinkAudioError` variant |
| `src/bin/bik-player.rs` | Instantiate `BinkAudioSink` on file load; expose mute/volume |
| `src/bin/bik_player_playback.rs` | Push audio per frame + drift-check via `sink.position()` |
| `src/bin/bik_player_ui.rs` | (optional) Volume slider / mute button |
| `tests/fixtures/bink/README.md` | Add `ffmpeg -f f32le` oracle recipe |

**Dependencies:** zero new crates. Cargo.toml is unchanged.

**What depends on what we change:** nothing outside `src/bin/bik-player*` and
`src/assets/bink_*`. No `sim/`, `render/`, `ui/`, `app_*`. PR 5 (in-game
cutscene playback) will consume `BinkAudioDecoder` + `BinkAudioSink` — public
APIs must stay stable, but that boundary is natural.

**Determinism:** N/A. Audio is presentation-only, never enters sim state hash.

**Risk areas**

1. **FFT/RDFT/DCT correctness** (highest risk). Off-by-one in bit-reverse,
   wrong twiddle-factor sign, wrong scaling factor → silent garbage with no
   panic or compile error. Mitigated by FFmpeg PCM oracle + per-primitive unit
   tests (round-trip, known-answer).
2. **SPSC ring buffer ordering.** Atomic `head`/`tail` indices need correct
   `Acquire`/`Release` semantics across UI and cpal threads. Standard pattern
   but easy to subtly misorder. Mitigated by hand-rolling ~40 lines following
   the canonical SPSC shape; loom test optional later.
3. **Seek state reset.** Audio decoder's `prev[]` overlap tail and `first` flag
   must both reset on seek or the first post-seek block is audibly clicky.
   Mitigated by single `BinkAudioDecoder::reset()` method called from
   `seek_to_frame`.
4. **Sample-rate resampling.** rodio 0.22 resamples `Source` impls whose
   `sample_rate()` differs from device rate. Verified by existing `music.rs` /
   `sfx.rs` already relying on this — no special handling needed.

## Chosen Approach

The five-dimensional decision space (FFT-source × threading × sync-policy ×
test-oracle × multi-track) was narrowed via brainstorming to a single
configuration:

- **Zero new crates.** Roll a radix-2 FFT and standard RDFT/DCT-II reductions
  ourselves (~200 lines of math). Consistent with the existing hand-rolled
  video IDCT.
- **Decoder runs on UI thread** inside `Playback::step()`. SPSC ring buffer
  carries decoded samples to a custom `rodio::Source` consumed by rodio's
  cpal audio thread. Audio thread does no work beyond popping samples.
- **Video FPS drives the clock.** Drift is sampled every ~10 ticks via
  `rodio::Player::get_pos()`. If drift exceeds one frame period, skip or stall
  one video frame to correct.
- **Test oracle is two-layer:** unit tests for FFT/RDFT/DCT primitives
  (round-trip + known-answer), plus one integration test comparing full
  Bink-audio decode against `ffmpeg -c:a pcm_f32le -f f32le` output with
  tight epsilon (~1e-4).
- **Multi-track `.bik`:** decode track 0 only; warn-log additional tracks.
  RA2's 141 cutscenes are all single-track; if multi-track ever appears we
  can upgrade to a track picker in a follow-up.

## Design

### Components

#### `BinkAudioDecoder` (`src/assets/bink_audio.rs`)

```rust
pub struct BinkAudioDecoder {
    sample_rate: u32,
    channels: u16,
    use_dct: bool,
    version_b: bool,        // 'b' revision flag; always false in RA2
    frame_len: usize,       // 512 / 1024 / 2048
    overlap_len: usize,     // frame_len / 16
    num_bands: usize,
    bands: [u32; 26],       // band boundaries in coefficient index space
    quant_table: [f32; 96], // dequantization multipliers
    root: f32,              // global scale factor (DCT vs RDFT variant)
    first: bool,            // true until first block decoded (skip overlap-add)
    prev: Vec<Vec<f32>>,    // per-channel overlap tail buffer
    coeffs: Vec<f32>,       // scratch — length frame_len (+2 for RDFT slack)
    fft: Fft,               // pre-computed twiddle + bit-reverse tables
}
```

#### FFT primitives (private in `bink_audio.rs`)

```rust
struct Fft {
    n: usize,
    twiddles: Vec<(f32, f32)>,   // cos/sin precomputed
    bit_reverse: Vec<u32>,
}

impl Fft {
    fn new(n: usize) -> Self;            // n must be power of 2
    fn forward_inplace(&self, buf: &mut [Complex32]);
}

fn inverse_rdft(input: &[f32], output: &mut [f32], fft: &Fft);
fn inverse_dct_ii(input: &[f32], output: &mut [f32], fft: &Fft);
```

#### `BinkAudioSink` (`src/bin/bik_player_audio.rs`)

```rust
pub struct BinkAudioSink {
    _device: MixerDeviceSink,    // kept alive
    player: Player,
    producer: RingProducer<f32>, // owns the write half
    sample_rate: u32,
    channels: u16,
}
```

The SPSC ring buffer is a hand-rolled ~40-line `Arc<RingBuffer>` with
`Vec<UnsafeCell<f32>>` + two `AtomicUsize` indices, `Acquire`/`Release`
ordering. The consumer side lives inside a `BinkAudioSource` struct
implementing `rodio::Source` + `Iterator<Item = f32>` — `next()` pops one
sample atomically; returns `Some(0.0)` if empty (output silence, never
stall the audio thread).

### Interfaces / Contracts

```rust
impl BinkAudioDecoder {
    pub fn new(track: AudioTrack) -> Result<Self, AssetError>;
    /// Decode one audio packet; returns (frame_len - overlap_len) * channels
    /// samples interleaved (stereo) or per-channel (mono).
    pub fn decode_packet(&mut self, bytes: &[u8]) -> Result<Vec<f32>, AssetError>;
    pub fn sample_rate(&self) -> u32;
    pub fn channels(&self) -> u16;
    pub fn frame_len(&self) -> usize;
    pub fn use_dct(&self) -> bool;
    pub fn reset(&mut self); // clears prev[] and sets first = true
}

impl BinkAudioSink {
    pub fn new(sample_rate: u32, channels: u16) -> Option<Self>;
    pub fn push(&mut self, samples: &[f32]) -> usize; // nonblocking; returns written count
    pub fn drain(&mut self);  // for seek
    pub fn pause(&self);
    pub fn resume(&self);
    pub fn position(&self) -> Duration;  // audio playback clock
    pub fn set_volume(&self, v: f32);
}
```

### Data Flow

**Per playback tick:**

```
Playback::step():
  for each video frame to advance:
    video_pkt = file.video_packet(i)
    video_dec.decode_frame(video_pkt)
    for ap in file.audio_packets(i):
      if ap.track_index == 0:
        samples = audio_dec.decode_packet(ap.bytes)?
        sink.push(&samples)     // silently drops if buffer full
    current_frame += 1

  if tick_counter % 10 == 0:
    drift = sink.position().as_secs_f64() - (current_frame as f64 / fps)
    if drift >  frame_dt:  current_frame += 1               // skip one video frame
    if drift < -frame_dt:  self.accumulator -= frame_dt     // stall one tick
```

**Lifecycle:**

| Event | Action |
|---|---|
| File load | New `BinkAudioDecoder` + new `BinkAudioSink` (old dropped, Player stops) |
| Seek | `audio_dec.reset()` + `sink.drain()`, push fresh samples from new position |
| Pause | `sink.pause()`; decoder state untouched |
| Resume | `sink.resume()` |
| File change | Drop old sink entirely; new one constructed |
| App close | Sink drop → Player stop → cpal thread exits |

### Error Handling

- Add `AssetError::BinkAudioError { reason: String }` mirroring existing
  `BinkError`.
- Truncated packets, invalid band/quantizer indices, unsupported FFT sizes →
  early `Err`.
- `BinkAudioSink::new` returns `None` on device-open failure.
  `BikPlayerApp` stores it as `Option<BinkAudioSink>`. Status label shows
  "(audio unavailable)". Video keeps playing silently.

### Testing Strategy

**Unit tests in `bink_audio.rs`:**

- `fft_round_trip_preserves_impulse` — forward + conjugate + forward on an
  impulse at sizes 256 / 512 / 1024 reproduces input within 1e-6.
- `rdft_sine_peak_in_correct_bin` — RDFT of a pure 440 Hz sine at 22050 Hz
  has its peak at `round(440 * N / 22050)`.
- `idct_constant_input_gives_constant_output` — DC-only input yields uniform
  output scaled by expected factor.
- `overlap_add_first_flag_skips_crossfade` — two consecutive identical
  packets with `first=true` then `first=false` produce identical output for
  the shared overlap region.
- `decode_silent_packet_yields_silence` — packet with all-zero quantized
  bands decodes to silence (within overlap tail).

**Integration test `tests/bink_audio_samples.rs`:**

```
fixture = tests/fixtures/bink/fixture.bik
oracle  = tests/fixtures/bink/fixture_audio.f32  (FFmpeg-produced)

decode all audio packets from frame 0..N of fixture
for each (ours, theirs) pair in interleaved samples:
  assert |ours - theirs| < 1e-4  (peak error)
also assert RMS error < 1e-5

SKIP if either fixture missing
```

**Test fixture recipe** (added to `tests/fixtures/bink/README.md`):

```
ffmpeg -i fixture.bik -c:a pcm_f32le -f f32le fixture_audio.f32
```

We need fixtures for both transform variants — one DCT-flag .bik and one
RDFT-flag .bik. The RA2 inventory likely uses RDFT for all 141 files; we
may need to synthesize a DCT fixture or pull one from another Bink source.

## Architectural Decisions

**Patterns followed:**

- Module naming `bink_audio.rs` / `bink_audio_data.rs` matches the existing
  `bink_file.rs` / `bink_decode.rs` / `bink_bits.rs` / `bink_data.rs` quartet.
- Layering: `src/assets/bink_audio.rs` imports only `src/assets/`,
  `src/util/`, and standard lib. Zero `sim/`, `render/`, `ui/`, `app_*`
  imports — matches the asset-layer invariant.
- rodio usage in `BinkAudioSink` mirrors `MusicPlayer` and `SfxPlayer`:
  owns a `MixerDeviceSink`, connects a `Player`, set_volume passes through.
- Test pattern mirrors `tests/bink_first_frame.rs` (SKIP-if-missing, tight
  numerical compare, fixtures documented in `tests/fixtures/bink/README.md`).
- Hand-rolled transform math follows the precedent of
  [bink_decode.rs:15-139](src/assets/bink_decode.rs#L15-L139)'s 8×8 video
  IDCT.

**Patterns deviated from (and why):**

- Custom `rodio::Source` impl instead of `SamplesBuffer<f32>`. Required
  because we're streaming, not playing a fixed sample array. Documented in
  the module header. First time this pattern appears in the codebase but
  standard for streaming audio.
- Hand-rolled SPSC ring buffer instead of using a crate. Same dep-footprint
  rationale that drives the no-FFT-crate decision: ~40 lines of
  well-trodden code, fully testable.

**Tech debt introduced:** none. The custom Source and SPSC ring buffer are
narrow, self-contained, and have no foreseen rework.

## Alternatives Considered

| Decision | Alternative | Rejected because |
|---|---|---|
| FFT crates | Add `rustfft` + `realfft` + `rustdct` | Three new deps for ~200 lines of math we can write ourselves; precedent of hand-rolled video IDCT |
| FFT crates | Add only `rustfft`, hand-roll RDFT/DCT on top | Saves only one crate vs three; same code-writing cost as the no-crate option |
| Threading | Decode on rodio's audio (cpal) thread | RT-safety violation; FFT spikes risk dropouts |
| Threading | Dedicated decoder thread + condvar | Over-engineering for a tool binary; thread lifecycle adds debug surface |
| A/V sync | Audio-master clock (mpv-style) | Larger rewrite of `Playback::step()`; fragile during pause/buffering startup |
| A/V sync | No drift correction at all | Drift accumulates; unacceptable for movie playback |
| Test oracle | Numerical sanity checks only (no FFmpeg compare) | Misses subtle bugs; weaker than existing video test rigor |
| Multi-track | Mix all tracks together | RA2 doesn't have multi-track files; YAGNI |
| Multi-track | UI track picker | Same — no demand exists |

## Parity Note

gamemd.exe uses BINKW32.DLL (RAD Game Tools, closed source). FFmpeg's
binkaudio decoder has been community-validated against BINKW32.DLL output
since ~2010. Matching FFmpeg output bit-close (within float tolerance) is
the most rigorous parity bar achievable without RE'ing the DLL. Passing
the integration test = matching original Bink playback to within
inaudible-to-humans precision.

## Out of Scope (deferred)

- In-game cutscene integration (PR 5 per the PR-1 design doc §6.4).
- Bink Audio v'b' variant (`version_b == true`). Not present in RA2.
- BIKb container variant. Not present in RA2.
- Per-track UI controls. Single-track is the only RA2 case.
