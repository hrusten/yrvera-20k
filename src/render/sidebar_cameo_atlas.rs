//! Sidebar cameo atlas.
//!
//! Loads build-palette cameo SHPs at map load time and packs them into a single
//! GPU texture so the custom sidebar can draw real art in one batch.

use std::collections::HashMap;
use std::path::Path;

use crate::assets::asset_manager::AssetManager;
use crate::assets::pal_file::Palette;
use crate::assets::shp_file::ShpFile;
use crate::render::batch::{BatchRenderer, BatchTexture};
use crate::render::gpu::GpuContext;
use crate::rules::art_data::ArtRegistry;
use crate::rules::ruleset::RuleSet;

const CAMEO_PADDING: u32 = 2;
const DEBUG_SAMPLE_COUNT: usize = 8;
const DEBUG_CELL_PADDING: u32 = 8;
const DEBUG_LABEL_HEIGHT: u32 = 18;

#[derive(Debug, Clone, Copy)]
pub struct SidebarCameoEntry {
    pub uv_origin: [f32; 2],
    pub uv_size: [f32; 2],
    pub pixel_size: [f32; 2],
}

pub struct SidebarCameoAtlas {
    pub texture: BatchTexture,
    entries: HashMap<String, SidebarCameoEntry>,
}

impl SidebarCameoAtlas {
    pub fn get(&self, type_id: &str) -> Option<&SidebarCameoEntry> {
        self.entries.get(&type_id.to_ascii_uppercase())
    }
}

