//! GPU ABuffer — screen-resolution shroud brightness texture and multiply pass.
//!
//! Replicates the original engine's ABuffer system: a per-pixel brightness value
//! derived from SHROUD.SHP is used to darken the rendered scene. The original
//! engine writes brightness values into a 16-bit circular buffer, then each
//! tile/sprite blitter reads them per-pixel to modulate color. Our GPU equivalent:
//!
//! 1. CPU-side: blit SHROUD.SHP raw brightness pixels into a screen-res R8 buffer
//! 2. Upload to an R8Unorm GPU texture
//! 3. Full-screen multiply pass darkens the framebuffer per-pixel
//!
//! ## Dependency rules
//! - Part of render/ — reads FogState (no mutation), uses GpuContext.

use crate::map::terrain::iso_to_screen;
use crate::render::gpu::GpuContext;
use crate::sim::vision::FogState;

const SHADER_SRC: &str = include_str!("shroud_multiply.wgsl");

/// ABuffer neutral value — 0x7F = full brightness (no darkening).
const NEUTRAL: u8 = 0x7F;

/// ABuffer black value — 0x00 = fully shrouded.
const BLACK: u8 = 0x00;

/// SHP transparent pixel marker — skip (don't overwrite buffer).
const TRANSPARENT: u8 = 0xFE;

/// Shroud edge frame lookup table.
///
/// Indexed by 8-bit neighbor bitmask (see `FogState::shroud_edge_mask_8bit`).
/// Values: 0xFF = no edge needed, 0xFE = fully surrounded, 0-46 = frame index.
#[rustfmt::skip]
pub const SHROUD_EDGE_LUT: [u8; 256] = [
    // 0x00–0x0F
    0xFF, 0x21, 0x02, 0x02, 0x22, 0x25, 0x02, 0x02,
    0x04, 0x1A, 0x06, 0x06, 0x04, 0x1A, 0x06, 0x06,
    // 0x10–0x1F
    0x23, 0x2D, 0x11, 0x11, 0x26, 0x29, 0x11, 0x11,
    0x04, 0x1A, 0x06, 0x06, 0x04, 0x1A, 0x06, 0x06,
    // 0x20–0x2F
    0x08, 0x15, 0x0A, 0x0A, 0x1B, 0x1F, 0x0A, 0x0A,
    0x0C, 0x17, 0x0E, 0x0E, 0x0C, 0x17, 0x0E, 0x0E,
    // 0x30–0x3F
    0x08, 0x15, 0x0A, 0x0A, 0x1B, 0x1F, 0x0A, 0x0A,
    0x0C, 0x17, 0x0E, 0x0E, 0x0C, 0x17, 0x0E, 0x0E,
    // 0x40–0x4F
    0x20, 0x24, 0x19, 0x19, 0x2C, 0x28, 0x19, 0x19,
    0x13, 0x1E, 0x14, 0x14, 0x13, 0x1E, 0x14, 0x14,
    // 0x50–0x5F
    0x27, 0x2B, 0x1D, 0x1D, 0x2A, 0x2E, 0x1D, 0x1D,
    0x13, 0x1E, 0x14, 0x14, 0x13, 0x1E, 0x14, 0x14,
    // 0x60–0x6F
    0x08, 0x15, 0x0A, 0x0A, 0x1B, 0x1F, 0x0A, 0x0A,
    0x0C, 0x17, 0x0E, 0x0E, 0x0C, 0x17, 0x0E, 0x0E,
    // 0x70–0x7F
    0x08, 0x15, 0x0A, 0x0A, 0x1B, 0x1F, 0x0A, 0x0A,
    0x0C, 0x17, 0x0E, 0x0E, 0x0C, 0x17, 0x0E, 0x0E,
    // 0x80–0x8F
    0x01, 0x01, 0x03, 0x03, 0x10, 0x10, 0x03, 0x03,
    0x05, 0x05, 0x07, 0x07, 0x05, 0x05, 0x07, 0x07,
    // 0x90–0x9F
    0x18, 0x18, 0x12, 0x12, 0x1C, 0x1C, 0x12, 0x12,
    0x05, 0x05, 0x07, 0x07, 0x05, 0x05, 0x07, 0x07,
    // 0xA0–0xAF
    0x09, 0x09, 0x0B, 0x0B, 0x16, 0x16, 0x0B, 0x0B,
    0x0D, 0x0D, 0xFE, 0xFE, 0x0D, 0x0D, 0xFE, 0xFE,
    // 0xB0–0xBF
    0x09, 0x09, 0x0B, 0x0B, 0x16, 0x16, 0x0B, 0x0B,
    0x0D, 0x0D, 0xFE, 0xFE, 0x0D, 0x0D, 0xFE, 0xFE,
    // 0xC0–0xCF
    0x01, 0x01, 0x03, 0x03, 0x10, 0x10, 0x03, 0x03,
    0x05, 0x05, 0x07, 0x07, 0x05, 0x05, 0x07, 0x07,
    // 0xD0–0xDF
    0x18, 0x18, 0x12, 0x12, 0x1C, 0x1C, 0x12, 0x12,
    0x05, 0x05, 0x07, 0x07, 0x05, 0x05, 0x07, 0x07,
    // 0xE0–0xEF
    0x09, 0x09, 0x0B, 0x0B, 0x16, 0x16, 0x0B, 0x0B,
    0x0D, 0x0D, 0xFE, 0xFE, 0x0D, 0x0D, 0xFE, 0xFE,
    // 0xF0–0xFF
    0x09, 0x09, 0x0B, 0x0B, 0x16, 0x16, 0x0B, 0x0B,
    0x0D, 0x0D, 0xFE, 0xFE, 0x0D, 0x0D, 0xFE, 0xFE,
];

