//! Isometric 3D selection bracket lines for buildings.
//!
//! When a building is selected, draws white bracket stub lines at the 3 visible
//! corners of its isometric bounding box (at roof level). Each corner has 3 short
//! lines radiating outward along the 3 isometric axes (X, Y, Z).
//!
//!
//! ## Dependency rules
//! - Part of the app layer — may depend on everything.

use crate::app::AppState;
use crate::app_commands::preferred_local_owner_name;
use crate::app_instances::in_view;
use crate::map::entities::EntityCategory;
use crate::render::batch::SpriteInstance;
use crate::sim::vision::FogState;

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

/// Height= multiplier: 1 art.ini Height unit = 15 screen pixels (HeightFactor * AdjustForZ).
const HEIGHT_PX: f32 = 15.0;

/// Bracket stub depth — drawn flat in the no-depth overlay pass.
const BRACKET_DEPTH: f32 = 0.0006;

/// Bracket line color — solid white, fully opaque.
const BRACKET_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// Check if an entity is visible to the local player for overlay purposes.
fn is_visible(
    local_owner: Option<crate::sim::intern::InternedId>,
    fog: &FogState,
    pos: &crate::sim::components::Position,
    entity_owner: crate::sim::intern::InternedId,
    ignore_visibility: bool,
) -> bool {
    if ignore_visibility {
        return true;
    }
    let Some(owner) = local_owner else {
        return true;
    };
    if owner == entity_owner {
        return true;
    }
    fog.is_cell_revealed(owner, pos.rx, pos.ry) && !fog.is_cell_gap_covered(owner, pos.rx, pos.ry)
}

/// Compute the 8 corners of a building's isometric bounding box in screen space.
///
/// Returns `(ground_corners, roof_corners)` where each is `[FL, FR, BL, BR]`.
/// Coordinates are absolute screen pixels (entity screen pos + foundation offset).
fn compute_box_corners(
    sx: f32,
    sy: f32,
    fw: f32,
    fh: f32,
    z_screen: f32,
) -> ([ScreenPt; 4], [ScreenPt; 4]) {
    // Foundation center offset from entity screen position (NW corner cell center).
    // Raw lepton offset: (fw-1)*128, (fh-1)*128.
    // Projected: cx = sx + (fw-fh)*15, cy = sy + 7.5*(fw+fh) - 15.
    let cx = sx + (fw - fh) * 15.0;
    let cy = sy + (fw + fh) * 7.5 - 15.0;

    // 4 ground corners relative to foundation center.
    // From gamemd projection: screen_dx = 30*(dx-dy)/256, screen_dy = 15*(dx+dy)/256
    // where dx = ±hw, dy = ±hh in leptons (hw = fw*128, hh = fh*128).
    // Simplifies to: screen offsets use (fw, fh) cells directly.
    let ground = [
        ScreenPt {
            x: cx - (fw + fh) * 15.0,
            y: cy + (fh - fw) * 7.5,
        }, // FL
        ScreenPt {
            x: cx + (fw - fh) * 15.0,
            y: cy + (fw + fh) * 7.5,
        }, // FR
        ScreenPt {
            x: cx + (fh - fw) * 15.0,
            y: cy - (fw + fh) * 7.5,
        }, // BL
        ScreenPt {
            x: cx + (fw + fh) * 15.0,
            y: cy - (fh - fw) * 7.5,
        }, // BR
    ];
    // Roof corners = ground corners shifted up by z_screen.
    let roof = [
        ScreenPt {
            x: ground[0].x,
            y: ground[0].y - z_screen,
        }, // FL roof
        ScreenPt {
            x: ground[1].x,
            y: ground[1].y - z_screen,
        }, // FR roof
        ScreenPt {
            x: ground[2].x,
            y: ground[2].y - z_screen,
        }, // BL roof
        ScreenPt {
            x: ground[3].x,
            y: ground[3].y - z_screen,
        }, // BR roof
    ];
    (ground, roof)
}

#[derive(Clone, Copy)]
struct ScreenPt {
    x: f32,
    y: f32,
}

/// Compute the quarter-point 25% from `a` toward `b`: (3a + b) / 4.
fn quarter_point(a: ScreenPt, b: ScreenPt) -> ScreenPt {
    ScreenPt {
        x: (a.x * 3.0 + b.x) * 0.25,
        y: (a.y * 3.0 + b.y) * 0.25,
    }
}

/// Emit 1px-wide line segments as pixel-stepping SpriteInstance quads.
///
/// Steps along the line from `a` to `b` using Bresenham-style integer stepping.
/// Each step emits a 2×1 (for iso diagonals) or 1×1 (for verticals) pixel quad.
fn emit_line(instances: &mut Vec<SpriteInstance>, a: ScreenPt, b: ScreenPt) {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let steps = dx.abs().max(dy.abs()).ceil() as i32;
    if steps <= 0 {
        return;
    }
    let step_x = dx / steps as f32;
    let step_y = dy / steps as f32;

    for i in 0..steps {
        let px = (a.x + step_x * i as f32).round();
        let py = (a.y + step_y * i as f32).round();
        instances.push(SpriteInstance {
            position: [px, py],
            size: [1.0, 1.0],
            uv_origin: [0.0, 0.0],
            uv_size: [1.0, 1.0],
            tint: [BRACKET_COLOR[0], BRACKET_COLOR[1], BRACKET_COLOR[2]],
            alpha: BRACKET_COLOR[3],
            depth: BRACKET_DEPTH,
        });
    }
}

