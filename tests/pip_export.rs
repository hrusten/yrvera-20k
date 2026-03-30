use std::path::{Path, PathBuf};

use image::RgbaImage;
use vera20k::assets::asset_manager::AssetManager;
use vera20k::assets::pal_file::Palette;
use vera20k::assets::shp_file::ShpFile;

fn ra2_dir() -> String {
    std::env::var("RA2_DIR")
        .unwrap_or_else(|_| panic!("Set RA2_DIR env var to your RA2/YR install directory"))
}
const ROOT_DIR: &str = env!("CARGO_MANIFEST_DIR");
const OUTPUT_FRAME_0: &str = "pipbrd_frame_00.png";
const OUTPUT_FRAME_1: &str = "pipbrd_frame_01.png";
const OUTPUT_SHEET: &str = "pipbrd_sheet.png";
const SHEET_PADDING: u32 = 4;
const SHEET_GAP: u32 = 4;
const CHECKER_SIZE: u32 = 2;
const CHECKER_LIGHT: [u8; 4] = [32, 32, 32, 255];
const CHECKER_DARK: [u8; 4] = [20, 20, 20, 255];

fn save_rgba_png(path: &Path, rgba: Vec<u8>, width: u32, height: u32) {
    let image = RgbaImage::from_raw(width, height, rgba).expect("rgba image");
    image.save(path).expect("save png");
}

fn make_checkerboard(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let color = if ((x / CHECKER_SIZE) + (y / CHECKER_SIZE)).is_multiple_of(2) {
                CHECKER_LIGHT
            } else {
                CHECKER_DARK
            };
            let idx = ((y * width + x) * 4) as usize;
            rgba[idx] = color[0];
            rgba[idx + 1] = color[1];
            rgba[idx + 2] = color[2];
            rgba[idx + 3] = color[3];
        }
    }
    rgba
}

fn blit_rgba(
    dst: &mut [u8],
    dst_width: u32,
    dst_height: u32,
    dst_x: u32,
    dst_y: u32,
    src: &[u8],
    src_width: u32,
    src_height: u32,
) {
    for y in 0..src_height {
        let target_y = dst_y + y;
        if target_y >= dst_height {
            continue;
        }
        for x in 0..src_width {
            let target_x = dst_x + x;
            if target_x >= dst_width {
                continue;
            }
            let src_idx = ((y * src_width + x) * 4) as usize;
            let dst_idx = ((target_y * dst_width + target_x) * 4) as usize;
            let alpha = src[src_idx + 3];
            if alpha == 0 {
                continue;
            }
            dst[dst_idx] = src[src_idx];
            dst[dst_idx + 1] = src[src_idx + 1];
            dst[dst_idx + 2] = src[src_idx + 2];
            dst[dst_idx + 3] = alpha;
        }
    }
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn export_pipbrd_pngs() {
    let ra2_dir = PathBuf::from(ra2_dir());
    if !ra2_dir.exists() {
        eprintln!("SKIP: RA2 dir not found at {}", ra2_dir.display());
        return;
    }

    let root_dir = PathBuf::from(ROOT_DIR);
    let asset_manager = AssetManager::new(&ra2_dir).expect("asset manager");
    let (shp_data, shp_source) = asset_manager
        .get_with_source("pipbrd.shp")
        .expect("pipbrd.shp should exist in RA2 assets");
    let (pal_data, pal_source) = asset_manager
        .get_with_source("unittem.pal")
        .expect("unittem.pal should exist in RA2 assets");
    let shp = ShpFile::from_bytes(&shp_data).expect("parse pipbrd.shp");
    let palette = Palette::from_bytes(&pal_data).expect("parse unittem.pal");

    eprintln!(
        "Found pipbrd.shp in {} ({} bytes)",
        shp_source,
        shp_data.len()
    );
    eprintln!(
        "Using unittem.pal from {} ({} bytes)",
        pal_source,
        pal_data.len()
    );
    eprintln!(
        "pipbrd.shp header: {}x{}, {} frames",
        shp.width,
        shp.height,
        shp.frames.len()
    );

    let mut frame_rgba = Vec::with_capacity(shp.frames.len());
    let mut total_width = SHEET_PADDING * 2;
    let mut max_height = 0u32;
    for (frame_idx, frame) in shp.frames.iter().enumerate() {
        let rgba = shp
            .frame_to_rgba(frame_idx, &palette)
            .expect("decode pipbrd frame");
        let width = frame.frame_width as u32;
        let height = frame.frame_height as u32;
        let output_name = format!("pipbrd_frame_{frame_idx:02}.png");
        let output_path = root_dir.join(&output_name);
        save_rgba_png(&output_path, rgba.clone(), width, height);
        eprintln!(
            "  frame {}: {}x{} -> {}",
            frame_idx,
            width,
            height,
            output_path.display()
        );
        frame_rgba.push((rgba, width, height));
        total_width += width;
        if frame_idx + 1 < shp.frames.len() {
            total_width += SHEET_GAP;
        }
        max_height = max_height.max(height);
    }

    let sheet_width = total_width;
    let sheet_height = max_height + SHEET_PADDING * 2;
    let mut sheet_rgba = make_checkerboard(sheet_width, sheet_height);
    let mut cursor_x = SHEET_PADDING;
    for (rgba, width, height) in &frame_rgba {
        let y = SHEET_PADDING + (max_height - *height) / 2;
        blit_rgba(
            &mut sheet_rgba,
            sheet_width,
            sheet_height,
            cursor_x,
            y,
            rgba,
            *width,
            *height,
        );
        cursor_x += *width + SHEET_GAP;
    }

    let sheet_path = root_dir.join(OUTPUT_SHEET);
    save_rgba_png(&sheet_path, sheet_rgba, sheet_width, sheet_height);
    eprintln!(
        "  sheet: {}x{} -> {}",
        sheet_width,
        sheet_height,
        sheet_path.display()
    );

    assert!(root_dir.join(OUTPUT_FRAME_0).exists());
    assert!(root_dir.join(OUTPUT_FRAME_1).exists());
    assert!(sheet_path.exists());
}
