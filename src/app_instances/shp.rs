//! SHP entity instance builders — per-frame SpriteInstance generation for buildings and infantry.
//!
//! Handles building animation overlays (Active/Idle/Special), bibs, build-up
//! animations, and infantry sprite frame resolution.
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
use crate::map::terrain::TILE_HEIGHT;
use crate::render::batch::SpriteInstance;
use crate::render::sprite_atlas::ShpSpriteKey;
use crate::render::unit_atlas::{canonical_turret_facing, UnitSpriteKey, VxlLayer};
use crate::rules::house_colors::HouseColorIndex;
use crate::sim::animation;
use crate::sim::components::BuildingUp;

/// Iterate visible SHP sprite entities from EntityStore and build SpriteInstances.
///
/// Build SpriteInstances for all SHP entities (buildings, infantry).
/// Building bibs and anims are emitted into `paged` together with bodies,
/// matching the original engine where bibs are drawn inside BuildingClass_DrawBody.
/// `unit_instances` receives building turret VXLs (drawn after building bodies).
pub(crate) fn build_shp_instances(
    state: &AppState,
    paged: &mut [Vec<SpriteInstance>],
    bridge_paged: &mut [Vec<SpriteInstance>],
    unit_instances: &mut Vec<SpriteInstance>,
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
    let local_owner = crate::app_commands::preferred_local_owner_name(state);
    let local_owner_id = local_owner.as_deref().and_then(|o| sim.interner.get(o));
    let ignore_visibility = state.sandbox_full_visibility;
    let art_reg: Option<&crate::rules::art_data::ArtRegistry> = state.art_registry.as_ref();

    for entity in sim.entities.values().filter(|e| !e.is_voxel) {
        // Skip entities inside a transport/garrison — they are hidden from the map.
        if entity.passenger_role.is_inside_transport() {
            continue;
        }
        let owner_str = sim.interner.resolve(entity.owner);
        let type_str = sim.interner.resolve(entity.type_ref);
        // Wall buildings render as overlays (auto-tiled connectivity frames).
        // Their Y-sorted rendering in the object pass is handled by including
        // wall overlay instances in the unified merge (draw_merged_object_pass),
        // not here. Skip them to avoid drawing frame 0 (isolated pillar).
        if entity.category == EntityCategory::Structure {
            let is_wall = state
                .rules
                .as_ref()
                .and_then(|r| r.object(type_str))
                .map(|o| o.wall)
                .unwrap_or(false);
            if is_wall {
                continue;
            }
        }
        let pos = &entity.position;
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
        // Screen position is computed by the sim layer (lepton_to_screen) every
        // tick with the correct z. No renderer-side interpolation needed.
        let (sx, sy, interp_z) = (pos.screen_x, pos.screen_y, pos.z);
        if !in_view(sx, sy, 200.0, 200.0, cam_x, cam_y, sw, sh, 200.0) {
            continue;
        }
        let hc: HouseColorIndex = state
            .house_color_map
            .get(owner_str)
            .copied()
            .unwrap_or(crate::rules::house_colors::NO_REMAP);
        // Determine if this building is in its make/build-up or build-down animation.
        let is_building_up: bool =
            entity.category == EntityCategory::Structure && entity.building_up.is_some();
        let is_building_down: bool =
            entity.category == EntityCategory::Structure && entity.building_down.is_some();
        let (shp_frame, make_type_id): (u16, Option<String>) = if is_building_up {
            let bu: &BuildingUp = entity.building_up.as_ref().expect("checked above");
            let make_key: String = format!("{}_MAKE", type_str);
            let total_make_frames: u16 =
                atlas.make_frame_counts.get(&make_key).copied().unwrap_or(0);
            if total_make_frames > 0 {
                // Map elapsed ticks to make frame index (forward: 0 → last).
                let progress: f32 = bu.elapsed_ticks as f32 / bu.total_ticks.max(1) as f32;
                let frame: u16 =
                    ((progress * total_make_frames as f32) as u16).min(total_make_frames - 1);
                (frame, Some(make_key))
            } else {
                (0, None)
            }
        } else if is_building_down {
            let bd = entity.building_down.as_ref().expect("checked above");
            let make_key: String = format!("{}_MAKE", type_str);
            let total_make_frames: u16 =
                atlas.make_frame_counts.get(&make_key).copied().unwrap_or(0);
            if total_make_frames > 0 {
                // Map elapsed ticks to make frame index in reverse (last → 0).
                let progress: f32 = bd.elapsed_ticks as f32 / bd.total_ticks.max(1) as f32;
                let reverse_frame: u16 = total_make_frames.saturating_sub(1).saturating_sub(
                    ((progress * total_make_frames as f32) as u16).min(total_make_frames - 1),
                );
                (reverse_frame, Some(make_key))
            } else {
                (0, None)
            }
        } else {
            match entity.category {
                EntityCategory::Structure => (0, None),
                _ => (
                    resolve_infantry_shp_frame(
                        state,
                        type_str,
                        entity.facing,
                        entity.animation.as_ref(),
                    ),
                    None,
                ),
            }
        };
        let key: ShpSpriteKey = ShpSpriteKey {
            type_id: make_type_id
                .as_deref()
                .unwrap_or(type_str)
                .to_string(),
            facing: 0,
            frame: shp_frame,
            house_color: hc,
        };
        let Some(entry) = atlas.get(&key) else {
            continue;
        };

        let final_x: f32 = sx + entry.offset_x;
        let final_y: f32 = sy + entry.offset_y;
        let base_depth: f32 = match entity.category {
            EntityCategory::Structure => {
                // Building render coords use (Location.X - 128, Location.Y - 128) — the
                // NW cell origin, not center. YSort = X + Y from those coords. In screen
                // space the -128 lepton shift equals -TILE_HEIGHT/2 on iso_row.
                // Our iso_to_screen bakes in +TILE_HEIGHT/2, so subtract it.
                compute_sprite_depth(state, sy - TILE_HEIGHT / 2.0, interp_z)
            }
            _ => {
                let depth_y: f32 = sy + entry.offset_y + entry.pixel_size[1];
                compute_sprite_depth(state, depth_y, interp_z)
            }
        };
        let depth: f32 = apply_bridge_depth_bias(state, entity, base_depth);
        let mut tint: [f32; 3] = state
            .lighting_grid
            .get(&(pos.rx, pos.ry))
            .copied()
            .unwrap_or(lighting::DEFAULT_TINT);
        // Entity ambient glow so infantry are visible on dark maps.
        // Buildings do NOT get entity glow — they use ExtraLight and point lights instead.
        if entity.category == EntityCategory::Infantry {
            if let Some(rules) = &state.rules {
                let glow = rules.general.extra_infantry_light;
                if glow > 0.0 {
                    tint[0] = (tint[0] + glow).min(lighting::TOTAL_AMBIENT_CAP);
                    tint[1] = (tint[1] + glow).min(lighting::TOTAL_AMBIENT_CAP);
                    tint[2] = (tint[2] + glow).min(lighting::TOTAL_AMBIENT_CAP);
                }
            }
        }
        let target_pages = if is_under_bridge_render_state(state, entity)
            && entity.category != EntityCategory::Structure
        {
            &mut *bridge_paged
        } else {
            &mut *paged
        };
        target_pages[entry.page as usize].push(SpriteInstance {
            position: [final_x, final_y],
            size: entry.pixel_size,
            uv_origin: entry.uv_origin,
            uv_size: entry.uv_size,
            depth,
            tint,
            alpha: 1.0,
        });

        // Emit building animation overlays and bib — but NOT during build-up/down.
        // Bibs and anims use the raw cell position (sy) — their own SHP offsets
        // (baked into the canvas) handle correct placement relative to the cell.
        if entity.category == EntityCategory::Structure && !is_building_up && !is_building_down {
            if let Some(art) = art_reg {
                // Bib is drawn INSIDE BuildingClass_DrawBody in the original engine,
                // right after the main body sprite, as part of the same object pass.
                // It overwrites the body's terrain-colored pixels at the ramp area.
                emit_building_bib(
                    paged,
                    atlas,
                    art,
                    state.rules.as_ref(),
                    type_str,
                    hc,
                    sx,
                    sy,
                    interp_z,
                    depth,
                    tint,
                );
                // Building anims render in the same pass as building bodies so they
                // can sort together via depth. Anims use the building's entity depth
                // so they render at the same depth as the body — visible where the
                // body has transparent pixels, covered where it's opaque.
                let is_garrisoned = entity
                    .passenger_role
                    .cargo()
                    .is_some_and(|c| !c.is_empty());
                let is_player_owned =
                    !crate::rules::house_colors::is_non_player_house(owner_str);
                emit_building_anims(
                    paged,
                    atlas,
                    art,
                    state.rules.as_ref(),
                    type_str,
                    hc,
                    sx,
                    sy,
                    depth,
                    tint,
                    entity.building_anim_overlays.as_ref(),
                    state.idle_anim_elapsed_ms,
                    Some(&sim.interner),
                    entity.dock_active_anim,
                    is_garrisoned,
                    is_player_owned,
                );
            }
            // Emit VXL turret on top of building (e.g., SAM site, Prism Tower).
            if let Some(rules_obj) = state
                .rules
                .as_ref()
                .and_then(|r| r.object(type_str))
            {
                if rules_obj.turret_anim_is_voxel {
                    if let Some(turret_id) = &rules_obj.turret_anim {
                        emit_building_turret_vxl(
                            unit_instances,
                            state,
                            turret_id,
                            entity.turret_facing.unwrap_or(0u16),
                            hc,
                            sx,
                            sy,
                            interp_z,
                            depth,
                            tint,
                            rules_obj.turret_anim_x,
                            rules_obj.turret_anim_y,
                            rules_obj.turret_anim_z_adjust,
                        );
                    }
                }
            }
        }
    }
}

