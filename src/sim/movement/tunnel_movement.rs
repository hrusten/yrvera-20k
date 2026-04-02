//! Tunnel locomotor — two-mode surface/underground movement.
//!
//! Tunnel units (e.g., Terror Drone underground) use surface movement for
//! short routes (≤11 cells) and burrow underground for longer ones. When
//! burrowed, the unit travels in a straight line at TunnelSpeed, bypassing
//! ground obstacles and occupancy.
//!
//! ## State machine
//! SurfaceMove → DigIn → UndergroundTravel → DigOut → Idle
//!
//! ## RA2 behavior
//! - Route > 11 cells: switch to burrowing mode
//! - Underground travel uses `[General] TunnelSpeed=` (default 6.0)
//! - Surface travel uses normal ground A* pathfinding
//! - Uses `MovementLayer::Underground` — invisible to ground occupancy
//!
//! ## Dependency rules
//! - Part of sim/ — depends on sim/game_entity, sim/entity_store, sim/locomotor,
//!   sim/pathfinding, sim/movement.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use crate::rules::locomotor_type::MovementZone;
use crate::sim::debug_event_log::DebugEventKind;
use crate::sim::entity_store::EntityStore;
use crate::sim::movement::facing_from_delta;
use crate::sim::movement::locomotor::MovementLayer;
use crate::sim::occupancy::OccupancyGrid;
use crate::sim::pathfinding::terrain_cost::TerrainCostGrid;
use crate::sim::pathfinding::{PathGrid, find_path_with_costs};
use crate::util::fixed_math::{SIM_ZERO, SimFixed, dt_from_tick_ms, int_distance_to_sim};
use crate::util::lepton::CELL_CENTER_LEPTON;

/// Route length threshold: if A* path is longer than this, the unit burrows.
const BURROW_THRESHOLD_CELLS: usize = 11;

/// Duration of the dig-in phase in seconds (playing burrow animation).
const DIG_IN_DURATION_S: SimFixed = SimFixed::lit("0.8");
/// Duration of the dig-out phase in seconds (surfacing animation).
const DIG_OUT_DURATION_S: SimFixed = SimFixed::lit("0.8");

/// Phase within the tunnel state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TunnelPhase {
    /// Short route: using normal ground movement (delegated to movement.rs).
    SurfaceMove,
    /// Burrowing underground — playing dig-in animation.
    DigIn,
    /// Traveling underground in a straight line at TunnelSpeed.
    UndergroundTravel,
    /// Surfacing — playing dig-out animation.
    DigOut,
}

/// State for an in-progress tunnel movement.
///
/// Attached when a tunnel-locomotor unit receives a move order with a route
/// longer than the burrow threshold. Removed when the unit surfaces.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TunnelState {
    /// Current phase in the tunnel sequence.
    pub phase: TunnelPhase,
    /// Target cell for underground travel.
    pub target_rx: u16,
    pub target_ry: u16,
    /// Timer counting down within DigIn/DigOut phases (seconds remaining).
    pub timer: SimFixed,
    /// Underground travel speed in cells per second (from GeneralRules.tunnel_speed).
    pub tunnel_speed: SimFixed,
    /// Interpolation progress for underground cell-to-cell movement (0.0 to dist).
    pub progress: SimFixed,
}

/// Issue a tunnel move command. Checks path length and decides surface vs burrow.
///
/// For short routes (≤11 cells), delegates to normal ground movement.
/// For long routes, attaches a TunnelState to begin burrowing.
/// `movement_zone` should be the unit's MovementZone from rules.ini — only
/// `Subterranean` units may burrow when no surface path exists.
///
/// Returns `true` if a movement was initiated (either surface or burrow).
pub fn issue_tunnel_move_command(
    grid: &PathGrid,
    target: (u16, u16),
    speed: SimFixed,
    tunnel_speed: SimFixed,
    terrain_costs: Option<&TerrainCostGrid>,
    movement_zone: MovementZone,
    entities: &mut EntityStore,
    entity_id: u64,
) -> bool {
    let (start_rx, start_ry) = match entities.get(entity_id) {
        Some(e) => (e.position.rx, e.position.ry),
        None => return false,
    };

    // Find path to measure route length. No entity blocks for tunnel route measurement.
    let path = match find_path_with_costs(
        grid,
        (start_rx, start_ry),
        target,
        terrain_costs,
        None,
        None,
        None,
    ) {
        Some(p) => p,
        None => {
            // No surface path — only Subterranean units may burrow blindly.
            if movement_zone == MovementZone::Subterranean {
                begin_burrow(entities, entity_id, target, tunnel_speed);
                return true;
            }
            return false;
        }
    };

    if path.len() <= BURROW_THRESHOLD_CELLS {
        // Short route: use normal surface movement via EntityStore.
        crate::sim::movement::issue_move_command(
            entities,
            grid,
            entity_id,
            target,
            speed,
            false,
            terrain_costs,
            None,
        )
    } else {
        // Long route: burrow underground.
        begin_burrow(entities, entity_id, target, tunnel_speed);
        true
    }
}

