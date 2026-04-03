//! Sidebar view builder: constructs `SidebarView` from production state.
//!
//! Extracted from sidebar/mod.rs for file-size limits.

use crate::sim::intern::InternedId;
use crate::sim::production::{
    BuildOption, BuildQueueState, ProducerFocusView, ProductionCategory, QueueItemView,
    ReadyBuildingView,
};
use crate::sim::superweapon::SuperWeaponView;

use super::{
    CAMEO_COLUMNS, Rect, SidebarAction, SidebarChromeLayoutSpec, SidebarControlButton, SidebarItem,
    SidebarTab, SidebarTabButton, SidebarView, compute_layout_with_spec,
};

pub fn build_sidebar_view(
    screen_w: f32,
    screen_h: f32,
    active_tab: SidebarTab,
    credits: i32,
    power_produced: i32,
    power_drained: i32,
    tab_button_size: Option<[f32; 2]>,
    queue_items: &[QueueItemView],
    build_options: &[BuildOption],
    ready_buildings: &[ReadyBuildingView],
    armed_building: Option<&str>,
    producer_focus: &[ProducerFocusView],
    scroll_rows: usize,
    interner: Option<&crate::sim::intern::StringInterner>,
) -> SidebarView {
    build_sidebar_view_with_spec(
        SidebarChromeLayoutSpec::stock(),
        screen_w,
        screen_h,
        active_tab,
        credits,
        power_produced,
        power_drained,
        tab_button_size,
        queue_items,
        build_options,
        ready_buildings,
        armed_building,
        producer_focus,
        scroll_rows,
        interner,
        &[],
    )
}