/// Emit a VXL turret sprite on top of a building (e.g., SAM site turret, Prism Tower).
///
/// Looks up the pre-rendered turret VXL from the UnitAtlas at the current turret facing,
/// positioned at the building's screen origin + pixel offset from TurretAnimX/Y.
fn emit_building_turret_vxl(
    instances: &mut Vec<SpriteInstance>,
    state: &AppState,
    turret_id: &str,
    turret_facing: u16,
    hc: HouseColorIndex,
    building_sx: f32,
    building_sy: f32,
    _z: u8,
    building_depth: f32,
    tint: [f32; 3],
    anim_x: i32,
    anim_y: i32,
    _z_adjust: i32,
) {
    let unit_atlas = match &state.unit_atlas {
        Some(a) => a,
        None => return,
    };
    let key = UnitSpriteKey {
        type_id: turret_id.to_string(),
        facing: canonical_turret_facing(turret_facing),
        house_color: hc,
        layer: VxlLayer::Composite,
        frame: 0,
        slope_type: 0, // building turrets don't tilt on slopes
    };
    let Some(entry) = unit_atlas.get(&key) else {
        return;
    };
    // Position turret at building cell origin + pixel offset from INI.
    // ZAdjust affects screen Y position (verified: AnimClass::DrawIt reads it 4 times alongside YDrawOffset).
    let center_x: f32 = building_sx;
    let tx: f32 = center_x + anim_x as f32 + entry.offset_x;
    let ty: f32 = building_sy + anim_y as f32 + entry.offset_y + 3.0;
    // Turret uses same depth as building body. It draws on top via draw
    // order (pushed to the instance list after the building body), not via
    // a depth bias — buildings use single-depth batch shader, not per-pixel Z.
    let turret_depth: f32 = building_depth;
    instances.push(SpriteInstance {
        position: [tx, ty],
        size: entry.pixel_size,
        uv_origin: entry.uv_origin,
        uv_size: entry.uv_size,
        depth: turret_depth,
        tint,
        alpha: 1.0,
    });
}

