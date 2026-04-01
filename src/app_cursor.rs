//! Cursor feedback analysis and software cursor frame selection.
//!
//! Determines what cursor state to show based on hover target, selection,
//! and game mode. Extracted from app_ui_overlays.rs for file-size limits.

use std::time::Instant;

use crate::app::AppState;
use crate::app_commands::preferred_local_owner_name;
use crate::app_instances::CellVisibilityState;
use crate::app_types::{
    CursorFeedbackKind, CursorId, HoverTargetKind, ScrollDir, SoftwareCursorFrame,
    SoftwareCursorSequence,
};
use crate::sim::combat;

pub(crate) fn current_cursor_feedback_kind(state: &AppState) -> Option<CursorFeedbackKind> {
    if state.middle_mouse_panning {
        return Some(CursorFeedbackKind::Pan);
    }
    if state.minimap_dragging || is_cursor_over_minimap(state) {
        // Show the minimap-specific Move cursor when hovering over the minimap
        // (reference §7.4 — MiniFrame/MiniCount for the Move cursor = frames 42–51).
        return Some(CursorFeedbackKind::MinimapMove);
    }
    // Edge-scroll arrows override everything else (except minimap above).
    if let Some(dir) = edge_scroll_direction(state) {
        return Some(CursorFeedbackKind::Scroll(dir));
    }
    if current_sidebar_view_hit(state) {
        return None;
    }
    if let Some(preview) = state.building_placement_preview.as_ref() {
        return Some(if preview.valid {
            CursorFeedbackKind::PlaceValid
        } else {
            CursorFeedbackKind::PlaceInvalid
        });
    }
    if state.armed_building_placement.is_some() {
        return Some(CursorFeedbackKind::Invalid);
    }
    let Some(sim) = &state.simulation else {
        return None;
    };
    let selected = crate::app_input::selected_stable_ids_sorted(&sim.entities);
    if selected.is_empty() {
        return None;
    }
    let owner = preferred_local_owner_name(state).unwrap_or_else(|| "Americans".to_string());
    let (world_x, world_y) =
        crate::app_sim_tick::screen_point_to_world(state, state.cursor_x, state.cursor_y);
    let (hover_rx, hover_ry) =
        crate::app_sim_tick::screen_point_to_world_cell(state, state.cursor_x, state.cursor_y);
    let owner_id = sim.interner.get(&owner);
    if crate::app_instances::cell_visibility_for_local_owner(
        owner_id,
        Some(&sim.fog),
        hover_rx,
        hover_ry,
        state.sandbox_full_visibility,
    ) != CellVisibilityState::Visible
    {
        // Over shrouded/fogged cells the player can still issue move orders,
        // so show the queued-order-mode cursor (Move / AttackMove / Guard)
        // instead of reverting to the default arrow.
        return Some(match state.queued_order_mode {
            crate::app_render::OrderMode::Move => CursorFeedbackKind::Move,
            crate::app_render::OrderMode::AttackMove => CursorFeedbackKind::AttackMove,
            crate::app_render::OrderMode::Guard => CursorFeedbackKind::Guard,
        });
    }
    if let Some(hover) = crate::app_entity_pick::hover_target_at_point(
        sim,
        world_x,
        world_y,
        &owner,
        state.sandbox_full_visibility,
        state.rules.as_ref(),
        &state.height_map,
        Some(&state.bridge_height_map),
    ) {
        let kind = capability_cursor_for_hover(sim, &selected, &hover, state.rules.as_ref());
        return Some(kind);
    }
    // Check for ore/gem under cursor — show attack cursor when miners are selected.
    let has_ore = sim
        .production
        .resource_nodes
        .get(&(hover_rx, hover_ry))
        .is_some_and(|n| n.remaining > 0);
    if has_ore {
        let any_miner = selected
            .iter()
            .any(|&sid| sim.entities.get(sid).is_some_and(|e| e.miner.is_some()));
        if any_miner {
            return Some(CursorFeedbackKind::AttackMove);
        }
    }
    Some(match state.queued_order_mode {
        crate::app_render::OrderMode::Move => CursorFeedbackKind::Move,
        crate::app_render::OrderMode::AttackMove => CursorFeedbackKind::AttackMove,
        crate::app_render::OrderMode::Guard => CursorFeedbackKind::Guard,
    })
}