pub fn build_sidebar_view_with_spec(
    layout_spec: SidebarChromeLayoutSpec,
    screen_w: f32,
    screen_h: f32,
    active_tab: SidebarTab,
    credits: i32,
    power_produced: i32,
    power_drained: i32,
    tab_button_size: Option<[f32; 2]>,
    queue_items: &[QueueItemView],
    build_options: &[BuildOption],
    ready_buildings: &[ReadyBuildingView],
    armed_building: Option<&str>,
    producer_focus: &[ProducerFocusView],
    scroll_rows: usize,
    interner: Option<&crate::sim::intern::StringInterner>,
    sw_views: &[SuperWeaponView],
) -> SidebarView {
    // Collect items first to know how many rows we need.
    let selected_category = active_tab.category();
    let mut all_entries = collect_build_entries(
        selected_category,
        queue_items,
        build_options,
        ready_buildings,
        armed_building,
        interner,
        sw_views,
    );
    let total_items = all_entries.len();
    let total_rows = (total_items + CAMEO_COLUMNS - 1) / CAMEO_COLUMNS;

    // Compute layout with actual item row count — sidebar height adapts to content.
    let layout = compute_layout_with_spec(layout_spec, screen_w, screen_h, total_rows);
    let panel_rect = Rect {
        x: layout.sidebar_x,
        y: 0.0,
        w: layout_spec.sidebar_width,
        h: screen_h,
    };
    let credits_frac = (credits.max(0) as f32 / 5000.0).clamp(0.0, 1.0);
    let power_frac = if power_drained <= 0 {
        1.0
    } else {
        (power_produced.max(0) as f32 / power_drained.max(1) as f32).clamp(0.0, 1.0)
    };
    let low_power = power_produced < power_drained;

    // Tab buttons bottom-align to the 16px strip so the extra button height
    // overhangs upward into side1 instead of downward into the cameo grid.
    let tab_count = SidebarTab::all().len();
    let tab_w = tab_button_size.map(|s| s[0]).unwrap_or(28.0);
    let tab_h = tab_button_size.map(|s| s[1]).unwrap_or(27.0);
    let tab_total = tab_w * tab_count as f32;
    let tab_start_x = layout.sidebar_x + (layout_spec.sidebar_width - tab_total) * 0.5;
    let tab_y = layout.cameo_grid_top - tab_h;
    let tabs: Vec<SidebarTabButton> = SidebarTab::all()
        .into_iter()
        .enumerate()
        .map(|(idx, tab)| {
            // Per-tab X nudges: tab00 shifted left 2px, tab03 shifted right 2px.
            let nudge = match idx {
                0 => -2.0,
                1 => -1.0,
                3 => 2.0,
                _ => 0.0,
            };
            SidebarTabButton {
                tab,
                rect: Rect {
                    x: tab_start_x + idx as f32 * tab_w + nudge,
                    y: tab_y,
                    w: tab_w,
                    h: tab_h,
                },
                active: tab == active_tab,
            }
        })
        .collect();

    // Cameo grid positioning.
    let grid_top = layout.cameo_grid_top + layout_spec.cameo_inset_y;
    let row_height = layout_spec.cameo_row_height;
    let visible_rows = layout.side2_tile_count;
    let max_scroll_rows = total_rows.saturating_sub(visible_rows);
    let scroll_rows = scroll_rows.min(max_scroll_rows);

    let visible_items = scroll_rows * CAMEO_COLUMNS;
    let max_visible = visible_rows * CAMEO_COLUMNS;
    let items: Vec<SidebarItem> = all_entries
        .drain(..)
        .skip(visible_items)
        .take(max_visible)
        .enumerate()
        .map(|(idx, entry)| {
            let row = idx / CAMEO_COLUMNS;
            let col = idx % CAMEO_COLUMNS;
            let x = (layout.sidebar_x
                + layout_spec.cameo_inset_x
                + col as f32 * (layout_spec.cameo_width + layout_spec.cameo_gap_x))
                .round();
            let y = (grid_top + row as f32 * row_height).round();
            SidebarItem {
                rect: Rect {
                    x,
                    y,
                    w: layout_spec.cameo_width.round(),
                    h: layout_spec.cameo_height.round(),
                },
                type_id: entry.type_id,
                display_name: entry.display_name,
                cost: entry.cost,
                has_cameo_art: false,
                queue_category: selected_category,
                enabled: entry.enabled,
                progress: entry.progress,
                queued_count: entry.queued_count,
                is_building_this_type: entry.is_building_this_type,
                is_ready: entry.is_ready,
                is_armed: entry.is_armed,
            }
        })
        .collect();

    // Control buttons at bottom of sidebar (below side3).
    let btn_w = layout_spec.sidebar_width * 0.45;
    let btn_h = layout_spec.control_button_height;
    let btn_y = layout.side3_y + layout_spec.side3_height + layout_spec.control_block_top_pad;
    let btn_pad = 4.0 * (layout_spec.sidebar_width / 168.0); // scale padding proportionally
    let btn_x1 = layout.sidebar_x + btn_pad;
    let btn_x2 = layout.sidebar_x + layout_spec.sidebar_width - btn_w - btn_pad;

    let active_queue_exists = queue_items
        .iter()
        .any(|item| item.queue_category == selected_category);
    let active_queue_paused = queue_items
        .iter()
        .find(|item| item.queue_category == selected_category)
        .map(|item| item.state == BuildQueueState::Paused)
        .unwrap_or(false);

    SidebarView {
        panel_rect,
        layout,
        credits,
        power_produced,
        power_drained,
        credits_frac,
        power_frac,
        low_power,
        scroll_rows,
        max_scroll_rows,
        tabs,
        items,
        pause_button: active_queue_exists.then_some(SidebarControlButton {
            rect: Rect {
                x: btn_x1,
                y: btn_y,
                w: btn_w,
                h: btn_h,
            },
            action: SidebarAction::TogglePauseQueue(selected_category),
            label: if active_queue_paused {
                "Resume".to_string()
            } else {
                "Pause".to_string()
            },
        }),
        producer_button: producer_focus
            .iter()
            .any(|f| f.category == selected_category)
            .then_some(SidebarControlButton {
                rect: Rect {
                    x: btn_x2,
                    y: btn_y,
                    w: btn_w,
                    h: btn_h,
                },
                action: SidebarAction::CycleProducer(selected_category),
                label: "Factory".to_string(),
            }),
        cancel_button: SidebarControlButton {
            rect: Rect {
                x: btn_x1,
                y: btn_y + btn_h + layout_spec.control_button_gap,
                w: btn_w,
                h: btn_h,
            },
            action: SidebarAction::CancelLastBuild,
            label: "Cancel".to_string(),
        },
        cycle_owner_button: SidebarControlButton {
            rect: Rect {
                x: btn_x2,
                y: btn_y + btn_h + layout_spec.control_button_gap,
                w: btn_w,
                h: btn_h,
            },
            action: SidebarAction::CycleOwner,
            label: "Owner".to_string(),
        },
        starter_base_button: SidebarControlButton {
            rect: Rect {
                x: btn_x1,
                y: btn_y + (btn_h + layout_spec.control_button_gap) * 2.0,
                w: btn_w,
                h: btn_h,
            },
            action: SidebarAction::PlaceStarterBase,
            label: "Base".to_string(),
        },
        spawn_test_units_button: SidebarControlButton {
            rect: Rect {
                x: btn_x2,
                y: btn_y + (btn_h + layout_spec.control_button_gap) * 2.0,
                w: btn_w,
                h: btn_h,
            },
            action: SidebarAction::SpawnTestUnits,
            label: "Spawn".to_string(),
        },
    }
}