/// Emit the BibShape SpriteInstance for a building's ground-level pad.
///
/// BibShape is a separate SHP (e.g., GAREFNBB for the Allied Refinery dock) drawn
/// behind the building at the same cell position. It provides the flat ground
/// surface where harvesters dock or other ground-level detail.
fn emit_building_bib(
    paged: &mut [Vec<SpriteInstance>],
    atlas: &crate::render::sprite_atlas::SpriteAtlas,
    art_reg: &crate::rules::art_data::ArtRegistry,
    rules: Option<&crate::rules::ruleset::RuleSet>,
    building_type: &str,
    house_color: HouseColorIndex,
    screen_x: f32,
    screen_y: f32,
    _z: u8,
    building_depth: f32,
    tint: [f32; 3],
) {
    let rules_image: String = rules
        .and_then(|r| r.object(building_type))
        .map(|o| o.image.clone())
        .unwrap_or_else(|| building_type.to_string());
    let art_entry = match art_reg.resolve_metadata_entry(building_type, &rules_image) {
        Some(e) => e,
        None => return,
    };
    let bib_name: &str = match art_entry.bib_shape.as_deref() {
        Some(name) => name,
        None => return,
    };
    let bib_key: ShpSpriteKey = ShpSpriteKey {
        type_id: bib_name.to_uppercase(),
        facing: 0,
        frame: 0,
        house_color,
    };
    let Some(bib_entry) = atlas.get(&bib_key) else {
        return;
    };
    let bx: f32 = screen_x + bib_entry.offset_x;
    let by: f32 = screen_y + bib_entry.offset_y;
    // The bib is drawn inside the building's draw body pass — it doesn't sort
    // independently. The entire building (body + bib) sorts as one unit at the
    // building's YSort position. Use the building's depth so bib and body stay
    // together in the Y-sorted merge, preventing bibs from incorrectly
    // overlapping walls at closer iso rows.
    paged[bib_entry.page as usize].push(SpriteInstance {
        position: [bx, by],
        size: bib_entry.pixel_size,
        uv_origin: bib_entry.uv_origin,
        uv_size: bib_entry.uv_size,
        depth: building_depth,
        tint,
        alpha: 1.0,
    });
}

