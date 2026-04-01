//! Software voxel rasterizer — renders VXL models to 2D RGBA sprites.
//!
//! Uses YR's original back-to-front spatial iteration: voxels are processed
//! layer-by-layer in the order determined by the camera transform, so closer
//! voxels naturally overwrite farther ones (painter's algorithm). Each voxel
//! is projected to screen space and drawn as a small filled rectangle, giving
//! the authentic flat-pixel look of the original game.
//!
//! Lighting uses Blinn-Phong via VPL lookup tables when available, falling
//! back to simple N·L diffuse otherwise.
//!
//! Camera: 60° isometric tilt + 45° world rotation (matching RA2/YR original).
//!
//! ## Dependency rules
//! - Part of render/ — depends on assets/ (VxlFile, HvaFile, Palette, VplFile).
//! - Uses glam for vector/matrix math.

use glam::{Mat4, Vec3, Vec4};

use crate::assets::hva_file::HvaFile;
use crate::assets::pal_file::{Color, Palette};
use crate::assets::vpl_file::VplFile;
use crate::assets::vxl_file::{VxlFile, VxlLimb};
use crate::render::vxl_normals;

/// Isometric camera pitch (60°, matching the original engine's isometric projection).
const CAMERA_PITCH_DEG: f32 = 60.0;

/// World yaw offset (45°) to align model north with isometric grid.
const WORLD_YAW_OFFSET_DEG: f32 = 45.0;

/// Margin in pixels added around the sprite to avoid clipping.
const SPRITE_MARGIN: u32 = 2;

/// Fixed-point shift for 16.16 integer projection.
/// The original RA2/TS voxel renderer used integer math with truncation,
/// producing characteristic pixel-snapping artifacts. We replicate this by
/// projecting through 16.16 fixed-point and truncating (>> 16) rather than
/// rounding, giving voxels the same "crunchy" pixel look as SHP sprites.
const FP_SHIFT: i32 = 16;
const FP_SCALE: f32 = (1 << FP_SHIFT) as f32; // 65536.0

/// Edge ramp tilt angle (slope types 1-4): `atan(tan(30°) / sqrt(2))`.
/// Derived from RA2's isometric geometry — one full cell edge raised by one height
/// level. `VXL_Init_EdgeTiltAngle` computes `atan(2h / diag)`.
const EDGE_TILT_RAD: f32 = 0.3876;

/// Corner ramp tilt angle (slope types 5-8): `atan(tan(30°) / 2)`.
/// One corner raised by one height level across the cell diagonal.
/// `VXL_Init_CornerTiltAngle` computes `atan(h / 256)`.
const CORNER_TILT_RAD: f32 = 0.2810;

/// Configuration for rendering a single VXL model frame.
#[derive(Debug, Clone)]
pub struct VxlRenderParams {
    /// HVA animation frame index (0-based). Use 0 for idle pose.
    pub frame: u32,
    /// Facing angle: 0–255 maps to 0–360° (RA2 convention).
    pub facing: u8,
    /// Terrain slope type (0–8). 0 = flat, 1-4 = edge ramps, 5-8 = corner ramps.
    /// The VXL model is tilted to match the terrain slope before camera projection.
    pub slope_type: u8,
    /// Pixel scale factor. Higher = larger sprite. Default: 1.045.
    /// TS default is CellSizeX=48; RA2 uses CellSizeX=60. Base scale = 60/48
    /// = 1.25, reduced by 16.4% (1.25 * 0.836 = 1.045) to match original RA2
    /// voxel proportions on the isometric grid.
    pub scale: f32,
    /// Ambient light intensity for fallback N·L shading. Default: 0.6.
    pub ambient: f32,
    /// Diffuse light intensity for fallback N·L shading. Default: 0.4.
    pub diffuse: f32,
    /// Light direction for fallback shading (when no VPL).
    pub light_dir: Vec3,
}

