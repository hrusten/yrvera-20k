//! UI overlay instance builders — status bars, software cursor.
//!
//! Extracted from app_render.rs. Part of the app layer — may depend on everything.

use crate::app::AppState;
use crate::app_commands::preferred_local_owner_name;
use crate::app_cursor::{
    current_cursor_feedback_kind, current_software_cursor_frame, cursor_id_for_feedback,
};
use crate::app_instances::in_view;
use crate::app_types::{CursorId, SoftwareCursorSequence};
use crate::map::entities::EntityCategory;
use crate::render::batch::{BatchTexture, SpriteInstance};
use crate::sim::components::Position;
/// Parse foundation dimensions from a string like "3x2" → (3, 2).
fn parse_foundation(foundation: &str) -> (u32, u32) {
    let mut parts = foundation.split('x');
    let w: u32 = parts
        .next()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(1)
        .max(1);
    let h: u32 = parts
        .next()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(1)
        .max(1);
    (w, h)
}

/// Authentic pip counts for unit health bars.
const UNIT_PIPS_VEHICLE: u32 = 17; // vehicles + aircraft
const UNIT_PIPS_INFANTRY: u32 = 8;

/// Horizontal pixel spacing between unit health pips (each pip drawn 2px apart).
const UNIT_PIP_STEP_X: f32 = 2.0;

/// Health bar offsets are baked into SelectionOverlay at load time
/// (game constant + canvas centering combined).

/// Fixed pip step along the NW isometric edge.
/// Each pip moves 4px left and 2px down, following the isometric 2:1 slope.
const PIP_STEP_X: f32 = -4.0;
const PIP_STEP_Y: f32 = 2.0;

/// Multiplier for the art.ini Height= value when computing vertical pip lift (z_screen).
/// Z = Height * HeightFactor (104) * AdjustForZ (≈0.14348) ≈ Height * 15.
/// Height=4 → z_screen=60, Height=5 → 75.
/// NOTE: Phobos `Height * 12` is for bracket EXTENT (vertical span), NOT z_screen.
const PIP_HEIGHT_FACTOR: f32 = 15.0;

/// Get INI-driven health condition thresholds, falling back to RA2 defaults.
fn condition_thresholds(state: &AppState) -> (f32, f32) {
    state
        .rules
        .as_ref()
        .map(|r| (r.general.condition_yellow, r.general.condition_red))
        .unwrap_or((0.5, 0.25))
}