/// Compute the current frame for a looping animation driven by the global elapsed timer.
///
/// Supports PingPong mode (bounces: 0→1→2→3→2→1→0→...) and linear looping (0→1→2→3→0→...).
/// LoopEnd in RA2 art.ini is **inclusive** — LoopStart=0,LoopEnd=3 means 4 frames (0,1,2,3).
fn looping_frame(anim: &crate::rules::art_data::BuildingAnimConfig, elapsed_ms: u32) -> u16 {
    // LoopEnd is EXCLUSIVE in RA2 art.ini — e.g. GAPOWR_A has LoopStart=0,
    // LoopEnd=8 meaning frames 0..8 (0-7), while GAPOWR_AD starts at frame 8.
    // The ranges are contiguous: normal=[0..8), damaged=[8..16).
    let range: u16 = anim.loop_end.saturating_sub(anim.loop_start).max(1);
    let rate: u32 = (anim.rate as u32).max(1) * 2;
    let tick: u32 = elapsed_ms / rate;

    if anim.ping_pong && range > 1 {
        // PingPong cycle: 0,1,2,...,N-1,N-2,...,1 → cycle length = 2*(N-1).
        let cycle: u32 = 2 * (range as u32 - 1);
        let pos: u32 = tick % cycle;
        let offset: u16 = if pos < range as u32 {
            pos as u16
        } else {
            // Bouncing back: cycle - pos.
            (cycle - pos) as u16
        };
        anim.loop_start + offset
    } else {
        anim.loop_start + (tick % range as u32) as u16
    }
}

