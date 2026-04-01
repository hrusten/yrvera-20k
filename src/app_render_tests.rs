use super::HoverTargetKind;
use crate::app_entity_pick::{
    compute_box_selection_snapshot, compute_click_selection_snapshot, hover_target_at_point,
    pick_enemy_target_stable_id,
};
use crate::app_input::CLICK_SELECT_RADIUS;
use crate::app_sidebar_render::sync_armed_building_placement;
use crate::map::entities::EntityCategory;
use crate::map::houses::HouseAllianceMap;
use crate::map::terrain;
use crate::sim::components::Health;
use crate::sim::entity_store::EntityStore;
use crate::sim::game_entity::GameEntity;
use crate::sim::intern::test_intern;
use crate::sim::production::ReadyBuildingView;
use crate::sim::vision::FogState;
use crate::sim::world::Simulation;
use std::collections::{BTreeMap, BTreeSet};

fn spawn_mobile(store: &mut EntityStore, sid: u64, x: f32, y: f32, owner: &str, selected: bool) {
    let mut entity = GameEntity::new(
        sid,
        0,
        0,
        0,
        0,
        test_intern(owner),
        Health {
            current: 100,
            max: 100,
        },
        test_intern("E1"),
        EntityCategory::Unit,
        0,
        5,
        false,
    );
    entity.position.screen_x = x;
    entity.position.screen_y = y;
    entity.selected = selected;
    store.insert(entity);
}

fn allied_fog_with_visible_cells(
    local_owner: &str,
    allied_owner: &str,
    visible_cells: &[(u16, u16)],
) -> FogState {
    let mut alliances = HouseAllianceMap::default();
    let allied_names = BTreeSet::from([
        local_owner.to_ascii_uppercase(),
        allied_owner.to_ascii_uppercase(),
    ]);
    alliances.insert(local_owner.to_ascii_uppercase(), allied_names.clone());
    alliances.insert(allied_owner.to_ascii_uppercase(), allied_names);

    let mut by_owner = BTreeMap::new();
    let mut visibility = crate::sim::vision::OwnerVisibility::new(64, 64);
    for &(rx, ry) in visible_cells {
        visibility.mark_visible(rx, ry);
    }
    by_owner.insert(crate::sim::intern::test_intern(allied_owner), visibility);

    FogState {
        width: 64,
        height: 64,
        by_owner,
        alliances,
        ..Default::default()
    }
}

#[test]
fn test_click_replace_selects_only_target() {
    let mut store = EntityStore::new();
    spawn_mobile(&mut store, 1, 100.0, 100.0, "Americans", true);
    spawn_mobile(&mut store, 2, 140.0, 100.0, "Americans", false);

    let empty_heights: BTreeMap<(u16, u16), u8> = BTreeMap::new();
    let snapshot = compute_click_selection_snapshot(
        &store,
        None,
        None,
        140.0,
        100.0,
        CLICK_SELECT_RADIUS,
        false,
        None,
        &empty_heights,
        None,
        None,
    )
    .expect("snapshot");
    assert_eq!(snapshot, vec![2]);
}

#[test]
fn test_click_additive_toggles_membership() {
    let mut store = EntityStore::new();
    spawn_mobile(&mut store, 1, 100.0, 100.0, "Americans", true);
    spawn_mobile(&mut store, 2, 140.0, 100.0, "Americans", false);

    let empty_heights: BTreeMap<(u16, u16), u8> = BTreeMap::new();
    let added = compute_click_selection_snapshot(
        &store,
        None,
        None,
        140.0,
        100.0,
        CLICK_SELECT_RADIUS,
        true,
        None,
        &empty_heights,
        None,
        None,
    )
    .expect("snapshot");
    assert_eq!(added, vec![1, 2]);

    let removed = compute_click_selection_snapshot(
        &store,
        None,
        None,
        100.0,
        100.0,
        CLICK_SELECT_RADIUS,
        true,
        None,
        &empty_heights,
        None,
        None,
    )
    .expect("snapshot");
    assert_eq!(removed, Vec::<u64>::new());
}

