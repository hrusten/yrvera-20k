//! Minimap (radar view) — tiny overhead view of the entire map.
//!
//! Shows terrain as colored pixels and entity positions as colored dots
//! in a fixed-size square at the bottom-left corner of the screen.
//! A white rectangle indicates the current camera viewport.
//!
//! ## Implementation
//! - Terrain image is generated once at map load from TerrainGrid data.
//! - Unit dots are overlaid each frame by copying the base image and stamping dots.
//! - The combined image is re-uploaded to the GPU each frame (200x200 = 160KB, negligible).
//! - A separate 2x2 white pixel texture is used for the viewport rectangle lines.
//!
//! ## Screen-space rendering trick
//! The batch shader subtracts camera_pos from world positions. To render UI elements
//! at fixed screen positions, we add camera_pos back:
//!   `instance.position = screen_pos + camera_offset`
//! This cancels out in the shader: `clip_pos = (screen_pos + cam - cam) / screen_size`.
//!
//! ## Dependency rules
//! - Part of render/ — depends on render/batch, render/gpu, map/terrain, sim/components.
//! - Reads from sim/ via EntityStore iteration (GameEntity.position, .owner) but NEVER mutates sim state.

use crate::map::entities::EntityCategory;
use crate::map::houses::HouseColorMap;
use crate::map::terrain::TerrainGrid;
use crate::render::batch::{BatchRenderer, BatchTexture, SpriteInstance};
use crate::render::gpu::GpuContext;
use crate::rules::ruleset::RuleSet;
use crate::sim::vision::FogState;

use super::minimap_helpers::{
    COLOR_BUILDING, COLOR_SHROUD, DOT_SIZE, MINIMAP_DEPTH, MINIMAP_SIZE, VIEWPORT_LINE_THICKNESS,
    cell_visibility_color, compute_aspect_fit, dim_color, draw_line, owner_dot_color,
    parse_foundation_size, radar_color_for_cell, set_pixel, terrain_brightness_for_theater,
    world_to_minimap_pixel, world_to_minimap_pixel_from_cell,
};
pub use super::minimap_helpers::{OverlayClassification, default_minimap_rect};
use super::minimap_helpers::{OverlayPixel, TerrainPixel};

/// Minimap renderer — manages terrain image, unit overlay, and viewport rectangle.
pub struct MinimapRenderer {
    /// Base terrain image (RGBA, MINIMAP_SIZE x MINIMAP_SIZE). Generated once at init.
    base_terrain_rgba: Vec<u8>,
    /// GPU texture containing the current minimap image (terrain + unit dots).
    map_texture: BatchTexture,
    /// Raw GPU texture handle for `write_texture()` reuse (avoids per-frame alloc).
    map_texture_raw: wgpu::Texture,
    /// Reusable RGBA scratch buffer for building the minimap each frame.
    rgba_scratch: Vec<u8>,
    /// Tiny 2x2 white pixel texture for drawing the viewport rectangle lines.
    white_texture: BatchTexture,
    /// Cached world bounds for coordinate mapping.
    world_origin_x: f32,
    world_origin_y: f32,
    world_width: f32,
    world_height: f32,
    terrain_pixels: Vec<TerrainPixel>,
    /// Pre-computed overlay pixels stamped between terrain and unit dots.
    overlay_pixels: Vec<OverlayPixel>,
    /// Aspect-fit sub-region within the 200×200 texture.
    map_offset_x: f32,
    map_offset_y: f32,
    map_pixel_w: f32,
    map_pixel_h: f32,
}

impl MinimapRenderer {
    /// Create a new MinimapRenderer, generating the initial terrain image.
    ///
    /// Each terrain cell is mapped to a minimap pixel based on its normalized
    /// position within the world. Color is chosen by TMP radar colors when
    /// available, falling back to tile classification (water/land/elevated).
    /// Overlay data is pre-classified by the caller to avoid render/ depending
    /// on map/overlay_types.
    pub fn new(
        gpu: &GpuContext,
        batch: &BatchRenderer,
        grid: &TerrainGrid,
        overlay_data: &[(u16, u16, OverlayClassification, u8, Option<[u8; 4]>)],
        theater_name: &str,
    ) -> Self {
        let size: u32 = MINIMAP_SIZE;
        let pixel_count: usize = (size * size * 4) as usize;
        let mut rgba: Vec<u8> = vec![0u8; pixel_count];
        let mut terrain_pixels = Vec::with_capacity(grid.cells.len());
        let terrain_brightness = terrain_brightness_for_theater(theater_name);

        // Fill with a dark background (unexplored/void areas).
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.copy_from_slice(&COLOR_SHROUD);
        }

