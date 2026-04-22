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
        let Some(file) = &app.file else {
            return;
        };
        let n = file.frame_index.len();
        if n == 0 {
            return;
        }

        let mut idx = app.current_frame.min(n - 1);
        let slider_resp = ui.add(
            egui::Slider::new(&mut idx, 0..=(n - 1)).text(format!("frame / {}", n)),
        );
        if slider_resp.changed() {
            app.current_frame = idx;
            // Seek: just re-decode from most recent keyframe up to idx.
            // Implementation in Task 38.
        }

        // Keyframe markers (simple overlay).
        let bar_rect = slider_resp.rect;
        let painter = ui.painter();
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
