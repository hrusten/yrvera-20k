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