        // Avoid division by zero for degenerate grids.
        let w: f32 = if grid.world_width > 0.0 {
            grid.world_width
        } else {
            1.0
        };
        let h: f32 = if grid.world_height > 0.0 {
            grid.world_height
        } else {
            1.0
        };

        let (map_offset_x, map_offset_y, map_pixel_w, map_pixel_h) = compute_aspect_fit(w, h);

        for cell in &grid.cells {
            let (px, py): (u32, u32) = world_to_minimap_pixel(
                cell.screen_x,
                cell.screen_y,
                grid.origin_x,
                grid.origin_y,
                w,
                h,
                map_offset_x,
                map_offset_y,
                map_pixel_w,
                map_pixel_h,
            );

            let color: [u8; 4] = radar_color_for_cell(cell, terrain_brightness);
            set_pixel(&mut rgba, size, px, py, color);
            terrain_pixels.push(TerrainPixel {
                rx: cell.rx,
                ry: cell.ry,
                px,
                py,
                color,
            });
        }

        // Build overlay pixels from classified overlay entries.
        let mut overlay_pixels: Vec<OverlayPixel> = Vec::new();
        for &(rx, ry, classification, density, precomputed) in overlay_data {
            let color: [u8; 4] = if let Some(c) = precomputed {
                c
            } else if let Some(c) = classification.color(density) {
                c
            } else {
                continue;
            };
            let (px, py): (u32, u32) = world_to_minimap_pixel_from_cell(
                rx,
                ry,
                grid,
                w,
                h,
                map_offset_x,
                map_offset_y,
                map_pixel_w,
                map_pixel_h,
            );
            set_pixel(&mut rgba, size, px, py, color);
            overlay_pixels.push(OverlayPixel {
                rx,
                ry,
                px,
                py,
                color,
                classification,
            });
        }

        let (map_texture_raw, map_texture) = batch.create_updatable_texture(gpu, &rgba, size, size);
        let white_texture: BatchTexture = create_white_texture(gpu, batch);
        let rgba_scratch: Vec<u8> = vec![0u8; pixel_count];

        log::info!(
            "Minimap created: {}x{} px, {} terrain cells, {} overlay pixels",
            size,
            size,
            grid.cells.len(),
            overlay_pixels.len(),
        );

