//! Minimal in-game HUD overlay (egui) for gameplay actions.
//!
//! Temporary bridge until the custom RA2 sidebar is implemented.

use crate::sim::production::{
    BuildDisabledReason, BuildOption, BuildQueueState, BuildingPlacementPreview, ProducerFocusView,
    ProductionCategory, QueueItemView, ReadyBuildingView, disabled_reason_text,
};

/// Actions produced by the in-game HUD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InGameHudAction {
    None,
    BuildType(String),
    ArmPlacement(String),
    ClearPlacementMode,
    TogglePauseQueue(ProductionCategory),
    CycleProducer(ProductionCategory),
    CancelLastBuild,
    CycleOwner,
    PlaceStarterBase,
    SpawnTestUnits,
}

/// Draw a compact HUD panel with credits/queue and categorized build list.
pub fn draw_in_game_hud(
    ctx: &egui::Context,
    credits: i32,
    queue_items: &[QueueItemView],
    owner_name: &str,
    rally_point: Option<(u16, u16)>,
    options: &[BuildOption],
    ready_buildings: &[ReadyBuildingView],
    armed_building: Option<&str>,
    placement_preview: Option<&BuildingPlacementPreview>,
    producer_focus: &[ProducerFocusView],
    interner: Option<&crate::sim::intern::StringInterner>,
) -> InGameHudAction {
    let resolve = |id: crate::sim::intern::InternedId| -> String {
        interner.map_or(format!("#{}", id.index()), |i| i.resolve(id).to_string())
    };
    let mut action = InGameHudAction::None;
    egui::Area::new("in_game_hud".into())
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-12.0, 12.0))
        .show(ctx, |ui| {
            egui::Frame::window(ui.style())
                .corner_radius(4.0)
                .show(ui, |ui| {
                    ui.set_min_width(300.0);
                    ui.heading("Command");
                    ui.label(format!("Owner: {}", owner_name));
                    if ui.button("Switch Owner").clicked() {
                        action = InGameHudAction::CycleOwner;
                    }
                    ui.label(format!("Credits: {}", credits));
                    ui.label(format!("Queue: {}", queue_items.len()));
                    if let Some((rx, ry)) = rally_point {
                        ui.label(format!("Rally: ({},{})", rx, ry));
                    } else {
                        ui.label("Rally: (not set)");
                    }
                    if ui.button("Place Starter Base").clicked() {
                        action = InGameHudAction::PlaceStarterBase;
                    }
                    if ui.button("Spawn Test Units").clicked() {
                        action = InGameHudAction::SpawnTestUnits;
                    }
                    if !ready_buildings.is_empty() {
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new("Placement Ready").strong());
                        if let Some(type_id) = armed_building {
                            ui.label(format!("Armed: {}", type_id));
                            if let Some(preview) = placement_preview {
                                let status = if preview.valid { "Valid" } else { "Invalid" };
                                ui.label(format!(
                                    "Preview: {} at ({}, {}) {}x{}",
                                    status, preview.rx, preview.ry, preview.width, preview.height
                                ));
                                if let Some(reason) = &preview.reason {
                                    ui.label(egui::RichText::new(reason.label()).weak());
                                }
                            }
                            if ui.button("Clear Placement Mode").clicked() {
                                action = InGameHudAction::ClearPlacementMode;
                            }
                        } else {
                            ui.label(
                                egui::RichText::new("Select a ready structure to place.").weak(),
                            );
                        }
                        for ready in ready_buildings {
                            let ready_type_str = resolve(ready.type_id);
                            let is_armed = armed_building
                                .map(|type_id| type_id.eq_ignore_ascii_case(&ready_type_str))
                                .unwrap_or(false);
                            let label = if is_armed {
                                format!(
                                    "{} ({}) [armed]",
                                    ready.display_name,
                                    ready.queue_category.label()
                                )
                            } else {
                                format!("{} ({})", ready.display_name, ready.queue_category.label())
                            };
                            if ui.button(label).clicked() {
                                action = if is_armed {
                                    InGameHudAction::ClearPlacementMode
                                } else {
                                    InGameHudAction::ArmPlacement(ready_type_str.clone())
                                };
                            }
                        }
                    }
                    if !queue_items.is_empty() {
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new("Production").strong());
                        let mut current_queue_category: Option<ProductionCategory> = None;
                        for (i, item) in queue_items.iter().enumerate() {
                            if current_queue_category != Some(item.queue_category) {
                                current_queue_category = Some(item.queue_category);
                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(item.queue_category.label()).strong(),
                                    );
                                    if let Some(producer) = producer_focus
                                        .iter()
                                        .find(|focus| focus.category == item.queue_category)
                                    {
                                        ui.label(format!(
                                            "Factory: {} @ ({},{})",
                                            producer.display_name, producer.rx, producer.ry
                                        ));
                                        if ui.button("Cycle").clicked() {
                                            action =
                                                InGameHudAction::CycleProducer(item.queue_category);
                                        }
                                    }
                                    let pause_label = if item.state == BuildQueueState::Paused {
                                        "Resume"
                                    } else {
                                        "Pause"
                                    };
                                    if ui.button(pause_label).clicked() {
                                        action =
                                            InGameHudAction::TogglePauseQueue(item.queue_category);
                                    }
                                });
                            }
                            let done = (item.total_ms.saturating_sub(item.remaining_ms)) as f32;
                            let frac = (done / item.total_ms as f32).clamp(0.0, 1.0);
                            let seconds = (item.remaining_ms as f32 / 1000.0).max(0.0);
                            ui.label(format!(
                                "{}. {} [{}] ({:.1}s)",
                                i + 1,
                                item.display_name,
                                item.state.label(),
                                seconds
                            ));
                            ui.add(egui::ProgressBar::new(frac).desired_width(240.0));
                        }
                        if ui.button("Cancel last").clicked() {
                            action = InGameHudAction::CancelLastBuild;
                        }
                    }
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("Build Palette").strong());
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            let mut current_category: Option<ProductionCategory> = None;
                            for opt in options {
                                if current_category != Some(opt.queue_category) {
                                    current_category = Some(opt.queue_category);
                                    ui.add_space(4.0);
                                    ui.label(
                                        egui::RichText::new(opt.queue_category.label()).strong(),
                                    );
                                }
                                let label = format!("{} ({})", opt.display_name, opt.cost);
                                let mut response =
                                    ui.add_enabled(opt.enabled, egui::Button::new(label));
                                if let Some(reason) = &opt.reason {
                                    response = response.on_hover_text(disabled_reason_text(reason));
                                }
                                if response.clicked() && opt.enabled {
                                    action = InGameHudAction::BuildType(resolve(opt.type_id));
                                }
                            }
                        });
                    if options.is_empty() {
                        ui.label(egui::RichText::new("No buildable definitions found.").weak());
                    } else if options.iter().all(|o| !o.enabled) {
                        if options
                            .iter()
                            .any(|o| matches!(o.reason, Some(BuildDisabledReason::NoFactory)))
                        {
                            ui.label(
                                egui::RichText::new(
                                    "No production building. Use Place Starter Base first.",
                                )
                                .weak(),
                            );
                        }
                    }
                    ui.label(egui::RichText::new("Shortcut: B").weak());
                });
        });
    action
}
