//! Resolved terrain/topology stage built from raw map cells plus theater/TMP metadata.
//!
//! This module sits between `MapFile` parsing and downstream consumers such as
//! rendering, pathfinding, and building placement. It preserves raw IsoMapPack5
//! data while attaching resolved per-cell metadata such as final LAT-adjusted
//! tile choice, land/slope bytes from TMP, and coarse blocking/buildability flags.

use crate::assets::tmp_file::{TmpFile, TmpTile};
use crate::map::lat;
use crate::map::map_file::{MapCell, MapFile};
use crate::map::overlay::OverlayEntry;
use crate::map::overlay_types::OverlayTypeRegistry;
use crate::map::theater::{self, TheaterData, TileKey};
use crate::rules::terrain_rules::{SpeedCostProfile, TerrainClass, TerrainRules};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Canonical ramp direction from TS++ TIBSUN_DEFINES.H (slope types 1-4).
/// These are the four basic full-edge ramps where two adjacent corners are raised.
///
/// Names are in **map coordinates** (as defined by TS++). In the isometric view,
/// map-North appears as screen upper-right. The actual tilt angles used for VXL
/// rendering come from the slope_type number (1-8) indexed into a pre-computed
/// matrix table — they don't depend on these labels.
pub enum RampDirection {
    West,
    North,
    East,
    South,
}

/// Bridge direction determines height offset and rendering behavior.
/// EW frames 0-3, NS frames 9-12. Low bridges have no height offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeDirection {
    /// BRIDGE1, BRIDGEB1 — EW direction. Height offset = CellHeight + 1 = 16px.
    EastWest,
    /// BRIDGE2, BRIDGEB2 — NS direction. Height offset = CellHeight * 2 + 1 = 31px.
    NorthSouth,
    /// LOBRDG*, LOBRDB* — ground-level bridge. No height offset.
    Low,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeLayer {
    pub overlay_id: u8,
    pub overlay_name: String,
    /// Bridge deck height level (ground level + offset).
    pub deck_level: u8,
    /// Bridge direction — determines height offset and rendering.
    pub direction: BridgeDirection,
}

#[derive(Debug, Clone)]
pub struct ResolvedTerrainCell {
    pub rx: u16,
    pub ry: u16,
    pub source_tile_index: i32,
    pub source_sub_tile: u8,
    pub final_tile_index: i32,
    pub final_sub_tile: u8,
    pub level: u8,
    pub filled_clear: bool,
    pub tileset_index: Option<u16>,
    pub land_type: u8,
    pub slope_type: u8,
    pub template_height: u8,
    pub render_offset_x: i32,
    pub render_offset_y: i32,
    pub terrain_class: TerrainClass,
    pub speed_costs: SpeedCostProfile,
    pub is_water: bool,
    pub is_cliff_like: bool,
    pub is_rough: bool,
    pub is_road: bool,
    /// FinalAlert2-style cliff redraw flag. When true, this cell's terrain tile
    /// is drawn a second time AFTER entities so cliff faces occlude units behind
    /// them. Computed from height differences with back-left neighbor cells
    /// (height diff >= 4). See MapData.cpp:3362-3377 in the EA FA2 source.
    pub is_cliff_redraw: bool,
    /// Tile visual variant index (FA2 bRNDImage): 0 = main tile, 1-4 = replacement a-d.
    pub variant: u8,
    pub has_ramp: bool,
    pub canonical_ramp: Option<RampDirection>,
    pub ground_walk_blocked: bool,
    pub terrain_object_blocks: bool,
    pub overlay_blocks: bool,
    pub base_build_blocked: bool,
    pub build_blocked: bool,
    pub has_bridge_deck: bool,
    pub bridge_walkable: bool,
    pub bridge_transition: bool,
    pub bridge_deck_level: u8,
    pub bridge_layer: Option<BridgeLayer>,
    /// Per-tile radar minimap color (left half of isometric diamond), from TMP header.
    pub radar_left: [u8; 3],
    /// Per-tile radar minimap color (right half of isometric diamond), from TMP header.
    pub radar_right: [u8; 3],
}

impl ResolvedTerrainCell {
    pub fn is_walkable(&self) -> bool {
        !self.ground_walk_blocked
    }

    pub fn is_bridge_transition_cell(&self) -> bool {
        self.bridge_transition
    }

    pub fn is_elevated_bridge_cell(&self) -> bool {
        self.bridge_walkable && self.bridge_deck_level > self.level
    }