struct RenderedCameo {
    type_id: String,
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

pub fn build_sidebar_cameo_atlas(
    gpu: &GpuContext,
    batch: &BatchRenderer,
    asset_manager: &AssetManager,
    rules: &RuleSet,
    art: Option<&ArtRegistry>,
    palette: &Palette,
) -> Option<SidebarCameoAtlas> {
    let mut rendered: Vec<RenderedCameo> = Vec::new();
    let type_ids = rules
        .building_ids
        .iter()
        .chain(rules.infantry_ids.iter())
        .chain(rules.vehicle_ids.iter())
        .chain(rules.aircraft_ids.iter());

    for type_id in type_ids {
        if let Some(cameo) = render_cameo(type_id, asset_manager, rules, art, palette) {
            rendered.push(cameo);
        }
    }

    // Load superweapon sidebar images (e.g. BOLTICON, CHROICON).
    // These are standalone SHP files in cameo(md).mix, not referenced in art.ini.
    for sw in rules.super_weapons.values() {
        if let Some(ref image_name) = sw.sidebar_image {
            let upper = image_name.to_ascii_uppercase();
            // Skip if already loaded (e.g. SPYPICON reused as unit cameo).
            if rendered.iter().any(|r| r.type_id == upper) {
                continue;
            }
            if let Some(cameo) = render_sw_cameo(&upper, asset_manager, palette) {
                rendered.push(cameo);
            }
        }
    }

    if rendered.is_empty() {
        log::warn!("Sidebar cameo atlas: no cameo art found");
        return None;
    }

    let atlas = pack_cameos(gpu, batch, &rendered)?;
    log::info!(
        "Sidebar cameo atlas built: {} entries, {}x{} px",
        atlas.entries.len(),
        atlas.texture.width,
        atlas.texture.height
    );
    Some(atlas)
}

/// Export a side-by-side palette comparison sheet for a few cameos.
///
/// This is an empirical debugging tool: when sidebar art looks washed out, a
/// single PNG with the same cameo rendered under several palettes is faster and
/// more reliable than guessing palette order from memory.
pub fn export_debug_palette_sheet(
    asset_manager: &AssetManager,
    rules: &RuleSet,
    art: Option<&ArtRegistry>,
    output_path: &Path,
    palette_names: &[&str],
) {
    let sample_type_ids: Vec<&str> = rules
        .building_ids
        .iter()
        .chain(rules.infantry_ids.iter())
        .chain(rules.vehicle_ids.iter())
        .chain(rules.aircraft_ids.iter())
        .take(DEBUG_SAMPLE_COUNT)
        .map(String::as_str)
        .collect();
    if sample_type_ids.is_empty() || palette_names.is_empty() {
        return;
    }

    let mut palette_rows: Vec<(&str, Vec<RenderedCameo>)> = Vec::new();
    let mut max_cameo_w = 1u32;
    let mut max_cameo_h = 1u32;
    for &palette_name in palette_names {
        let Some(data) = asset_manager.get_ref(palette_name) else {
            continue;
        };
        let Ok(palette) = Palette::from_bytes(data) else {
            continue;
        };
        let mut row: Vec<RenderedCameo> = Vec::new();
        for type_id in &sample_type_ids {
            if let Some(cameo) = render_cameo(type_id, asset_manager, rules, art, &palette) {
                max_cameo_w = max_cameo_w.max(cameo.width);
                max_cameo_h = max_cameo_h.max(cameo.height);
                row.push(cameo);
            }
        }
        if !row.is_empty() {
            palette_rows.push((palette_name, row));
        }
    }
    if palette_rows.is_empty() {
        return;
    }

    let cols = sample_type_ids.len() as u32;
    let rows = palette_rows.len() as u32;
    let cell_w = max_cameo_w + DEBUG_CELL_PADDING * 2;
    let cell_h = max_cameo_h + DEBUG_CELL_PADDING * 2 + DEBUG_LABEL_HEIGHT;
    let canvas_w = cols * cell_w;
    let canvas_h = rows * cell_h;
    let mut rgba = vec![32u8; (canvas_w * canvas_h * 4) as usize];
    for px in rgba.chunks_exact_mut(4) {
        px[3] = 255;
    }

    for (row_idx, (palette_name, cameos)) in palette_rows.iter().enumerate() {
        log::info!("Sidebar cameo debug row for palette {}", palette_name);
        for (col_idx, cameo) in cameos.iter().enumerate() {
            let cell_x = col_idx as u32 * cell_w;
            let cell_y = row_idx as u32 * cell_h;
            let origin_x = cell_x + DEBUG_CELL_PADDING;
            let origin_y = cell_y + DEBUG_CELL_PADDING;
            blit_rgba(
                &mut rgba,
                canvas_w,
                canvas_h,
                origin_x,
                origin_y,
                &cameo.rgba,
                cameo.width,
                cameo.height,
            );
        }
    }

    if let Some(img) = image::RgbaImage::from_raw(canvas_w, canvas_h, rgba) {
        if let Err(err) = img.save(output_path) {
            log::warn!(
                "Failed to save sidebar cameo palette debug sheet {}: {}",
                output_path.display(),
                err
            );
        } else {
            log::info!(
                "Saved sidebar cameo palette debug sheet {} ({}x{})",
                output_path.display(),
                canvas_w,
                canvas_h
            );
        }
    }
}

fn render_cameo(
    type_id: &str,
    asset_manager: &AssetManager,
    rules: &RuleSet,
    art: Option<&ArtRegistry>,
    palette: &Palette,
) -> Option<RenderedCameo> {
    let rules_image = rules
        .object(type_id)
        .map(|obj| obj.image.clone())
        .unwrap_or_else(|| type_id.to_string());
    let resolved_image = art
        .map(|registry| registry.resolve_effective_image_id(type_id, &rules_image))
        .unwrap_or_else(|| rules_image.to_ascii_uppercase());
    let resolved_cameo = art
        .map(|registry| registry.resolve_declared_cameo_id(type_id, &rules_image))
        .unwrap_or_else(|| resolved_image.clone());

    let mut candidates: Vec<String> = Vec::with_capacity(8);
    push_unique(&mut candidates, format!("{resolved_cameo}.SHP"));
    if !resolved_image.eq_ignore_ascii_case(&resolved_cameo) {
        push_unique(&mut candidates, format!("{resolved_image}.SHP"));
    }
    candidates.extend(legacy_cameo_fallback_candidates(
        type_id,
        &resolved_image,
        &resolved_cameo,
    ));

    let (data, file_name) = candidates
        .iter()
        .find_map(|name| asset_manager.get_ref(name).map(|data| (data, name.clone())))?;
    let shp = ShpFile::from_bytes(data).ok()?;
    if shp.frames.is_empty() {
        return None;
    }

    let frame_rgba = shp.frame_to_rgba(0, palette).ok()?;
    let frame = &shp.frames[0];
    let full_w = shp.width as u32;
    let full_h = shp.height as u32;
    if full_w == 0 || full_h == 0 {
        return None;
    }
    let mut full_rgba = vec![0u8; (full_w * full_h * 4) as usize];
    blit_frame_into_full_bounds(&mut full_rgba, full_w, full_h, frame, &frame_rgba);
    let (cropped_rgba, cropped_w, cropped_h) = crop_visible_bounds(&full_rgba, full_w, full_h)?;
    log::debug!("Sidebar cameo {} loaded from {}", type_id, file_name);

    Some(RenderedCameo {
        type_id: type_id.to_ascii_uppercase(),
        rgba: cropped_rgba,
        width: cropped_w,
        height: cropped_h,
    })
}

/// Load a superweapon sidebar image SHP directly by name.
///
/// SW sidebar images are standalone SHP files (e.g. `BOLTICON.SHP`) in
/// `cameo(md).mix`, not referenced in art.ini. No image/cameo resolution needed.
fn render_sw_cameo(
    image_name: &str,
    asset_manager: &AssetManager,
    palette: &Palette,
) -> Option<RenderedCameo> {
    let shp_name = format!("{image_name}.SHP");
    let data = asset_manager.get_ref(&shp_name)?;
    let shp = ShpFile::from_bytes(data).ok()?;
    if shp.frames.is_empty() {
        return None;
    }
    let frame_rgba = shp.frame_to_rgba(0, palette).ok()?;
    let frame = &shp.frames[0];
    let full_w = shp.width as u32;
    let full_h = shp.height as u32;
    if full_w == 0 || full_h == 0 {
        return None;
    }
    let mut full_rgba = vec![0u8; (full_w * full_h * 4) as usize];
    blit_frame_into_full_bounds(&mut full_rgba, full_w, full_h, frame, &frame_rgba);
    let (cropped_rgba, cropped_w, cropped_h) = crop_visible_bounds(&full_rgba, full_w, full_h)?;
    log::debug!("SW sidebar cameo {} loaded from {}", image_name, shp_name);
    Some(RenderedCameo {
        type_id: image_name.to_ascii_uppercase(),
        rgba: cropped_rgba,
        width: cropped_w,
        height: cropped_h,
    })
}

/// Repo-only fallback guesses kept out of `rules::art_data`.
///
/// `needs verification`: these guesses match current repo behavior and common RA2
/// sidebar conventions, but they are not part of exact art-data resolution.
fn legacy_cameo_fallback_candidates(
    type_id: &str,
    resolved_image: &str,
    resolved_cameo: &str,
) -> Vec<String> {
    let upper_type = type_id.to_ascii_uppercase();
    let mut candidates: Vec<String> = Vec::with_capacity(8);

    if !resolved_cameo.to_ascii_uppercase().ends_with("ICON") {
        push_unique(&mut candidates, format!("{resolved_cameo}ICON.SHP"));
    }
    push_unique(&mut candidates, format!("{resolved_image}ICON.SHP"));

    if !upper_type.eq_ignore_ascii_case(resolved_cameo)
        && !upper_type.eq_ignore_ascii_case(resolved_image)
    {
        push_unique(&mut candidates, format!("{upper_type}ICON.SHP"));
        push_unique(&mut candidates, format!("{upper_type}.SHP"));
    }

    for name in [&upper_type, resolved_image] {
        if name.len() > 2 {
            let prefix = &name[..2];
            if matches!(prefix, "GA" | "NA" | "YA" | "CA") {
                push_unique(&mut candidates, format!("{}ICON.SHP", &name[2..]));
            }
        }
    }

    candidates
}

fn push_unique(candidates: &mut Vec<String>, candidate: String) {
    if !candidates.contains(&candidate) {
        candidates.push(candidate);
    }
}

fn blit_frame_into_full_bounds(
    full_rgba: &mut [u8],
    full_w: u32,
    full_h: u32,
    frame: &crate::assets::shp_file::ShpFrame,
    frame_rgba: &[u8],
) {
    let fw = frame.frame_width as u32;
    let fh = frame.frame_height as u32;
    let fx = frame.frame_x as u32;
    let fy = frame.frame_y as u32;
    for y in 0..fh {
        let dst_y = fy + y;
        if dst_y >= full_h {
            break;
        }
        let src_start = (y * fw * 4) as usize;
        let dst_start = ((dst_y * full_w + fx) * 4) as usize;
        let copy_w = fw.min(full_w.saturating_sub(fx));
        let byte_count = (copy_w * 4) as usize;
        if src_start + byte_count <= frame_rgba.len() && dst_start + byte_count <= full_rgba.len() {
            full_rgba[dst_start..dst_start + byte_count]
                .copy_from_slice(&frame_rgba[src_start..src_start + byte_count]);
        }
    }
}

fn crop_visible_bounds(rgba: &[u8], width: u32, height: u32) -> Option<(Vec<u8>, u32, u32)> {
    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    let mut found = false;

    for y in 0..height {
        for x in 0..width {
            let alpha = rgba[((y * width + x) * 4 + 3) as usize];
            if alpha == 0 {
                continue;
            }
            found = true;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }

    if !found {
        return None;
    }

    let cropped_w = max_x - min_x + 1;
    let cropped_h = max_y - min_y + 1;
    let mut cropped = vec![0u8; (cropped_w * cropped_h * 4) as usize];

    for y in 0..cropped_h {
        let src_start = (((min_y + y) * width + min_x) * 4) as usize;
        let dst_start = (y * cropped_w * 4) as usize;
        let byte_count = (cropped_w * 4) as usize;
        cropped[dst_start..dst_start + byte_count]
            .copy_from_slice(&rgba[src_start..src_start + byte_count]);
    }

    Some((cropped, cropped_w, cropped_h))
}

fn blit_rgba(
    dst: &mut [u8],
    dst_w: u32,
    dst_h: u32,
    dst_x: u32,
    dst_y: u32,
    src: &[u8],
    src_w: u32,
    src_h: u32,
) {
    for y in 0..src_h {
        let out_y = dst_y + y;
        if out_y >= dst_h {
            break;
        }
        let src_start = (y * src_w * 4) as usize;
        let dst_start = ((out_y * dst_w + dst_x) * 4) as usize;
        let copy_w = src_w.min(dst_w.saturating_sub(dst_x));
        let byte_count = (copy_w * 4) as usize;
        if src_start + byte_count <= src.len() && dst_start + byte_count <= dst.len() {
            dst[dst_start..dst_start + byte_count]
                .copy_from_slice(&src[src_start..src_start + byte_count]);
        }
    }
}

fn pack_cameos(
    gpu: &GpuContext,
    batch: &BatchRenderer,
    cameos: &[RenderedCameo],
) -> Option<SidebarCameoAtlas> {
    let mut indices: Vec<usize> = (0..cameos.len()).collect();
    indices.sort_by(|&a, &b| cameos[b].height.cmp(&cameos[a].height));

    let total_area: u64 = cameos
        .iter()
        .map(|cameo| {
            (cameo.width as u64 + CAMEO_PADDING as u64)
                * (cameo.height as u64 + CAMEO_PADDING as u64)
        })
        .sum();
    let max_texture_dim = gpu.device.limits().max_texture_dimension_2d;
    let widest_cameo = cameos.iter().map(|cameo| cameo.width).max().unwrap_or(1);
    let mut atlas_width =
        ((total_area as f64).sqrt().ceil() as u32).clamp(widest_cameo.max(64), max_texture_dim);
    let placements;
    let atlas_height;

    loop {
        let (trial_placements, trial_height) = shelf_pack(cameos, &indices, atlas_width);
        if trial_height <= max_texture_dim {
            placements = trial_placements;
            atlas_height = trial_height;
            break;
        }
        if atlas_width >= max_texture_dim {
            log::error!(
                "Sidebar cameo atlas exceeds GPU limit even at max width: height={} limit={}",
                trial_height,
                max_texture_dim
            );
            return None;
        }
        atlas_width = (atlas_width.saturating_mul(2)).min(max_texture_dim);
    }
    let mut rgba = vec![0u8; (atlas_width * atlas_height * 4) as usize];
    let mut entries = HashMap::with_capacity(cameos.len());
    let aw = atlas_width as f32;
    let ah = atlas_height as f32;

    for (idx, px, py) in placements {
        let cameo = &cameos[idx];
        for y in 0..cameo.height {
            let src_start = (y * cameo.width * 4) as usize;
            let dst_start = (((py + y) * atlas_width + px) * 4) as usize;
            let byte_count = (cameo.width * 4) as usize;
            rgba[dst_start..dst_start + byte_count]
                .copy_from_slice(&cameo.rgba[src_start..src_start + byte_count]);
        }
        entries.insert(
            cameo.type_id.clone(),
            SidebarCameoEntry {
                uv_origin: [px as f32 / aw, py as f32 / ah],
                uv_size: [cameo.width as f32 / aw, cameo.height as f32 / ah],
                pixel_size: [cameo.width as f32, cameo.height as f32],
            },
        );
    }

    Some(SidebarCameoAtlas {
        texture: batch.create_texture(gpu, &rgba, atlas_width, atlas_height),
        entries,
    })
}

fn shelf_pack(
    cameos: &[RenderedCameo],
    indices: &[usize],
    atlas_width: u32,
) -> (Vec<(usize, u32, u32)>, u32) {
    let mut placements: Vec<(usize, u32, u32)> = Vec::with_capacity(cameos.len());
    let mut cursor_x = 0;
    let mut cursor_y = 0;
    let mut shelf_height = 0;
    for &idx in indices {
        let cameo = &cameos[idx];
        if cursor_x + cameo.width > atlas_width {
            cursor_y += shelf_height + CAMEO_PADDING;
            cursor_x = 0;
            shelf_height = 0;
        }
        placements.push((idx, cursor_x, cursor_y));
        cursor_x += cameo.width + CAMEO_PADDING;
        shelf_height = shelf_height.max(cameo.height);
    }
    let atlas_height = placements
        .iter()
        .map(|(idx, _, py)| py + cameos[*idx].height)
        .max()
        .unwrap_or(1);
    (placements, atlas_height)
}

#[cfg(test)]
mod tests {
    use super::legacy_cameo_fallback_candidates;

    #[test]
    fn test_legacy_cameo_fallbacks_start_after_declared_candidates() {
        let candidates = legacy_cameo_fallback_candidates("GACNST", "GACNST", "CIVICON");
        assert_eq!(candidates[0], "GACNSTICON.SHP");
        assert!(candidates.contains(&"CNSTICON.SHP".to_string()));
        assert!(!candidates.contains(&"CIVICONICON.SHP".to_string()));
    }
}
