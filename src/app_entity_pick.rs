//! Entity picking, hover-target resolution, and selection snapshot computation.
//!
//! Handles click and box selection logic, enemy target picking for attack
//! commands, and hover-target classification (friendly/enemy/structure/unit).
//!
//! Extracted from app_render.rs to keep files under 400 lines.
//!
//! ## Dependency rules
//! - Part of the app layer — may depend on everything.

use crate::app_types::HoverTargetKind;
use crate::map::entities::EntityCategory;
use crate::rules::ruleset::RuleSet;
use crate::sim::entity_store::EntityStore;
use crate::sim::intern::InternedId;
use crate::sim::vision::FogState;

/// Elliptical distance threshold for picking objects at a screen point.
/// `dx*dx + 0.5*dy*dy < 200` with 0.5 Y-weight compensating for isometric projection.
/// Max horizontal reach: sqrt(200) ≈ 14px, max vertical reach: sqrt(400) = 20px.
const PICK_DISTANCE_THRESHOLD: f32 = 200.0;

/// Y-axis weight in the elliptical pick distance formula.
/// Makes the hit zone taller than wide, matching the isometric projection where
/// units appear elongated vertically.
const PICK_Y_WEIGHT: f32 = 0.5;

/// Compute the elliptical pick distance for object selection.
/// Returns `dx² + 0.5 * dy²` — an ellipse wider vertically than horizontally.
fn pick_distance_sq(dx: f32, dy: f32) -> f32 {
    dx * dx + PICK_Y_WEIGHT * dy * dy
}

pub(crate) fn pick_enemy_target_stable_id(
    sim: &crate::sim::world::Simulation,
    world_x: f32,
    world_y: f32,
    friendly_owner: &str,
    ignore_visibility: bool,
    rules: Option<&RuleSet>,
    height_map: &std::collections::BTreeMap<(u16, u16), u8>,
    bridge_height_map: Option<&std::collections::BTreeMap<(u16, u16), u8>>,
) -> Option<u64> {
    hover_target_at_point(
        sim,
        world_x,
        world_y,
        friendly_owner,
        ignore_visibility,
        rules,
        height_map,
        bridge_height_map,
    )
    .and_then(|hover| match hover.kind {
        HoverTargetKind::EnemyUnit | HoverTargetKind::EnemyStructure => Some(hover.stable_id),
        _ => None,
    })
}

/// Force-fire: pick any entity under cursor (including friendlies).
pub(crate) fn pick_any_target_stable_id(
    sim: &crate::sim::world::Simulation,
    world_x: f32,
    world_y: f32,
    ignore_visibility: bool,
    rules: Option<&RuleSet>,
    height_map: &std::collections::BTreeMap<(u16, u16), u8>,
    bridge_height_map: Option<&std::collections::BTreeMap<(u16, u16), u8>>,
) -> Option<u64> {
    // Use empty owner so everything is considered "enemy" in hover logic.
    hover_target_at_point(
        sim,
        world_x,
        world_y,
        "",
        ignore_visibility,
        rules,
        height_map,
        bridge_height_map,
    )
    .filter(|hover| hover.kind != HoverTargetKind::HiddenEnemy)
    .map(|hover| hover.stable_id)
}

