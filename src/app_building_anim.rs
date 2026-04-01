//! Building animation lifecycle, damage fire overlays, sidebar UI tick, and sound playback.
//!
//! These are per-frame runtime updates that run after the sim tick advances.
//! Extracted from app_sim_tick.rs to separate animation/audio/UI concerns from
//! core simulation advancement.
//!
//! ## Dependency rules
//! - Part of the app layer — may depend on everything.

use crate::app::AppState;
use crate::app_commands::preferred_local_owner_name;
use crate::map::entities::EntityCategory;
use crate::sim::components::{
    AnimOverlayState, BuildingAnimOverlays, DamageFireAnim, DamageFireOverlays, GarrisonMuzzleFlash,
};
use crate::sim::production;

/// Advance one-shot building animation overlays stored as ECS components,
/// and the global idle animation timer.
///
/// ActiveAnim plays once (one-shot) when triggered by building placement.
/// When all frames have played, the component is removed and the
/// renderer falls back to frame 0 (idle pose).
/// IdleAnims are handled via a global elapsed timer (always looping).
pub(crate) fn tick_crane_animations(state: &mut AppState, dt_ms: u32) {
    if let Some(sim) = &mut state.simulation {
        // Advance all active building overlay animations using per-anim Rate.
        let keys: Vec<u64> = sim.entities.keys_sorted();
        for &id in &keys {
            let Some(entity) = sim.entities.get_mut(id) else {
                continue;
            };
            let Some(overlays) = entity.building_anim_overlays.as_mut() else {
                continue;
            };
            for anim in overlays.anims.iter_mut() {
                if anim.finished {
                    continue;
                }
                anim.elapsed_ms += dt_ms;
                while anim.elapsed_ms >= anim.rate_ms {
                    anim.elapsed_ms -= anim.rate_ms;
                    anim.frame += 1;
                    if anim.frame >= anim.loop_end {
                        // One-shot: clamp to last frame and mark finished.
                        anim.frame = anim.loop_end.saturating_sub(1);
                        anim.finished = true;
                        break;
                    }
                }
            }
            // Remove finished anims from the vec.
            overlays.anims.retain(|a| !a.finished);
            if overlays.anims.is_empty() {
                entity.building_anim_overlays = None;
            }
        }
    }

    // Advance the global idle animation timer (looping anims: flags, smokestacks, etc.).
    state.idle_anim_elapsed_ms += dt_ms;
}