/// Start the burrowing sequence: attach TunnelState with DigIn phase.
fn begin_burrow(
    entities: &mut EntityStore,
    entity_id: u64,
    target: (u16, u16),
    tunnel_speed: SimFixed,
) {
    let Some(entity) = entities.get_mut(entity_id) else {
        return;
    };

    // Remove any existing ground movement.
    entity.movement_target = None;

    entity.tunnel_state = Some(TunnelState {
        phase: TunnelPhase::DigIn,
        target_rx: target.0,
        target_ry: target.1,
        timer: DIG_IN_DURATION_S,
        tunnel_speed,
        progress: SIM_ZERO,
    });
    entity.push_debug_event(
        0,
        DebugEventKind::SpecialMovementStart {
            kind: "Tunnel".into(),
        },
    );
}

/// Advance all in-progress tunnel state machines.
///
/// Called once per simulation tick from `advance_tick()`.
pub fn tick_tunnel_movement(
    entities: &mut EntityStore,
    occupancy: &mut OccupancyGrid,
    tick_ms: u32,
    sim_tick: u64,
) {
    if tick_ms == 0 {
        return;
    }
    let dt: SimFixed = dt_from_tick_ms(tick_ms);

    let mut finished: Vec<u64> = Vec::new();

    let keys = entities.keys_sorted();
    for &id in &keys {
        let Some(entity) = entities.get_mut(id) else {
            continue;
        };
        // Need tunnel_state, position, locomotor, facing — all on entity.
        let Some(ref mut tunnel) = entity.tunnel_state else {
            continue;
        };

        // Track phase before processing to detect transitions.
        let phase_before = tunnel.phase;

        match tunnel.phase {
            TunnelPhase::SurfaceMove => {
                // Surface movement is handled by movement.rs — this phase is
                // only a marker. If we reach here, something went wrong.
                finished.push(id);
            }
            TunnelPhase::DigIn => {
                tunnel.timer -= dt;
                if tunnel.timer <= SIM_ZERO {
                    // Remove from ground occupancy before going underground.
                    occupancy.remove(entity.position.rx, entity.position.ry, id);
                    // Transition to underground: switch layer.
                    if let Some(ref mut loco) = entity.locomotor {
                        loco.layer = MovementLayer::Underground;
                    }
                    tunnel.phase = TunnelPhase::UndergroundTravel;
                    tunnel.progress = SIM_ZERO;
                    // Update facing toward destination.
                    let dx = tunnel.target_rx as i32 - entity.position.rx as i32;
                    let dy = tunnel.target_ry as i32 - entity.position.ry as i32;
                    entity.facing = facing_from_delta(dx, dy);
                }
            }
            TunnelPhase::UndergroundTravel => {
                // Straight-line underground movement at tunnel_speed.
                tunnel.progress += tunnel.tunnel_speed * dt;

                // Compute distance in i32 to avoid I16F16 overflow on large maps.
                let dx: i32 = tunnel.target_rx as i32 - entity.position.rx as i32;
                let dy: i32 = tunnel.target_ry as i32 - entity.position.ry as i32;
                let dist: SimFixed = int_distance_to_sim(dx, dy);

                if dist < SimFixed::from_num(1) || tunnel.progress >= dist {
                    // Arrived at destination — start surfacing.
                    entity.position.rx = tunnel.target_rx;
                    entity.position.ry = tunnel.target_ry;
                    entity.position.sub_x = CELL_CENTER_LEPTON;
                    entity.position.sub_y = CELL_CENTER_LEPTON;
                    entity.position.refresh_screen_coords();
                    // Underground layer is not tracked in occupancy — no move needed.
                    tunnel.phase = TunnelPhase::DigOut;
                    tunnel.timer = DIG_OUT_DURATION_S;
                } else {
                    // Interpolate position along the straight line.
                    // Uses SimFixed to avoid platform-dependent f32 rounding.
                    let frac: SimFixed = tunnel.progress / dist;
                    let start_rx: SimFixed = SimFixed::from_num(entity.position.rx);
                    let start_ry: SimFixed = SimFixed::from_num(entity.position.ry);
                    let dx_sim: SimFixed = SimFixed::from_num(dx);
                    let dy_sim: SimFixed = SimFixed::from_num(dy);
                    let new_rx: u16 = (start_rx + dx_sim * frac)
                        .to_num::<i32>()
                        .clamp(0, u16::MAX as i32) as u16;
                    let new_ry: u16 = (start_ry + dy_sim * frac)
                        .to_num::<i32>()
                        .clamp(0, u16::MAX as i32) as u16;
                    if new_rx != entity.position.rx || new_ry != entity.position.ry {
                        entity.position.rx = new_rx;
                        entity.position.ry = new_ry;
                        entity.position.sub_x = CELL_CENTER_LEPTON;
                        entity.position.sub_y = CELL_CENTER_LEPTON;
                        entity.position.refresh_screen_coords();
                        // Underground layer is not tracked in occupancy.
                    }
                }
            }
            TunnelPhase::DigOut => {
                tunnel.timer -= dt;
                if tunnel.timer <= SIM_ZERO {
                    // Return to ground layer.
                    if let Some(ref mut loco) = entity.locomotor {
                        loco.layer = MovementLayer::Ground;
                    }
                    // Re-add to ground occupancy now that we've surfaced.
                    occupancy.add(
                        entity.position.rx,
                        entity.position.ry,
                        id,
                        MovementLayer::Ground,
                        entity.sub_cell,
                    );
                    finished.push(id);
                }
            }
        }

        // Log phase transition if it changed.
        let phase_after = tunnel.phase;
        if phase_after != phase_before {
            let phase_name = format!("{:?}", phase_after);
            let _ = tunnel;
            entity.push_debug_event(
                sim_tick as u32,
                DebugEventKind::SpecialMovementPhase { phase: phase_name },
            );
        }
    }

    for id in finished {
        if let Some(entity) = entities.get_mut(id) {
            entity.tunnel_state = None;
            entity.push_debug_event(sim_tick as u32, DebugEventKind::SpecialMovementEnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::locomotor_type::LocomotorKind;
    use crate::sim::entity_store::EntityStore;
    use crate::sim::game_entity::GameEntity;
    use crate::sim::movement::locomotor::{GroundMovePhase, LocomotorState, MovementLayer};
    use crate::util::fixed_math::{SIM_ONE, SIM_ZERO, SimFixed};

    /// Make a minimal LocomotorState for testing.
    fn make_tunnel_loco() -> LocomotorState {
        LocomotorState {
            kind: LocomotorKind::Tunnel,
            layer: MovementLayer::Ground,
            phase: GroundMovePhase::Idle,
            air_phase: crate::sim::movement::locomotor::AirMovePhase::Landed,
            speed_multiplier: SIM_ONE,
            speed_fraction: SIM_ONE,
            fly_current_speed: SIM_ZERO,
            altitude: SIM_ZERO,
            target_altitude: SIM_ZERO,
            climb_rate: SIM_ZERO,
            jumpjet_speed: SIM_ZERO,
            jumpjet_wobbles: 0.0,
            jumpjet_accel: SIM_ZERO,
            jumpjet_current_speed: SIM_ZERO,
            jumpjet_deviation: 0,
            jumpjet_crash_speed: SIM_ZERO,
            jumpjet_turn_rate: 4,
            balloon_hover: false,
            hover_attack: false,
            speed_type: crate::rules::locomotor_type::SpeedType::Track,
            movement_zone: crate::rules::locomotor_type::MovementZone::Subterranean,
            rot: 0,
            override_state: None,
            air_progress: SIM_ZERO,
            infantry_wobble_phase: 0.0,
            subcell_dest: None,
        }
    }

    #[test]
    fn test_short_route_uses_surface_movement() {
        let mut entities = EntityStore::new();
        let grid = PathGrid::new(20, 20);
        let loco = make_tunnel_loco();
        let mut e = GameEntity::test_default(1, "DVIL", "Soviet", 5, 5);
        e.locomotor = Some(loco);
        entities.insert(e);

        // Target 3 cells away — should use surface movement, not burrow.
        let result = issue_tunnel_move_command(
            &grid,
            (8, 5),
            SimFixed::from_num(4),
            SimFixed::from_num(6),
            None,
            MovementZone::Subterranean,
            &mut entities,
            1,
        );
        assert!(result);
        // Should NOT have a TunnelState (surface movement used).
        let entity = entities.get(1).expect("should exist");
        assert!(entity.tunnel_state.is_none());
    }

    #[test]
    fn test_long_route_burrows() {
        let mut entities = EntityStore::new();
        let grid = PathGrid::new(30, 30);
        let loco = make_tunnel_loco();
        let mut e = GameEntity::test_default(1, "DVIL", "Soviet", 2, 2);
        e.locomotor = Some(loco);
        entities.insert(e);

        // Target 15 cells away — should burrow.
        let result = issue_tunnel_move_command(
            &grid,
            (17, 2),
            SimFixed::from_num(4),
            SimFixed::from_num(6),
            None,
            MovementZone::Subterranean,
            &mut entities,
            1,
        );
        assert!(result);
        // Should have TunnelState with DigIn phase.
        let entity = entities.get(1).expect("should exist");
        let ts = entity
            .tunnel_state
            .as_ref()
            .expect("should have TunnelState");
        assert_eq!(ts.phase, TunnelPhase::DigIn);
        // Should NOT have ground MovementTarget.
        assert!(entity.movement_target.is_none());
    }

    #[test]
    fn test_burrow_full_sequence() {
        let mut entities = EntityStore::new();
        let loco = make_tunnel_loco();
        let mut e = GameEntity::test_default(1, "DVIL", "Soviet", 2, 2);
        e.locomotor = Some(loco);
        e.tunnel_state = Some(TunnelState {
            phase: TunnelPhase::DigIn,
            target_rx: 10,
            target_ry: 2,
            timer: DIG_IN_DURATION_S,
            tunnel_speed: SimFixed::from_num(20), // Fast for testing.
            progress: SIM_ZERO,
        });
        entities.insert(e);

        // Tick through the entire sequence.
        for _ in 0..200 {
            tick_tunnel_movement(&mut entities, 33, 0);
        }

        // Should have arrived at destination and be back on ground.
        let entity = entities.get(1).expect("should exist");
        assert_eq!(entity.position.rx, 10);
        assert_eq!(entity.position.ry, 2);

        let loco = entity.locomotor.as_ref().expect("has loco");
        assert_eq!(loco.layer, MovementLayer::Ground);

        // TunnelState should be removed.
        assert!(entity.tunnel_state.is_none());
    }

    #[test]
    fn test_underground_layer_during_travel() {
        let mut entities = EntityStore::new();
        let loco = make_tunnel_loco();
        let mut e = GameEntity::test_default(1, "DVIL", "Soviet", 2, 2);
        e.locomotor = Some(loco);
        e.tunnel_state = Some(TunnelState {
            phase: TunnelPhase::DigIn,
            target_rx: 20,
            target_ry: 2,
            timer: SimFixed::lit("0.1"), // Very short dig-in for testing.
            tunnel_speed: SimFixed::from_num(2),
            progress: SIM_ZERO,
        });
        entities.insert(e);

        // Tick past DigIn into UndergroundTravel.
        for _ in 0..10 {
            tick_tunnel_movement(&mut entities, 33, 0);
        }

        let entity = entities.get(1).expect("should exist");
        let loco = entity.locomotor.as_ref().expect("has loco");
        assert_eq!(
            loco.layer,
            MovementLayer::Underground,
            "Should be underground during travel"
        );
    }
}
