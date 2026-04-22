//! Standalone Bink 1 video player for RA2/YR cutscenes.
//!
//! Usage:
//!   cargo run --bin bik-player <path-or-asset-name>
//!
//! If the argument is a filesystem path it's loaded directly; otherwise it's
//! looked up via AssetManager (MOVIES*.MIX + movmd03.mix).

mod bik_player_playback;
mod bik_player_ui;

use eframe::egui;

fn main() -> Result<(), eframe::Error> {
    env_logger::init();
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([960.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        "vera20k bik-player",
        native_options,
        Box::new(|cc| Ok(Box::new(BikPlayerApp::new(cc)))),
    )
}

struct BikPlayerApp {
    // Task 34 fills in.
}

impl BikPlayerApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {}
    }
}

impl eframe::App for BikPlayerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("bik-player");
            ui.label("Task 33: scaffold only — UI in Task 34.");
        });
    }
}