/// Spawn, remove, and advance DamageFireAnim overlays on buildings.
///
/// When a building's health drops below ConditionYellow, fire/smoke overlays are
/// created at the DamageFireOffset positions defined in art.ini. When repaired
/// above the threshold, fires are removed. Each fire loops independently.
pub(crate) fn tick_damage_fire_overlays(state: &mut AppState, dt_ms: u32) {
    let condition_yellow = state
        .rules
        .as_ref()
        .map(|r| r.general.condition_yellow)
        .unwrap_or(0.5);

    // Collect fire type info outside the entity loop to avoid borrow conflicts.
    let fire_types: Vec<(String, u32)> = state
        .rules
        .as_ref()
        .map(|r| {
            r.general
                .damage_fire_types
                .iter()
                .map(|f| (f.name.clone(), f.rate_ms))
                .collect()
        })
        .unwrap_or_default();

    if fire_types.is_empty() {
        return;
    }

    // Phase 1: Identify buildings that need new fire overlays spawned.
    // Collect (entity_id, type_ref) pairs while only holding immutable borrows.
    let needs_spawn: Vec<(u64, String)> = {
        let sim = match &state.simulation {
            Some(s) => s,
            None => return,
        };
        sim.entities
            .values()
            .filter_map(|entity| {
                if entity.category != EntityCategory::Structure {
                    return None;
                }
                if entity.health.max == 0 {
                    return None;
                }
                let ratio = entity.health.current as f32 / entity.health.max as f32;
                if ratio <= condition_yellow && entity.damage_fire_overlays.is_none() {
                    Some((
                        entity.stable_id,
                        sim.interner.resolve(entity.type_ref).to_string(),
                    ))
                } else {
                    None
                }
            })
            .collect()
    };

    // Resolve art offsets for buildings that need fires (immutable borrow of art_registry).
    let spawn_data: Vec<(u64, Vec<DamageFireAnim>)> = {
        let art_reg = state.art_registry.as_ref();
        let effect_counts = state.simulation.as_ref().map(|s| &s.effect_frame_counts);
        needs_spawn
            .into_iter()
            .filter_map(|(id, type_ref)| {
                let offsets = art_reg
                    .and_then(|a| a.get(&type_ref))
                    .map(|art| &art.damage_fire_offsets)?;
                if offsets.is_empty() {
                    return None;
                }
                let fires: Vec<DamageFireAnim> = offsets
                    .iter()
                    .enumerate()
                    .map(|(i, &(px, py))| {
                        let (ref name, rate_ms) = fire_types[i % fire_types.len()];
                        let name_id_for_lookup = state
                            .simulation
                            .as_ref()
                            .and_then(|sim| sim.interner.get(name));
                        let total_frames = effect_counts
                            .and_then(|m| m.get(&name_id_for_lookup?).copied())
                            .unwrap_or(1);
                        let start_frame = if total_frames > 1 {
                            (id.wrapping_mul(31).wrapping_add(i as u64 * 7) % total_frames as u64)
                                as u16
                        } else {
                            0
                        };
                        let shp_name_id = state
                            .simulation
                            .as_ref()
                            .map(|s| s.interner.get(name).unwrap_or_default())
                            .unwrap_or_default();
                        DamageFireAnim {
                            shp_name: shp_name_id,
                            pixel_x: px,
                            pixel_y: py,
                            frame: start_frame,
                            total_frames,
                            rate_ms,
                            elapsed_ms: 0,
                        }
                    })
                    .collect();
                Some((id, fires))
            })
            .collect()
    };

    // Phase 2: Apply spawns + advance existing + remove healed (mutable borrow of sim).
    let sim = match &mut state.simulation {
        Some(s) => s,
        None => return,
    };

    // Apply spawns.
    for (id, fires) in spawn_data {
        if let Some(entity) = sim.entities.get_mut(id) {
            entity.damage_fire_overlays = Some(DamageFireOverlays { fires });
        }
    }

    // Advance existing fire anims and remove fires on healed buildings.
    let keys: Vec<u64> = sim.entities.keys_sorted();
    for &id in &keys {
        let entity = match sim.entities.get_mut(id) {
            Some(e) => e,
            None => continue,
        };
        if entity.category != EntityCategory::Structure {
            continue;
        }
        if entity.health.max == 0 {
            continue;
        }
        let ratio = entity.health.current as f32 / entity.health.max as f32;

        if ratio > condition_yellow {
            // Healed above threshold — remove fires.
            if entity.damage_fire_overlays.is_some() {
                entity.damage_fire_overlays = None;
            }
        } else if let Some(overlays) = entity.damage_fire_overlays.as_mut() {
            // Advance fire animation frames.
            for fire in &mut overlays.fires {
                fire.elapsed_ms += dt_ms;
                while fire.elapsed_ms >= fire.rate_ms && fire.rate_ms > 0 {
                    fire.elapsed_ms -= fire.rate_ms;
                    fire.frame += 1;
                    if fire.frame >= fire.total_frames {
                        fire.frame = 0; // Loop infinitely.
                    }
                }
            }
        }
    }
}