impl Default for VxlRenderParams {
    fn default() -> Self {
        let pitch: f32 = 50.0_f32.to_radians();
        let yaw: f32 = 240.0_f32.to_radians();
        let light_dir: Vec3 = Vec3::new(
            yaw.cos() * pitch.cos(),
            yaw.sin() * pitch.cos(),
            pitch.sin(),
        );
        Self {
            frame: 0,
            facing: 0,
            slope_type: 0,
            scale: 1.045,
            ambient: 0.6,
            diffuse: 0.4,
            light_dir,
        }
    }
}

/// A rendered 2D sprite produced by the software voxel rasterizer.
#[derive(Debug, Clone)]
pub struct VxlSprite {
    /// RGBA pixel data (width × height × 4 bytes).
    pub rgba: Vec<u8>,
    /// Per-pixel depth buffer (width × height floats). Used for depth-correct
    /// compositing of body/turret/barrel layers. NEG_INFINITY = no voxel.
    pub depth: Vec<f32>,
    /// Sprite width in pixels.
    pub width: u32,
    /// Sprite height in pixels.
    pub height: u32,
    /// X offset from model center to sprite top-left.
    pub offset_x: f32,
    /// Y offset from model center to sprite top-left.
    pub offset_y: f32,
}

// ---------------------------------------------------------------------------
// Packed voxel grid — 3D lookup built from the sparse Vec<VxlVoxel>.
// High byte = color_index, low byte = normal_index. Zero = empty cell.
// ---------------------------------------------------------------------------

/// Packed voxel data: `(color_index << 8) | normal_index`. Zero = empty.
type PackedVoxel = u16;

fn pack_voxel(color_index: u8, normal_index: u8) -> PackedVoxel {
    (color_index as u16) << 8 | normal_index as u16
}

fn unpack_color(v: PackedVoxel) -> u8 {
    (v >> 8) as u8
}

fn unpack_normal(v: PackedVoxel) -> u8 {
    (v & 0xFF) as u8
}

/// Build a dense 3D grid from a limb's sparse voxel list.
/// Indexed as `grid[x * sy * sz + y * sz + z]`.
fn build_voxel_grid(limb: &VxlLimb) -> Vec<PackedVoxel> {
    let sy: usize = limb.size_y as usize;
    let sz: usize = limb.size_z as usize;
    let total: usize = limb.size_x as usize * sy * sz;
    let mut grid: Vec<PackedVoxel> = vec![0u16; total];
    for v in &limb.voxels {
        let idx: usize = v.x as usize * sy * sz + v.y as usize * sz + v.z as usize;
        grid[idx] = pack_voxel(v.color_index, v.normal_index);
    }
    grid
}

// ---------------------------------------------------------------------------
// Back-to-front axis iteration — determines which direction to walk each
// axis so that farther voxels are processed first (painter's algorithm).
// ---------------------------------------------------------------------------

/// Iterator parameters for one axis: start, exclusive end, step direction.
struct AxisIter {
    start: i32,
    end: i32,
    step: i32,
}

impl AxisIter {
    /// Produce an iterator yielding values from start toward end by step.
    fn iter(&self) -> AxisRange {
        AxisRange {
            current: self.start,
            end: self.end,
            step: self.step,
        }
    }
}

struct AxisRange {
    current: i32,
    end: i32,
    step: i32,
}

impl Iterator for AxisRange {
    type Item = i32;
    fn next(&mut self) -> Option<i32> {
        if self.step > 0 && self.current >= self.end {
            return None;
        }
        if self.step < 0 && self.current <= self.end {
            return None;
        }
        let val: i32 = self.current;
        self.current += self.step;
        Some(val)
    }
}

