//! App init helper functions — map file loading, atlas building, rules/art loading,
//! skirmish seeding, overlay atlas construction.
//!
//! Extracted from app_init.rs for file-size limits.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use crate::assets::asset_manager::AssetManager;
use crate::assets::pal_file::Palette;
use crate::map::houses::HouseColorMap;
use crate::map::map_file::MapFile;
use crate::map::resolved_terrain::ResolvedTerrainGrid;
use crate::map::terrain::TerrainGrid;
use crate::map::theater::{self, TileImage, TileKey};
use crate::map::trigger_graph;
use crate::render::batch::BatchRenderer;
use crate::render::gpu::GpuContext;
use crate::render::sidebar_cameo_atlas::{self, SidebarCameoAtlas};
use crate::render::sprite_atlas::{self, SpriteAtlas};
use crate::render::tile_atlas::{self, TileAtlas};
use crate::render::unit_atlas::{self, UnitAtlas};
use crate::rules::art_data::ArtRegistry;
use crate::rules::ini_parser::IniFile;
use crate::rules::ruleset::RuleSet;
use crate::sim::world::Simulation;

use crate::app_skirmish::deployable_building_types;

pub(crate) fn build_sidebar_cameo_atlas(
    gpu: &GpuContext,
    batch: &BatchRenderer,
    asset_manager: &AssetManager,
    rules: Option<&RuleSet>,
    art: Option<&ArtRegistry>,
) -> Option<SidebarCameoAtlas> {
    let rules = rules?;
    maybe_export_sidebar_cameo_debug(asset_manager, rules, art);
    let palette = load_sidebar_cameo_palette(asset_manager)?;
    sidebar_cameo_atlas::build_sidebar_cameo_atlas(gpu, batch, asset_manager, rules, art, &palette)
}

pub(crate) fn maybe_export_sidebar_cameo_debug(
    asset_manager: &AssetManager,
    rules: &RuleSet,
    art: Option<&ArtRegistry>,
) {
    let enabled = std::env::var("RA2_DEBUG_CAMEO_PALETTES")
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(false);
    if !enabled {
        return;
    }

    let palette_names = [
        "cameo.pal",
        "cameomd.pal",
        "mousepal.pal",
        "anim.pal",
        "unittem.pal",
        "unit.pal",
        "temperat.pal",
        "isotem.pal",
    ];
    sidebar_cameo_atlas::export_debug_palette_sheet(
        asset_manager,
        rules,
        art,
        Path::new("debug_sidebar_cameo_palettes.png"),
        &palette_names,
    );
}

pub(crate) fn load_sidebar_cameo_palette(asset_manager: &AssetManager) -> Option<Palette> {
    let palette_names = [
        "cameo.pal",
        "cameomd.pal",
        "mousepal.pal",
        "anim.pal",
        "unittem.pal",
        "unit.pal",
        "temperat.pal",
    ];
    for name in palette_names {
        if let Some(data) = asset_manager.get_ref(name) {
            if let Ok(palette) = Palette::from_bytes(data) {
                log::info!("Sidebar cameos using palette {}", name);
                return Some(palette);
            }
        }
    }
    log::warn!("Sidebar cameo palette not found");
    None
}

