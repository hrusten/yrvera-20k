//! Parser for RA2 .vxl voxel model files.
//!
//! VXL files contain 3D voxel geometry for vehicles and aircraft. Each model
//! has one or more limbs (e.g., body + turret), each stored as a sparse
//! column-based voxel grid. Voxels carry a palette color index and a
//! normal index for lighting.
//!
//! ## File structure
//! - 802-byte header (32 file header + 2 remap bytes + 768 internal palette)
//! - Section headers (28 bytes × limb_count): limb names
//! - Body data (sparse column voxels for each limb)
//! - Section tailers (92 bytes × limb_count): bounds, size, scale, normals mode
//!
//! See `vxl_decode` for the column-based span decoding logic.
//!
//! ## Dependency rules
//! - Part of assets/ — no dependencies on game modules.

use crate::assets::error::AssetError;
use crate::assets::vxl_decode;
use crate::util::read_helpers::{read_f32_le, read_u32_le};

/// Expected magic string at the start of every VXL file.
const VXL_MAGIC: &[u8; 16] = b"Voxel Animation\0";

/// Total header size: 32 (file header) + 2 (remap) + 768 (palette) = 802 bytes.
const VXL_HEADER_SIZE: usize = 802;

/// Per-limb section header size in bytes (name + 3 u32 fields).
const SECTION_HEADER_SIZE: usize = 28;

/// Per-limb section tailer size in bytes (offsets + scale + matrix + bounds + size + mode).
const SECTION_TAILER_SIZE: usize = 92;

/// A single voxel within a limb's 3D grid.
#[derive(Debug, Clone, Copy)]
pub struct VxlVoxel {
    /// X position in the limb's grid (0..size_x).
    pub x: u8,
    /// Y position in the limb's grid (0..size_y).
    pub y: u8,
    /// Z position (height) in the limb's grid (0..size_z).
    pub z: u8,
    /// Palette color index. Index 0 = transparent (skip during rendering).
    pub color_index: u8,
    /// Normal table index for per-voxel lighting direction.
    pub normal_index: u8,
}

/// A single limb/section of a VXL model (e.g., body, turret, barrel).
#[derive(Debug)]
pub struct VxlLimb {
    /// Limb name from the section header (null-terminated, up to 16 chars).
    pub name: String,
    /// Scale factor applied to HVA transform translations.
    pub scale: f32,
    /// Axis-aligned bounding box: [min_x, min_y, min_z, max_x, max_y, max_z].
    pub bounds: [f32; 6],
    /// Default transform matrix from the tailer (3×4, row-major, 12 floats).
    pub transform: [f32; 12],
    /// Grid width in voxels.
    pub size_x: u8,
    /// Grid depth in voxels.
    pub size_y: u8,
    /// Grid height in voxels.
    pub size_z: u8,
    /// Normal table selector (2 = TiberianSun/36, 4 = RedAlert2/256).
    pub normals_mode: u8,
    /// All non-empty voxels in this limb.
    pub voxels: Vec<VxlVoxel>,
}

/// A parsed VXL voxel model containing one or more limbs.
#[derive(Debug)]
pub struct VxlFile {
    /// Number of limbs in this model.
    pub limb_count: u32,
    /// Body data size from the file header (used for offset calculations).
    pub body_size: u32,
    /// Internal palette (256 RGB colors). Usually unused — game applies theater palette.
    pub palette: Vec<[u8; 3]>,
    /// All limbs in this model.
    pub limbs: Vec<VxlLimb>,
}

