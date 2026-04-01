//! Target/action lines — colored lines from selected units to command destinations.
//!
//! 25-tick global timer resets on each command issued, attack target takes priority
//! over move target, only mobile units (not buildings) draw lines. This is purely
//! app-layer — sim/ is never touched.
//!
//! ## Dependency rules
//! - Part of the app layer — reads sim state but never modifies it.

use std::collections::BTreeMap;

use crate::map::entities::EntityCategory;
use crate::map::terrain;
use crate::render::batch::SpriteInstance;
use crate::sim::command::{Command, CommandEnvelope};
use crate::sim::world::Simulation;

/// How long target lines remain visible after a command is issued (sim ticks).
const DURATION_TICKS: u64 = 25;

/// Attack target line — bright green (PALETTE.PAL index 8).
const ATTACK_COLOR: [f32; 3] = [0.0, 1.0, 0.0];
/// Move target line — lighter green (PALETTE.PAL index 3).
const MOVE_COLOR: [f32; 3] = [0.33, 1.0, 0.33];
/// Depth: above debug overlays (0.0004), below selection brackets (0.0006).
const LINE_DEPTH: f32 = 0.0005;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Determines line color.
#[derive(Debug, Clone, Copy)]
enum LineKind {
    Move,
    Attack,
}

/// Where the line points to.
#[derive(Debug, Clone, Copy)]
enum LineDest {
    /// A cell on the map (move/attackmove destination).
    Cell { rx: u16, ry: u16 },
    /// A living entity (attack target).
    Entity { target_id: u64 },
}

/// One stored target line: source entity → destination.
#[derive(Debug, Clone)]
struct LineEntry {
    entity_id: u64,
    kind: LineKind,
    dest: LineDest,
}

/// Global target line state — stored on `AppState`.
#[derive(Debug, Clone, Default)]
pub(crate) struct TargetLineState {
    /// Tick at which the timer was last started (0 = never started).
    start_tick: u64,
    /// Per-entity destination records, deduplicated by entity_id.
    entries: Vec<LineEntry>,
}

// ---------------------------------------------------------------------------
// Timer management
// ---------------------------------------------------------------------------

/// Extract target line entries from queued commands and reset the global timer.
///
/// Called from `app_context_order` right before commands are pushed to
/// `pending_commands`, so lines appear immediately on click — not after
/// network/input delay.
pub(crate) fn record_command_lines(
    state: &mut TargetLineState,
    commands: &[CommandEnvelope],
    current_tick: u64,
) {
    let mut any = false;
    for envelope in commands {
        let entry = match &envelope.payload {
            Command::Move {
                entity_id,
                target_rx,
                target_ry,
                ..
            }
            | Command::AttackMove {
                entity_id,
                target_rx,
                target_ry,
                ..
            } => Some(LineEntry {
                entity_id: *entity_id,
                kind: LineKind::Move,
                dest: LineDest::Cell {
                    rx: *target_rx,
                    ry: *target_ry,
                },
            }),
            Command::Attack {
                attacker_id,
                target_id,
            }
            | Command::ForceAttack {
                attacker_id,
                target_id,
            } => Some(LineEntry {
                entity_id: *attacker_id,
                kind: LineKind::Attack,
                dest: LineDest::Entity {
                    target_id: *target_id,
                },
            }),
            _ => None,
        };
        if let Some(e) = entry {
            // Deduplicate: one line per entity, latest command wins.
            state
                .entries
                .retain(|existing| existing.entity_id != e.entity_id);
            state.entries.push(e);
            any = true;
        }
    }
    if any {
        state.start_tick = current_tick;
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Build `SpriteInstance` quads for all active target lines.
///
/// Returns an empty vec if the timer has expired or no simulation is loaded.
pub(crate) fn build_target_line_instances(
    line_state: &TargetLineState,
    sim: Option<&Simulation>,
    height_map: &BTreeMap<(u16, u16), u8>,
) -> Vec<SpriteInstance> {
    let sim = match sim {
        Some(s) => s,
        None => return Vec::new(),
    };

    // Check global timer.
    if line_state.start_tick == 0 {
        return Vec::new();
    }
    if sim.tick.saturating_sub(line_state.start_tick) >= DURATION_TICKS {
        return Vec::new();
    }

    let mut instances = Vec::new();

    for entry in &line_state.entries {
        // Source entity must still exist, be selected, and not be a building.
        let source = match sim.entities.get(entry.entity_id) {
            Some(e) if e.selected && e.category != EntityCategory::Structure => e,
            _ => continue,
        };

        let src_x = source.position.screen_x;
        let src_y = source.position.screen_y;

        // Resolve destination screen position.
        let (dst_x, dst_y) = match entry.dest {
            LineDest::Cell { rx, ry } => {
                let z = height_map.get(&(rx, ry)).copied().unwrap_or(0);
                let (sx, sy) = terrain::iso_to_screen(rx, ry, z);
                // iso_to_screen returns NW corner of cell; shift to cell center.
                (sx + 30.0, sy + 15.0)
            }
            LineDest::Entity { target_id } => {
                match sim.entities.get(target_id) {
                    Some(target) => (target.position.screen_x, target.position.screen_y),
                    None => continue, // Target dead — skip line.
                }
            }
        };

        let color = match entry.kind {
            LineKind::Attack => ATTACK_COLOR,
            LineKind::Move => MOVE_COLOR,
        };

        emit_colored_line(&mut instances, src_x, src_y, dst_x, dst_y, color);
    }

    instances
}

/// Pixel-stepping line emitter: steps from (ax, ay) to (bx, by) emitting
/// 1×1 SpriteInstance quads colored with `tint`.
fn emit_colored_line(
    instances: &mut Vec<SpriteInstance>,
    ax: f32,
    ay: f32,
    bx: f32,
    by: f32,
    tint: [f32; 3],
) {
    let dx = bx - ax;
    let dy = by - ay;
    let steps = dx.abs().max(dy.abs()).ceil() as i32;
    if steps <= 0 {
        return;
    }
    let step_x = dx / steps as f32;
    let step_y = dy / steps as f32;

    for i in 0..steps {
        let px = (ax + step_x * i as f32).round();
        let py = (ay + step_y * i as f32).round();
        instances.push(SpriteInstance {
            position: [px, py],
            size: [1.0, 1.0],
            uv_origin: [0.0, 0.0],
            uv_size: [1.0, 1.0],
            tint,
            alpha: 1.0,
            depth: LINE_DEPTH,
        });
    }
}