/// Choose iteration direction for one axis based on the camera transform.
/// If moving along +axis increases depth (closer to camera), iterate
/// low→high so far voxels are drawn first and near voxels overwrite.
fn axis_order(size: u8, depth_contribution: f32) -> AxisIter {
    if depth_contribution >= 0.0 {
        // +axis = closer to camera → iterate low (far) to high (near).
        AxisIter {
            start: 0,
            end: size as i32,
            step: 1,
        }
    } else {
        // +axis = farther from camera → iterate high (far) to low (near).
        AxisIter {
            start: size as i32 - 1,
            end: -1,
            step: -1,
        }
    }
}

// ---------------------------------------------------------------------------
// Precomputed per-limb data gathered before rendering begins.
// ---------------------------------------------------------------------------

/// Per-limb precomputed data for the two-phase render pipeline.
/// Public so the GPU compute renderer can reuse the transform computation.
pub struct LimbRenderData {
    pub grid: Vec<PackedVoxel>,
    pub combined: Mat4,
    pub vpl_pages: [u8; 256],
    pub normals_mode: u8,
    pub size_x: u8,
    pub size_y: u8,
    pub size_z: u8,
}

/// Bounding box and layout info for a rendered VXL sprite.
/// Produced by `compute_sprite_bounds` and consumed by the rasterizer or GPU compute.
pub struct SpriteBounds {
    pub width: u32,
    pub height: u32,
    pub fill_size: i32,
    pub half_fill: i32,
    pub buf_off_x_fp: i32,
    pub buf_off_y_fp: i32,
    pub offset_x: f32,
    pub offset_y: f32,
}

/// Compute the slope rotation matrix for a given terrain slope type (0–8).
///
/// Formula: `slope_matrix = Rz(compass) * Rx(tilt) * Rz(-compass)`
/// where compass is the slope direction angle and tilt is the pitch amount.
///
/// Returns `Mat4::IDENTITY` for slope_type 0 (flat) or unknown types (9+).
fn compute_slope_rotation(slope_type: u8) -> Mat4 {
    let (compass_rad, tilt_rad): (f32, f32) = match slope_type {
        0 => return Mat4::IDENTITY,
        // Edge ramps (two adjacent corners raised one height level).
        1 => (4.7124, EDGE_TILT_RAD),               // West,  270°
        2 => (std::f32::consts::PI, EDGE_TILT_RAD), // North, 180°
        3 => (std::f32::consts::FRAC_PI_2, EDGE_TILT_RAD), // East,  90°
        4 => (0.0, EDGE_TILT_RAD),                  // South, 0°
        // Corner ramps (one corner raised one height level).
        5 => (3.9270, CORNER_TILT_RAD), // NW, 225°
        6 => (2.3562, CORNER_TILT_RAD), // NE, 135°
        7 => (0.7854, CORNER_TILT_RAD), // SE, 45°
        8 => (5.4978, CORNER_TILT_RAD), // SW, 315°
        _ => return Mat4::IDENTITY,     // slopes 9-20: treat as flat for now
    };
    Mat4::from_rotation_z(compass_rad)
        * Mat4::from_rotation_x(tilt_rad)
        * Mat4::from_rotation_z(-compass_rad)
}