/// Building health: discrete pips from pips.shp along the isometric NW foundation edge.
///
/// Pip positions are computed from Dimension2 (foundation size in leptons) projected
/// through CoordsToScreen along the NW edge.
///
/// Simplified formula (CTS offsets cancel with foundation geometry):
///   num_pips  = floor(H * 15 / 2)
///   pip_x[i]  = loc_x + (-(W+H)*15 + 3 + num_pips*4) - i*4
///   pip_y[i]  = loc_y + ((H-W)*7.5 + 4 - 2*num_pips) + i*2
///
/// Where loc = entity screen position. lepton_to_screen now returns cell center
/// (matching the original CoordsToClient), so loc = (screen_x, screen_y).
pub(crate) fn build_building_status_instances(
    state: &AppState,
    sw: f32,
    sh: f32,
) -> Vec<SpriteInstance> {
    let (Some(sim), Some(overlay)) = (&state.simulation, &state.selection_overlay) else {
        return Vec::new();
    };
    let local_owner = preferred_local_owner_name(state);
    let local_owner_id = local_owner.as_deref().and_then(|n| sim.interner.get(n));
    let ignore_visibility = state.sandbox_full_visibility;
    let pip_size: [f32; 2] = overlay.pip_frame_size();
    let pip_uv_size: [f32; 2] = overlay.pip_uv_size();
    let (_pip_adj_x, _pip_adj_y) = overlay.pip_canvas_adj();
    let has_pips: bool = overlay.pip_texture().is_some();
    let (cond_y, cond_r) = condition_thresholds(state);
    let mut instances = Vec::new();
    for e in sim.entities.values() {
        if e.category != EntityCategory::Structure {
            continue;
        }
        let health = &e.health;
        if !e.selected && health.current >= health.max {
            continue;
        }
        let type_str = sim.interner.resolve(e.type_ref);
        if !status_entity_visible_plain(
            local_owner_id,
            &sim.fog,
            &e.position,
            e.owner,
            ignore_visibility,
        ) {
            continue;
        }
        let (sx, sy) = (e.position.screen_x, e.position.screen_y);
        // Foundation= is merged from art.ini into ObjectType by merge_art_data().
        // Height= is an art.ini property, looked up via Image= redirect.
        let obj = state.rules.as_ref().and_then(|r| r.object(type_str));
        let foundation: (u32, u32) = obj
            .map(|o| parse_foundation(&o.foundation))
            .unwrap_or((2, 2));
        // gamemd reads Height from the ART SECTION (Image= redirect target, NOT the
        // type ID itself). If the Image section doesn't define Height, use default 2
        // (BuildingTypeClass constructor default at 0x45DD90).
        // Do NOT fall back to the type_ref section — that would read the wrong value
        // for buildings with Image= redirect (e.g., GAPOWR→YAPOWR).
        let art_key: &str = obj
            .map(|o| {
                let img = o.image.as_str();
                if img.is_empty() { o.id.as_str() } else { img }
            })
            .unwrap_or(type_str);
        let art_height: f32 = state
            .art_registry
            .as_ref()
            .and_then(|art| art.get(art_key))
            .map(|entry| entry.height as f32)
            .unwrap_or(2.0);
        let ratio: f32 = if health.max == 0 {
            0.0
        } else {
            (health.current as f32 / health.max as f32).clamp(0.0, 1.0)
        };
        let depth: f32 = 0.0006;

        // Pip count: floor(H * 7.5) — from original (screen1.y - screen2.y) / 2.
        let num_pips: u32 = ((foundation.1 * 15) / 2).max(1);

        // Building health bar pip positioning derivation.
        // DrawExtras calls GetCoords (foundation center), then passes that screen
        // position to DrawHealthBar as pLocation.
        // Full derivation: pip0.Y = foundCenter.Y + screen1.Y + 4 - numPips*2
        //   = (sy + 7.5*(fw+fh) - 15) + (7.5*(fh-fw) - H*15) + 4 - 15*fh
        //   = sy + 7.5*(fw+fh) - 15 + 7.5*fh - 7.5*fw - H*15 + 4 - 15*fh
        //   = sy - 11 - H*15   (all foundation terms cancel!)
        // Similarly for X: pip0.X = foundCenter.X + screen1.X + 3 + numPips*4
        //   = (sx + 15*(fw-fh)) + (-15*(fw+fh)) + 3 + 30*fh = sx + 3
        let n: f32 = num_pips as f32;
        let start_x: f32 = sx + 3.0;
        // -11 from derivation; -5 empirical adjustment for building sprite
        // anchor difference (our sprites use NW cell center, gamemd uses foundation center).
        let start_y: f32 = sy - 6.0 - art_height * PIP_HEIGHT_FACTOR;

        if has_pips && foundation.1 > 0 {
            let pip_w: f32 = pip_size[0];
            let pip_h: f32 = pip_size[1];
            // Bounding box for culling: pips span from start to start + (N-1)*step.
            let end_x: f32 = start_x + (n - 1.0) * PIP_STEP_X;
            let end_y: f32 = start_y + (n - 1.0) * PIP_STEP_Y;
            let min_x: f32 = start_x.min(end_x);
            let min_y: f32 = start_y.min(end_y);
            let total_w: f32 = (start_x - end_x).abs() + pip_w;
            let total_h: f32 = (start_y - end_y).abs() + pip_h;
            if !in_view(
                min_x,
                min_y,
                total_w,
                total_h,
                state.camera_x,
                state.camera_y,
                sw,
                sh,
                48.0,
            ) {
                continue;
            }
            // ftol(ratio * numPips), clamped to [1, numPips].
            let filled: u32 = ((num_pips as f32 * ratio) as u32).max(1).min(num_pips);
            let health_variant: u32 = health_pip_variant(ratio, cond_y, cond_r);
            for i in 0..num_pips {
                let px: f32 = start_x + i as f32 * PIP_STEP_X;
                let py: f32 = start_y + i as f32 * PIP_STEP_Y;
                let variant: u32 = if i < filled { health_variant } else { 0 };
                let uv_origin: [f32; 2] = overlay.pip_uv_origin(variant);
                instances.push(SpriteInstance {
                    position: [px, py],
                    size: [pip_w, pip_h],
                    uv_origin,
                    uv_size: pip_uv_size,
                    depth,
                    tint: [1.0, 1.0, 1.0],
                    alpha: 1.0,
                });
            }
        } else {
            // Fallback: procedural colored segments when pips.shp unavailable.
            let seg_w: f32 = 3.0;
            let seg_h: f32 = 4.0;
            let end_x: f32 = start_x + (n - 1.0) * PIP_STEP_X;
            let end_y: f32 = start_y + (n - 1.0) * PIP_STEP_Y;
            let min_x: f32 = start_x.min(end_x);
            let min_y: f32 = start_y.min(end_y);
            let total_w: f32 = (start_x - end_x).abs() + seg_w;
            let total_h: f32 = (start_y - end_y).abs() + seg_h;
            if !in_view(
                min_x,
                min_y,
                total_w,
                total_h,
                state.camera_x,
                state.camera_y,
                sw,
                sh,
                48.0,
            ) {
                continue;
            }
            let fill_color = health_fill_color(ratio, cond_y, cond_r);
            let filled: u32 = ((num_pips as f32 * ratio) as u32).max(1).min(num_pips);
            for i in 0..num_pips {
                let seg_x: f32 = start_x + i as f32 * PIP_STEP_X;
                let seg_y: f32 = start_y + i as f32 * PIP_STEP_Y;
                let tint = if i < filled {
                    fill_color
                } else {
                    [0.10, 0.10, 0.10]
                };
                instances.push(SpriteInstance {
                    position: [seg_x, seg_y],
                    size: [seg_w, seg_h],
                    uv_origin: [0.0, 0.0],
                    uv_size: [1.0, 1.0],
                    depth,
                    tint,
                    alpha: 1.0,
                });
            }
        }
    }
    instances
}