#[test]
fn test_box_additive_toggles_and_excludes_structures() {
    let mut store = EntityStore::new();
    spawn_mobile(&mut store, 1, 90.0, 90.0, "Americans", true);
    spawn_mobile(&mut store, 2, 130.0, 90.0, "Americans", true);
    spawn_mobile(&mut store, 3, 170.0, 90.0, "Americans", false);
    let mut building = GameEntity::new(
        4,
        0,
        0,
        0,
        0,
        test_intern("Americans"),
        Health {
            current: 100,
            max: 100,
        },
        test_intern("GAPOWR"),
        EntityCategory::Structure,
        0,
        5,
        false,
    );
    building.position.screen_x = 100.0;
    building.position.screen_y = 120.0;
    store.insert(building);

    let snapshot =
        compute_box_selection_snapshot(&store, None, None, 80.0, 80.0, 180.0, 130.0, true, None)
            .expect("snapshot");
    assert_eq!(snapshot, vec![3]);
}

#[test]
fn test_box_replace_can_clear_selection_when_empty() {
    let mut store = EntityStore::new();
    spawn_mobile(&mut store, 1, 90.0, 90.0, "Americans", true);

    let snapshot =
        compute_box_selection_snapshot(&store, None, None, 300.0, 300.0, 340.0, 340.0, false, None)
            .expect("snapshot");
    assert!(snapshot.is_empty());
}

#[test]
fn test_click_selection_allows_visible_allied_units_for_local_owner() {
    let mut store = EntityStore::new();
    let mut entity = GameEntity::new(
        7,
        11,
        10,
        0,
        0,
        test_intern("British"),
        Health {
            current: 100,
            max: 100,
        },
        test_intern("E1"),
        EntityCategory::Unit,
        0,
        5,
        false,
    );
    entity.position.screen_x = 140.0;
    entity.position.screen_y = 100.0;
    store.insert(entity);

    let fog = allied_fog_with_visible_cells("Americans", "British", &[(11, 10)]);

    let empty_heights: BTreeMap<(u16, u16), u8> = BTreeMap::new();
    let snapshot = compute_click_selection_snapshot(
        &store,
        Some(&fog),
        Some("Americans"),
        140.0,
        100.0,
        CLICK_SELECT_RADIUS,
        false,
        None,
        &empty_heights,
        None,
        None,
    )
    .expect("snapshot");

    assert_eq!(snapshot, vec![7]);
}

#[test]
fn test_pick_enemy_target_ignores_hidden_entities() {
    let mut sim = Simulation::new();
    let soviet_id = sim.interner.intern("Soviet");
    let e1_id = sim.interner.intern("E1");

    let (hx, hy) = terrain::iso_to_screen(10, 10, 0);
    let mut hidden = GameEntity::new(
        2,
        10,
        10,
        0,
        0,
        soviet_id,
        Health {
            current: 100,
            max: 100,
        },
        e1_id,
        EntityCategory::Unit,
        0,
        5,
        false,
    );
    hidden.position.screen_x = hx;
    hidden.position.screen_y = hy;
    sim.entities.insert(hidden);

    let empty_heights: BTreeMap<(u16, u16), u8> = BTreeMap::new();
    let picked_hidden =
        pick_enemy_target_stable_id(&sim, hx, hy, "Americans", false, None, &empty_heights, None);
    assert!(
        picked_hidden.is_none(),
        "Hidden enemy must not be targetable"
    );

    let (vx, vy) = terrain::iso_to_screen(11, 10, 0);
    let mut visible = GameEntity::new(
        3,
        11,
        10,
        0,
        0,
        soviet_id,
        Health {
            current: 100,
            max: 100,
        },
        e1_id,
        EntityCategory::Unit,
        0,
        5,
        false,
    );
    visible.position.screen_x = vx;
    visible.position.screen_y = vy;
    sim.entities.insert(visible);
    sim.fog
        .mark_visible_for_owner(crate::sim::intern::test_intern("Americans"), 11, 10);

    let picked_visible =
        pick_enemy_target_stable_id(&sim, vx, vy, "Americans", false, None, &empty_heights, None);
    assert_eq!(picked_visible, Some(3));

    let still_hidden =
        pick_enemy_target_stable_id(&sim, hx, hy, "Americans", false, None, &empty_heights, None);
    assert_ne!(still_hidden, Some(2));
}

