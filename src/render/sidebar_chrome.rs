//! Sidebar chrome atlas — loads all original RA2 sidebar art pieces from
//! theme-specific MIX archives (sidec01/02/02md) and packs them into a single
//! GPU texture for efficient batched rendering.
//!
//! ## Art pieces loaded (from sidec0x.mix)
//! - radar.shp  (168x110, 33 frames) — radar minimap frame
//! - side1.shp  (168x69)  — top header (credits/power area)
//! - tabs.shp   (168x16)  — tab strip background
//! - tab00-03.shp (28x27 each, 5 frames) — individual tab buttons
//! - side2.shp  (168x50)  — repeating middle (cameo row background)
//! - side3.shp  (168x26)  — bottom footer
//! - repair.shp (64x31)   — repair button
//! - sell.shp   (64x31)   — sell button
//! - power.shp  (27x30)   — power indicator

use crate::assets::asset_manager::AssetManager;
use crate::assets::mix_archive::MixArchive;
use crate::assets::mix_hash::mix_hash;
use crate::assets::pal_file::Palette;
use crate::assets::shp_file::ShpFile;
use crate::render::batch::{BatchRenderer, BatchTexture};
use crate::render::gpu::GpuContext;

const CHROME_PADDING: u32 = 2;
// Frame 0 is the intact stock radar shell. Frame 32 is effectively a blank
// inner state and makes the top cap look missing when used as the default.
const RADAR_DEFAULT_FRAME: usize = 0;
const TOP_STRIP_LEFT_ID: i32 = 0xD508C1A4u32 as i32;
const TOP_STRIP_SIDEBAR_ID: i32 = 0xF0F1CE8Du32 as i32;
const TOP_STRIP_THIN_ID: i32 = 0x7637D6E1u32 as i32;
const UNKNOWN_TOP_HOUSING_ID: i32 = 0x7AEBAE6Bu32 as i32;
const UNKNOWN_MID_PANEL_ID: i32 = 0xB0259C24u32 as i32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarTheme {
    Allied,
    Soviet,
    Yuri,
}

/// UV coordinates and pixel dimensions for one chrome piece in the atlas.
#[derive(Debug, Clone, Copy)]
pub struct SidebarChromeEntry {
    pub uv_origin: [f32; 2],
    pub uv_size: [f32; 2],
    pub pixel_size: [f32; 2],
}

#[derive(Debug, Clone)]
pub struct SidebarChromeExtraEntry {
    pub id: i32,
    pub label: String,
    pub frame_count: usize,
    pub entry: SidebarChromeEntry,
}

/// All sidebar chrome art for one faction theme, packed into a single texture.
pub struct SidebarChromeAtlas {
    pub texture: BatchTexture,
    pub top_strip_left: Option<SidebarChromeEntry>,
    pub top_strip_sidebar: Option<SidebarChromeEntry>,
    pub top_strip_thin: Option<SidebarChromeEntry>,
    pub unknown_top_housing: Option<SidebarChromeEntry>,
    pub unknown_mid_panel: Option<SidebarChromeEntry>,
    pub background_large: Option<SidebarChromeEntry>,
    pub background_medium: Option<SidebarChromeEntry>,
    pub background_small: Option<SidebarChromeEntry>,
    /// All pre-rendered RGBA frames of radar.shp for the opening/closing animation.
    /// Frame 0 = fully open radar housing, last frame = fully closed.
    pub radar_frames: Vec<Vec<u8>>,
    /// Pixel dimensions of each radar frame (width, height).
    pub radar_frame_size: [u32; 2],
    /// Content insets derived from the transparent opening in radar.shp frame 0.
    /// [left, top, right, bottom] in unscaled pixels. The minimap fills the area
    /// inside these insets. Derived automatically — no manual tuning needed.
    pub radar_content_insets: [u32; 4],
    pub radar: SidebarChromeEntry,
    pub side1: SidebarChromeEntry,
    pub tabs: Option<SidebarChromeEntry>,
    /// Tab buttons: tab00 through tab03, inactive (frame 0) state.
    pub tab_buttons: Vec<SidebarChromeEntry>,
    /// Tab buttons: tab00 through tab03, active/selected (frame 3) state.
    pub tab_buttons_active: Vec<SidebarChromeEntry>,
    pub side2: SidebarChromeEntry,
    pub side3: SidebarChromeEntry,
    pub repair: Option<SidebarChromeEntry>,
    pub sell: Option<SidebarChromeEntry>,
    pub power: Option<SidebarChromeEntry>,
    /// powerp.shp frames: 5 colored strip segments for the power bar meter.
    /// [0]=dark/bg, [1]=green, [2]=yellow, [3]=red, [4]=dark/off.
    pub powerp_frames: [Option<SidebarChromeEntry>; 5],
    pub extra_entries: Vec<SidebarChromeExtraEntry>,
}

