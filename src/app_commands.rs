//! Build and production commands — queuing builds, placing buildings, owner management.
//!
//! Extracted from app_render.rs. Part of the app layer — may depend on everything.

use std::collections::HashMap;

use crate::app::AppState;
use crate::map::entities::EntityCategory;
use crate::sim::command::{Command, CommandEnvelope, QueueMode};
use crate::sim::intern::InternedId;
use crate::sim::production;

/// Default owner name when no playable house is found.
const DEFAULT_OWNER: &str = "Americans";

/// Intern an owner name string, returning its InternedId.
/// Requires a mutable simulation (for intern). Returns default if no sim.
fn intern_owner(state: &mut AppState, owner: &str) -> InternedId {
    state
        .simulation
        .as_mut()
        .map(|s| s.interner.intern(owner))
        .unwrap_or_default()
}

/// Intern a type_id string, returning its InternedId.
fn intern_type(state: &mut AppState, type_id: &str) -> InternedId {
    state
        .simulation
        .as_mut()
        .map(|s| s.interner.intern(type_id))
        .unwrap_or_default()
}

/// Resolve the local owner, set the override, and return the owned String.
/// Centralizes the common pattern of getting the owner + updating the override,
/// reducing one clone per call site compared to doing it inline.
fn resolve_owner(state: &mut AppState) -> String {
    let owner: String = preferred_local_owner(state).unwrap_or_else(|| DEFAULT_OWNER.to_string());
    state.local_owner_override = Some(owner.clone());
    owner
}

pub(crate) fn queue_default_build(state: &mut AppState) {
    let owner: String = resolve_owner(state);
    let (Some(sim), Some(rules)) = (&state.simulation, &state.rules) else {
        return;
    };
    let Some(default_type) = production::build_options_for_owner(sim, rules, &owner)
        .into_iter()
        .find(|o| {
            o.enabled
                && matches!(
                    o.queue_category,
                    production::ProductionCategory::Infantry
                        | production::ProductionCategory::Vehicle
                        | production::ProductionCategory::Aircraft
                )
        })
        .map(|o| o.type_id)
    else {
        log::warn!("No default buildable unit available for owner={}", owner);
        return;
    };
    let owner_id = intern_owner(state, &owner);
    schedule_command(
        state,
        &owner,
        Command::QueueProduction {
            owner: owner_id,
            type_id: default_type,
            mode: QueueMode::Append,
        },
    );
    let type_name = state.simulation.as_ref().map_or("?".to_string(), |s| {
        s.interner.resolve(default_type).to_string()
    });
    log::info!(
        "Build command queued: owner={} type={} execute_tick>=current+{}",
        owner,
        type_name,
        state.configured_input_delay_ticks
    );
}

pub(crate) fn queue_build_by_type(state: &mut AppState, type_id: &str) {
    let owner: String = resolve_owner(state);
    let owner_id = intern_owner(state, &owner);
    let type_interned = intern_type(state, type_id);
    schedule_command(
        state,
        &owner,
        Command::QueueProduction {
            owner: owner_id,
            type_id: type_interned,
            mode: QueueMode::Append,
        },
    );
    log::info!(
        "Build command queued: owner={} type={} execute_tick>=current+{}",
        owner,
        type_id,
        state.configured_input_delay_ticks
    );
}

pub(crate) fn toggle_pause_build_queue(
    state: &mut AppState,
    category: production::ProductionCategory,
) {
    let owner: String = resolve_owner(state);
    let owner_id = intern_owner(state, &owner);
    schedule_command(
        state,
        &owner,
        Command::TogglePauseProduction {
            owner: owner_id,
            category,
        },
    );
    log::info!(
        "Build pause/resume command queued: owner={} category={} execute_tick>=current+{}",
        owner,
        category.label(),
        state.configured_input_delay_ticks
    );
}

pub(crate) fn cycle_active_producer(
    state: &mut AppState,
    category: production::ProductionCategory,
) {
    let owner: String = resolve_owner(state);
    let owner_id = intern_owner(state, &owner);
    schedule_command(
        state,
        &owner,
        Command::CycleProducerFocus {
            owner: owner_id,
            category,
        },
    );
    log::info!(
        "Producer focus cycle queued: owner={} category={} execute_tick>=current+{}",
        owner,
        category.label(),
        state.configured_input_delay_ticks
    );
}

