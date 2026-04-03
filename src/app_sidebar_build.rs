//! Sidebar sprite instance builders — slots, chrome, cameos, text, and placeholders.
//!
//! Builds the per-frame SpriteInstance vectors for each sidebar layer:
//! background rectangles, chrome art, cameo icons, text labels, progress bars.
//!
//! Extracted from app_sidebar_render.rs to keep files under 400 lines.
//!
//! ## Dependency rules
//! - Part of the app layer — may depend on everything.

use crate::app::AppState;
use crate::app_sidebar_render::current_sidebar_chrome;
use crate::render::batch::SpriteInstance;
use crate::render::sidebar_chrome::SidebarChromeAtlas;
use crate::sidebar::power_bar_anim::PowerBarAnimState;
use crate::sidebar::{SidebarChromeLayoutSpec, SidebarLayout, SidebarTabButton, SidebarView};

// ---------------------------------------------------------------------------
// Main sidebar panel instances (backgrounds, progress, badges, buttons, meters)
// ---------------------------------------------------------------------------

pub(crate) fn build_sidebar_instances(
    _state: &AppState,
    _view: &SidebarView,
) -> Vec<SpriteInstance> {
    Vec::new()
}

// ---------------------------------------------------------------------------
// Chrome art instances
// ---------------------------------------------------------------------------

pub(crate) fn build_sidebar_chrome_instances(
    state: &AppState,
    view: &SidebarView,
) -> Vec<SpriteInstance> {
    let Some(atlas) = current_sidebar_chrome(state) else {
        return Vec::new();
    };
    build_sidebar_chrome_instances_for_layout(
        atlas,
        state.sidebar_layout_spec,
        &view.layout,
        view,
        &view.tabs,
        &state.power_bar_anim,
        [state.render_width() as f32, state.render_height() as f32],
        [state.camera_x, state.camera_y],
        state.ui_scale,
    )
}

pub fn build_sidebar_chrome_instances_for_layout(
    atlas: &SidebarChromeAtlas,
    spec: SidebarChromeLayoutSpec,
    layout: &SidebarLayout,
    _view: &SidebarView,
    tabs: &[SidebarTabButton],
    power_bar_anim: &PowerBarAnimState,
    _screen_size: [f32; 2],
    camera_offset: [f32; 2],
    ui_scale: f32,
) -> Vec<SpriteInstance> {
    let mut inst = Vec::new();
    let d = 0.00048;
    let s = ui_scale;
    let cx = layout.sidebar_x;
    if let Some(top_sidebar) = atlas.top_strip_sidebar {
        push_chrome(
            &mut inst,
            top_sidebar,
            cx + spec.top_strip_sidebar_x,
            spec.top_strip_sidebar_y,
            d + 0.00003,
            camera_offset,
            s,
        );
    }
    if let Some(top_thin) = atlas.top_strip_thin {
        push_chrome(
            &mut inst,
            top_thin,
            cx + spec.top_strip_thin_x,
            spec.top_strip_thin_y,
            d + 0.00002,
            camera_offset,
            s,
        );
    }
    if let Some(unknown_top_housing) = atlas.unknown_top_housing {
        let width = if spec.unknown_top_housing_width > 0.0 {
            spec.unknown_top_housing_width
        } else {
            unknown_top_housing.pixel_size[0] * s
        };
        let height = if spec.unknown_top_housing_height > 0.0 {
            spec.unknown_top_housing_height
        } else {
            unknown_top_housing.pixel_size[1] * s
        };
        push_chrome_sized(
            &mut inst,
            unknown_top_housing,
            cx + spec.unknown_top_housing_x,
            layout.side3_y + spec.side3_height - height + spec.unknown_top_housing_y,
            [width, height],
            d + 0.00001,
            camera_offset,
        );
    }

    push_chrome(
        &mut inst,
        atlas.radar,
        cx,
        layout.radar_y,
        d,
        camera_offset,
        s,
    );
    push_chrome(
        &mut inst,
        atlas.side1,
        cx,
        layout.side1_y,
        d,
        camera_offset,
        s,
    );
    if let Some(tabs) = atlas.tabs {
        push_chrome(&mut inst, tabs, cx, layout.tabs_y, d, camera_offset, s);
    }
    let td = d - 0.00001;
    for tab_btn in tabs {
        let idx = tab_btn.tab.tab_index();
        let entry = if tab_btn.active {
            atlas.tab_buttons_active.get(idx).copied()
        } else {
            atlas.tab_buttons.get(idx).copied()
        };
        if let Some(e) = entry {
            push_chrome(
                &mut inst,
                e,
                tab_btn.rect.x,
                tab_btn.rect.y,
                td,
                camera_offset,
                s,
            );
        }
    }
    let mut y = layout.cameo_grid_top;
    let side2_scaled_h = atlas.side2.pixel_size[1] * s;
    while y < layout.cameo_grid_bottom - 1.0 {
        push_chrome(&mut inst, atlas.side2, cx, y, d, camera_offset, s);
        y += side2_scaled_h;
    }
    push_chrome(
        &mut inst,
        atlas.side3,
        cx,
        layout.side3_y,
        d,
        camera_offset,
        s,
    );

    // --- Sell / Repair buttons (inside the side1 area) ---
    // TODO: these use wrong palette (sidebar.pal instead of OBSERVER.PAL) — disabled until fixed
    let _btn_depth = d - 0.00002;
    // if let Some(sell) = atlas.sell {
    //     push_chrome(
    //         &mut inst,
    //         sell,
    //         cx + spec.sell_x,
    //         layout.side1_y + spec.sell_y,
    //         _btn_depth,
    //         camera_offset,
    //         s,
    //     );
    // }
    // if let Some(repair) = atlas.repair {
    //     push_chrome(
    //         &mut inst,
    //         repair,
    //         cx + spec.repair_x,
    //         layout.side1_y + spec.repair_y,
    //         _btn_depth,
    //         camera_offset,
    //         s,
    //     );
    // }

    // --- Power bar meter (powerp.shp strips stacked from top) ---
    render_power_bar(
        &mut inst,
        atlas,
        spec,
        layout,
        power_bar_anim,
        camera_offset,
        d,
    );

    inst
}

