// Ported from FFmpeg's libavcodec/bink.c and libavcodec/binkdsp.c.
// Copyright (c) 2009 Konstantin Shishkov
// Copyright (c) 2011 Peter Ross <pross@xvid.org>
// Licensed LGPL-2.1-or-later. See LICENSES/LGPL-2.1-or-later.txt in the repo root.

//! Bink 1 video decoder.
//!
//! Decodes one video packet at a time into a YUV420P frame. Supports BIKi and
//! BIKk variants. B-frames (BIKb), Bink 2 (KB2), alpha, and grayscale paths
//! are not implemented — not used by RA2 / YR cutscenes.
//!
//! ## Dependency rules
//! - Part of assets/ — no dependencies on game modules.

// (empty — implementation in Tasks 11-32)