    pub fn bridge_deck_level_if_any(&self) -> Option<u8> {
        self.has_bridge_deck.then_some(self.bridge_deck_level)
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedTerrainGrid {
    width: u16,
    height: u16,
    pub cells: Vec<ResolvedTerrainCell>,
}

impl ResolvedTerrainGrid {
    pub fn from_cells(width: u16, height: u16, cells: Vec<ResolvedTerrainCell>) -> Self {
        Self {
            width,
            height,
            cells,
        }
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    pub fn index(&self, rx: u16, ry: u16) -> Option<usize> {
        if rx < self.width && ry < self.height {
            Some(ry as usize * self.width as usize + rx as usize)
        } else {
            None
        }
    }

    pub fn cell(&self, rx: u16, ry: u16) -> Option<&ResolvedTerrainCell> {
        self.index(rx, ry).and_then(|i| self.cells.get(i))
    }

    pub fn iter(&self) -> impl Iterator<Item = &ResolvedTerrainCell> {
        self.cells.iter()
    }

    pub fn build(
        map: &MapFile,
        theater_data: Option<&TheaterData>,
        asset_manager: Option<&crate::assets::asset_manager::AssetManager>,
        terrain_rules: Option<&TerrainRules>,
        overlay_registry: Option<&OverlayTypeRegistry>,
        lat_enabled: bool,
        cliff_back_impassability: u8,
    ) -> Self {
        let (width, height) = grid_dimensions(&map.cells);
        if width == 0 || height == 0 {
            return Self {
                width: 0,
                height: 0,
                cells: Vec::new(),
            };
        }

        let mut final_cells: Vec<MapCell> = map.cells.clone();
        if lat_enabled {
            if let Some(td) = theater_data {
                let lat_config = lat::parse_lat_config(&td.ini_data, &td.lookup);
                if !lat_config.grounds.is_empty() {
                    lat::apply_lat(&mut final_cells, &lat_config, &td.lookup);
                }
            }
        }

        let raw_lookup: HashMap<(u16, u16), &MapCell> =
            map.cells.iter().map(|c| ((c.rx, c.ry), c)).collect();
        let final_lookup: HashMap<(u16, u16), &MapCell> =
            final_cells.iter().map(|c| ((c.rx, c.ry), c)).collect();

        let terrain_objects: HashSet<(u16, u16)> = map
            .terrain_objects
            .iter()
            .map(|obj| (obj.rx, obj.ry))
            .collect();

        let mut overlays_by_cell: HashMap<(u16, u16), Vec<&OverlayEntry>> = HashMap::new();
        for overlay in &map.overlays {
            overlays_by_cell
                .entry((overlay.rx, overlay.ry))
                .or_default()
                .push(overlay);
        }

        let mut metadata_cache: HashMap<TileKey, TileMetadata> = HashMap::new();
        let mut warned_unknown_land_types: HashSet<u8> = HashSet::new();
        let mut cells: Vec<ResolvedTerrainCell> =
            Vec::with_capacity(width as usize * height as usize);

        for ry in 0..height {
            for rx in 0..width {
                let raw = raw_lookup.get(&(rx, ry)).copied();
                let final_cell = final_lookup.get(&(rx, ry)).copied();
                let (final_tile_index, final_sub_tile, level) = final_cell
                    .map(|cell| (cell.tile_index, cell.sub_tile, cell.z))
                    .unwrap_or((0, 0, 0));
                let tile_key = TileKey {
                    tile_id: normalize_tile_id(final_tile_index),
                    sub_tile: final_sub_tile,
                    variant: 0,
                };
                let mut metadata = if let Some(metadata) = metadata_cache.get(&tile_key) {
                    metadata.clone()
                } else {
                    let metadata = load_tile_metadata(
                        theater_data,
                        asset_manager,
                        terrain_rules,
                        tile_key,
                        &mut warned_unknown_land_types,
                    );
                    metadata_cache.insert(tile_key, metadata.clone());
                    metadata
                };
                let terrain_object_blocks = terrain_objects.contains(&(rx, ry));
                let overlay_effects = classify_overlay_effects(
                    overlays_by_cell.get(&(rx, ry)),
                    overlay_registry,
                    level,
                );
                let canonical_ramp = canonical_ramp_from_slope_type(metadata.slope_type);
                // Low bridges override underlying terrain to Road (NoUseTileLandType=true,
                // Land=Road in rulesmd.ini). This makes water cells under low bridges
                // passable for ground units.
                if overlay_effects.is_low_bridge {
                    metadata.is_water = false;
                    metadata.is_road = true;
                    metadata.ground_blocked = false;
                    metadata.is_cliff_like = false;
                    metadata.terrain_class = TerrainClass::Road;
                    metadata.land_type =
                        crate::sim::pathfinding::passability::LandType::Road.as_index();
                }
                // Road/pavement overlays override underlying terrain to Road.
                // Matches original engine: RecalcLandType sets LandType=Road(1)
                // when overlay.Wall is true.
                if overlay_effects.is_road && !overlay_effects.is_low_bridge {
                    metadata.is_road = true;
                    metadata.terrain_class = TerrainClass::Road;
                    metadata.land_type =
                        crate::sim::pathfinding::passability::LandType::Road.as_index();
                }
                // Tiberium/ore overlays change the effective terrain type for passability.
                // Matches original engine: RecalcLandType sets cell+0xEC when tiberium present.
                // Also update speed_costs from [Tiberium] INI section so the terrain
                // cost grid uses correct speed modifiers (Foot=90%, Track=70%, etc.).
                if overlay_effects.has_tiberium {
                    metadata.land_type =
                        crate::sim::pathfinding::passability::LandType::Tiberium.as_index();
                    metadata.terrain_class = TerrainClass::Tiberium;
                    if let Some(tib_semantics) =
                        terrain_rules.and_then(|tr| tr.semantics_by_name("Tiberium"))
                    {
                        metadata.speed_costs = tib_semantics.speed_costs;
                    }
                }
                let base_ground_walk_blocked = canonical_ramp.is_none() && metadata.ground_blocked;
                let is_cliff_like = metadata.is_cliff_like;
                let ground_walk_blocked = base_ground_walk_blocked
                    || terrain_object_blocks
                    || overlay_effects.overlay_blocks;
                let base_build_blocked = metadata.build_blocked
                    || terrain_object_blocks
                    || overlay_effects.overlay_blocks
                    || canonical_ramp.is_some();
                let bridge_walkable = overlay_effects.has_bridge_deck
                    && !terrain_object_blocks
                    && !overlay_effects.overlay_blocks;
                // Allow layer transitions on any bridge deck cell. High bridges over
                // water have ground_walk_blocked=true, but units still need to transition
                // from Ground→Bridge at the ramp/entry cells.
                // Only bridgehead ramp cells (detected below) allow layer
                // transitions. Deck cells must NOT be transitions — otherwise
                // the A* can switch Bridge→Ground mid-span and units clip
                // through the bridge.
                let bridge_transition = false;
                let build_blocked = base_build_blocked || bridge_walkable;
                cells.push(ResolvedTerrainCell {
                    rx,
                    ry,
                    source_tile_index: raw.map(|c| c.tile_index).unwrap_or(theater::NO_TILE),
                    source_sub_tile: raw.map(|c| c.sub_tile).unwrap_or(0),
                    final_tile_index,
                    final_sub_tile,
                    level,
                    filled_clear: raw.is_none(),
                    tileset_index: metadata.tileset_index,
                    land_type: metadata.land_type,
                    slope_type: metadata.slope_type,
                    template_height: metadata.template_height,
                    render_offset_x: metadata.render_offset_x,
                    render_offset_y: metadata.render_offset_y,
                    terrain_class: metadata.terrain_class,
                    speed_costs: metadata.speed_costs,
                    is_water: metadata.is_water,
                    is_cliff_like,
                    is_rough: metadata.is_rough,
                    is_road: metadata.is_road,
                    is_cliff_redraw: false,
                    variant: 0,
                    has_ramp: metadata.has_ramp,
                    canonical_ramp,
                    ground_walk_blocked,
                    terrain_object_blocks,
                    overlay_blocks: overlay_effects.overlay_blocks,
                    base_build_blocked,
                    build_blocked,
                    has_bridge_deck: overlay_effects.has_bridge_deck,
                    bridge_walkable,
                    bridge_transition,
                    bridge_deck_level: overlay_effects
                        .bridge_layer
                        .as_ref()
                        .map(|layer| layer.deck_level)
                        .unwrap_or(level),
                    bridge_layer: overlay_effects.bridge_layer,
                    radar_left: metadata.radar_left,
                    radar_right: metadata.radar_right,
                });
            }
        }

        // High bridges store only the center cell overlay but span 3 cells wide.
        // Extrapolate bridge deck flags to the two perpendicular side cells so
        // pathfinding treats the full bridge width as walkable.
        //   EW (BRIDGE1/BRIDGEB1): bridge runs along rx, side cells at ry±1
        //   NS (BRIDGE2/BRIDGEB2): bridge runs along ry, side cells at rx±1
        let mut side_cells: Vec<(usize, u8, BridgeDirection)> = Vec::new();
        for cell in &cells {
            let bl = match &cell.bridge_layer {
                Some(bl)
                    if bl.direction == BridgeDirection::EastWest
                        || bl.direction == BridgeDirection::NorthSouth =>
                {
                    bl
                }
                _ => continue,
            };
            let offsets: [(i32, i32); 2] = match bl.direction {
                BridgeDirection::EastWest => [(0, -1), (0, 1)],
                BridgeDirection::NorthSouth => [(-1, 0), (1, 0)],
                BridgeDirection::Low => unreachable!(),
            };
            for (dx, dy) in offsets {
                let sx = cell.rx as i32 + dx;
                let sy = cell.ry as i32 + dy;
                if sx < 0 || sy < 0 || sx >= width as i32 || sy >= height as i32 {
                    continue;
                }
                if let Some(idx) = Some(sy as usize * width as usize + sx as usize) {
                    if idx < cells.len() && !cells[idx].has_bridge_deck {
                        side_cells.push((idx, bl.deck_level, bl.direction));
                    }
                }
            }
        }
        let extrapolated_count = side_cells.len();
        for (idx, deck_level, _direction) in side_cells {
            cells[idx].has_bridge_deck = true;
            cells[idx].bridge_walkable = true;
            cells[idx].bridge_deck_level = deck_level;
            cells[idx].build_blocked = true;
        }
        if extrapolated_count > 0 {
            log::info!(
                "ResolvedTerrain: extrapolated {} high bridge side cells",
                extrapolated_count,
            );
        }

        // Normalize bridge deck levels via connected-component flood fill.
        // All adjacent high bridge cells must share the same deck height so the
        // bridge surface is flat. Center cells (ry=N) and their side cells
        // (ry=N±1) are in different rows but must have the same deck_level.
        // BFS finds each connected group of bridge cells and applies the max
        // deck_level within each group.
        {
            let mut visited = vec![false; cells.len()];
            let mut normalized: usize = 0;
            for start in 0..cells.len() {
                if !cells[start].has_bridge_deck
                    || visited[start]
                    || cells[start]
                        .bridge_layer
                        .as_ref()
                        .is_some_and(|bl| bl.direction == BridgeDirection::Low)
                {
                    continue;
                }
                // BFS to find connected component of high bridge cells.
                let mut component: Vec<usize> = Vec::new();
                let mut queue = std::collections::VecDeque::new();
                queue.push_back(start);
                visited[start] = true;
                let mut max_level: u8 = 0;
                while let Some(idx) = queue.pop_front() {
                    component.push(idx);
                    max_level = max_level.max(cells[idx].bridge_deck_level);
                    let crx = cells[idx].rx as i32;
                    let cry = cells[idx].ry as i32;
                    for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                        let nx = crx + dx;
                        let ny = cry + dy;
                        if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                            continue;
                        }
                        let nidx = ny as usize * width as usize + nx as usize;
                        if nidx < cells.len()
                            && !visited[nidx]
                            && cells[nidx].has_bridge_deck
                            && !cells[nidx]
                                .bridge_layer
                                .as_ref()
                                .is_some_and(|bl| bl.direction == BridgeDirection::Low)
                        {
                            visited[nidx] = true;
                            queue.push_back(nidx);
                        }
                    }
                }
                // Apply uniform deck_level to all cells in this bridge.
                for &idx in &component {
                    if cells[idx].bridge_deck_level != max_level {
                        normalized += 1;
                        cells[idx].bridge_deck_level = max_level;
                        if let Some(ref mut bl) = cells[idx].bridge_layer {
                            bl.deck_level = max_level;
                        }
                    }
                }
            }
            if normalized > 0 {
                log::info!(
                    "ResolvedTerrain: normalized deck_level for {} bridge cells",
                    normalized,
                );
            }
        }

        // Bridgehead TMP tiles (ramps at each end of a high bridge) are solid
        // ground terrain, walkable on the ground layer. The original engine sets
        // CellFlags::BridgeHead on these cells,
        // enabling Ground↔Bridge layer transitions at the ramp.
        // Mark them as transition cells so the layered A* can route through them.
        // Do NOT set has_bridge_deck or bridge_walkable — they're ground terrain.
        if let Some(td) = theater_data {
            // Collect bridgehead cell indices first, then apply changes.
            let mut bridgehead_updates: Vec<(usize, u8)> = Vec::new();
            for (idx, cell) in cells.iter().enumerate() {
                let Some(ts_idx) = cell.tileset_index else {
                    continue;
                };
                let is_bridgehead = td.bridge_set.is_some_and(|bs| ts_idx == bs)
                    || td.wood_bridge_set.is_some_and(|ws| ts_idx == ws);
                if is_bridgehead && !cell.has_bridge_deck {
                    // Use the deck_level from an adjacent bridge span cell so
                    // the bridgehead matches the normalized bridge height.
                    // Prevents z-discontinuity at the ramp-to-span transition.
                    let crx = cell.rx as i32;
                    let cry = cell.ry as i32;
                    let mut span_deck: Option<u8> = None;
                    for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                        let nx = crx + dx;
                        let ny = cry + dy;
                        if nx >= 0 && ny >= 0 && nx < width as i32 && ny < height as i32 {
                            let nidx = ny as usize * width as usize + nx as usize;
                            if nidx < cells.len() && cells[nidx].has_bridge_deck {
                                span_deck = Some(cells[nidx].bridge_deck_level);
                                break;
                            }
                        }
                    }
                    let deck = span_deck.unwrap_or_else(|| cell.level.saturating_add(4));
                    bridgehead_updates.push((idx, deck));
                }
            }
            let bridgehead_count = bridgehead_updates.len();
            for (idx, deck_level) in &bridgehead_updates {
                cells[*idx].bridge_transition = true;
                // bridge_walkable=true enables Ground↔Bridge layer transitions in
                // the layered A*. Bridgeheads are ground-level ramps, not elevated
                // deck cells, but they must be walkable on BOTH layers so the
                // pathfinder can switch layers here. Rendering uses bridge_occupancy
                // (runtime flag), not bridge_walkable, so this doesn't affect visuals.
                cells[*idx].bridge_walkable = true;
                cells[*idx].bridge_deck_level = *deck_level;
                log::info!(
                    "BRIDGEHEAD cell ({},{}) ground_level={} deck_level={}",
                    cells[*idx].rx,
                    cells[*idx].ry,
                    cells[*idx].level,
                    deck_level,
                );
            }
            if bridgehead_count > 0 {
                log::info!(
                    "ResolvedTerrain: {} bridgehead transition cells detected",
                    bridgehead_count,
                );
            }
        }

        // Gap-fill pass: bridge overlays may not exist on every cell (the sprite
        // visually covers adjacent cells). Fill 1-cell gaps between bridge deck
        // cells so the walkable surface is continuous. A cell is filled if it has
        // has_bridge_deck neighbors on opposite sides (both rx±1 or both ry±1).
        {
            let mut gap_fills: Vec<(usize, u8)> = Vec::new();
            for idx in 0..cells.len() {
                if cells[idx].has_bridge_deck {
                    continue;
                }
                let rx = cells[idx].rx as i32;
                let ry = cells[idx].ry as i32;
                let w = width as i32;
                let h = height as i32;

                // Check rx-axis neighbors (both rx-1 and rx+1 have bridge deck).
                let left_idx = if rx > 0 {
                    Some((ry * w + rx - 1) as usize)
                } else {
                    None
                };
                let right_idx = if rx < w - 1 {
                    Some((ry * w + rx + 1) as usize)
                } else {
                    None
                };
                let rx_gap = left_idx.zip(right_idx).is_some_and(|(l, r)| {
                    l < cells.len()
                        && r < cells.len()
                        && cells[l].has_bridge_deck
                        && cells[r].has_bridge_deck
                        && !cells[l]
                            .bridge_layer
                            .as_ref()
                            .is_some_and(|bl| bl.direction == BridgeDirection::Low)
                });

                // Check ry-axis neighbors (both ry-1 and ry+1 have bridge deck).
                let up_idx = if ry > 0 {
                    Some(((ry - 1) * w + rx) as usize)
                } else {
                    None
                };
                let down_idx = if ry < h - 1 {
                    Some(((ry + 1) * w + rx) as usize)
                } else {
                    None
                };
                let ry_gap = up_idx.zip(down_idx).is_some_and(|(u, d)| {
                    u < cells.len()
                        && d < cells.len()
                        && cells[u].has_bridge_deck
                        && cells[d].has_bridge_deck
                        && !cells[u]
                            .bridge_layer
                            .as_ref()
                            .is_some_and(|bl| bl.direction == BridgeDirection::Low)
                });

                if rx_gap || ry_gap {
                    // Use deck_level from a neighbor.
                    let neighbor_level = if rx_gap {
                        cells[left_idx.unwrap()].bridge_deck_level
                    } else {
                        cells[up_idx.unwrap()].bridge_deck_level
                    };
                    gap_fills.push((idx, neighbor_level));
                }
            }
            let gap_count = gap_fills.len();
            for (idx, deck_level) in gap_fills {
                cells[idx].has_bridge_deck = true;
                cells[idx].bridge_walkable = true;
                cells[idx].bridge_deck_level = deck_level;
                cells[idx].build_blocked = true;
            }
            if gap_count > 0 {
                log::info!("ResolvedTerrain: filled {} bridge deck gaps", gap_count,);
            }
        }

        // Log all high bridge deck cells (center + extrapolated) to diagnose gaps.
        {
            let mut high_deck: Vec<(u16, u16, u8, &str)> = cells
                .iter()
                .filter(|c| {
                    c.has_bridge_deck
                        && !c
                            .bridge_layer
                            .as_ref()
                            .is_some_and(|bl| bl.direction == BridgeDirection::Low)
                })
                .map(|c| {
                    let label = if c.bridge_layer.is_some() {
                        "center"
                    } else {
                        "side"
                    };
                    (c.rx, c.ry, c.bridge_deck_level, label)
                })
                .collect();
            high_deck.sort_by_key(|(rx, ry, _, _)| (*rx, *ry));
            if !high_deck.is_empty() {
                log::info!("High bridge deck cells ({} total):", high_deck.len(),);
                for (rx, ry, dl, label) in &high_deck {
                    log::info!("  ({}, {}) deck_level={} [{}]", rx, ry, dl, label);
                }
            }
        }

        // Log overlay entries near bridge cells that were NOT classified as bridges.
        // This helps diagnose gaps in the bridge deck coverage.
        if let Some(first_center) = cells.iter().find(|c| {
            c.bridge_layer
                .as_ref()
                .is_some_and(|bl| bl.direction != BridgeDirection::Low)
        }) {
            let center_ry = first_center.ry;
            let center_rx = first_center.rx;
            let mut unrecognized: Vec<(u16, u16, u8, String)> = Vec::new();
            for overlay in &map.overlays {
                // Check overlays near the bridge span (±3 cells).
                if overlay.ry.abs_diff(center_ry) <= 3 && overlay.rx >= center_rx.saturating_sub(2)
                {
                    let idx = overlay.ry as usize * width as usize + overlay.rx as usize;
                    if idx < cells.len() && !cells[idx].has_bridge_deck {
                        let name = overlay_registry
                            .and_then(|reg| reg.name(overlay.overlay_id))
                            .unwrap_or("?")
                            .to_string();
                        unrecognized.push((overlay.rx, overlay.ry, overlay.overlay_id, name));
                    }
                }
            }
            if !unrecognized.is_empty() {
                unrecognized.sort_by_key(|(rx, ry, _, _)| (*rx, *ry));
                log::info!(
                    "Overlays near bridge NOT classified as deck ({}):",
                    unrecognized.len(),
                );
                for (rx, ry, id, name) in &unrecognized {
                    log::info!("  ({}, {}) overlay_id={} name={}", rx, ry, id, name);
                }
            }
        }

        // Log bridge cell statistics for diagnostics.
        let bridge_cell_count: usize = cells.iter().filter(|c| c.has_bridge_deck).count();
        let low_bridge_count: usize = cells
            .iter()
            .filter(|c| {
                c.bridge_layer
                    .as_ref()
                    .map(|bl| bl.direction == BridgeDirection::Low)
                    .unwrap_or(false)
            })
            .count();
        let high_bridge_count: usize = bridge_cell_count - low_bridge_count;
        if bridge_cell_count > 0 {
            log::info!(
                "ResolvedTerrain: {} bridge deck cells ({} high, {} low)",
                bridge_cell_count,
                high_bridge_count,
                low_bridge_count,
            );
        }

        // FinalAlert2-style cliff redraw detection (MapData.cpp:3362-3377).
        // For each cell, check the 2x2 block of neighbors at offsets (-2..-1, -2..-1)
        // in isometric (rx, ry) space. If any neighbor is >= 4 levels lower than this
        // cell, mark it for second-pass terrain redraw so cliff faces occlude entities.
        const CLIFF_HEIGHT_THRESHOLD: u8 = 4;
        let mut cliff_redraw_count: usize = 0;
        for idx in 0..cells.len() {
            let rx = cells[idx].rx as i32;
            let ry = cells[idx].ry as i32;
            let h = cells[idx].level;
            if h < CLIFF_HEIGHT_THRESHOLD {
                continue;
            }
            let mut redraw = false;
            'outer: for dy in [-2i32, -1] {
                for dx in [-2i32, -1] {
                    let nx = rx + dx;
                    let ny = ry + dy;
                    if nx >= 0 && ny >= 0 && nx < width as i32 && ny < height as i32 {
                        let nidx = ny as usize * width as usize + nx as usize;
                        if nidx < cells.len()
                            && h.saturating_sub(cells[nidx].level) >= CLIFF_HEIGHT_THRESHOLD
                        {
                            redraw = true;
                            break 'outer;
                        }
                    }
                }
            }
            if redraw {
                cells[idx].is_cliff_redraw = true;
                cliff_redraw_count += 1;
            }
        }
        if cliff_redraw_count > 0 {
            log::info!(
                "ResolvedTerrain: {} cells flagged for cliff redraw",
                cliff_redraw_count,
            );
        }