#[test]
fn test_hover_target_distinguishes_friendly_and_enemy_categories() {
    let mut sim = Simulation::new();
    let americans_id = sim.interner.intern("Americans");
    let soviet_id = sim.interner.intern("Soviet");
    let gapowr_id = sim.interner.intern("GAPOWR");
    let e1_id = sim.interner.intern("E1");

    // Compute screen positions using iso_to_screen, offset to cell center so
    // screen_to_iso round-trip resolves back to the correct cell.
    let half_tile = terrain::TILE_WIDTH / 2.0;
    let (fsx, fsy) = terrain::iso_to_screen(5, 5, 0);
    let (friendly_sx, friendly_sy) = (fsx + half_tile, fsy);
    let mut friendly = GameEntity::new(
        10,
        5,
        5,
        0,
        0,
        americans_id,
        Health {
            current: 100,
            max: 100,
        },
        gapowr_id,
        EntityCategory::Structure,
        0,
        5,
        false,
    );
    friendly.position.screen_x = friendly_sx;
    friendly.position.screen_y = friendly_sy;
    sim.entities.insert(friendly);

    let (esx, esy) = terrain::iso_to_screen(20, 5, 0);
    let (enemy_sx, enemy_sy) = (esx + half_tile, esy);
    let mut enemy = GameEntity::new(
        11,
        20,
        5,
        0,
        0,
        soviet_id,
        Health {
            current: 100,
            max: 100,
        },
        e1_id,
        EntityCategory::Unit,
        0,
        5,
        false,
    );
    enemy.position.screen_x = enemy_sx;
    enemy.position.screen_y = enemy_sy;
    sim.entities.insert(enemy);
    sim.fog
        .mark_visible_for_owner(crate::sim::intern::test_intern("Americans"), 20, 5);

    // Provide empty height maps — structure picking now uses foundation-based hit testing.
    let empty_heights: BTreeMap<(u16, u16), u8> = BTreeMap::new();
    let friendly_hover = hover_target_at_point(
        &sim,
        friendly_sx,
        friendly_sy,
        "Americans",
        false,
        None,
        &empty_heights,
        None,
    )
    .expect("friendly hover");
    assert_eq!(friendly_hover.kind, HoverTargetKind::FriendlyStructure);
    assert_eq!(friendly_hover.stable_id, 10);

    let enemy_hover = hover_target_at_point(
        &sim,
        enemy_sx,
        enemy_sy,
        "Americans",
        false,
        None,
        &empty_heights,
        None,
    )
    .expect("enemy hover");
    assert_eq!(enemy_hover.kind, HoverTargetKind::EnemyUnit);
    assert_eq!(enemy_hover.stable_id, 11);
}

#[test]
fn test_ready_buildings_do_not_auto_arm_placement() {
    let mut armed = None;
    let mut preview = None;
    let ready = vec![ReadyBuildingView {
        type_id: crate::sim::intern::test_intern("GAPOWR"),
        display_name: "Power Plant".to_string(),
        queue_category: crate::sim::production::ProductionCategory::Building,
    }];

    sync_armed_building_placement(&mut armed, &mut preview, &ready, None);

    assert!(
        armed.is_none(),
        "ready building should not auto-arm placement"
    );
    assert!(preview.is_none());
}

#[test]
fn test_invalid_armed_building_clears_when_not_ready() {
    let mut armed = Some("GAPOWR".to_string());
    let mut preview = Some(crate::sim::production::BuildingPlacementPreview {
        type_id: crate::sim::intern::test_intern("GAPOWR"),
        rx: 5,
        ry: 5,
        width: 2,
        height: 2,
        valid: false,
        reason: None,
        cell_valid: vec![false; 4],
    });

    sync_armed_building_placement(&mut armed, &mut preview, &[], None);

    assert!(armed.is_none());
    assert!(preview.is_none());
}
