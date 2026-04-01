//! GUI browser for RA2 MIX archives and SHP assets.
//!
//! Usage: `cargo run --bin mix-browser`
//!
//! Supports browsing individual archives, previewing SHP sprites with smart
//! palette inference, scanning all archives for SHP entries, and searching
//! assets by filename.

mod mix_browser_csf;
mod mix_browser_data;
mod mix_browser_preview;
mod mix_browser_renderers;
mod mix_browser_ui;

use eframe::egui;
use mix_browser_data::{ShpIndex, build_best_dictionary, load_mix_contents};
use mix_browser_preview::PreviewState;
use vera20k::assets::asset_manager::AssetManager;
use vera20k::assets::pal_file::Palette;
use vera20k::rules::art_data::ArtRegistry;
use vera20k::rules::house_colors::{self, HouseColorIndex};
use vera20k::rules::ini_parser::IniFile;
use vera20k::util::config::GameConfig;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BrowserViewMode {
    Archive,
    AllShps,
}

/// Main application state for the MIX browser.
pub struct MixBrowserApp {
    pub archive_names: Vec<String>,
    pub sidebar_archive_names: Vec<String>,
    pub loaded: Vec<mix_browser_data::MixContents>,
    pub selected: usize,
    pub filter: String,
    pub dict: Vec<(String, i32)>,
    pub asset_manager: AssetManager,
    pub preview: Option<PreviewState>,
    pub preview_zoom: f32,
    pub asset_search: String,
    pub search_palette_name: String,
    pub view_mode: BrowserViewMode,
    pub shp_index: Option<ShpIndex>,
    pub csf_state: Option<mix_browser_csf::CsfBrowserState>,
    pub art_registry: ArtRegistry,
    /// All palette filenames found across archives (sorted, deduplicated).
    pub available_palettes: Vec<String>,
}

impl MixBrowserApp {
    fn new(asset_manager: AssetManager) -> Self {
        let (mut dict, xcc_loaded) = build_best_dictionary();
        if xcc_loaded {
            log::info!("XCC database loaded — enhanced filename resolution active");
        }
        // Expand dictionary with filenames from rules.ini / art.ini.
        mix_browser_data::expand_dictionary_from_ini(&mut dict, &asset_manager);

        // Build ArtRegistry from art.ini + artmd.ini for palette lookups.
        let art_registry = Self::build_art_registry(&asset_manager);
        let available_palettes = Self::collect_palettes(&asset_manager, &dict);
        let archive_names = asset_manager.loaded_archive_names();
        let sidebar_archive_names = Self::collect_sidebar_archives(&archive_names);

        // Default to sidebar chrome archives, or first 3 available.
        let mut default_mixes = Vec::new();
        default_mixes.extend(sidebar_archive_names.iter().take(4).cloned());
        if default_mixes.is_empty() {
            default_mixes.extend(archive_names.iter().take(3).cloned());
        }

        let loaded = default_mixes
            .iter()
            .map(|name| load_mix_contents(&asset_manager, name, &dict))
            .collect();

        Self {
            archive_names,
            sidebar_archive_names,
            loaded,
            selected: 0,
            filter: String::new(),
            dict,
            asset_manager,
            preview: None,
            preview_zoom: 2.0,
            asset_search: String::new(),
            search_palette_name: "unittem.pal".to_string(),
            view_mode: BrowserViewMode::Archive,
            shp_index: None,
            csf_state: None,
            art_registry,
            available_palettes,
        }
    }

    fn collect_sidebar_archives(archive_names: &[String]) -> Vec<String> {
        let mut names: Vec<String> = archive_names
            .iter()
            .filter(|name| {
                let lower = name.to_ascii_lowercase();
                lower.starts_with("sidec") && lower.ends_with(".mix")
            })
            .cloned()
            .collect();
        names.sort_by_key(|name| name.to_ascii_lowercase());
        names
    }