/// Determine the cursor feedback kind for a hover target, checking ObjectType
/// capability flags from rules.ini before falling back to the generic attack/select logic.
///
/// The original engine picks a single "best" selected unit via
/// `SelectBestObjectForAction` (priority: armed mobile > unarmed mobile >
/// immobile; ties broken by distance to target) and uses that unit's
/// `What_Action_OnObject` to determine the cursor for the entire group.
///
/// Priority (highest first):
/// 1. Deployer self-hover: selected unit IS the hovered entity and has Deployer=yes.
/// 2. SabotageCursor: selected unit has SabotageCursor=yes hovering an enemy structure.
/// 3. Engineer capturing: selected Engineer hovering capturable enemy building.
/// 4. Engineer repairing: selected Engineer hovering damaged friendly building.
/// 5. Infantry boarding: selected infantry hovering friendly transport (Passengers>0).
/// 6. Infantry garrisoning: selected Occupier infantry hovering friendly CanBeOccupied building.
/// 7. AttackCursorOnFriendlies: selected unit attacks friendlies, treat as attack target.
/// 8. Generic friendly/enemy/in-range/out-of-range fallback.
fn capability_cursor_for_hover(
    sim: &crate::sim::world::Simulation,
    selected: &[u64],
    hover: &crate::app_entity_pick::HoverTargetKindWithId,
    rules: Option<&crate::rules::ruleset::RuleSet>,
) -> CursorFeedbackKind {
    use crate::map::entities::EntityCategory;

    let hovered_entity = sim.entities.get(hover.stable_id);
    let hovered_obj =
        rules.and_then(|r| hovered_entity.and_then(|e| r.object(sim.interner.resolve(e.type_ref))));

    // 1. Deployer self-hover — the cursor is over the selected unit itself.
    //    Show the deploy cursor for units with Deployer=yes (e.g. GGI, Guardian GI)
    //    OR units with DeploysInto= set (e.g. MCV → ConYard).  In the original game
    //    both kinds show the deploy cursor when hovering over themselves.
    if selected.len() == 1 && selected[0] == hover.stable_id {
        let entity = sim.entities.get(selected[0]);
        let obj =
            entity.and_then(|e| rules.and_then(|r| r.object(sim.interner.resolve(e.type_ref))));
        if let Some(obj) = obj {
            if obj.deployer || obj.deploys_into.is_some() {
                return CursorFeedbackKind::Deploy;
            }
        }
        // 1b. Garrisoned building self-hover — show deploy cursor to unload occupants.
        if let Some(entity) = entity {
            if entity.category == EntityCategory::Structure {
                if let Some(obj) = obj {
                    if obj.can_be_occupied {
                        let has_occupants =
                            entity.passenger_role.cargo().is_some_and(|c| !c.is_empty());
                        if has_occupants {
                            return CursorFeedbackKind::Deploy;
                        }
                    }
                }
            }
        }
    }

    // Pick the "best" selected unit for capability cursor checks.
    // Matches the original engine's SelectBestObjectForAction priority system.
    let hover_pos = hovered_entity.map(|e| (e.position.rx, e.position.ry));
    let best_id = select_best_for_action(sim, selected, hover_pos, rules);

    if let Some(best_id) = best_id {
        if let (Some(sel_entity), Some(sel_obj)) = (
            sim.entities.get(best_id),
            sim.entities
                .get(best_id)
                .and_then(|e| rules.and_then(|r| r.object(sim.interner.resolve(e.type_ref)))),
        ) {
            // 2. SabotageCursor: Tanya/Navy SEAL hovering an enemy structure.
            if sel_obj.sabotage_cursor {
                if matches!(hover.kind, HoverTargetKind::EnemyStructure) {
                    return CursorFeedbackKind::Enter;
                }
            }

            let is_infantry = sel_entity.category == EntityCategory::Infantry;

            if sel_obj.engineer {
                // 3. Engineer on capturable enemy building → capture (Enter cursor).
                if matches!(hover.kind, HoverTargetKind::EnemyStructure) {
                    if hovered_obj.map_or(false, |o| o.capturable) {
                        return CursorFeedbackKind::Enter;
                    }
                }
                // 4. Engineer on damaged friendly building → repair.
                if matches!(hover.kind, HoverTargetKind::FriendlyStructure) {
                    if let Some(he) = hovered_entity {
                        if he.health.current < he.health.max {
                            return CursorFeedbackKind::EngineerRepair;
                        }
                    }
                }
            }

            // 5. Infantry boarding a friendly transport (Passengers > 0).
            if is_infantry && matches!(hover.kind, HoverTargetKind::FriendlyUnit) {
                if hovered_obj.map_or(false, |o| o.passengers > 0) {
                    return CursorFeedbackKind::Enter;
                }
            }

            // 6. Infantry garrisoning a CanBeOccupied building (friendly or neutral/civilian).
            //    Original engine checks Occupier=yes via BuildingClass::CanDock.
            //    Neutral/civilian buildings are classified as EnemyStructure but still
            //    garrisonable — only show Enter for those, not actual enemy-player buildings.
            if is_infantry && sel_obj.occupier && hovered_obj.map_or(false, |o| o.can_be_occupied) {
                let is_garrisonable_target = match hover.kind {
                    HoverTargetKind::FriendlyStructure => true,
                    HoverTargetKind::EnemyStructure => {
                        // Only neutral/civilian buildings — not real enemy player buildings.
                        hovered_entity.map_or(false, |e| {
                            let ow = sim.interner.resolve(e.owner);
                            ow.eq_ignore_ascii_case("neutral") || ow.eq_ignore_ascii_case("special")
                        })
                    }
                    _ => false,
                };
                if is_garrisonable_target {
                    return CursorFeedbackKind::Enter;
                }
            }

            // 7. AttackCursorOnFriendlies — treat friendly targets as attack targets.
            if sel_obj.attack_cursor_on_friendlies {
                if matches!(
                    hover.kind,
                    HoverTargetKind::FriendlyUnit | HoverTargetKind::FriendlyStructure
                ) {
                    let in_range =
                        any_selected_unit_in_range(sim, selected, hover.stable_id, rules);
                    return if in_range {
                        if hover.kind == HoverTargetKind::FriendlyUnit {
                            CursorFeedbackKind::EnemyUnit
                        } else {
                            CursorFeedbackKind::EnemyStructure
                        }
                    } else {
                        CursorFeedbackKind::EnemyOutOfRange
                    };
                }
            }
        }
    }

    // 8. Generic fallback.
    match hover.kind {
        HoverTargetKind::FriendlyUnit => CursorFeedbackKind::FriendlyUnit,
        HoverTargetKind::FriendlyStructure => CursorFeedbackKind::FriendlyStructure,
        HoverTargetKind::EnemyUnit | HoverTargetKind::EnemyStructure => {
            let in_range = any_selected_unit_in_range(sim, selected, hover.stable_id, rules);
            if in_range {
                if hover.kind == HoverTargetKind::EnemyUnit {
                    CursorFeedbackKind::EnemyUnit
                } else {
                    CursorFeedbackKind::EnemyStructure
                }
            } else {
                CursorFeedbackKind::EnemyOutOfRange
            }
        }
        HoverTargetKind::HiddenEnemy => CursorFeedbackKind::Invalid,
    }
}

