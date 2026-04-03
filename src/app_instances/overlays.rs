//! Overlay, world effect, and fog snapshot instance builders.
//!
//! Generates SpriteInstances for map overlays (ore/gems, bridges, terrain objects),
//! world-position effects (warp sparkles), and fog-of-war building snapshots.
//! Extracted from app_instances.rs to keep files under the 600-line limit.
//!
//! ## Dependency rules
//! - Part of the app layer — may depend on everything.

use crate::app::AppState;
use crate::map::lighting;
use crate::map::overlay_types::is_bridge_overlay_index;
use crate::map::terrain::{self, TILE_HEIGHT, TILE_WIDTH};
use crate::render::batch::SpriteInstance;
use crate::render::bridge_atlas::is_high_bridge_body_name;
use crate::render::overlay_atlas::OverlaySpriteKey;
use crate::render::sprite_atlas::ShpSpriteKey;
use crate::rules::house_colors::HouseColorIndex;
use crate::sim::miner::ResourceType;

use super::helpers::{compute_sprite_depth, compute_sprite_depth_params, in_view};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverlayRenderBucket {
    Generic,
    Wall,
    BridgeBody,
    BridgeDetail,
}

fn bridge_y_offset_for_name(name: &str) -> f32 {
    match name.to_ascii_uppercase().as_str() {
        "BRIDGE1" | "BRIDGEB1" => -16.0,
        "BRIDGE2" | "BRIDGEB2" => -31.0,
        _ => 0.0,
    }
}

fn classify_overlay_render_bucket(
    name: &str,
    overlay_id: u8,
    is_wall: bool,
) -> OverlayRenderBucket {
    if is_wall {
        OverlayRenderBucket::Wall
    } else if is_high_bridge_body_name(name) {
        OverlayRenderBucket::BridgeBody
    } else if is_bridge_overlay_index(overlay_id) {
        OverlayRenderBucket::BridgeDetail
    } else {
        OverlayRenderBucket::Generic
    }
}