        // CliffBackImpassability: mark cells at the base of ≥4-level cliffs as
        // impassable. Matches gamemd.exe CellClass::RecalcAttributes (0x0047d2b0).
        // When value == 2 (YR default), cells where ANY of 6 isometric neighbors
        // is ≥4 levels above get land_type=Rock and ground_walk_blocked=true.
        // Only overrides Clear(0), Water(4), Beach(3) land types.
        if cliff_back_impassability == 2 {
            const CLIFF_BACK_HEIGHT_DIFF: u8 = 4;
            // 6 neighbor offsets in (dx, dy) matching gamemd.exe RecalcAttributes:
            // (X, Y-1), (X-1, Y), (X+2, Y+2), (X+1, Y+1), (X-1, Y+1), (X+1, Y-1)
            const NEIGHBOR_OFFSETS: [(i32, i32); 6] =
                [(0, -1), (-1, 0), (2, 2), (1, 1), (-1, 1), (1, -1)];
            let rock_lt = crate::sim::pathfinding::passability::LandType::Rock.as_index();
            let clear_lt = crate::sim::pathfinding::passability::LandType::Clear.as_index();
            let water_lt = crate::sim::pathfinding::passability::LandType::Water.as_index();
            let beach_lt = crate::sim::pathfinding::passability::LandType::Beach.as_index();

            let mut cliff_back_count: usize = 0;
            for idx in 0..cells.len() {
                let lt = cells[idx].land_type;
                if lt != clear_lt && lt != water_lt && lt != beach_lt {
                    continue;
                }
                let cell_level = cells[idx].level;
                let rx = cells[idx].rx as i32;
                let ry = cells[idx].ry as i32;

                let mut behind_cliff = false;
                for &(dx, dy) in &NEIGHBOR_OFFSETS {
                    let nx = rx + dx;
                    let ny = ry + dy;
                    if nx >= 0 && ny >= 0 && nx < width as i32 && ny < height as i32 {
                        let nidx = ny as usize * width as usize + nx as usize;
                        if nidx < cells.len()
                            && cells[nidx].level >= cell_level + CLIFF_BACK_HEIGHT_DIFF
                        {
                            behind_cliff = true;
                            break;
                        }
                    }
                }
                if behind_cliff {
                    cells[idx].land_type = rock_lt;
                    cells[idx].ground_walk_blocked = true;
                    cells[idx].is_cliff_like = true;
                    cliff_back_count += 1;
                }
            }
            if cliff_back_count > 0 {
                log::info!(
                    "ResolvedTerrain: {} cells marked impassable by CliffBackImpassability",
                    cliff_back_count,
                );
            }
        }