/// Occupant pips for garrisoned buildings (pips.shp frames 6-12).
///
/// Drawn for every visible building with `ShowOccupantPips=yes` and `MaxNumberOccupants > 0`.
/// One pip per slot: filled slots use the infantry's `OccupyPip` color, empty slots use
/// frame 6 (gray). Starts at (screen_x+6, screen_y-1), step (+4, +2) per pip along
/// the isometric NW edge.
pub(crate) fn build_occupant_pip_instances(
    state: &AppState,
    sw: f32,
    sh: f32,
) -> Vec<SpriteInstance> {
    let (Some(sim), Some(overlay)) = (&state.simulation, &state.selection_overlay) else {
        return Vec::new();
    };
    let has_tex = overlay.occupant_pip_texture().is_some();
    if !has_tex {
        return Vec::new();
    }
    let pip_size: [f32; 2] = overlay.occupant_pip_frame_size();
    let pip_uv_size: [f32; 2] = overlay.occupant_pip_uv_size();
    let (adj_x, adj_y) = overlay.occupant_pip_canvas_adj();
    let local_owner = preferred_local_owner_name(state);
    let local_owner_id = local_owner.as_deref().and_then(|n| sim.interner.get(n));
    let ignore_visibility = state.sandbox_full_visibility;
    let rules = state.rules.as_ref();
    let mut instances = Vec::new();

    for e in sim.entities.values() {
        if e.category != EntityCategory::Structure {
            continue;
        }
        let type_str = sim.interner.resolve(e.type_ref);
        let Some(obj) = rules.and_then(|r| r.object(type_str)) else {
            continue;
        };
        if !obj.show_occupant_pips || obj.max_number_occupants == 0 {
            continue;
        }
        if !status_entity_visible_plain(
            local_owner_id,
            &sim.fog,
            &e.position,
            e.owner,
            ignore_visibility,
        ) {
            continue;
        }
        let cargo = match e.passenger_role.cargo() {
            Some(c) => c,
            None => continue,
        };
        let (sx, sy) = (e.position.screen_x, e.position.screen_y);
        // Occupant pips start at (screen_x + 6, screen_y - 1).
        let start_x: f32 = sx + 6.0 + adj_x;
        let start_y: f32 = sy - 1.0 + adj_y;
        let count: u32 = obj.max_number_occupants;
        // Occupant pip step: +4 right, +2 down (isometric NW edge, same as health pips but positive X).
        const STEP_X: f32 = 4.0;
        const STEP_Y: f32 = 2.0;

        // Bounding box culling.
        let end_x: f32 = start_x + (count.saturating_sub(1)) as f32 * STEP_X;
        let end_y: f32 = start_y + (count.saturating_sub(1)) as f32 * STEP_Y;
        let min_x: f32 = start_x.min(end_x);
        let min_y: f32 = start_y.min(end_y);
        let total_w: f32 = (end_x - start_x).abs() + pip_size[0];
        let total_h: f32 = (end_y - start_y).abs() + pip_size[1];
        if !in_view(
            min_x,
            min_y,
            total_w,
            total_h,
            state.camera_x,
            state.camera_y,
            sw,
            sh,
            48.0,
        ) {
            continue;
        }

        for i in 0..count {
            // Determine pip frame: occupied slot → occupant's OccupyPip, empty → frame 6.
            let frame_index: u32 = if (i as usize) < cargo.passengers.len() {
                let pax_id = cargo.passengers[i as usize];
                sim.entities
                    .get(pax_id)
                    .and_then(|pax| {
                        rules.and_then(|r| r.object(sim.interner.resolve(pax.type_ref)))
                    })
                    .map(|pax_obj| pax_obj.occupy_pip)
                    .unwrap_or(7) // default PersonGreen
            } else {
                6 // empty slot
            };
            let px: f32 = start_x + i as f32 * STEP_X;
            let py: f32 = start_y + i as f32 * STEP_Y;
            let uv_origin: [f32; 2] = overlay.occupant_pip_uv_origin(frame_index);
            instances.push(SpriteInstance {
                position: [px, py],
                size: [pip_size[0], pip_size[1]],
                uv_origin,
                uv_size: pip_uv_size,
                depth: 0.0006,
                tint: [1.0, 1.0, 1.0],
                alpha: 1.0,
            });
        }
    }
    instances
}

