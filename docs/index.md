---
layout: default
title: VERA20k Engine
---




The entire engine is built from two things: structs and functions. There are zero abstractions layers. Code are logically organized in files and folders. Codebase is therefore very machine and human friendly.   

All game state lives in one struct: `Simulation`.

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

### The render layer just reads the same structs

`render/` reads `GameEntity.position`, `.facing`, `.health`, `.is_voxel` etc. to decide what to draw and where. It never writes to simulation state.

You can work on a module without neccesarly knowing anything on outside. Sim is hermetically sealed from render. 

---

