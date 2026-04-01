//! Production queue management: enqueue items, advance timers, spawn completed units.
//!
//! Core queue loop driven by `tick_production()`. Handles credit deduction,
//! timer advancement with dynamic rate scaling, and completed-item dispatch.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::rules::ruleset::RuleSet;
use crate::sim::intern::InternedId;
use crate::sim::miner::{ResourceNode, ResourceType};
use crate::sim::world::Simulation;

use super::production_economy::tick_resource_economy;
use super::production_spawn::{find_helipad_for_aircraft, find_spawn_cell_for_owner};
use super::production_tech::{
    build_option_for_owner, build_time_base_frames, effective_progress_rate_ppm_for_type,
    effective_time_to_build_frames_for_type, estimated_real_time_ms,
    production_category_for_object, should_use_relaxed_build_mode, supports_live_production,
};
use super::production_types::*;

const RA2_QUEUE_FRAME_MS: u64 = 66;

/// Set rally point for an owner's production output.
pub fn set_rally_point_for_owner(sim: &mut Simulation, owner: &InternedId, rx: u16, ry: u16) {
    if let Some(house) = sim.houses.get_mut(owner) {
        house.rally_point = Some((rx, ry));
    }
}

/// Return current rally point for owner, if one has been set.
pub fn rally_point_for_owner(sim: &Simulation, owner: &str) -> Option<(u16, u16)> {
    sim.interner
        .get(owner)
        .and_then(|id| sim.houses.get(&id))
        .and_then(|h| h.rally_point)
}

pub fn credits_for_owner(sim: &Simulation, owner: &str) -> i32 {
    sim.interner
        .get(owner)
        .and_then(|id| sim.houses.get(&id))
        .map(|h| h.credits)
        .unwrap_or(STARTING_CREDITS)
}

pub fn power_balance_for_owner(sim: &Simulation, _rules: &RuleSet, owner: &str) -> (i32, i32) {
    // Read from cached PowerState (health-scaled output, full-rated drain).
    // Updated each tick by power_system::tick_power_states().
    let Some(owner_id) = sim.interner.get(owner) else {
        return (0, 0);
    };
    sim.power_states
        .get(&owner_id)
        .map(|state| (state.total_output, state.total_drain))
        .unwrap_or((0, 0))
}

/// Sum of |Power=| from TypeClass for ALL owned buildings (including under
/// construction). Used by the sidebar power bar fill curve.
pub fn theoretical_power_for_owner(sim: &Simulation, owner: &str) -> i32 {
    let Some(owner_id) = sim.interner.get(owner) else {
        return 0;
    };
    sim.power_states
        .get(&owner_id)
        .map(|state| state.theoretical_total_power)
        .unwrap_or(0)
}

pub(in crate::sim) fn credits_entry_for_owner<'a>(
    sim: &'a mut Simulation,
    owner: &str,
) -> &'a mut i32 {
    let key = sim.interner.intern(owner);
    // Ensure house entry exists (auto-create with defaults if missing).
    if !sim.houses.contains_key(&key) {
        sim.houses.insert(
            key,
            crate::sim::house_state::HouseState::new(key, 0, None, false, STARTING_CREDITS, 10),
        );
    }
    &mut sim.houses.get_mut(&key).unwrap().credits
}

pub(super) fn queues_for_owner_mut<'a>(
    sim: &'a mut Simulation,
    owner: &str,
) -> &'a mut std::collections::BTreeMap<ProductionCategory, VecDeque<BuildQueueItem>> {
    let key = sim.interner.intern(owner);
    sim.production.queues_by_owner.entry(key).or_default()
}

pub(super) fn queue_for_owner_category_mut<'a>(
    sim: &'a mut Simulation,
    owner: &str,
    category: ProductionCategory,
) -> &'a mut VecDeque<BuildQueueItem> {
    queues_for_owner_mut(sim, owner)
        .entry(category)
        .or_default()
}

pub(super) fn next_enqueue_order(sim: &mut Simulation) -> u64 {
    let order = sim.production.next_enqueue_order;
    sim.production.next_enqueue_order = sim.production.next_enqueue_order.saturating_add(1);
    order
}

