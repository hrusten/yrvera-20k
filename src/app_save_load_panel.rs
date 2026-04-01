//! Save/load panel — egui overlay for managing save files.
//!
//! Scans the `saves/` directory, reads snapshot headers (without deserializing
//! the full Simulation), and displays a scrollable list. The player can load
//! or delete saves from here.
//!
//! The directory scan is cached — it only runs when the panel first opens or
//! after a save/delete invalidates the cache, not every frame.
//!
//! ## Dependency rules
//! - Part of the app layer — may depend on sim/snapshot for header parsing.

use crate::sim::snapshot::{GameSnapshot, GameSnapshotHeader};
use crate::ui::client_theme;

const SAVES_DIR: &str = "saves";

/// One row in the save file list.
pub(crate) struct SaveEntry {
    /// Absolute path to the .bin file.
    pub path: std::path::PathBuf,
    /// Parsed header metadata.
    pub header: GameSnapshotHeader,
    /// File size in bytes.
    pub file_size: u64,
}

/// Cached save-file listing. Stored in `AppState` so the directory is only
/// scanned when explicitly invalidated (panel open, save, delete).
pub(crate) struct SaveListCache {
    pub entries: Vec<SaveEntry>,
    /// When true, the next `draw_save_load_panel` call will rescan before rendering.
    pub dirty: bool,
}

impl SaveListCache {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            dirty: true,
        }
    }

    /// Mark the cache as needing a rescan on next render.
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// Rescan if dirty, then clear the flag.
    pub fn refresh_if_dirty(&mut self) {
        if self.dirty {
            self.entries = scan_saves();
            self.dirty = false;
        }
    }
}

/// Action produced by the save/load panel each frame.
pub(crate) enum SaveLoadAction {
    /// Load the save at this path.
    Load(std::path::PathBuf),
    /// Delete the save at this path.
    Delete(std::path::PathBuf),
    /// Close the panel.
    Close,
    /// No action.
    None,
}

/// Scan the saves directory and collect entries with valid headers.
fn scan_saves() -> Vec<SaveEntry> {
    let Ok(dir) = std::fs::read_dir(SAVES_DIR) else {
        return Vec::new();
    };
    let mut entries: Vec<SaveEntry> = Vec::new();
    for item in dir {
        let Ok(item) = item else { continue };
        let path = item.path();
        if path.extension().and_then(|e| e.to_str()) != Some("bin") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let file_size = bytes.len() as u64;
        let Ok(header) = GameSnapshot::read_header(&bytes) else {
            continue;
        };
        entries.push(SaveEntry {
            path,
            header,
            file_size,
        });
    }
    // Most recent first.
    entries.sort_by(|a, b| b.header.save_timestamp.cmp(&a.header.save_timestamp));
    entries
}

/// Format a unix timestamp as a human-readable relative string.
fn format_timestamp(unix_secs: u64) -> String {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let time = UNIX_EPOCH + Duration::from_secs(unix_secs);
    let now = SystemTime::now();
    if let Ok(elapsed) = now.duration_since(time) {
        let secs = elapsed.as_secs();
        if secs < 60 {
            return format!("{secs}s ago");
        }
        let mins = secs / 60;
        if mins < 60 {
            return format!("{mins}m ago");
        }
        let hours = mins / 60;
        if hours < 24 {
            return format!("{hours}h {}m ago", mins % 60);
        }
        let days = hours / 24;
        return format!("{days}d {}h ago", hours % 24);
    }
    format!("timestamp {unix_secs}")
}