/// Emit SpriteInstances for a building's animation overlays.
///
/// Each anim overlay (e.g., CAOILD_A for Oil Derrick's tower) is looked up
/// in the sprite atlas and positioned at the building's cell center + the
/// animation's (X, Y) pixel offset from art.ini.
fn emit_building_anims(
    paged: &mut [Vec<SpriteInstance>],
    atlas: &crate::render::sprite_atlas::SpriteAtlas,
    art_reg: &crate::rules::art_data::ArtRegistry,
    rules: Option<&crate::rules::ruleset::RuleSet>,
    building_type: &str,
    house_color: HouseColorIndex,
    screen_x: f32,
    screen_y: f32,
    building_depth: f32,
    tint: [f32; 3],
    overlays: Option<&crate::sim::components::BuildingAnimOverlays>,
    idle_anim_elapsed_ms: u32,
    interner: Option<&crate::sim::intern::StringInterner>,
    dock_active_anim: bool,
    is_garrisoned: bool,
    is_player_owned: bool,
) {
    let rules_image: String = rules
        .and_then(|r| r.object(building_type))
        .map(|o| o.image.clone())
        .unwrap_or_else(|| building_type.to_string());
    let art_entry = match art_reg.resolve_metadata_entry(building_type, &rules_image) {
        Some(e) => e,
        None => return,
    };
    for anim in &art_entry.building_anims {
        // Determine current frame based on animation type and art.ini properties.
        //
        // One-shot anims (Active/Production with LoopCount>0): driven by ECS overlays.
        // Infinite-loop anims (LoopCount=-1 or IdleAnim): driven by global elapsed timer.
        // Special/Super: event-triggered one-shot — skip entirely if not in overlays.
        let anim_upper: String = anim.anim_type.to_uppercase();
        let anim_upper_id: Option<crate::sim::intern::InternedId> = interner.and_then(|i| i.get(&anim_upper));
        let frame: u16 = if matches!(
            anim.kind,
            crate::rules::art_data::BuildingAnimKind::ActiveGarrisoned
        ) {
            // ActiveAnimGarrisoned: only show when building has garrison occupants.
            // Loops continuously while garrisoned, hidden otherwise.
            if is_garrisoned {
                looping_frame(anim, idle_anim_elapsed_ms)
            } else {
                continue;
            }
        } else if matches!(
            anim.kind,
            crate::rules::art_data::BuildingAnimKind::Active
                | crate::rules::art_data::BuildingAnimKind::Production
        ) {
            if dock_active_anim
                && matches!(anim.kind, crate::rules::art_data::BuildingAnimKind::Active)
            {
                // A miner is docked and unloading — force-play the ActiveAnim
                // (unloading arm / conveyor) using the global elapsed timer.
                looping_frame(anim, idle_anim_elapsed_ms)
            } else if anim.loop_count < 0 {
                // Infinite loop ActiveAnim on a capturable tech building
                // (Oil Derrick, Airport, etc.): the primary slot (ActiveAnim)
                // only plays after capture. Decorative civilian buildings
                // (country flags, etc.) always animate.
                let is_capturable: bool = rules
                    .and_then(|r| r.object(building_type))
                    .map(|o| o.capturable)
                    .unwrap_or(false);
                if anim.is_primary && is_capturable && !is_player_owned {
                    anim.start_frame
                } else {
                    looping_frame(anim, idle_anim_elapsed_ms)
                }
            } else {
                // One-shot: look up current frame from ECS BuildingAnimOverlays component.
                overlays
                    .and_then(|o| o.anims.iter().find(|a| anim_upper_id == Some(a.anim_type)))
                    .map(|a| a.frame)
                    .unwrap_or_else(|| resting_building_anim_frame(anim))
            }
        } else if matches!(anim.kind, crate::rules::art_data::BuildingAnimKind::Idle) {
            looping_frame(anim, idle_anim_elapsed_ms)
        } else {
            // Special/Super are one-shot event-triggered animations (e.g., GAREFNOR ore
            // conveyor). Only render if actively playing in the BuildingAnimOverlays state.
            // When not triggered, skip this anim entirely — don't show frame 0.
            match overlays.and_then(|o| o.anims.iter().find(|a| anim_upper_id == Some(a.anim_type))) {
                Some(s) if !s.finished => s.frame,
                _ => continue,
            }
        };
        // If the computed frame isn't in the atlas, fall back to the last
        // available frame rather than skipping the overlay entirely.
        // This prevents a visual glitch where the anim disappears for one
        // tick when the atlas has fewer frames than the art.ini loop range.
        let mut anim_key: ShpSpriteKey = ShpSpriteKey {
            type_id: anim.anim_type.clone(),
            facing: 0,
            frame,
            house_color,
        };
        let mut anim_entry_opt = atlas.get(&anim_key);
        if anim_entry_opt.is_none() && frame > 0 {
            // Try the previous frame as fallback.
            anim_key.frame = frame - 1;
            anim_entry_opt = atlas.get(&anim_key);
        }
        let Some(anim_entry) = anim_entry_opt else {
            continue;
        };

        // Position: cell center + anim X/Y offset from art.ini.
        // Building anims use building positioning (building convention).
        // The anim's own draw offset (XDrawOffset/YDrawOffset) is already baked
        // into anim_entry.offset_x/y by the sprite atlas builder.
        let ax: f32 = screen_x + anim.x as f32 + anim_entry.offset_x;
        let ay: f32 = screen_y + anim.y as f32 + anim_entry.offset_y;

        // Building anims use the same depth as the building body. In the original
        // engine, building overlay anims render in terrain pass step 6 (before walls
        // in step 7). With painter's algorithm, anims are emitted in the same pass as
        // the building body, so they draw on top via instance order.
        // YSortAdjust from art.ini affects draw ORDER in the original, not depth.
        let anim_depth: f32 = building_depth;

        paged[anim_entry.page as usize].push(SpriteInstance {
            position: [ax, ay],
            size: anim_entry.pixel_size,
            uv_origin: anim_entry.uv_origin,
            uv_size: anim_entry.uv_size,
            depth: anim_depth,
            tint,
            alpha: 1.0,
        });
    }
}

