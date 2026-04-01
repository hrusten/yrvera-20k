//! Debug info panel — egui overlay showing PathGrid, cursor cell, and entity info.
//!
//! Visible when the PathGrid debug overlay is active (F9 / P toggle).
//! Shows: grid dimensions, cell under cursor, walkability, entities at cell,
//! and the selected unit's current path.
//!
//! ## Dependency rules
//! - Part of the app layer — may depend on everything.

use crate::app::AppState;
use crate::map::entities::EntityCategory;
use crate::sim::debug_event_log::DebugEventKind;

/// Light-themed frame for all debug panels — .NET/Windows-style appearance.
fn debug_panel_frame() -> egui::Frame {
    egui::Frame {
        fill: egui::Color32::from_rgb(245, 245, 245),
        stroke: egui::Stroke::new(1.0, egui::Color32::from_rgb(180, 180, 180)),
        inner_margin: egui::Margin::same(6),
        outer_margin: egui::Margin::same(2),
        corner_radius: egui::CornerRadius::same(3),
        ..Default::default()
    }
}

/// Apply light theme visuals to the egui context so window title bars render
/// with dark text on light chrome. Call before any debug panel windows.
/// Returns the previous visuals so they can be restored after.
pub(crate) fn push_debug_light_visuals(ctx: &egui::Context) -> egui::Visuals {
    let prev = ctx.style().visuals.clone();
    // Use egui's built-in light preset — it properly sets all widget states,
    // title bar text, separator colors, and scroll bar styling for light mode.
    let mut visuals = egui::Visuals::light();
    visuals.window_fill = egui::Color32::from_rgb(245, 245, 245);
    visuals.panel_fill = egui::Color32::from_rgb(245, 245, 245);
    visuals.window_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(180, 180, 180));
    ctx.set_visuals(visuals);
    prev
}

/// Restore previous visuals after debug panels are done.
pub(crate) fn pop_debug_light_visuals(ctx: &egui::Context, prev: egui::Visuals) {
    ctx.set_visuals(prev);
}

/// Apply light theme text overrides inside a debug panel UI scope.
fn apply_light_text(ui: &mut egui::Ui) {
    ui.visuals_mut().override_text_color = Some(egui::Color32::from_rgb(30, 30, 30));
}

/// Draw hotkey reference overlay (top-right corner).
pub(crate) fn draw_hotkey_help(ctx: &egui::Context) {
    egui::Window::new("Hotkeys (F1)")
        .default_pos([ctx.content_rect().max.x - 280.0, 4.0])
        .default_width(260.0)
        .frame(debug_panel_frame())
        .collapsible(true)
        .resizable(false)
        .show(ctx, |ui| {
            apply_light_text(ui);

            ui.label(
                egui::RichText::new("Game Controls")
                    .strong()
                    .color(egui::Color32::from_rgb(20, 20, 20)),
            );
            let game_keys: &[(&str, &str)] = &[
                ("S", "Stop selected units"),
                ("D", "Deploy / Undeploy"),
                ("A", "Attack move mode"),
                ("G", "Guard mode"),
                ("M", "Quicksave"),
                ("N", "Load most recent save"),
                ("F5", "Save/load panel"),
                ("T", "Select same type"),
                ("Del", "Sell building"),
                ("B", "Move mode"),
                ("0-9", "Select control group"),
                ("Ctrl+0-9", "Assign control group"),
                ("H", "Jump to base (MCV)"),
                ("Space", "Jump to radar event"),
                ("Esc", "Pause / Cancel"),
            ];
            for (key, desc) in game_keys {
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::from_rgb(160, 120, 0), format!("{:>10}", key));
                    ui.label(*desc);
                });
            }

            ui.separator();
            ui.label(
                egui::RichText::new("Debug Overlays")
                    .strong()
                    .color(egui::Color32::from_rgb(20, 20, 20)),
            );
            let debug_keys: &[(&str, &str)] = &[
                ("F1", "This help panel"),
                ("P / F9", "Terrain costs + debug panel"),
                ("[ ]", "Cycle SpeedType (when P active)"),
                ("K", "Height map (blue=bridge)"),
                ("L", "Cell grid (cyan+yellow)"),
                ("V / F10", "Toggle fog of war"),
                ("X", "Unit inspector (event log)"),
            ];
            for (key, desc) in debug_keys {
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::from_rgb(0, 90, 160), format!("{:>10}", key));
                    ui.label(*desc);
                });
            }

            ui.separator();
            ui.label(
                egui::RichText::new("Mouse")
                    .strong()
                    .color(egui::Color32::from_rgb(20, 20, 20)),
            );
            let mouse_keys: &[(&str, &str)] = &[
                ("LMB", "Select / Place"),
                ("RMB", "Order / Deselect"),
                ("MMB drag", "Pan camera"),
                ("Wheel", "Scroll sidebar"),
            ];
            for (key, desc) in mouse_keys {
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::from_rgb(20, 120, 20), format!("{:>10}", key));
                    ui.label(*desc);
                });
            }
        });
}