    /// Build an ArtRegistry from art.ini + artmd.ini loaded via AssetManager.
    fn build_art_registry(asset_manager: &AssetManager) -> ArtRegistry {
        let art_data = asset_manager.get("art.ini").unwrap_or_default();
        let Ok(mut art_ini) = IniFile::from_bytes(&art_data) else {
            return ArtRegistry::empty();
        };
        if let Some(artmd_data) = asset_manager.get("artmd.ini") {
            if let Ok(artmd) = IniFile::from_bytes(&artmd_data) {
                art_ini.merge(&artmd);
            }
        }
        let registry = ArtRegistry::from_ini(&art_ini);
        log::info!(
            "Mix-browser: ArtRegistry loaded with {} entries",
            registry.len()
        );
        registry
    }

    /// Collect all available palette names from archives and art.ini Palette= values.
    ///
    /// Two-pass approach:
    /// 1. Scan archives for 768-byte entries, resolve names via hash dictionary
    /// 2. Try loading known palette names (from art.ini + hardcoded list) by name
    fn collect_palettes(asset_manager: &AssetManager, dict: &[(String, i32)]) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();

        // Pass 1: scan archives for 768-byte entries with known hashes.
        asset_manager.visit_archives(|_source, archive| {
            for entry in archive.entries() {
                if entry.size != 768 {
                    continue;
                }
                let name = dict
                    .iter()
                    .find(|(_, h)| *h == entry.id)
                    .map(|(n, _)| n.clone());
                if let Some(name) = name {
                    names.push(name);
                }
            }
        });

        // Pass 2: try loading palette names extracted from art.ini Palette= values
        // and a broad list of known RA2/YR palette naming patterns.
        let extra_candidates = Self::palette_name_candidates(asset_manager);
        for candidate in extra_candidates {
            if let Some(data) = asset_manager.get(&candidate) {
                if data.len() == 768 {
                    names.push(candidate);
                }
            }
        }

