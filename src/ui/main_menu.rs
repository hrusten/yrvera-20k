//! Main menu screen for skirmish setup and loading.
//!
//! Uses egui for a pragmatic client shell rather than pixel-perfect RA2 chrome.

use crate::{app_init::MapMenuEntry, ui::client_theme};

/// Action returned by the main menu to the app orchestrator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    /// No action this frame.
    None,
    /// User clicked "Start" for the selected map.
    StartSelected,
    /// User clicked "Exit".
    Exit,
}

/// Player's chosen faction side for skirmish games.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkirmishSide {
    #[default]
    Allied,
    Soviet,
}

impl SkirmishSide {
    pub fn label(self) -> &'static str {
        match self {
            Self::Allied => "Allied",
            Self::Soviet => "Soviet",
        }
    }

    pub const ALL: [SkirmishSide; 2] = [Self::Allied, Self::Soviet];
}

/// Individual country selection for skirmish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkirmishCountry {
    #[default]
    America,
    Korea,
    France,
    Germany,
    GreatBritain,
    Libya,
    Iraq,
    Cuba,
    Russia,
    Yuri,
}

impl SkirmishCountry {
    pub const ALL: [SkirmishCountry; 10] = [
        Self::America,
        Self::Korea,
        Self::France,
        Self::Germany,
        Self::GreatBritain,
        Self::Libya,
        Self::Iraq,
        Self::Cuba,
        Self::Russia,
        Self::Yuri,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::America => "America",
            Self::Korea => "Korea",
            Self::France => "France",
            Self::Germany => "Germany",
            Self::GreatBritain => "Great Britain",
            Self::Libya => "Libya",
            Self::Iraq => "Iraq",
            Self::Cuba => "Cuba",
            Self::Russia => "Russia",
            Self::Yuri => "Yuri",
        }
    }

    pub fn side(self) -> SkirmishSide {
        match self {
            Self::America | Self::Korea | Self::France | Self::Germany | Self::GreatBritain => {
                SkirmishSide::Allied
            }
            Self::Libya | Self::Iraq | Self::Cuba | Self::Russia | Self::Yuri => {
                SkirmishSide::Soviet
            }
        }
    }

    pub fn country_name(self) -> &'static str {
        match self {
            Self::America => "Americans",
            Self::Korea => "Alliance",
            Self::France => "French",
            Self::Germany => "Germans",
            Self::GreatBritain => "British",
            Self::Libya => "Africans",
            Self::Iraq => "Arabs",
            Self::Cuba => "Confederation",
            Self::Russia => "Russians",
            Self::Yuri => "YuriCountry",
        }
    }
}

/// Player's chosen start position on the map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartPosition {
    /// Automatic; let the game route through spawn picking.
    Auto,
    /// Specific waypoint index.
    Position(u8),
}

impl Default for StartPosition {
    fn default() -> Self {
        StartPosition::Position(0)
    }
}

/// Available starting credit amounts for the skirmish dropdown.
pub const CREDITS_OPTIONS: [i32; 10] = [
    100_000, 50_000, 30_000, 25_000, 20_000, 15_000, 10_000, 7_500, 5_000, 2_500,
];

/// Default starting credits index in `CREDITS_OPTIONS` (10,000).
const DEFAULT_CREDITS_IDX: usize = 6;

/// All configurable skirmish options, set in the main menu before launch.
#[derive(Debug, Clone)]
pub struct SkirmishSettings {
    pub selected_map_idx: usize,
    pub player_country: SkirmishCountry,
    pub ai_country: SkirmishCountry,
    pub starting_credits: i32,
    pub start_position: StartPosition,
    pub short_game: bool,
    /// Allow mouse-wheel zoom in-game (zoom in/out the battlefield).
    pub zoom_enabled: bool,
}

impl Default for SkirmishSettings {
    fn default() -> Self {
        Self {
            selected_map_idx: 0,
            player_country: SkirmishCountry::default(),
            ai_country: SkirmishCountry::Russia,
            starting_credits: CREDITS_OPTIONS[DEFAULT_CREDITS_IDX],
            start_position: StartPosition::default(),
            short_game: true,
            zoom_enabled: true,
        }
    }
}

const BUTTON_WIDTH: f32 = 400.0;
const BUTTON_HEIGHT: f32 = 54.0;

/// Draw the basic main menu without any map metadata.
pub fn draw_main_menu(ctx: &egui::Context) -> MenuAction {
    let mut settings = SkirmishSettings::default();
    draw_main_menu_with_maps(ctx, &[], &mut settings)
}

/// Draw the main menu with map selector and credits.
pub fn draw_main_menu_with_maps(
    ctx: &egui::Context,
    maps: &[MapMenuEntry],
    settings: &mut SkirmishSettings,
) -> MenuAction {
    let palette = client_theme::apply_client_theme(ctx);
    let mut action = MenuAction::None;
    let button_size = egui::vec2(BUTTON_WIDTH, BUTTON_HEIGHT);
    let has_maps = !maps.is_empty();

    if has_maps && settings.selected_map_idx >= maps.len() {
        settings.selected_map_idx = 0;
    }

    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(palette.bg))
        .show(ctx, |ui| {
            client_theme::paint_background(ui, palette);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(24.0);
                    ui.horizontal(|ui| {
                        ui.add_space(24.0);
                        ui.vertical(|ui| {
                            controls_panel(
                                ui,
                                maps,
                                settings,
                                button_size,
                                has_maps,
                                palette,
                                &mut action,
                            );
                        });
                        ui.add_space(24.0);
                    });
                    ui.add_space(24.0);
                });
        });

    action
}