        // Assign random tile visual variants (FA2 bRNDImage, MapData.cpp:3292-3306).
        // Uses deterministic hash of (rx, ry) for reproducibility across sessions.
        // Tiles with HasDamagedData (bridges) use variants for damage states, not
        // visual diversity — those are excluded.
        if let Some(td) = theater_data {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut variant_total: usize = 0;
            for cell in &mut cells {
                let tile_id = normalize_tile_id(cell.final_tile_index);
                let vc = td.lookup.variant_count(tile_id);
                if vc == 0 {
                    continue;
                }
                let mut hasher = DefaultHasher::new();
                (cell.rx, cell.ry).hash(&mut hasher);
                let hash = hasher.finish();
                // 0 = main tile, 1..=vc = replacement. Matches FA2's
                // rand() * (1 + count) / RAND_MAX distribution.
                cell.variant = (hash % (vc as u64 + 1)) as u8;
                if cell.variant > 0 {
                    variant_total += 1;
                }
            }
            if variant_total > 0 {
                log::info!(
                    "ResolvedTerrain: {} cells assigned tile variants",
                    variant_total,
                );
            }
        }

        Self {
            width,
            height,
            cells,
        }
    }

    pub fn build_height_map(&self) -> BTreeMap<(u16, u16), u8> {
        self.cells
            .iter()
            .map(|cell| ((cell.rx, cell.ry), cell.level))
            .collect()
    }

    /// Build a bridge deck height map — only HIGH bridge cells are included.
    /// Low bridges (LOBRDG/LOBRDB) are at ground level and don't need height
    /// correction for click resolution or debug overlays.
    pub fn build_bridge_height_map(&self) -> BTreeMap<(u16, u16), u8> {
        self.cells
            .iter()
            .filter(|cell| {
                cell.has_bridge_deck
                    && !cell
                        .bridge_layer
                        .as_ref()
                        .is_some_and(|bl| bl.direction == BridgeDirection::Low)
            })
            .map(|cell| ((cell.rx, cell.ry), cell.bridge_deck_level))
            .collect()
    }
}