/// Precompute per-limb transforms, voxel grids, lighting pages, and footprints.
///
/// This is Phase 1 of the VXL render pipeline. It builds the combined
/// world+section transform for each non-empty limb, computes VPL brightness
/// pages, and returns the maximum voxel footprint. Both the CPU rasterizer
/// and the GPU compute renderer use this function.
pub fn prepare_limb_data(
    vxl: &VxlFile,
    hva: Option<&HvaFile>,
    params: &VxlRenderParams,
) -> (Vec<LimbRenderData>, f32) {
    let facing_rad: f32 = (params.facing as f32) / 256.0 * std::f32::consts::TAU;
    let scale: f32 = params.scale;

    // World rotation matching YR: RotZ(45° - facing) then RotX(-60°).
    let rotate_to_world: Mat4 = Mat4::from_rotation_x(-CAMERA_PITCH_DEG.to_radians())
        * Mat4::from_rotation_z(WORLD_YAW_OFFSET_DEG.to_radians() - facing_rad);

    let mut limb_data: Vec<LimbRenderData> = Vec::new();
    let mut max_footprint: f32 = 1.0;

    for (limb_idx, limb) in vxl.limbs.iter().enumerate() {
        if limb.voxels.is_empty() {
            continue;
        }

        let vpl_pages: [u8; 256] = vxl_normals::blinn_phong_pages(limb.normals_mode, facing_rad);

        // Section scale: maps grid coordinates to model-space units.
        let sx: f32 = if limb.size_x > 0 {
            (limb.bounds[3] - limb.bounds[0]) / limb.size_x as f32
        } else {
            1.0
        };
        let sy: f32 = if limb.size_y > 0 {
            (limb.bounds[4] - limb.bounds[1]) / limb.size_y as f32
        } else {
            1.0
        };
        let sz: f32 = if limb.size_z > 0 {
            (limb.bounds[5] - limb.bounds[2]) / limb.size_z as f32
        } else {
            1.0
        };

        let section_scale: Mat4 = Mat4::from_scale(Vec3::new(sx, sy, sz));
        let section_translate: Mat4 =
            Mat4::from_translation(Vec3::new(limb.bounds[0], limb.bounds[1], limb.bounds[2]));

        let bone_mat: Mat4 = match hva {
            Some(h) => match h.get_transform(params.frame, limb_idx as u32) {
                Some(raw) => hva_to_mat4(raw, limb.scale),
                None => hva_to_mat4(&limb.transform, limb.scale),
            },
            None => hva_to_mat4(&limb.transform, limb.scale),
        };

        let section_transform: Mat4 = section_translate * bone_mat * section_scale;
        let slope_mat: Mat4 = compute_slope_rotation(params.slope_type);
        let combined: Mat4 = rotate_to_world * slope_mat * section_transform;

        let footprint: f32 = compute_voxel_footprint(&combined, scale);
        if footprint > max_footprint {
            max_footprint = footprint;
        }

        let grid: Vec<PackedVoxel> = build_voxel_grid(limb);

        limb_data.push(LimbRenderData {
            grid,
            combined,
            vpl_pages,
            normals_mode: limb.normals_mode,
            size_x: limb.size_x,
            size_y: limb.size_y,
            size_z: limb.size_z,
        });
    }

    (limb_data, max_footprint)
}

