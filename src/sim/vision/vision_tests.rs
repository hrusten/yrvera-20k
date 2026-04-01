//! Tests for the fog/shroud visibility system.

use super::*;
use crate::map::entities::EntityCategory;
use crate::sim::components::Health;
use crate::sim::entity_store::EntityStore;
use crate::sim::game_entity::GameEntity;
use crate::sim::intern;

fn spawn_with_vision(store: &mut EntityStore, id: u64, owner: &str, rx: u16, ry: u16, range: u16) {
    let entity = GameEntity::new(
        id,
        rx,
        ry,
        0,
        0,
        intern::test_intern(owner),
        Health {
            current: 100,
            max: 100,
        },
        intern::test_intern("E1"),
        EntityCategory::Infantry,
        0,
        range,
        false,
    );
    store.insert(entity);
}

fn ti() -> intern::StringInterner {
    intern::test_interner()
}

/// Helper: default VisionConfig for tests.
fn default_config() -> VisionConfig {
    VisionConfig::default()
}

// -- Flat grid unit tests --

#[test]
fn test_owner_visibility_basic() {
    let mut vis = OwnerVisibility::new(10, 10);
    assert!(!vis.is_visible(3, 3));
    assert!(!vis.is_revealed(3, 3));

    vis.mark_visible(3, 3);
    assert!(vis.is_visible(3, 3));
    assert!(vis.is_revealed(3, 3));

    // Out of bounds returns false.
    assert!(!vis.is_visible(10, 0));
    assert!(!vis.is_revealed(0, 10));
}

#[test]
fn test_merge_revealed_preserves_bits() {
    let mut old = OwnerVisibility::new(8, 8);
    old.mark_visible(2, 2);
    old.mark_visible(4, 4);

    // New grid has no revealed bits yet.
    let mut new = OwnerVisibility::new(8, 8);
    assert!(!new.is_revealed(2, 2));

    new.merge_revealed_from(&old);
    // Revealed bits carried over.
    assert!(new.is_revealed(2, 2));
    assert!(new.is_revealed(4, 4));
    // Visible bits were NOT carried (only revealed).
    assert!(!new.is_visible(2, 2));
}

#[test]
fn test_merge_revealed_different_dimensions() {
    let mut old = OwnerVisibility::new(10, 10);
    old.mark_visible(5, 5);

    let mut new = OwnerVisibility::new(8, 8);
    new.merge_revealed_from(&old);
    assert!(new.is_revealed(5, 5));

    // Cell (9,9) was in old but outside new's bounds — silently skipped.
    assert!(!new.is_revealed(9, 9));
}

// -- Recompute visibility integration tests --

#[test]
fn test_recompute_visibility_reveals_expected_cells() {
    let mut store = EntityStore::new();
    spawn_with_vision(&mut store, 1, "Americans", 5, 5, 2);

    let fog = recompute_owner_visibility(
        &store,
        Some(&PathGrid::new(16, 16)),
        &Default::default(),
        &default_config(),
        &ti(),
    );
    assert!(fog.is_cell_visible(intern::test_intern("Americans"), 5, 5));
    assert!(fog.is_cell_visible(intern::test_intern("Americans"), 7, 5));
    assert!(fog.is_cell_visible(intern::test_intern("Americans"), 5, 7));
    assert!(!fog.is_cell_visible(intern::test_intern("Americans"), 8, 5));
    assert!(fog.is_cell_revealed(intern::test_intern("Americans"), 6, 6));
}

#[test]
fn test_recompute_visibility_clamps_to_grid_bounds() {
    let mut store = EntityStore::new();
    spawn_with_vision(&mut store, 1, "Americans", 0, 0, 4);

    let fog = recompute_owner_visibility(
        &store,
        Some(&PathGrid::new(3, 3)),
        &Default::default(),
        &default_config(),
        &ti(),
    );
    assert!(fog.is_cell_visible(intern::test_intern("Americans"), 0, 0));
    assert!(fog.is_cell_visible(intern::test_intern("Americans"), 2, 2));
    assert!(!fog.is_cell_visible(intern::test_intern("Americans"), 3, 0));
    assert_eq!(fog.width, 3);
    assert_eq!(fog.height, 3);
}

