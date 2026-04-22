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
/// post-shuffle layout. `fft` must be an FFT instance sized `n / 2` (not `n`).
///
/// Output: `frame_len` real samples in `output[0..frame_len]`.
fn inverse_rdft(input: &[f32], output: &mut [f32], fft: &Fft) {
    let half = fft.n;
    let n = half * 2;
    assert_eq!(input.len(), n + 2);
    assert_eq!(output.len(), n);

    // Build the half-size complex spectrum from the real-input layout.
    // Split an N-point real IDFT into an (N/2)-point complex IDFT:
    //   x[2m]   = Re(y[m]), x[2m+1] = Im(y[m])
    // where y[m] = IDFT_{N/2}(G[k] + j * W[k] * H[k]), with
    //   G[k] = X[k] + conj(X[N/2-k])
    //   H[k] = X[k] - conj(X[N/2-k])
    //   W[k] = exp(2πi k / N)
    // This gives an un-normalized result (no 1/N).
    let mut buf = vec![Complex32::default(); half];

    let dc = input[0];
    let nyq = input[n];
    // Bin 0: G = X[0] + X[N/2] (both real); H = X[0] - X[N/2]; W = 1, so j*W*H = j*(dc-nyq).
    buf[0] = Complex32::new(dc + nyq, dc - nyq);

    for m in 1..half {
        let xr = input[2 * m];
        let xi = input[2 * m + 1];
        let yr = input[2 * (half - m)];
        let yi = input[2 * (half - m) + 1];
        // Pre-twiddle: W[m] = exp(2πi m / N) = exp(j * π * m / (N/2)).
        let theta = std::f32::consts::PI * (m as f32) / (half as f32);
        let wr = theta.cos();
        let wi = theta.sin();

        // G[m] = X[m] + conj(X[N/2-m]) = (xr + yr) + j*(xi - yi)
        let gr = xr + yr;
        let gi = xi - yi;
        // H[m] = X[m] - conj(X[N/2-m]) = (xr - yr) + j*(xi + yi)
        let hr = xr - yr;
        let hi = xi + yi;
        // j*W*H = j*(wr + j*wi)*(hr + j*hi)
        //       = -(wr*hi + wi*hr) + j*(wr*hr - wi*hi)
        let jwhr = -(wr * hi + wi * hr);
        let jwhi = wr * hr - wi * hi;

        buf[m] = Complex32::new(gr + jwhr, gi + jwhi);
    }

    // Un-normalized inverse complex FFT of size half (conjugate-forward-conjugate).
    // We deliberately omit the 1/half scaling: FFmpeg's AV_TX_FLOAT_RDFT produces
    // un-normalized output too, and the decoder's `root` factor absorbs the
    // per-sample normalization. The remaining overall 0.5 scaling vs. FFmpeg's
    // scale=0.5 convention is applied in the dispatch.
    for c in buf.iter_mut() {
        c.im = -c.im;
    }
    fft.forward_inplace(&mut buf);
    for c in buf.iter_mut() {
        c.im = -c.im;
    }

    // Unpack: output[2m] = re, output[2m+1] = im.
    for m in 0..half {
        output[2 * m] = buf[m].re;
        output[2 * m + 1] = buf[m].im;
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
}