pub(crate) fn log_trigger_graph_diagnostics(map_data: &MapFile) {
    let diag = trigger_graph::analyze_trigger_graph(
        &map_data.cell_tags,
        &map_data.tags,
        &map_data.triggers,
        &map_data.events,
        &map_data.actions,
    );
    if diag.cell_tags_total == 0
        && diag.tags_total == 0
        && diag.triggers_total == 0
        && map_data.events.is_empty()
        && map_data.actions.is_empty()
    {
        return;
    }

    log::info!(
        "Trigger graph: cell_tags={}/{} resolved, tags={}/{} trigger refs resolved, triggers={} events={} actions={}",
        diag.cell_tags_resolved,
        diag.cell_tags_total,
        diag.tags_resolved_to_triggers,
        diag.tags_with_trigger_ref,
        diag.triggers_total,
        diag.triggers_with_event,
        diag.triggers_with_action
    );
    if !diag.dangling_cell_tags.is_empty() {
        log::warn!(
            "Trigger graph dangling cell tags (first 8): {:?}",
            &diag.dangling_cell_tags[..diag.dangling_cell_tags.len().min(8)]
        );
    }
    if !diag.dangling_tag_trigger_refs.is_empty() {
        log::warn!(
            "Trigger graph dangling tag->trigger refs (first 8): {:?}",
            &diag.dangling_tag_trigger_refs[..diag.dangling_tag_trigger_refs.len().min(8)]
        );
    }
    if !diag.triggers_missing_event.is_empty() {
        log::warn!(
            "Trigger graph triggers missing events (first 8): {:?}",
            &diag.triggers_missing_event[..diag.triggers_missing_event.len().min(8)]
        );
    }
    if !diag.triggers_missing_action.is_empty() {
        log::warn!(
            "Trigger graph triggers missing actions (first 8): {:?}",
            &diag.triggers_missing_action[..diag.triggers_missing_action.len().min(8)]
        );
    }
}

pub(crate) fn parse_debug_spawn_units_env() -> Option<Vec<String>> {
    let raw = std::env::var("RA2_DEBUG_SPAWN_UNITS").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let enabled_tokens = ["1", "true", "yes", "on"];
    if enabled_tokens
        .iter()
        .any(|v| trimmed.eq_ignore_ascii_case(v))
    {
        return Some(vec![
            "HTNK".to_string(),
            "MTNK".to_string(),
            "E1".to_string(),
        ]);
    }
    let items: Vec<String> = trimmed
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    if items.is_empty() { None } else { Some(items) }
}

/// Build a texture atlas from pre-loaded theater data and the terrain grid.
pub(crate) fn build_tile_atlas(
    asset_manager: &AssetManager,
    lookup: &theater::TilesetLookup,
    palette: &Palette,
    _ext: &str,
    grid: &TerrainGrid,
    gpu: &GpuContext,
    batch: &BatchRenderer,
) -> Option<TileAtlas> {
    let cell_pairs: Vec<(i32, u8)> = grid
        .cells
        .iter()
        .map(|c| (c.tile_id as i32, c.sub_tile))
        .collect();
    let mut needed: HashSet<TileKey> = theater::collect_used_tiles(&cell_pairs);
    // Always include tile_id 0 (clear ground) — used as fallback for missing tiles.
    needed.insert(TileKey {
        tile_id: 0,
        sub_tile: 0,
        variant: 0,
    });
    log::info!("Map uses {} unique tile keys", needed.len());

    let images: HashMap<TileKey, TileImage> =
        theater::load_tile_images(asset_manager, lookup, palette, &needed);
    if images.is_empty() {
        log::warn!("No tile images loaded — falling back to single tile");
        return None;
    }

    let atlas: TileAtlas = tile_atlas::build_atlas(gpu, batch, &images);
    log::info!("Atlas built: {} tiles", atlas.tile_count());
    Some(atlas)
}

/// Fallback theater extension from theater name when load_theater fails.
pub(crate) fn theater_ext_for(theater_name: &str) -> &'static str {
    match theater_name.to_uppercase().as_str() {
        "TEMPERATE" => "tem",
        "SNOW" => "sno",
        "URBAN" => "urb",
        "DESERT" => "des",
        "LUNAR" => "lun",
        "NEWURBAN" => "ubn",
        _ => "tem",
    }
}