#[test]
fn test_recompute_visibility_tracks_multiple_owners() {
    let mut store = EntityStore::new();
    spawn_with_vision(&mut store, 1, "Americans", 2, 2, 1);
    spawn_with_vision(&mut store, 2, "Soviet", 10, 10, 1);

    let fog = recompute_owner_visibility(
        &store,
        Some(&PathGrid::new(16, 16)),
        &Default::default(),
        &default_config(),
        &ti(),
    );
    assert!(fog.is_cell_visible(intern::test_intern("Americans"), 2, 2));
    assert!(!fog.is_cell_visible(intern::test_intern("Americans"), 10, 10));
    assert!(fog.is_cell_visible(intern::test_intern("Soviet"), 10, 10));
}

#[test]
fn test_allied_visibility_is_shared() {
    let mut store = EntityStore::new();
    spawn_with_vision(&mut store, 1, "Americans", 4, 4, 1);
    let mut alliances = HouseAllianceMap::new();
    alliances
        .entry("AMERICANS".to_string())
        .or_default()
        .insert("ALLIANCE".to_string());
    alliances
        .entry("ALLIANCE".to_string())
        .or_default()
        .insert("AMERICANS".to_string());

    let mut fog = recompute_owner_visibility(
        &store,
        Some(&PathGrid::new(16, 16)),
        &alliances,
        &default_config(),
        &ti(),
    );
    // Build merged grid so Alliance sees Americans' vision via the alliance.
    fog.build_merged_for(intern::test_intern("Alliance"), &ti());
    assert!(fog.is_cell_visible(intern::test_intern("Alliance"), 4, 4));
    assert!(fog.is_friendly("Alliance", "Americans"));
}

// -- Sight cap tests --

#[test]
fn test_sight_capped_at_max_range() {
    let mut store = EntityStore::new();
    // Spawn with sight=15, which exceeds MAX_SIGHT_RANGE (10).
    spawn_with_vision(&mut store, 1, "Americans", 20, 20, 15);

    let fog = recompute_owner_visibility(
        &store,
        Some(&PathGrid::new(50, 50)),
        &Default::default(),
        &default_config(),
        &ti(),
    );
    // Cell at distance 10 should be visible (exactly at MAX_SIGHT_RANGE).
    assert!(fog.is_cell_visible(intern::test_intern("Americans"), 30, 20));
    // Cell at distance 11 should NOT be visible (capped).
    assert!(!fog.is_cell_visible(intern::test_intern("Americans"), 31, 20));
}

#[test]
fn test_veteran_sight_bonus() {
    let mut store = EntityStore::new();
    // Spawn veteran unit (veterancy >= 100) with base sight 5.
    let entity = GameEntity::new(
        1,
        10,
        10,
        0,
        0,
        intern::test_intern("Americans"),
        Health {
            current: 100,
            max: 100,
        },
        intern::test_intern("E1"),
        EntityCategory::Infantry,
        100, // veterancy >= 100
        5,   // vision_range
        false,
    );
    store.insert(entity);

    let config = VisionConfig {
        veteran_sight_bonus: 2,
        leptons_per_sight_increase: 0,
        reveal_by_height: false,
    };
    let fog = recompute_owner_visibility(
        &store,
        Some(&PathGrid::new(30, 30)),
        &Default::default(),
        &config,
        &ti(),
    );
    // Effective sight = 5 + 2 = 7. Cell at distance 7 should be visible.
    assert!(fog.is_cell_visible(intern::test_intern("Americans"), 17, 10));
    // Cell at distance 8 should not be visible.
    assert!(!fog.is_cell_visible(intern::test_intern("Americans"), 18, 10));
}