pub struct SidebarChromeSet {
    pub allied: Option<SidebarChromeAtlas>,
    pub soviet: Option<SidebarChromeAtlas>,
    pub yuri: Option<SidebarChromeAtlas>,
}

impl SidebarChromeSet {
    pub fn for_theme(&self, theme: SidebarTheme) -> Option<&SidebarChromeAtlas> {
        match theme {
            SidebarTheme::Allied => self.allied.as_ref(),
            SidebarTheme::Soviet => self.soviet.as_ref().or(self.allied.as_ref()),
            SidebarTheme::Yuri => self
                .yuri
                .as_ref()
                .or(self.soviet.as_ref())
                .or(self.allied.as_ref()),
        }
    }
}

pub fn build_sidebar_chrome_set(
    gpu: &GpuContext,
    batch: &BatchRenderer,
    asset_manager: &AssetManager,
) -> Option<SidebarChromeSet> {
    let allied = build_theme_atlas(
        gpu,
        batch,
        asset_manager,
        "sidec01.mix",
        "sidebar.pal",
        "radar.shp",
        Some(("bkgdlg.shp", "bkgdmd.shp", "bkgdsm.shp")),
    );
    let soviet = build_theme_atlas(
        gpu,
        batch,
        asset_manager,
        "sidec02.mix",
        "sidebar.pal",
        "radar.shp",
        Some(("bkgdlg.shp", "bkgdmd.shp", "bkgdsm.shp")),
    );
    let yuri = build_theme_atlas(
        gpu,
        batch,
        asset_manager,
        "sidec02md.mix",
        "radaryuri.pal",
        "radary.shp",
        Some(("bkgdlgy.shp", "bkgdmdy.shp", "bkgdsmy.shp")),
    );

    if allied.is_none() && soviet.is_none() && yuri.is_none() {
        return None;
    }

    Some(SidebarChromeSet {
        allied,
        soviet,
        yuri,
    })
}