pub(super) fn refresh_queue_states(queue: &mut VecDeque<BuildQueueItem>) {
    for (idx, item) in queue.iter_mut().enumerate() {
        if idx == 0 {
            if item.state != BuildQueueState::Paused {
                item.state = BuildQueueState::Building;
            }
        } else {
            item.state = BuildQueueState::Queued;
        }
    }
}

/// Seed deterministic resource nodes from parsed map overlays.
///
/// Returns how many resource cells were added.
pub fn seed_resource_nodes_from_overlays(
    sim: &mut Simulation,
    overlays: &[crate::map::overlay::OverlayEntry],
    overlay_names: &BTreeMap<u8, String>,
) -> usize {
    let mut added = 0usize;
    let mut warned_ids: BTreeSet<u8> = BTreeSet::new();
    for entry in overlays {
        let Some(name) = overlay_names.get(&entry.overlay_id) else {
            if warned_ids.insert(entry.overlay_id) {
                log::warn!(
                    "Overlay ID {} not in overlay_names -- resource nodes with this ID skipped",
                    entry.overlay_id,
                );
            }
            continue;
        };
        let upper = name.to_ascii_uppercase();
        let is_ore = upper.starts_with("TIB");
        let is_gem = upper.starts_with("GEM");
        if !is_ore && !is_gem {
            continue;
        }
        let richness = u16::from(entry.frame.min(11)).saturating_add(1);
        let base = if is_gem { 180 } else { 120 };
        let stock = base * richness;
        let res_type = if is_gem {
            ResourceType::Gem
        } else {
            ResourceType::Ore
        };
        let key = (entry.rx, entry.ry);
        sim.production
            .resource_nodes
            .entry(key)
            .and_modify(|node| node.remaining = node.remaining.saturating_add(stock))
            .or_insert(ResourceNode {
                resource_type: res_type,
                remaining: stock,
            });
        added += 1;
    }
    added
}

/// Try to enqueue a default buildable unit for `owner`.
///
/// Returns the enqueued type ID on success.
pub fn enqueue_default_unit_for_owner(
    sim: &mut Simulation,
    rules: &RuleSet,
    owner: &str,
) -> Option<InternedId> {
    let type_id: InternedId = pick_default_buildable_unit(sim, rules, owner)?;
    let type_str = sim.interner.resolve(type_id).to_string();
    enqueue_by_type(sim, rules, owner, &type_str).then_some(type_id)
}

/// Enqueue a specific unit type.
pub fn enqueue_by_type(sim: &mut Simulation, rules: &RuleSet, owner: &str, type_id: &str) -> bool {
    let relaxed: bool = should_use_relaxed_build_mode(sim, rules, owner);
    let mode = if relaxed {
        BuildMode::PrototypeRelaxed
    } else {
        BuildMode::Strict
    };
    if let Some(opt) = build_option_for_owner(sim, rules, owner, type_id, mode) {
        if !opt.enabled {
            return false;
        }
    } else {
        return false;
    }
    let Some(obj) = rules.object(type_id) else {
        return false;
    };
    if !supports_live_production(obj) {
        return false;
    }
    let queue_category = production_category_for_object(obj);
    let owner_credits = credits_for_owner(sim, owner);
    if obj.cost <= 0 || owner_credits < obj.cost {
        return false;
    }
    let total_base_frames: u32 = build_time_base_frames(rules, obj);
    *credits_entry_for_owner(sim, owner) -= obj.cost;
    let owner_id = sim.interner.intern(owner);
    let type_interned = sim.interner.intern(type_id);
    let enqueue_order = next_enqueue_order(sim);
    queue_for_owner_category_mut(sim, owner, queue_category).push_back(BuildQueueItem {
        owner: owner_id,
        type_id: type_interned,
        queue_category,
        state: BuildQueueState::Queued,
        total_base_frames,
        remaining_base_frames: total_base_frames,
        progress_carry: 0,
        enqueue_order,
    });
    refresh_queue_states(queue_for_owner_category_mut(sim, owner, queue_category));
    true
}