#[test]
fn test_elevation_sight_bonus_z8_gives_one_extra_cell() {
    let mut store = EntityStore::new();
    // z=8, LeptonsPerSightIncrease=2000: bonus = 8*256/2000 = 1 (integer division).
    let entity = GameEntity::new(
        1,
        10,
        10,
        8,
        0,
        intern::test_intern("Americans"),
        Health {
            current: 100,
            max: 100,
        },
        intern::test_intern("E1"),
        EntityCategory::Infantry,
        0,
        5,
        false,
    );
    store.insert(entity);
    let config = VisionConfig {
        veteran_sight_bonus: 0,
        leptons_per_sight_increase: 2000,
        reveal_by_height: false,
    };
    let fog = recompute_owner_visibility(
        &store,
        Some(&PathGrid::new(30, 30)),
        &Default::default(),
        &config,
        &ti(),
    );
    // Effective = 5 + 1 = 6. Cell at distance 6 visible, 7 not.
    assert!(fog.is_cell_visible(intern::test_intern("Americans"), 16, 10));
    assert!(!fog.is_cell_visible(intern::test_intern("Americans"), 17, 10));
}

#[test]
fn test_elevation_sight_bonus_z0_gives_no_bonus() {
    let mut store = EntityStore::new();
    let entity = GameEntity::new(
        1,
        10,
        10,
        0,
        0,
        intern::test_intern("Americans"),
        Health {
            current: 100,
            max: 100,
        },
        intern::test_intern("E1"),
        EntityCategory::Infantry,
        0,
        5,
        false,
    );
    store.insert(entity);
    let config = VisionConfig {
        veteran_sight_bonus: 0,
        leptons_per_sight_increase: 2000,
        reveal_by_height: false,
    };
    let fog = recompute_owner_visibility(
        &store,
        Some(&PathGrid::new(30, 30)),
        &Default::default(),
        &config,
        &ti(),
    );
    // z=0 → bonus = 0. Effective = 5. Cell at distance 5 visible, 6 not.
    assert!(fog.is_cell_visible(intern::test_intern("Americans"), 15, 10));
    assert!(!fog.is_cell_visible(intern::test_intern("Americans"), 16, 10));
}

#[test]
fn test_elevation_sight_bonus_disabled_when_zero() {
    let mut store = EntityStore::new();
    // High z — would give large bonus if enabled.
    let entity = GameEntity::new(
        1,
        10,
        10,
        16,
        0,
        intern::test_intern("Americans"),
        Health {
            current: 100,
            max: 100,
        },
        intern::test_intern("E1"),
        EntityCategory::Infantry,
        0,
        5,
        false,
    );
    store.insert(entity);
    // leptons_per_sight_increase=0 → elevation bonus disabled.
    let config = VisionConfig {
        veteran_sight_bonus: 0,
        leptons_per_sight_increase: 0,
        reveal_by_height: false,
    };
    let fog = recompute_owner_visibility(
        &store,
        Some(&PathGrid::new(30, 30)),
        &Default::default(),
        &config,
        &ti(),
    );
    // Effective = 5 only. Cell at 5 visible, 6 not.
    assert!(fog.is_cell_visible(intern::test_intern("Americans"), 15, 10));
    assert!(!fog.is_cell_visible(intern::test_intern("Americans"), 16, 10));
}

#[test]
fn test_merged_visibility_fast_path() {
    let mut store = EntityStore::new();
    spawn_with_vision(&mut store, 1, "Americans", 5, 5, 3);

    let mut fog = recompute_owner_visibility(
        &store,
        Some(&PathGrid::new(16, 16)),
        &Default::default(),
        &default_config(),
        &ti(),
    );

    // Before building merged, queries still work (slow fallback).
    assert!(fog.is_cell_visible(intern::test_intern("Americans"), 5, 5));

    // Build merged cache for "Americans".
    fog.build_merged_for(intern::test_intern("Americans"), &ti());

    // Fast path should return the same results.
    assert!(fog.is_cell_visible(intern::test_intern("Americans"), 5, 5));
    assert!(fog.is_cell_visible(intern::test_intern("Americans"), 7, 5));
    assert!(!fog.is_cell_visible(intern::test_intern("Americans"), 9, 5));
    assert!(fog.is_cell_revealed(intern::test_intern("Americans"), 6, 6));
}