fn build_theme_atlas(
    gpu: &GpuContext,
    batch: &BatchRenderer,
    asset_manager: &AssetManager,
    mix_name: &str,
    palette_name: &str,
    radar_name: &str,
    background_names: Option<(&str, &str, &str)>,
) -> Option<SidebarChromeAtlas> {
    let mix = asset_manager.archive(mix_name)?;
    let palette = mix
        .get_by_name(palette_name)
        .and_then(|bytes| Palette::from_bytes(bytes).ok())
        .or_else(|| {
            asset_manager
                .get_ref(palette_name)
                .and_then(|bytes| Palette::from_bytes(bytes).ok())
        })?;
    // For sidebar inspection/fidelity work, decode every sidebar-side MIX SHP
    // with the theme's main sidebar palette so unknown pieces are comparable
    // under one consistent color treatment.
    let tabs_palette = palette.clone();

    // Required pieces — without these, skip the theme entirely.
    let radar = render_entry(
        asset_manager,
        &mix,
        radar_name,
        &palette,
        RADAR_DEFAULT_FRAME,
    )?;

    // Pre-render all radar.shp frames for the opening/closing animation.
    let (radar_frames, radar_frame_size, radar_content_insets) =
        render_all_radar_frames(asset_manager, &mix, radar_name, &palette);
    let side1 = render_entry(asset_manager, &mix, "side1.shp", &palette, 0)?;
    let side2 = render_entry(asset_manager, &mix, "side2.shp", &palette, 0)?;
    let side3 = render_entry(asset_manager, &mix, "side3.shp", &palette, 0)?;

    // Optional pieces — gracefully degrade if missing.
    let tabs = render_entry(asset_manager, &mix, "tabs.shp", &tabs_palette, 0);
    let tab_entries: Vec<RenderedChromeEntry> = (0..4)
        .filter_map(|i| render_entry(asset_manager, &mix, &format!("tab0{i}.shp"), &palette, 0))
        .collect();
    // Frame 1 is the brighter selected/highlighted tab state in the stock art.
    let tab_active_entries: Vec<RenderedChromeEntry> = (0..4)
        .filter_map(|i| render_entry(asset_manager, &mix, &format!("tab0{i}.shp"), &palette, 1))
        .collect();
    let repair = render_entry(asset_manager, &mix, "repair.shp", &palette, 0);
    let sell = render_entry(asset_manager, &mix, "sell.shp", &palette, 0);
    let power = render_entry(asset_manager, &mix, "power.shp", &palette, 0);
    // powerp.shp: strip frames for the power bar meter.
    // Use raw frame pixel data (not the full SHP canvas) to avoid transparent
    // padding from frame offsets. Then force opaque: the original CC_Draw_Shape
    // skips index-0 pixels, letting the sidebar background show through. Since
    // our renderer uses textured quads, we make them opaque black instead.
    let powerp_rendered: Vec<RenderedChromeEntry> = (0..5)
        .filter_map(|i| {
            let mut entry = render_shp_frame_only(&mix, "powerp.shp", &palette, i)?;
            for pixel in entry.rgba.chunks_exact_mut(4) {
                pixel[3] = 255;
            }
            Some(entry)
        })
        .collect();
    let top_strip_left = render_entry_by_id(&mix, TOP_STRIP_LEFT_ID, &tabs_palette, 0);
    let top_strip_sidebar = render_entry_by_id(&mix, TOP_STRIP_SIDEBAR_ID, &tabs_palette, 0);
    let top_strip_thin = render_entry_by_id(&mix, TOP_STRIP_THIN_ID, &tabs_palette, 0);
    let unknown_top_housing = render_entry_by_id(&mix, UNKNOWN_TOP_HOUSING_ID, &palette, 0);
    let unknown_mid_panel = render_entry_by_id(&mix, UNKNOWN_MID_PANEL_ID, &palette, 0);
    let (background_large, background_medium, background_small) =
        if let Some((large, medium, small)) = background_names {
            (
                render_entry(asset_manager, &mix, large, &tabs_palette, 0),
                render_entry(asset_manager, &mix, medium, &tabs_palette, 0),
                render_entry(asset_manager, &mix, small, &tabs_palette, 0),
            )
        } else {
            (None, None, None)
        };
    let excluded_extra_ids = known_loose_piece_ids(radar_name);
    let extra_rendered = collect_extra_entries(
        &mix,
        &palette,
        &tabs_palette,
        background_names,
        &excluded_extra_ids,
    );

    // Collect all pieces to pack into the atlas.
    let mut all_entries: Vec<&RenderedChromeEntry> = vec![&radar, &side1, &side2, &side3];
    if let Some(ref top) = top_strip_left {
        all_entries.push(top);
    }
    if let Some(ref top) = top_strip_sidebar {
        all_entries.push(top);
    }
    if let Some(ref top) = top_strip_thin {
        all_entries.push(top);
    }
    if let Some(ref unknown) = unknown_top_housing {
        all_entries.push(unknown);
    }
    if let Some(ref unknown) = unknown_mid_panel {
        all_entries.push(unknown);
    }
    if let Some(ref bg) = background_large {
        all_entries.push(bg);
    }
    if let Some(ref bg) = background_medium {
        all_entries.push(bg);
    }
    if let Some(ref bg) = background_small {
        all_entries.push(bg);
    }
    if let Some(ref t) = tabs {
        all_entries.push(t);
    }
    for tab in &tab_entries {
        all_entries.push(tab);
    }
    for tab in &tab_active_entries {
        all_entries.push(tab);
    }
    if let Some(ref r) = repair {
        all_entries.push(r);
    }
    if let Some(ref s) = sell {
        all_entries.push(s);
    }
    if let Some(ref p) = power {
        all_entries.push(p);
    }
    for pf in &powerp_rendered {
        all_entries.push(pf);
    }
    for extra in &extra_rendered {
        all_entries.push(&extra.rendered);
    }

    let atlas_width = all_entries.iter().map(|e| e.width).max().unwrap_or(1);
    let atlas_height: u32 = all_entries
        .iter()
        .map(|e| e.height + CHROME_PADDING)
        .sum::<u32>();
    let mut rgba = vec![0u8; (atlas_width * atlas_height * 4) as usize];
    let mut y = 0u32;

    let top_strip_left_uv = top_strip_left.as_ref().map(|entry| {
        let uv = blit_entry(&mut rgba, atlas_width, atlas_height, y, entry);
        y += entry.height + CHROME_PADDING;
        uv
    });
    let top_strip_sidebar_uv = top_strip_sidebar.as_ref().map(|entry| {
        let uv = blit_entry(&mut rgba, atlas_width, atlas_height, y, entry);
        y += entry.height + CHROME_PADDING;
        uv
    });
    let top_strip_thin_uv = top_strip_thin.as_ref().map(|entry| {
        let uv = blit_entry(&mut rgba, atlas_width, atlas_height, y, entry);
        y += entry.height + CHROME_PADDING;
        uv
    });
    let unknown_top_housing_uv = unknown_top_housing.as_ref().map(|entry| {
        let uv = blit_entry(&mut rgba, atlas_width, atlas_height, y, entry);
        y += entry.height + CHROME_PADDING;
        uv
    });
    let unknown_mid_panel_uv = unknown_mid_panel.as_ref().map(|entry| {
        let uv = blit_entry(&mut rgba, atlas_width, atlas_height, y, entry);
        y += entry.height + CHROME_PADDING;
        uv
    });
    let background_large_uv = background_large.as_ref().map(|entry| {
        let uv = blit_entry(&mut rgba, atlas_width, atlas_height, y, entry);
        y += entry.height + CHROME_PADDING;
        uv
    });
    let background_medium_uv = background_medium.as_ref().map(|entry| {
        let uv = blit_entry(&mut rgba, atlas_width, atlas_height, y, entry);
        y += entry.height + CHROME_PADDING;
        uv
    });
    let background_small_uv = background_small.as_ref().map(|entry| {
        let uv = blit_entry(&mut rgba, atlas_width, atlas_height, y, entry);
        y += entry.height + CHROME_PADDING;
        uv
    });
    let radar_uv = blit_entry(&mut rgba, atlas_width, atlas_height, y, &radar);
    y += radar.height + CHROME_PADDING;
    let side1_uv = blit_entry(&mut rgba, atlas_width, atlas_height, y, &side1);
    y += side1.height + CHROME_PADDING;

    let tabs_uv = tabs.as_ref().map(|t| {
        let uv = blit_entry(&mut rgba, atlas_width, atlas_height, y, t);
        y += t.height + CHROME_PADDING;
        uv
    });

    let mut tab_button_uvs = Vec::new();
    for tab in &tab_entries {
        let uv = blit_entry(&mut rgba, atlas_width, atlas_height, y, tab);
        y += tab.height + CHROME_PADDING;
        tab_button_uvs.push(uv);
    }
    let mut tab_button_active_uvs = Vec::new();
    for tab in &tab_active_entries {
        let uv = blit_entry(&mut rgba, atlas_width, atlas_height, y, tab);
        y += tab.height + CHROME_PADDING;
        tab_button_active_uvs.push(uv);
    }

    let side2_uv = blit_entry(&mut rgba, atlas_width, atlas_height, y, &side2);
    y += side2.height + CHROME_PADDING;
    let side3_uv = blit_entry(&mut rgba, atlas_width, atlas_height, y, &side3);
    y += side3.height + CHROME_PADDING;

    let repair_uv = repair.as_ref().map(|r| {
        let uv = blit_entry(&mut rgba, atlas_width, atlas_height, y, r);
        y += r.height + CHROME_PADDING;
        uv
    });
    let sell_uv = sell.as_ref().map(|s| {
        let uv = blit_entry(&mut rgba, atlas_width, atlas_height, y, s);
        y += s.height + CHROME_PADDING;
        uv
    });
    let power_uv = power.as_ref().map(|p| {
        let uv = blit_entry(&mut rgba, atlas_width, atlas_height, y, p);
        y += p.height + CHROME_PADDING;
        uv
    });
    let mut powerp_uvs: [Option<SidebarChromeEntry>; 5] = [None; 5];
    for (i, pf) in powerp_rendered.iter().enumerate() {
        if i < 5 {
            let uv = blit_entry(&mut rgba, atlas_width, atlas_height, y, pf);
            y += pf.height + CHROME_PADDING;
            powerp_uvs[i] = Some(uv);
        }
    }
    let mut extra_entries = Vec::with_capacity(extra_rendered.len());
    for extra in &extra_rendered {
        let uv = blit_entry(&mut rgba, atlas_width, atlas_height, y, &extra.rendered);
        y += extra.rendered.height + CHROME_PADDING;
        extra_entries.push(SidebarChromeExtraEntry {
            id: extra.id,
            label: extra.label.clone(),
            frame_count: extra.frame_count,
            entry: uv,
        });
    }

    log::info!(
        "Sidebar chrome atlas for {}: {}x{} px, {} pieces",
        mix_name,
        atlas_width,
        atlas_height,
        all_entries.len()
    );
    log::info!(
        "  radar={}x{} side1={}x{} tabs={} side2={}x{} side3={}x{}",
        radar.width,
        radar.height,
        side1.width,
        side1.height,
        tabs.as_ref()
            .map(|t| format!("{}x{}", t.width, t.height))
            .unwrap_or("none".into()),
        side2.width,
        side2.height,
        side3.width,
        side3.height,
    );
    for (i, tab) in tab_entries.iter().enumerate() {
        log::info!("  tab0{} (inactive): {}x{}", i, tab.width, tab.height);
    }
    for (i, tab) in tab_active_entries.iter().enumerate() {
        log::info!("  tab0{} (active):   {}x{}", i, tab.width, tab.height);
    }

    let texture = batch.create_texture(gpu, &rgba, atlas_width, atlas_height);
    Some(SidebarChromeAtlas {
        texture,
        top_strip_left: top_strip_left_uv,
        top_strip_sidebar: top_strip_sidebar_uv,
        top_strip_thin: top_strip_thin_uv,
        unknown_top_housing: unknown_top_housing_uv,
        unknown_mid_panel: unknown_mid_panel_uv,
        background_large: background_large_uv,
        background_medium: background_medium_uv,
        background_small: background_small_uv,
        radar_frames,
        radar_frame_size,
        radar_content_insets,
        radar: radar_uv,
        side1: side1_uv,
        tabs: tabs_uv,
        tab_buttons: tab_button_uvs,
        tab_buttons_active: tab_button_active_uvs,
        side2: side2_uv,
        side3: side3_uv,
        repair: repair_uv,
        sell: sell_uv,
        power: power_uv,
        powerp_frames: powerp_uvs,
        extra_entries,
    })
}

