//! Voxel unit instance builders — per-frame SpriteInstance generation for VXL entities.
//!
//! Handles turret/barrel separation, harvest overlays, and VXL animation frames.
//! Extracted from app_instances.rs to keep files under the 600-line limit.
//!
//! ## Dependency rules
//! - Part of the app layer — may depend on everything.

use super::helpers::{
    apply_bridge_depth_bias, compute_sprite_depth, in_view, is_entity_visible_for_local_owner,
    is_under_bridge_render_state,
};
use crate::app::AppState;
use crate::map::entities::EntityCategory;
use crate::map::lighting;
use crate::map::terrain::{TILE_HEIGHT, TILE_WIDTH};
use crate::render::batch::SpriteInstance;
use crate::render::sprite_atlas::ShpSpriteKey;
use crate::render::unit_atlas::{
    UnitSpriteKey, VxlLayer, canonical_turret_facing, canonical_unit_facing,
};
use crate::rules::house_colors::HouseColorIndex;
use crate::sim::components::HarvestOverlay;

/// Iterate visible voxel units from EntityStore and build SpriteInstances.
///
/// Non-turret units emit a single Composite sprite. Turret units emit up to 3
/// sprites: Body at body facing, Turret + Barrel at turret facing with screen
/// offset computed from art.ini TurretOffset.
pub(crate) fn build_unit_instances(
    state: &AppState,
    instances: &mut Vec<SpriteInstance>,
    bridge_instances: &mut Vec<SpriteInstance>,
    shp_paged: &mut [Vec<SpriteInstance>],
) {
    let (sim, atlas) = match (&state.simulation, &state.unit_atlas) {
        (Some(s), Some(a)) => (s, a),
        _ => return,
    };
    let z = state.zoom_level;
    let (cam_x, cam_y, sw, sh) = (
        state.camera_x,
        state.camera_y,
        state.render_width() as f32 / z,
        state.render_height() as f32 / z,
    );
    let local_owner = crate::app_commands::preferred_local_owner_name(state);
    let local_owner_id = local_owner.as_deref().and_then(|o| sim.interner.get(o));
    let ignore_visibility = state.sandbox_full_visibility;
    let art_reg: Option<&crate::rules::art_data::ArtRegistry> = state.art_registry.as_ref();

    for entity in sim.entities.values().filter(|e| e.is_voxel) {
        // Skip entities inside a transport — they are hidden from the map.
        if entity.passenger_role.is_inside_transport() {
            continue;
        }
        let pos = &entity.position;
        let owner_str = sim.interner.resolve(entity.owner);
        let type_str = sim.interner.resolve(entity.type_ref);
        if !is_entity_visible_for_local_owner(
            local_owner.as_deref(),
            &sim.fog,
            pos,
            owner_str,
            ignore_visibility,
            local_owner_id,
        ) {
            continue;
        }
        // Determine terrain slope under this entity for tilted VXL rendering.
        // Aircraft fly above terrain and never tilt on slopes.
        let slope_type: u8 = if entity.category == EntityCategory::Aircraft {
            0
        } else {
            state
                .resolved_terrain
                .as_ref()
                .and_then(|t| t.cell(pos.rx, pos.ry))
                .map(|c| if c.slope_type <= 8 { c.slope_type } else { 0 })
                .unwrap_or(0)
        };
        // Screen position is computed by the sim layer (lepton_to_screen) every
        // tick with the correct z. No renderer-side interpolation needed.
        // Aircraft altitude: offset screen Y upward so flying units appear above ground.
        // 0.06 px per lepton → cruise altitude (600) ≈ 36px up (~2.4 elevation levels).
        let altitude_y_offset: f32 = entity
            .locomotor
            .as_ref()
            .map(|l| crate::util::fixed_math::sim_to_f32(l.altitude) * 0.06)
            .unwrap_or(0.0);
        let (sx, sy, interp_z) = (pos.screen_x, pos.screen_y - altitude_y_offset, pos.z);
        if !in_view(sx, sy, TILE_WIDTH, TILE_HEIGHT, cam_x, cam_y, sw, sh, 120.0) {
            continue;
        }
        let hc: HouseColorIndex = state
            .house_color_map
            .get(owner_str)
            .copied()
            .unwrap_or_default();
        let mut tint: [f32; 3] = state
            .lighting_grid
            .get(&(pos.rx, pos.ry))
            .copied()
            .unwrap_or(lighting::DEFAULT_TINT);
        // Entity ambient glow so VXL units/aircraft are visible on dark maps.
        if let Some(rules) = &state.rules {
            use crate::map::entities::EntityCategory;
            let glow = match entity.category {
                EntityCategory::Aircraft => rules.general.extra_aircraft_light,
                _ => rules.general.extra_unit_light,
            };
            if glow > 0.0 {
                tint[0] = (tint[0] + glow).min(lighting::TOTAL_AMBIENT_CAP);
                tint[1] = (tint[1] + glow).min(lighting::TOTAL_AMBIENT_CAP);
                tint[2] = (tint[2] + glow).min(lighting::TOTAL_AMBIENT_CAP);
            }
        }
        let center_x: f32 = sx;
        let center_y: f32 = sy;

        // Docked miners render in front of the refinery building they're on.
        // The pad cell is inside the building footprint (north of the south edge),
        // so without adjustment the miner's depth_y is above the building's
        // foundation bottom and it draws behind. We offset depth_y in screen-space
        // (not depth-space) so the correction scales naturally with map size.
        // One full tile height pushes the sort point past the foundation bottom.
        let dock_depth_y_offset: f32 = if entity
            .miner
            .as_ref()
            .is_some_and(|m| matches!(m.state, crate::sim::miner::MinerState::Dock))
        {
            TILE_HEIGHT
        } else {
            0.0
        };

        let anim_frame: u32 = entity.voxel_animation.map(|a| a.frame).unwrap_or(0);

        // Chrono warp translucency: 50% alpha while being_warped_ticks > 0.
        let alpha: f32 = if entity
            .teleport_state
            .as_ref()
            .is_some_and(|t| t.being_warped_ticks > 0)
        {
            0.5
        } else {
            1.0
        };
        let target_instances = if is_under_bridge_render_state(state, entity) {
            &mut *bridge_instances
        } else {
            &mut *instances
        };

        if let Some(turret_facing) = entity.turret_facing {
            // Turret unit: emit body, turret, and barrel as separate sprites.
            emit_turret_unit_sprites(
                target_instances,
                atlas,
                art_reg,
                entity,
                type_str,
                entity.facing,
                turret_facing,
                hc,
                center_x,
                center_y,
                state,
                interp_z,
                tint,
                alpha,
                anim_frame,
                dock_depth_y_offset,
                slope_type,
            );
        } else {
            // Non-turret unit: single composite sprite.
            let key: UnitSpriteKey = UnitSpriteKey {
                type_id: type_str.to_string(),
                facing: canonical_unit_facing(entity.facing),
                house_color: hc,
                layer: VxlLayer::Composite,
                frame: anim_frame,
                slope_type,
            };
            if let Some(entry) = atlas_get_with_frame_fallback(atlas, &key) {
                let depth_y: f32 = sy + entry.offset_y + entry.pixel_size[1] + dock_depth_y_offset;
                let depth: f32 = apply_bridge_depth_bias(
                    state,
                    entity,
                    compute_sprite_depth(state, depth_y, interp_z),
                );
                target_instances.push(SpriteInstance {
                    position: [center_x + entry.offset_x, center_y + entry.offset_y],
                    size: entry.pixel_size,
                    uv_origin: entry.uv_origin,
                    uv_size: entry.uv_size,
                    depth,
                    tint,
                    alpha,
                });
            }
        }

        // Emit harvest overlay (oregath.shp) if the miner is actively harvesting.
        // OREGATH is an SHP sprite from sprite_atlas — it must go into shp_paged
        // (not the voxel unit instance list) so it draws with the correct texture.
        if let Some(ref ho) = entity.harvest_overlay {
            if ho.visible {
                emit_harvest_overlay(
                    shp_paged,
                    state,
                    entity,
                    entity.facing,
                    ho,
                    center_x,
                    center_y,
                    pos.z,
                    tint,
                );
            }
        }
    }
}