fn resting_building_anim_frame(anim: &crate::rules::art_data::BuildingAnimConfig) -> u16 {
    if anim.loop_end > anim.loop_start {
        // LoopEnd is exclusive — last valid frame is loop_end - 1.
        anim.loop_end - 1
    } else {
        anim.start_frame
    }
}

fn resolve_infantry_shp_frame(
    state: &AppState,
    type_id: &str,
    facing: u8,
    anim: Option<&animation::Animation>,
) -> u16 {
    // Pass raw facing (not canonical) to resolve_shp_frame so the
    // facing-to-index division works correctly for any facing count
    // (6, 8, 10, etc.). The absolute frame index encodes the direction.
    let sequence_set = state.animation_sequences.get(type_id);
    if let (Some(anim_state), Some(set)) = (anim, sequence_set) {
        if let Some(def) = set.get(&anim_state.sequence) {
            return animation::resolve_shp_frame(def, facing, anim_state.frame_index);
        }
    }
    // Fallback to stand frame bucket (0..7), counter-clockwise SHP order.
    // +32 offset for isometric rotation (SHP frame 0 = screen-N, not cell-N).
    (8 - (facing.wrapping_add(32) / 32) as u16) % 8
}

#[cfg(test)]
mod tests {
    use super::resting_building_anim_frame;
    use crate::rules::art_data::{BuildingAnimConfig, BuildingAnimKind};

    #[test]
    fn one_shot_building_anim_rests_on_last_loop_frame() {
        // LoopEnd is exclusive in RA2 art.ini: LoopEnd=8 means frames 0..8 (8 frames),
        // so the resting frame is 7 (the last valid frame before LoopEnd).
        let anim = BuildingAnimConfig {
            anim_type: "GAAIRC_A".to_string(),
            kind: BuildingAnimKind::Active,
            x: 0,
            y: 0,
            y_sort: 0,
            z_adjust: 0,
            loop_start: 0,
            loop_end: 8,
            loop_count: 1,
            rate: 100,
            start_frame: 0,
            ping_pong: false,
            is_primary: false,
        };

        assert_eq!(resting_building_anim_frame(&anim), 7);
    }

    #[test]
    fn one_shot_building_anim_without_loop_range_uses_start_frame() {
        let anim = BuildingAnimConfig {
            anim_type: "TEST".to_string(),
            kind: BuildingAnimKind::Active,
            x: 0,
            y: 0,
            y_sort: 0,
            z_adjust: 0,
            loop_start: 0,
            loop_end: 0,
            loop_count: 1,
            rate: 100,
            start_frame: 3,
            ping_pong: false,
            is_primary: false,
        };

        assert_eq!(resting_building_anim_frame(&anim), 3);
    }
}