pub(crate) fn cancel_last_build(state: &mut AppState) {
    let owner: String = resolve_owner(state);
    let owner_id = intern_owner(state, &owner);
    schedule_command(
        state,
        &owner,
        Command::CancelLastProduction { owner: owner_id },
    );
    log::info!(
        "Build cancel command queued: owner={} execute_tick>=current+{}",
        owner,
        state.configured_input_delay_ticks
    );
}

pub(crate) fn cancel_build_by_type(state: &mut AppState, type_id: &str) {
    let owner: String = resolve_owner(state);
    let owner_id = intern_owner(state, &owner);
    let type_interned = intern_type(state, type_id);
    schedule_command(
        state,
        &owner,
        Command::CancelProductionByType {
            owner: owner_id,
            type_id: type_interned,
        },
    );
    log::info!(
        "Build cancel-by-type queued: owner={} type={} execute_tick>=current+{}",
        owner,
        type_id,
        state.configured_input_delay_ticks
    );
}

/// Sell all selected buildings owned by the local player.
pub(crate) fn sell_selected_buildings(state: &mut AppState) {
    let owner: String = preferred_local_owner(state).unwrap_or_else(|| DEFAULT_OWNER.to_string());
    let Some(sim) = &state.simulation else {
        return;
    };
    let to_sell: Vec<u64> = sim
        .entities
        .values()
        .filter(|e| {
            e.selected
                && e.category == EntityCategory::Structure
                && sim.interner.resolve(e.owner).eq_ignore_ascii_case(&owner)
        })
        .map(|e| e.stable_id)
        .collect();
    let count = to_sell.len();
    for entity_id in to_sell {
        schedule_command(state, &owner, Command::SellBuilding { entity_id });
    }
    if count > 0 {
        log::info!("Sell command queued for {} buildings", count);
    }
}

pub(crate) fn place_ready_building_at_cursor(state: &mut AppState, type_id: &str) {
    let owner: String = resolve_owner(state);
    // Use the preview's stored (rx, ry) so the placed building exactly matches
    // the ghost the player saw, avoiding any cursor-movement drift between frames.
    let (rx, ry) = if let Some(preview) = state.building_placement_preview.as_ref() {
        log::info!(
            "Click placement: using preview ({},{}) size={}x{} type={}",
            preview.rx,
            preview.ry,
            preview.width,
            preview.height,
            preview.type_id,
        );
        (preview.rx, preview.ry)
    } else {
        crate::app_sim_tick::screen_point_to_world_cell(state, state.cursor_x, state.cursor_y)
    };
    if let Some(preview) = state.building_placement_preview.as_ref() {
        if !preview.valid {
            if let Some(reason) = &preview.reason {
                log::warn!(
                    "Ready building placement rejected locally: owner={} type={} cell=({}, {}) reason={}",
                    owner,
                    type_id,
                    rx,
                    ry,
                    reason.label()
                );
            }
            return;
        }
    }
    let owner_id = intern_owner(state, &owner);
    let type_interned = intern_type(state, type_id);
    schedule_command(
        state,
        &owner,
        Command::PlaceReadyBuilding {
            owner: owner_id,
            type_id: type_interned,
            rx,
            ry,
        },
    );
    // Clear placement mode immediately so the foundation preview stops following
    // the cursor. Without this, the preview keeps moving during the input_delay_ticks
    // gap before the sim processes the command, making the placed building appear
    // offset from where the user last saw the preview.
    state.armed_building_placement = None;
    state.building_placement_preview = None;
    log::info!(
        "Ready building placement queued: owner={} type={} cell=({}, {}) execute_tick>=current+{}",
        owner,
        type_id,
        rx,
        ry,
        state.configured_input_delay_ticks
    );

    // Wall fill: if the placed type is a wall, flood-fill free overlay segments
    // toward the nearest same-type wall in each cardinal direction.
    let is_wall = state
        .rules
        .as_ref()
        .and_then(|r| r.object(type_id))
        .map(|o| o.wall)
        .unwrap_or(false);
    if is_wall {
        fill_wall_between_endpoints(state, type_id, rx, ry);
    }
}