/// Non-building health bar backgrounds: pipbrd.shp bracket sprites.
/// Frame 0 = vehicle/aircraft (36×4), frame 1 = infantry (18×4).
pub(crate) fn build_unit_status_bg_instances(
    state: &AppState,
    sw: f32,
    sh: f32,
) -> Vec<SpriteInstance> {
    let (Some(sim), Some(overlay)) = (&state.simulation, &state.selection_overlay) else {
        return Vec::new();
    };
    if overlay.pipbrd_texture().is_none() {
        return Vec::new();
    }
    let local_owner = preferred_local_owner_name(state);
    let local_owner_id = local_owner.as_deref().and_then(|n| sim.interner.get(n));
    let ignore_visibility = state.sandbox_full_visibility;
    let mut instances = Vec::new();
    for e in sim.entities.values() {
        if e.category == EntityCategory::Structure {
            continue;
        }
        if e.passenger_role.is_inside_transport() {
            continue;
        }
        let health = &e.health;
        if !e.selected && health.current >= health.max {
            continue;
        }
        if !status_entity_visible_plain(
            local_owner_id,
            &sim.fog,
            &e.position,
            e.owner,
            ignore_visibility,
        ) {
            continue;
        }
        // Sub-cell offsets are already baked into screen_x/screen_y by the sim tick.
        let (sx, raw_sy) = crate::app_instances::interpolated_screen_position_entity(e);
        // Aircraft altitude: lift health bar to match the unit sprite position.
        let altitude_y_offset: f32 = e
            .locomotor
            .as_ref()
            .map(|l| crate::util::fixed_math::sim_to_f32(l.altitude) * 0.06)
            .unwrap_or(0.0);
        let sy = raw_sy - altitude_y_offset;
        let is_infantry: bool = e.category == EntityCategory::Infantry;
        let (bar_size, uv_origin, uv_size) = if is_infantry {
            (
                overlay.pipbrd_infantry_size(),
                overlay.pipbrd_infantry_uv().0,
                overlay.pipbrd_infantry_uv().1,
            )
        } else {
            (
                overlay.pipbrd_vehicle_size(),
                overlay.pipbrd_vehicle_uv().0,
                overlay.pipbrd_vehicle_uv().1,
            )
        };
        let bracket_delta: f32 = state
            .rules
            .as_ref()
            .and_then(|r| r.object(sim.interner.resolve(e.type_ref)))
            .map(|obj| obj.pixel_selection_bracket_delta as f32)
            .unwrap_or(0.0);
        let (off_x, off_y) = overlay.pipbrd_offset(is_infantry);
        let (bar_x, bar_y) = (sx + off_x, sy + bracket_delta + off_y);
        if !in_view(
            bar_x,
            bar_y,
            bar_size[0],
            bar_size[1],
            state.camera_x,
            state.camera_y,
            sw,
            sh,
            48.0,
        ) {
            continue;
        }
        instances.push(SpriteInstance {
            position: [bar_x, bar_y],
            size: bar_size,
            uv_origin,
            uv_size,
            depth: 0.0006,
            tint: [1.0, 1.0, 1.0],
            alpha: 1.0,
        });
    }
    instances
}

