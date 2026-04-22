---
layout: default
title: VERA20k Engine
---

# VERA20k

Red Alert 2: Yuri's Revenge — rebuilt from scratch in Rust.

The entire engine is built from two things: structs and functions. There are zero abstraction layers. Code is logically organized in files and folders. The codebase is therefore very machine and human friendly.

### All game state lives in one struct: `Simulation`

It contains:

- `entities: EntityStore` — every unit/building/aircraft in the game
- `production: ProductionState` — build queues, credits, rally points per player
- `fog: FogState` — shroud/visibility per player
- `power_states` — per-player power grid (output, drain, blackout)
- `super_weapons` — per-player superweapon countdowns
- `occupancy: OccupancyGrid` — which entity occupies which cell
- `houses` — per-player state (alliances, defeat status)
- `terrain_costs` — pathfinding cost grids
- `zone_grid` — zone connectivity for unreachability checks
- `overlay_grid` — ore, gems, walls on the map
- `bridge_state` — bridge health and connectivity
- `rng: SimRng` — single deterministic random number generator
- `tick: u64` — current game tick counter

Each of those is a plain struct or a map of structs. No behavior attached to them.

### Each `GameEntity` is one struct with optional fields

Every object in the game — tank, soldier, building, aircraft — is the same struct. Always-present fields: `stable_id`, `position`, `health`, `owner`, `facing`, `type_ref`, `category`. Optional fields are `Option<T>`: a tank has `locomotor` + `turret_facing` + `drive_track`, a building has `production`, a harvester has `miner`. No component has methods — they're all data.

### Behavior is plain functions

(Example functions below)
- `tick_movement()` — reads entity positions and locomotor data, writes new positions
- `tick_combat()` — reads attack targets and weapon stats, applies damage
- `tick_production()` — advances build queues, spawns finished units
- `tick_power_states()` — recalculates per-player power from buildings
- `tick_superweapons()` — counts down timers, fires effects
- `tick_ore_growth()` — spreads ore across the map

These functions all read and write to the same `Simulation` struct. 45 times a second at 45 FPS(standard multiplayer FPS) There is no message buses, no event systems.

### The game loop is one function calling the others in order

`Simulation::advance_tick()` calls: commands → movement → combat → vision → power → superweapons → production → AI → defeat check → state hash. Every tick, same order.

### Rendering

A 2D sprite renderer using wgpu. At map load, all sprites (buildings, infantry, terrain tiles, overlays) are packed into atlas textures — big images containing many sprites side by side. Voxel models (vehicles, aircraft) are pre-rendered into 2D sprites and packed into atlases the same way.

Each frame, the renderer walks through all entities in `Simulation`, reads their position, facing, health, and animation frame, looks up the matching sprite in the atlas, and tells the GPU where to draw it on screen. Isometric depth is handled by draw order and depth values — there's no 3D geometry.

The render code only reads from `Simulation`. It never writes back. You can change rendering without touching game logic, and vice versa.

### App layer

The app layer wires everything together. It contains no game logic and no rendering logic — just the connections between them.

When you click on a unit, the app layer handles that. It figures out which entity you clicked, translates it into a command, and passes it to the simulation. The app layer is the translator between "what the player did" and "what the simulation understands."

The simulation runs at a fixed 45 ticks per second, independent of frame rate. The app layer keeps track of elapsed time and runs the right number of sim ticks each frame — sometimes one, sometimes two if the frame was slow, never more than eight to prevent spiral-of-death lag.

After each tick, the app layer hands the updated simulation state to the renderer, which draws the frame. It also drains sound events that the simulation produced (weapon fired, unit died, construction complete) and plays them through the audio system.

Like everything else, it's split into files by concern — `app_input.rs`, `app_camera.rs`, `app_commands.rs`, `app_sidebar_build.rs` — but they're all just functions operating on one shared `AppState` struct. 

---