/// Compute the screen-space offset for a turret pivot point from art.ini TurretOffset.
///
/// Rotate (0, -TurretOffset) by body facing, then convert from leptons to
/// isometric screen coordinates.
/// The offset rotates with body facing since the pivot is fixed on the hull.
fn turret_screen_offset(turret_offset: i32, body_facing: u8) -> (f32, f32) {
    if turret_offset == 0 {
        return (0.0, 0.0);
    }
    // Our VXL rasterizer uses facing/256 (not 255). Must match so offset
    // aligns with the rendered model at all facings.
    let angle: f32 = std::f32::consts::TAU * (body_facing as f32 / 256.0);
    let (sin, cos) = angle.sin_cos();
    // XNA Vector2.Transform with CreateRotationZ(angle):
    //   x' = vx * cos + vy * (-sin)
    //   y' = vx * sin + vy * cos
    // With v = (0, -TurretOffset):
    //   x' = TurretOffset * sin(angle)
    //   y' = -TurretOffset * cos(angle)
    let to: f32 = turret_offset as f32;
    let rx: f32 = to * sin;
    let ry: f32 = -to * cos;
    // Convert leptons → screen coords. CellSizeInLeptons=256, our cells are 60×30.
    let cx: f32 = rx / 256.0;
    let cy: f32 = ry / 256.0;
    let screen_x: f32 = (cx - cy) * 60.0 / 2.0;
    let screen_y: f32 = (cx + cy) * 30.0 / 2.0;
    (screen_x, screen_y)
}