/// Check if any selected unit has a weapon that can reach the target entity.
fn any_selected_unit_in_range(
    sim: &crate::sim::world::Simulation,
    selected_ids: &[u64],
    target_id: u64,
    rules: Option<&crate::rules::ruleset::RuleSet>,
) -> bool {
    let rules = match rules {
        Some(r) => r,
        None => return true,
    };
    let target_pos = match sim.entities.get(target_id) {
        Some(t) => (t.position.rx, t.position.ry),
        None => return false,
    };
    for &sid in selected_ids {
        let Some(entity) = sim.entities.get(sid) else {
            continue;
        };
        let Some(obj) = rules.object(sim.interner.resolve(entity.type_ref)) else {
            continue;
        };
        let weapon_range = obj
            .primary
            .as_ref()
            .and_then(|w| rules.weapon(w))
            .map(|w| w.range)
            .unwrap_or(crate::util::fixed_math::SIM_ZERO);
        if weapon_range <= crate::util::fixed_math::SIM_ZERO {
            continue;
        }
        let dist_sq = combat::cell_distance_sq(
            entity.position.rx,
            entity.position.ry,
            target_pos.0,
            target_pos.1,
        );
        if combat::is_within_weapon_range_sq(dist_sq, weapon_range) {
            return true;
        }
    }
    false
}

