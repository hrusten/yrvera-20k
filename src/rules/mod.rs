//! Game data definitions parsed from rules.ini and art.ini.
//!
//! rules.ini is the heart of Red Alert 2 — it defines EVERY unit type, building type,
//! weapon, warhead, projectile, speed value, cost, build time, and prerequisite chain.
//! The sim/ module is essentially a rules.ini interpreter.
//!
//! ## Key types
//! - `RuleSet` — master lookup table for all game data, loaded once at startup
//! - `ObjectType` — defines a game object (unit/building/aircraft) with shared properties
//! - `WeaponType` — defines a weapon (damage, range, rate of fire, projectile, warhead)
//! - `WarheadType` — defines damage spread and armor effectiveness (Verses)
//!
//! ## Dependency rules
//! - rules/ depends on: assets/ (reads INI files extracted from .mix archives)
//! - rules/ is depended on by: sim/, map/, render/, sidebar/
//! - rules/ does NOT depend on: sim/, render/, ui/, sidebar/, audio/, net/

pub mod art_data;
pub mod error;
pub mod flh;
pub mod house_colors;
pub mod infantry_sequence;
pub mod ini_parser;
pub mod jumpjet_params;
pub mod locomotor_type;
pub mod object_type;
pub mod projectile_type;
pub mod radar_event_config;
pub mod ruleset;
pub mod shp_vehicle_sequence;
pub mod sound_ini;
pub mod superweapon_type;
pub mod terrain_rules;
pub mod warhead_type;
pub mod weapon_type;