struct BuildEntry {
    type_id: String,
    display_name: String,
    cost: Option<i32>,
    enabled: bool,
    progress: f32,
    queued_count: usize,
    /// True when this type is the one actively being produced in its category.
    is_building_this_type: bool,
    is_ready: bool,
    is_armed: bool,
}

fn collect_build_entries(
    category: ProductionCategory,
    queue_items: &[QueueItemView],
    build_options: &[BuildOption],
    ready_buildings: &[ReadyBuildingView],
    armed_building: Option<&str>,
    interner: Option<&crate::sim::intern::StringInterner>,
    sw_views: &[SuperWeaponView],
) -> Vec<BuildEntry> {
    let armed_id: Option<InternedId> = armed_building.and_then(|s| interner.and_then(|i| i.get(s)));
    let resolve = |id: InternedId| -> String {
        interner.map_or(format!("#{}", id.index()), |i| i.resolve(id).to_string())
    };

    // Superweapon cameos go first on the Defense tab, sorted before regular items.
    let mut sw_entries: Vec<BuildEntry> = Vec::new();
    if category == ProductionCategory::Defense {
        for sw in sw_views {
            // Use sidebar_image (e.g. "INTICON") as the type_id for cameo atlas lookup.
            let type_id = sw
                .sidebar_image
                .as_deref()
                .unwrap_or(&sw.display_name)
                .to_string();
            sw_entries.push(BuildEntry {
                type_id,
                display_name: sw.display_name.clone(),
                cost: None,
                enabled: sw.is_online,
                progress: sw.progress,
                queued_count: 0,
                is_building_this_type: !sw.is_ready && sw.is_online && sw.progress > 0.0,
                is_ready: sw.is_ready,
                is_armed: false,
            });
        }
    }

    // Collect build options, merging ready-building state into matching entries
    // so that a completed building shows "READY" on its existing cameo slot
    // instead of spawning a duplicate entry.
    let mut entries: Vec<BuildEntry> = build_options
        .iter()
        .filter(|opt| {
            opt.queue_category == category
                || (category == ProductionCategory::Vehicle
                    && opt.queue_category == ProductionCategory::Aircraft)
        })
        .map(|opt| {
            // Check if this type has a completed building waiting for placement.
            let is_ready = ready_buildings.iter().any(|r| r.type_id == opt.type_id);
            let is_armed = is_ready && armed_id == Some(opt.type_id);

            if is_ready {
                // Building is done — show as ready for placement.
                BuildEntry {
                    type_id: resolve(opt.type_id),
                    display_name: opt.display_name.clone(),
                    cost: Some(opt.cost),
                    enabled: true,
                    progress: 1.0,
                    queued_count: 1,
                    is_building_this_type: false,
                    is_ready: true,
                    is_armed,
                }
            } else {
                let queued_count = queue_items
                    .iter()
                    .filter(|item| item.type_id == opt.type_id)
                    .count();
                // Check if this type has an item in Building state (actively producing).
                let is_building_this_type = queue_items.iter().any(|item| {
                    item.type_id == opt.type_id
                        && item.state == crate::sim::production::BuildQueueState::Building
                });
                let progress = queue_items
                    .iter()
                    .find(|item| item.type_id == opt.type_id)
                    .map(|item| {
                        let total = item.total_ms.max(1) as f32;
                        (total - item.remaining_ms as f32) / total
                    })
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0);
                BuildEntry {
                    type_id: resolve(opt.type_id),
                    display_name: opt.display_name.clone(),
                    cost: Some(opt.cost),
                    enabled: opt.enabled,
                    progress,
                    queued_count,
                    is_building_this_type,
                    is_ready: false,
                    is_armed: false,
                }
            }
        })
        .collect();

    // Append any ready buildings that don't have a matching build option
    // (edge case: type was removed from buildable list but still in ready queue).
    for r in ready_buildings
        .iter()
        .filter(|r| r.queue_category == category)
    {
        let r_type_str = resolve(r.type_id);
        let already_listed = entries
            .iter()
            .any(|e| e.type_id.eq_ignore_ascii_case(&r_type_str));
        if !already_listed {
            let is_armed = armed_id == Some(r.type_id);
            entries.push(BuildEntry {
                type_id: r_type_str,
                display_name: r.display_name.clone(),
                cost: None,
                enabled: true,
                progress: 1.0,
                queued_count: 1,
                is_building_this_type: false,
                is_ready: true,
                is_armed,
            });
        }
    }

    // Prepend superweapon entries before regular defense items.
    if !sw_entries.is_empty() {
        sw_entries.append(&mut entries);
        return sw_entries;
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::super::{SidebarAction, SidebarTab, build_sidebar_view};

    fn approx_eq(a: f32, b: f32) {
        assert!(
            (a - b).abs() <= f32::EPSILON,
            "expected {a} ~= {b}, diff={}",
            (a - b).abs()
        );
    }

    #[test]
    fn tab_buttons_bottom_align_to_cameo_grid_top() {
        let view = build_sidebar_view(
            1280.0,
            960.0,
            SidebarTab::Building,
            0,
            0,
            0,
            Some([28.0, 27.0]),
            &[],
            &[],
            &[],
            None,
            &[],
            0,
            None,
        );

        for tab in &view.tabs {
            approx_eq(tab.rect.y + tab.rect.h, view.layout.cameo_grid_top);
        }
    }

    #[test]
    fn control_buttons_stay_inside_panel() {
        let view = build_sidebar_view(
            1280.0,
            960.0,
            SidebarTab::Building,
            1000,
            100,
            150,
            Some([28.0, 27.0]),
            &[],
            &[],
            &[],
            None,
            &[],
            0,
            None,
        );

        for button in [
            Some(&view.cancel_button),
            Some(&view.cycle_owner_button),
            Some(&view.starter_base_button),
            Some(&view.spawn_test_units_button),
            view.pause_button.as_ref(),
            view.producer_button.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            assert!(button.rect.x >= view.panel_rect.x);
            assert!(button.rect.y >= view.panel_rect.y);
            assert!(button.rect.x + button.rect.w <= view.panel_rect.x + view.panel_rect.w);
            assert!(button.rect.y + button.rect.h <= view.panel_rect.y + view.panel_rect.h);
        }
    }

    #[test]
    fn hit_test_returns_control_button_actions() {
        let view = build_sidebar_view(
            1280.0,
            960.0,
            SidebarTab::Building,
            1000,
            100,
            150,
            Some([28.0, 27.0]),
            &[],
            &[],
            &[],
            None,
            &[],
            0,
            None,
        );

        let action = super::super::hit_test(
            &view,
            view.cancel_button.rect.x + 1.0,
            view.cancel_button.rect.y + 1.0,
            false,
        );
        assert_eq!(action, SidebarAction::CancelLastBuild);
    }
}