/// Trigger a one-shot crane animation on the active producer (ConYard) for an owner.
/// Called when a building is placed on the map. Creates/updates a BuildingAnimOverlays
/// ECS component on the producer entity.
pub(crate) fn trigger_crane_anim(state: &mut AppState, owner: &str) {
    // Gather data from immutable borrows first to avoid borrow conflicts.
    let (stable_id, type_id, rules_image) = {
        let (Some(sim), Some(rules)) = (&state.simulation, &state.rules) else {
            return;
        };
        let structure_cat = production::ProductionCategory::Building;
        let producer =
            production::active_producer_for_owner_category(sim, rules, owner, structure_cat);
        let Some(view) = producer else {
            log::info!(
                "trigger_crane_anim: no active Building producer for '{}'",
                owner
            );
            return;
        };
        // Use EntityStore O(1) lookup to find the type_id.
        let Some(ge) = sim.entities.get(view.stable_id) else {
            return;
        };
        let type_str = sim.interner.resolve(ge.type_ref);
        let rules_image: String = rules
            .object(type_str)
            .map(|o| o.image.clone())
            .unwrap_or_else(|| type_str.to_string());
        (view.stable_id, type_str.to_string(), rules_image)
    };

    let Some(art_reg) = &state.art_registry else {
        return;
    };
    let Some(entry) = art_reg.resolve_metadata_entry(&type_id, &rules_image) else {
        return;
    };

    // Collect one-shot anim overlay states to attach.
    let mut new_anims: Vec<AnimOverlayState> = Vec::new();
    for anim in &entry.building_anims {
        if !matches!(
            anim.kind,
            crate::rules::art_data::BuildingAnimKind::Active
                | crate::rules::art_data::BuildingAnimKind::Production
        ) {
            continue;
        }
        // Skip infinite-loop anims (LoopCount=-1) — those loop via idle timer.
        if anim.loop_count < 0 {
            continue;
        }
        // Skip anims with no loop range.
        if anim.loop_end <= anim.loop_start {
            continue;
        }
        let anim_upper: String = anim.anim_type.to_uppercase();
        let loop_end: u16 = anim.loop_end;
        let loop_start: u16 = anim.loop_start;
        let rate: u16 = anim.rate;
        let frame_count: u16 = loop_end - loop_start;

        log::info!(
            "Crane anim triggered: owner='{}' anim='{}' frames={}-{} ({} frames) rate={}ms duration={:.0}ms",
            owner,
            anim_upper,
            loop_start,
            loop_end,
            frame_count,
            rate,
            frame_count as f32 * rate as f32,
        );
        let anim_type_id = state
            .simulation
            .as_mut()
            .map(|s| s.interner.intern(&anim_upper))
            .unwrap_or_default();
        new_anims.push(AnimOverlayState {
            anim_type: anim_type_id,
            frame: anim.start_frame.max(loop_start),
            loop_start,
            loop_end,
            rate_ms: rate as u32,
            elapsed_ms: 0,
            finished: false,
        });
    }

    if new_anims.is_empty() {
        return;
    }

    // Attach or update the BuildingAnimOverlays on the producer entity.
    let Some(sim) = &mut state.simulation else {
        return;
    };
    let Some(ge) = sim.entities.get_mut(stable_id) else {
        return;
    };
    if let Some(overlays) = ge.building_anim_overlays.as_mut() {
        // Merge: add new anims that aren't already playing.
        for new_anim in new_anims {
            let already_playing = overlays
                .anims
                .iter()
                .any(|a| a.anim_type == new_anim.anim_type);
            if !already_playing {
                overlays.anims.push(new_anim);
            }
        }
    } else {
        ge.building_anim_overlays = Some(BuildingAnimOverlays { anims: new_anims });
    }
}

