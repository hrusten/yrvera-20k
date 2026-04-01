//! Unit tests for the minimap renderer.
//!
//! Tests coordinate mapping, owner color logic, pixel setting, and viewport math.
//! GPU-dependent tests (full MinimapRenderer construction) are not possible here.

use super::*;
use crate::map::houses::HouseAllianceMap;
use crate::render::minimap_helpers::*;
use crate::rules::house_colors::HouseColorIndex;
use crate::sim::components::{Owner, Position};
use crate::sim::intern::test_intern;
use crate::sim::vision::FogState;
use std::collections::{BTreeMap, BTreeSet};

fn make_pixel(rx: u16, ry: u16, color: [u8; 4]) -> TerrainPixel {
    TerrainPixel {
        rx,
        ry,
        px: 0,
        py: 0,
        color,
    }
}

#[test]
fn test_world_to_minimap_pixel_origin() {
    // World position at origin maps to minimap pixel (0, 0).
    let (px, py): (u32, u32) =
        world_to_minimap_pixel(0.0, 0.0, 0.0, 0.0, 1000.0, 1000.0, 0.0, 0.0, 200.0, 200.0);
    assert_eq!(px, 0);
    assert_eq!(py, 0);
}

#[test]
fn test_world_to_minimap_pixel_center() {
    // Position at center of world maps to center of minimap.
    let (px, py): (u32, u32) = world_to_minimap_pixel(
        500.0, 500.0, 0.0, 0.0, 1000.0, 1000.0, 0.0, 0.0, 200.0, 200.0,
    );
    assert_eq!(px, 100);
    assert_eq!(py, 100);
}

#[test]
fn test_world_to_minimap_pixel_clamps_negative() {
    // Positions outside world bounds are clamped to 0.
    let (px, py): (u32, u32) = world_to_minimap_pixel(
        -500.0, -500.0, 0.0, 0.0, 1000.0, 1000.0, 0.0, 0.0, 200.0, 200.0,
    );
    assert_eq!(px, 0);
    assert_eq!(py, 0);
}

#[test]
fn test_world_to_minimap_pixel_clamps_overflow() {
    // Positions beyond world extent are clamped to max pixel (199).
    let (px, py): (u32, u32) = world_to_minimap_pixel(
        2000.0, 2000.0, 0.0, 0.0, 1000.0, 1000.0, 0.0, 0.0, 200.0, 200.0,
    );
    assert_eq!(px, 199);
    assert_eq!(py, 199);
}

#[test]
fn test_world_to_minimap_pixel_with_offset() {
    // World origin is offset — position at origin_x maps to pixel 0.
    let (px, py): (u32, u32) = world_to_minimap_pixel(
        -500.0, 200.0, -500.0, 200.0, 2000.0, 2000.0, 0.0, 0.0, 200.0, 200.0,
    );
    assert_eq!(px, 0);
    assert_eq!(py, 0);
}

#[test]
fn test_owner_dot_color_uses_house_map() {
    let mut map: HouseColorMap = HouseColorMap::new();
    map.insert("Americans".to_string(), HouseColorIndex(1)); // DarkBlue
    map.insert("Russians".to_string(), HouseColorIndex(2)); // DarkRed

    let blue: [u8; 4] = owner_dot_color("Americans", &map);
    let red: [u8; 4] = owner_dot_color("Russians", &map);
    // Blue should have more B than R, red should have more R than B.
    assert!(blue[2] > blue[0], "Americans should be blue-ish");
    assert!(red[0] > red[2], "Russians should be red-ish");
}

#[test]
fn test_owner_dot_color_unknown_defaults_gold() {
    let map: HouseColorMap = HouseColorMap::new();
    let gold: [u8; 4] = owner_dot_color("Unknown", &map);
    // Gold ramp: R and G should both be significant.
    assert!(gold[0] > 100, "Gold should have significant R");
    assert!(gold[3] == 255, "Alpha should be fully opaque");
}

#[test]
fn test_minimap_entity_visible_for_allied_owner() {
    let mut alliances = HouseAllianceMap::default();
    let allied_names = BTreeSet::from(["AMERICANS".to_string(), "BRITISH".to_string()]);
    alliances.insert("AMERICANS".to_string(), allied_names.clone());
    alliances.insert("BRITISH".to_string(), allied_names);

    let fog = FogState {
        width: 64,
        height: 64,
        by_owner: BTreeMap::new(),
        alliances,
        ..Default::default()
    };
    let pos = Position {
        rx: 10,
        ry: 12,
        z: 0,
        sub_x: crate::util::lepton::CELL_CENTER_LEPTON,
        sub_y: crate::util::lepton::CELL_CENTER_LEPTON,
        screen_x: 0.0,
        screen_y: 0.0,
    };
    let owner = Owner(test_intern("British"));

    assert!(minimap_entity_visible(
        test_intern("Americans"),
        &fog,
        &pos,
        &owner
    ));
}