/// Emit a bracket stub: a 25% line from corner `a` toward `b`.
fn emit_stub(instances: &mut Vec<SpriteInstance>, a: ScreenPt, b: ScreenPt) {
    let qp = quarter_point(a, b);
    emit_line(instances, a, qp);
}

/// Build bracket instances for all selected buildings.
pub(crate) fn build_selection_bracket_instances(
    state: &AppState,
    sw: f32,
    sh: f32,
) -> Vec<SpriteInstance> {
    let Some(sim) = &state.simulation else {
        return Vec::new();
    };
    let local_owner = preferred_local_owner_name(state);
    let local_owner_id = local_owner.as_deref().and_then(|n| sim.interner.get(n));
    let ignore_visibility = state.sandbox_full_visibility;
    let cam_x = state.camera_x;
    let cam_y = state.camera_y;
    let mut instances = Vec::new();

    for e in sim.entities.values() {
        if e.category != EntityCategory::Structure || !e.selected {
            continue;
        }
        let type_str = sim.interner.resolve(e.type_ref);
        if !is_visible(
            local_owner_id,
            &sim.fog,
            &e.position,
            e.owner,
            ignore_visibility,
        ) {
            continue;
        }

        let (sx, sy) = (e.position.screen_x, e.position.screen_y);

        // Look up foundation and Height from rules/art.
        let obj = state.rules.as_ref().and_then(|r| r.object(type_str));
        let (fw_u, fh_u) = obj
            .map(|o| parse_foundation(&o.foundation))
            .unwrap_or((2, 2));
        let fw = fw_u as f32;
        let fh = fh_u as f32;

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
        let z_screen = art_height * HEIGHT_PX;

        // Compute 8 corners.
        let (g, r) = compute_box_corners(sx, sy, fw, fh, z_screen);
        // g = [FL, FR, BL, BR] ground, r = [FL, FR, BL, BR] roof

        // Viewport cull: bounding box of all roof corners (ground is below roof).
        let min_x = r[0].x.min(r[1].x).min(r[2].x).min(r[3].x);
        let max_x = r[0].x.max(r[1].x).max(r[2].x).max(r[3].x);
        let min_y = r[0].y.min(r[1].y).min(r[2].y).min(r[3].y);
        let max_y = g[0].y.max(g[1].y).max(g[2].y).max(g[3].y); // ground is lower
        if !in_view(
            min_x,
            min_y,
            max_x - min_x,
            max_y - min_y,
            cam_x,
            cam_y,
            sw,
            sh,
            60.0,
        ) {
            continue;
        }
        // --- 12 edges of the isometric bounding box ---
        // Indices: FL=0, FR=1, BL=2, BR=3

        // DrawBehind edges (5): stubs at both ends, behind sprite (hidden by building art).
        // These are drawn anyway — the building sprite naturally occludes them.
        emit_stub(&mut instances, g[2], r[2]); // Edge 1: BL ground→BL roof (BL vertical)
        emit_stub(&mut instances, r[2], g[2]);
        emit_stub(&mut instances, g[3], g[2]); // Edge 2: BR ground→BL ground (back ground)
        emit_stub(&mut instances, g[2], g[3]);
        emit_stub(&mut instances, g[2], g[0]); // Edge 3: BL ground→FL ground (left ground)
        emit_stub(&mut instances, g[0], g[2]);
        emit_stub(&mut instances, r[0], r[2]); // Edge 4: FL roof→BL roof (left roof)
        emit_stub(&mut instances, r[2], r[0]);
        emit_stub(&mut instances, r[3], r[2]); // Edge 5: BR roof→BL roof (back roof)
        emit_stub(&mut instances, r[2], r[3]);

        // DrawExtras bracket corner edges (4): stubs at both ends, in front of sprite.
        emit_stub(&mut instances, g[0], g[1]); // Edge 6: FL ground→FR ground (front ground)
        emit_stub(&mut instances, g[1], g[0]);
        emit_stub(&mut instances, g[3], g[1]); // Edge 7: BR ground→FR ground (right ground)
        emit_stub(&mut instances, g[1], g[3]);
        emit_stub(&mut instances, r[0], g[0]); // Edge 8: FL roof→FL ground (FL vertical)
        emit_stub(&mut instances, g[0], r[0]);
        emit_stub(&mut instances, r[3], g[3]); // Edge 9: BR roof→BR ground (BR vertical)
        emit_stub(&mut instances, g[3], r[3]);

        // DrawExtras single-stub edges (3): only stub at the visible end.
        // All converge at hidden FR_roof corner.
        emit_stub(&mut instances, r[0], r[1]); // Edge 10: FL roof→FR roof (front roof, stub at FL)
        emit_stub(&mut instances, r[3], r[1]); // Edge 11: BR roof→FR roof (right roof, stub at BR)
        emit_stub(&mut instances, g[1], r[1]); // Edge 12: FR ground→FR roof (FR vertical, stub at FR ground)
    }

    instances
}