/// Non-building health bar fills: colored rectangles drawn over pipbrd.shp backgrounds.
/// Uses white_texture with per-instance color tint. Falls back to procedural segments
/// if pipbrd.shp is unavailable.
pub(crate) fn build_unit_status_fill_instances(
    state: &AppState,
    sw: f32,
    sh: f32,
) -> Vec<SpriteInstance> {
    let (Some(sim), Some(overlay)) = (&state.simulation, &state.selection_overlay) else {
        return Vec::new();
    };
    let local_owner = preferred_local_owner_name(state);
    let local_owner_id = local_owner.as_deref().and_then(|n| sim.interner.get(n));
    let ignore_visibility = state.sandbox_full_visibility;
    let (cond_y, cond_r) = condition_thresholds(state);
    let mut instances = Vec::new();
    for e in sim.entities.values() {
        if e.category == EntityCategory::Structure {
            continue;
        }
        if e.passenger_role.is_inside_transport() {
            continue;
        }
        let health = &e.health;
        if !e.selected && health.current >= health.max {
            continue;
        }
        if !status_entity_visible_plain(
            local_owner_id,
            &sim.fog,
            &e.position,
            e.owner,
            ignore_visibility,
        ) {
            continue;
        }
        // Sub-cell offsets are already baked into screen_x/screen_y by the sim tick.
        let (sx, raw_sy) = crate::app_instances::interpolated_screen_position_entity(e);
        // Aircraft altitude: lift health bar to match the unit sprite position.
        let altitude_y_offset: f32 = e
            .locomotor
            .as_ref()
            .map(|l| crate::util::fixed_math::sim_to_f32(l.altitude) * 0.06)
            .unwrap_or(0.0);
        let sy = raw_sy - altitude_y_offset;
        let ratio: f32 = if health.max == 0 {
            0.0
        } else {
            (health.current as f32 / health.max as f32).clamp(0.0, 1.0)
        };

        let is_infantry: bool = e.category == EntityCategory::Infantry;
        let num_pips: u32 = if is_infantry {
            UNIT_PIPS_INFANTRY
        } else {
            UNIT_PIPS_VEHICLE
        };
        let bracket_delta: f32 = state
            .rules
            .as_ref()
            .and_then(|r| r.object(sim.interner.resolve(e.type_ref)))
            .map(|obj| obj.pixel_selection_bracket_delta as f32)
            .unwrap_or(0.0);
        let (pip_off_x, pip_off_y) = overlay.pip_offset(is_infantry);
        let (pip_start_x, pip_start_y) = (sx + pip_off_x, sy + bracket_delta + pip_off_y);
        // Filled pip count: floor(ratio * maxPips), clamped to [1, maxPips].
        let raw_filled: u32 = (ratio * num_pips as f32) as u32;
        let filled: u32 = raw_filled.max(1).min(num_pips);
        // Map health ratio to unit pip variant: 0=green, 1=yellow, 2=red.
        let variant: u32 = if ratio > cond_y {
            0
        } else if ratio > cond_r {
            1
        } else {
            2
        };

        if let Some(_unit_pip_tex) = overlay.unit_pip_texture() {
            // Authentic RA2 style: individual pip sprites from pips.shp frames 16-18.
            let pip_size: [f32; 2] = overlay.unit_pip_frame_size();
            let pip_uv_size: [f32; 2] = overlay.unit_pip_uv_size();
            let total_w: f32 = (num_pips - 1) as f32 * UNIT_PIP_STEP_X + pip_size[0];
            if !in_view(
                pip_start_x,
                pip_start_y,
                total_w,
                pip_size[1],
                state.camera_x,
                state.camera_y,
                sw,
                sh,
                48.0,
            ) {
                continue;
            }
            // Only filled pips are drawn (no empty pips — PIPBRD.SHP is the background).
            let uv_origin: [f32; 2] = overlay.unit_pip_uv_origin(variant);
            for i in 0..filled {
                let px: f32 = pip_start_x + i as f32 * UNIT_PIP_STEP_X;
                let py: f32 = pip_start_y; // Y is constant for all pips in a bar.
                instances.push(SpriteInstance {
                    position: [px, py],
                    size: pip_size,
                    uv_origin,
                    uv_size: pip_uv_size,
                    depth: 0.0005,
                    tint: [1.0, 1.0, 1.0],
                    alpha: 1.0,
                });
            }
        } else {
            // Fallback: procedural colored segments when pips.shp unavailable.
            let seg_w: f32 = 2.0;
            let seg_h: f32 = 3.0;
            let total_w: f32 = num_pips as f32 * seg_w;
            if !in_view(
                pip_start_x,
                pip_start_y,
                total_w,
                seg_h,
                state.camera_x,
                state.camera_y,
                sw,
                sh,
                48.0,
            ) {
                continue;
            }
            let fill_color = health_fill_color(ratio, cond_y, cond_r);
            for i in 0..num_pips {
                let seg_x: f32 = pip_start_x + i as f32 * seg_w;
                let tint = if i < filled {
                    fill_color
                } else {
                    [0.10, 0.10, 0.10]
                };
                instances.push(SpriteInstance {
                    position: [seg_x, pip_start_y],
                    size: [seg_w, seg_h],
                    uv_origin: [0.0, 0.0],
                    uv_size: [1.0, 1.0],
                    depth: 0.0005,
                    tint,
                    alpha: 1.0,
                });
            }
        }
    }
    instances
}