/// Render the vertical power bar meter by stacking powerp.shp strip tiles.
///
/// Draws segments from top to bottom matching the original PowerClass::Draw_It:
///   Empty (top)  = unused bar space (frame 0)
///   Red          = deficit segments (frame 3)
///   Yellow       = balance indicator (frame 2)
///   Green        = surplus / consumed power (frame 1, with frame 4 blink)
///
/// Segment counts come from `PowerBarAnimState` which animates them
/// one-at-a-time toward their targets for a smooth sliding effect.
fn render_power_bar(
    inst: &mut Vec<SpriteInstance>,
    atlas: &SidebarChromeAtlas,
    spec: SidebarChromeLayoutSpec,
    layout: &SidebarLayout,
    anim: &PowerBarAnimState,
    camera_offset: [f32; 2],
    base_depth: f32,
) {
    let bar_x: f32 = layout.sidebar_x + spec.power_bar_x;
    let bar_top: f32 = layout.tabs_y + spec.power_bar_top_y;
    let bar_w: f32 = spec.power_bar_width;
    let tile_h: f32 = spec.power_bar_tile_height;

    if tile_h <= 0.0 || anim.max_segments() <= 0 {
        return;
    }

    let fill_depth: f32 = base_depth - 0.00002;

    // Draw order top-to-bottom: empty → blink → surplus(green) → output(yellow) → drain(red).
    let (n_empty, n_surplus, n_output, n_drain) = anim.segment_counts();

    let bg_entry = atlas.powerp_frames[0];
    let surplus_entry = atlas.powerp_frames[1]; // green
    let output_entry = atlas.powerp_frames[2]; // yellow
    let drain_entry = atlas.powerp_frames[3]; // red
    let blink_entry = atlas.powerp_frames[4];

    let flashing = anim.is_flashing();

    let mut y: f32 = bar_top;

    // 1. Empty segments (frame 0) — top of bar.
    if let Some(bg) = bg_entry {
        for _ in 0..n_empty {
            push_chrome_sized(
                inst,
                bg,
                bar_x,
                y,
                [bar_w, tile_h],
                fill_depth,
                camera_offset,
            );
            y += tile_h;
        }
    } else {
        y += n_empty as f32 * tile_h;
    }

    // 2. Blink frame at empty/filled boundary (frame 4, replaces first surplus segment).
    let mut surplus_drawn: i32 = 0;
    if flashing && n_surplus > 0 {
        if let Some(blink) = blink_entry {
            push_chrome_sized(
                inst,
                blink,
                bar_x,
                y,
                [bar_w, tile_h],
                fill_depth,
                camera_offset,
            );
        } else if let Some(s) = surplus_entry {
            push_chrome_sized(
                inst,
                s,
                bar_x,
                y,
                [bar_w, tile_h],
                fill_depth,
                camera_offset,
            );
        }
        y += tile_h;
        surplus_drawn = 1;
    }

    // 3. Surplus segments (frame 1, green) — top of filled area.
    if let Some(s) = surplus_entry {
        for _ in surplus_drawn..n_surplus {
            push_chrome_sized(
                inst,
                s,
                bar_x,
                y,
                [bar_w, tile_h],
                fill_depth,
                camera_offset,
            );
            y += tile_h;
        }
    } else {
        y += (n_surplus - surplus_drawn) as f32 * tile_h;
    }

    // 4. Output segments (frame 2, yellow) — middle.
    if let Some(o) = output_entry {
        for _ in 0..n_output {
            push_chrome_sized(
                inst,
                o,
                bar_x,
                y,
                [bar_w, tile_h],
                fill_depth,
                camera_offset,
            );
            y += tile_h;
        }
    } else {
        y += n_output as f32 * tile_h;
    }

    // 5. Drain segments (frame 3, red) — bottom of bar.
    if let Some(d) = drain_entry {
        for _ in 0..n_drain {
            push_chrome_sized(
                inst,
                d,
                bar_x,
                y,
                [bar_w, tile_h],
                fill_depth,
                camera_offset,
            );
            y += tile_h;
        }
    }
}