/// Build a production list across supported sidebar categories for an owner.
///
/// In RA2, only items the player has unlocked via the tech tree are shown.
/// Items with missing prerequisites, wrong faction, or no factory are hidden
/// entirely — only items with insufficient credits are shown greyed out.
pub fn build_options_for_owner(sim: &Simulation, rules: &RuleSet, owner: &str) -> Vec<BuildOption> {
    let strict: Vec<BuildOption> =
        super::production_tech::build_options_for_owner_mode(sim, rules, owner, BuildMode::Strict);

    // Diagnostic: log reason breakdown when nothing is buildable.
    let enabled_count = strict.iter().filter(|o| o.enabled).count();
    if enabled_count == 0 && sim.tick % 90 == 0 {
        let mut reason_counts: BTreeMap<&str, usize> = BTreeMap::new();
        for opt in &strict {
            let key = match &opt.reason {
                Some(BuildDisabledReason::UnbuildableTechLevel) => "UnbuildableTechLevel",
                Some(BuildDisabledReason::WrongOwner) => "WrongOwner",
                Some(BuildDisabledReason::WrongHouse) => "WrongHouse",
                Some(BuildDisabledReason::ForbiddenHouse) => "ForbiddenHouse",
                Some(BuildDisabledReason::RequiresStolenTech) => "RequiresStolenTech",
                Some(BuildDisabledReason::MissingPrerequisite(_)) => "MissingPrerequisite",
                Some(BuildDisabledReason::NoFactory) => "NoFactory",
                Some(BuildDisabledReason::AtBuildLimit) => "AtBuildLimit",
                Some(BuildDisabledReason::InsufficientCredits) => "InsufficientCredits",
                Some(BuildDisabledReason::PlacementModeUnavailable) => "PlacementModeUnavailable",
                None => "Enabled",
            };
            *reason_counts.entry(key).or_default() += 1;
        }
        log::warn!(
            "[BUILD-DIAG] owner='{}' tick={} total_items={} reasons={:?}",
            owner,
            sim.tick,
            strict.len(),
            reason_counts
        );
        // Log owned structures and their factory status.
        for e in sim.entities.values() {
            if sim.interner.resolve(e.owner).eq_ignore_ascii_case(owner)
                && e.category == crate::map::entities::EntityCategory::Structure
            {
                let ts = sim.interner.resolve(e.type_ref);
                log::warn!(
                    "[BUILD-DIAG]   structure '{}' building_up={} factory_type={:?}",
                    ts,
                    e.building_up.is_some(),
                    rules.factory_type(ts)
                );
            }
        }
        // Log a few sample failures to show the exact reason per item.
        for opt in strict.iter().filter(|o| !o.enabled).take(5) {
            let type_str = sim.interner.resolve(opt.type_id);
            log::warn!(
                "[BUILD-DIAG]   sample: '{}' reason={:?}",
                type_str,
                opt.reason
            );
        }
    }

    let visible: Vec<BuildOption> = strict
        .into_iter()
        .filter(|opt| {
            opt.enabled
                || matches!(
                    opt.reason,
                    Some(BuildDisabledReason::InsufficientCredits)
                        | Some(BuildDisabledReason::AtBuildLimit)
                )
        })
        .collect();
    let visible = dedupe_visible_build_options(visible, rules, owner, &sim.interner);
    if !visible.is_empty() || !super::production_tech::prototype_fallback_enabled() {
        return visible;
    }
    dedupe_visible_build_options(
        super::production_tech::build_options_for_owner_mode(
            sim,
            rules,
            owner,
            BuildMode::PrototypeRelaxed,
        ),
        rules,
        owner,
        &sim.interner,
    )
}

fn dedupe_visible_build_options(
    options: Vec<BuildOption>,
    rules: &RuleSet,
    owner: &str,
    interner: &crate::sim::intern::StringInterner,
) -> Vec<BuildOption> {
    let mut deduped: Vec<BuildOption> = Vec::new();
    let mut seen: BTreeMap<(ProductionCategory, String), usize> = BTreeMap::new();

    for option in options {
        let Some(key) = build_option_sidebar_key(rules, &option, interner) else {
            deduped.push(option);
            continue;
        };

        let seen_key = (option.queue_category, key);
        if let Some(existing_idx) = seen.get(&seen_key).copied() {
            if prefers_sidebar_variant(rules, owner, &option, &deduped[existing_idx], interner) {
                deduped[existing_idx] = option;
            }
            continue;
        }

        seen.insert(seen_key, deduped.len());
        deduped.push(option);
    }

    deduped
}