/// Compute the sprite bounding box from precomputed limb transforms.
///
/// This is Phase 2 of the VXL render pipeline. It projects the 8 corners of
/// each limb's voxel grid through the combined transform to find the screen-
/// space bounding box, then computes pixel dimensions and buffer offsets.
/// Both the CPU rasterizer and the GPU compute renderer use this function.
pub fn compute_sprite_bounds(
    limb_data: &[LimbRenderData],
    scale: f32,
    max_footprint: f32,
) -> SpriteBounds {
    let fill_size: i32 = max_footprint.ceil().max(1.0) as i32;
    let half_fill: i32 = fill_size / 2;

    let mut min_x_fp: i32 = i32::MAX;
    let mut max_x_fp: i32 = i32::MIN;
    let mut min_y_fp: i32 = i32::MAX;
    let mut max_y_fp: i32 = i32::MIN;

    for ld in limb_data {
        let gx: f32 = (ld.size_x as i32 - 1).max(0) as f32;
        let gy: f32 = (ld.size_y as i32 - 1).max(0) as f32;
        let gz: f32 = (ld.size_z as i32 - 1).max(0) as f32;
        let corners: [Vec3; 8] = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(gx, 0.0, 0.0),
            Vec3::new(0.0, gy, 0.0),
            Vec3::new(gx, gy, 0.0),
            Vec3::new(0.0, 0.0, gz),
            Vec3::new(gx, 0.0, gz),
            Vec3::new(0.0, gy, gz),
            Vec3::new(gx, gy, gz),
        ];
        for corner in &corners {
            let world_pos: Vec3 = ld.combined.transform_point3(*corner);
            let sx_fp: i32 = (world_pos.x * scale * FP_SCALE) as i32;
            let sy_fp: i32 = (-world_pos.y * scale * FP_SCALE) as i32;
            if sx_fp < min_x_fp {
                min_x_fp = sx_fp;
            }
            if sx_fp > max_x_fp {
                max_x_fp = sx_fp;
            }
            if sy_fp < min_y_fp {
                min_y_fp = sy_fp;
            }
            if sy_fp > max_y_fp {
                max_y_fp = sy_fp;
            }
        }
    }

    let width: u32 =
        ((max_x_fp - min_x_fp) >> FP_SHIFT) as u32 + 1 + SPRITE_MARGIN * 2 + fill_size as u32;
    let height: u32 =
        ((max_y_fp - min_y_fp) >> FP_SHIFT) as u32 + 1 + SPRITE_MARGIN * 2 + fill_size as u32;

    let margin_fp: i32 = (SPRITE_MARGIN as i32 + half_fill) << FP_SHIFT;
    let buf_off_x_fp: i32 = -min_x_fp + margin_fp;
    let buf_off_y_fp: i32 = -min_y_fp + margin_fp;

    let offset_x: f32 = (min_x_fp >> FP_SHIFT) as f32 - SPRITE_MARGIN as f32 - half_fill as f32;
    let offset_y: f32 = (min_y_fp >> FP_SHIFT) as f32 - SPRITE_MARGIN as f32 - half_fill as f32;

    SpriteBounds {
        width,
        height,
        fill_size,
        half_fill,
        buf_off_x_fp,
        buf_off_y_fp,
        offset_x,
        offset_y,
    }
}

