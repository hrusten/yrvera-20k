//! Custom-rendered in-game sidebar layout/model.
//!
//! This module stays render-agnostic: it describes sidebar geometry, tab/item
//! state, and hit-testing, while the app/render layers decide how to draw it.
//!
//! Layout matches the original RA2 sidebar:
//!   radar (168x110) -> side1 (168x69) -> tabs (168x16) -> side2 tiled (168x50)
//!   -> side3 (168x26). Cameos in 2-column grid within the side2 region.

mod layout_spec;
pub mod power_bar_anim;
mod sidebar_view;

use crate::sim::production::ProductionCategory;

pub use layout_spec::{SIDEBAR_LAYOUT_FILE_NAME, SidebarChromeLayoutSpec};
pub use power_bar_anim::PowerBarAnimState;
pub use sidebar_view::{build_sidebar_view, build_sidebar_view_with_spec};

/// Original RA2 sidebar chrome width (all SHPs are 168px wide).
pub const SIDEBAR_WIDTH: f32 = 168.0;
pub const SIDEBAR_TOP_INSET: f32 = 50.0;

/// Heights of each chrome piece (from sidec01.mix SHP inspection).
pub const RADAR_HEIGHT: f32 = 110.0;
pub(crate) const SIDE1_HEIGHT: f32 = 69.0;
pub(crate) const TABS_HEIGHT: f32 = 16.0;
pub(crate) const SIDE2_HEIGHT: f32 = 50.0;
pub(crate) const SIDE3_HEIGHT: f32 = 26.0;
pub(crate) const CONTROL_BUTTON_HEIGHT: f32 = 20.0;
pub(crate) const CONTROL_BUTTON_GAP: f32 = 2.0;
pub(crate) const CONTROL_BLOCK_TOP_PAD: f32 = 2.0;
pub(crate) const CONTROL_BLOCK_BOTTOM_PAD: f32 = 4.0;
const MIN_VISIBLE_ROWS: usize = 4;
pub(crate) const RADAR_CONTENT_WIDTH: f32 = 150.0;
pub(crate) const RADAR_CONTENT_HEIGHT: f32 = 96.0;

/// Cameo grid: 2 columns. Standard RA2 cameo icons are 60x48 pixels.
/// They sit within the side2.shp dark slots, overlapping chrome edges slightly
/// (same as the original game). Positions derived from side2.shp analysis.
pub(crate) const CAMEO_COLUMNS: usize = 2;
pub(crate) const CAMEO_W: f32 = 60.0;
pub(crate) const CAMEO_H: f32 = 48.0;
/// Gap between left slot end and right slot start.
pub(crate) const CAMEO_GAP_X: f32 = 8.0;
/// Horizontal offset from sidebar left edge to left cameo.
pub(crate) const CAMEO_INSET_X: f32 = 21.0;
/// Vertical offset from each side2 tile top to the cameo.
pub(crate) const CAMEO_INSET_Y: f32 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.x + self.w && py >= self.y && py <= self.y + self.h
    }
}

pub fn radar_minimap_rect(screen_w: f32) -> Rect {
    radar_minimap_rect_with_spec(screen_w, SidebarChromeLayoutSpec::stock())
}