/// Fill wall overlay segments between the newly placed cell and the nearest existing
/// same-type wall in each of the 4 cardinal directions, for free.
///
/// RA2 behavior: placing a wall between two existing wall endpoints auto-fills all
/// intermediate cells at no cost. Only overlay entries are injected — no entities,
/// no queue consumption — then connectivity is recomputed across the whole line.
fn fill_wall_between_endpoints(state: &mut AppState, type_id: &str, rx: u16, ry: u16) {
    let overlay_id = match state
        .overlay_registry
        .as_ref()
        .and_then(|r| r.id_for_name(type_id))
    {
        Some(id) => id,
        None => return,
    };

    // Directions: (drx, dry) — one axis moves, the other stays.
    // Check each direction for an existing wall of the same overlay_id.
    // If found, inject free overlay entries for all cells between click and that wall.
    let directions: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)];
    let mut new_cells: Vec<(u16, u16)> = Vec::new();

    for (drx, dry) in directions {
        // Walk outward until we hit an existing wall, a building, or the map edge.
        let mut cx = rx as i32 + drx;
        let mut cy = ry as i32 + dry;
        let mut line: Vec<(u16, u16)> = Vec::new();
        loop {
            if cx < 0 || cy < 0 || cx > 511 || cy > 511 {
                break;
            }
            let cell = (cx as u16, cy as u16);
            // Stop if a non-wall building occupies this cell (can't build through it).
            if let (Some(sim), Some(rules)) = (&state.simulation, &state.rules) {
                if crate::sim::production::structure_occupies_cell(
                    &sim.entities,
                    rules,
                    cell.0,
                    cell.1,
                    &sim.interner,
                ) {
                    break;
                }
            }
            let has_wall = state
                .overlays
                .iter()
                .any(|e| e.rx == cell.0 && e.ry == cell.1 && e.overlay_id == overlay_id);
            if has_wall {
                // Found an existing wall — fill everything in `line` between here and click.
                new_cells.extend_from_slice(&line);
                break;
            }
            line.push(cell);
            cx += drx;
            cy += dry;
        }
    }

    if new_cells.is_empty() {
        return;
    }

    // Inject free overlay entries for all fill cells.
    for (fx, fy) in &new_cells {
        let already = state
            .overlays
            .iter()
            .any(|e| e.rx == *fx && e.ry == *fy && e.overlay_id == overlay_id);
        if !already {
            state.overlays.push(crate::map::overlay::OverlayEntry {
                rx: *fx,
                ry: *fy,
                overlay_id,
                frame: 0,
            });
        }
    }
    log::info!(
        "Wall fill: {} free cells between ({},{}) and existing walls",
        new_cells.len(),
        rx,
        ry
    );

    // Recompute connectivity for the whole updated overlay list.
    if let Some(registry) = &state.overlay_registry {
        crate::map::overlay::compute_wall_connectivity(&mut state.overlays, registry);
    }
}

pub(crate) fn place_starter_base_for_local_owner(state: &mut AppState) {
    let owner: String = resolve_owner(state);
    let (Some(sim), Some(rules)) = (&state.simulation, &state.rules) else {
        return;
    };
    let opening = [
        pick_building_for_owner(rules, &owner, &["GAPOWR", "NAPOWR", "YAPOWR"]),
        pick_building_for_owner(
            rules,
            &owner,
            &["GAPILE", "NAHAND", "YABRCK", "NABRCK", "GABARR"],
        ),
        pick_building_for_owner(rules, &owner, &["GAREFN", "NAREFN", "YAREFN", "GAOREP"]),
    ];
    let build_options = production::build_options_for_owner(sim, rules, &owner);
    let queueable: Vec<String> = opening
        .into_iter()
        .flatten()
        .filter(|type_id| {
            build_options.iter().any(|opt| {
                let opt_str = sim.interner.resolve(opt.type_id);
                opt_str.eq_ignore_ascii_case(type_id) && opt.enabled
            })
        })
        .collect();
    let mut queued = 0u32;
    for type_id in queueable {
        let owner_id = intern_owner(state, &owner);
        let type_interned = intern_type(state, &type_id);
        schedule_command(
            state,
            &owner,
            Command::QueueProduction {
                owner: owner_id,
                type_id: type_interned,
                mode: QueueMode::Append,
            },
        );
        queued += 1;
    }
    if queued > 0 {
        log::info!(
            "Starter opening queued: owner={} count={} execute_tick>=current+{}",
            owner,
            queued,
            state.configured_input_delay_ticks
        );
    } else {
        log::warn!(
            "Starter opening queue failed: owner={} (no compatible first-build sequence available)",
            owner
        );
    }
}