fn build_option_sidebar_key(
    rules: &RuleSet,
    option: &BuildOption,
    interner: &crate::sim::intern::StringInterner,
) -> Option<String> {
    let type_str = interner.resolve(option.type_id);
    let obj = rules.object(type_str)?;
    let image_key = if obj.image.trim().is_empty() {
        obj.id.to_ascii_uppercase()
    } else {
        obj.image.to_ascii_uppercase()
    };
    Some(format!("{}:{image_key}", option.object_category as u8))
}

fn prefers_sidebar_variant(
    rules: &RuleSet,
    owner: &str,
    candidate: &BuildOption,
    existing: &BuildOption,
    interner: &crate::sim::intern::StringInterner,
) -> bool {
    sidebar_variant_rank(rules, owner, candidate, interner)
        > sidebar_variant_rank(rules, owner, existing, interner)
}

fn sidebar_variant_rank(
    rules: &RuleSet,
    owner: &str,
    option: &BuildOption,
    interner: &crate::sim::intern::StringInterner,
) -> (u8, u16, u8) {
    let type_str = interner.resolve(option.type_id);
    let Some(obj) = rules.object(type_str) else {
        return (0, 0, 0);
    };

    let required_house_match = obj
        .required_houses
        .iter()
        .any(|house| house.eq_ignore_ascii_case(owner));
    let owner_specificity = u16::MAX.saturating_sub(obj.owner.len() as u16);
    let enabled = option.enabled as u8;

    (required_house_match as u8, owner_specificity, enabled)
}

/// True if this owner has at least one strictly buildable production option.
///
/// This ignores prototype-relaxed fallback and is useful for picking a likely
/// local player house in UI code.
pub fn has_strict_build_option_for_owner(sim: &Simulation, rules: &RuleSet, owner: &str) -> bool {
    super::production_tech::build_options_for_owner_mode(sim, rules, owner, BuildMode::Strict)
        .iter()
        .any(|o| o.enabled)
}