pub(crate) fn hover_target_at_point(
    sim: &crate::sim::world::Simulation,
    world_x: f32,
    world_y: f32,
    local_owner: &str,
    ignore_visibility: bool,
    rules: Option<&RuleSet>,
    height_map: &std::collections::BTreeMap<(u16, u16), u8>,
    bridge_height_map: Option<&std::collections::BTreeMap<(u16, u16), u8>>,
) -> Option<HoverTargetKindWithId> {
    let local_owner_id: InternedId = sim.interner.get(local_owner).unwrap_or_default();
    let mut best: Option<(u64, f32)> = None;
    for entity in sim.entities.values() {
        let is_structure = entity.category == EntityCategory::Structure;
        let type_str = sim.interner.resolve(entity.type_ref);
        let owner_str = sim.interner.resolve(entity.owner);
        let (sx, sy) = if is_structure {
            (entity.position.screen_x, entity.position.screen_y)
        } else {
            crate::app_instances::interpolated_screen_position_entity(entity)
        };
        // Hit test: structures use foundation cells, mobile units use elliptical distance.
        if is_structure {
            let foundation = rules
                .and_then(|r| r.object(type_str))
                .map(|o| o.foundation.as_str())
                .unwrap_or("1x1");
            if !click_hits_foundation(
                world_x,
                world_y,
                entity.position.rx,
                entity.position.ry,
                foundation,
                height_map,
                bridge_height_map,
            ) {
                continue;
            }
        } else {
            let dx = sx - world_x;
            let dy = sy - world_y;
            if pick_distance_sq(dx, dy) >= PICK_DISTANCE_THRESHOLD {
                continue;
            }
        }
        // Distance for tie-breaking (prefer closest to anchor point).
        let dx = sx - world_x;
        let dy = sy - world_y;
        let dist_sq = dx * dx + dy * dy;
        let is_friendly = sim.fog.is_friendly(local_owner, owner_str);
        let is_visible = ignore_visibility
            || (sim
                .fog
                .is_cell_revealed(local_owner_id, entity.position.rx, entity.position.ry)
                && !sim.fog.is_cell_gap_covered(
                    local_owner_id,
                    entity.position.rx,
                    entity.position.ry,
                ));
        let kind = if is_friendly {
            if is_structure {
                HoverTargetKind::FriendlyStructure
            } else {
                HoverTargetKind::FriendlyUnit
            }
        } else if !is_visible {
            HoverTargetKind::HiddenEnemy
        } else if is_structure {
            HoverTargetKind::EnemyStructure
        } else {
            HoverTargetKind::EnemyUnit
        };
        match best {
            Some((_, best_dist_sq)) if dist_sq >= best_dist_sq => {}
            _ => best = Some((encode_hover_kind_with_id(kind, entity.stable_id), dist_sq)),
        }
    }
    best.map(|(encoded, _)| decode_hover_kind_with_id(encoded))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HoverTargetKindWithId {
    pub(crate) kind: HoverTargetKind,
    pub(crate) stable_id: u64,
}

fn encode_hover_kind_with_id(kind: HoverTargetKind, stable_id: u64) -> u64 {
    let kind_bits = match kind {
        HoverTargetKind::FriendlyUnit => 1u64,
        HoverTargetKind::FriendlyStructure => 2,
        HoverTargetKind::EnemyUnit => 3,
        HoverTargetKind::EnemyStructure => 4,
        HoverTargetKind::HiddenEnemy => 5,
    };
    (stable_id << 3) | kind_bits
}

fn decode_hover_kind_with_id(encoded: u64) -> HoverTargetKindWithId {
    let kind = match encoded & 0b111 {
        1 => HoverTargetKind::FriendlyUnit,
        2 => HoverTargetKind::FriendlyStructure,
        3 => HoverTargetKind::EnemyUnit,
        4 => HoverTargetKind::EnemyStructure,
        _ => HoverTargetKind::HiddenEnemy,
    };
    HoverTargetKindWithId {
        kind,
        stable_id: encoded >> 3,
    }
}

pub(crate) fn compute_click_selection_snapshot(
    entities: &EntityStore,
    fog: Option<&FogState>,
    local_owner: Option<&str>,
    world_x: f32,
    world_y: f32,
    click_radius: f32,
    additive: bool,
    rules: Option<&RuleSet>,
    height_map: &std::collections::BTreeMap<(u16, u16), u8>,
    bridge_height_map: Option<&std::collections::BTreeMap<(u16, u16), u8>>,
    interner: Option<&crate::sim::intern::StringInterner>,
) -> Option<Vec<u64>> {
    let current: Vec<u64> = selected_stable_ids_sorted_from_store(entities);
    let Some(picked_sid) = pick_entity_at_point(
        entities,
        fog,
        local_owner,
        world_x,
        world_y,
        click_radius,
        rules,
        height_map,
        bridge_height_map,
        interner,
    ) else {
        // Non-shift click clears selection; shift-click with no hit keeps it.
        return if additive {
            None
        } else if current.is_empty() {
            None
        } else {
            Some(Vec::new())
        };
    };
    let mut out = if additive {
        current.clone()
    } else {
        Vec::new()
    };
    if additive {
        if let Some(idx) = out.iter().position(|v| *v == picked_sid) {
            out.remove(idx);
        } else {
            out.push(picked_sid);
        }
    } else {
        out.push(picked_sid);
    }
    out.sort_unstable();
    out.dedup();
    Some(out)
}

pub(crate) fn compute_box_selection_snapshot(
    entities: &EntityStore,
    fog: Option<&FogState>,
    local_owner: Option<&str>,
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
    additive: bool,
    interner: Option<&crate::sim::intern::StringInterner>,
) -> Option<Vec<u64>> {
    let current: Vec<u64> = selected_stable_ids_sorted_from_store(entities);
    let candidates = entities_in_rect(
        entities,
        fog,
        local_owner,
        min_x,
        min_y,
        max_x,
        max_y,
        interner,
    );
    if candidates.is_empty() && additive {
        return None;
    }
    let mut out = if additive {
        current.clone()
    } else {
        Vec::new()
    };
    let mut changed = false;
    if additive {
        for sid in candidates {
            if let Some(idx) = out.iter().position(|v| *v == sid) {
                out.remove(idx);
                changed = true;
            } else {
                out.push(sid);
                changed = true;
            }
        }
        if !changed {
            return None;
        }
    } else {
        out.extend(candidates);
    }
    out.sort_unstable();
    out.dedup();
    Some(out)
}

/// Get sorted stable IDs of selected entities from EntityStore.
fn selected_stable_ids_sorted_from_store(entities: &EntityStore) -> Vec<u64> {
    let mut ids: Vec<u64> = entities
        .values()
        .filter(|e| e.selected)
        .map(|e| e.stable_id)
        .collect();
    ids.sort_unstable();
    ids
}

fn entities_in_rect(
    entities: &EntityStore,
    fog: Option<&FogState>,
    local_owner: Option<&str>,
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
    interner: Option<&crate::sim::intern::StringInterner>,
) -> Vec<u64> {
    let local_owner_id = local_owner.and_then(|o| interner.and_then(|i| i.get(o)));
    entities
        .values()
        .filter_map(|entity| {
            let owner_str = interner.map_or("", |i| i.resolve(entity.owner));
            if !is_selectable_entity(
                fog,
                local_owner,
                owner_str,
                &entity.position,
                local_owner_id,
            ) {
                return None;
            }
            // Structures excluded from band-box selection (RA2 convention).
            if entity.category == EntityCategory::Structure {
                return None;
            }
            let (sx, sy) = crate::app_instances::interpolated_screen_position_entity(entity);
            (sx >= min_x && sx <= max_x && sy >= min_y && sy <= max_y).then_some(entity.stable_id)
        })
        .collect()
}

/// Parse a foundation string like "3x2" → (3, 2). Returns (1, 1) for malformed input.
fn parse_foundation(s: &str) -> (u16, u16) {
    let mut parts = s.split('x');
    let w = parts.next().and_then(|p| p.parse().ok()).unwrap_or(1u16);
    let h = parts.next().and_then(|p| p.parse().ok()).unwrap_or(1u16);
    (w, h)
}

/// Check if a world-space click point falls on a building's foundation cells.
/// The building occupies cells `(rx..rx+fw, ry..ry+fh)`. We convert the click
/// to cell coords and check containment. This matches the original engine which
/// uses the foundation footprint, not the visual sprite bounds.
fn click_hits_foundation(
    world_x: f32,
    world_y: f32,
    entity_rx: u16,
    entity_ry: u16,
    foundation: &str,
    height_map: &std::collections::BTreeMap<(u16, u16), u8>,
    bridge_height_map: Option<&std::collections::BTreeMap<(u16, u16), u8>>,
) -> bool {
    let (fw, fh) = parse_foundation(foundation);
    let (click_rx, click_ry) =
        crate::app_sim_tick::world_point_to_cell(world_x, world_y, height_map, bridge_height_map);
    let crx = click_rx as i32;
    let cry = click_ry as i32;
    let brx = entity_rx as i32;
    let bry = entity_ry as i32;
    crx >= brx && crx < brx + fw as i32 && cry >= bry && cry < bry + fh as i32
}

fn pick_entity_at_point(
    entities: &EntityStore,
    fog: Option<&FogState>,
    local_owner: Option<&str>,
    world_x: f32,
    world_y: f32,
    click_radius: f32,
    rules: Option<&RuleSet>,
    height_map: &std::collections::BTreeMap<(u16, u16), u8>,
    bridge_height_map: Option<&std::collections::BTreeMap<(u16, u16), u8>>,
    interner: Option<&crate::sim::intern::StringInterner>,
) -> Option<u64> {
    // Elliptical pick distance: dx² + 0.5*dy² < 200, with the 0.5 Y-weight
    // compensating for isometric projection.
    // The click_radius parameter is kept for API compatibility but not used.
    let _ = click_radius;
    let local_owner_id = local_owner.and_then(|o| interner.and_then(|i| i.get(o)));
    let mut best_mobile: Option<(u64, f32)> = None;
    let mut best_structure: Option<(u64, f32)> = None;

    for entity in entities.values() {
        let owner_str = interner.map_or("", |i| i.resolve(entity.owner));
        let type_str = interner.map_or("", |i| i.resolve(entity.type_ref));
        if !is_selectable_entity(
            fog,
            local_owner,
            owner_str,
            &entity.position,
            local_owner_id,
        ) {
            continue;
        }
        let is_structure = entity.category == EntityCategory::Structure;
        if is_structure {
            // Foundation-based hit test: click must land on one of the building's
            // foundation cells.
            let foundation = rules
                .and_then(|r| r.object(type_str))
                .map(|o| o.foundation.as_str())
                .unwrap_or("1x1");
            if !click_hits_foundation(
                world_x,
                world_y,
                entity.position.rx,
                entity.position.ry,
                foundation,
                height_map,
                bridge_height_map,
            ) {
                continue;
            }
            let (sx, sy) = (entity.position.screen_x, entity.position.screen_y);
            let dx = sx - world_x;
            let dy = sy - world_y;
            let dist_sq = pick_distance_sq(dx, dy);
            match best_structure {
                Some((_, best_dist)) if dist_sq >= best_dist => {}
                _ => best_structure = Some((entity.stable_id, dist_sq)),
            }
        } else {
            let (sx, sy) = crate::app_instances::interpolated_screen_position_entity(entity);
            let dx = sx - world_x;
            let dy = sy - world_y;
            let dist_sq = pick_distance_sq(dx, dy);
            if dist_sq < PICK_DISTANCE_THRESHOLD {
                match best_mobile {
                    Some((_, best_dist)) if dist_sq >= best_dist => {}
                    _ => best_mobile = Some((entity.stable_id, dist_sq)),
                }
            }
        }
    }

    best_mobile.or(best_structure).map(|(sid, _)| sid)
}

/// Visibility check for selection — replicates selection::is_selectable_for_player
/// using plain string fields instead of ECS components.
fn is_selectable_entity(
    fog: Option<&FogState>,
    local_owner: Option<&str>,
    entity_owner: &str,
    pos: &crate::sim::components::Position,
    local_owner_id: Option<InternedId>,
) -> bool {
    let Some(local_owner) = local_owner else {
        return true;
    };
    let Some(fog) = fog else {
        return entity_owner.eq_ignore_ascii_case(local_owner);
    };
    if fog.is_friendly(local_owner, entity_owner) {
        return true;
    }
    let owner_id = local_owner_id.unwrap_or_default();
    fog.is_cell_revealed(owner_id, pos.rx, pos.ry)
        && !fog.is_cell_gap_covered(owner_id, pos.rx, pos.ry)
}