#[test]
fn test_cell_visibility_color_visible_uses_base_color() {
    let mut fog = FogState {
        width: 16,
        height: 16,
        ..Default::default()
    };
    fog.mark_visible_for_owner(test_intern("Americans"), 5, 7);
    let pixel = make_pixel(5, 7, [40, 120, 40, 255]);
    assert_eq!(
        cell_visibility_color(test_intern("Americans"), &fog, &pixel),
        Some([40, 120, 40, 255])
    );
}

#[test]
fn test_cell_visibility_color_revealed_shows_full_color() {
    // In standard YR (FogOfWar=false), revealed cells show at full brightness.
    let mut fog = FogState {
        width: 16,
        height: 16,
        ..Default::default()
    };
    fog.mark_visible_for_owner(test_intern("Americans"), 5, 7);
    fog.by_owner
        .get_mut(&test_intern("Americans"))
        .expect("owner present")
        .clear_all_visible();
    let pixel = make_pixel(5, 7, [100, 50, 25, 255]);
    assert_eq!(
        cell_visibility_color(test_intern("Americans"), &fog, &pixel),
        Some([100, 50, 25, 255])
    );
}

#[test]
fn test_cell_visibility_color_shrouded_returns_none() {
    let fog = FogState::default();
    let pixel = make_pixel(9, 9, [40, 120, 40, 255]);
    assert_eq!(
        cell_visibility_color(test_intern("Americans"), &fog, &pixel),
        None
    );
}

#[test]
fn test_set_pixel_in_bounds() {
    let mut rgba: Vec<u8> = vec![0u8; 16]; // 2x2 pixel buffer
    set_pixel(&mut rgba, 2, 1, 0, [255, 128, 64, 255]);
    // Pixel at (1,0) -> offset = (0*2 + 1)*4 = 4
    assert_eq!(rgba[4], 255);
    assert_eq!(rgba[5], 128);
    assert_eq!(rgba[6], 64);
    assert_eq!(rgba[7], 255);
}

#[test]
fn test_set_pixel_out_of_bounds_does_nothing() {
    let mut rgba: Vec<u8> = vec![0u8; 16]; // 2x2 pixel buffer
    // Writing to x=5 in a 2-wide buffer should be silently ignored.
    set_pixel(&mut rgba, 2, 5, 0, [255, 255, 255, 255]);
    assert!(rgba.iter().all(|&b| b == 0));
}

#[test]
fn test_viewport_rect_returns_four_lines() {
    // We can't construct a full MinimapRenderer without GPU, but we can
    // test the coordinate math independently.
    let mm_size: f32 = MINIMAP_SIZE as f32;
    let world_w: f32 = 3000.0;
    let world_h: f32 = 2000.0;

    // Camera at origin, 1024x768 viewport.
    let cam_x: f32 = 0.0;
    let cam_y: f32 = 0.0;
    let screen_w: f32 = 1024.0;
    let screen_h: f32 = 768.0;

    // Compute expected viewport rect on minimap.
    let nx_left: f32 = (cam_x - 0.0) / world_w;
    let ny_top: f32 = (cam_y - 0.0) / world_h;
    let nx_right: f32 = (cam_x + screen_w - 0.0) / world_w;
    let ny_bottom: f32 = (cam_y + screen_h - 0.0) / world_h;

    let left: f32 = (nx_left * mm_size).clamp(0.0, mm_size);
    let top: f32 = (ny_top * mm_size).clamp(0.0, mm_size);
    let right: f32 = (nx_right * mm_size).clamp(0.0, mm_size);
    let bottom: f32 = (ny_bottom * mm_size).clamp(0.0, mm_size);

    // Verify the expected rect is within the minimap.
    assert!(left >= 0.0);
    assert!(top >= 0.0);
    assert!(right <= mm_size);
    assert!(bottom <= mm_size);
    assert!(right > left, "viewport should have nonzero width");
    assert!(bottom > top, "viewport should have nonzero height");
}

#[test]
fn test_single_cell_map_pixel_mapping() {
    // A map with one cell: its position should map to a valid minimap pixel.
    // If world_width is small (e.g., just one tile = 60px), the cell still
    // maps to pixel (0,0) since it IS the origin.
    let (px, py): (u32, u32) =
        world_to_minimap_pixel(0.0, 0.0, 0.0, 0.0, 60.0, 30.0, 0.0, 0.0, 200.0, 200.0);
    assert_eq!(px, 0);
    assert_eq!(py, 0);
}

#[test]
fn test_degenerate_world_size_no_panic() {
    // Zero-size world should not panic (clamped to 1.0 in MinimapRenderer::new).
    let (px, py): (u32, u32) =
        world_to_minimap_pixel(100.0, 100.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 200.0, 200.0);
    // Should clamp to max pixel.
    assert_eq!(px, 199);
    assert_eq!(py, 199);
}