/// Horizontal pixel spacing between cargo pips (each pip drawn 4px apart).
const CARGO_PIP_STEP_X: f32 = 4.0;

/// Tiberium/cargo pips for harvesters (pips2.shp frames 0, 2, 5).
///
/// Drawn for selected vehicles with PipScale=Tiberium that have a miner component.
/// Each bale in the cargo is one pip: ore=green (variant 1), gem=colored (variant 2).
/// Empty slots shown as variant 0. Start at (sx - 15 + canvas_adj_x, sy + 10 + canvas_adj_y),
/// step (+4, 0) per pip. Draw order: gem pips first, then ore pips, then empty slots.
pub(crate) fn build_cargo_pip_instances(state: &AppState, sw: f32, sh: f32) -> Vec<SpriteInstance> {
    let (Some(sim), Some(overlay)) = (&state.simulation, &state.selection_overlay) else {
        return Vec::new();
    };
    let Some(_tib_tex) = overlay.tiberium_pip_texture() else {
        log::debug!("cargo pips: tiberium pip texture not loaded (pips2.shp missing?)");
        return Vec::new();
    };
    let local_owner = preferred_local_owner_name(state);
    let local_owner_id = local_owner.as_deref().and_then(|n| sim.interner.get(n));
    let ignore_visibility = state.sandbox_full_visibility;
    let mut instances = Vec::new();
    let (tib_adj_x, tib_adj_y) = overlay.tiberium_pip_canvas_adj();
    let pip_size: [f32; 2] = overlay.tiberium_pip_frame_size();
    let pip_uv_size: [f32; 2] = overlay.tiberium_pip_uv_size();

    for e in sim.entities.values() {
        if e.category == EntityCategory::Structure {
            continue;
        }
        if e.passenger_role.is_inside_transport() {
            continue;
        }
        if !e.selected {
            continue;
        }
        let obj = state
            .rules
            .as_ref()
            .and_then(|r| r.object(sim.interner.resolve(e.type_ref)));
        let is_tiberium_scale = obj
            .map(|o| o.pip_scale == crate::rules::object_type::PipScale::Tiberium)
            .unwrap_or(false);
        if !is_tiberium_scale {
            continue;
        }
        let Some(ref miner) = e.miner else {
            continue;
        };
        if !status_entity_visible_plain(
            local_owner_id,
            &sim.fog,
            &e.position,
            e.owner,
            ignore_visibility,
        ) {
            continue;
        }

        let (sx, sy) = crate::app_instances::interpolated_screen_position_entity(e);
        let bracket_delta: f32 = obj
            .map(|o| o.pixel_selection_bracket_delta as f32)
            .unwrap_or(0.0);
        // Pip scale offset for non-buildings: (pLoc.X - 15, pLoc.Y + 10).
        let start_x: f32 = sx - 15.0 + tib_adj_x;
        let start_y: f32 = sy + bracket_delta + 10.0 + tib_adj_y;
        // 5-pip display: cargo_pips() returns 0-5 based on fill ratio.
        const MAX_PIPS: u32 = 5;
        let filled: u32 = miner.cargo_pips() as u32;
        let empty_count: u32 = MAX_PIPS - filled;

        // Bounding box culling.
        let total_w: f32 = (MAX_PIPS - 1) as f32 * CARGO_PIP_STEP_X + pip_size[0];
        if !in_view(
            start_x,
            start_y,
            total_w,
            pip_size[1],
            state.camera_x,
            state.camera_y,
            sw,
            sh,
            48.0,
        ) {
            continue;
        }

        // Proportion filled pips between gem and ore based on cargo contents.
        let gem_bales: u32 = miner
            .cargo
            .iter()
            .filter(|b| b.resource_type == crate::sim::miner::ResourceType::Gem)
            .count() as u32;
        let total_bales: u32 = miner.cargo.len() as u32;
        let gem_pips: u32 = if total_bales > 0 {
            (gem_bales * filled + total_bales - 1) / total_bales // round up
        } else {
            0
        };
        let ore_pips: u32 = filled - gem_pips.min(filled);

        let mut pip_idx: u32 = 0;
        // Gem pips (variant 2).
        let uv_gem: [f32; 2] = overlay.tiberium_pip_uv_origin(2);
        for _ in 0..gem_pips {
            let px: f32 = start_x + pip_idx as f32 * CARGO_PIP_STEP_X;
            instances.push(SpriteInstance {
                position: [px, start_y],
                size: pip_size,
                uv_origin: uv_gem,
                uv_size: pip_uv_size,
                depth: 0.0004,
                tint: [1.0, 1.0, 1.0],
                alpha: 1.0,
            });
            pip_idx += 1;
        }
        // Ore pips (variant 1).
        let uv_ore: [f32; 2] = overlay.tiberium_pip_uv_origin(1);
        for _ in 0..ore_pips {
            let px: f32 = start_x + pip_idx as f32 * CARGO_PIP_STEP_X;
            instances.push(SpriteInstance {
                position: [px, start_y],
                size: pip_size,
                uv_origin: uv_ore,
                uv_size: pip_uv_size,
                depth: 0.0004,
                tint: [1.0, 1.0, 1.0],
                alpha: 1.0,
            });
            pip_idx += 1;
        }
        // Empty pips (variant 0).
        let uv_empty: [f32; 2] = overlay.tiberium_pip_uv_origin(0);
        for _ in 0..empty_count {
            let px: f32 = start_x + pip_idx as f32 * CARGO_PIP_STEP_X;
            instances.push(SpriteInstance {
                position: [px, start_y],
                size: pip_size,
                uv_origin: uv_empty,
                uv_size: pip_uv_size,
                depth: 0.0004,
                tint: [1.0, 1.0, 1.0],
                alpha: 1.0,
            });
            pip_idx += 1;
        }
    }
    if !instances.is_empty() {
        log::debug!(
            "cargo pips: {} instances for {} entities (pip_size={:?}, uv_size={:?})",
            instances.len(),
            sim.entities
                .values()
                .filter(|e| e.selected && e.miner.is_some())
                .count(),
            pip_size,
            pip_uv_size,
        );
    }
    instances
}