#[test]
fn test_reset_explored_for_owner() {
    let mut fog = FogState::default();
    fog.width = 10;
    fog.height = 10;
    fog.mark_visible_for_owner(intern::test_intern("Americans"), 3, 3);
    assert!(fog.is_cell_revealed(intern::test_intern("Americans"), 3, 3));

    fog.reset_explored_for_owner(intern::test_intern("Americans"));
    assert!(!fog.is_cell_revealed(intern::test_intern("Americans"), 3, 3));
    assert!(!fog.is_cell_visible(intern::test_intern("Americans"), 3, 3));
}

// -- Neighbor mask tests --

#[test]
fn test_shroud_edge_mask_interior_cell() {
    // All neighbors also shrouded → mask = 0b1111 (all bits set).
    let fog = FogState::default();
    let mask = fog.shroud_edge_mask(intern::test_intern("Americans"), 5, 5);
    assert_eq!(mask, 0b1111, "all neighbors shrouded → all bits set");
}

#[test]
fn test_shroud_edge_mask_with_revealed_neighbors() {
    let mut fog = FogState {
        width: 16,
        height: 16,
        ..Default::default()
    };
    // Reveal the SE neighbor (rx+1, ry) of cell (5,5) → that's (6,5).
    fog.mark_visible_for_owner(intern::test_intern("Americans"), 6, 5);

    let mask = fog.shroud_edge_mask(intern::test_intern("Americans"), 5, 5);
    // SE bit (bit 1) should be CLEAR because the SE neighbor IS revealed.
    assert_eq!(mask & 0x02, 0, "SE neighbor revealed → bit 1 clear");
    // Other bits should still be set.
    assert_eq!(mask & 0x01, 0x01, "NE neighbor still shrouded");
    assert_eq!(mask & 0x04, 0x04, "SW neighbor still shrouded");
    assert_eq!(mask & 0x08, 0x08, "NW neighbor still shrouded");
}

#[test]
fn test_shroud_edge_mask_at_grid_edge() {
    let fog = FogState::default();
    // Cell at (0,0): NE neighbor is (0, -1) which is OOB (ry underflow) → bit set.
    // NW neighbor is (-1, 0) which is OOB (rx underflow) → bit set.
    let mask = fog.shroud_edge_mask(intern::test_intern("Americans"), 0, 0);
    assert_eq!(mask & 0x01, 0x01, "NE OOB → bit set");
    assert_eq!(mask & 0x08, 0x08, "NW OOB → bit set");
}

#[test]
fn test_shroud_edge_mask_ne_uses_correct_neighbor() {
    // Verify NE checks (rx, ry-1), the edge-sharing neighbor, not (rx+1, ry-1).
    let mut fog = FogState {
        width: 16,
        height: 16,
        ..Default::default()
    };
    // Reveal the NE edge-sharing neighbor of (5,5) → that's (5, 4).
    fog.mark_visible_for_owner(intern::test_intern("Americans"), 5, 4);

    let mask = fog.shroud_edge_mask(intern::test_intern("Americans"), 5, 5);
    assert_eq!(mask & 0x01, 0, "NE neighbor (5,4) revealed → bit 0 clear");

    // The vertex-sharing cell (6, 4) should NOT affect the NE bit.
    let mut fog2 = FogState {
        width: 16,
        height: 16,
        ..Default::default()
    };
    fog2.mark_visible_for_owner(intern::test_intern("Americans"), 6, 4);
    let mask2 = fog2.shroud_edge_mask(intern::test_intern("Americans"), 5, 5);
    assert_eq!(
        mask2 & 0x01,
        0x01,
        "vertex neighbor (6,4) should NOT affect NE bit"
    );
}

// -- SpySat tests --

