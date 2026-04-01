//! Locomotor unit tests — verifies locomotor state initialization, speed type mapping,
//! and ObjectType-to-LocomotorState conversion for various unit categories.

use super::*;
use crate::rules::jumpjet_params::JumpjetParams;
use crate::rules::locomotor_type::{LocomotorKind, MovementZone, SpeedType};
use crate::rules::object_type::{ObjectCategory, ObjectType, PipScale};
use crate::util::fixed_math::{SIM_ONE, SimFixed, sim_from_f32};

/// Helper to create a minimal ObjectType with the given locomotor.
fn make_obj(locomotor: LocomotorKind, category: ObjectCategory) -> ObjectType {
    ObjectType {
        id: "TEST".to_string(),
        category,
        name: None,
        cost: 0,
        strength: 100,
        armor: "none".to_string(),
        speed: 6,
        accel_factor: SimFixed::lit("0.03"),
        decel_factor: SimFixed::lit("0.02"),
        slowdown_distance: 512,
        sight: 5,
        tech_level: -1,
        build_time_multiplier: 1.0,
        build_time_multiplier_x1000: 1000,
        owner: vec![],
        required_houses: vec![],
        forbidden_houses: vec![],
        prerequisite: vec![],
        prerequisite_override: vec![],
        build_limit: 0,
        requires_stolen_allied_tech: false,
        requires_stolen_soviet_tech: false,
        requires_stolen_third_tech: false,
        primary: None,
        secondary: None,
        image: "TEST".to_string(),
        power: 0,
        foundation: "1x1".to_string(),
        pixel_selection_bracket_delta: 0,
        build_cat: None,
        adjacent: 6,
        base_normal: true,
        crewed: false,
        voice_select: None,
        voice_move: None,
        voice_attack: None,
        die_sound: None,
        move_sound: None,
        has_turret: false,
        turret_rot: 0,
        turret_anim: None,
        turret_anim_is_voxel: false,
        turret_anim_x: 0,
        turret_anim_y: 0,
        turret_anim_z_adjust: 0,
        guard_range: None,
        explodes: false,
        death_weapon: None,
        spy_sat: false,
        gap_generator: false,
        radar: false,
        radar_invisible: false,
        radar_visible: false,
        harvester: false,
        refinery: false,
        storage: 0,
        free_unit: None,
        dock: vec![],
        queueing_cell: None,
        docking_offset: None,
        unloading_class: None,
        ammo: -1,
        enslaves: None,
        slaves_number: 0,
        slave_regen_rate: 0,
        slave_reload_rate: 0,
        slaved: false,
        harvest_rate: 0,
        resource_gatherer: false,
        resource_destination: false,
        ore_purifier: false,
        locomotor,
        speed_type: SpeedType::Track,
        movement_zone: MovementZone::Normal,
        considered_aircraft: false,
        zfudge_bridge: 7,
        too_big_to_fit_under_bridge: false,
        crashable: false,
        teleporter: false,
        hover_attack: false,
        balloon_hover: false,
        airport_bound: false,
        fighter: false,
        fly_by: false,
        fly_back: false,
        landable: false,
        jumpjet: false,
        jumpjet_params: None,
        deploys_into: None,
        undeploys_into: None,
        factory: None,
        exit_coord: None,
        crushable: false,
        omni_crusher: false,
        omni_crush_resistant: false,
        engineer: false,
        deployer: false,
        capturable: false,
        repairable: true,
        can_be_occupied: false,
        can_occupy_fire: false,
        show_occupant_pips: false,
        passengers: 0,
        size_limit: 0,
        size: 3,
        open_topped: false,
        gunner: false,
        ifv_mode: 0,
        max_number_occupants: 0,
        occupier: false,
        assaulter: false,
        occupy_weapon: None,
        elite_occupy_weapon: None,
        occupy_pip: 7,
        pip_scale: PipScale::None,
        infantry_absorb: false,
        unit_absorb: false,
        weapon_list: vec![],
        attack_cursor_on_friendlies: false,
        sabotage_cursor: false,
        unit_repair: false,
        unit_reload: false,
        helipad: false,
        number_of_docks: 1,
        toggle_power: false,
        powered: false,
        can_disguise: false,
        wall: false,
        light_visibility: 0,
        light_intensity: 0.0,
        light_red_tint: 1.0,
        light_green_tint: 1.0,
        light_blue_tint: 1.0,
        water_bound: false,
        naval: false,
        number_impassable_rows: -1,
    }
}