/// Format byte size in a human-friendly way.
fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Draw the save/load panel. Returns an action for the caller to execute.
///
/// The caller must pass `&mut SaveListCache` so the panel can refresh once
/// on open rather than scanning the filesystem every frame.
pub(crate) fn draw_save_load_panel(
    ctx: &egui::Context,
    cache: &mut SaveListCache,
) -> SaveLoadAction {
    cache.refresh_if_dirty();

    let palette = client_theme::apply_client_theme(ctx);
    let mut action = SaveLoadAction::None;

    // Semi-transparent backdrop.
    egui::Area::new("saveload_backdrop".into())
        .fixed_pos(egui::pos2(0.0, 0.0))
        .interactable(false)
        .show(ctx, |ui| {
            let screen = ctx.content_rect();
            ui.painter().rect_filled(
                screen,
                0.0,
                egui::Color32::from_rgba_unmultiplied(0, 0, 0, 120),
            );
        });

    egui::Window::new("")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .frame(client_theme::card_frame(palette.panel, palette.line))
        .min_width(500.0)
        .max_height(500.0)
        .show(ctx, |ui| {
            ui.set_max_width(500.0);
            ui.vertical(|ui| {
                client_theme::section_label(ui, "SAVES", palette);
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("Save / Load")
                        .size(28.0)
                        .strong()
                        .color(palette.text),
                );
                ui.label(
                    egui::RichText::new("Press M to quicksave, or click a row to load.")
                        .size(13.0)
                        .color(palette.text_muted),
                );

                ui.add_space(12.0);

                if cache.entries.is_empty() {
                    ui.add_space(20.0);
                    ui.label(
                        egui::RichText::new("No saves found. Press M to create one.")
                            .size(14.0)
                            .color(palette.text_muted),
                    );
                    ui.add_space(20.0);
                } else {
                    // Header row.
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Map")
                                .size(12.0)
                                .strong()
                                .color(palette.text_muted),
                        );
                        ui.add_space(80.0);
                        ui.label(
                            egui::RichText::new("Tick")
                                .size(12.0)
                                .strong()
                                .color(palette.text_muted),
                        );
                        ui.add_space(40.0);
                        ui.label(
                            egui::RichText::new("Saved")
                                .size(12.0)
                                .strong()
                                .color(palette.text_muted),
                        );
                        ui.add_space(40.0);
                        ui.label(
                            egui::RichText::new("Size")
                                .size(12.0)
                                .strong()
                                .color(palette.text_muted),
                        );
                    });
                    ui.add_space(4.0);
                    ui.separator();

                    // Scrollable list of saves.
                    egui::ScrollArea::vertical()
                        .max_height(350.0)
                        .show(ui, |ui| {
                            for entry in &cache.entries {
                                let row_id = egui::Id::new(&entry.path);
                                let resp = ui
                                    .push_id(row_id, |ui| {
                                        ui.horizontal(|ui| {
                                            // Map name (truncated).
                                            let map_label = if entry.header.map_name.len() > 18 {
                                                format!("{}...", &entry.header.map_name[..18])
                                            } else {
                                                entry.header.map_name.clone()
                                            };
                                            ui.add_sized(
                                                egui::vec2(120.0, 20.0),
                                                egui::Label::new(
                                                    egui::RichText::new(map_label)
                                                        .size(13.0)
                                                        .color(palette.text),
                                                ),
                                            );

                                            // Tick number.
                                            ui.add_sized(
                                                egui::vec2(70.0, 20.0),
                                                egui::Label::new(
                                                    egui::RichText::new(format!(
                                                        "{}",
                                                        entry.header.tick
                                                    ))
                                                    .size(13.0)
                                                    .color(palette.text),
                                                ),
                                            );

                                            // Timestamp.
                                            ui.add_sized(
                                                egui::vec2(100.0, 20.0),
                                                egui::Label::new(
                                                    egui::RichText::new(format_timestamp(
                                                        entry.header.save_timestamp,
                                                    ))
                                                    .size(13.0)
                                                    .color(palette.text_muted),
                                                ),
                                            );

                                            // File size.
                                            ui.add_sized(
                                                egui::vec2(70.0, 20.0),
                                                egui::Label::new(
                                                    egui::RichText::new(format_size(
                                                        entry.file_size,
                                                    ))
                                                    .size(13.0)
                                                    .color(palette.text_muted),
                                                ),
                                            );

                                            // Load button.
                                            if ui
                                                .add_sized(
                                                    egui::vec2(50.0, 22.0),
                                                    egui::Button::new(
                                                        egui::RichText::new("Load")
                                                            .size(12.0)
                                                            .color(palette.accent),
                                                    ),
                                                )
                                                .clicked()
                                            {
                                                action = SaveLoadAction::Load(entry.path.clone());
                                            }

                                            // Delete button.
                                            if ui
                                                .add_sized(
                                                    egui::vec2(20.0, 22.0),
                                                    egui::Button::new(
                                                        egui::RichText::new("X")
                                                            .size(12.0)
                                                            .color(palette.danger),
                                                    ),
                                                )
                                                .clicked()
                                            {
                                                action = SaveLoadAction::Delete(entry.path.clone());
                                            }
                                        });
                                    })
                                    .response;
                                // Subtle hover highlight.
                                if resp.hovered() {
                                    ui.painter().rect_filled(
                                        resp.rect,
                                        2.0,
                                        egui::Color32::from_rgba_unmultiplied(0, 0, 0, 10),
                                    );
                                }
                            }
                        });
                }

                ui.add_space(12.0);
                if ui
                    .add_sized(
                        egui::vec2(160.0, 36.0),
                        egui::Button::new(egui::RichText::new("Close").size(16.0).strong()),
                    )
                    .clicked()
                {
                    action = SaveLoadAction::Close;
                }
                ui.add_space(4.0);
            });
        });

    action
}