struct RenderedChromeEntry {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

struct ExtraRenderedEntry {
    id: i32,
    label: String,
    frame_count: usize,
    rendered: RenderedChromeEntry,
}

fn known_loose_piece_ids(radar_name: &str) -> std::collections::HashSet<i32> {
    let mut ids = std::collections::HashSet::new();
    ids.insert(mix_hash(radar_name));
    ids.insert(mix_hash("side1.shp"));
    ids.insert(mix_hash("tabs.shp"));
    ids.insert(mix_hash("tab00.shp"));
    ids.insert(mix_hash("tab01.shp"));
    ids.insert(mix_hash("tab02.shp"));
    ids.insert(mix_hash("tab03.shp"));
    ids.insert(mix_hash("side2.shp"));
    ids.insert(mix_hash("side3.shp"));
    ids.insert(mix_hash("repair.shp"));
    ids.insert(mix_hash("sell.shp"));
    ids.insert(mix_hash("powerp.shp"));
    ids.insert(TOP_STRIP_SIDEBAR_ID);
    ids.insert(TOP_STRIP_THIN_ID);
    ids.insert(UNKNOWN_TOP_HOUSING_ID);
    ids.insert(UNKNOWN_MID_PANEL_ID);
    ids
}

fn collect_extra_entries(
    mix: &MixArchive,
    palette: &Palette,
    _tabs_palette: &Palette,
    _background_names: Option<(&str, &str, &str)>,
    excluded_ids: &std::collections::HashSet<i32>,
) -> Vec<ExtraRenderedEntry> {
    let mut entries = Vec::new();
    for mix_entry in mix.entries() {
        if excluded_ids.contains(&mix_entry.id) {
            continue;
        }
        let Some(shp_bytes) = mix.get_by_id(mix_entry.id) else {
            continue;
        };
        let Ok(shp) = ShpFile::from_bytes(shp_bytes) else {
            continue;
        };
        let Some(rendered) = render_shp(&shp, palette, 0) else {
            continue;
        };
        entries.push(ExtraRenderedEntry {
            id: mix_entry.id,
            label: format!(
                "id {:#010X} ({}x{}, {}f)",
                mix_entry.id as u32,
                shp.width,
                shp.height,
                shp.frames.len()
            ),
            frame_count: shp.frames.len(),
            rendered,
        });
    }
    entries
}

/// Pre-render all frames of radar.shp to RGBA buffers for animation.
/// Returns (Vec of RGBA buffers, [width, height], content insets).
/// Content insets [left, top, right, bottom] are derived from the transparent
/// opening in frame 0 (fully open housing).
fn render_all_radar_frames(
    asset_manager: &AssetManager,
    mix: &MixArchive,
    radar_name: &str,
    palette: &Palette,
) -> (Vec<Vec<u8>>, [u32; 2], [u32; 4]) {
    let shp_bytes = match mix
        .get_by_name(radar_name)
        .or_else(|| asset_manager.get_ref(radar_name))
    {
        Some(b) => b,
        None => return (Vec::new(), [0, 0], [0; 4]),
    };
    let shp = match ShpFile::from_bytes(shp_bytes) {
        Ok(s) => s,
        Err(_) => return (Vec::new(), [0, 0], [0; 4]),
    };
    let frame_count: usize = shp.frames.len();
    let canvas_w: u32 = shp.width as u32;
    let canvas_h: u32 = shp.height as u32;

    // Render frame 0 (the housing base) first — all other frames are composited
    // on top of it so the housing art always shows through transparent areas.
    let base_rgba: Vec<u8> = match render_shp(&shp, palette, RADAR_DEFAULT_FRAME) {
        Some(entry) => entry.rgba,
        None => vec![0u8; (canvas_w * canvas_h * 4) as usize],
    };

    // Detect the content opening by scanning frame 0 for the transparent region.
    let content_insets = detect_radar_content_insets(&base_rgba, canvas_w, canvas_h);

    let mut frames: Vec<Vec<u8>> = Vec::with_capacity(frame_count);
    for i in 0..frame_count {
        let frame_rgba = match render_shp(&shp, palette, i) {
            Some(entry) => entry.rgba,
            None => vec![0u8; (canvas_w * canvas_h * 4) as usize],
        };
        // Composite: start with housing base, overlay this frame's opaque pixels.
        let mut composited: Vec<u8> = base_rgba.clone();
        for (dst, src) in composited
            .chunks_exact_mut(4)
            .zip(frame_rgba.chunks_exact(4))
        {
            if src[3] > 0 {
                dst[0] = src[0];
                dst[1] = src[1];
                dst[2] = src[2];
                dst[3] = src[3];
            }
        }
        frames.push(composited);
    }
    log::info!(
        "Pre-rendered {} radar animation frames ({}x{}, content insets: l={} t={} r={} b={})",
        frames.len(),
        canvas_w,
        canvas_h,
        content_insets[0],
        content_insets[1],
        content_insets[2],
        content_insets[3],
    );
    (frames, [canvas_w, canvas_h], content_insets)
}

/// Scan the fully-open radar frame (frame 0) to find the transparent opening.
///
/// The radar chrome has an opaque border and a transparent interior where the
/// minimap shows through. We find the bounding box of the transparent region
/// and return it as [left, top, right, bottom] insets from the frame edges.
fn detect_radar_content_insets(rgba: &[u8], width: u32, height: u32) -> [u32; 4] {
    let mut min_x: u32 = width;
    let mut min_y: u32 = height;
    let mut max_x: u32 = 0;
    let mut max_y: u32 = 0;
    let mut found = false;

    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4 + 3) as usize;
            if idx < rgba.len() && rgba[idx] == 0 {
                // Transparent pixel — part of the content opening.
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
                found = true;
            }
        }
    }

    if !found {
        // No transparent region found — fall back to small default insets.
        return [9, 7, 9, 7];
    }

    let left = min_x;
    let top = min_y;
    let right = width.saturating_sub(max_x + 1);
    let bottom = height.saturating_sub(max_y + 1);
    [left, top, right, bottom]
}