/// Render a VXL model to a 2D RGBA sprite using back-to-front spatial iteration.
///
/// Voxels are iterated in spatial order determined by the camera transform so
/// that closer voxels naturally overwrite farther ones (painter's algorithm).
/// Each voxel is projected and drawn as a small filled rectangle. Z-buffer is
/// maintained for inter-limb occlusion and downstream body/turret compositing.
pub fn render_vxl(
    vxl: &VxlFile,
    hva: Option<&HvaFile>,
    palette: &Palette,
    params: &VxlRenderParams,
    vpl: Option<&VplFile>,
) -> VxlSprite {
    let facing_rad: f32 = (params.facing as f32) / 256.0 * std::f32::consts::TAU;
    let scale: f32 = params.scale;

    // Phase 1: Precompute per-limb transforms, grids, and footprints.
    let (limb_data, max_footprint) = prepare_limb_data(vxl, hva, params);

    // Handle empty models.
    if limb_data.is_empty() {
        return VxlSprite {
            rgba: vec![0, 0, 0, 0],
            depth: vec![f32::NEG_INFINITY],
            width: 1,
            height: 1,
            offset_x: 0.0,
            offset_y: 0.0,
        };
    }

    // Phase 2: Compute bounding box from grid corners (fixed-point).
    let bounds: SpriteBounds = compute_sprite_bounds(&limb_data, scale, max_footprint);
    let width: u32 = bounds.width;
    let height: u32 = bounds.height;
    let pixel_count: usize = width as usize * height as usize;

    let mut rgba: Vec<u8> = vec![0u8; pixel_count * 4];
    let mut depth_buf: Vec<f32> = vec![f32::NEG_INFINITY; pixel_count];

    let fill_size: i32 = bounds.fill_size;
    let half_fill: i32 = bounds.half_fill;
    let buf_off_x_fp: i32 = bounds.buf_off_x_fp;
    let buf_off_y_fp: i32 = bounds.buf_off_y_fp;

    // --- Phase 3: Back-to-front spatial iteration per limb ---

    // Pre-compute facing rotation matrix for fallback N·L shading.
    let facing_rot: Mat4 = Mat4::from_rotation_z(facing_rad);

    for ld in &limb_data {
        // Determine iteration direction per axis from the camera transform.
        // The Z component of the transformed axis vector tells us which
        // direction along that axis is "toward the camera" (higher depth).
        let depth_x: f32 = ld.combined.transform_vector3(Vec3::X).z;
        let depth_y: f32 = ld.combined.transform_vector3(Vec3::Y).z;
        let depth_z: f32 = ld.combined.transform_vector3(Vec3::Z).z;

        let iter_x: AxisIter = axis_order(ld.size_x, depth_x);
        let iter_y: AxisIter = axis_order(ld.size_y, depth_y);
        let iter_z: AxisIter = axis_order(ld.size_z, depth_z);

        let sy: usize = ld.size_y as usize;
        let sz: usize = ld.size_z as usize;

        for ix in iter_x.iter() {
            for iy in iter_y.iter() {
                for iz in iter_z.iter() {
                    let grid_idx: usize = ix as usize * sy * sz + iy as usize * sz + iz as usize;
                    let packed: PackedVoxel = ld.grid[grid_idx];
                    if packed == 0 {
                        continue;
                    }

                    let color_index: u8 = unpack_color(packed);
                    let normal_index: u8 = unpack_normal(packed);

                    // VPL Blinn-Phong lighting or fallback N·L shading.
                    let final_color_index: u8 = match vpl {
                        Some(vpl_file) => {
                            let page: u8 = ld.vpl_pages[normal_index as usize];
                            vpl_file.get_palette_index(page, color_index)
                        }
                        None => color_index,
                    };

                    let base_color: Color = palette.colors[final_color_index as usize];

                    let color: [u8; 4] = if vpl.is_some() {
                        [base_color.r, base_color.g, base_color.b, base_color.a]
                    } else {
                        let normal: Vec3 = vxl_normals::get_normal(ld.normals_mode, normal_index);
                        let rotated: Vec3 = facing_rot.transform_vector3(normal);
                        let brightness: f32 = vxl_normals::diffuse_shade(
                            rotated,
                            params.light_dir,
                            params.ambient,
                            params.diffuse,
                        );
                        shade_color(base_color, brightness)
                    };

                    // Project voxel center to screen space (fixed-point truncation).
                    let center: Vec3 = Vec3::new(ix as f32, iy as f32, iz as f32);
                    let world_pos: Vec3 = ld.combined.transform_point3(center);
                    let depth: f32 = world_pos.z;

                    // 16.16 fixed-point projection with truncation (>> 16).
                    // The `as i32` cast truncates toward zero, matching the
                    // original RA2/TS integer math pixel-snapping behavior.
                    let sx_fp: i32 = (world_pos.x * scale * FP_SCALE) as i32;
                    let sy_fp: i32 = (-world_pos.y * scale * FP_SCALE) as i32;
                    let px: i32 = (sx_fp + buf_off_x_fp) >> FP_SHIFT;
                    let py: i32 = (sy_fp + buf_off_y_fp) >> FP_SHIFT;

                    // Plot a filled rectangle centered on the projected point.
                    // Z-buffer check needed for inter-limb occlusion.
                    for dy in -half_fill..=(fill_size - 1 - half_fill) {
                        for dx in -half_fill..=(fill_size - 1 - half_fill) {
                            let fx: i32 = px + dx;
                            let fy: i32 = py + dy;
                            if fx < 0 || fy < 0 || fx >= width as i32 || fy >= height as i32 {
                                continue;
                            }
                            let buf_idx: usize = fy as usize * width as usize + fx as usize;
                            if depth < depth_buf[buf_idx] {
                                continue;
                            }
                            depth_buf[buf_idx] = depth;
                            let base: usize = buf_idx * 4;
                            rgba[base] = color[0];
                            rgba[base + 1] = color[1];
                            rgba[base + 2] = color[2];
                            rgba[base + 3] = color[3];
                        }
                    }
                }
            }
        }
    }

    VxlSprite {
        rgba,
        depth: depth_buf,
        width,
        height,
        offset_x: bounds.offset_x,
        offset_y: bounds.offset_y,
    }
}