        Self {
            base_terrain_rgba: rgba,
            map_texture,
            map_texture_raw,
            rgba_scratch,
            white_texture,
            world_origin_x: grid.origin_x,
            world_origin_y: grid.origin_y,
            world_width: w,
            world_height: h,
            terrain_pixels,
            overlay_pixels,
            map_offset_x,
            map_offset_y,
            map_pixel_w,
            map_pixel_h,
        }
    }

    /// Update the minimap texture with unit dot overlays from the ECS world.
    ///
    /// Copies the base terrain image, stamps overlay pixels (ore, gems, walls,
    /// bridges, trees), then stamps a colored dot for each entity with
    /// Position + Owner, and re-uploads to the GPU.
    pub fn update_unit_dots(
        &mut self,
        gpu: &GpuContext,
        _batch: &BatchRenderer,
        entities: &crate::sim::entity_store::EntityStore,
        house_colors: &HouseColorMap,
        visibility: Option<(crate::sim::intern::InternedId, &FogState)>,
        rules: Option<&RuleSet>,
        radar_events: Option<&crate::sim::radar::RadarEventQueue>,
        interner: Option<&crate::sim::intern::StringInterner>,
    ) {
        let size: u32 = MINIMAP_SIZE;
        let rgba: &mut Vec<u8> = &mut self.rgba_scratch;

        // Fill scratch buffer: either shroud + fog-aware terrain, or base terrain copy.
        if let Some((local_owner, fog)) = visibility {
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.copy_from_slice(&COLOR_SHROUD);
            }
            for terrain_pixel in &self.terrain_pixels {
                let color = match cell_visibility_color(local_owner, fog, terrain_pixel) {
                    Some(color) => color,
                    None => continue,
                };
                set_pixel(rgba, size, terrain_pixel.px, terrain_pixel.py, color);
            }
        } else {
            rgba.copy_from_slice(&self.base_terrain_rgba);
        }

        // Stamp overlay pixels on top of terrain (ore, gems, walls, bridges, trees).
        for overlay in &self.overlay_pixels {
            if let Some((local_owner, fog)) = visibility {
                if !fog.is_cell_revealed(local_owner, overlay.rx, overlay.ry) {
                    continue;
                }
                let mut color: [u8; 4] = overlay.color;
                if overlay.classification == OverlayClassification::Bridge {
                    color = dim_color(color, 0.5);
                }
                set_pixel(rgba, size, overlay.px, overlay.py, color);
            } else {
                let mut color = overlay.color;
                if overlay.classification == OverlayClassification::Bridge {
                    color = dim_color(color, 0.5);
                }
                set_pixel(rgba, size, overlay.px, overlay.py, color);
            }
        }

        // Stamp unit dots on top of terrain + overlays.
        for entity in entities.values() {
            let pos = &entity.position;
            let type_str = interner.map_or("", |i| i.resolve(entity.type_ref));
            let owner_str = interner.map_or("", |i| i.resolve(entity.owner));
            let obj = rules.and_then(|r| r.object(type_str));
            let radar_invisible: bool = obj.is_some_and(|o| o.radar_invisible);
            let radar_visible: bool = obj.is_some_and(|o| o.radar_visible);

            if let Some((local_owner, fog)) = visibility {
                let friendly =
                    interner.map_or(false, |i| fog.is_friendly_id(local_owner, entity.owner, i));
                if radar_visible {
                    // Always show — RadarVisible overrides fog.
                } else if radar_invisible && !friendly {
                    continue;
                } else if !friendly
                    && (!fog.is_cell_revealed(local_owner, pos.rx, pos.ry)
                        || fog.is_cell_gap_covered(local_owner, pos.rx, pos.ry))
                {
                    continue;
                }
            }

            let (px, py): (u32, u32) = world_to_minimap_pixel(
                pos.screen_x,
                pos.screen_y,
                self.world_origin_x,
                self.world_origin_y,
                self.world_width,
                self.world_height,
                self.map_offset_x,
                self.map_offset_y,
                self.map_pixel_w,
                self.map_pixel_h,
            );

            let is_building = entity.category == EntityCategory::Structure;
            let color: [u8; 4] = if is_building {
                COLOR_BUILDING
            } else {
                owner_dot_color(owner_str, house_colors)
            };
            let dot_size: u32 = if is_building {
                let (fw, fh) = obj
                    .map(|o| parse_foundation_size(&o.foundation))
                    .unwrap_or((1, 1));
                (fw.max(fh) + 1).min(5)
            } else {
                DOT_SIZE
            };
            for dy in 0..dot_size {
                for dx in 0..dot_size {
                    let dot_x: u32 = px.saturating_add(dx);
                    let dot_y: u32 = py.saturating_add(dy);
                    set_pixel(rgba, size, dot_x, dot_y, color);
                }
            }
        }

        // Draw animated radar event diamonds on top of everything.
        if let Some(events) = radar_events {
            let config = rules.map(|r| &r.radar_event_config);
            for event in events.iter() {
                let (sx, sy) = crate::map::terrain::iso_to_screen(event.rx, event.ry, 0);
                let (cx, cy) = world_to_minimap_pixel(
                    sx,
                    sy,
                    self.world_origin_x,
                    self.world_origin_y,
                    self.world_width,
                    self.world_height,
                    self.map_offset_x,
                    self.map_offset_y,
                    self.map_pixel_w,
                    self.map_pixel_h,
                );
                let progress: f32 = event.progress();
                let min_radius: f32 = config.map_or(4.0, |c| c.min_radius);
                let start_radius: f32 = min_radius * 4.0;
                let radius: f32 = start_radius + (min_radius - start_radius) * progress;
                // Brightness pulses via sin wave.
                let color_speed: f32 = config.map_or(0.05, |c| c.color_speed);
                let pulse: f32 = 0.6 + 0.4 * (event.age_ms as f32 * color_speed * 0.01).sin().abs();
                let base_color = event.event_type.color();
                let r: u8 = (base_color[0] as f32 * pulse).min(255.0) as u8;
                let g: u8 = (base_color[1] as f32 * pulse).min(255.0) as u8;
                let b: u8 = (base_color[2] as f32 * pulse).min(255.0) as u8;
                let color: [u8; 4] = [r, g, b, 255];
                // Compute 4 diamond corners from rotation angle + radius.
                let cos_a = event.rotation.cos();
                let sin_a = event.rotation.sin();
                // Outer bright diamond.
                let cxi = cx as i32;
                let cyi = cy as i32;
                let corners: [(i32, i32); 4] = [
                    (cxi + (radius * cos_a) as i32, cyi + (radius * sin_a) as i32),
                    (cxi - (radius * sin_a) as i32, cyi + (radius * cos_a) as i32),
                    (cxi - (radius * cos_a) as i32, cyi - (radius * sin_a) as i32),
                    (cxi + (radius * sin_a) as i32, cyi - (radius * cos_a) as i32),
                ];
                for i in 0..4 {
                    let (x0, y0) = corners[i];
                    let (x1, y1) = corners[(i + 1) % 4];
                    draw_line(rgba, size, x0, y0, x1, y1, color);
                }
                // Inner dim diamond (70% radius, 50% brightness).
                let inner_r = radius * 0.7;
                if inner_r >= 1.0 {
                    let dim_color_val = dim_color(color, 0.5);
                    let inner: [(i32, i32); 4] = [
                        (
                            cxi + (inner_r * cos_a) as i32,
                            cyi + (inner_r * sin_a) as i32,
                        ),
                        (
                            cxi - (inner_r * sin_a) as i32,
                            cyi + (inner_r * cos_a) as i32,
                        ),
                        (
                            cxi - (inner_r * cos_a) as i32,
                            cyi - (inner_r * sin_a) as i32,
                        ),
                        (
                            cxi + (inner_r * sin_a) as i32,
                            cyi - (inner_r * cos_a) as i32,
                        ),
                    ];
                    for i in 0..4 {
                        let (x0, y0) = inner[i];
                        let (x1, y1) = inner[(i + 1) % 4];
                        draw_line(rgba, size, x0, y0, x1, y1, dim_color_val);
                    }
                }
            }
        }

        // Rewrite existing GPU texture instead of creating a new one.
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.map_texture_raw,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size * 4),
                rows_per_image: Some(size),
            },
            wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Build a SpriteInstance that fills the given screen rect with the minimap.
    ///
    /// The minimap stretches to fill the entire container, matching the original
    /// RA2 behavior where the radar fills its housing regardless of map aspect ratio.
    pub fn build_minimap_instance_in_rect(
        &self,
        camera_x: f32,
        camera_y: f32,
        screen_x: f32,
        screen_y: f32,
        width: f32,
        height: f32,
    ) -> SpriteInstance {
        SpriteInstance {
            position: [camera_x + screen_x, camera_y + screen_y],
            size: [width, height],
            uv_origin: [0.0, 0.0],
            uv_size: [1.0, 1.0],
            depth: MINIMAP_DEPTH,
            tint: [1.0, 1.0, 1.0],
            alpha: 1.0,
        }
    }

    /// Return true if a screen-space cursor position is inside the minimap rectangle.
    pub fn contains_screen_point_in_rect(
        &self,
        screen_x: f32,
        screen_y: f32,
        rect_x: f32,
        rect_y: f32,
        rect_w: f32,
        rect_h: f32,
    ) -> bool {
        screen_x >= rect_x
            && screen_x <= rect_x + rect_w
            && screen_y >= rect_y
            && screen_y <= rect_y + rect_h
    }

    /// Convert a screen-space minimap click/drag point to camera top-left world position.
    pub fn camera_top_left_for_screen_point_in_rect(
        &self,
        screen_x: f32,
        screen_y: f32,
        screen_w: f32,
        screen_h: f32,
        rect_x: f32,
        rect_y: f32,
        rect_w: f32,
        rect_h: f32,
    ) -> (f32, f32) {
        let size = MINIMAP_SIZE as f32;
        // Screen click → texture pixel → world normalized via aspect-fit sub-region.
        let tex_x = (screen_x - rect_x) / rect_w.max(1.0) * size;
        let tex_y = (screen_y - rect_y) / rect_h.max(1.0) * size;
        let nx = ((tex_x - self.map_offset_x) / self.map_pixel_w.max(1.0)).clamp(0.0, 1.0);
        let ny = ((tex_y - self.map_offset_y) / self.map_pixel_h.max(1.0)).clamp(0.0, 1.0);
        let world_x = self.world_origin_x + nx * self.world_width;
        let world_y = self.world_origin_y + ny * self.world_height;
        (world_x - screen_w * 0.5, world_y - screen_h * 0.5)
    }

    /// Build SpriteInstances for the camera viewport rectangle on the minimap.
    pub fn build_viewport_rect_in_rect(
        &self,
        camera_x: f32,
        camera_y: f32,
        screen_w: f32,
        screen_h: f32,
        rect_x: f32,
        rect_y: f32,
        rect_w: f32,
        rect_h: f32,
    ) -> Vec<SpriteInstance> {
        let size = MINIMAP_SIZE as f32;
        // World coords → normalized 0..1 → texture pixel via aspect-fit → screen proportion.
        let nx_left: f32 = (camera_x - self.world_origin_x) / self.world_width;
        let ny_top: f32 = (camera_y - self.world_origin_y) / self.world_height;
        let nx_right: f32 = (camera_x + screen_w - self.world_origin_x) / self.world_width;
        let ny_bottom: f32 = (camera_y + screen_h - self.world_origin_y) / self.world_height;

        let left: f32 =
            ((nx_left * self.map_pixel_w + self.map_offset_x) / size * rect_w).clamp(0.0, rect_w);
        let top: f32 =
            ((ny_top * self.map_pixel_h + self.map_offset_y) / size * rect_h).clamp(0.0, rect_h);
        let right: f32 =
            ((nx_right * self.map_pixel_w + self.map_offset_x) / size * rect_w).clamp(0.0, rect_w);
        let bottom: f32 =
            ((ny_bottom * self.map_pixel_h + self.map_offset_y) / size * rect_h).clamp(0.0, rect_h);

        let vp_w: f32 = right - left;
        let vp_h: f32 = bottom - top;
        let t: f32 = VIEWPORT_LINE_THICKNESS;

        let mut lines: Vec<SpriteInstance> = Vec::with_capacity(4);

        // Top edge.
        lines.push(SpriteInstance {
            position: [camera_x + rect_x + left, camera_y + rect_y + top],
            size: [vp_w, t],
            uv_origin: [0.0, 0.0],
            uv_size: [1.0, 1.0],
            depth: MINIMAP_DEPTH,
            tint: [1.0, 1.0, 1.0],
            alpha: 1.0,
        });
        // Bottom edge.
        lines.push(SpriteInstance {
            position: [camera_x + rect_x + left, camera_y + rect_y + bottom - t],
            size: [vp_w, t],
            uv_origin: [0.0, 0.0],
            uv_size: [1.0, 1.0],
            depth: MINIMAP_DEPTH,
            tint: [1.0, 1.0, 1.0],
            alpha: 1.0,
        });
        // Left edge.
        lines.push(SpriteInstance {
            position: [camera_x + rect_x + left, camera_y + rect_y + top],
            size: [t, vp_h],
            uv_origin: [0.0, 0.0],
            uv_size: [1.0, 1.0],
            depth: MINIMAP_DEPTH,
            tint: [1.0, 1.0, 1.0],
            alpha: 1.0,
        });
        // Right edge.
        lines.push(SpriteInstance {
            position: [camera_x + rect_x + right - t, camera_y + rect_y + top],
            size: [t, vp_h],
            uv_origin: [0.0, 0.0],
            uv_size: [1.0, 1.0],
            depth: MINIMAP_DEPTH,
            tint: [1.0, 1.0, 1.0],
            alpha: 1.0,
        });

        lines
    }

    /// Get a reference to the minimap texture for drawing.
    pub fn map_texture(&self) -> &BatchTexture {
        &self.map_texture
    }

    /// Get a reference to the white texture for drawing viewport lines.
    pub fn white_texture(&self) -> &BatchTexture {
        &self.white_texture
    }
}

/// Create a 2x2 solid white texture for drawing lines and rectangles.
fn create_white_texture(gpu: &GpuContext, batch: &BatchRenderer) -> BatchTexture {
    let white_rgba: [u8; 16] = [
        255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    ];
    batch.create_texture(gpu, &white_rgba, 2, 2)
}

#[cfg(test)]
#[path = "minimap_tests.rs"]
mod tests;