#[test]
fn test_drive_locomotor() {
    let obj = make_obj(LocomotorKind::Drive, ObjectCategory::Vehicle);
    let state = LocomotorState::from_object_type(&obj, 1500);
    assert_eq!(state.kind, LocomotorKind::Drive);
    assert_eq!(state.layer, MovementLayer::Ground);
    assert_eq!(state.phase, GroundMovePhase::Idle);
    assert_eq!(state.air_phase, AirMovePhase::Landed);
    assert_eq!(state.speed_multiplier, SIM_ONE);
    assert!(state.is_ground_mover());
    assert!(!state.is_air_mover());
}

#[test]
fn test_hover_speed_multiplier() {
    let obj = make_obj(LocomotorKind::Hover, ObjectCategory::Vehicle);
    let state = LocomotorState::from_object_type(&obj, 1500);
    assert_eq!(state.kind, LocomotorKind::Hover);
    assert_eq!(state.speed_multiplier, HOVER_SPEED_MULTIPLIER);
    assert!(state.is_ground_mover());
}

#[test]
fn test_walk_locomotor() {
    let obj = make_obj(LocomotorKind::Walk, ObjectCategory::Infantry);
    let state = LocomotorState::from_object_type(&obj, 1500);
    assert_eq!(state.kind, LocomotorKind::Walk);
    assert_eq!(state.layer, MovementLayer::Ground);
    assert!(state.is_ground_mover());
}

#[test]
fn test_fly_locomotor_air_layer() {
    let obj = make_obj(LocomotorKind::Fly, ObjectCategory::Aircraft);
    let state = LocomotorState::from_object_type(&obj, 1500);
    assert_eq!(state.kind, LocomotorKind::Fly);
    assert_eq!(state.layer, MovementLayer::Air);
    assert_eq!(state.air_phase, AirMovePhase::Landed);
    assert!(!state.is_ground_mover());
    assert!(state.is_air_mover());
    assert_eq!(state.target_altitude, SimFixed::from_num(1500));
    assert_eq!(state.climb_rate, FLY_CLIMB_RATE);
}

#[test]
fn test_jumpjet_air_layer() {
    let obj = make_obj(LocomotorKind::Jumpjet, ObjectCategory::Infantry);
    let state = LocomotorState::from_object_type(&obj, 1500);
    assert_eq!(state.kind, LocomotorKind::Jumpjet);
    assert_eq!(state.layer, MovementLayer::Air);
    assert!(!state.is_ground_mover());
    assert!(state.is_air_mover());
    assert_eq!(state.target_altitude, SimFixed::from_num(500));
}

#[test]
fn test_jumpjet_with_custom_params() {
    let mut obj = make_obj(LocomotorKind::Jumpjet, ObjectCategory::Infantry);
    obj.jumpjet = true;
    obj.jumpjet_params = Some(JumpjetParams {
        turn_rate: 4,
        speed: sim_from_f32(20.0),
        climb: sim_from_f32(8.0),
        crash: sim_from_f32(5.0),
        height: 750,
        accel: sim_from_f32(2.0),
        wobbles: 0.2,
        deviation: 40,
        no_wobbles: false,
    });
    let state = LocomotorState::from_object_type(&obj, 1500);
    assert_eq!(state.target_altitude, SimFixed::from_num(750));
    assert_eq!(state.jumpjet_speed, sim_from_f32(20.0));
    assert!((state.jumpjet_wobbles - 0.2).abs() < f32::EPSILON);
    assert_eq!(state.climb_rate, sim_from_f32(8.0) * SimFixed::from_num(15));
}