fn push_chrome(
    instances: &mut Vec<SpriteInstance>,
    entry: crate::render::sidebar_chrome::SidebarChromeEntry,
    x: f32,
    y: f32,
    depth: f32,
    camera_offset: [f32; 2],
    scale: f32,
) {
    instances.push(SpriteInstance {
        position: [x + camera_offset[0], y + camera_offset[1]],
        size: [entry.pixel_size[0] * scale, entry.pixel_size[1] * scale],
        uv_origin: entry.uv_origin,
        uv_size: entry.uv_size,
        depth,
        tint: [1.0, 1.0, 1.0],
        alpha: 1.0,
    });
}

fn push_chrome_sized(
    instances: &mut Vec<SpriteInstance>,
    entry: crate::render::sidebar_chrome::SidebarChromeEntry,
    x: f32,
    y: f32,
    size: [f32; 2],
    depth: f32,
    camera_offset: [f32; 2],
) {
    instances.push(SpriteInstance {
        position: [x + camera_offset[0], y + camera_offset[1]],
        size,
        uv_origin: entry.uv_origin,
        uv_size: entry.uv_size,
        depth,
        tint: [1.0, 1.0, 1.0],
        alpha: 1.0,
    });
}

// ---------------------------------------------------------------------------
// Cameo icon instances
// ---------------------------------------------------------------------------

/// Horizontal padding around the ready text (each side, in native pixels).
const READY_PAD_X: f32 = 2.0;
/// Vertical padding around the ready text (each side, in native pixels).
const READY_PAD_Y: f32 = 1.0;

/// Horizontal padding for queue count badge (native pixels, matches ComputeTextRect x_pad=2).
const QUEUE_COUNT_PAD_X: f32 = 2.0;
/// Vertical padding for queue count badge (native pixels, matches ComputeTextRect y_pad=1).
const QUEUE_COUNT_PAD_Y: f32 = 1.0;

/// Compute the text scale for cameo overlay text (READY, queue count).
/// Uses full ui_scale so text stays proportional to the scaled cameos.
fn ready_text_scale(ui_scale: f32) -> f32 {
    ui_scale
}