/// Advance production timers and spawn completed items.
pub fn tick_production(
    sim: &mut Simulation,
    rules: &RuleSet,
    height_map: &BTreeMap<(u16, u16), u8>,
    path_grid: Option<&crate::sim::pathfinding::PathGrid>,
    tick_ms: u32,
) -> bool {
    if tick_ms == 0 {
        return false;
    }
    let miner_config = crate::sim::miner::MinerConfig::from_general_rules(&rules.general);
    tick_resource_economy(sim, rules, &miner_config, path_grid);
    // Collect upfront: the loop body needs get_mut on queues_by_owner, so we
    // must release the immutable borrow before iterating. InternedId is Copy.
    let owner_categories: Vec<(InternedId, ProductionCategory)> = sim
        .production
        .queues_by_owner
        .iter()
        .flat_map(|(owner, queues)| queues.keys().map(move |category| (*owner, *category)))
        .collect();
    if owner_categories.is_empty() {
        return false;
    }

    let mut spawned_any = false;
    for (owner_id, queue_category) in owner_categories {
        let owner_str = sim.interner.resolve(owner_id).to_string();
        let progress_rate: u64 = sim
            .production
            .queues_by_owner
            .get(&owner_id)
            .and_then(|queues| queues.get(&queue_category))
            .and_then(|queue| queue.front())
            .map(|front| {
                let type_str = sim.interner.resolve(front.type_id);
                effective_progress_rate_ppm_for_type(sim, rules, &owner_str, type_str)
            })
            .unwrap_or(PRODUCTION_RATE_SCALE);
        let completed: Option<BuildQueueItem> = {
            let queue = sim
                .production
                .queues_by_owner
                .get_mut(&owner_id)
                .and_then(|queues| queues.get_mut(&queue_category));
            let Some(queue) = queue else { continue };
            refresh_queue_states(queue);
            if let Some(front) = queue.front_mut() {
                if front.state == BuildQueueState::Paused {
                    None
                } else {
                    advance_queue_item(front, tick_ms, progress_rate);
                    if front.remaining_base_frames > 0 {
                        None
                    } else {
                        front.state = BuildQueueState::Done;
                        let done = queue.pop_front();
                        refresh_queue_states(queue);
                        done
                    }
                }
            } else {
                None
            }
        };

        let Some(done) = completed else { continue };

        let done_type_str = sim.interner.resolve(done.type_id).to_string();
        let produced_category = rules.object(&done_type_str).map(|o| o.category);
        if produced_category == Some(crate::rules::object_type::ObjectCategory::Building) {
            sim.production
                .ready_by_owner
                .entry(done.owner)
                .or_default()
                .push_back(done.type_id);
            sim.sound_events
                .push(crate::sim::world::SimSoundEvent::BuildingComplete { owner: done.owner });
            continue;
        }
        // Aircraft use helipad spawn path; other units use exit cell path.
        let is_aircraft =
            produced_category == Some(crate::rules::object_type::ObjectCategory::Aircraft);
        let spawn_cell: Option<(u16, u16)>;
        let helipad_airfield: Option<u64>;

        if is_aircraft {
            if let Some((af_id, rx, ry)) = find_helipad_for_aircraft(sim, rules, &owner_str) {
                spawn_cell = Some((rx, ry));
                helipad_airfield = Some(af_id);
            } else {
                // No free helipad — refund.
                if let Some(obj) = rules.object(&done_type_str) {
                    *credits_entry_for_owner(sim, &owner_str) += obj.cost.max(0);
                }
                continue;
            }
        } else {
            let is_naval: bool = rules.object(&done_type_str).map_or(false, |o| o.naval);
            spawn_cell = produced_category.and_then(|cat| {
                find_spawn_cell_for_owner(sim, rules, &owner_str, cat, path_grid, is_naval)
            });
            helipad_airfield = None;
            if spawn_cell.is_none() {
                if let Some(obj) = rules.object(&done_type_str) {
                    *credits_entry_for_owner(sim, &owner_str) += obj.cost.max(0);
                }
                continue;
            }
        }
        let (rx, ry) = spawn_cell.unwrap();

        let spawned = sim.spawn_object(&done_type_str, &owner_str, rx, ry, 64, rules, height_map);
        if let Some(stable_id) = spawned {
            // Aircraft spawned on helipad: set DockedIdle and reserve dock slot.
            if let Some(af_id) = helipad_airfield {
                if let Some(entity) = sim.entities.get_mut(stable_id) {
                    entity.aircraft_mission =
                        Some(crate::sim::aircraft::AircraftMission::DockedIdle {
                            airfield_id: af_id,
                        });
                }
                let max_slots = sim
                    .entities
                    .get(af_id)
                    .and_then(|af| {
                        let af_type = sim.interner.resolve(af.type_ref);
                        let af_obj = rules.object(af_type)?;
                        Some(af_obj.number_of_docks.max(1))
                    })
                    .unwrap_or(1);
                sim.production
                    .airfield_docks
                    .try_reserve(af_id, stable_id, max_slots);
            }
            sim.sound_events
                .push(crate::sim::world::SimSoundEvent::UnitComplete { owner: done.owner });
            // Auto-move newly produced unit to rally point (if set).
            // Skip for aircraft docked on helipad — they wait for orders.
            if helipad_airfield.is_none() {
                if let (Some(grid), Some((tx, ty))) =
                    (path_grid, rally_point_for_owner(sim, &owner_str))
                {
                    let obj = rules.object(&done_type_str);
                    let loco_mult = sim
                        .entities
                        .get(stable_id)
                        .and_then(|e| e.locomotor.as_ref())
                        .map(|l| l.speed_multiplier)
                        .unwrap_or(crate::util::fixed_math::SIM_ONE);
                    let speed = obj
                        .map(|o| crate::util::fixed_math::ra2_speed_to_leptons_per_second(o.speed))
                        .unwrap_or(crate::util::fixed_math::ra2_speed_to_leptons_per_second(4));
                    let speed =
                        (speed * loco_mult).max(crate::util::fixed_math::SimFixed::lit("25"));
                    let speed_type = sim
                        .entities
                        .get(stable_id)
                        .and_then(|e| e.locomotor.as_ref())
                        .map(|l| l.speed_type);
                    let cost_grid = speed_type.and_then(|st| sim.terrain_costs.get(&st));
                    let _ = crate::sim::movement::issue_move_command_with_layered(
                        &mut sim.entities,
                        grid,
                        sim.layered_path_grid.as_ref(),
                        stable_id,
                        (tx, ty),
                        speed,
                        false,
                        cost_grid,
                        None,
                        sim.resolved_terrain.as_ref(),
                    );
                }
            }
            spawned_any = true;
        } else {
            if let Some(obj) = rules.object(&done_type_str) {
                *credits_entry_for_owner(sim, &owner_str) += obj.cost.max(0);
            }
        }
    }

    sim.production.queues_by_owner.retain(|_, queues| {
        queues.retain(|_, queue| !queue.is_empty());
        !queues.is_empty()
    });
    spawned_any
}