/// Load rules.ini from MIX archives and parse into RuleSet.
///
/// In YR, rulesmd.ini is a PATCH on top of rules.ini — it only contains
/// the changes/additions that Yuri's Revenge makes. We must load rules.ini
/// first as the base, then merge rulesmd.ini on top. Without this merge,
/// buildings are missing key properties like Foundation sizes.
pub(crate) fn load_rules_ini(asset_manager: &AssetManager) -> Option<RuleSet> {
    // Step 1: Load base rules.ini.
    let mut ini: IniFile = if let Some((data, source)) = asset_manager.get_with_source("rules.ini")
    {
        log::info!(
            "Loading rules.ini ({} bytes) from {} (base)",
            data.len(),
            source
        );
        IniFile::from_bytes(&data).ok()?
    } else {
        log::warn!("rules.ini not found in MIX archives");
        return None;
    };

    // Step 2: If rulesmd.ini exists, merge it on top (YR patch).
    if let Some((patch_data, patch_source)) = asset_manager.get_with_source("rulesmd.ini") {
        log::info!(
            "Loading rulesmd.ini ({} bytes) from {} (YR patch)",
            patch_data.len(),
            patch_source
        );
        if let Ok(patch_ini) = IniFile::from_bytes(&patch_data) {
            let patch_sections: usize = patch_ini.section_count();
            ini.merge(&patch_ini);
            log::info!(
                "Merged {} rulesmd.ini sections on top of rules.ini",
                patch_sections
            );
        }
    }

    match RuleSet::from_ini(&ini) {
        Ok(rules) => {
            log::info!("RuleSet: {} objects loaded", rules.object_count());
            Some(rules)
        }
        Err(e) => {
            log::warn!("Failed to parse merged rules: {}", e);
            None
        }
    }
}

/// Load art.ini from MIX archives and parse into ArtRegistry.
///
/// Like rules, artmd.ini is a YR patch on top of art.ini. We load art.ini
/// first, then merge artmd.ini on top so all base entries are preserved.
pub(crate) fn load_art_ini(asset_manager: &AssetManager) -> Option<(ArtRegistry, IniFile)> {
    // Step 1: Load base art.ini.
    let mut ini: IniFile = if let Some((data, source)) = asset_manager.get_with_source("art.ini") {
        log::info!(
            "Loading art.ini ({} bytes) from {} (base)",
            data.len(),
            source
        );
        match IniFile::from_bytes(&data) {
            Ok(i) => i,
            Err(e) => {
                log::warn!("Failed to parse art.ini: {}", e);
                return None;
            }
        }
    } else {
        log::warn!("art.ini not found in MIX archives");
        return None;
    };

    // Step 2: If artmd.ini exists, merge it on top (YR patch).
    if let Some((patch_data, patch_source)) = asset_manager.get_with_source("artmd.ini") {
        log::info!(
            "Loading artmd.ini ({} bytes) from {} (YR patch)",
            patch_data.len(),
            patch_source
        );
        if let Ok(patch_ini) = IniFile::from_bytes(&patch_data) {
            let patch_sections: usize = patch_ini.section_count();
            ini.merge(&patch_ini);
            log::info!(
                "Merged {} artmd.ini sections on top of art.ini",
                patch_sections
            );
        }
    }

    let reg: ArtRegistry = ArtRegistry::from_ini(&ini);
    log::info!("ArtRegistry: {} entries loaded", reg.len());
    Some((reg, ini))
}