fn render_shp(shp: &ShpFile, palette: &Palette, frame_index: usize) -> Option<RenderedChromeEntry> {
    if frame_index >= shp.frames.len() {
        return None;
    }
    let frame = &shp.frames[frame_index];
    let frame_rgba = shp.frame_to_rgba(frame_index, palette).ok()?;
    let canvas_w = shp.width as u32;
    let canvas_h = shp.height as u32;
    let fw = frame.frame_width as u32;
    let fh = frame.frame_height as u32;
    let fx = frame.frame_x as u32;
    let fy = frame.frame_y as u32;
    if canvas_w == fw && canvas_h == fh && fx == 0 && fy == 0 {
        return Some(RenderedChromeEntry {
            rgba: frame_rgba,
            width: canvas_w,
            height: canvas_h,
        });
    }
    let mut canvas = vec![0u8; (canvas_w * canvas_h * 4) as usize];
    for row in 0..fh {
        let src_off = (row * fw * 4) as usize;
        let dst_off = (((fy + row) * canvas_w + fx) * 4) as usize;
        let len = (fw * 4) as usize;
        if src_off + len <= frame_rgba.len() && dst_off + len <= canvas.len() {
            canvas[dst_off..dst_off + len].copy_from_slice(&frame_rgba[src_off..src_off + len]);
        }
    }
    Some(RenderedChromeEntry {
        rgba: canvas,
        width: canvas_w,
        height: canvas_h,
    })
}