/// Build a queue snapshot for one owner, including progress metadata for UI.
pub fn queue_view_for_owner(sim: &Simulation, rules: &RuleSet, owner: &str) -> Vec<QueueItemView> {
    let owner_id = sim.interner.get(owner);
    let queues = owner_id.and_then(|id| sim.production.queues_by_owner.get(&id));
    let Some(queues) = queues else {
        return Vec::new();
    };
    let mut items: Vec<&BuildQueueItem> =
        queues.iter().flat_map(|(_, queue)| queue.iter()).collect();
    items.sort_by_key(|item| (item.queue_category, item.enqueue_order));
    items
        .into_iter()
        .map(|q| {
            let type_str = sim.interner.resolve(q.type_id);
            let (display_name, remaining_frames, total_frames) = rules
                .object(type_str)
                .map(|obj| {
                    (
                        obj.name.clone().unwrap_or_else(|| type_str.to_string()),
                        effective_time_to_build_frames_for_type(
                            sim,
                            rules,
                            owner,
                            type_str,
                            q.remaining_base_frames,
                        ),
                        effective_time_to_build_frames_for_type(
                            sim,
                            rules,
                            owner,
                            type_str,
                            q.total_base_frames.max(1),
                        ),
                    )
                })
                .unwrap_or_else(|| {
                    let frames = q.remaining_base_frames;
                    (type_str.to_string(), frames, q.total_base_frames.max(1))
                });
            QueueItemView {
                type_id: q.type_id,
                display_name,
                queue_category: q.queue_category,
                state: q.state,
                remaining_ms: estimated_real_time_ms(remaining_frames, PRODUCTION_RATE_SCALE),
                total_ms: estimated_real_time_ms(total_frames, PRODUCTION_RATE_SCALE),
            }
        })
        .collect()
}