fn controls_panel(
    ui: &mut egui::Ui,
    maps: &[MapMenuEntry],
    settings: &mut SkirmishSettings,
    button_size: egui::Vec2,
    has_maps: bool,
    palette: client_theme::ClientPalette,
    action: &mut MenuAction,
) {
    client_theme::card_frame(palette.panel, palette.line).show(ui, |ui| {
        ui.vertical(|ui| {
            ui.add_space(4.0);
            labeled_map_combo(ui, maps, settings, palette);
            ui.add_space(6.0);
            labeled_credits_combo(ui, settings, palette);

            ui.add_space(8.0);
            ui.checkbox(
                &mut settings.zoom_enabled,
                egui::RichText::new("Allow Zoom")
                    .size(16.0)
                    .color(palette.text),
            );

            ui.add_space(18.0);
            if ui
                .add_enabled(
                    has_maps,
                    egui::Button::new(egui::RichText::new("Start Game").size(22.0).strong())
                        .min_size(button_size),
                )
                .clicked()
            {
                *action = MenuAction::StartSelected;
            }
        });
    });
}

fn labeled_map_combo(
    ui: &mut egui::Ui,
    maps: &[MapMenuEntry],
    settings: &mut SkirmishSettings,
    palette: client_theme::ClientPalette,
) {
    client_theme::section_label(ui, "MAP", palette);
    let selected_label = maps
        .get(settings.selected_map_idx)
        .map(|map| map.display_name.as_str())
        .unwrap_or("(no maps found)");

    egui::ComboBox::from_id_salt("map_select")
        .width(BUTTON_WIDTH)
        .selected_text(selected_label)
        .show_ui(ui, |ui| {
            for (idx, map) in maps.iter().enumerate() {
                let label = match map.author.as_deref() {
                    Some(author) if !author.trim().is_empty() => {
                        format!("{}  -  {}", map.display_name, author)
                    }
                    _ => map.display_name.clone(),
                };
                ui.selectable_value(&mut settings.selected_map_idx, idx, label);
            }
        });
}

fn labeled_credits_combo(
    ui: &mut egui::Ui,
    settings: &mut SkirmishSettings,
    palette: client_theme::ClientPalette,
) {
    client_theme::section_label(ui, "STARTING CREDITS", palette);
    egui::ComboBox::from_id_salt("credits_select")
        .width(BUTTON_WIDTH)
        .selected_text(format!("{}", settings.starting_credits))
        .show_ui(ui, |ui| {
            for &amount in &CREDITS_OPTIONS {
                ui.selectable_value(
                    &mut settings.starting_credits,
                    amount,
                    format!("{}", amount),
                );
            }
        });
}

/// Draw the loading screen shown while map data is being parsed.
pub fn draw_loading_screen(ctx: &egui::Context, map_name: &str) {
    let palette = client_theme::apply_client_theme(ctx);

    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(palette.bg))
        .show(ctx, |ui| {
            client_theme::paint_background(ui, palette);
            let panel = ui.max_rect();

            if let Some(texture) = loading_screen_texture(ctx) {
                let image_size = texture.size_vec2();
                let scale = (panel.width() / image_size.x).min(panel.height() / image_size.y);
                let desired = image_size * (scale * 0.96);
                let image_rect = egui::Align2::CENTER_CENTER.align_size_within_rect(desired, panel);
                ui.put(image_rect, egui::Image::new((texture.id(), desired)));
                ui.painter().rect_filled(
                    image_rect.expand(2.0),
                    20.0,
                    egui::Color32::from_rgba_unmultiplied(0, 0, 0, 36),
                );
            }

            let overlay_size = egui::vec2(430.0, 132.0);
            let overlay_rect = egui::Rect::from_min_size(
                egui::pos2(panel.left() + 28.0, panel.bottom() - overlay_size.y - 28.0),
                overlay_size,
            );
            ui.painter().rect_filled(
                overlay_rect,
                18.0,
                egui::Color32::from_rgba_premultiplied(9, 14, 20, 214),
            );
            ui.painter().rect_stroke(
                overlay_rect,
                18.0,
                egui::Stroke::new(1.0, palette.line.gamma_multiply(0.85)),
                egui::StrokeKind::Middle,
            );

            let text_origin = overlay_rect.min + egui::vec2(18.0, 16.0);
            ui.painter().text(
                text_origin,
                egui::Align2::LEFT_TOP,
                "Mission deployment",
                egui::FontId::proportional(14.0),
                palette.accent,
            );
            ui.painter().text(
                text_origin + egui::vec2(0.0, 24.0),
                egui::Align2::LEFT_TOP,
                "Loading...",
                egui::FontId::proportional(32.0),
                palette.text,
            );
            ui.painter().text(
                text_origin + egui::vec2(0.0, 68.0),
                egui::Align2::LEFT_TOP,
                format!("Map: {}", map_name),
                egui::FontId::proportional(16.0),
                palette.text_muted,
            );
            ui.painter().text(
                text_origin + egui::vec2(0.0, 92.0),
                egui::Align2::LEFT_TOP,
                "Parsing map, spawning actors, and preparing assets.",
                egui::FontId::proportional(14.0),
                palette.text_muted,
            );
        });
}

fn loading_screen_texture(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let texture_id = egui::Id::new("loading_screen_texture");
    if let Some(existing) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(texture_id)) {
        return Some(existing);
    }

    let image = loading_screen_image()?;
    let texture = ctx.load_texture(
        "loading_screen_texture",
        image.clone(),
        egui::TextureOptions::LINEAR,
    );
    ctx.data_mut(|d| d.insert_temp(texture_id, texture.clone()));
    Some(texture)
}

fn loading_screen_image() -> Option<&'static egui::ColorImage> {
    // Loading screen image removed — the caller gracefully handles None.
    None
}