#[derive(Debug, Clone)]
struct TileMetadata {
    tileset_index: Option<u16>,
    has_tmp_metadata: bool,
    /// Mapped land type (0-7) for passability matrix lookups.
    land_type: u8,
    /// Raw TMP terrain_type byte (0-15) for rules.ini semantic lookups.
    raw_land_type: u8,
    slope_type: u8,
    template_height: u8,
    render_offset_x: i32,
    render_offset_y: i32,
    terrain_class: TerrainClass,
    speed_costs: SpeedCostProfile,
    is_water: bool,
    is_cliff_like: bool,
    is_rough: bool,
    is_road: bool,
    has_ramp: bool,
    ground_blocked: bool,
    build_blocked: bool,
    /// Per-tile radar minimap color (left half of isometric diamond), from TMP header.
    radar_left: [u8; 3],
    /// Per-tile radar minimap color (right half of isometric diamond), from TMP header.
    radar_right: [u8; 3],
}

impl Default for TileMetadata {
    fn default() -> Self {
        Self {
            tileset_index: None,
            has_tmp_metadata: false,
            land_type: 0,
            raw_land_type: 0,
            slope_type: 0,
            template_height: 0,
            render_offset_x: 0,
            render_offset_y: 0,
            terrain_class: TerrainClass::Unknown,
            speed_costs: SpeedCostProfile::default(),
            is_water: false,
            is_cliff_like: false,
            is_rough: false,
            is_road: false,
            has_ramp: false,
            ground_blocked: false,
            build_blocked: false,
            radar_left: [0, 0, 0],
            radar_right: [0, 0, 0],
        }
    }
}

#[derive(Debug, Clone, Default)]
struct OverlayEffects {
    overlay_blocks: bool,
    has_bridge_deck: bool,
    bridge_layer: Option<BridgeLayer>,
    /// Low bridges override terrain to Road (NoUseTileLandType=true, Land=Road).
    /// When set, overrides the cell's is_water/is_road/ground_walk_blocked flags.
    is_low_bridge: bool,
    /// Cell has a Tiberium/ore overlay — changes effective land_type to Tiberium (5).
    has_tiberium: bool,
    /// Cell has a road/pavement overlay (Land=Road in rules.ini).
    /// Original engine: RecalcLandType sets LandType=Road(1) when overlay.Wall is true.
    is_road: bool,
}

fn grid_dimensions(cells: &[MapCell]) -> (u16, u16) {
    let mut max_rx: u16 = 0;
    let mut max_ry: u16 = 0;
    let mut found = false;
    for cell in cells {
        found = true;
        max_rx = max_rx.max(cell.rx);
        max_ry = max_ry.max(cell.ry);
    }
    if found {
        (max_rx.saturating_add(1), max_ry.saturating_add(1))
    } else {
        (0, 0)
    }
}

fn normalize_tile_id(tile_index: i32) -> u16 {
    if tile_index == 0xFFFF || tile_index < 0 {
        0
    } else {
        tile_index as u16
    }
}

fn load_tile_metadata(
    theater_data: Option<&TheaterData>,
    asset_manager: Option<&crate::assets::asset_manager::AssetManager>,
    terrain_rules: Option<&TerrainRules>,
    key: TileKey,
    warned_unknown_land_types: &mut HashSet<u8>,
) -> TileMetadata {
    let Some(td) = theater_data else {
        return TileMetadata::default();
    };
    let Some(asset_manager) = asset_manager else {
        return metadata_from_set_name(
            td.lookup
                .tileset_index(key.tile_id)
                .and_then(|idx| td.lookup.set_name(idx)),
            td.lookup.tileset_index(key.tile_id),
        );
    };
    let tileset_index = td.lookup.tileset_index(key.tile_id);
    let set_name = tileset_index.and_then(|idx| td.lookup.set_name(idx));
    let mut metadata = metadata_from_set_name(set_name, tileset_index);

    let Some(filename) = td.lookup.filename(key.tile_id as i32) else {
        return metadata;
    };
    let Some(bytes) = asset_manager.get(filename) else {
        return metadata;
    };
    let Ok(tmp) = TmpFile::from_bytes(&bytes) else {
        return metadata;
    };
    let Some(tile) = tmp
        .tiles
        .get(key.sub_tile as usize)
        .and_then(|t| t.as_ref())
    else {
        apply_land_type_semantics(&mut metadata, terrain_rules, warned_unknown_land_types);
        return metadata;
    };
    // Remember tileset-name road detection before TMP byte overrides it.
    let tileset_says_road = metadata.is_road;
    merge_tmp_metadata(&mut metadata, tile);
    apply_land_type_semantics(&mut metadata, terrain_rules, warned_unknown_land_types);
    // Some road/pavement tilesets encode terrain_type 0 (Clear) in TMP instead of
    // 11 (Road). If the tileset name says "road"/"pavement" but the TMP byte mapped
    // to Clear, trust the tileset name — the visual road should be a road.
    if tileset_says_road && !metadata.is_road && metadata.terrain_class == TerrainClass::Clear {
        metadata.is_road = true;
        metadata.terrain_class = TerrainClass::Road;
    }
    metadata
}