pub(crate) fn spawn_test_units_for_local_owner(state: &mut AppState) {
    let owner: String = resolve_owner(state);
    let sw: f32 = state.render_width() as f32;
    let sh: f32 = state.render_height() as f32;
    let (mut base_rx, mut base_ry) =
        crate::app_sim_tick::screen_point_to_world_cell(state, sw * 0.5, sh * 0.5);
    let (Some(sim), Some(rules)) = (&mut state.simulation, &state.rules) else {
        return;
    };
    if let Some(grid) = &state.path_grid {
        (base_rx, base_ry) = crate::app_sim_tick::clamp_cell_to_grid(grid, (base_rx, base_ry));
    }

    let mut debug_types: Vec<String> = {
        let options = production::build_options_for_owner(sim, rules, &owner);
        let mut selected: Vec<String> = options
            .iter()
            .filter(|o| {
                o.enabled
                    && o.object_category == crate::rules::object_type::ObjectCategory::Infantry
            })
            .take(3)
            .map(|o| sim.interner.resolve(o.type_id).to_string())
            .collect();
        if selected.len() < 3 {
            let vehicles = options
                .iter()
                .filter(|o| {
                    o.enabled
                        && o.object_category == crate::rules::object_type::ObjectCategory::Vehicle
                })
                .take(3 - selected.len())
                .map(|o| sim.interner.resolve(o.type_id).to_string());
            selected.extend(vehicles);
        }
        selected
    };
    if debug_types.is_empty() {
        debug_types = vec!["HTNK".to_string(), "MTNK".to_string(), "E1".to_string()];
    }

    let mut spawned: u32 = 0;
    let mut first_spawn: Option<(u16, u16)> = None;
    for (i, type_id) in debug_types.iter().enumerate() {
        let mut desired = (
            base_rx.saturating_add(2 + i as u16 * 2),
            base_ry.saturating_add(2),
        );
        if let Some(grid) = &state.path_grid {
            desired = crate::app_sim_tick::clamp_cell_to_grid(grid, desired);
        }
        let spawn_cell = state
            .path_grid
            .as_ref()
            .and_then(|g| crate::app_sim_tick::nearest_walkable_cell(g, desired, 16))
            .unwrap_or(desired);
        if sim
            .spawn_object(
                type_id,
                &owner,
                spawn_cell.0,
                spawn_cell.1,
                64,
                rules,
                &state.height_map,
            )
            .is_some()
        {
            if first_spawn.is_none() {
                first_spawn = Some(spawn_cell);
            }
            let name = rules
                .object(type_id)
                .and_then(|o| o.name.clone())
                .unwrap_or_else(|| type_id.clone());
            log::info!("Spawn test unit: {} ({})", name, type_id);
            spawned += 1;
        }
    }
    if spawned > 0 {
        crate::app_sim_tick::refresh_entity_atlases(state);
        if let Some((rx, ry)) = first_spawn {
            crate::app_camera::center_camera_on_cell(state, rx, ry);
        }
    }
    log::info!(
        "Spawn test units: owner={} spawned={} at ({},{}) types={:?}",
        owner,
        spawned,
        base_rx,
        base_ry,
        debug_types
    );
}

pub(crate) fn cycle_local_owner(state: &mut AppState) {
    let mut owners = collect_playable_owners(state);
    if owners.is_empty() {
        return;
    }
    let current = preferred_local_owner(state);
    let next_idx = current
        .as_ref()
        .and_then(|c| owners.iter().position(|o| o.eq_ignore_ascii_case(c)))
        .map(|idx| (idx + 1) % owners.len())
        .unwrap_or(0);
    // Move out of Vec instead of cloning, then clone once for the override.
    let next = owners.swap_remove(next_idx);
    state.local_owner_override = Some(next.clone());
    state.armed_building_placement = None;
    log::info!("Local owner switched to {}", next);
}