#[test]
fn test_jumpjet_no_wobbles() {
    let mut obj = make_obj(LocomotorKind::Jumpjet, ObjectCategory::Infantry);
    obj.jumpjet_params = Some(JumpjetParams {
        turn_rate: 4,
        speed: sim_from_f32(14.0),
        climb: sim_from_f32(5.0),
        crash: sim_from_f32(5.0),
        height: 500,
        accel: sim_from_f32(2.0),
        wobbles: 0.15,
        deviation: 40,
        no_wobbles: true,
    });
    let state = LocomotorState::from_object_type(&obj, 1500);
    assert!((state.jumpjet_wobbles).abs() < f32::EPSILON);
}

#[test]
fn test_ship_is_ground_mover() {
    let obj = make_obj(LocomotorKind::Ship, ObjectCategory::Vehicle);
    let state = LocomotorState::from_object_type(&obj, 1500);
    assert_eq!(state.kind, LocomotorKind::Ship);
    assert!(state.is_ground_mover());
    assert!(!state.is_air_mover());
}

#[test]
fn test_is_airborne() {
    let obj = make_obj(LocomotorKind::Fly, ObjectCategory::Aircraft);
    let mut state = LocomotorState::from_object_type(&obj, 1500);
    assert!(!state.is_airborne());
    state.altitude = SimFixed::from_num(100);
    assert!(state.is_airborne());
}

// --- Override/Piggyback mechanism tests ---

#[test]
fn test_override_teleport_round_trip() {
    let obj = make_obj(LocomotorKind::Drive, ObjectCategory::Vehicle);
    let mut state = LocomotorState::from_object_type(&obj, 1500);
    assert!(!state.is_overridden());
    assert_eq!(state.kind, LocomotorKind::Drive);
    assert_eq!(state.layer, MovementLayer::Ground);

    // Begin teleport override.
    state.begin_override(OverrideKind::Teleport);
    assert!(state.is_overridden());
    assert_eq!(state.kind, LocomotorKind::Teleport);
    assert_eq!(state.layer, MovementLayer::Ground);

    // End override — should restore Drive.
    let kind = state.end_override();
    assert_eq!(kind, Some(OverrideKind::Teleport));
    assert!(!state.is_overridden());
    assert_eq!(state.kind, LocomotorKind::Drive);
    assert_eq!(state.layer, MovementLayer::Ground);
    assert_eq!(state.speed_multiplier, SIM_ONE);
}

#[test]
fn test_override_droppod_round_trip() {
    let obj = make_obj(LocomotorKind::Walk, ObjectCategory::Infantry);
    let mut state = LocomotorState::from_object_type(&obj, 1500);
    assert_eq!(state.kind, LocomotorKind::Walk);
    assert_eq!(state.layer, MovementLayer::Ground);

    // Begin DropPod override — layer switches to Air.
    state.begin_override(OverrideKind::DropPod);
    assert!(state.is_overridden());
    assert_eq!(state.kind, LocomotorKind::DropPod);
    assert_eq!(state.layer, MovementLayer::Air);

    // End override — restores Walk on Ground.
    let kind = state.end_override();
    assert_eq!(kind, Some(OverrideKind::DropPod));
    assert!(!state.is_overridden());
    assert_eq!(state.kind, LocomotorKind::Walk);
    assert_eq!(state.layer, MovementLayer::Ground);
}

#[test]
fn test_end_override_without_active_returns_none() {
    let obj = make_obj(LocomotorKind::Drive, ObjectCategory::Vehicle);
    let mut state = LocomotorState::from_object_type(&obj, 1500);
    let result = state.end_override();
    assert_eq!(result, None);
    assert_eq!(state.kind, LocomotorKind::Drive);
}

#[test]
fn test_override_preserves_speed_type() {
    let mut obj = make_obj(LocomotorKind::Drive, ObjectCategory::Vehicle);
    obj.speed_type = SpeedType::Wheel;
    let mut state = LocomotorState::from_object_type(&obj, 1500);
    assert_eq!(state.speed_type, SpeedType::Wheel);

    state.begin_override(OverrideKind::Teleport);
    // SpeedType should still reflect the original during override.
    state.end_override();
    assert_eq!(state.speed_type, SpeedType::Wheel);
}