/// Render an SHP frame using only the frame's own pixel dimensions, ignoring
/// the SHP canvas size and frame offsets. This avoids transparent padding from
/// frames that are smaller than or offset within the overall canvas.
fn render_shp_frame_only(
    mix: &MixArchive,
    shp_name: &str,
    palette: &Palette,
    frame_index: usize,
) -> Option<RenderedChromeEntry> {
    let shp_bytes = mix.get_by_name(shp_name)?;
    let shp = ShpFile::from_bytes(shp_bytes).ok()?;
    if frame_index >= shp.frames.len() {
        return None;
    }
    let frame = &shp.frames[frame_index];
    let frame_rgba = shp.frame_to_rgba(frame_index, palette).ok()?;
    Some(RenderedChromeEntry {
        rgba: frame_rgba,
        width: frame.frame_width as u32,
        height: frame.frame_height as u32,
    })
}

fn render_entry(
    asset_manager: &AssetManager,
    mix: &MixArchive,
    shp_name: &str,
    palette: &Palette,
    frame_index: usize,
) -> Option<RenderedChromeEntry> {
    let shp_bytes = mix
        .get_by_name(shp_name)
        .or_else(|| asset_manager.get_ref(shp_name))?;
    let shp = ShpFile::from_bytes(shp_bytes).ok()?;
    render_shp(&shp, palette, frame_index)
}