impl VxlFile {
    /// Parse a VXL file from raw bytes.
    pub fn from_bytes(data: &[u8]) -> Result<Self, AssetError> {
        if data.len() < VXL_HEADER_SIZE {
            return Err(AssetError::InvalidVxlFile {
                reason: format!(
                    "File too small for header: {} bytes (need {})",
                    data.len(),
                    VXL_HEADER_SIZE
                ),
            });
        }

        // Validate magic string.
        if &data[0..16] != VXL_MAGIC.as_slice() {
            return Err(AssetError::InvalidVxlFile {
                reason: "Missing 'Voxel Animation' magic string".to_string(),
            });
        }

        let limb_count: u32 = read_u32_le(data, 20);
        let tailer_count: u32 = read_u32_le(data, 24);
        let body_size: u32 = read_u32_le(data, 28);

        if limb_count == 0 {
            return Err(AssetError::InvalidVxlFile {
                reason: "Limb count is zero".to_string(),
            });
        }
        if tailer_count != limb_count {
            return Err(AssetError::InvalidVxlFile {
                reason: format!(
                    "Tailer count ({}) != limb count ({})",
                    tailer_count, limb_count
                ),
            });
        }

        // Read internal palette (256 RGB triplets at offset 34).
        let palette: Vec<[u8; 3]> = (0..256)
            .map(|i| {
                let off: usize = 34 + i * 3;
                [data[off], data[off + 1], data[off + 2]]
            })
            .collect();

        // Validate file has enough data for all sections.
        let headers_end: usize = VXL_HEADER_SIZE + SECTION_HEADER_SIZE * limb_count as usize;
        let tailers_start: usize = headers_end + body_size as usize;
        let tailers_end: usize = tailers_start + SECTION_TAILER_SIZE * limb_count as usize;

        if data.len() < tailers_end {
            return Err(AssetError::InvalidVxlFile {
                reason: format!(
                    "File too small: {} bytes (need {} for {} limbs)",
                    data.len(),
                    tailers_end,
                    limb_count
                ),
            });
        }

        // Body data starts right after section headers.
        let body_start: usize = headers_end;

        // Parse each limb: header + tailer + voxel data.
        let mut limbs: Vec<VxlLimb> = Vec::with_capacity(limb_count as usize);
        for i in 0..limb_count as usize {
            let limb: VxlLimb = parse_limb(data, i, body_start, tailers_start)?;
            limbs.push(limb);
        }

        Ok(VxlFile {
            limb_count,
            body_size,
            palette,
            limbs,
        })
    }
}

