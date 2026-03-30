//! Game simulation — EntityStore + GameEntity, fixed-point math, deterministic logic.
//!
//! ALL simulation math uses fixed-point arithmetic (I32F16) — never floats.
//! This is a day-one decision for deterministic multiplayer: identical results
//! across different machines regardless of CPU floating-point behavior.
//!
//! The simulation is data-driven: all unit stats, weapon damage, build times
//! come from RuleSet (parsed from rules.ini). Sim code contains pure logic,
//! never hardcoded game balance numbers.
//!
//! ## Key types
//! - `Simulation` — owns EntityStore (BTreeMap<u64, GameEntity>), ticks game state forward
//! - `GameEntity` — unified plain struct (position, health, owner, locomotor, etc.)
//! - Systems: movement, combat, harvesting, production, pathfinding
//!
//! ## Dependency rules â€" THIS IS THE #1 ARCHITECTURAL INVARIANT
//! - sim/ depends on: rules/, map/
//! - sim/ NEVER depends on: render/, ui/, sidebar/, audio/, net/
//! - This isolation is what makes alternative views possible (Commander mode,
//!   spectator view, headless server) without touching sim code.
//! - sim/ receives commands but never calls into presentation modules.

// --- Core types: entity storage, components, commands, RNG, interning ---
pub mod command;
pub mod components;
pub mod entity_store;
pub mod game_entity;
pub mod intern;
pub mod rng;

// --- Subsystem folders (multi-file subsystems with internal mod.rs) ---
pub mod combat; // targeting, weapons, AOE, fire gates, damage resolution
pub mod miner; // harvester state machine, dock sequences, ore delivery
pub mod production; // build queue, placement, economy, tech tree, selling
pub mod world; // Simulation orchestrator, command dispatch, spawn, hash

// --- Movement: ground pathing, speed ramping, cell transitions,
//     special locomotors, drive tracks, turret rotation ---
pub mod movement;
pub mod pathfinding; // A* search, zone connectivity, terrain costs, path smoothing

// --- Docking: repair depots and airfield landing pads ---
pub mod docking;

// --- Vision, fog of war, power ---
pub mod power_system;
pub mod vision;

// --- Animation, building overlays, bridge state ---
pub mod animation;
pub mod bridge_state;

// --- Passengers, transport, slaves ---
pub mod passenger;
pub mod slave_miner;

// --- Economy, map resources ---
pub mod ore_growth;
pub mod radar;

// --- Per-match settings, per-player state ---
pub mod game_options;
pub mod house_state;

// --- Trigger runtime (map trigger evaluation during gameplay) ---
pub mod trigger_runtime;

// --- AI, replay, selection, debug ---
pub mod ai;
pub mod debug_event_log;
pub mod replay;
pub mod selection;