        names.sort();
        names.dedup();
        log::info!("Found {} named palettes across archives", names.len());
        names
    }

    /// Generate a broad list of palette filename candidates to try loading.
    ///
    /// Includes art.ini Palette= values, theater variants, and common RA2 palettes.
    fn palette_name_candidates(asset_manager: &AssetManager) -> Vec<String> {
        let mut candidates: Vec<String> = Vec::new();

        // Extract Palette= values from art.ini / artmd.ini.
        for ini_name in ["art.ini", "artmd.ini"] {
            let Some(data) = asset_manager.get(ini_name) else {
                continue;
            };
            let text = String::from_utf8_lossy(&data);
            for line in text.lines() {
                let trimmed = line.trim();
                if let Some((key, value)) = trimmed.split_once('=') {
                    if key.trim().eq_ignore_ascii_case("Palette") {
                        let val = value.trim();
                        if !val.is_empty() {
                            // art.ini Palette= stores base name without extension.
                            candidates.push(format!("{}.pal", val.to_ascii_lowercase()));
                        }
                    }
                }
            }
        }

        // Common RA2/YR palette base names (theater variants generated below).
        let bases = [
            "unit", "iso", "temperat", "snow", "urban", "desert", "lunar", "newurban", "lib",
            "anim", "sidebar", "cameo", "mousepal", "grftxt", "theater", "load", "ls800", "ls640",
            "ls400",
        ];
        // Theater suffixes.
        let suffixes = ["tem", "sno", "urb", "des", "lun", "nurb", "ubn"];
        for base in bases {
            candidates.push(format!("{}.pal", base));
            for suffix in suffixes {
                candidates.push(format!("{}{}.pal", base, suffix));
            }
            candidates.push(format!("{}md.pal", base));
        }

        // Additional known palettes.
        for name in [
            "cameo.pal",
            "cameomd.pal",
            "sidebar.pal",
            "uibkgd.pal",
            "uibkgdy.pal",
            "radaryuri.pal",
            "mousepal.pal",
            "anim.pal",
            "lib.pal",
            "grftxt.pal",
            "unittem.pal",
            "unitsno.pal",
            "uniturb.pal",
            "unitdes.pal",
            "unitlun.pal",
            "isotem.pal",
            "isosno.pal",
            "isourb.pal",
            "isodes.pal",
            "isolun.pal",
            "isonurb.pal",
            "isotemmd.pal",
            "isosnomd.pal",
            "isourbmd.pal",
            "temperat.pal",
            "snow.pal",
            "urban.pal",
            "desert.pal",
            "lunar.pal",
            "newurban.pal",
            "neutral.pal",
            "generic.pal",
            // Loading screen palettes.
            "ls800bkg.pal",
            "ls640bkg.pal",
            "ls800.pal",
            "load.pal",
            "ls800bkgr.pal",
            "lsbkgr.pal",
            "loadscr.pal",
            // YR variants.
            "unittemmd.pal",
            "unitsnomd.pal",
            "uniturbmd.pal",
            "temperatmd.pal",
            "snowmd.pal",
            "urbanmd.pal",
        ] {
            candidates.push(name.to_string());
        }

        candidates.sort();
        candidates.dedup();
        candidates
    }

    /// Ensure an archive is loaded, returning its index in `self.loaded`.
    pub fn ensure_loaded(&mut self, name: &str) -> usize {
        if let Some(idx) = self
            .loaded
            .iter()
            .position(|c| c.mix_name.eq_ignore_ascii_case(name))
        {
            return idx;
        }
        let contents = load_mix_contents(&self.asset_manager, name, &self.dict);
        self.loaded.push(contents);
        self.loaded.len() - 1
    }

    pub fn select_archive(&mut self, name: &str) {
        let idx = self.ensure_loaded(name);
        self.selected = idx;
    }

    pub fn load_sidebar_archives(&mut self) {
        let names = self.sidebar_archive_names.clone();
        for name in names {
            self.ensure_loaded(&name);
        }
    }

    /// Build or rebuild the all-SHPs scan index.
    pub fn rebuild_shp_index(&mut self) {
        self.shp_index = Some(mix_browser_data::build_shp_index(
            &self.asset_manager,
            &self.dict,
        ));
    }

    /// Select an archive entry for preview.
    fn select_entry(
        &mut self,
        source_archive: &str,
        entry_hash: i32,
        hinted_name: Option<&str>,
        ctx: &egui::Context,
    ) {
        // Skip if already previewing this exact entry.
        if let Some(preview) = &self.preview {
            if preview.entry_hash == entry_hash && preview.source_name == source_archive {
                return;
            }
        }

        let Some(data) = self
            .asset_manager
            .archive_entry_data(source_archive, entry_hash)
        else {
            self.preview = Some(PreviewState::error(
                entry_hash,
                source_archive,
                hinted_name.unwrap_or("unknown"),
                "Could not read entry data".to_string(),
            ));
            return;
        };

        // Try CSF first — if it parses, show the string browser.
        if data.len() >= 24 && data[0..4] == [0x20, 0x53, 0x43, 0x46] {
            if let Some(csf) =
                mix_browser_csf::CsfBrowserState::from_bytes(&data, hinted_name.unwrap_or("csf"))
            {
                self.csf_state = Some(csf);
                self.preview = Some(PreviewState::error(
                    entry_hash,
                    source_archive,
                    hinted_name.unwrap_or("csf"),
                    String::new(),
                ));
                return;
            }
        }
        self.csf_state = None;

        self.preview = Some(mix_browser_preview::preview_from_bytes(
            &self.asset_manager,
            &self.dict,
            &self.art_registry,
            source_archive,
            source_archive,
            entry_hash,
            data,
            hinted_name,
            None,
            ctx,
        ));
    }

    /// Handle asset search from the toolbar.
    fn handle_search(&mut self, name: &str, palette_name: &str, ctx: &egui::Context) {
        let (preview, source) = mix_browser_preview::search_asset(
            &self.asset_manager,
            &self.dict,
            &self.art_registry,
            name,
            palette_name,
            ctx,
        );
        self.preview = Some(preview);
        if let Some(source) = source {
            let idx = self.ensure_loaded(&source);
            self.selected = idx;
        }
    }

    /// Apply house color remapping to the current SHP preview.
    fn apply_house_color(&mut self, color_idx: usize, ctx: &egui::Context) {
        let Some(preview) = &mut self.preview else {
            return;
        };
        preview.house_color_index = color_idx;

        let Some(shp) = &preview.shp else {
            return;
        };
        let Some(base_palette) = &preview.palette else {
            return;
        };

        // Apply house color remap if index > 0 (0 = no remap).
        let render_palette: Palette = if color_idx > 0 {
            let ramp = house_colors::house_color_ramp(HouseColorIndex((color_idx - 1) as u8));
            base_palette.with_house_colors(ramp)
        } else {
            base_palette.clone()
        };

        preview.texture = mix_browser_preview::render_shp_frame_texture(
            ctx,
            shp,
            preview.current_frame,
            &render_palette,
        );
    }

    /// Apply a user-selected palette to the current preview.
    fn apply_palette(&mut self, palette_name: &str, ctx: &egui::Context) {
        let Some(bytes) = self.asset_manager.get(palette_name) else {
            return;
        };
        let Ok(new_palette) = Palette::from_bytes(&bytes) else {
            return;
        };

        let Some(preview) = &mut self.preview else {
            return;
        };

        preview.palette = Some(new_palette.clone());
        preview.palette_name = Some(palette_name.to_string());

        // Re-render based on asset type.
        if let Some(shp) = &preview.shp {
            // Apply house color remap if active.
            let render_pal = if preview.house_color_index > 0 {
                let ramp = house_colors::house_color_ramp(HouseColorIndex(
                    (preview.house_color_index - 1) as u8,
                ));
                new_palette.with_house_colors(ramp)
            } else {
                new_palette
            };
            preview.texture = mix_browser_preview::render_shp_frame_texture(
                ctx,
                shp,
                preview.current_frame,
                &render_pal,
            );
        } else if let Some(raw) = &preview.raw_bytes {
            // TMP re-render.
            if preview.file_type.starts_with("TMP") {
                if let Ok(tmp) = vera20k::assets::tmp_file::TmpFile::from_bytes(raw) {
                    if let Some((image, _)) =
                        mix_browser_renderers::render_tmp_preview(&tmp, &new_palette)
                    {
                        preview.texture = Some(ctx.load_texture(
                            format!("tmp_preview_{}", preview.entry_hash),
                            image,
                            egui::TextureOptions::NEAREST,
                        ));
                    }
                }
            }
        }
    }

    /// Export the current preview texture to a PNG file.
    fn export_current_preview(&self) {
        let Some(preview) = &self.preview else {
            return;
        };
        let Some(texture) = &preview.texture else {
            log::warn!("No texture to export");
            return;
        };

        // Build filename from resolved name + frame number.
        let base_name = preview
            .resolved_name
            .replace(['/', '\\', '?', '*', '"', '<', '>', '|'], "_");
        let filename = if preview.frame_count > 1 {
            format!("{}_frame{}.png", base_name, preview.current_frame)
        } else {
            format!("{}.png", base_name)
        };

        // Read pixel data from the egui texture image.
        let size = texture.size();
        let width = size[0] as u32;
        let height = size[1] as u32;

        // Re-render the frame to get raw RGBA (textures don't expose pixels).
        let rgba: Vec<u8> = if let Some(shp) = &preview.shp {
            if let Some(palette) = &preview.palette {
                let pal = if preview.house_color_index > 0 {
                    let ramp = house_colors::house_color_ramp(HouseColorIndex(
                        (preview.house_color_index - 1) as u8,
                    ));
                    palette.with_house_colors(ramp)
                } else {
                    palette.clone()
                };
                let frame = &shp.frames[preview.current_frame];
                let w = shp.width as usize;
                let h = shp.height as usize;
                let mut buf = Vec::with_capacity(w * h * 4);
                for y in 0..h {
                    for x in 0..w {
                        let fx = x as i32 - frame.frame_x as i32;
                        let fy = y as i32 - frame.frame_y as i32;
                        let in_frame = fx >= 0
                            && fy >= 0
                            && fx < frame.frame_width as i32
                            && fy < frame.frame_height as i32;
                        if in_frame {
                            let idx = fy as usize * frame.frame_width as usize + fx as usize;
                            let pi = frame.pixels[idx];
                            if pi == 0 {
                                buf.extend_from_slice(&[0, 0, 0, 0]);
                            } else {
                                let c = pal.colors[pi as usize];
                                buf.extend_from_slice(&[c.r, c.g, c.b, 255]);
                            }
                        } else {
                            buf.extend_from_slice(&[0, 0, 0, 0]);
                        }
                    }
                }
                buf
            } else {
                return;
            }
        } else if let Some(raw) = &preview.raw_bytes {
            // PAL grid: re-render it.
            if raw.len() == 768 {
                if let Ok(pal) = vera20k::assets::pal_file::Palette::from_bytes(raw) {
                    let img = mix_browser_renderers::render_palette_grid(&pal);
                    // egui::ColorImage pixels are [u8; 4] per pixel.
                    img.pixels
                        .iter()
                        .flat_map(|c| [c.r(), c.g(), c.b(), c.a()])
                        .collect()
                } else {
                    return;
                }
            } else {
                return;
            }
        } else {
            return;
        };

        match image::save_buffer(&filename, &rgba, width, height, image::ColorType::Rgba8) {
            Ok(()) => log::info!("Exported {}", filename),
            Err(e) => log::error!("Export failed: {}", e),
        }
    }

    /// Tick animation playback: advance frame if enough time has elapsed.
    fn tick_animation(&mut self, ctx: &egui::Context) {
        let Some(preview) = &mut self.preview else {
            return;
        };
        if !preview.is_playing || preview.frame_count <= 1 {
            return;
        }

        let now = ctx.input(|i| i.time);
        let interval = 1.0 / preview.play_speed_fps as f64;
        if now - preview.last_frame_time >= interval {
            let next = (preview.current_frame + 1) % preview.frame_count;
            preview.last_frame_time = now;
            mix_browser_preview::set_preview_frame(preview, next, ctx);
        }
        ctx.request_repaint();
    }
}