/// Look up a unit sprite from the atlas with cascading fallbacks:
/// 1. Try the exact key (slope + frame).
/// 2. Fall back to frame 0 if the requested frame doesn't exist (mismatched HVA counts).
/// 3. Fall back to slope_type=0 if the tilted sprite isn't in the atlas yet
///    (unit just moved onto a ramp and the atlas hasn't rebuilt).
/// This prevents units from disappearing during atlas rebuilds.
fn atlas_get_with_frame_fallback<'a>(
    atlas: &'a crate::render::unit_atlas::UnitAtlas,
    key: &UnitSpriteKey,
) -> Option<&'a crate::render::unit_atlas::UnitSpriteEntry> {
    atlas.get(key).or_else(|| {
        // Fallback 1: try frame 0 with same slope.
        if key.frame > 0 {
            let fallback = UnitSpriteKey {
                frame: 0,
                ..key.clone()
            };
            if let Some(entry) = atlas.get(&fallback) {
                return Some(entry);
            }
        }
        // Fallback 2: try slope_type=0 (flat) with original frame.
        if key.slope_type != 0 {
            let flat_key = UnitSpriteKey {
                slope_type: 0,
                ..key.clone()
            };
            if let Some(entry) = atlas.get(&flat_key) {
                return Some(entry);
            }
            // Fallback 3: slope_type=0 + frame 0.
            if key.frame > 0 {
                let flat_frame0 = UnitSpriteKey {
                    slope_type: 0,
                    frame: 0,
                    ..key.clone()
                };
                return atlas.get(&flat_frame0);
            }
        }
        None
    })
}