/// GPU ABuffer: screen-resolution R8 brightness texture + multiply pipeline.
pub struct ShroudBuffer {
    /// CPU-side brightness buffer (one byte per screen pixel).
    /// 0x00 = black, 0x7F = full brightness. Stored with padded row stride
    /// for GPU upload alignment.
    pixels: Vec<u8>,
    /// Actual screen width.
    width: u32,
    /// Actual screen height.
    height: u32,
    /// Padded bytes-per-row (aligned to 256 for wgpu).
    row_stride: u32,
    /// GPU R8 texture.
    texture: wgpu::Texture,
    /// Bind group for the multiply shader.
    bind_group: wgpu::BindGroup,
    /// Bind group layout (needed for recreation on resize).
    bgl: wgpu::BindGroupLayout,
    /// Multiply-blend render pipeline.
    pipeline: wgpu::RenderPipeline,
    /// Raw brightness pixels per SHROUD.SHP frame (canvas_w × canvas_h each).
    /// Pixel values: 0x00=black, 0x7F=clear, 0xFE=transparent.
    frame_pixels: Vec<Vec<u8>>,
    /// SHP canvas width (typically 60).
    canvas_w: u32,
    /// SHP canvas height (typically 30).
    canvas_h: u32,
    /// Cached camera X for change detection.
    last_cam_x: f32,
    /// Cached camera Y for change detection.
    last_cam_y: f32,
    /// Cached fog generation for change detection.
    last_fog_gen: u64,
    /// Cached screen width for resize detection.
    last_screen_w: u32,
    /// Cached screen height for resize detection.
    last_screen_h: u32,
    /// Cached zoom level for change detection.
    last_zoom: f32,
    /// Map dimensions in cells.
    map_width: u16,
    map_height: u16,
    /// 256-byte LUT mapping neighbor bitmask to SHROUD.SHP frame index.
    lut: [u8; 256],
}

/// Align `n` up to the next multiple of `align`.
fn align_up(n: u32, align: u32) -> u32 {
    (n + align - 1) / align * align
}

