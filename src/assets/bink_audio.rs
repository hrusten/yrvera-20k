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
