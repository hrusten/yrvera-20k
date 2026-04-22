// Ported from FFmpeg's libavformat/bink.c.
// Copyright (c) 2008-2010 Peter Ross (pross@xvid.org)
// Copyright (c) 2009 Daniel Verkamp (daniel@drv.nu)
// Licensed LGPL-2.1-or-later. See LICENSES/LGPL-2.1-or-later.txt in the repo root.

//! Bink 1 container demuxer.
//!
//! Parses the fixed header, audio track descriptors, per-frame offset table,
//! and splits each frame packet into its audio blocks + video bitstream.
//!
//! Only BIKi and BIKk revisions are supported — the only variants that ship
//! in RA2 / Yuri's Revenge cutscenes.
//!
//! ## Dependency rules
//! - Part of assets/ — no dependencies on game modules.
//! - Uses util/read_helpers for binary reading.

// (empty — implementation in Tasks 6-10)