/// Parse a single limb from its header + tailer + voxel data.
fn parse_limb(
    data: &[u8],
    index: usize,
    body_start: usize,
    tailers_start: usize,
) -> Result<VxlLimb, AssetError> {
    // Section header: name (16 bytes) + limb_number(4) + unk1(4) + unk2(4).
    let hdr_off: usize = VXL_HEADER_SIZE + index * SECTION_HEADER_SIZE;
    let name: String = vxl_decode::read_null_string(&data[hdr_off..hdr_off + 16]);

    // Section tailer: 92 bytes of metadata.
    let tail_off: usize = tailers_start + index * SECTION_TAILER_SIZE;
    let span_start_off: u32 = read_u32_le(data, tail_off);
    let span_end_off: u32 = read_u32_le(data, tail_off + 4);
    let data_span_off: u32 = read_u32_le(data, tail_off + 8);
    let scale: f32 = read_f32_le(data, tail_off + 12);

    let mut transform: [f32; 12] = [0.0; 12];
    for (k, slot) in transform.iter_mut().enumerate() {
        *slot = read_f32_le(data, tail_off + 16 + k * 4);
    }

    let mut bounds: [f32; 6] = [0.0; 6];
    for (k, slot) in bounds.iter_mut().enumerate() {
        *slot = read_f32_le(data, tail_off + 64 + k * 4);
    }

    let size_x: u8 = data[tail_off + 88];
    let size_y: u8 = data[tail_off + 89];
    let size_z: u8 = data[tail_off + 90];
    let normals_mode: u8 = data[tail_off + 91];

    // Decode voxels from body data using span offsets.
    let voxels: Vec<VxlVoxel> = vxl_decode::decode_limb_voxels(
        data,
        body_start,
        span_start_off,
        span_end_off,
        data_span_off,
        size_x,
        size_y,
        size_z,
    )?;

    Ok(VxlLimb {
        name,
        scale,
        bounds,
        transform,
        size_x,
        size_y,
        size_z,
        normals_mode,
        voxels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid VXL: 1 limb, 2×2×2 grid, a few voxels.
    fn make_test_vxl() -> Vec<u8> {
        let mut data: Vec<u8> = Vec::new();

        // File header (32 bytes).
        data.extend_from_slice(b"Voxel Animation\0"); // 16: magic
        data.extend_from_slice(&1u32.to_le_bytes()); // 16: palette_count
        data.extend_from_slice(&1u32.to_le_bytes()); // 20: limb_count
        data.extend_from_slice(&1u32.to_le_bytes()); // 24: tailer_count
        let body_size_offset: usize = data.len();
        data.extend_from_slice(&0u32.to_le_bytes()); // 28: body_size (patch later)
        data.push(0);
        data.push(0); // remap_start, remap_end
        data.extend_from_slice(&[128u8; 768]); // internal palette
        assert_eq!(data.len(), VXL_HEADER_SIZE);

        // Section header (28 bytes).
        data.extend_from_slice(b"body\0\0\0\0\0\0\0\0\0\0\0\0");
        data.extend_from_slice(&[0u8; 12]); // limb_number + unk1 + unk2

        let body_start: usize = data.len();

        // Column start offsets (4 columns × i32). Relative to data_base.
        let span_start_pos: usize = data.len() - body_start;
        data.extend_from_slice(&0i32.to_le_bytes()); // col 0: offset 0
        data.extend_from_slice(&(-1i32).to_le_bytes()); // col 1: empty
        data.extend_from_slice(&(-1i32).to_le_bytes()); // col 2: empty
        data.extend_from_slice(&7i32.to_le_bytes()); // col 3: after col 0

        // Column end offsets (4 columns × i32). Not used by parser.
        let span_end_pos: usize = data.len() - body_start;
        data.extend_from_slice(&[0u8; 16]);

        // Voxel span data.
        let data_span_pos: usize = data.len() - body_start;

        // Column 0: 2 voxels at z=0,1.
        data.push(0);
        data.push(2); // z_skip=0, count=2
        data.push(10);
        data.push(20); // voxel 0: color=10, normal=20
        data.push(11);
        data.push(21); // voxel 1: color=11, normal=21
        data.push(2); // dup_count

        // Column 3: 1 voxel at z=1.
        data.push(1);
        data.push(1); // z_skip=1, count=1
        data.push(50);
        data.push(60); // voxel: color=50, normal=60
        data.push(1); // dup_count

        let body_size: u32 = (data.len() - body_start) as u32;
        let bs: [u8; 4] = body_size.to_le_bytes();
        data[body_size_offset..body_size_offset + 4].copy_from_slice(&bs);

        // Section tailer (92 bytes).
        data.extend_from_slice(&(span_start_pos as u32).to_le_bytes());
        data.extend_from_slice(&(span_end_pos as u32).to_le_bytes());
        data.extend_from_slice(&(data_span_pos as u32).to_le_bytes());
        data.extend_from_slice(&1.0f32.to_le_bytes()); // scale
        // Identity transform (12 floats).
        for k in 0..12 {
            let v: f32 = if k == 0 || k == 4 || k == 8 { 1.0 } else { 0.0 };
            data.extend_from_slice(&v.to_le_bytes());
        }
        // Bounds [0,0,0, 2,2,2].
        for &v in &[0.0f32, 0.0, 0.0, 2.0, 2.0, 2.0] {
            data.extend_from_slice(&v.to_le_bytes());
        }
        data.push(2);
        data.push(2);
        data.push(2);
        data.push(4); // size + normals_mode
        data
    }

    #[test]
    fn test_parse_vxl_basic() {
        let vxl: VxlFile = VxlFile::from_bytes(&make_test_vxl()).expect("Should parse");
        assert_eq!(vxl.limb_count, 1);
        assert_eq!(vxl.limbs.len(), 1);
        let limb: &VxlLimb = &vxl.limbs[0];
        assert_eq!(limb.name, "body");
        assert_eq!(limb.size_x, 2);
        assert_eq!(limb.size_y, 2);
        assert_eq!(limb.size_z, 2);
        assert_eq!(limb.normals_mode, 4);
        assert!((limb.scale - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_voxel_span_decode() {
        let vxl: VxlFile = VxlFile::from_bytes(&make_test_vxl()).expect("Should parse");
        let limb: &VxlLimb = &vxl.limbs[0];
        assert_eq!(limb.voxels.len(), 3);

        assert_eq!(limb.voxels[0].x, 0);
        assert_eq!(limb.voxels[0].y, 0);
        assert_eq!(limb.voxels[0].z, 0);
        assert_eq!(limb.voxels[0].color_index, 10);
        assert_eq!(limb.voxels[0].normal_index, 20);
        assert_eq!(limb.voxels[1].z, 1);
        assert_eq!(limb.voxels[1].color_index, 11);
        assert_eq!(limb.voxels[2].x, 1);
        assert_eq!(limb.voxels[2].y, 1);
        assert_eq!(limb.voxels[2].z, 1);
        assert_eq!(limb.voxels[2].color_index, 50);
    }

    #[test]
    fn test_reject_bad_magic() {
        let mut data: Vec<u8> = make_test_vxl();
        data[0] = b'X';
        assert!(VxlFile::from_bytes(&data).is_err());
    }

    #[test]
    fn test_reject_too_small() {
        assert!(VxlFile::from_bytes(&vec![0u8; 100]).is_err());
    }

    #[test]
    fn test_limb_bounds_and_transform() {
        let vxl: VxlFile = VxlFile::from_bytes(&make_test_vxl()).expect("Should parse");
        let limb: &VxlLimb = &vxl.limbs[0];
        assert!((limb.bounds[0] - 0.0).abs() < f32::EPSILON);
        assert!((limb.bounds[3] - 2.0).abs() < f32::EPSILON);
        assert!((limb.transform[0] - 1.0).abs() < f32::EPSILON);
        assert!((limb.transform[4] - 1.0).abs() < f32::EPSILON);
        assert!((limb.transform[8] - 1.0).abs() < f32::EPSILON);
    }
}