impl ShroudBuffer {
    /// Create a new shroud buffer for the given screen and map dimensions.
    ///
    /// `frame_pixels` is the raw SHROUD.SHP brightness data per frame,
    /// extracted by the caller from the SHP file.
    pub fn new(
        gpu: &GpuContext,
        screen_w: u32,
        screen_h: u32,
        map_width: u16,
        map_height: u16,
        frame_pixels: Vec<Vec<u8>>,
        canvas_w: u32,
        canvas_h: u32,
        lut: [u8; 256],
    ) -> Self {
        let row_stride = align_up(screen_w, 256);
        let pixels = vec![NEUTRAL; (row_stride * screen_h) as usize];

        let texture = create_r8_texture(gpu, screen_w, screen_h);
        let bgl = create_bgl(gpu);
        let bind_group = create_bind_group(gpu, &bgl, &texture);
        let pipeline = create_pipeline(gpu, &bgl);

        Self {
            pixels,
            width: screen_w,
            height: screen_h,
            row_stride,
            texture,
            bind_group,
            bgl,
            pipeline,
            frame_pixels,
            canvas_w,
            canvas_h,
            last_cam_x: f32::NAN,
            last_cam_y: f32::NAN,
            last_fog_gen: u64::MAX,
            last_screen_w: screen_w,
            last_screen_h: screen_h,
            last_zoom: 1.0,
            map_width,
            map_height,
            lut,
        }
    }

    /// Rebuild the shroud buffer if camera moved, fog changed, or screen resized.
    ///
    /// Blits SHROUD.SHP brightness pixels into the CPU buffer matching the
    /// original ABuffer fill order, then uploads to GPU.
    pub fn rebuild_if_needed(
        &mut self,
        gpu: &GpuContext,
        fog: &FogState,
        owner: crate::sim::intern::InternedId,
        cam_x: f32,
        cam_y: f32,
        screen_w: u32,
        screen_h: u32,
        zoom: f32,
        height_grid: Option<&[u8]>,
    ) {
        // Render shroud at virtual resolution (screen / zoom) so diamond blits
        // stay at fixed world-pixel sizes. The fullscreen GPU shader stretches the
        // texture to fill the screen, naturally applying the zoom.
        // Cap at 4096 to avoid runaway allocation at extreme zoom-out.
        let virt_w = ((screen_w as f32 / zoom).ceil() as u32).min(4096);
        let virt_h = ((screen_h as f32 / zoom).ceil() as u32).min(4096);

        // Resize GPU texture if virtual dimensions changed.
        if virt_w != self.width
            || virt_h != self.height
            || screen_w != self.last_screen_w
            || screen_h != self.last_screen_h
        {
            self.width = virt_w;
            self.height = virt_h;
            self.row_stride = align_up(virt_w, 256);
            self.pixels
                .resize((self.row_stride * virt_h) as usize, NEUTRAL);
            self.texture = create_r8_texture(gpu, virt_w, virt_h);
            self.bind_group = create_bind_group(gpu, &self.bgl, &self.texture);
            self.last_screen_w = screen_w;
            self.last_screen_h = screen_h;
            // Force rebuild after resize.
            self.last_fog_gen = u64::MAX;
        }

        // Skip if nothing changed (camera rounded to pixel + fog generation + zoom).
        let cam_x_r = cam_x.floor();
        let cam_y_r = cam_y.floor();
        if fog.generation == self.last_fog_gen
            && cam_x_r == self.last_cam_x
            && cam_y_r == self.last_cam_y
            && (zoom - self.last_zoom).abs() < 1e-6
        {
            return;
        }
        self.last_fog_gen = fog.generation;
        self.last_cam_x = cam_x_r;
        self.last_cam_y = cam_y_r;
        self.last_zoom = zoom;

        // Fill bright, then darken unrevealed cells and blit edge transitions.
        // Blitting in world-pixel coordinates at virtual resolution
        // means diamond tiles match the world grid exactly; the GPU stretch handles zoom.
        self.pixels.fill(NEUTRAL);

        let vp_w = virt_w as i32;
        let vp_h = virt_h as i32;
        let cam_xi = cam_x_r as i32;
        let cam_yi = cam_y_r as i32;

        for ry in 0..self.map_height {
            for rx in 0..self.map_width {
                // Position shroud diamond at the cell's actual terrain height so
                // the brightness buffer aligns with where the terrain renders.
                // Fully shrouded cells aren't rendered as terrain at all (filtered
                // in build_visible_instances), so alignment only matters for edges.
                let cell_z = height_grid
                    .and_then(|hg| {
                        let idx = ry as usize * self.map_width as usize + rx as usize;
                        hg.get(idx).copied()
                    })
                    .unwrap_or(0);
                let (sx, sy) = iso_to_screen(rx, ry, cell_z);
                let vx = sx as i32 - cam_xi;
                let vy = sy as i32 - cam_yi;

                if vx + self.canvas_w as i32 <= 0
                    || vx >= vp_w
                    || vy + self.canvas_h as i32 <= 0
                    || vy >= vp_h
                {
                    continue;
                }

                if !fog.is_cell_revealed(owner, rx, ry) {
                    self.blit_dark_diamond(vx, vy, vp_w, vp_h);
                    continue;
                }

                let bitmask = fog.shroud_edge_mask_8bit(owner, rx, ry);
                if bitmask == 0 {
                    continue; // Fully revealed — already bright.
                }
                let lut_val = self.lut[bitmask as usize];
                match lut_val {
                    0xFF => continue,
                    0xFE => {
                        // Fully surrounded by shroud — use SHP frame 15
                        // (full 60x30 black diamond). Adjacent cells' frames
                        // cover frame 15's missing row-0 tip.
                        self.blit_frame(15, vx, vy, vp_w, vp_h);
                    }
                    idx => {
                        self.blit_frame(idx as usize, vx, vy, vp_w, vp_h);
                    }
                }
            }
        }

        // Upload to GPU.
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &self.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.row_stride),
                rows_per_image: Some(self.height),
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Blit one SHROUD.SHP frame's raw brightness pixels into the CPU buffer.
    ///
    /// Coordinates are in viewport space (0,0 = top-left of screen).
    /// Handles clipping and transparent pixel skip (0xFE).
    fn blit_frame(&mut self, frame_idx: usize, vx: i32, vy: i32, vp_w: i32, vp_h: i32) {
        let Some(frame_data) = self.frame_pixels.get(frame_idx) else {
            return;
        };
        let cw = self.canvas_w as i32;
        let ch = self.canvas_h as i32;

        // Compute clipped source/dest rectangles.
        let src_x0 = (-vx).max(0);
        let src_y0 = (-vy).max(0);
        let dst_x0 = vx.max(0);
        let dst_y0 = vy.max(0);
        let x_end = (vx + cw).min(vp_w);
        let y_end = (vy + ch).min(vp_h);

        if dst_x0 >= x_end || dst_y0 >= y_end {
            return;
        }

        let stride = self.row_stride as usize;
        for row in 0..(y_end - dst_y0) {
            let src_row = (src_y0 + row) as u32;
            let dst_row = (dst_y0 + row) as u32;
            let src_base = (src_row * self.canvas_w) as usize;
            let dst_base = (dst_row as usize) * stride + dst_x0 as usize;

            for col in 0..(x_end - dst_x0) {
                let src_col = (src_x0 + col) as usize;
                let pixel = frame_data[src_base + src_col];
                if pixel != TRANSPARENT {
                    self.pixels[dst_base + col as usize] = pixel;
                }
            }
        }
    }