/// Returns (cameo_instances, gclock_instances, overlay_instances).
/// Cameo instances use the cameo atlas texture.
/// Gclock instances use the GCLOCK2 atlas texture (progress overlay).
/// Overlay instances are dark strip quads drawn with the darken_texture.
pub(crate) fn build_sidebar_cameo_instances(
    state: &AppState,
    view: &SidebarView,
    ready_text: &str,
) -> (
    Vec<SpriteInstance>,
    Vec<SpriteInstance>,
    Vec<SpriteInstance>,
) {
    let Some(atlas) = state.sidebar_cameo_atlas.as_ref() else {
        return (Vec::new(), Vec::new(), Vec::new());
    };
    let mut instances = Vec::new();
    let mut gclock_instances = Vec::new();
    let mut overlay_instances = Vec::new();
    let co = [state.camera_x, state.camera_y];
    let gclock_frames: &[crate::render::sidebar_chrome::SidebarChromeEntry] =
        crate::app_sidebar_render::current_sidebar_chrome(state)
            .map(|a| a.gclock_frames.as_slice())
            .unwrap_or(&[]);
    for item in &view.items {
        let Some(entry) = atlas.get(&item.type_id) else {
            continue;
        };
        let slot = item.cameo_rect();
        let [aw, ah] = entry.pixel_size;
        if aw <= 0.0 || ah <= 0.0 {
            continue;
        }
        let scale = (slot.w / aw).min(slot.h / ah);
        let dw = (aw * scale).round();
        let dh = (ah * scale).round();
        let is_building = !item.is_ready && item.progress > 0.0;

        if is_building {
            // Full cameo quad (normal tint — GCLOCK2 overlay handles darkening).
            let cam_x = (slot.x + (slot.w - dw) * 0.5 + co[0]).round();
            let cam_y = (slot.y + (slot.h - dh) * 0.5 + co[1]).round();
            instances.push(SpriteInstance {
                position: [cam_x, cam_y],
                size: [dw, dh],
                uv_origin: entry.uv_origin,
                uv_size: entry.uv_size,
                depth: 0.00044,
                tint: [1.0, 1.0, 1.0],
                alpha: 1.0,
            });

            // GCLOCK2 overlay — select frame from progress (0.0-1.0).
            // gamemd: frame = Production_Value + 1, Production_Value = 0..54.
            if !gclock_frames.is_empty() {
                let progress = item.progress.clamp(0.0, 1.0);
                // gamemd draws frame = Production_Value + 1 (range 1-55), skipping
                // frame 0. Map our 0.0-1.0 progress to frames 1..last.
                let last_frame = gclock_frames.len() - 1;
                let frame_index = if last_frame >= 2 {
                    ((progress * (last_frame - 1) as f32).round() as usize + 1).min(last_frame)
                } else {
                    last_frame.min(1)
                };
                let gclock_entry = &gclock_frames[frame_index];
                gclock_instances.push(SpriteInstance {
                    position: [cam_x, cam_y],
                    size: [dw, dh],
                    uv_origin: gclock_entry.uv_origin,
                    uv_size: gclock_entry.uv_size,
                    depth: 0.00043,
                    tint: [1.0, 1.0, 1.0],
                    alpha: 1.0,
                });
            }
        } else {
            // Non-building items: single full cameo quad. No blinking.
            instances.push(SpriteInstance {
                position: [
                    (slot.x + (slot.w - dw) * 0.5 + co[0]).round(),
                    (slot.y + (slot.h - dh) * 0.5 + co[1]).round(),
                ],
                size: [dw, dh],
                uv_origin: entry.uv_origin,
                uv_size: entry.uv_size,
                depth: 0.00044,
                tint: [1.0, 1.0, 1.0],
                alpha: 1.0,
            });
        }

        // Queue badge only for unit categories — buildings are one-at-a-time.
        let is_unit_category = !matches!(
            item.queue_category,
            crate::sim::production::ProductionCategory::Building
                | crate::sim::production::ProductionCategory::Defense
        );
        // Original badge condition: count > 1 OR (count > 0 AND active object is different type).
        let has_queue_badge = is_unit_category
            && (item.queued_count > 1 || (item.queued_count > 0 && !item.is_building_this_type));

        // Dark strip overlay behind "Ready" text (alpha 0xAF).
        // When queue badge is also present, the Ready strip shifts left.
        if item.is_ready && state.sidebar_text.darken_texture().is_some() {
            let s = state.ui_scale;
            let ts = ready_text_scale(s);
            let text_w = state.sidebar_text.text_width(ready_text) * ts;
            let strip_w = text_w + READY_PAD_X * 2.0 * ts;
            let strip_h = (state.sidebar_text.glyph_height() + READY_PAD_Y * 2.0) * ts;
            let strip_x = if has_queue_badge {
                slot.x + co[0]
            } else {
                slot.x + (slot.w - strip_w) * 0.5 + co[0]
            };
            overlay_instances.push(SpriteInstance {
                position: [strip_x, slot.y + co[1]],
                size: [strip_w, strip_h.min(slot.h)],
                uv_origin: [0.0, 0.0],
                uv_size: [1.0, 1.0],
                depth: 0.00043,
                tint: [1.0, 1.0, 1.0],
                alpha: 1.0,
            });
        }

        // Dark strip overlay behind queue count badge (top-right, same alpha as Ready strip).
        // Original: ComputeTextRect(cameo_x+60, cameo_y+1, 0x242, x_pad=2, y_pad=1)
        // The dark rect extends x_pad (2px) past the cameo right edge.
        if has_queue_badge && state.sidebar_text.darken_texture().is_some() {
            let ts = ready_text_scale(state.ui_scale);
            let count_str = format!("{}", item.queued_count);
            let text_w = state.sidebar_text.text_width(&count_str) * ts;
            let glyph_h = state.sidebar_text.glyph_height();
            let strip_w = text_w + QUEUE_COUNT_PAD_X * 2.0 * ts;
            let strip_h = (glyph_h + QUEUE_COUNT_PAD_Y * 2.0) * ts;
            // Right-align anchor at cameo right edge; strip extends x_pad past it.
            let strip_x = slot.x + slot.w - text_w - QUEUE_COUNT_PAD_X * ts;
            overlay_instances.push(SpriteInstance {
                position: [strip_x + co[0], slot.y + co[1]],
                size: [strip_w, strip_h.min(slot.h)],
                uv_origin: [0.0, 0.0],
                uv_size: [1.0, 1.0],
                depth: 0.00043,
                tint: [1.0, 1.0, 1.0],
                alpha: 1.0,
            });
        }
    }
    (instances, gclock_instances, overlay_instances)
}