pub(crate) fn preferred_local_owner_name(state: &AppState) -> Option<String> {
    preferred_local_owner(state)
}

pub(crate) fn preferred_local_owner(state: &AppState) -> Option<String> {
    let sim = state.simulation.as_ref()?;
    // Prefer owner of selected unit first.
    for entity in sim.entities.values() {
        let owner_str = sim.interner.resolve(entity.owner);
        if entity.selected && is_playable_house_name(owner_str) {
            return Some(owner_str.to_string());
        }
    }

    // Then explicit local override set by debug actions.
    if let Some(owner) = &state.local_owner_override {
        if is_playable_house_name(owner) {
            return Some(owner.clone());
        }
    }

    // Prefer owners that currently have structures.
    let mut structure_counts: HashMap<String, usize> = HashMap::new();
    for entity in sim.entities.values() {
        let owner_str = sim.interner.resolve(entity.owner);
        if entity.category == EntityCategory::Structure && is_playable_house_name(owner_str) {
            *structure_counts.entry(owner_str.to_string()).or_insert(0) += 1;
        }
    }
    if !structure_counts.is_empty() {
        let mut ranked: Vec<(usize, String)> = structure_counts
            .into_iter()
            .filter_map(|(owner, count)| {
                let strict_buildable = state.rules.as_ref().is_some_and(|rules| {
                    production::has_strict_build_option_for_owner(sim, rules, &owner)
                });
                strict_buildable.then_some((count, owner))
            })
            .collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        if let Some((_, owner)) = ranked.first() {
            return Some(owner.clone());
        }
    }

    // Next fallback: playable houses from map config.
    let houses = collect_playable_owners(state);
    if let Some(owner) = houses.first() {
        return Some(owner.clone());
    }

    // Last fallback: any playable owner present in entity store.
    let mut owners: Vec<String> = sim
        .entities
        .values()
        .map(|e| sim.interner.resolve(e.owner).to_string())
        .filter(|o| is_playable_house_name(o))
        .collect();
    owners.sort();
    owners.dedup();
    owners.first().cloned()
}

pub(crate) fn collect_playable_owners(state: &AppState) -> Vec<String> {
    let mut owners: Vec<String> = state
        .house_roster
        .houses
        .iter()
        .filter(|house| is_playable_house_name(&house.name))
        .filter(|house| house.player_control != Some(false))
        .map(|house| house.name.clone())
        .collect();
    if let Some(sim) = &state.simulation {
        for entity in sim.entities.values() {
            let owner_str = sim.interner.resolve(entity.owner);
            if is_playable_house_name(owner_str) {
                owners.push(owner_str.to_string());
            }
        }
    }
    owners.sort();
    owners.dedup();
    owners
}

fn pick_building_for_owner(
    rules: &crate::rules::ruleset::RuleSet,
    owner: &str,
    candidates: &[&str],
) -> Option<String> {
    for id in candidates {
        let Some(obj) = rules.object(id) else {
            continue;
        };
        if obj.category != crate::rules::object_type::ObjectCategory::Building {
            continue;
        }
        if !obj.owner.is_empty() && !obj.owner.iter().any(|o| o.eq_ignore_ascii_case(owner)) {
            continue;
        }
        return Some((*id).to_string());
    }
    for id in candidates {
        let Some(obj) = rules.object(id) else {
            continue;
        };
        if obj.category == crate::rules::object_type::ObjectCategory::Building {
            return Some((*id).to_string());
        }
    }
    None
}

pub(crate) fn schedule_command(state: &mut AppState, owner: &str, payload: Command) {
    let execute_tick = state
        .simulation
        .as_ref()
        .map_or(state.configured_input_delay_ticks, |s| {
            s.tick.saturating_add(s.input_delay_ticks)
        });
    if let Some(sim) = &mut state.simulation {
        let owner_id = sim.interner.intern(owner);
        sim.queue_command(CommandEnvelope::new(owner_id, execute_tick, payload));
    }
}

pub(crate) fn is_playable_house_name(name: &str) -> bool {
    let up = name.to_ascii_uppercase();
    !matches!(
        up.as_str(),
        "NEUTRAL" | "SPECIAL" | "CIVILIAN" | "GOODGUY" | "BADGUY" | "JP"
    )
}
