//! egui UI panels for the bik-player binary.
// Implementation in Tasks 34-37.

use crate::{BikPlayerApp, PickerEntry};
use eframe::egui;

pub fn draw_top_panel(app: &mut BikPlayerApp, ctx: &egui::Context) {
    egui::TopBottomPanel::top("top").show(ctx, |ui| {
        ui.horizontal(|ui| {
            if ui.button("Open .bik…").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Bink video", &["bik", "BIK"])
                    .pick_file()
                {
                    app.load_path(&path);
                }
            }
            ui.separator();

            // Dropdown of every physical .bik entry in every loaded MIX archive.
            if !app.available_entries.is_empty() {
                let current = if app.source_name.is_empty() {
                    "— pick a .bik —".to_string()
                } else {
                    app.source_name.clone()
                };
                let mut picked: Option<PickerEntry> = None;
                egui::ComboBox::from_label(format!("({} .bik entries)", app.available_entries.len()))
                    .selected_text(current)
                    .width(320.0)
                    .show_ui(ui, |ui| {
                        for entry in &app.available_entries {
                            if ui
                                .selectable_label(app.source_name == entry.display, &entry.display)
                                .clicked()
                            {
                                picked = Some(entry.clone());
                            }
                        }
                    });
                if let Some(entry) = picked {
                    app.load_picker_entry(&entry);
                }
                ui.separator();
            }

            ui.label("Vol");
            let mut v = app.audio_volume;
            if ui.add(egui::Slider::new(&mut v, 0.0..=1.0).show_value(false)).changed() {
                app.audio_volume = v;
                if let Some(sink) = app.audio_sink.as_ref() {
                    sink.set_volume(v);
                }
            }
            if ui
                .button(if app.audio_volume > 0.0 { "Mute" } else { "Unmute" })
                .clicked()
            {
                if app.audio_volume > 0.0 {
                    app.audio_volume = 0.0;
                } else {
                    app.audio_volume = 0.7;
                }
                if let Some(sink) = app.audio_sink.as_ref() {
                    sink.set_volume(app.audio_volume);
                }
            }
        });
        ui.label(&app.status);
    });
}

pub fn draw_timeline(app: &mut BikPlayerApp, ctx: &egui::Context) {
    egui::TopBottomPanel::bottom("timeline").show(ctx, |ui| {
        // Read frame count + keyframe indices up front so we don't hold a
        // borrow on `app.file` while seek_to_frame needs `&mut app`.
        let n = match &app.file {
            Some(f) => f.frame_index.len(),
            None => return,
        };
        if n == 0 {
            return;
        }

        let mut idx = app.current_frame.min(n - 1);
        let slider_resp = ui.add(
            egui::Slider::new(&mut idx, 0..=(n - 1)).text(format!("frame / {}", n)),
        );
        if slider_resp.changed() {
            if let Err(e) = crate::bik_player_playback::seek_to_frame(app, idx) {
                app.status = format!("seek error: {}", e);
            }
        }

        // Keyframe markers (simple overlay).
        let bar_rect = slider_resp.rect;
        let painter = ui.painter();
        let Some(file) = &app.file else {
            return;
        };
        for (i, entry) in file.frame_index.iter().enumerate() {
            if !entry.is_keyframe {
                continue;
            }
            let t = i as f32 / (n.saturating_sub(1).max(1) as f32);
            let x = bar_rect.left() + t * bar_rect.width();
            painter.line_segment(
                [egui::pos2(x, bar_rect.top()), egui::pos2(x, bar_rect.bottom())],
                egui::Stroke::new(1.0, egui::Color32::from_rgb(0, 160, 0)),
            );
        }
    });
}