pub fn radar_minimap_rect_with_spec(screen_w: f32, spec: SidebarChromeLayoutSpec) -> Rect {
    let sw = spec.sidebar_width;
    let radar_x = screen_w - sw + spec.x_offset;
    Rect {
        x: radar_x + (sw - spec.radar_content_width) * 0.5,
        y: spec.top_inset + (spec.radar_height - spec.radar_content_height) * 0.5,
        w: spec.radar_content_width,
        h: spec.radar_content_height,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarTab {
    Building,
    Defense,
    Infantry,
    Vehicle,
}

impl SidebarTab {
    pub fn all() -> [Self; 4] {
        [Self::Building, Self::Defense, Self::Infantry, Self::Vehicle]
    }

    pub fn category(self) -> ProductionCategory {
        match self {
            Self::Building => ProductionCategory::Building,
            Self::Defense => ProductionCategory::Defense,
            Self::Infantry => ProductionCategory::Infantry,
            Self::Vehicle => ProductionCategory::Vehicle,
        }
    }

    pub fn default_active_tab() -> Self {
        default_active_tab()
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Building => "Build",
            Self::Defense => "Def",
            Self::Infantry => "Inf",
            Self::Vehicle => "Veh",
        }
    }

    /// Index into tab00..tab03 SHP array.
    pub fn tab_index(self) -> usize {
        match self {
            Self::Building => 0,
            Self::Defense => 1,
            Self::Infantry => 2,
            Self::Vehicle => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarAction {
    None,
    SelectTab(SidebarTab),
    BuildType(String),
    ArmPlacement(String),
    ClearPlacementMode,
    TogglePauseQueue(ProductionCategory),
    CycleProducer(ProductionCategory),
    CancelBuild(String),
    CancelLastBuild,
    CycleOwner,
    PlaceStarterBase,
    SpawnTestUnits,
    Deploy,
}

#[derive(Debug, Clone)]
pub struct SidebarItem {
    pub rect: Rect,
    pub type_id: String,
    pub display_name: String,
    pub cost: Option<i32>,
    pub has_cameo_art: bool,
    pub queue_category: ProductionCategory,
    pub enabled: bool,
    pub progress: f32,
    pub queued_count: usize,
    /// True when this type is the one actively being produced in its category.
    pub is_building_this_type: bool,
    pub is_ready: bool,
    pub is_armed: bool,
}

impl SidebarItem {
    /// The cameo art slot inside this item (the full item rect IS the cameo).
    pub fn cameo_rect(&self) -> Rect {
        self.rect
    }
}

#[derive(Debug, Clone)]
pub struct SidebarTabButton {
    pub tab: SidebarTab,
    pub rect: Rect,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub struct SidebarControlButton {
    pub rect: Rect,
    pub action: SidebarAction,
    pub label: String,
}

/// Computed vertical offsets for each chrome section, based on screen height.
#[derive(Debug, Clone, Copy)]
pub struct SidebarLayout {
    pub sidebar_x: f32,
    pub radar_y: f32,
    pub side1_y: f32,
    pub tabs_y: f32,
    pub cameo_grid_top: f32,
    pub cameo_grid_bottom: f32,
    pub side3_y: f32,
    /// How many side2 tiles fit in the cameo region.
    pub side2_tile_count: usize,
}

#[derive(Debug, Clone)]
pub struct SidebarView {
    pub panel_rect: Rect,
    pub layout: SidebarLayout,
    pub credits: i32,
    pub power_produced: i32,
    pub power_drained: i32,
    pub credits_frac: f32,
    pub power_frac: f32,
    pub low_power: bool,
    pub scroll_rows: usize,
    pub max_scroll_rows: usize,
    pub tabs: Vec<SidebarTabButton>,
    pub items: Vec<SidebarItem>,
    pub cancel_button: SidebarControlButton,
    pub cycle_owner_button: SidebarControlButton,
    pub starter_base_button: SidebarControlButton,
    pub spawn_test_units_button: SidebarControlButton,
    pub pause_button: Option<SidebarControlButton>,
    pub producer_button: Option<SidebarControlButton>,
}

pub fn default_active_tab() -> SidebarTab {
    SidebarTab::Building
}

pub fn tab_for_category(category: ProductionCategory) -> SidebarTab {
    match category {
        ProductionCategory::Building => SidebarTab::Building,
        ProductionCategory::Defense => SidebarTab::Defense,
        ProductionCategory::Infantry => SidebarTab::Infantry,
        ProductionCategory::Vehicle => SidebarTab::Vehicle,
        ProductionCategory::Aircraft => SidebarTab::Vehicle,
    }
}

/// Compute the vertical layout of chrome sections for a given screen height.
/// The sidebar shell fills the available height; item count only affects scrolling.
pub fn compute_layout(screen_w: f32, screen_h: f32, item_rows: usize) -> SidebarLayout {
    compute_layout_with_spec(
        SidebarChromeLayoutSpec::stock(),
        screen_w,
        screen_h,
        item_rows,
    )
}

pub(crate) fn compute_layout_with_spec(
    spec: SidebarChromeLayoutSpec,
    screen_w: f32,
    screen_h: f32,
    item_rows: usize,
) -> SidebarLayout {
    let sidebar_x = screen_w - spec.sidebar_width + spec.x_offset;
    let radar_y = spec.top_inset;
    let side1_y = radar_y + spec.radar_height;
    let tabs_y = side1_y + spec.side1_height;
    let cameo_grid_top = tabs_y + spec.tabs_height;
    let control_block_h = spec.control_block_top_pad
        + spec.control_button_height * 3.0
        + spec.control_button_gap * 2.0
        + spec.control_block_bottom_pad;

    // Maximum rows that fit between tabs and screen bottom.
    // In the default layout we reserve room for the bottom control block.
    // Fill-to-bottom mode lets the chrome stack consume that space instead.
    let reserved_bottom_h = if spec.fill_to_bottom {
        spec.fill_bottom_margin.max(0.0)
    } else {
        control_block_h
    };
    let max_region_h = screen_h - cameo_grid_top - spec.side3_height - reserved_bottom_h;
    let max_rows = (max_region_h / spec.cameo_row_height).floor().max(1.0) as usize;

    // Default behavior clamps to actual item count so the chrome doesn't show
    // empty rows. Fill-to-bottom mode keeps extending the sidebar chrome stack
    // to the bottom even when there are fewer build items.
    let visible_rows = if spec.fill_to_bottom {
        max_rows.max(MIN_VISIBLE_ROWS)
    } else {
        item_rows.clamp(MIN_VISIBLE_ROWS, max_rows.max(MIN_VISIBLE_ROWS))
    };
    let cameo_region_h = visible_rows as f32 * spec.cameo_row_height;
    let cameo_grid_bottom = cameo_grid_top + cameo_region_h;
    let side3_y = cameo_grid_bottom;

    SidebarLayout {
        sidebar_x,
        radar_y,
        side1_y,
        tabs_y,
        cameo_grid_top,
        cameo_grid_bottom,
        side3_y,
        side2_tile_count: visible_rows,
    }
}

pub fn hit_test(view: &SidebarView, x: f32, y: f32, right_click: bool) -> SidebarAction {
    if !view.panel_rect.contains(x, y) {
        return SidebarAction::None;
    }

    for tab in &view.tabs {
        if tab.rect.contains(x, y) {
            return SidebarAction::SelectTab(tab.tab);
        }
    }

    for item in &view.items {
        if item.rect.contains(x, y) {
            return if right_click {
                // Right-click: cancel one queued item of this type (RA2 standard).
                if item.queued_count > 0 || item.is_ready {
                    SidebarAction::CancelBuild(item.type_id.clone())
                } else {
                    SidebarAction::None
                }
            } else if item.is_ready {
                if item.is_armed {
                    SidebarAction::ClearPlacementMode
                } else {
                    SidebarAction::ArmPlacement(item.type_id.clone())
                }
            } else if item.enabled {
                SidebarAction::BuildType(item.type_id.clone())
            } else {
                SidebarAction::None
            };
        }
    }

    if let Some(button) = view.pause_button.as_ref() {
        if button.rect.contains(x, y) {
            return button.action.clone();
        }
    }
    if let Some(button) = view.producer_button.as_ref() {
        if button.rect.contains(x, y) {
            return button.action.clone();
        }
    }
    for button in [
        &view.cancel_button,
        &view.cycle_owner_button,
        &view.starter_base_button,
        &view.spawn_test_units_button,
    ] {
        if button.rect.contains(x, y) {
            return button.action.clone();
        }
    }

    SidebarAction::None
}

#[cfg(test)]
mod tests {
    use super::{SIDEBAR_WIDTH, SidebarChromeLayoutSpec, compute_layout_with_spec};

    #[test]
    fn stock_spec_matches_legacy_layout_geometry() {
        let layout = compute_layout_with_spec(SidebarChromeLayoutSpec::stock(), 1024.0, 768.0, 0);
        assert_eq!(layout.sidebar_x, 1024.0 - SIDEBAR_WIDTH);
        assert_eq!(layout.radar_y, 50.0);
        assert_eq!(layout.side1_y, 160.0);
        assert_eq!(layout.tabs_y, 229.0);
        assert_eq!(layout.cameo_grid_top, 245.0);
        assert_eq!(layout.side2_tile_count, 4);
        assert_eq!(layout.side3_y, 445.0);
    }
}