    /// Fill the cell's diamond area with a given value (NEUTRAL or BLACK).
    ///
    /// Uses the exact isometric diamond geometry from SHROUD.SHP (60x30 canvas,
    /// rows expand by 4px per row, widest at center). Extends to row 0 which
    /// frame 15 leaves empty — without this, the top-pixel seam stays black.
    fn blit_diamond(&mut self, vx: i32, vy: i32, vp_w: i32, vp_h: i32, value: u8) {
        let cw = self.canvas_w as i32; // 60
        let ch = self.canvas_h as i32; // 30
        let half_w = cw / 2; // 30
        let half_h = ch / 2; // 15
        let stride = self.row_stride as usize;

        // The diamond expands 2px per side per row from the tip.
        // Row 0: width 2 (cols 29..30), row 1: width 4 (cols 28..31), ...
        // row 15: width 60 (cols 0..59), then contracts symmetrically.
        // Frame 15 starts at row 1 (width 4), missing row 0. We include it.
        for row in 0..ch {
            let dy = vy + row;
            if dy < 0 || dy >= vp_h {
                continue;
            }
            // Distance from center row (row 15 for 30-high canvas).
            let dist = (row - half_h).abs();
            // Half-width at this row: at center = half_w, shrinks by 2 per row.
            let half_row_w = half_w - dist * 2;
            if half_row_w <= 0 {
                continue;
            }
            let x_start = (vx + half_w - half_row_w).max(0);
            let x_end = (vx + half_w + half_row_w).min(vp_w);
            if x_start >= x_end {
                continue;
            }
            let dst_base = (dy as usize) * stride;
            for x in x_start..x_end {
                self.pixels[dst_base + x as usize] = value;
            }
        }
    }