/// Map health ratio to pip atlas variant index (1=green, 2=yellow, 3=red).
fn health_pip_variant(ratio: f32, condition_yellow: f32, condition_red: f32) -> u32 {
    if ratio > condition_yellow {
        1 // Green.
    } else if ratio > condition_red {
        2 // Yellow.
    } else {
        3 // Red.
    }
}

/// Resolve the active cursor sequence for the current game state.
/// Maps game-state intent → CursorId → loaded sequence via HashMap lookup.
fn active_cursor_sequence(state: &AppState) -> Option<&SoftwareCursorSequence> {
    let cursor = state.software_cursor.as_ref()?;
    let id: CursorId = current_cursor_feedback_kind(state)
        .and_then(cursor_id_for_feedback)
        .unwrap_or(CursorId::Default);
    cursor.get(id)
}

pub(crate) fn build_software_cursor_instances(state: &AppState) -> Vec<SpriteInstance> {
    if !state.use_software_cursor() {
        return Vec::new();
    }
    let Some(sequence) = active_cursor_sequence(state) else {
        return Vec::new();
    };
    let Some(frame) = current_software_cursor_frame(sequence) else {
        return Vec::new();
    };
    // Cursor rendering: the hotspot pixel must sit exactly at the OS cursor position.
    // Scroll cursors use edge-aligned hotspots, so cursor_x/y represents the hotspot
    // location — subtract the hotspot offset to find the top-left of the sprite.
    // Note: cursor_x/y are in screen space; camera offset is NOT applied (cursor is UI).
    vec![SpriteInstance {
        position: [
            state.cursor_x + state.camera_x - sequence.hotspot[0],
            state.cursor_y + state.camera_y - sequence.hotspot[1],
        ],
        size: [frame.width, frame.height],
        uv_origin: [0.0, 0.0],
        uv_size: [1.0, 1.0],
        depth: 0.0001,
        tint: [1.0, 1.0, 1.0],
        alpha: 1.0,
    }]
}