/// Compute the screen-space pixel footprint for one voxel unit.
///
/// Projects the 3 unit axis vectors (1,0,0), (0,1,0), (0,0,1) through the
/// combined section+world transform and returns the maximum screen-space
/// distance. This determines how large a rectangle to draw for each voxel.
pub fn compute_voxel_footprint(combined: &Mat4, scale: f32) -> f32 {
    let origin: Vec3 = combined.transform_point3(Vec3::ZERO);
    let mut max_dist: f32 = 0.0;
    for axis in &[Vec3::X, Vec3::Y, Vec3::Z] {
        let projected: Vec3 = combined.transform_point3(*axis);
        let dx: f32 = (projected.x - origin.x) * scale;
        let dy: f32 = (projected.y - origin.y) * scale;
        let dist: f32 = (dx * dx + dy * dy).sqrt();
        if dist > max_dist {
            max_dist = dist;
        }
    }
    max_dist
}

/// Convert HVA 3×4 row-major transform to glam Mat4.
/// Translation (indices 3, 7, 11) scaled by limb_scale.
pub fn hva_to_mat4(raw: &[f32; 12], limb_scale: f32) -> Mat4 {
    Mat4::from_cols(
        Vec4::new(raw[0], raw[4], raw[8], 0.0),
        Vec4::new(raw[1], raw[5], raw[9], 0.0),
        Vec4::new(raw[2], raw[6], raw[10], 0.0),
        Vec4::new(
            raw[3] * limb_scale,
            raw[7] * limb_scale,
            raw[11] * limb_scale,
            1.0,
        ),
    )
}