/// Spawn map entities into ECS world and build voxel + SHP sprite atlases.
pub(crate) fn spawn_entities(
    map_data: &MapFile,
    resolved_terrain: &ResolvedTerrainGrid,
    asset_manager: &AssetManager,
    gpu: &GpuContext,
    batch: &BatchRenderer,
    theater_ext: &str,
    theater_name: &str,
    rules: Option<&RuleSet>,
    art: Option<&ArtRegistry>,
    house_colors: &HouseColorMap,
    height_map: &BTreeMap<(u16, u16), u8>,
    theater_unit_palette: Option<&Palette>,
    infantry_sequences: &crate::rules::infantry_sequence::InfantrySequenceRegistry,
    vxl_compute: Option<&mut crate::render::vxl_compute::VxlComputeRenderer>,
) -> (Option<Simulation>, Option<UnitAtlas>, Option<SpriteAtlas>) {
    let mut sim: Simulation = Simulation::new();
    sim.resolved_terrain = Some(resolved_terrain.clone());
    let bridge_destroyable = map_data
        .special_flags
        .destroyable_bridges
        .unwrap_or_else(|| {
            rules
                .map(|rules| rules.bridge_rules.destroyable_by_default)
                .unwrap_or(true)
        });
    let bridge_strength = rules
        .map(|rules| rules.bridge_rules.strength)
        .unwrap_or(250);
    sim.bridge_state = Some(
        crate::sim::bridge_state::BridgeRuntimeState::from_resolved_terrain(
            resolved_terrain,
            bridge_destroyable,
            bridge_strength,
        ),
    );
    sim.bridge_explosions = rules
        .map(|r| {
            r.bridge_rules
                .explosions
                .iter()
                .map(|s| sim.interner.intern(s))
                .collect()
        })
        .unwrap_or_default();
    sim.refresh_terrain_views();
    if !map_data.entities.is_empty() {
        let _count: u32 = sim.spawn_from_map_with_resolved(
            &map_data.entities,
            rules,
            height_map,
            Some(resolved_terrain),
        );
        let miner_count: usize = sim.entities.values().filter(|e| e.miner.is_some()).count();
        log::info!("Miner components attached: {}", miner_count);
        // Sync building footprints to LayeredPathGrid so layered A* respects buildings.
        sim.sync_building_footprints_to_layered_grid(rules);
    }
    let (unit_atlas, shp_atlas) = build_entity_atlases(
        &sim,
        asset_manager,
        gpu,
        batch,
        theater_ext,
        theater_name,
        rules,
        art,
        house_colors,
        theater_unit_palette,
        infantry_sequences,
        vxl_compute,
    );
    // Update VoxelAnimation frame counts from atlas HVA data.
    if let Some(ref atlas) = unit_atlas {
        sim.update_voxel_anim_frame_counts(&atlas.frame_counts);
    }
    (Some(sim), unit_atlas, shp_atlas)
}

pub(crate) fn build_entity_atlases(
    sim: &Simulation,
    asset_manager: &AssetManager,
    gpu: &GpuContext,
    batch: &BatchRenderer,
    theater_ext: &str,
    theater_name: &str,
    rules: Option<&RuleSet>,
    art: Option<&ArtRegistry>,
    house_colors: &HouseColorMap,
    theater_unit_palette: Option<&Palette>,
    infantry_sequences: &crate::rules::infantry_sequence::InfantrySequenceRegistry,
    vxl_compute: Option<&mut crate::render::vxl_compute::VxlComputeRenderer>,
) -> (Option<UnitAtlas>, Option<SpriteAtlas>) {
    // Use the theater-specific unit palette if provided, otherwise fall back to search.
    let palette: Option<Palette> = theater_unit_palette.cloned().or_else(|| {
        let pal_names: &[&str] = &["unittem.pal", "unit.pal", "temperat.pal"];
        pal_names.iter().find_map(|name| {
            let data: Vec<u8> = asset_manager.get(name)?;
            Palette::from_bytes(&data).ok()
        })
    });
    let unit_atlas: Option<UnitAtlas> = palette.as_ref().and_then(|pal| {
        unit_atlas::build_unit_atlas(
            gpu,
            batch,
            &sim.entities,
            asset_manager,
            pal,
            rules,
            art,
            house_colors,
            None, // initial build — no existing cache
            vxl_compute,
            Some(&sim.interner),
        )
    });
    // Pre-load building types that can be spawned at runtime (e.g., ConYards from MCV deploy).
    let extra_buildings: Vec<&str> =
        deployable_building_types(&sim.entities, rules, Some(&sim.interner));
    let shp_atlas: Option<SpriteAtlas> = palette.as_ref().and_then(|pal| {
        sprite_atlas::build_sprite_atlas(
            gpu,
            batch,
            &sim.entities,
            asset_manager,
            pal,
            theater_ext,
            theater_name,
            rules,
            art,
            house_colors,
            &extra_buildings,
            infantry_sequences,
            None, // initial build — no existing cache
            Some(&sim.interner),
        )
    });
    (unit_atlas, shp_atlas)
}