/// Tick the sidebar power bar animation (segment-by-segment transition).
pub(crate) fn update_power_bar_anim(state: &mut AppState) {
    let owner_name = preferred_local_owner_name(state);
    let (power_produced, power_drained) =
        match (&state.simulation, &state.rules, owner_name.as_deref()) {
            (Some(sim), Some(rules), Some(owner)) => {
                production::power_balance_for_owner(sim, rules, owner)
            }
            _ => (0, 0),
        };
    let theoretical = match (&state.simulation, owner_name.as_deref()) {
        (Some(sim), Some(owner)) => production::theoretical_power_for_owner(sim, owner),
        _ => 0,
    };

    // Compute bar height from sidebar layout.
    let spec = state.sidebar_layout_spec;
    let sw = state.render_width() as f32;
    let sh = state.render_height() as f32;
    let layout = crate::sidebar::compute_layout_with_spec(spec, sw, sh, 0);
    let region_bottom = layout.side3_y + spec.side3_height - spec.power_bar_bottom_y;
    let region_top = layout.tabs_y + spec.power_bar_top_y;
    let bar_height_px = (region_bottom - region_top).max(0.0) as i32;

    state.power_bar_anim.set_max_segments(bar_height_px);
    state
        .power_bar_anim
        .update(power_produced, power_drained, theoretical);
    state.power_bar_anim.tick();
}

/// Update radar availability from ECS and tick the radar chrome animation.
pub(crate) fn update_radar_state(state: &mut AppState, dt_ms: f32) {
    let new_has_radar: bool = match (
        &state.simulation,
        &state.rules,
        preferred_local_owner_name(state).as_deref(),
    ) {
        (Some(sim), Some(rules), Some(owner)) => {
            crate::sim::radar::has_radar_for_owner(sim, rules, owner)
        }
        _ => false,
    };
    state.has_radar = new_has_radar;

    if let Some(ref mut ra) = state.radar_anim {
        ra.set_has_radar(new_has_radar);
        ra.tick(&state.gpu, dt_ms);
    }
}

/// Map an owner's country name to the EVA faction key used in eva.ini sections.
///
/// Returns "Allied", "Russian", or "Yuri" for lookup in `EvaRegistry::get()`.
pub(crate) fn eva_faction_key(
    owner: &str,
    house_roster: &crate::map::houses::HouseRoster,
) -> &'static str {
    // Find the house's country name from the roster.
    let country = house_roster
        .houses
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(owner))
        .and_then(|h| h.country.as_deref())
        .unwrap_or(owner);

    // Map country to EVA faction key.
    // Soviet countries use "Russian" (the key name in eva.ini).
    match country.to_ascii_lowercase().as_str() {
        "yuricountry" => "Yuri",
        "russians" | "confederation" | "africans" | "arabs" => "Russian",
        _ => "Allied",
    }
}

/// Drain pending sound events from the queue and play them through the SFX player.
///
/// Voice events (VoiceSelect, VoiceMove, VoiceAttack) are routed to the dedicated
/// voice slot which cuts off the previous voice. All other sounds go to the SFX pool.
pub(crate) fn drain_sound_events(state: &mut AppState) {
    use crate::audio::events::GameSoundEvent;
    use crate::audio::sfx::calc_spatial_volume;

    let events = state.sound_events.drain();
    if events.is_empty() {
        return;
    }
    let vp_w = state.render_width() as f32;
    let vp_h = state.render_height() as f32;
    let (Some(sfx), Some(assets)) = (&mut state.sfx_player, &state.asset_manager) else {
        return;
    };
    let cam_x = state.camera_x;
    let cam_y = state.camera_y;

    for event in &events {
        match event {
            // Voice events — always full volume (non-positional), use dedicated voice slot.
            GameSoundEvent::UnitSelected { .. }
            | GameSoundEvent::UnitMoveOrder { .. }
            | GameSoundEvent::UnitAttackOrder { .. } => {
                sfx.play_voice_sound(
                    event.sound_id(),
                    &state.sound_registry,
                    assets,
                    &state.audio_indices,
                );
            }
            // EVA events — temporarily disabled.
            GameSoundEvent::BuildingReady { .. } | GameSoundEvent::UnitReady { .. } => {}
            // UI events — always full volume (non-positional).
            GameSoundEvent::UiSound { .. } => {
                sfx.play_sound(
                    event.sound_id(),
                    &state.sound_registry,
                    assets,
                    &state.audio_indices,
                );
            }
            // Spatial events — apply distance-based volume scaling using
            // per-sound Range and MinVolume from sound.ini.
            _ => {
                let spatial_vol = if let Some((sx, sy)) = event.screen_pos() {
                    let (range, min_vol) = state
                        .sound_registry
                        .get(event.sound_id())
                        .map(|e| (e.range, e.min_volume))
                        .unwrap_or((crate::audio::sfx::DEFAULT_RANGE_CELLS, 0));
                    calc_spatial_volume(sx, sy, vp_w, vp_h, cam_x, cam_y, range, min_vol)
                } else {
                    1.0
                };

                if spatial_vol > 0.0 {
                    sfx.play_sound_with_volume(
                        event.sound_id(),
                        spatial_vol,
                        &state.sound_registry,
                        assets,
                        &state.audio_indices,
                    );
                }
            }
        }
    }
}

