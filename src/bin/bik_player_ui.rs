//! egui UI panels for the bik-player binary.
// Implementation in Tasks 34-37.

use crate::BikPlayerApp;
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

            // Dropdown of .bik assets discovered in loaded MIX archives.
            if !app.available_assets.is_empty() {
                let current = if app.source_name.is_empty() {
                    "— pick a .bik —".to_string()
                } else {
                    app.source_name.clone()
                };
                let mut picked: Option<String> = None;
                egui::ComboBox::from_label(format!("({} .bik)", app.available_assets.len()))
                    .selected_text(current)
                    .width(240.0)
                    .show_ui(ui, |ui| {
                        for name in &app.available_assets {
                            if ui
                                .selectable_label(app.source_name == *name, name)
                                .clicked()
                            {
                                picked = Some(name.clone());
                            }
                        }
                    });
                if let Some(name) = picked {
                    app.load_asset(&name);
                }
                ui.separator();
            }

            ui.label("MIX asset:");
            // Bind to the struct field, not a closure-local — otherwise user
            // input is reset on every egui repaint.
            let resp = ui.text_edit_singleline(&mut app.asset_name_input);
            if resp.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                && !app.asset_name_input.is_empty()
            {
                let name = app.asset_name_input.clone();
                app.load_asset(&name);
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