#[test]
fn test_spy_sat_reveals_all_cells() {
    let mut fog = FogState {
        width: 20,
        height: 20,
        ..Default::default()
    };
    // Initially nothing is visible.
    assert!(!fog.is_cell_visible(intern::test_intern("Americans"), 10, 10));

    let americans_id = intern::test_intern("Americans");
    let interner = ti();
    apply_spy_sat(&mut fog, &[americans_id], &interner);

    // After SpySat, all cells should be visible and revealed.
    assert!(fog.is_cell_visible(intern::test_intern("Americans"), 0, 0));
    assert!(fog.is_cell_visible(intern::test_intern("Americans"), 10, 10));
    assert!(fog.is_cell_visible(intern::test_intern("Americans"), 19, 19));
    assert!(fog.is_cell_revealed(intern::test_intern("Americans"), 15, 15));
}

// -- Gap Generator tests --

#[test]
fn test_gap_generator_suppresses_enemy_visibility() {
    let mut store = EntityStore::new();
    // Spawn a Soviet unit at (10, 10) with sight 8.
    spawn_with_vision(&mut store, 1, "Soviet", 10, 10, 8);

    let mut fog = recompute_owner_visibility(
        &store,
        Some(&PathGrid::new(30, 30)),
        &Default::default(),
        &default_config(),
        &ti(),
    );
    // Soviet can see (10, 10) and nearby.
    assert!(fog.is_cell_visible(intern::test_intern("Soviet"), 10, 10));
    assert!(fog.is_cell_visible(intern::test_intern("Soviet"), 13, 10));

    // Allied gap generator at (12, 10) with radius 5.
    let americans_id = intern::test_intern("Americans");
    let interner = ti();
    apply_gap_generators(&mut fog, &[(americans_id, 12, 10)], 5, &interner);

    // Soviet's vision within gap radius should be suppressed.
    // (13, 10) is distance 1 from gap center (12,10) — inside gap.
    assert!(!fog.is_cell_visible(intern::test_intern("Soviet"), 13, 10));
    // But the gap generator does NOT suppress friendly vision.
    // (Soviet unit at 10,10 is outside the gap center's radius check scope
    // but its own sight is cleared for cells inside the gap.)
}

#[test]
fn test_gap_generator_does_not_suppress_friendly() {
    let mut fog = FogState {
        width: 20,
        height: 20,
        ..Default::default()
    };
    fog.mark_visible_for_owner(intern::test_intern("Americans"), 10, 10);

    // Gap generator owned by Americans — should NOT suppress American vision.
    let americans_id = intern::test_intern("Americans");
    let interner = ti();
    apply_gap_generators(&mut fog, &[(americans_id, 10, 10)], 5, &interner);
    assert!(fog.is_cell_visible(intern::test_intern("Americans"), 10, 10));
}

// -- In-place recompute tests --

#[test]
fn test_in_place_preserves_revealed() {
    let mut store = EntityStore::new();
    spawn_with_vision(&mut store, 1, "Americans", 5, 5, 2);
    let grid = PathGrid::new(16, 16);
    let cfg = default_config();
    let alliances = HouseAllianceMap::default();

    // First compute: reveals cells around (5,5).
    let mut fog = FogState::default();
    recompute_owner_visibility_in_place(
        &mut fog,
        &store,
        Some(&grid),
        &alliances,
        &cfg,
        None,
        &ti(),
    );
    assert!(fog.is_cell_revealed(intern::test_intern("Americans"), 5, 5));
    assert!(fog.is_cell_visible(intern::test_intern("Americans"), 5, 5));

    // Move unit: remove old, spawn at (10, 10).
    store.remove(1);
    spawn_with_vision(&mut store, 2, "Americans", 10, 10, 2);

    // Second compute in-place: (5,5) should still be revealed but not visible.
    recompute_owner_visibility_in_place(
        &mut fog,
        &store,
        Some(&grid),
        &alliances,
        &cfg,
        None,
        &ti(),
    );
    assert!(fog.is_cell_revealed(intern::test_intern("Americans"), 5, 5));
    assert!(!fog.is_cell_visible(intern::test_intern("Americans"), 5, 5));
    assert!(fog.is_cell_visible(intern::test_intern("Americans"), 10, 10));
}