// ---------------------------------------------------------------------------
// Text label instances
// ---------------------------------------------------------------------------

pub(crate) fn build_sidebar_text_instances(
    state: &AppState,
    view: &SidebarView,
    ready_text: &str,
    ready_tint: [f32; 3],
) -> Vec<SpriteInstance> {
    if state.sidebar_text.darken_texture().is_none() {
        // No FNT loaded — text will be rendered by egui fallback.
        return Vec::new();
    }
    let s = state.ui_scale;
    let ts = ready_text_scale(s);
    let co = [state.camera_x, state.camera_y];
    let mut instances = Vec::new();
    let glyph_h = state.sidebar_text.glyph_height();

    for item in &view.items {
        let slot = item.rect;

        // Queue badge only for unit categories — buildings are one-at-a-time.
        let is_unit_category = !matches!(
            item.queue_category,
            crate::sim::production::ProductionCategory::Building
                | crate::sim::production::ProductionCategory::Defense
        );
        let has_queue_badge = is_unit_category
            && (item.queued_count > 1 || (item.queued_count > 0 && !item.is_building_this_type));

        // "Ready" text — at the top of the cameo.
        // When a queue badge is also shown, the Ready text shifts left to avoid
        // overlap (original: x = cameo_x+2, flags 0x42 vs centered cameo_x+30, 0x142).
        if item.is_ready {
            let text_w = state.sidebar_text.text_width(ready_text) * ts;
            let strip_h = (glyph_h + READY_PAD_Y * 2.0) * ts;
            let text_x = if has_queue_badge {
                slot.x + READY_PAD_X * ts
            } else {
                slot.x + (slot.w - text_w) * 0.5
            };
            let text_y = slot.y + (strip_h - glyph_h * ts) * 0.5;
            instances.extend(
                state
                    .sidebar_text
                    .build_text(ready_text, text_x, text_y, ts, 0.00042, ready_tint, co),
            );
        }

        // Queue count badge — right-aligned at top-right of cameo.
        // Original: ComputeTextRect(cameo_x+60, cameo_y+1, 0x242, 2, 1)
        // 0x242 = right-align. Uses same side-dependent color as Ready text.
        if has_queue_badge {
            let count_str = format!("{}", item.queued_count);
            let text_w = state.sidebar_text.text_width(&count_str) * ts;
            // Right-align: text right edge at cameo right edge (anchor = cameo_x + 60).
            let text_x = slot.x + slot.w - text_w;
            let text_y = slot.y + QUEUE_COUNT_PAD_Y * ts;
            instances.extend(
                state
                    .sidebar_text
                    .build_text(&count_str, text_x, text_y, ts, 0.00042, ready_tint, co),
            );
        }
    }
    instances
}