/// Apply brightness to a palette color (fallback when VPL is unavailable).
fn shade_color(color: Color, brightness: f32) -> [u8; 4] {
    [
        (color.r as f32 * brightness).round().min(255.0) as u8,
        (color.g as f32 * brightness).round().min(255.0) as u8,
        (color.b as f32 * brightness).round().min(255.0) as u8,
        color.a,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::vxl_file::{VxlLimb, VxlVoxel};

    fn make_test_vxl() -> VxlFile {
        let identity: [f32; 12] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        VxlFile {
            limb_count: 1,
            body_size: 0,
            palette: vec![[0; 3]; 256],
            limbs: vec![VxlLimb {
                name: "body".to_string(),
                scale: 1.0,
                bounds: [-1.0, -1.0, -1.0, 1.0, 1.0, 1.0],
                transform: identity,
                size_x: 2,
                size_y: 2,
                size_z: 2,
                normals_mode: 4,
                voxels: vec![
                    VxlVoxel {
                        x: 1,
                        y: 1,
                        z: 1,
                        color_index: 10,
                        normal_index: 0,
                    },
                    VxlVoxel {
                        x: 0,
                        y: 0,
                        z: 0,
                        color_index: 20,
                        normal_index: 1,
                    },
                ],
            }],
        }
    }

    fn make_test_palette() -> Palette {
        let mut colors: [Color; 256] = [Color::rgb(128, 128, 128); 256];
        colors[0] = Color::transparent();
        colors[10] = Color::rgb(255, 0, 0);
        colors[20] = Color::rgb(0, 0, 255);
        Palette { colors }
    }

    #[test]
    fn test_render_produces_nonempty_sprite() {
        let vxl: VxlFile = make_test_vxl();
        let palette: Palette = make_test_palette();
        let params: VxlRenderParams = VxlRenderParams::default();
        let sprite: VxlSprite = render_vxl(&vxl, None, &palette, &params, None);

        assert!(sprite.width > 0);
        assert!(sprite.height > 0);
        assert_eq!(
            sprite.rgba.len(),
            (sprite.width * sprite.height * 4) as usize
        );

        let opaque_count: usize = sprite.rgba.chunks(4).filter(|p| p[3] > 0).count();
        assert!(
            opaque_count >= 2,
            "Expected at least 2 opaque pixels, got {}",
            opaque_count
        );
    }

    #[test]
    fn test_empty_model_returns_transparent() {
        let vxl: VxlFile = VxlFile {
            limb_count: 0,
            body_size: 0,
            palette: vec![],
            limbs: vec![],
        };
        let palette: Palette = make_test_palette();
        let params: VxlRenderParams = VxlRenderParams::default();
        let sprite: VxlSprite = render_vxl(&vxl, None, &palette, &params, None);
        assert_eq!(sprite.width, 1);
        assert_eq!(sprite.height, 1);
        assert_eq!(sprite.rgba[3], 0);
    }

    #[test]
    fn test_facing_changes_output() {
        let vxl: VxlFile = make_test_vxl();
        let palette: Palette = make_test_palette();

        let sprite_0: VxlSprite = render_vxl(
            &vxl,
            None,
            &palette,
            &VxlRenderParams {
                facing: 0,
                ..Default::default()
            },
            None,
        );
        let sprite_128: VxlSprite = render_vxl(
            &vxl,
            None,
            &palette,
            &VxlRenderParams {
                facing: 128,
                ..Default::default()
            },
            None,
        );

        let same_offset: bool = (sprite_0.offset_x - sprite_128.offset_x).abs() < 0.01
            && (sprite_0.offset_y - sprite_128.offset_y).abs() < 0.01;
        assert!(
            !same_offset || sprite_0.rgba != sprite_128.rgba,
            "Facing 0 and 128 should produce different output"
        );
    }

    #[test]
    fn test_point_plot_fills_pixels() {
        let vxl: VxlFile = make_test_vxl();
        let palette: Palette = make_test_palette();
        let params: VxlRenderParams = VxlRenderParams::default();
        let sprite: VxlSprite = render_vxl(&vxl, None, &palette, &params, None);

        let opaque: usize = sprite.rgba.chunks(4).filter(|p| p[3] > 0).count();
        assert!(
            opaque >= 2,
            "Point-plot should produce at least 2 opaque pixels, got {}",
            opaque
        );
    }

    #[test]
    fn test_voxel_grid_packing() {
        // Verify packed voxel round-trips correctly.
        let packed: PackedVoxel = pack_voxel(42, 137);
        assert_eq!(unpack_color(packed), 42);
        assert_eq!(unpack_normal(packed), 137);

        // Color index 0 = empty sentinel.
        let empty: PackedVoxel = pack_voxel(0, 99);
        assert_eq!(empty, 0x0063); // color 0 still packs but...
        // Our grid check uses `packed == 0` which requires both to be 0.
        // Color index 0 means transparent, so we skip it during grid build
        // (the original voxel list already excludes color_index 0 in practice).
        let truly_empty: PackedVoxel = 0;
        assert_eq!(unpack_color(truly_empty), 0);
        assert_eq!(unpack_normal(truly_empty), 0);
    }

    #[test]
    fn test_axis_order_positive_depth() {
        // Positive depth contribution → iterate low to high.
        let iter: AxisIter = axis_order(5, 1.0);
        let vals: Vec<i32> = iter.iter().collect();
        assert_eq!(vals, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn test_axis_order_negative_depth() {
        // Negative depth contribution → iterate high to low.
        let iter: AxisIter = axis_order(5, -1.0);
        let vals: Vec<i32> = iter.iter().collect();
        assert_eq!(vals, vec![4, 3, 2, 1, 0]);
    }
}