/// Draw the debug info panel as an egui window (top-left corner).
///
/// Only call this when `state.debug_show_pathgrid` is true.
pub(crate) fn draw_debug_panel(ctx: &egui::Context, state: &AppState) {
    // Convert cursor screen position to world coordinates, then to iso cell.
    let (cursor_rx, cursor_ry) =
        crate::app_sim_tick::screen_point_to_world_cell(state, state.cursor_x, state.cursor_y);

    egui::Window::new("Terrain Debug")
        .default_pos([4.0, 4.0])
        .default_width(280.0)
        .frame(debug_panel_frame())
        .collapsible(true)
        .resizable(false)
        .show(ctx, |ui| {
            apply_light_text(ui);
            // --- Grid dimensions ---
            if let Some(grid) = &state.path_grid {
                ui.label(format!("Grid: {}x{}", grid.width(), grid.height()));
            } else {
                ui.label("Grid: (none)");
            }

            // --- Active SpeedType for terrain cost overlay ---
            let active_st = crate::app_debug_overlays::resolve_debug_speed_type(state);
            ui.colored_label(
                egui::Color32::from_rgb(0, 90, 160),
                format!("Overlay: {} ([ ] to cycle)", active_st.name()),
            );

            // --- VXL render pipeline ---
            if let Some(atlas) = &state.unit_atlas {
                ui.separator();
                if atlas.gpu_rendered > 0 {
                    ui.colored_label(
                        egui::Color32::from_rgb(20, 120, 20),
                        format!(
                            "VXL: {} GPU compute, {} CPU",
                            atlas.gpu_rendered, atlas.cpu_rendered,
                        ),
                    );
                } else {
                    ui.colored_label(
                        egui::Color32::from_rgb(160, 120, 0),
                        format!("VXL: {} CPU (GPU inactive)", atlas.cpu_rendered),
                    );
                }
            }

            ui.separator();

            // --- Cursor cell info ---
            ui.label(format!("Cursor cell: ({}, {})", cursor_rx, cursor_ry));
            let z: u8 = state
                .height_map
                .get(&(cursor_rx, cursor_ry))
                .copied()
                .unwrap_or(0);
            ui.label(format!("Elevation: {}", z));

            if let Some(grid) = &state.path_grid {
                let walkable: bool = grid.is_walkable(cursor_rx, cursor_ry);
                let label = if walkable { "WALKABLE" } else { "BLOCKED" };
                ui.colored_label(
                    if walkable {
                        egui::Color32::from_rgb(20, 120, 20)
                    } else {
                        egui::Color32::from_rgb(180, 0, 0)
                    },
                    format!("PathGrid: {}", label),
                );
            }

            // Show terrain cost for common SpeedTypes at cursor cell.
            // The active overlay SpeedType is highlighted.
            if let Some(sim) = &state.simulation {
                use crate::rules::locomotor_type::SpeedType;
                let speed_types = [
                    (SpeedType::Foot, "Foot"),
                    (SpeedType::Track, "Track"),
                    (SpeedType::Wheel, "Wheel"),
                    (SpeedType::Float, "Float"),
                    (SpeedType::Amphibious, "Amphibious"),
                ];
                for (st, name) in &speed_types {
                    if let Some(cg) = sim.terrain_costs.get(st) {
                        let cost = cg.cost_at(cursor_rx, cursor_ry);
                        let label = if cost == 0 {
                            format!("{}: BLOCKED", name)
                        } else {
                            format!("{}: cost={}", name, cost)
                        };
                        let is_active = *st == active_st;
                        let color = if is_active {
                            if cost == 0 {
                                egui::Color32::from_rgb(180, 0, 0)
                            } else {
                                egui::Color32::from_rgb(20, 120, 20)
                            }
                        } else {
                            egui::Color32::from_rgb(130, 130, 130)
                        };
                        ui.colored_label(color, label);
                    }
                }
            }

            ui.separator();

            // --- Entities at cursor cell ---
            if let Some(sim) = &state.simulation {
                let mut found: Vec<String> = Vec::new();
                for entity in sim.entities.values() {
                    if entity.position.rx == cursor_rx && entity.position.ry == cursor_ry {
                        let cat_str = match entity.category {
                            EntityCategory::Unit => "Unit",
                            EntityCategory::Infantry => "Inf",
                            EntityCategory::Structure => "Bld",
                            EntityCategory::Aircraft => "Air",
                        };
                        found.push(format!(
                            "{}({}) [{}] hp={}/{}",
                            sim.interner.resolve(entity.type_ref),
                            cat_str,
                            sim.interner.resolve(entity.owner),
                            entity.health.current,
                            entity.health.max,
                        ));
                    }
                }
                if found.is_empty() {
                    ui.label("Entities: (none)");
                } else {
                    ui.label(format!("Entities ({})", found.len()));
                    for desc in &found {
                        ui.label(format!("  {}", desc));
                    }
                }

                // Show building footprint info for structures near cursor.
                if let Some(rules) = &state.rules {
                    for entity in sim.entities.values() {
                        if entity.category != EntityCategory::Structure {
                            continue;
                        }
                        let foundation = rules
                            .object(sim.interner.resolve(entity.type_ref))
                            .map(|obj| obj.foundation.as_str())
                            .unwrap_or("1x1");
                        let (fw, fh) = crate::sim::production::foundation_dimensions(foundation);
                        let ex: u16 = entity.position.rx;
                        let ey: u16 = entity.position.ry;
                        // Check if cursor is within this building's footprint.
                        if cursor_rx >= ex
                            && cursor_rx < ex + fw as u16
                            && cursor_ry >= ey
                            && cursor_ry < ey + fh as u16
                        {
                            ui.colored_label(
                                egui::Color32::from_rgb(160, 120, 0),
                                format!(
                                    "In footprint: {} ({}) @ ({},{}) {}",
                                    sim.interner.resolve(entity.type_ref),
                                    foundation,
                                    ex,
                                    ey,
                                    sim.interner.resolve(entity.owner)
                                ),
                            );
                        }
                    }
                }
            }

            ui.separator();

            // --- Selected unit path info ---
            if let Some(sim) = &state.simulation {
                let selected: Vec<u64> = sim
                    .entities
                    .values()
                    .filter(|e| e.selected)
                    .map(|e| e.stable_id)
                    .collect();
                if selected.is_empty() {
                    ui.label("Selected: (none)");
                } else {
                    for &sid in &selected {
                        if let Some(entity) = sim.entities.get(sid) {
                            ui.label(format!(
                                "Sel: {} @ ({},{}) sub=({},{})",
                                sim.interner.resolve(entity.type_ref),
                                entity.position.rx,
                                entity.position.ry,
                                entity.position.sub_x,
                                entity.position.sub_y,
                            ));
                            if let Some(ref mt) = entity.movement_target {
                                ui.label(format!(
                                    "Path: {}/{} steps, blocked={}",
                                    mt.next_index,
                                    mt.path.len(),
                                    mt.path_blocked,
                                ));
                                if let Some(goal) = mt.final_goal {
                                    ui.label(format!("Goal: ({},{})", goal.0, goal.1));
                                }
                                // Show first few path steps with walkability.
                                let start = mt.next_index.saturating_sub(1);
                                let end = (start + 8).min(mt.path.len());
                                let grid = state.path_grid.as_ref();
                                for i in start..end {
                                    let (px, py) = mt.path[i];
                                    let w = grid.map_or(true, |g| g.is_walkable(px, py));
                                    let marker = if i == mt.next_index { ">" } else { " " };
                                    let color = if w {
                                        egui::Color32::from_rgb(20, 120, 20)
                                    } else {
                                        egui::Color32::from_rgb(180, 0, 0)
                                    };
                                    ui.colored_label(
                                        color,
                                        format!(
                                            "  {}[{}] ({},{}) {}",
                                            marker,
                                            i,
                                            px,
                                            py,
                                            if w { "" } else { "BLOCKED" }
                                        ),
                                    );
                                }
                            } else {
                                ui.label("Path: (idle)");
                            }
                        }
                    }
                }
            }

            // --- Miner debug info for selected harvesters ---
            if let Some(sim) = &state.simulation {
                for entity in sim.entities.values().filter(|e| e.selected) {
                    let Some(ref miner) = entity.miner else {
                        continue;
                    };
                    ui.separator();
                    ui.label(
                        egui::RichText::new(format!(
                            "Miner: {} ({:?})",
                            sim.interner.resolve(entity.type_ref),
                            miner.kind
                        ))
                        .strong(),
                    );
                    let state_color = match miner.state {
                        crate::sim::miner::MinerState::Harvest => {
                            egui::Color32::from_rgb(20, 120, 20)
                        }
                        crate::sim::miner::MinerState::MoveToOre => {
                            egui::Color32::from_rgb(160, 120, 0)
                        }
                        crate::sim::miner::MinerState::SearchOre => {
                            egui::Color32::from_rgb(0, 90, 160)
                        }
                        crate::sim::miner::MinerState::ReturnToRefinery => {
                            egui::Color32::from_rgb(160, 110, 0)
                        }
                        crate::sim::miner::MinerState::Dock => {
                            egui::Color32::from_rgb(180, 100, 20)
                        }
                        crate::sim::miner::MinerState::WaitNoOre => {
                            egui::Color32::from_rgb(180, 0, 0)
                        }
                        _ => egui::Color32::from_rgb(80, 80, 80),
                    };
                    ui.colored_label(state_color, format!("State: {:?}", miner.state));
                    if matches!(miner.state, crate::sim::miner::MinerState::Dock) {
                        ui.label(format!("  Dock phase: {:?}", miner.dock_phase));
                    }
                    ui.label(format!(
                        "Cargo: {}/{}",
                        miner.cargo.len(),
                        miner.capacity_bales
                    ));
                    ui.label(format!("Harvest timer: {}", miner.harvest_timer));
                    if let Some(ore) = miner.target_ore_cell {
                        ui.label(format!("Target ore: ({},{})", ore.0, ore.1));
                    } else {
                        ui.label("Target ore: (none)");
                    }
                    if let Some(ref_id) = miner.reserved_refinery {
                        let ref_type = sim
                            .entities
                            .get(ref_id)
                            .map(|e| sim.interner.resolve(e.type_ref))
                            .unwrap_or("?");
                        ui.label(format!("Refinery: {} (id={})", ref_type, ref_id));
                    }
                    if let Some(last) = miner.last_harvest_cell {
                        ui.label(format!("Last harvest: ({},{})", last.0, last.1));
                    }
                    let has_mt = entity.movement_target.is_some();
                    let has_tp = entity.teleport_state.is_some();
                    ui.label(format!(
                        "Movement: {} Teleport: {}",
                        if has_mt { "active" } else { "idle" },
                        if has_tp { "active" } else { "idle" },
                    ));
                    if let Some(ref mt) = entity.movement_target {
                        ui.label(format!("  ignore_terrain: {}", mt.ignore_terrain_cost));
                    }
                }
            }
        });
}