/// Pick the single "best" selected object for determining the action cursor.
///
/// Matches the original engine's `SelectBestObjectForAction` (0x005353d0):
///   Priority 5 — mobile, not building, has weapons (WeaponRange > 0)
///   Priority 4 — mobile, not building
///   Priority 3 — can move (any mobile entity)
///   Priority 2 — exists on map
///   Priority 1 — deploying
///   Priority 0 — warping/teleporting
/// Ties within the same priority broken by closest distance to the hover target.
fn select_best_for_action(
    sim: &crate::sim::world::Simulation,
    selected: &[u64],
    hover_pos: Option<(u16, u16)>,
    rules: Option<&crate::rules::ruleset::RuleSet>,
) -> Option<u64> {
    use crate::map::entities::EntityCategory;

    let mut best_id: Option<u64> = None;
    let mut best_priority: i32 = -1;
    let mut best_dist: u32 = u32::MAX;

    for &sid in selected {
        let Some(entity) = sim.entities.get(sid) else {
            continue;
        };
        // Compute priority tier.
        let priority = if entity.category == EntityCategory::Structure {
            // Buildings: can't move, priority 2 (exists on map).
            2
        } else {
            // Mobile unit: at least priority 3.
            let obj = rules.and_then(|r| r.object(sim.interner.resolve(entity.type_ref)));
            let has_weapon = obj
                .and_then(|o| o.primary.as_ref())
                .and_then(|w| rules.and_then(|r| r.weapon(w)))
                .is_some_and(|w| w.range > crate::util::fixed_math::SIM_ZERO);
            if has_weapon { 5 } else { 4 }
        };

        // Distance to hover target (squared, in cells).
        let dist = hover_pos.map_or(0u32, |(hx, hy)| {
            let dx = (entity.position.rx as i32 - hx as i32).unsigned_abs();
            let dy = (entity.position.ry as i32 - hy as i32).unsigned_abs();
            dx * dx + dy * dy
        });

        if priority > best_priority || (priority == best_priority && dist < best_dist) {
            best_priority = priority;
            best_dist = dist;
            best_id = Some(sid);
        }
    }
    best_id
}

/// Map a game-state cursor intent to the visual CursorId to display.
/// Returns None for feedback kinds that use procedural visuals instead of a software cursor
/// (e.g. building placement preview).
/// Alias mappings live here: Pan→Move, Guard→Select, FriendlyUnit→Select, etc.
pub(crate) fn cursor_id_for_feedback(kind: CursorFeedbackKind) -> Option<CursorId> {
    match kind {
        CursorFeedbackKind::FriendlyUnit
        | CursorFeedbackKind::FriendlyStructure
        | CursorFeedbackKind::Guard => Some(CursorId::Select),
        CursorFeedbackKind::Move => Some(CursorId::Move),
        CursorFeedbackKind::Pan => Some(CursorId::Pan),
        CursorFeedbackKind::AttackMove => Some(CursorId::AttackMove),
        CursorFeedbackKind::EnemyUnit | CursorFeedbackKind::EnemyStructure => {
            Some(CursorId::Attack)
        }
        CursorFeedbackKind::EnemyOutOfRange => Some(CursorId::AttackOutOfRange),
        CursorFeedbackKind::Invalid => Some(CursorId::NoMove),
        CursorFeedbackKind::PlaceValid | CursorFeedbackKind::PlaceInvalid => None,
        CursorFeedbackKind::Scroll(dir) => Some(scroll_dir_to_cursor_id(dir)),
        CursorFeedbackKind::MinimapMove => Some(CursorId::MinimapMove),
        CursorFeedbackKind::Enter => Some(CursorId::Enter),
        CursorFeedbackKind::EngineerRepair => Some(CursorId::EngineerRepair),
        CursorFeedbackKind::Deploy => Some(CursorId::Deploy),
    }
}