#[test]
fn test_dead_owner_keeps_revealed() {
    let mut store = EntityStore::new();
    spawn_with_vision(&mut store, 1, "Soviet", 5, 5, 2);
    let grid = PathGrid::new(16, 16);
    let cfg = default_config();
    let alliances = HouseAllianceMap::default();

    let mut fog = FogState::default();
    recompute_owner_visibility_in_place(
        &mut fog,
        &store,
        Some(&grid),
        &alliances,
        &cfg,
        None,
        &ti(),
    );
    assert!(fog.is_cell_revealed(intern::test_intern("Soviet"), 5, 5));

    // Remove all Soviet entities.
    store.remove(1);
    recompute_owner_visibility_in_place(
        &mut fog,
        &store,
        Some(&grid),
        &alliances,
        &cfg,
        None,
        &ti(),
    );

    // Soviet's revealed state persists, but nothing is visible.
    assert!(fog.is_cell_revealed(intern::test_intern("Soviet"), 5, 5));
    assert!(!fog.is_cell_visible(intern::test_intern("Soviet"), 5, 5));
}

#[test]
fn test_in_place_matches_fresh() {
    let mut store = EntityStore::new();
    spawn_with_vision(&mut store, 1, "Americans", 5, 5, 3);
    spawn_with_vision(&mut store, 2, "Soviet", 10, 10, 2);
    let grid = PathGrid::new(20, 20);
    let cfg = default_config();
    let alliances = HouseAllianceMap::default();

    // Fresh allocation path.
    let fresh = recompute_owner_visibility(&store, Some(&grid), &alliances, &cfg, &ti());

    // In-place path (from default).
    let mut in_place = FogState::default();
    recompute_owner_visibility_in_place(
        &mut in_place,
        &store,
        Some(&grid),
        &alliances,
        &cfg,
        None,
        &ti(),
    );

    // Both should have identical by_owner contents.
    assert_eq!(fresh.by_owner.len(), in_place.by_owner.len());
    for (owner, fresh_vis) in &fresh.by_owner {
        let ip_vis = in_place
            .by_owner
            .get(owner)
            .expect("owner missing in in-place result");
        assert_eq!(
            fresh_vis.cells_raw(),
            ip_vis.cells_raw(),
            "mismatch for {}",
            owner
        );
    }
}

// -- FLAG_GAP_COVERED tests --

#[test]
fn test_gap_generator_sets_gap_covered_flag() {
    let mut store = EntityStore::new();
    spawn_with_vision(&mut store, 1, "Soviet", 10, 10, 8);

    let mut fog = recompute_owner_visibility(
        &store,
        Some(&PathGrid::new(30, 30)),
        &Default::default(),
        &default_config(),
        &ti(),
    );

    // Before gap: cell is revealed and visible, NOT gap-covered.
    assert!(fog.is_cell_revealed(intern::test_intern("Soviet"), 12, 10));
    assert!(fog.is_cell_visible(intern::test_intern("Soviet"), 12, 10));
    fog.build_merged_for(intern::test_intern("Soviet"), &ti());
    assert!(!fog.is_cell_gap_covered(intern::test_intern("Soviet"), 12, 10));

    // American gap generator at (12, 10) with radius 5.
    let americans_id = intern::test_intern("Americans");
    let interner = ti();
    apply_gap_generators(&mut fog, &[(americans_id, 12, 10)], 5, &interner);
    fog.build_merged_for(intern::test_intern("Soviet"), &ti());

    // Cell should now be gap-covered AND not visible for Soviet.
    assert!(fog.is_cell_gap_covered(intern::test_intern("Soviet"), 12, 10));
    assert!(!fog.is_cell_visible(intern::test_intern("Soviet"), 12, 10));
    // But still revealed (gap doesn't erase exploration).
    assert!(fog.is_cell_revealed(intern::test_intern("Soviet"), 12, 10));
}