/// Draw the unit event history inspector as a separate window (light theme).
///
/// Positioned to the right of the Terrain Debug panel. Only shown when
/// `debug_unit_inspector` is true (X hotkey).
pub(crate) fn draw_event_history_panel(ctx: &egui::Context, state: &AppState) {
    if !state.debug_unit_inspector {
        return;
    }
    let Some(sim) = &state.simulation else { return };

    egui::Window::new("Event History")
        .default_pos([294.0, 4.0])
        .default_width(380.0)
        .frame(debug_panel_frame())
        .collapsible(true)
        .resizable(true)
        .show(ctx, |ui| {
            apply_light_text(ui);

            let selected: Vec<_> = sim.entities.values().filter(|e| e.selected).collect();
            if selected.is_empty() {
                ui.label(
                    egui::RichText::new("Select a unit to inspect")
                        .italics()
                        .color(egui::Color32::from_rgb(120, 120, 120)),
                );
                return;
            }

            for entity in &selected {
                if let Some(ref log) = entity.debug_log {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} (id={})",
                            sim.interner.resolve(entity.type_ref),
                            entity.stable_id
                        ))
                        .strong()
                        .color(egui::Color32::from_rgb(20, 20, 20)),
                    );
                    ui.separator();

                    if log.events.is_empty() {
                        ui.label(
                            egui::RichText::new("(no events recorded)")
                                .color(egui::Color32::from_rgb(140, 140, 140)),
                        );
                    } else {
                        egui::ScrollArea::vertical()
                            .max_height(400.0)
                            .show(ui, |ui| {
                                let events: Vec<_> = log.events.iter().rev().take(30).collect();
                                for event in events {
                                    let (color, text) = format_debug_event_light(event);
                                    ui.colored_label(color, text);
                                }
                            });
                    }
                } else {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} — inspector not active when spawned",
                            sim.interner.resolve(entity.type_ref)
                        ))
                        .color(egui::Color32::from_rgb(140, 140, 140)),
                    );
                }
            }
        });
}