    /// Fill the cell's diamond area with 0x00 (full shroud).
    fn blit_dark_diamond(&mut self, vx: i32, vy: i32, vp_w: i32, vp_h: i32) {
        self.blit_diamond(vx, vy, vp_w, vp_h, BLACK);
    }

    /// Draw the full-screen multiply pass, darkening the framebuffer by the
    /// shroud buffer brightness values.
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..6, 0..1);
    }
}

/// Extract raw brightness pixel data from a loaded SHROUD.SHP file.
///
/// Returns `(frame_pixels, canvas_w, canvas_h)` where each entry in
/// `frame_pixels` is a `canvas_w × canvas_h` buffer of raw SHP pixel values.
pub fn extract_shp_brightness(shp: &crate::assets::shp_file::ShpFile) -> (Vec<Vec<u8>>, u32, u32) {
    let canvas_w = shp.width as u32;
    let canvas_h = shp.height as u32;
    let canvas_size = (canvas_w * canvas_h) as usize;
    let mut all_frames: Vec<Vec<u8>> = Vec::with_capacity(shp.frames.len());

    for frame in &shp.frames {
        // Start with TRANSPARENT so pixels outside the actual SHP subframe
        // are skipped by blit_frame(). Only real SHP pixels affect the buffer.
        let mut buf = vec![TRANSPARENT; canvas_size];
        let fw = frame.frame_width as u32;
        let fh = frame.frame_height as u32;
        let fx = frame.frame_x as u32;
        let fy = frame.frame_y as u32;

        for row in 0..fh {
            for col in 0..fw {
                let src = (row * fw + col) as usize;
                let pixel = frame.pixels[src];
                let dx = fx + col;
                let dy = fy + row;
                if dx < canvas_w && dy < canvas_h {
                    buf[(dy * canvas_w + dx) as usize] = pixel;
                }
            }
        }
        all_frames.push(buf);
    }

    (all_frames, canvas_w, canvas_h)
}

// ---------------------------------------------------------------------------
// GPU resource helpers
// ---------------------------------------------------------------------------

fn create_r8_texture(gpu: &GpuContext, w: u32, h: u32) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Shroud ABuffer Texture"),
        size: wgpu::Extent3d {
            width: w.max(1),
            height: h.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn create_bgl(gpu: &GpuContext) -> wgpu::BindGroupLayout {
    gpu.device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Shroud ABuffer BGL"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        })
}

fn create_bind_group(
    gpu: &GpuContext,
    bgl: &wgpu::BindGroupLayout,
    texture: &wgpu::Texture,
) -> wgpu::BindGroup {
    let view = texture.create_view(&Default::default());
    let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("Shroud ABuffer Sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        ..Default::default()
    });
    gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Shroud ABuffer BG"),
        layout: bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    })
}

fn create_pipeline(gpu: &GpuContext, bgl: &wgpu::BindGroupLayout) -> wgpu::RenderPipeline {
    let shader = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Shroud Multiply Shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER_SRC.into()),
        });

    let layout = gpu
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Shroud Multiply Pipeline Layout"),
            bind_group_layouts: &[bgl],
            push_constant_ranges: &[],
        });

    gpu.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Shroud Multiply Pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: gpu.surface_format,
                    // Multiplicative blending: final = src * dst.
                    // src = shroud brightness (0–1), dst = existing scene color.
                    // Result: scene pixels are darkened by the shroud value.
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::Zero,
                            dst_factor: wgpu::BlendFactor::Src,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::Zero,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            // Must specify depth format to match the main render pass, but
            // the multiply pass does not read or write depth.
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        })
}