/// Build SpriteInstances for active world-position effects (warp sparkles, etc.).
///
/// Appends to the SHP instance list so they draw in the same depth-sorted pass.
/// Each effect's current frame is looked up in the SHP atlas.
pub(crate) fn build_world_effect_instances(state: &AppState, paged: &mut [Vec<SpriteInstance>]) {
    let (sim, atlas) = match (&state.simulation, &state.sprite_atlas) {
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
    for fx in &sim.world_effects {
        if fx.delay_ms > 0 {
            continue;
        }
        let (sx, sy) = terrain::iso_to_screen(fx.rx, fx.ry, fx.z);
        if !in_view(sx, sy, TILE_WIDTH, TILE_HEIGHT, cam_x, cam_y, sw, sh, 120.0) {
            continue;
        }
        let key = ShpSpriteKey {
            type_id: sim.interner.resolve(fx.shp_name).to_string(),
            facing: 0,
            frame: fx.frame,
            house_color: HouseColorIndex(0),
        };
        let Some(entry) = atlas.get(&key) else {
            continue;
        };
        let center_x: f32 = sx + TILE_WIDTH / 2.0;
        let center_y: f32 = sy + TILE_HEIGHT / 2.0;
        let depth_y: f32 = sy + TILE_HEIGHT / 2.0 + entry.offset_y + entry.pixel_size[1];
        let depth: f32 = compute_sprite_depth(state, depth_y, fx.z);
        let tint: [f32; 3] = state
            .lighting_grid
            .get(&(fx.rx, fx.ry))
            .copied()
            .unwrap_or(lighting::DEFAULT_TINT);
        paged[entry.page as usize].push(SpriteInstance {
            position: [center_x + entry.offset_x, center_y + entry.offset_y],
            size: entry.pixel_size,
            uv_origin: entry.uv_origin,
            uv_size: entry.uv_size,
            depth,
            tint,
            alpha: 1.0,
        });
    }
}

/// Build SpriteInstances for damage fire/smoke overlays on buildings below ConditionYellow.
///
/// Each fire is positioned at the building's screen origin + pixel offset from art.ini
/// `DamageFireOffset`. Fire SHPs (FIRE01/02/03) are looked up in the SHP atlas.
pub(crate) fn build_damage_fire_instances(state: &AppState, paged: &mut [Vec<SpriteInstance>]) {
    let (sim, atlas) = match (&state.simulation, &state.sprite_atlas) {
        (Some(s), Some(a)) => (s, a),
        _ => return,
    };
    let z2 = state.zoom_level;
    let (cam_x, cam_y, sw, sh) = (
        state.camera_x,
        state.camera_y,
        state.render_width() as f32 / z2,
        state.render_height() as f32 / z2,
    );
    for entity in sim.entities.values() {
        let overlays = match &entity.damage_fire_overlays {
            Some(o) => o,
            None => continue,
        };
        let pos = &entity.position;
        let (bx, by) = (pos.screen_x, pos.screen_y);
        if !in_view(bx, by, 200.0, 200.0, cam_x, cam_y, sw, sh, 200.0) {
            continue;
        }
        let tint: [f32; 3] = state
            .lighting_grid
            .get(&(pos.rx, pos.ry))
            .copied()
            .unwrap_or(lighting::DEFAULT_TINT);
        let center_x: f32 = bx;
        // Damage fires are building anims — use building anim depth bias so walls
        // overwrite them, matching the original's terrain pass step 6 ordering.
        let (origin_y, world_height) = state
            .terrain_grid
            .as_ref()
            .map(|g| (g.origin_y, g.world_height))
            .unwrap_or((0.0, 1.0));
        let fire_depth: f32 = compute_sprite_depth_params(origin_y, world_height, by, pos.z);

        for fire in &overlays.fires {
            let key = ShpSpriteKey {
                type_id: sim.interner.resolve(fire.shp_name).to_string(),
                facing: 0,
                frame: fire.frame,
                house_color: HouseColorIndex(0),
            };
            let Some(entry) = atlas.get(&key) else {
                continue;
            };
            let fx: f32 = center_x + fire.pixel_x as f32 + entry.offset_x;
            let fy: f32 = by + fire.pixel_y as f32 + entry.offset_y;
            paged[entry.page as usize].push(SpriteInstance {
                position: [fx, fy],
                size: entry.pixel_size,
                uv_origin: entry.uv_origin,
                uv_size: entry.uv_size,
                depth: fire_depth,
                tint,
                alpha: 1.0,
            });
        }
    }
}

/// Build SpriteInstances for visible overlay objects and terrain objects.
pub(crate) fn build_overlay_instances(
    state: &AppState,
    sw: f32,
    sh: f32,
    instances: &mut Vec<SpriteInstance>,
    bridge_detail_instances: &mut Vec<SpriteInstance>,
    bridge_body_instances: &mut Vec<SpriteInstance>,
    wall_instances: &mut Vec<SpriteInstance>,
) {
    let atlas = match &state.overlay_atlas {
        Some(a) => a,
        None => return,
    };
    let (cam_x, cam_y) = (state.camera_x, state.camera_y);
    let (origin_y, world_height) = state
        .terrain_grid
        .as_ref()
        .map(|g| (g.origin_y, g.world_height))
        .unwrap_or((0.0, 1.0));

    // Playable area bounds — skip overlays outside LocalSize (border filler).
    let local_bounds = state.terrain_grid.as_ref().and_then(|g| g.local_bounds);

    // Overlay entries from [OverlayPack].
    // All overlays are drawn — the shroud multiply pass handles per-pixel darkening.
    for entry in &state.overlays {
        let Some(name) = state.overlay_names.get(&entry.overlay_id) else {
            continue;
        };

        // Skip destroyed bridge overlays — the sim marks bridge cells destroyed
        // but the overlay list is static map data. Without this check, destroyed
        // bridges continue rendering visually even though units can't cross.
        if is_bridge_overlay_index(entry.overlay_id) {
            if let Some(bridge_state) = state
                .simulation
                .as_ref()
                .and_then(|s| s.bridge_state.as_ref())
            {
                if !bridge_state.is_bridge_walkable(entry.rx, entry.ry) {
                    continue;
                }
            }
        }

        // Derive the live render frame for this overlay.
        // If OverlayGrid is available, use its mutable state (handles ore density
        // changes, wall damage, bridge frame stepping). Otherwise fall back to the
        // old reverse-compute from ResourceNode for ore, or static map frame.
        let upper = name.to_ascii_uppercase();
        let overlay_flags = state
            .overlay_registry
            .as_ref()
            .and_then(|reg| reg.flags(entry.overlay_id));
        let is_wall: bool = overlay_flags.map(|f| f.wall).unwrap_or(false);
        let is_resource = upper.starts_with("TIB") || upper.starts_with("GEM");

        let render_frame: u8 = if let Some(overlay_grid) = state
            .simulation
            .as_ref()
            .and_then(|sim| sim.overlay_grid.as_ref())
        {
            let live_cell = overlay_grid.cell(entry.rx, entry.ry);
            if live_cell.overlay_id.is_none() && is_resource {
                // Overlay cleared (fully depleted) — skip rendering.
                continue;
            }
            if live_cell.overlay_id.is_some() {
                // Use live overlay_data for all overlay types.
                let base_frame = live_cell.overlay_data;
                // Healthy bridges (frame 0 or 9) get per-cell Latin square variation.
                if !is_wall && (base_frame == 0 || base_frame == 9) {
                    const BRIDGE_FRAME_VARIATION: [u8; 16] =
                        [0, 1, 2, 3, 3, 2, 1, 0, 2, 3, 0, 1, 1, 0, 3, 2];
                    let idx: usize = ((entry.ry & 3) as usize) << 2 | (entry.rx & 3) as usize;
                    base_frame + BRIDGE_FRAME_VARIATION[idx]
                } else {
                    base_frame
                }
            } else {
                // No overlay in grid — fall back to static map frame.
                entry.frame
            }
        } else {
            // No OverlayGrid — fall back to old behavior.
            if is_resource {
                match state
                    .simulation
                    .as_ref()
                    .and_then(|sim| sim.production.resource_nodes.get(&(entry.rx, entry.ry)))
                {
                    None => continue,
                    Some(node) => {
                        let base: u16 = match node.resource_type {
                            ResourceType::Ore => 120,
                            ResourceType::Gem => 180,
                        };
                        let richness = (node.remaining / base).max(1);
                        (richness - 1).min(11) as u8
                    }
                }
            } else if !is_wall && (entry.frame == 0 || entry.frame == 9) {
                const BRIDGE_FRAME_VARIATION: [u8; 16] =
                    [0, 1, 2, 3, 3, 2, 1, 0, 2, 3, 0, 1, 1, 0, 3, 2];
                let idx: usize = ((entry.ry & 3) as usize) << 2 | (entry.rx & 3) as usize;
                entry.frame + BRIDGE_FRAME_VARIATION[idx]
            } else {
                entry.frame
            }
        };

        // Bridge overlay Y-offset. The direction-dependent values come from the
        // isometric projection: EW and NS bridge decks have different vertical extents.
        // heightOffset = cell.Level * CellHeight + direction_offset.
        // Our iso_to_screen already applies cell.Level * CellHeight via z, so we only
        // add the direction_offset. The CC_Draw_Shape Z parameter is a depth value
        // for the blitter, NOT a screen Y offset — screen position comes from SHP frame
        // draw offsets + centering flag.
        //   EW (BRIDGE1/BRIDGEB1): -(CellHeight + 1) = -16px
        //   NS (BRIDGE2/BRIDGEB2): -(CellHeight * 2 + 1) = -31px
        //   Low (LOBRDG*/LOBRDB*): 0px (ground level)
        let bridge_y_offset: f32 = bridge_y_offset_for_name(&upper);
        let bucket = classify_overlay_render_bucket(&upper, entry.overlay_id, is_wall);
        // FA2 IsoView.cpp:5955-5956: track overlays render +CellHeight (15px) lower.
        let track_y_offset: f32 = if overlay_flags.map(|f| f.track).unwrap_or(false) {
            15.0
        } else {
            0.0
        };

        let z: u8 = state
            .height_map
            .get(&(entry.rx, entry.ry))
            .copied()
            .unwrap_or(0);
        let (screen_x, screen_y) = terrain::iso_to_screen(entry.rx, entry.ry, z);
        let screen_y: f32 = screen_y + bridge_y_offset + track_y_offset;

        // Playable area bounds — skip overlays outside LocalSize (border filler).
        if let Some(ref bounds) = local_bounds {
            if !bounds.contains(screen_x, screen_y) {
                continue;
            }
        }
        if !in_view(
            screen_x, screen_y, 120.0, 120.0, cam_x, cam_y, sw, sh, 120.0,
        ) {
            continue;
        }

        let key = OverlaySpriteKey {
            name: name.clone(),
            frame: render_frame,
        };
        let key_fallback = OverlaySpriteKey {
            name: name.clone(),
            frame: 0,
        };
        let spr = match bucket {
            OverlayRenderBucket::BridgeBody => {
                state.bridge_atlas.as_ref().and_then(|bridge_atlas| {
                    bridge_atlas
                        .get(&key)
                        .or_else(|| bridge_atlas.get(&key_fallback))
                })
            }
            _ => atlas.get(&key).or_else(|| atlas.get(&key_fallback)),
        };
        let Some(spr) = spr else { continue };

        // Depth for high bridges uses (tile.Level + HighBridgeHeight) where
        // HighBridgeHeight=4. This pushes bridge overlays forward in the depth
        // buffer so they render in front of terrain/water below the bridge.
        let depth_z: u8 = if matches!(bucket, OverlayRenderBucket::BridgeBody) {
            z.saturating_add(4)
        } else {
            z
        };
        // Walls use the same NW-corner render coords as buildings:
        // (Location.X - 128, Location.Y - 128). Apply -TILE_HEIGHT/2 to match.
        // Without this, a wall one cell behind a building lands at the same
        // sort depth (the building's own -TILE_HEIGHT/2 shift eats the gap)
        // and the tie-break nudge below pushes the wall in front.
        let depth_y: f32 = if is_wall {
            screen_y - TILE_HEIGHT / 2.0
        } else {
            screen_y
        };
        let raw_depth: f32 = compute_sprite_depth_params(origin_y, world_height, depth_y, depth_z);
        // Walls get a tiny depth nudge closer to the camera so they win ties
        // against building bodies at the same iso row. In the original engine,
        // walls sort AFTER other buildings at the same YSort (inserted later →
        // drawn later → in front). Our merge can't replicate insertion-order
        // tie-breaking, so this nudge ensures walls render in front of building
        // bibs/bodies at equal depth.
        let depth: f32 = if is_wall {
            (raw_depth - 0.00005).clamp(0.001, 0.999)
        } else {
            raw_depth
        };
        let tint: [f32; 3] = state
            .lighting_grid
            .get(&(entry.rx, entry.ry))
            .copied()
            .unwrap_or(lighting::DEFAULT_TINT);

        let target = match bucket {
            OverlayRenderBucket::Wall => &mut *wall_instances,
            OverlayRenderBucket::BridgeBody => &mut *bridge_body_instances,
            OverlayRenderBucket::BridgeDetail => &mut *bridge_detail_instances,
            OverlayRenderBucket::Generic => &mut *instances,
        };
        target.push(SpriteInstance {
            position: [
                screen_x + TILE_WIDTH / 2.0 + spr.offset_x,
                screen_y + TILE_HEIGHT / 2.0 + spr.offset_y,
            ],
            size: spr.pixel_size,
            uv_origin: spr.uv_origin,
            uv_size: spr.uv_size,
            depth,
            tint,
            alpha: 1.0,
        });
    }

    if std::env::var("RA2_DEBUG_BRIDGE_RENDER_BUCKETS").is_ok() {
        log::debug!(
            "Bridge buckets: body={} detail={} generic={} walls={}",
            bridge_body_instances.len(),
            bridge_detail_instances.len(),
            instances.len(),
            wall_instances.len()
        );
    }

    // Terrain objects from [Terrain] section.
    // FA2 IsoView.cpp:6389 applies a -3px Y fudge to terrain objects (trees, rocks):
    //   drawy = ... + f_y/2 - 3 - pic.wMaxHeight/2
    const TERRAIN_OBJECT_Y_FUDGE: f32 = -3.0;
    for obj in &state.terrain_objects {
        let z: u8 = state
            .height_map
            .get(&(obj.rx, obj.ry))
            .copied()
            .unwrap_or(0);
        let (screen_x, screen_y) = terrain::iso_to_screen(obj.rx, obj.ry, z);
        if let Some(ref bounds) = local_bounds {
            if !bounds.contains(screen_x, screen_y) {
                continue;
            }
        }
        if !in_view(
            screen_x, screen_y, 120.0, 120.0, cam_x, cam_y, sw, sh, 120.0,
        ) {
            continue;
        }

        // Animated terrain objects (flags) cycle through all frames using the
        // global idle animation timer. Static terrain uses frame 0.
        let frame: u8 = if let Some(count) = atlas.terrain_anim_frame_count(&obj.name) {
            // RA2 terrain animation rate: ~83ms per frame (12 fps).
            const TERRAIN_ANIM_RATE_MS: u32 = 83;
            let tick = state.idle_anim_elapsed_ms / TERRAIN_ANIM_RATE_MS;
            (tick % count as u32) as u8
        } else {
            0
        };
        let key = OverlaySpriteKey {
            name: obj.name.clone(),
            frame,
        };
        let Some(spr) = atlas.get(&key) else { continue };

        let depth: f32 = compute_sprite_depth_params(origin_y, world_height, screen_y, z);
        let tint: [f32; 3] = state
            .lighting_grid
            .get(&(obj.rx, obj.ry))
            .copied()
            .unwrap_or(lighting::DEFAULT_TINT);

        instances.push(SpriteInstance {
            position: [
                screen_x + TILE_WIDTH / 2.0 + spr.offset_x,
                screen_y + TILE_HEIGHT / 2.0 + spr.offset_y + TERRAIN_OBJECT_Y_FUDGE,
            ],
            size: spr.pixel_size,
            uv_origin: spr.uv_origin,
            uv_size: spr.uv_size,
            depth,
            tint,
            alpha: 1.0,
        });
    }
}

/// Build SpriteInstances for garrison muzzle flash animations (OccupantAnim).
///
/// Each flash is positioned at the building's screen origin + pixel offset
/// from art.ini MuzzleFlashN. Mirrors `build_damage_fire_instances` but reads
/// from the `AppState.garrison_muzzle_flashes` queue instead of per-entity overlays.
pub(crate) fn build_garrison_muzzle_flash_instances(
    state: &AppState,
    paged: &mut [Vec<SpriteInstance>],
) {
    let (sim, atlas) = match (&state.simulation, &state.sprite_atlas) {
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
    let (origin_y, world_height) = state
        .terrain_grid
        .as_ref()
        .map(|g| (g.origin_y, g.world_height))
        .unwrap_or((0.0, 1.0));

    for flash in &state.garrison_muzzle_flashes {
        let entity = match sim.entities.get(flash.building_id) {
            Some(e) => e,
            None => continue,
        };
        let pos = &entity.position;
        let (bx, by) = (pos.screen_x, pos.screen_y);
        if !in_view(bx, by, 200.0, 200.0, cam_x, cam_y, sw, sh, 200.0) {
            continue;
        }
        let key = ShpSpriteKey {
            type_id: sim.interner.resolve(flash.shp_name).to_string(),
            facing: 0,
            frame: flash.frame,
            house_color: HouseColorIndex(0),
        };
        let Some(entry) = atlas.get(&key) else {
            continue;
        };
        let fx: f32 = bx + flash.pixel_x as f32 + entry.offset_x;
        let fy: f32 = by + flash.pixel_y as f32 + entry.offset_y;
        let tint: [f32; 3] = state
            .lighting_grid
            .get(&(pos.rx, pos.ry))
            .copied()
            .unwrap_or(lighting::DEFAULT_TINT);
        let depth: f32 = compute_sprite_depth_params(origin_y, world_height, by, pos.z);
        paged[entry.page as usize].push(SpriteInstance {
            position: [fx, fy],
            size: entry.pixel_size,
            uv_origin: entry.uv_origin,
            uv_size: entry.uv_size,
            depth,
            tint,
            alpha: 1.0,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{OverlayRenderBucket, bridge_y_offset_for_name, classify_overlay_render_bucket};

    #[test]
    fn high_bridge_body_uses_dedicated_bucket() {
        assert_eq!(
            classify_overlay_render_bucket("BRIDGE1", 24, false),
            OverlayRenderBucket::BridgeBody
        );
        assert_eq!(
            classify_overlay_render_bucket("BRIDGEB2", 238, false),
            OverlayRenderBucket::BridgeBody
        );
    }

    #[test]
    fn non_body_bridge_overlay_stays_passthrough() {
        assert_eq!(
            classify_overlay_render_bucket("LOBRDG10", 83, false),
            OverlayRenderBucket::BridgeDetail
        );
    }

    #[test]
    fn bridge_offsets_match_direction() {
        assert_eq!(bridge_y_offset_for_name("BRIDGE1"), -16.0);
        assert_eq!(bridge_y_offset_for_name("BRIDGE2"), -31.0);
        assert_eq!(bridge_y_offset_for_name("LOBRDG10"), 0.0);
    }
}