/// Format a debug event with colors suited for a light background.
fn format_debug_event_light(
    event: &crate::sim::debug_event_log::DebugEvent,
) -> (egui::Color32, String) {
    let tick = event.tick;
    match &event.kind {
        DebugEventKind::PhaseChange { from, to, reason } => (
            egui::Color32::from_rgb(20, 120, 20),
            format!("[{tick}] {from} \u{2192} {to} ({reason})"),
        ),
        DebugEventKind::Repath {
            reason,
            new_path_len,
        } => (
            egui::Color32::from_rgb(160, 130, 0),
            format!("[{tick}] Repath: {reason} ({new_path_len} steps)"),
        ),
        DebugEventKind::Blocked { by_entity, cell } => (
            egui::Color32::from_rgb(200, 50, 50),
            format!(
                "[{tick}] Blocked at ({},{}){}",
                cell.0,
                cell.1,
                by_entity.map(|id| format!(" by #{id}")).unwrap_or_default(),
            ),
        ),
        DebugEventKind::StuckAbort { blocked_ticks } => (
            egui::Color32::from_rgb(180, 0, 0),
            format!("[{tick}] STUCK ABORT after {blocked_ticks} ticks"),
        ),
        DebugEventKind::PathSegmentComplete { final_goal } => (
            egui::Color32::from_rgb(30, 90, 160),
            format!(
                "[{tick}] Path segment complete{}",
                final_goal
                    .map(|(x, y)| format!(", repathing to ({x},{y})"))
                    .unwrap_or_default(),
            ),
        ),
        DebugEventKind::MinerStateChange { from, to } => (
            egui::Color32::from_rgb(160, 110, 0),
            format!("[{tick}] Miner: {from} \u{2192} {to}"),
        ),
        DebugEventKind::DockPhaseChange { from, to } => (
            egui::Color32::from_rgb(180, 100, 20),
            format!("[{tick}] Dock: {from} \u{2192} {to}"),
        ),
        DebugEventKind::SpecialMovementStart { kind } => (
            egui::Color32::from_rgb(90, 50, 180),
            format!("[{tick}] {kind} started"),
        ),
        DebugEventKind::SpecialMovementPhase { phase } => (
            egui::Color32::from_rgb(90, 50, 180),
            format!("[{tick}] \u{2192} {phase}"),
        ),
        DebugEventKind::SpecialMovementEnd => (
            egui::Color32::from_rgb(90, 50, 180),
            format!("[{tick}] Special movement ended"),
        ),
        DebugEventKind::LocomotorOverride { kind, active } => (
            egui::Color32::from_rgb(160, 120, 0),
            format!(
                "[{tick}] Override {}: {kind}",
                if *active { "ON" } else { "OFF" },
            ),
        ),
    }
}