pub(crate) fn current_software_cursor_texture(state: &AppState) -> Option<&BatchTexture> {
    let sequence = active_cursor_sequence(state)?;
    Some(&current_software_cursor_frame(sequence)?.texture)
}

fn status_entity_visible_plain(
    local_owner: Option<crate::sim::intern::InternedId>,
    fog: &crate::sim::vision::FogState,
    pos: &Position,
    entity_owner: crate::sim::intern::InternedId,
    ignore_visibility: bool,
) -> bool {
    if ignore_visibility {
        return true;
    }
    let Some(local_owner) = local_owner else {
        return true;
    };
    if local_owner == entity_owner {
        return true;
    }
    fog.is_cell_revealed(local_owner, pos.rx, pos.ry)
        && !fog.is_cell_gap_covered(local_owner, pos.rx, pos.ry)
}

pub(crate) fn health_fill_color(ratio: f32, condition_yellow: f32, condition_red: f32) -> [f32; 3] {
    if ratio > condition_yellow {
        // Bright lime green — high health (#00FF00).
        [0.0, 1.0, 0.0]
    } else if ratio > condition_red {
        // Yellow — medium health (#FFFF00).
        [1.0, 1.0, 0.0]
    } else {
        // Red — critical health (#FF0000).
        [1.0, 0.0, 0.0]
    }
}
