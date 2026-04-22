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