pub fn ready_buildings_for_owner(
    sim: &Simulation,
    rules: &RuleSet,
    owner: &str,
) -> Vec<ReadyBuildingView> {
    let owner_id = sim.interner.get(owner);
    let ready = owner_id.and_then(|id| sim.production.ready_by_owner.get(&id));
    ready
        .map(|ready| {
            ready
                .iter()
                .filter_map(|&type_id| {
                    let type_str = sim.interner.resolve(type_id);
                    let obj = rules.object(type_str)?;
                    Some(ReadyBuildingView {
                        type_id,
                        display_name: obj.name.clone().unwrap_or_else(|| type_str.to_string()),
                        queue_category: production_category_for_object(obj),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Cancel the most recently queued item for this owner and refund full cost.
pub fn cancel_last_for_owner(sim: &mut Simulation, rules: &RuleSet, owner: &str) -> bool {
    let owner_id = sim.interner.intern(owner);
    let Some(category) = ({
        sim.production.queues_by_owner.get(&owner_id).and_then(|q| {
            q.iter()
                .filter_map(|(category, queue)| {
                    queue.back().map(|item| (*category, item.enqueue_order))
                })
                .max_by_key(|(_, order)| *order)
                .map(|(category, _)| category)
        })
    }) else {
        return false;
    };
    let Some(item) = sim
        .production
        .queues_by_owner
        .get_mut(&owner_id)
        .and_then(|owner_queues| owner_queues.get_mut(&category))
        .and_then(|queue| {
            let item = queue.pop_back();
            refresh_queue_states(queue);
            item
        })
    else {
        return false;
    };
    let type_str = sim.interner.resolve(item.type_id).to_string();
    if let Some(obj) = rules.object(&type_str) {
        *credits_entry_for_owner(sim, owner) += obj.cost.max(0);
    }
    sim.production.queues_by_owner.retain(|_, queues| {
        queues.retain(|_, queue| !queue.is_empty());
        !queues.is_empty()
    });
    true
}

/// Cancel one queued item of a specific type_id (right-click cameo in RA2).
/// Removes the last-enqueued instance of that type, refunding its cost.
pub fn cancel_by_type_for_owner(
    sim: &mut Simulation,
    rules: &RuleSet,
    owner: &str,
    type_id: &str,
) -> bool {
    let owner_id = sim.interner.intern(owner);
    let type_interned = sim.interner.intern(type_id);
    let target = sim
        .production
        .queues_by_owner
        .get(&owner_id)
        .and_then(|owner_queues| {
            for (category, queue) in owner_queues.iter() {
                let idx = queue
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, item)| item.type_id == type_interned)
                    .map(|(i, _)| i);
                if let Some(idx) = idx {
                    return Some((*category, idx));
                }
            }
            None
        });
    let Some((category, idx)) = target else {
        // Fallback: check the ready_by_owner queue (completed buildings awaiting placement).
        return cancel_ready_by_type_for_owner(sim, rules, owner, type_id);
    };
    let removed = sim
        .production
        .queues_by_owner
        .get_mut(&owner_id)
        .and_then(|queues| queues.get_mut(&category))
        .and_then(|queue| {
            let item = queue.remove(idx);
            refresh_queue_states(queue);
            item
        });
    if let Some(removed) = removed {
        let removed_type_str = sim.interner.resolve(removed.type_id).to_string();
        if let Some(obj) = rules.object(&removed_type_str) {
            *credits_entry_for_owner(sim, owner) += obj.cost.max(0);
        }
    }
    sim.production.queues_by_owner.retain(|_, queues| {
        queues.retain(|_, q| !q.is_empty());
        !queues.is_empty()
    });
    true
}

/// Cancel a completed building from the ready_by_owner queue (awaiting placement).
/// Used as fallback when `cancel_by_type_for_owner` finds nothing in the build queue.
fn cancel_ready_by_type_for_owner(
    sim: &mut Simulation,
    rules: &RuleSet,
    owner: &str,
    type_id: &str,
) -> bool {
    let owner_id = sim.interner.intern(owner);
    let type_interned = sim.interner.intern(type_id);
    let Some(ready_queue) = sim.production.ready_by_owner.get_mut(&owner_id) else {
        return false;
    };
    // Remove last instance of this type (consistent with queue cancel using .rev()).
    let ready_idx = ready_queue
        .iter()
        .enumerate()
        .rev()
        .find(|(_, tid)| **tid == type_interned)
        .map(|(i, _)| i);
    let Some(idx) = ready_idx else {
        return false;
    };
    ready_queue.remove(idx);
    if ready_queue.is_empty() {
        sim.production.ready_by_owner.remove(&owner_id);
    }
    // Refund full cost.
    if let Some(obj) = rules.object(type_id) {
        *credits_entry_for_owner(sim, owner) += obj.cost.max(0);
    }
    true
}

fn advance_queue_item(item: &mut BuildQueueItem, tick_ms: u32, rate_ppm: u64) {
    if item.remaining_base_frames == 0 || tick_ms == 0 {
        return;
    }
    let frame_scale = RA2_QUEUE_FRAME_MS.saturating_mul(PRODUCTION_RATE_SCALE);
    let scaled_progress = u64::from(tick_ms)
        .saturating_mul(rate_ppm)
        .saturating_add(item.progress_carry);
    let progressed_base_frames = (scaled_progress / frame_scale) as u32;
    item.progress_carry = scaled_progress % frame_scale;

    if progressed_base_frames >= item.remaining_base_frames {
        item.remaining_base_frames = 0;
        item.progress_carry = 0;
    } else {
        item.remaining_base_frames -= progressed_base_frames;
    }
}

fn pick_default_buildable_unit(
    sim: &Simulation,
    rules: &RuleSet,
    owner: &str,
) -> Option<InternedId> {
    let mode = if should_use_relaxed_build_mode(sim, rules, owner) {
        BuildMode::PrototypeRelaxed
    } else {
        BuildMode::Strict
    };
    super::production_tech::build_options_for_owner_mode(sim, rules, owner, mode)
        .into_iter()
        .find(|opt| {
            opt.enabled
                && matches!(
                    opt.queue_category,
                    ProductionCategory::Infantry
                        | ProductionCategory::Vehicle
                        | ProductionCategory::Aircraft
                )
        })
        .map(|opt| opt.type_id)
}