/// Spawn new garrison muzzle flash animations from pending fire events and
/// advance existing ones. One-shot flashes are removed when their animation
/// completes.
///
/// Fire events with `garrison_muzzle_index` and `occupant_anim` produce a
/// short OccupantAnim SHP (e.g., UCFLASH) at the building's MuzzleFlash
/// pixel offset from art.ini.
pub(crate) fn tick_garrison_muzzle_flashes(state: &mut AppState, dt_ms: u32) {
    // Phase 1: spawn new flashes from pending fire events.
    let new_flashes: Vec<GarrisonMuzzleFlash> = {
        let sim = match &state.simulation {
            Some(s) => s,
            None => {
                state.garrison_muzzle_flashes.clear();
                return;
            }
        };
        let art_reg = match &state.art_registry {
            Some(a) => a,
            None => {
                state.garrison_muzzle_flashes.clear();
                return;
            }
        };
        let rules = match &state.rules {
            Some(r) => r,
            None => {
                state.garrison_muzzle_flashes.clear();
                return;
            }
        };
        state
            .pending_fire_effects
            .iter()
            .filter_map(|ev| {
                let muzzle_idx = ev.garrison_muzzle_index? as usize;
                let anim_name = ev.occupant_anim.as_ref()?;
                let entity = sim.entities.get(ev.attacker_id)?;
                let etype_str = sim.interner.resolve(entity.type_ref);
                let rules_image = rules
                    .object(etype_str)
                    .map(|o| o.image.clone())
                    .unwrap_or_else(|| etype_str.to_string());
                let art = art_reg.resolve_metadata_entry(etype_str, &rules_image)?;
                if art.muzzle_flash_positions.is_empty() {
                    return None;
                }
                let (px, py) =
                    art.muzzle_flash_positions[muzzle_idx % art.muzzle_flash_positions.len()];
                let total_frames = sim.effect_frame_counts.get(anim_name).copied().unwrap_or(1);
                Some(GarrisonMuzzleFlash {
                    building_id: ev.attacker_id,
                    shp_name: anim_name.clone(),
                    pixel_x: px,
                    pixel_y: py,
                    frame: 0,
                    total_frames,
                    rate_ms: 67, // ~15fps, standard for RA2 muzzle flash anims
                    elapsed_ms: 0,
                })
            })
            .collect()
    };
    state.garrison_muzzle_flashes.extend(new_flashes);

    // Phase 2: advance existing flashes and remove finished ones.
    state.garrison_muzzle_flashes.retain_mut(|flash| {
        flash.elapsed_ms += dt_ms;
        while flash.elapsed_ms >= flash.rate_ms && flash.rate_ms > 0 {
            flash.elapsed_ms -= flash.rate_ms;
            flash.frame += 1;
        }
        flash.frame < flash.total_frames
    });
}