#[test]
fn test_gap_covered_not_set_for_friendly() {
    let mut fog = FogState {
        width: 20,
        height: 20,
        ..Default::default()
    };
    fog.mark_visible_for_owner(intern::test_intern("Americans"), 10, 10);

    // Gap owned by Americans — should NOT gap-cover American cells.
    let americans_id = intern::test_intern("Americans");
    let interner = ti();
    apply_gap_generators(&mut fog, &[(americans_id, 10, 10)], 5, &interner);
    fog.build_merged_for(intern::test_intern("Americans"), &ti());

    assert!(!fog.is_cell_gap_covered(intern::test_intern("Americans"), 10, 10));
}

#[test]
fn test_gap_covered_cleared_each_tick() {
    let mut vis = OwnerVisibility::new(10, 10);
    vis.mark_visible(5, 5);
    // Manually set gap-covered bit.
    if let Some(i) = vis.index(5, 5) {
        vis.cells[i] |= 0x04; // FLAG_GAP_COVERED
    }
    assert!(vis.is_gap_covered(5, 5));

    // clear_all_visible should also clear gap-covered.
    vis.clear_all_visible();
    assert!(!vis.is_gap_covered(5, 5));
    // But revealed persists.
    assert!(vis.is_revealed(5, 5));
}

// -- Height-based LOS (RevealByHeight) tests --

#[test]
fn test_height_los_blocks_sight_behind_cliff() {
    // Unit at (5,5) height 0, sight 5.
    // Cliff at (7,5) height 5 should block sight to (8,5).
    let mut vis = OwnerVisibility::new(20, 20);
    let width: u16 = 20;
    let height: u16 = 20;
    let mut hg = vec![0u8; width as usize * height as usize];
    // Place a cliff at (7,5) — 5 levels high (> viewer_level 0 + 3).
    hg[5 * width as usize + 7] = 5;

    reveal_radius_into(&mut vis, 5, 5, 5, 0, true, Some(&hg), width, height);

    // (6,5) is before the cliff — should be visible.
    assert!(vis.is_visible(6, 5));
    // (7,5) is the cliff cell itself — the mirror check for this cell looks at
    // (7+mirror_dx, 5+mirror_dy). Whether it's blocked depends on the mirror table.
    // (8,5) is behind the cliff — its mirror offset points back toward (7,5),
    // which has level 5. Since 0 + 3 < 5, sight is blocked.
    assert!(!vis.is_visible(8, 5));
}

#[test]
fn test_height_los_high_viewer_sees_past_cliff() {
    // Unit at (5,5) height 4, sight 5.
    // Cliff at (7,5) height 5 — viewer_level + 3 = 7, NOT < 5, so LOS passes.
    let mut vis = OwnerVisibility::new(20, 20);
    let width: u16 = 20;
    let height: u16 = 20;
    let mut hg = vec![0u8; width as usize * height as usize];
    hg[5 * width as usize + 7] = 5;

    reveal_radius_into(&mut vis, 5, 5, 5, 4, true, Some(&hg), width, height);

    // High viewer (level 4 + 3 = 7 >= 5) sees past the cliff.
    assert!(vis.is_visible(8, 5));
}

#[test]
fn test_height_los_disabled_when_false() {
    // Same cliff scenario but reveal_by_height=false — should NOT block.
    let mut vis = OwnerVisibility::new(20, 20);
    let width: u16 = 20;
    let height: u16 = 20;
    let mut hg = vec![0u8; width as usize * height as usize];
    hg[5 * width as usize + 7] = 5;

    reveal_radius_into(&mut vis, 5, 5, 5, 0, false, Some(&hg), width, height);

    // With reveal_by_height=false, the cliff doesn't block.
    assert!(vis.is_visible(8, 5));
}

#[test]
fn test_height_los_none_grid_disables_check() {
    // reveal_by_height=true but no height grid — should NOT block.
    let mut vis = OwnerVisibility::new(20, 20);
    let width: u16 = 20;
    let height: u16 = 20;

    reveal_radius_into(&mut vis, 5, 5, 5, 0, true, None, width, height);

    // Without a height grid, all cells in range are visible.
    assert!(vis.is_visible(8, 5));
}