fn scroll_dir_to_cursor_id(dir: ScrollDir) -> CursorId {
    match dir {
        ScrollDir::N => CursorId::ScrollN,
        ScrollDir::NE => CursorId::ScrollNE,
        ScrollDir::E => CursorId::ScrollE,
        ScrollDir::SE => CursorId::ScrollSE,
        ScrollDir::S => CursorId::ScrollS,
        ScrollDir::SW => CursorId::ScrollSW,
        ScrollDir::W => CursorId::ScrollW,
        ScrollDir::NW => CursorId::ScrollNW,
    }
}

pub(crate) fn current_software_cursor_frame(
    sequence: &SoftwareCursorSequence,
) -> Option<&SoftwareCursorFrame> {
    if sequence.frames.is_empty() {
        return None;
    }
    if sequence.frames.len() == 1 || sequence.interval_ms == 0 {
        return sequence.frames.first();
    }
    let elapsed_ms: u64 = cursor_animation_start()
        .elapsed()
        .as_millis()
        .try_into()
        .ok()?;
    let frame_idx = ((elapsed_ms / sequence.interval_ms) % sequence.frames.len() as u64) as usize;
    sequence.frames.get(frame_idx)
}

fn cursor_animation_start() -> &'static Instant {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now)
}

fn is_cursor_over_minimap(state: &AppState) -> bool {
    // Minimap interaction disabled when radar is not online.
    let minimap_visible: bool = state
        .radar_anim
        .as_ref()
        .map_or(true, |ra| ra.is_minimap_visible());
    if !minimap_visible {
        return false;
    }
    let Some(_minimap) = &state.minimap else {
        return false;
    };
    let rect = crate::app_sidebar_render::active_minimap_screen_rect(state);
    state
        .minimap
        .as_ref()
        .unwrap()
        .contains_screen_point_in_rect(
            state.cursor_x,
            state.cursor_y,
            rect.x,
            rect.y,
            rect.w,
            rect.h,
        )
}

/// Screen margin (pixels from window edge) that triggers edge-scroll cursors.
/// Must match EDGE_SCROLL_MARGIN in app_sim_tick.rs.
const EDGE_SCROLL_MARGIN: f32 = 10.0;

/// Return the edge-scroll direction (if any) based on cursor proximity to window edges.
/// Diagonal corners are detected by combining horizontal and vertical proximity.
fn edge_scroll_direction(state: &AppState) -> Option<ScrollDir> {
    let sw = state.render_width() as f32;
    let sh = state.render_height() as f32;
    let sidebar_x = sw - state.sidebar_layout_spec.sidebar_width;
    let x = state.cursor_x;
    let y = state.cursor_y;
    let near_left = x < EDGE_SCROLL_MARGIN;
    let near_right = x < sidebar_x && x > sidebar_x - EDGE_SCROLL_MARGIN;
    let near_top = y < EDGE_SCROLL_MARGIN;
    let near_bottom = y > sh - EDGE_SCROLL_MARGIN;
    match (near_left, near_right, near_top, near_bottom) {
        (true, _, true, _) => Some(ScrollDir::NW),
        (_, true, true, _) => Some(ScrollDir::NE),
        (true, _, _, true) => Some(ScrollDir::SW),
        (_, true, _, true) => Some(ScrollDir::SE),
        (_, _, true, _) => Some(ScrollDir::N),
        (_, _, _, true) => Some(ScrollDir::S),
        (true, _, _, _) => Some(ScrollDir::W),
        (_, true, _, _) => Some(ScrollDir::E),
        _ => None,
    }
}

fn current_sidebar_view_hit(state: &AppState) -> bool {
    let sw = state.sidebar_layout_spec.sidebar_width;
    let panel_rect = crate::sidebar::Rect {
        x: state.render_width() as f32 - sw - 10.0,
        y: 10.0,
        w: sw,
        h: state.render_height() as f32 - 20.0,
    };
    panel_rect.contains(state.cursor_x, state.cursor_y)
}