/// Emit body + turret + barrel sprites for a turret-equipped voxel unit.
///
/// Body is drawn at body facing. Turret + barrel are drawn at turret facing,
/// shifted by the art.ini TurretOffset (rotated by body facing) so the turret
/// sits on its correct pivot point on the hull.
fn emit_turret_unit_sprites(
    instances: &mut Vec<SpriteInstance>,
    atlas: &crate::render::unit_atlas::UnitAtlas,
    art_reg: Option<&crate::rules::art_data::ArtRegistry>,
    entity: &crate::sim::game_entity::GameEntity,
    type_id: &str,
    body_facing: u8,
    turret_facing: u16,
    hc: HouseColorIndex,
    center_x: f32,
    center_y: f32,
    state: &AppState,
    z: u8,
    tint: [f32; 3],
    alpha: f32,
    anim_frame: u32,
    dock_depth_y_offset: f32,
    slope_type: u8,
) {
    let body_key = UnitSpriteKey {
        type_id: type_id.to_string(),
        facing: canonical_unit_facing(body_facing),
        house_color: hc,
        layer: VxlLayer::Body,
        frame: anim_frame,
        slope_type,
    };
    let turret_key = UnitSpriteKey {
        type_id: type_id.to_string(),
        facing: canonical_turret_facing(turret_facing),
        house_color: hc,
        layer: VxlLayer::Turret,
        frame: anim_frame,
        slope_type,
    };
    let barrel_key = UnitSpriteKey {
        type_id: type_id.to_string(),
        facing: canonical_turret_facing(turret_facing),
        house_color: hc,
        layer: VxlLayer::Barrel,
        frame: anim_frame,
        slope_type,
    };

    // Look up TurretOffset from art.ini and compute screen-space shift.
    let art_offset: i32 = art_reg
        .and_then(|a| a.get(type_id))
        .map(|e| e.turret_offset)
        .unwrap_or(0);
    let (tur_ox, tur_oy) = turret_screen_offset(art_offset, body_facing);

    // Emit body first (always). Uses frame fallback for mismatched HVA counts.
    if let Some(entry) = atlas_get_with_frame_fallback(atlas, &body_key) {
        let depth_y: f32 = center_y + entry.offset_y + entry.pixel_size[1] + dock_depth_y_offset;
        let depth: f32 =
            apply_bridge_depth_bias(state, entity, compute_sprite_depth(state, depth_y, z));
        instances.push(SpriteInstance {
            position: [center_x + entry.offset_x, center_y + entry.offset_y],
            size: entry.pixel_size,
            uv_origin: entry.uv_origin,
            uv_size: entry.uv_size,
            depth,
            tint,
            alpha,
        });
    }

    // Draw order for turret+barrel depends on facing direction.
    // South-facing (facing 32-160): barrel first (behind turret).
    // North-facing: turret first (behind barrel).
    // Convert to u8 for draw-order check (32..160 in u8 = 8192..40960 in u16).
    let turret_u8: u8 = (turret_facing >> 8) as u8;
    let is_south_facing: bool = turret_u8 >= 32 && turret_u8 <= 160;
    let (first_key, second_key) = if is_south_facing {
        (&barrel_key, &turret_key)
    } else {
        (&turret_key, &barrel_key)
    };

    for key in [first_key, second_key] {
        if let Some(entry) = atlas_get_with_frame_fallback(atlas, key) {
            let depth_y: f32 =
                center_y + entry.offset_y + entry.pixel_size[1] + dock_depth_y_offset;
            let depth: f32 =
                apply_bridge_depth_bias(state, entity, compute_sprite_depth(state, depth_y, z));
            instances.push(SpriteInstance {
                position: [
                    center_x + entry.offset_x + tur_ox,
                    center_y + entry.offset_y + tur_oy,
                ],
                size: entry.pixel_size,
                uv_origin: entry.uv_origin,
                uv_size: entry.uv_size,
                depth,
                tint,
                alpha,
            });
        }
    }
}

