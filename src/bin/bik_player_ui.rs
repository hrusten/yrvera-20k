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