fn render_entry_by_id(
    mix: &MixArchive,
    id: i32,
    palette: &Palette,
    frame_index: usize,
) -> Option<RenderedChromeEntry> {
    let shp_bytes = mix.get_by_id(id)?;
    let shp = ShpFile::from_bytes(shp_bytes).ok()?;
    render_shp(&shp, palette, frame_index)
}

fn blit_entry(
    atlas_rgba: &mut [u8],
    atlas_width: u32,
    atlas_height: u32,
    dst_y: u32,
    entry: &RenderedChromeEntry,
) -> SidebarChromeEntry {
    for row in 0..entry.height {
        let src_start = (row * entry.width * 4) as usize;
        let dst_row = dst_y + row;
        if dst_row >= atlas_height {
            break;
        }
        let dst_start = ((dst_row * atlas_width) * 4) as usize;
        let len = (entry.width * 4) as usize;
        if src_start + len <= entry.rgba.len() && dst_start + len <= atlas_rgba.len() {
            atlas_rgba[dst_start..dst_start + len]
                .copy_from_slice(&entry.rgba[src_start..src_start + len]);
        }
    }

    SidebarChromeEntry {
        uv_origin: [0.0, dst_y as f32 / atlas_height as f32],
        uv_size: [
            entry.width as f32 / atlas_width as f32,
            entry.height as f32 / atlas_height as f32,
        ],
        pixel_size: [entry.width as f32, entry.height as f32],
    }
}