fn metadata_from_set_name(set_name: Option<&str>, tileset_index: Option<u16>) -> TileMetadata {
    let lower = set_name.unwrap_or("").to_ascii_lowercase();
    let is_water = lower.contains("water");
    let is_cliff_like =
        lower.contains("cliff") || lower.contains("rock") || lower.contains("shore");
    let is_rough = lower.contains("rough");
    let is_road = lower.contains("road") || lower.contains("pavement") || lower.contains("pave");
    let land_type = if is_water {
        crate::sim::pathfinding::passability::LandType::Water.as_index()
    } else if is_road {
        crate::sim::pathfinding::passability::LandType::Road.as_index()
    } else if is_rough {
        crate::sim::pathfinding::passability::LandType::Rough.as_index()
    } else if is_cliff_like {
        crate::sim::pathfinding::passability::LandType::Rock.as_index()
    } else {
        crate::sim::pathfinding::passability::LandType::Clear.as_index()
    };
    let terrain_class = if is_water {
        TerrainClass::Water
    } else if lower.contains("cliff") {
        TerrainClass::Cliff
    } else if lower.contains("rock") {
        TerrainClass::Rock
    } else if is_road {
        TerrainClass::Road
    } else if is_rough {
        TerrainClass::Rough
    } else if !lower.is_empty() {
        TerrainClass::Clear
    } else {
        TerrainClass::Unknown
    };

    TileMetadata {
        tileset_index,
        land_type,
        terrain_class,
        is_water,
        is_cliff_like,
        is_rough,
        is_road,
        ground_blocked: is_water || is_cliff_like,
        build_blocked: is_water || is_cliff_like,
        ..TileMetadata::default()
    }
}

fn merge_tmp_metadata(metadata: &mut TileMetadata, tile: &TmpTile) {
    metadata.raw_land_type = tile.terrain_type;
    metadata.land_type =
        crate::sim::pathfinding::passability::tmp_terrain_to_land_type(tile.terrain_type)
            .as_index();
    metadata.slope_type = tile.ramp_type;
    metadata.template_height = tile.height;
    metadata.render_offset_x = tile.offset_x;
    metadata.render_offset_y = tile.offset_y;
    metadata.has_ramp = tile.ramp_type != 0;
    metadata.has_tmp_metadata = true;
    metadata.radar_left = tile.radar_left;
    metadata.radar_right = tile.radar_right;
}

/// Maps TMP ramp_type byte to canonical direction.
/// Values from TS++ TIBSUN_DEFINES.H. Tilt matrix angles:
/// 270 deg=W, 180 deg=N, 90 deg=E, 0 deg=S for slope types 1-4.
fn canonical_ramp_from_slope_type(slope_type: u8) -> Option<RampDirection> {
    match slope_type {
        1 => Some(RampDirection::West),
        2 => Some(RampDirection::North),
        3 => Some(RampDirection::East),
        4 => Some(RampDirection::South),
        _ => None,
    }
}

fn apply_land_type_semantics(
    metadata: &mut TileMetadata,
    terrain_rules: Option<&TerrainRules>,
    warned_unknown_land_types: &mut HashSet<u8>,
) {
    let Some(terrain_rules) = terrain_rules else {
        return;
    };
    if !metadata.has_tmp_metadata {
        return;
    }
    // Use the raw TMP byte (0-15) for rules.ini section lookup — that's how the
    // KNOWN_LAND_TYPES table is indexed.  The mapped land_type (0-7) is already
    // stored on the metadata for passability matrix lookups.
    let Some(semantics) = terrain_rules
        .semantics_for_land_type(metadata.raw_land_type)
        .copied()
    else {
        if warned_unknown_land_types.insert(metadata.raw_land_type) {
            log::warn!(
                "Unknown TMP LandType byte {}; falling back to tileset-name heuristics",
                metadata.raw_land_type
            );
        }
        return;
    };

    metadata.terrain_class = semantics.terrain_class;
    metadata.speed_costs = semantics.speed_costs;
    metadata.is_water = semantics.water;
    metadata.is_cliff_like = semantics.cliff_like;
    metadata.is_rough = semantics.rough;
    metadata.is_road = semantics.road;
    metadata.ground_blocked = semantics.ground_blocked;
    metadata.build_blocked = !semantics.buildable;
}