/// Arm offset in leptons for the oregath harvest overlay. The overlay is drawn offset
/// from the unit center by this distance, rotated by the body facing, so the harvest
/// arm visually tracks the correct side of the harvester.
const OREGATH_ARM_OFFSET_LEPTONS: f32 = 30.0;

/// Emit the oregath.shp harvest overlay sprite for a mining harvester.
///
/// The overlay uses the sprite atlas (keyed as "OREGATH") with 15 frames × 8 facings.
/// SHP frame index = facing_index * 15 + anim_frame.
///
/// The draw position is offset from the unit center by 30 leptons rotated by body
/// facing (verified from binary at 0x0073D12F–0x0073D1D6). This places the overlay
/// at the harvest arm position rather than dead center on the unit.
fn emit_harvest_overlay(
    shp_paged: &mut [Vec<SpriteInstance>],
    state: &AppState,
    entity: &crate::sim::game_entity::GameEntity,
    body_facing: u8,
    overlay: &HarvestOverlay,
    center_x: f32,
    center_y: f32,
    z: u8,
    tint: [f32; 3],
) {
    let sprite_atlas = match &state.sprite_atlas {
        Some(a) => a,
        None => return,
    };
    // Map body facing (0-255) to counter-clockwise SHP frame index (0..7).
    // +32 offset for isometric rotation (SHP frame 0 = screen-N, not cell-N).
    let facing_index: u16 = (8 - (body_facing.wrapping_add(32) / 32) as u16) % 8;
    let shp_frame: u16 = facing_index * 15 + overlay.frame;
    let key = ShpSpriteKey {
        type_id: "OREGATH".to_string(),
        facing: 0,
        frame: shp_frame,
        house_color: HouseColorIndex::default(),
    };
    let Some(entry) = sprite_atlas.get(&key) else {
        return;
    };
    let page = entry.page as usize;
    if page >= shp_paged.len() {
        return;
    }
    // Compute arm offset: rotate 30 leptons by body facing, then convert to screen.
    // Same sin/cos + isometric transform used by turret_screen_offset.
    let (arm_sx, arm_sy) = harvest_arm_screen_offset(body_facing);
    let draw_x: f32 = center_x + arm_sx;
    let draw_y: f32 = center_y + arm_sy;
    let depth_y: f32 = draw_y + entry.offset_y + entry.pixel_size[1];
    let depth: f32 =
        apply_bridge_depth_bias(state, entity, compute_sprite_depth(state, depth_y, z));
    shp_paged[page].push(SpriteInstance {
        position: [draw_x + entry.offset_x, draw_y + entry.offset_y],
        size: entry.pixel_size,
        uv_origin: entry.uv_origin,
        uv_size: entry.uv_size,
        depth,
        tint,
        alpha: 1.0,
    });
}

/// Convert the oregath arm offset (30 leptons) into isometric screen pixels.
///
/// Mirrors the binary's logic at 0x0073D12F–0x0073D1D6:
///   world_x = sin(angle) * 30 + base.X
///   world_y = base.Y - cos(angle) * 30
/// Then isometric projection converts leptons to screen pixels.
fn harvest_arm_screen_offset(body_facing: u8) -> (f32, f32) {
    let angle: f32 = std::f32::consts::TAU * (body_facing as f32 / 256.0);
    let (sin, cos) = angle.sin_cos();
    // World-space offset in leptons, matching the binary's sin/cos convention.
    let lx: f32 = OREGATH_ARM_OFFSET_LEPTONS * sin;
    let ly: f32 = -OREGATH_ARM_OFFSET_LEPTONS * cos;
    // Leptons → tile fractions (256 leptons per cell).
    let cx: f32 = lx / 256.0;
    let cy: f32 = ly / 256.0;
    // Isometric projection: tile offset → screen pixels (60×30 cell).
    let screen_x: f32 = (cx - cy) * 60.0 / 2.0;
    let screen_y: f32 = (cx + cy) * 30.0 / 2.0;
    (screen_x, screen_y)
}