impl eframe::App for MixBrowserApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Ensure SHP index is built when in AllShps mode.
        if self.view_mode == BrowserViewMode::AllShps && self.shp_index.is_none() {
            self.rebuild_shp_index();
        }

        // Tick animation playback.
        self.tick_animation(ctx);

        // Draw UI panels and collect interactions.
        let pending_search = mix_browser_ui::draw_toolbar(self, ctx);
        if let Some((name, palette_name)) = pending_search {
            self.handle_search(&name, &palette_name, ctx);
        }

        mix_browser_ui::draw_status_bar(self, ctx);

        let panel = mix_browser_ui::draw_preview_panel(
            &self.preview,
            self.preview_zoom,
            &self.available_palettes,
            ctx,
        );
        if let Some(frame) = panel.pending_frame {
            if let Some(preview) = &mut self.preview {
                mix_browser_preview::set_preview_frame(preview, frame, ctx);
            }
        }
        if panel.toggle_play {
            if let Some(preview) = &mut self.preview {
                preview.is_playing = !preview.is_playing;
                if preview.is_playing {
                    preview.last_frame_time = ctx.input(|i| i.time);
                }
            }
        }
        if let Some(fps) = panel.new_fps {
            if let Some(preview) = &mut self.preview {
                preview.play_speed_fps = fps;
            }
        }
        if let Some(color_idx) = panel.house_color_changed {
            self.apply_house_color(color_idx, ctx);
        }
        if let Some(ref pal_name) = panel.palette_changed {
            let name = pal_name.clone();
            self.apply_palette(&name, ctx);
        }
        if panel.export_png {
            self.export_current_preview();
        }

        // Central panel: CSF browser if active, otherwise archive/shp grid.
        if let Some(csf) = &mut self.csf_state {
            egui::CentralPanel::default().show(ctx, |ui| {
                mix_browser_csf::draw_csf_browser(csf, ui);
            });
        } else {
            let clicked = mix_browser_ui::draw_central_panel(self, ctx);
            if let Some((source, hash, hinted_name)) = clicked {
                self.select_entry(&source, hash, Some(&hinted_name), ctx);
            }
        }
    }
}

fn main() -> eframe::Result {
    let config = match GameConfig::load() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("Error: config.toml not found ({})", err);
            std::process::exit(1);
        }
    };

    let mut asset_manager = match AssetManager::new(&config.paths.ra2_dir) {
        Ok(manager) => manager,
        Err(err) => {
            eprintln!("Error: AssetManager init failed ({})", err);
            std::process::exit(1);
        }
    };
    if let Err(err) = asset_manager.load_all_disk_mixes() {
        eprintln!("Warning: could not scan extra disk MIX files ({})", err);
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RA2 MIX Browser")
            .with_inner_size([1350.0, 820.0]),
        ..Default::default()
    };

    eframe::run_native(
        "RA2 MIX Browser",
        options,
        Box::new(|_cc| Ok(Box::new(MixBrowserApp::new(asset_manager)))),
    )
}