fn classify_overlay_effects(
    overlays: Option<&Vec<&OverlayEntry>>,
    overlay_registry: Option<&OverlayTypeRegistry>,
    level: u8,
) -> OverlayEffects {
    let mut result = OverlayEffects::default();
    let Some(entries) = overlays else {
        return result;
    };
    for overlay in entries {
        let name = overlay_registry
            .and_then(|reg| reg.name(overlay.overlay_id))
            .unwrap_or("");
        let is_wall = overlay_registry
            .and_then(|reg| reg.flags(overlay.overlay_id))
            .map(|flags| flags.wall)
            .unwrap_or(false);
        // Bridge overlays identified by hardcoded index, matching original engine.
        let is_bridge = crate::map::overlay_types::is_bridge_overlay_index(overlay.overlay_id);

        let is_tiberium = overlay_registry
            .and_then(|reg| reg.flags(overlay.overlay_id))
            .map(|flags| flags.tiberium)
            .unwrap_or(false);

        // Road/pavement overlays have Land=Road in rules.ini. In the original
        // engine, Wall=yes overlays with Land=Road act as road surfaces, not
        // movement blockers. Only walls WITHOUT Land=Road actually block.
        let is_road_overlay = overlay_registry
            .and_then(|reg| reg.flags(overlay.overlay_id))
            .and_then(|flags| flags.land.as_deref())
            .map(|land| land.eq_ignore_ascii_case("Road"))
            .unwrap_or(false);

        if is_road_overlay {
            result.is_road = true;
        } else if is_wall {
            result.overlay_blocks = true;
        }
        if is_tiberium {
            result.has_tiberium = true;
        }
        if is_bridge && result.bridge_layer.is_none() {
            result.has_bridge_deck = true;
            // Direction determined by index: 24/237=EW, 25/238=NS, rest=Low.
            let direction = match overlay.overlay_id {
                24 | 237 => BridgeDirection::EastWest,
                25 | 238 => BridgeDirection::NorthSouth,
                _ => BridgeDirection::Low,
            };
            // High bridges: deck 4 levels above ground (HighBridgeHeight=4).
            // Low bridges: deck at ground level (no elevation change).
            let deck_level = match direction {
                BridgeDirection::EastWest | BridgeDirection::NorthSouth => level.saturating_add(4),
                BridgeDirection::Low => level,
            };
            if direction == BridgeDirection::Low {
                result.is_low_bridge = true;
            }
            result.bridge_layer = Some(BridgeLayer {
                overlay_id: overlay.overlay_id,
                overlay_name: name.to_string(),
                deck_level,
                direction,
            });
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::tmp_file::TmpTile;
    use crate::map::overlay::TerrainObject;
    use crate::map::overlay_types::OverlayTypeRegistry;
    use crate::rules::ini_parser::IniFile;
    use crate::rules::terrain_rules::{TerrainClass, TerrainRules};
    use std::collections::HashSet;

    fn make_map(
        cells: Vec<MapCell>,
        overlays: Vec<OverlayEntry>,
        terrain_objects: Vec<TerrainObject>,
    ) -> MapFile {
        MapFile {
            header: crate::map::map_file::MapHeader {
                theater: "TEMPERATE".to_string(),
                width: 4,
                height: 4,
                local_left: 0,
                local_top: 0,
                local_width: 4,
                local_height: 4,
            },
            basic: crate::map::basic::BasicSection::default(),
            briefing: crate::map::briefing::BriefingSection::default(),
            preview: crate::map::preview::PreviewSection::default(),
            cells,
            entities: Vec::new(),
            overlays,
            terrain_objects,
            waypoints: HashMap::new(),
            cell_tags: HashMap::new(),
            tags: HashMap::new(),
            triggers: HashMap::new(),
            events: HashMap::new(),
            actions: HashMap::new(),
            local_variables: HashMap::new(),
            trigger_graph: crate::map::trigger_graph::TriggerGraph::default(),
            special_flags: crate::map::basic::SpecialFlagsSection::default(),
            ini: IniFile::from_str(""),
        }
    }

    #[test]
    fn test_resolved_grid_preserves_raw_fields_and_fills_clear_cells() {
        let map = make_map(
            vec![MapCell {
                rx: 1,
                ry: 1,
                tile_index: 5,
                sub_tile: 3,
                z: 2,
            }],
            Vec::new(),
            Vec::new(),
        );
        let grid = ResolvedTerrainGrid::build(&map, None, None, None, None, false, 0);
        assert_eq!(grid.width(), 2);
        assert_eq!(grid.height(), 2);

        let cell = grid.cell(1, 1).expect("resolved cell");
        assert_eq!(cell.source_tile_index, 5);
        assert_eq!(cell.source_sub_tile, 3);
        assert_eq!(cell.final_tile_index, 5);
        assert_eq!(cell.final_sub_tile, 3);
        assert_eq!(cell.level, 2);
        assert!(!cell.filled_clear);

        let clear = grid.cell(0, 0).expect("filled clear");
        assert!(clear.filled_clear);
        assert_eq!(clear.final_tile_index, 0);
        assert_eq!(clear.level, 0);
    }

    #[test]
    fn test_merge_tmp_metadata_reads_land_and_slope_bytes() {
        let mut metadata = TileMetadata::default();
        let tile = TmpTile {
            height: 4,
            terrain_type: 7,
            ramp_type: 2,
            radar_left: [100, 120, 80],
            radar_right: [90, 110, 70],
            pixels: Vec::new(),
            depth: Vec::new(),
            pixel_width: 60,
            pixel_height: 30,
            offset_x: -5,
            offset_y: -6,
            has_damaged_data: false,
        };
        merge_tmp_metadata(&mut metadata, &tile);
        assert_eq!(metadata.land_type, 7);
        assert_eq!(metadata.slope_type, 2);
        assert_eq!(metadata.template_height, 4);
        assert_eq!(metadata.render_offset_x, -5);
        assert_eq!(metadata.render_offset_y, -6);
        assert!(metadata.has_ramp);
        assert!(metadata.has_tmp_metadata);
        assert_eq!(metadata.radar_left, [100, 120, 80]);
        assert_eq!(metadata.radar_right, [90, 110, 70]);
    }

    #[test]
    fn test_canonical_ramp_detection_only_marks_slope_types_one_to_four() {
        assert_eq!(canonical_ramp_from_slope_type(1), Some(RampDirection::West));
        assert_eq!(
            canonical_ramp_from_slope_type(4),
            Some(RampDirection::South)
        );
        assert_eq!(canonical_ramp_from_slope_type(0), None);
        assert_eq!(canonical_ramp_from_slope_type(7), None);
    }

    #[test]
    fn test_bridge_overlay_creates_upper_layer_without_ground_block() {
        // BRIDGE1 is hardcoded at overlay index 24 in the original engine.
        // Build a registry large enough so index 24 resolves to "BRIDGE1".
        let mut ini_str = String::from("[OverlayTypes]\n");
        for i in 0..24 {
            ini_str.push_str(&format!("{i}=FILLER{i}\n"));
        }
        ini_str.push_str("24=BRIDGE1\n");
        let ini = IniFile::from_str(&ini_str);
        let reg = OverlayTypeRegistry::from_ini(&ini);
        let effects = classify_overlay_effects(
            Some(&vec![&OverlayEntry {
                rx: 0,
                ry: 0,
                overlay_id: 24,
                frame: 0,
            }]),
            Some(&reg),
            3,
        );
        assert!(effects.has_bridge_deck);
        assert!(!effects.overlay_blocks);
        assert_eq!(
            effects
                .bridge_layer
                .as_ref()
                .map(|b| b.overlay_name.as_str()),
            Some("BRIDGE1")
        );
        // BRIDGE1 = EastWest high bridge: deck_level = ground(3) + HighBridgeHeight(4) = 7.
        assert_eq!(effects.bridge_layer.as_ref().map(|b| b.deck_level), Some(7));
        assert_eq!(
            effects.bridge_layer.as_ref().map(|b| b.direction),
            Some(BridgeDirection::EastWest)
        );
    }

    #[test]
    fn test_rules_backed_land_type_overrides_tileset_heuristics_when_tmp_exists() {
        let terrain_rules =
            TerrainRules::from_ini(&IniFile::from_str("[Rough]\nBuildable=yes\nTrack=75%\n"));
        let mut metadata = metadata_from_set_name(Some("Water"), Some(2));
        let tile = TmpTile {
            height: 0,
            terrain_type: 14,
            ramp_type: 0,
            radar_left: [0, 0, 0],
            radar_right: [0, 0, 0],
            pixels: Vec::new(),
            depth: Vec::new(),
            pixel_width: 60,
            pixel_height: 30,
            offset_x: 0,
            offset_y: 0,
            has_damaged_data: false,
        };
        merge_tmp_metadata(&mut metadata, &tile);
        let mut warned = HashSet::new();
        apply_land_type_semantics(&mut metadata, Some(&terrain_rules), &mut warned);

        assert_eq!(metadata.terrain_class, TerrainClass::Rough);
        assert!(metadata.is_rough);
        assert!(!metadata.is_water);
        assert!(!metadata.ground_blocked);
        assert!(!metadata.build_blocked);
    }

    #[test]
    fn test_unknown_land_type_keeps_tileset_fallback() {
        // Use a LandType byte outside the 0-15 range (all 0-15 are now mapped).
        // Byte 200 is genuinely unknown and should fall back to tileset-name heuristics.
        let terrain_rules = TerrainRules::from_ini(&IniFile::from_str(""));
        let mut metadata = metadata_from_set_name(Some("Water Cliffs"), Some(5));
        let tile = TmpTile {
            height: 0,
            terrain_type: 200,
            ramp_type: 0,
            radar_left: [0, 0, 0],
            radar_right: [0, 0, 0],
            pixels: Vec::new(),
            depth: Vec::new(),
            pixel_width: 60,
            pixel_height: 30,
            offset_x: 0,
            offset_y: 0,
            has_damaged_data: false,
        };
        merge_tmp_metadata(&mut metadata, &tile);
        let mut warned = HashSet::new();
        apply_land_type_semantics(&mut metadata, Some(&terrain_rules), &mut warned);

        assert_eq!(metadata.terrain_class, TerrainClass::Water);
        assert!(metadata.is_water);
        assert!(metadata.is_cliff_like);
        assert!(metadata.ground_blocked);
        assert_eq!(warned, HashSet::from([200]));
    }

    #[test]
    fn test_tileset_water_fallback_sets_water_land_type() {
        let metadata = metadata_from_set_name(Some("TEMPERATE WATER"), Some(5));
        assert!(metadata.is_water);
        assert_eq!(
            metadata.land_type,
            crate::sim::pathfinding::passability::LandType::Water.as_index()
        );
    }

    #[test]
    fn test_canonical_ramp_is_ground_passable_but_stays_non_buildable() {
        let map = make_map(
            vec![MapCell {
                rx: 0,
                ry: 0,
                tile_index: 0,
                sub_tile: 0,
                z: 0,
            }],
            Vec::new(),
            Vec::new(),
        );
        let terrain_rules = TerrainRules::from_ini(&IniFile::from_str("[Cliff]\nBuildable=no\n"));
        let mut metadata = TileMetadata {
            has_tmp_metadata: true,
            raw_land_type: 15,
            land_type: crate::sim::pathfinding::passability::tmp_terrain_to_land_type(15)
                .as_index(),
            slope_type: 2,
            terrain_class: TerrainClass::Cliff,
            ground_blocked: true,
            build_blocked: true,
            is_cliff_like: true,
            has_ramp: true,
            ..TileMetadata::default()
        };
        let mut warned = HashSet::new();
        apply_land_type_semantics(&mut metadata, Some(&terrain_rules), &mut warned);

        let canonical_ramp = canonical_ramp_from_slope_type(metadata.slope_type);
        let base_ground_walk_blocked = canonical_ramp.is_none() && metadata.ground_blocked;
        assert!(!base_ground_walk_blocked);
        let grid = ResolvedTerrainGrid::from_cells(
            1,
            1,
            vec![ResolvedTerrainCell {
                rx: 0,
                ry: 0,
                source_tile_index: 0,
                source_sub_tile: 0,
                final_tile_index: 0,
                final_sub_tile: 0,
                level: 0,
                filled_clear: false,
                tileset_index: Some(0),
                land_type: metadata.land_type,
                slope_type: metadata.slope_type,
                template_height: 0,
                render_offset_x: 0,
                render_offset_y: 0,
                terrain_class: metadata.terrain_class,
                speed_costs: metadata.speed_costs,
                is_water: false,
                is_cliff_like: true,
                is_rough: false,
                is_road: false,
                is_cliff_redraw: false,
                variant: 0,
                has_ramp: true,
                canonical_ramp,
                ground_walk_blocked: false,
                terrain_object_blocks: false,
                overlay_blocks: false,
                base_build_blocked: true,
                build_blocked: true,
                has_bridge_deck: false,
                bridge_walkable: false,
                bridge_transition: false,
                bridge_deck_level: 0,
                bridge_layer: None,
                radar_left: [0, 0, 0],
                radar_right: [0, 0, 0],
            }],
        );
        let cell = grid.cell(0, 0).expect("resolved ramp cell");
        assert_eq!(cell.canonical_ramp, Some(RampDirection::North));
        assert!(!cell.ground_walk_blocked);
        assert!(cell.build_blocked);
        assert_eq!(map.header.width, 4);
    }

    #[test]
    fn test_cliff_redraw_flag_set_when_height_diff_ge_4() {
        // Cell at (3,3) z=6, neighbor at (1,1) z=0. Height diff 6 >= 4.
        let map = make_map(
            vec![
                MapCell {
                    rx: 1,
                    ry: 1,
                    tile_index: 0,
                    sub_tile: 0,
                    z: 0,
                },
                MapCell {
                    rx: 2,
                    ry: 1,
                    tile_index: 0,
                    sub_tile: 0,
                    z: 0,
                },
                MapCell {
                    rx: 1,
                    ry: 2,
                    tile_index: 0,
                    sub_tile: 0,
                    z: 0,
                },
                MapCell {
                    rx: 2,
                    ry: 2,
                    tile_index: 0,
                    sub_tile: 0,
                    z: 0,
                },
                MapCell {
                    rx: 3,
                    ry: 3,
                    tile_index: 0,
                    sub_tile: 0,
                    z: 6,
                },
            ],
            Vec::new(),
            Vec::new(),
        );
        let grid = ResolvedTerrainGrid::build(&map, None, None, None, None, false, 0);
        let cell = grid.cell(3, 3).expect("high cell");
        assert!(
            cell.is_cliff_redraw,
            "height diff 6 >= 4 should flag cliff redraw"
        );
    }

    #[test]
    fn test_cliff_redraw_flag_not_set_when_height_diff_lt_4() {
        // Cell at (3,3) z=3, neighbors at z=0. Height diff 3 < 4.
        let map = make_map(
            vec![
                MapCell {
                    rx: 1,
                    ry: 1,
                    tile_index: 0,
                    sub_tile: 0,
                    z: 0,
                },
                MapCell {
                    rx: 2,
                    ry: 2,
                    tile_index: 0,
                    sub_tile: 0,
                    z: 0,
                },
                MapCell {
                    rx: 3,
                    ry: 3,
                    tile_index: 0,
                    sub_tile: 0,
                    z: 3,
                },
            ],
            Vec::new(),
            Vec::new(),
        );
        let grid = ResolvedTerrainGrid::build(&map, None, None, None, None, false, 0);
        let cell = grid.cell(3, 3).expect("slightly elevated cell");
        assert!(
            !cell.is_cliff_redraw,
            "height diff 3 < 4 should NOT flag cliff redraw"
        );
    }

    #[test]
    fn cliff_back_impassability_marks_low_cell() {
        // Cell (1,1) at level 0, cell (1,0) at level 4.
        // Neighbor offset (0,-1) means (1,0) is checked from (1,1).
        // Height diff = 4 >= 4 → cell (1,1) should be marked impassable.
        let map = make_map(
            vec![
                MapCell {
                    rx: 0,
                    ry: 0,
                    tile_index: -1,
                    sub_tile: 0,
                    z: 0,
                },
                MapCell {
                    rx: 1,
                    ry: 0,
                    tile_index: -1,
                    sub_tile: 0,
                    z: 4,
                },
                MapCell {
                    rx: 0,
                    ry: 1,
                    tile_index: -1,
                    sub_tile: 0,
                    z: 0,
                },
                MapCell {
                    rx: 1,
                    ry: 1,
                    tile_index: -1,
                    sub_tile: 0,
                    z: 0,
                },
            ],
            Vec::new(),
            Vec::new(),
        );
        let grid = ResolvedTerrainGrid::build(&map, None, None, None, None, false, 2);
        let cell = grid.cell(1, 1).unwrap();
        assert!(
            cell.ground_walk_blocked,
            "Cell at base of cliff should be blocked"
        );
        assert!(
            cell.is_cliff_like,
            "Cell at base of cliff should be cliff-like"
        );
        assert_eq!(
            cell.land_type,
            crate::sim::pathfinding::passability::LandType::Rock.as_index(),
            "Cell at base of cliff should have Rock land type"
        );
    }

    #[test]
    fn cliff_back_impassability_skips_when_disabled() {
        let map = make_map(
            vec![
                MapCell {
                    rx: 0,
                    ry: 0,
                    tile_index: -1,
                    sub_tile: 0,
                    z: 0,
                },
                MapCell {
                    rx: 1,
                    ry: 0,
                    tile_index: -1,
                    sub_tile: 0,
                    z: 4,
                },
                MapCell {
                    rx: 0,
                    ry: 1,
                    tile_index: -1,
                    sub_tile: 0,
                    z: 0,
                },
                MapCell {
                    rx: 1,
                    ry: 1,
                    tile_index: -1,
                    sub_tile: 0,
                    z: 0,
                },
            ],
            Vec::new(),
            Vec::new(),
        );
        // cliff_back_impassability = 0 → disabled
        let grid = ResolvedTerrainGrid::build(&map, None, None, None, None, false, 0);
        let cell = grid.cell(1, 1).unwrap();
        assert!(
            !cell.ground_walk_blocked,
            "Should NOT be blocked when disabled"
        );
    }

    #[test]
    fn cliff_back_impassability_ignores_small_height_diff() {
        let map = make_map(
            vec![
                MapCell {
                    rx: 0,
                    ry: 0,
                    tile_index: -1,
                    sub_tile: 0,
                    z: 0,
                },
                MapCell {
                    rx: 1,
                    ry: 0,
                    tile_index: -1,
                    sub_tile: 0,
                    z: 3,
                },
                MapCell {
                    rx: 0,
                    ry: 1,
                    tile_index: -1,
                    sub_tile: 0,
                    z: 0,
                },
                MapCell {
                    rx: 1,
                    ry: 1,
                    tile_index: -1,
                    sub_tile: 0,
                    z: 0,
                },
            ],
            Vec::new(),
            Vec::new(),
        );
        let grid = ResolvedTerrainGrid::build(&map, None, None, None, None, false, 2);
        let cell = grid.cell(1, 1).unwrap();
        assert!(
            !cell.ground_walk_blocked,
            "Height diff 3 should NOT trigger (threshold is 4)"
        );
    }
}
