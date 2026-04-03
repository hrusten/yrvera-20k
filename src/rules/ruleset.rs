//! Master game data container loaded from rules.ini.
//!
//! RuleSet is the single source of truth for all game object definitions.
//! It parses the type registries ([InfantryTypes], [VehicleTypes], etc.),
//! then loads each referenced object's section into typed structs. Weapons
//! and warheads referenced by objects are also parsed.
//!
//! ## Loading strategy
//! 1. Parse type registries → collect all object IDs per category
//! 2. For each ID, look up its [ID] section → parse into ObjectType
//! 3. Collect weapon/warhead IDs referenced by all objects
//! 4. Parse each referenced weapon/warhead section
//! 5. Log summary counts
//!
//! ## Dependency rules
//! - Part of rules/ — depends on rules/ini_parser, rules/object_type,
//!   rules/weapon_type, rules/warhead_type.
//! - No dependencies on sim/, render/, ui/, etc.

use std::collections::{HashMap, HashSet};

use crate::rules::error::RulesError;
use crate::rules::ini_parser::IniFile;
use crate::rules::object_type::{FactoryType, ObjectCategory, ObjectType};
use crate::rules::projectile_type::ProjectileType;
use crate::rules::radar_event_config::RadarEventConfig;
use crate::rules::terrain_rules::TerrainRules;
use crate::rules::warhead_type::WarheadType;
use crate::rules::superweapon_type::SuperWeaponType;
use crate::rules::weapon_type::WeaponType;
use crate::util::fixed_math::{SimFixed, sim_from_f32};

/// Registry section names in rules.ini and their corresponding category.
const TYPE_REGISTRIES: &[(&str, ObjectCategory)] = &[
    ("InfantryTypes", ObjectCategory::Infantry),
    ("VehicleTypes", ObjectCategory::Vehicle),
    ("AircraftTypes", ObjectCategory::Aircraft),
    ("BuildingTypes", ObjectCategory::Building),
];

/// Production timing rules parsed from `[General]`.
///
/// The `_ppm` (parts-per-million) fields are pre-computed at INI parse time from the
/// corresponding f32 fields so that sim code can use pure integer arithmetic.
/// 1_000_000 = 1.0×. The f32 originals are kept for logging/debugging.
#[derive(Debug, Clone, Copy)]
pub struct ProductionRules {
    /// Minutes to build an object that costs 1000 credits before per-object modifiers.
    pub build_speed: f32,
    /// Time multiplier applied for each extra matching factory.
    pub multiple_factory: f32,
    /// Severity of the low-power speed penalty.
    pub low_power_penalty_modifier: f32,
    /// Lower bound on production speed while low power is active.
    pub min_low_power_production_speed: f32,
    /// Upper bound on production speed while low power is active.
    pub max_low_power_production_speed: f32,
    // -- Pre-computed integer-scaled values for deterministic sim math --
    /// `multiple_factory` scaled to PPM (e.g., 0.8 → 800_000).
    pub multiple_factory_ppm: u64,
    /// `low_power_penalty_modifier` scaled to PPM.
    pub low_power_penalty_modifier_ppm: u64,
    /// `min_low_power_production_speed` scaled to PPM.
    pub min_low_power_production_speed_ppm: u64,
    /// `max_low_power_production_speed` scaled to PPM.
    pub max_low_power_production_speed_ppm: u64,
    /// `build_speed` pre-scaled ×1000 for deterministic build-time computation.
    pub build_speed_x1000: u64,
    /// Speed coefficient applied to wall building production after all other
    /// queue time scaling. Parsed from `WallBuildSpeedCoefficient=` in [General].
    pub wall_build_speed_coefficient: f32,
}

/// PPM scale constant (1_000_000 = 1.0×) used for f32→integer conversion at parse time.
const PRODUCTION_PPM: u64 = 1_000_000;

/// Convert an f32 value clamped to `[min, ∞)` into PPM u64 at parse time only.
fn f32_to_ppm(val: f32, min: f32) -> u64 {
    (val.max(min) as f64 * PRODUCTION_PPM as f64) as u64
}

impl Default for ProductionRules {
    fn default() -> Self {
        Self {
            build_speed: 0.05,
            multiple_factory: 0.8,
            low_power_penalty_modifier: 1.0,
            min_low_power_production_speed: 0.5,
            max_low_power_production_speed: 0.9,
            multiple_factory_ppm: f32_to_ppm(0.8, 0.01),
            low_power_penalty_modifier_ppm: f32_to_ppm(1.0, 0.0),
            min_low_power_production_speed_ppm: f32_to_ppm(0.5, 0.0),
            max_low_power_production_speed_ppm: f32_to_ppm(0.9, 0.0),
            build_speed_x1000: (0.05f64 * 1000.0) as u64,
            wall_build_speed_coefficient: 1.0,
        }
    }
}

/// A world-effect animation reference parsed from rules.ini + art.ini.
///
/// The name comes from rules.ini `[General]` (e.g., WarpIn=WARPIN).
/// The rate comes from the anim's own art.ini section (e.g., `[WARPIN]` Rate=120).
#[derive(Debug, Clone)]
pub struct AnimRef {
    /// SHP animation name (uppercase), e.g., "WARPIN".
    pub name: String,
    /// Milliseconds per frame from art.ini `[ANIM_NAME]` Rate= key.
    pub rate_ms: u32,
}

/// Global gameplay constants from `[General]` that affect vision, gap generators, etc.
#[derive(Debug, Clone)]
pub struct GeneralRules {
    /// Additive sight bonus for veteran+ units (VeteranSight=).
    /// Default 0 in vanilla RA2 (no sight bonus from veterancy).
    pub veteran_sight: i32,
    /// Leptons of elevation per +1 sight cell (LeptonsPerSightIncrease=).
    /// 256 leptons = 1 z-level in RA2. 0 disables the elevation bonus.
    pub leptons_per_sight_increase: i32,
    /// Gap Generator effect radius in cells (GapRadius=). Default 10.
    pub gap_radius: i32,
    /// Height-based LOS obstruction (RevealByHeight= in [General]).
    /// When true, terrain 4+ levels above the viewer at the midpoint blocks sight.
    /// Default true (the standard RA2/YR setting).
    pub reveal_by_height: bool,
    /// How impassable cells behind ≥4-level cliffs are (CliffBackImpassability= in [General]).
    /// 0 = disabled, 2 = enabled (marks cells as Rock). Default 2 in standard YR.
    pub cliff_back_impassability: u8,
    /// Underground travel speed for Tunnel locomotor units (TunnelSpeed=).
    /// Default 6.0 cells/second matching RA2 default.
    pub tunnel_speed: SimFixed,
    /// Default cruise altitude for Fly-locomotor aircraft (FlightLevel= in [General]).
    /// Default 1500 leptons. Per-type override possible but not yet implemented.
    pub flight_level: i32,
    /// Whether ore cells grow denser over time (TiberiumGrows= in [General]).
    /// Default true. Can be overridden per-map in [SpecialFlags].
    pub tiberium_grows: bool,
    /// Whether rich ore spreads to adjacent empty cells (TiberiumSpreads= in [General]).
    /// Default true. Can be overridden per-map in [SpecialFlags].
    pub tiberium_spreads: bool,
    /// Minutes per full map growth scan cycle (GrowthRate= in [General]).
    /// Default 5.0 minutes. Controls how fast ore regenerates.
    pub growth_rate_minutes: f32,
    /// Animation played when a unit warps in (WarpIn= in [General]).
    pub warp_in: AnimRef,
    /// Animation played when a unit warps out (WarpOut= in [General]).
    pub warp_out: AnimRef,
    /// Animation for chrono-erasing a unit (WarpAway= in [General]).
    pub warp_away: AnimRef,
    /// Sparkle particles during chrono teleport (ChronoSparkle1= in [General], YR feature).
    pub chrono_sparkle1: AnimRef,
    /// Wake animation spawned behind ships moving on water (Wake= in [General]).
    pub wake: AnimRef,
    /// Whether the attack cursor appears on a disguised Spy (AttackCursorOnDisguise= in [General]).
    /// Default false (vanilla RA2). When false, a disguised Spy does not show the attack cursor.
    pub attack_cursor_on_disguise: bool,
    /// Whether the attack cursor appears on trees/terrain (TreeTargeting= in [General]).
    /// Default false in vanilla RA2.
    pub tree_targeting: bool,
    /// Health ratio threshold below which the bar turns yellow (ConditionYellow= in [AudioVisual]).
    /// Default 0.5 (50%).
    pub condition_yellow: f32,
    /// Health ratio threshold below which the bar turns red (ConditionRed= in [AudioVisual]).
    /// Default 0.25 (25%).
    pub condition_red: f32,
    /// `condition_red` pre-scaled to integer ×1000 for deterministic sim comparisons.
    /// Computed once at parse time: `(condition_red * 1000.0) as i64`.
    pub condition_red_x1000: i64,
    /// Interval in minutes between low-power degradation damage ticks on Powered=yes buildings.
    /// Parsed from DamageDelay= in [General]. Default 1.0 minute.
    pub damage_delay_minutes: f32,
    /// Duration of spy-triggered total power blackout in game frames (15 fps).
    /// Parsed from SpyPowerBlackout= in [General]. Default 1000 frames (~67 seconds).
    pub spy_power_blackout_frames: u32,
    /// Fire/smoke anim types spawned on buildings below ConditionYellow health.
    /// Parsed from DamageFireTypes= in [General]. Default: FIRE01,FIRE02,FIRE03.
    pub damage_fire_types: Vec<AnimRef>,

    // -- Harvester scan radii and economy --
    /// Short-range ore scan radius in cells (TiberiumShortScan= in [General]).
    /// Used when harvesting a single patch — scan nearby for the next cell.
    /// Default 6 cells. YR only (RA2 hardcodes the same value).
    pub tiberium_short_scan: i32,
    /// Long-range ore scan radius in cells (TiberiumLongScan= in [General]).
    /// Used when short scan fails — look further for a new ore patch.
    /// Default 48 cells.
    pub tiberium_long_scan: i32,
    /// Slave Miner short scan distance in cells (SlaveMinerShortScan= in [General]).
    /// Deployed Slave Miner checks this range to decide if it should reposition.
    /// Default 8.
    pub slave_miner_short_scan: i32,
    /// Slave unit scan distance in cells (SlaveMinerSlaveScan= in [General]).
    /// Slaves scan further than their master since they trust it would reposition if needed.
    /// Default 14.
    pub slave_miner_slave_scan: i32,
    /// Slave Miner long scan distance in cells (SlaveMinerLongScan= in [General]).
    /// Used when searching for a new ore field to deploy near. Default 48.
    pub slave_miner_long_scan: i32,
    /// Cell improvement threshold for Slave Miner repositioning (SlaveMinerScanCorrection=).
    /// The new spot must be this many cells closer to ore to justify moving. Default 3.
    pub slave_miner_scan_correction: i32,
    /// Guard duration before deployed Slave Miner re-scans for ore (SlaveMinerKickFrameDelay=).
    /// In game frames (15 fps). Default 150 (~10 seconds).
    pub slave_miner_kick_frame_delay: u32,
    /// Standard harvester "too far" threshold in cells (HarvesterTooFarDistance=).
    /// If the nearest refinery is farther than this, the harvester drives next to it
    /// before reserving a dock. Default 5.
    pub harvester_too_far_distance: i32,
    /// Chrono harvester "too far" threshold in cells (ChronoHarvTooFarDistance=).
    /// Larger than standard because chrono miners teleport back. Default 50.
    pub chrono_harv_too_far_distance: i32,

    // -- Harvester timing --
    /// Frames per StepTimer increment during ore gathering (HarvesterLoadRate=).
    /// One bale requires 9 steps, so harvest_interval = rate * 9. Default 2.
    pub harvester_load_rate: i32,
    /// Minutes per bale during refinery unloading (HarvesterDumpRate=).
    /// Converted to frames: rate * 900.0. Default 0.016 (= 14.4 frames/bale).
    pub harvester_dump_rate: f32,

    // -- Chrono warp delay constants --
    /// Post-warp lock duration in game frames (ChronoDelay= in [General]).
    /// Applied after Chronosphere warp. Default 60 frames.
    pub chrono_delay: i32,
    /// Chrono reinforcement warp delay in game frames (ChronoReinfDelay= in [General]).
    /// Default 180 frames.
    pub chrono_reinf_delay: i32,
    /// Distance divisor for warp delay: delay = distance_leptons / factor
    /// (ChronoDistanceFactor= in [General]). Default 48.
    pub chrono_distance_factor: i32,
    /// Whether warp delay scales with distance (ChronoTrigger= in [General]).
    /// If false, always use ChronoMinimumDelay. Default true.
    pub chrono_trigger: bool,
    /// Minimum warp delay in game frames (ChronoMinimumDelay= in [General]).
    /// Floor for the distance-based calculation. Default 16 frames.
    pub chrono_minimum_delay: i32,
    /// Distance (leptons) below which delay is forced to minimum
    /// (ChronoRangeMinimum= in [General]). Default 0.
    pub chrono_range_minimum: i32,

    /// Ore Purifier bonus as a fraction (PurifierBonus= in [General]).
    /// When a player owns an Ore Purifier building, all ore gains this bonus.
    /// 0.25 = 25% bonus. Default 0.25.
    pub purifier_bonus: f32,

    // -- Survivor spawning on sell/destroy --
    /// Divisor to compute survivor count for Allied buildings (AlliedSurvivorDivisor= in [General]).
    /// Survivor count = sell_refund / divisor (rounded down, min 0). Default 500.
    pub allied_survivor_divisor: i32,
    /// Divisor to compute survivor count for Soviet buildings (SovietSurvivorDivisor= in [General]).
    /// Default 250.
    pub soviet_survivor_divisor: i32,
    /// Divisor to compute survivor count for Third-side (Yuri) buildings (ThirdSurvivorDivisor= in [General]).
    /// YR addition. Default 750.
    pub third_survivor_divisor: i32,

    // -- Terrain movement modifiers --
    /// Speed multiplier when moving uphill (SlopeClimb= in [General]).
    /// Applied per-cell during movement when next cell is higher than current.
    /// Not present in vanilla rulesmd.ini; uses compiled default from the original engine.
    pub slope_climb: SimFixed,
    /// Speed multiplier when moving downhill (SlopeDescend= in [General]).
    /// Applied per-cell during movement when next cell is lower than current.
    pub slope_descend: SimFixed,

    // -- Entity ambient glow on dark maps --
    /// Additive brightness boost for unit sprites (ExtraUnitLight= in [General]).
    /// Makes vehicles visible on dark maps. Default 0.2.
    pub extra_unit_light: f32,
    /// Additive brightness boost for infantry sprites (ExtraInfantryLight= in [General]).
    pub extra_infantry_light: f32,
    /// Additive brightness boost for aircraft sprites (ExtraAircraftLight= in [General]).
    pub extra_aircraft_light: f32,

    // -- Movement arrival --
    /// Distance in leptons below which a blocked unit stops instead of repathing.
    /// CloseEnough=2.25 in vanilla rulesmd.ini (2.25 cells × 256 lep/cell ≈ 576 leptons).
    pub close_enough: SimFixed,

    // -- Service depot / unit repair --
    /// Ticks between applying RepairStep HP when a unit is on a repair depot.
    /// Derived from URepairRate= in [General] (minutes). Default 0.016 min ≈ 14 ticks at 15 Hz.
    pub unit_repair_rate_ticks: u32,
    /// HP healed per repair step on a service depot (RepairStep= in [General]). Default 8.
    pub repair_step: u16,
    /// Percent of build cost charged for a full unit repair (RepairPercent= in [General]).
    /// Default 15 (meaning 15%). Total cost = cost * repair_percent / 100.
    pub repair_percent: u16,

    // -- Aircraft ammo reload --
    /// Ticks to reload one ammo point at an airfield (from ReloadRate= minutes in [General]).
    /// Default: 270 ticks (0.3 min × 60 sec × 15 ticks/sec).
    pub reload_rate_ticks: u32,

    // -- Movement delay timers --
    /// Ticks between pathfinding retry attempts (PathDelay= in [General]).
    /// INI value is in minutes; converted to ticks: minutes × 60 × 15.
    /// Default: 0.01 min = 9 ticks. While counting down, pathfinding is not called.
    pub path_delay_ticks: u16,
    /// Ticks to wait when blocked by a friendly unit before aggressive repath
    /// (BlockagePathDelay= in [General]). INI value is in frames (directly).
    /// When this timer expires, the unit re-pathfinds with urgency=2 (scatter).
    pub blockage_path_delay_ticks: u16,

    /// Overlay type names that are opaque concrete walls (ConcreteWalls= in [General]).
    /// Concrete walls do NOT render a ghost sprite during placement -- only the
    /// valid/invalid cell grid is shown. Fence walls (not in this list) still
    /// render their connectivity ghost. Stored uppercase for case-insensitive matching.
    pub concrete_walls: Vec<String>,

    // -- Lightning Storm superweapon constants --
    /// Duration of active storm in game frames (LightningStormDuration= in [General]).
    /// Default 180 frames (12 seconds at 15 fps).
    pub lightning_storm_duration: i32,
    /// Damage per lightning bolt strike (LightningDamage= in [General]). Default 250.
    pub lightning_damage: i32,
    /// Deferment countdown before storm bolts begin (LightningDeferment= in [General]).
    /// Default 250 frames.
    pub lightning_deferment: i32,
    /// Frames between center bolt strikes (LightningHitDelay= in [General]). Default 10.
    pub lightning_hit_delay: i32,
    /// Frames between scatter bolt strikes (LightningScatterDelay= in [General]). Default 5.
    pub lightning_scatter_delay: i32,
    /// Cell radius for scatter bolt placement (LightningCellSpread= in [General]). Default 10.
    pub lightning_cell_spread: i32,
    /// Minimum manhattan distance between consecutive bolts (LightningSeparation= in [General]).
    /// Default 3.
    pub lightning_separation: i32,
    /// Warhead ID for lightning bolt damage (LightningWarhead= in [General]). Default "IonWH".
    pub lightning_warhead: String,
}

/// Default animation rate when art.ini section is missing.
/// Matches gamemd constructor default: 1 game frame at 60fps ≈ 17ms.
const DEFAULT_ANIM_RATE_MS: u32 = 17;

impl Default for GeneralRules {
    fn default() -> Self {
        Self {
            veteran_sight: 0,
            leptons_per_sight_increase: 0,
            gap_radius: 10,
            reveal_by_height: true,
            tunnel_speed: sim_from_f32(6.0),
            flight_level: 1500,
            tiberium_grows: true,
            tiberium_spreads: true,
            growth_rate_minutes: 5.0,
            warp_in: AnimRef {
                name: "WARPIN".to_string(),
                rate_ms: 120,
            },
            warp_out: AnimRef {
                name: "WARPOUT".to_string(),
                rate_ms: 120,
            },
            warp_away: AnimRef {
                name: "WARPAWAY".to_string(),
                rate_ms: 300,
            },
            chrono_sparkle1: AnimRef {
                name: "CHRONOSK".to_string(),
                rate_ms: 120,
            },
            wake: AnimRef {
                name: "WAKE1".to_string(),
                rate_ms: 120,
            },
            attack_cursor_on_disguise: false,
            tree_targeting: false,
            condition_yellow: 0.5,
            condition_red: 0.25,
            condition_red_x1000: 250,
            damage_delay_minutes: 1.0,
            spy_power_blackout_frames: 1000,
            damage_fire_types: vec![],
            tiberium_short_scan: 6,
            tiberium_long_scan: 48,
            slave_miner_short_scan: 8,
            slave_miner_slave_scan: 14,
            slave_miner_long_scan: 48,
            slave_miner_scan_correction: 3,
            slave_miner_kick_frame_delay: 150,
            harvester_too_far_distance: 5,
            chrono_harv_too_far_distance: 50,
            harvester_load_rate: 2,
            harvester_dump_rate: 0.016,
            chrono_delay: 60,
            chrono_reinf_delay: 180,
            chrono_distance_factor: 48,
            chrono_trigger: true,
            chrono_minimum_delay: 16,
            chrono_range_minimum: 0,
            purifier_bonus: 0.25,
            allied_survivor_divisor: 500,
            soviet_survivor_divisor: 250,
            third_survivor_divisor: 750,
            // Compiled defaults from the original engine.
            // Not present in vanilla rulesmd.ini — mods can override via [General].
            slope_climb: SimFixed::lit("0.6"),
            slope_descend: SimFixed::lit("1.2"),
            extra_unit_light: 0.2,
            extra_infantry_light: 0.2,
            extra_aircraft_light: 0.2,
            // CloseEnough=2.25 cells in vanilla rulesmd.ini → 576 leptons.
            close_enough: SimFixed::from_num(576),
            // URepairRate=.016 min = 0.96 sec ≈ 14 ticks at 15 Hz.
            unit_repair_rate_ticks: 14,
            repair_step: 8,
            repair_percent: 15,
            // ReloadRate=.3 min = 18 sec = 270 ticks at 15 Hz.
            reload_rate_ticks: 270,
            // PathDelay=.01 min = 0.6 sec = 9 ticks at 15 Hz.
            path_delay_ticks: 9,
            // BlockagePathDelay=60 frames (directly in frames, not minutes).
            blockage_path_delay_ticks: 60,
            concrete_walls: Vec::new(),
            cliff_back_impassability: 2,
            lightning_storm_duration: 180,
            lightning_damage: 250,
            lightning_deferment: 250,
            lightning_hit_delay: 10,
            lightning_scatter_delay: 5,
            lightning_cell_spread: 10,
            lightning_separation: 3,
            lightning_warhead: "IonWH".to_string(),
        }
    }
}

/// Garrison/occupation combat rules parsed from `[CombatDamage]` in `rules(md).ini`.
/// These global multipliers govern how garrisoned infantry fire from buildings.
#[derive(Debug, Clone)]
pub struct GarrisonRules {
    /// Damage multiplier applied to garrison fire.
    pub occupy_damage_multiplier: SimFixed,
    /// ROF divisor for garrison fire -- higher = faster.
    pub occupy_rof_multiplier: SimFixed,
    /// Fixed weapon range in cells for garrisoned fire, replaces weapon's own range.
    pub occupy_weapon_range: i32,
    /// Damage multiplier for bunker passengers.
    pub bunker_damage_multiplier: f32,
    /// ROF divisor for bunker passengers.
    pub bunker_rof_multiplier: f32,
    /// Range bonus in cells for bunker passengers.
    pub bunker_weapon_range_bonus: i32,
    /// Damage multiplier for open-topped passengers.
    pub open_topped_damage_multiplier: f32,
    /// Range bonus in cells for open-topped passengers.
    pub open_topped_range_bonus: i32,
}

impl Default for GarrisonRules {
    fn default() -> Self {
        Self {
            occupy_damage_multiplier: SimFixed::ONE,
            occupy_rof_multiplier: SimFixed::ONE,
            occupy_weapon_range: 5,
            bunker_damage_multiplier: 1.0,
            bunker_rof_multiplier: 1.0,
            bunker_weapon_range_bonus: 0,
            open_topped_damage_multiplier: 1.0,
            open_topped_range_bonus: 0,
        }
    }
}

impl GarrisonRules {
    fn from_ini(ini: &IniFile) -> Self {
        let section = ini.section("CombatDamage");
        let get_f32 = |key: &str, default: f32| -> f32 {
            section.and_then(|s| s.get_f32(key)).unwrap_or(default)
        };
        let get_i32 = |key: &str, default: i32| -> i32 {
            section.and_then(|s| s.get_i32(key)).unwrap_or(default)
        };
        Self {
            occupy_damage_multiplier: sim_from_f32(get_f32("OccupyDamageMultiplier", 1.0)),
            occupy_rof_multiplier: sim_from_f32(get_f32("OccupyROFMultiplier", 1.0)),
            occupy_weapon_range: get_i32("OccupyWeaponRange", 5),
            bunker_damage_multiplier: get_f32("BunkerDamageMultiplier", 1.0),
            bunker_rof_multiplier: get_f32("BunkerROFMultiplier", 1.0),
            bunker_weapon_range_bonus: get_i32("BunkerWeaponRangeBonus", 0),
            open_topped_damage_multiplier: get_f32("OpenToppedDamageMultiplier", 1.0),
            open_topped_range_bonus: get_i32("OpenToppedRangeBonus", 0),
        }
    }
}

/// Bridge damage/destruction rules parsed from `rules(md).ini`.
#[derive(Debug, Clone)]
pub struct BridgeRules {
    /// Hit points shared by a destroyable bridge span.
    pub strength: u16,
    /// Whether bridges are destroyable unless the map overrides it.
    pub destroyable_by_default: bool,
    /// SHP animation names to spawn when a bridge group is destroyed
    /// (e.g., TWLT026, TWLT036, TWLT050, TWLT070). Picked randomly per cell.
    pub explosions: Vec<String>,
}

impl Default for BridgeRules {
    fn default() -> Self {
        Self {
            strength: 250,
            destroyable_by_default: true,
            explosions: Vec::new(),
        }
    }
}

impl BridgeRules {
    fn from_ini(ini: &IniFile) -> Self {
        let strength = ini
            .section("CombatDamage")
            .and_then(|section| section.get_i32("BridgeStrength"))
            .unwrap_or(250)
            .max(1) as u16;
        let destroyable_by_default = ini
            .section("SpecialFlags")
            .and_then(|section| section.get_bool("DestroyableBridges"))
            .unwrap_or(true);
        let explosions = ini
            .section("General")
            .and_then(|section| section.get_list("BridgeExplosions"))
            .map(|list| list.into_iter().map(|s| s.to_uppercase()).collect())
            .unwrap_or_default();
        Self {
            strength,
            destroyable_by_default,
            explosions,
        }
    }
}

impl GeneralRules {
    fn from_ini(ini: &IniFile) -> Self {
        let Some(general) = ini.section("General") else {
            return Self::default();
        };
        // ConditionYellow/ConditionRed live in [AudioVisual], not [General].
        let audio_visual = ini.section("AudioVisual");
        // WarpIn/WarpOut/WarpAway values may contain semicolons with secondary
        // anims (e.g., "WARPIN;WAKE2"). We only use the primary anim name.
        let parse_anim_name = |key: &str, default: &str| -> String {
            general
                .get(key)
                .map(|v| v.split(';').next().unwrap_or(default).trim().to_string())
                .unwrap_or_else(|| default.to_string())
        };
        let defaults = Self::default();
        let condition_red_f32: f32 = audio_visual
            .and_then(|s| s.get_percent("ConditionRed"))
            .unwrap_or(0.25);
        Self {
            veteran_sight: general.get_i32("VeteranSight").unwrap_or(0),
            leptons_per_sight_increase: general.get_i32("LeptonsPerSightIncrease").unwrap_or(0),
            gap_radius: general.get_i32("GapRadius").unwrap_or(10),
            reveal_by_height: general.get_bool("RevealByHeight").unwrap_or(true),
            tunnel_speed: general
                .get_f32("TunnelSpeed")
                .map(sim_from_f32)
                .unwrap_or(sim_from_f32(6.0)),
            flight_level: general.get_i32("FlightLevel").unwrap_or(1500),
            tiberium_grows: general.get_bool("TiberiumGrows").unwrap_or(true),
            tiberium_spreads: general.get_bool("TiberiumSpreads").unwrap_or(true),
            growth_rate_minutes: general.get_f32("GrowthRate").unwrap_or(5.0),
            attack_cursor_on_disguise: general.get_bool("AttackCursorOnDisguise").unwrap_or(false),
            tree_targeting: general.get_bool("TreeTargeting").unwrap_or(false),
            condition_yellow: audio_visual
                .and_then(|s| s.get_percent("ConditionYellow"))
                .unwrap_or(0.5),
            condition_red: condition_red_f32,
            condition_red_x1000: (condition_red_f32 as f64 * 1000.0) as i64,
            warp_in: AnimRef {
                name: parse_anim_name("WarpIn", "WARPIN"),
                rate_ms: defaults.warp_in.rate_ms,
            },
            warp_out: AnimRef {
                name: parse_anim_name("WarpOut", "WARPOUT"),
                rate_ms: defaults.warp_out.rate_ms,
            },
            warp_away: AnimRef {
                name: parse_anim_name("WarpAway", "WARPAWAY"),
                rate_ms: defaults.warp_away.rate_ms,
            },
            chrono_sparkle1: AnimRef {
                name: parse_anim_name("ChronoSparkle1", "CHRONOSK"),
                rate_ms: defaults.chrono_sparkle1.rate_ms,
            },
            wake: AnimRef {
                name: parse_anim_name("Wake", "WAKE1"),
                rate_ms: defaults.wake.rate_ms,
            },
            damage_delay_minutes: general.get_f32("DamageDelay").unwrap_or(1.0),
            spy_power_blackout_frames: general.get_i32("SpyPowerBlackout").unwrap_or(1000).max(0)
                as u32,
            damage_fire_types: general
                .get_list("DamageFireTypes")
                .map(|list| {
                    list.into_iter()
                        .filter(|s| !s.is_empty())
                        .map(|s| AnimRef {
                            name: s.to_uppercase(),
                            rate_ms: DEFAULT_ANIM_RATE_MS,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            tiberium_short_scan: general.get_i32("TiberiumShortScan").unwrap_or(6),
            tiberium_long_scan: general.get_i32("TiberiumLongScan").unwrap_or(48),
            slave_miner_short_scan: general.get_i32("SlaveMinerShortScan").unwrap_or(8),
            slave_miner_slave_scan: general.get_i32("SlaveMinerSlaveScan").unwrap_or(14),
            slave_miner_long_scan: general.get_i32("SlaveMinerLongScan").unwrap_or(48),
            slave_miner_scan_correction: general.get_i32("SlaveMinerScanCorrection").unwrap_or(3),
            slave_miner_kick_frame_delay: general
                .get_i32("SlaveMinerKickFrameDelay")
                .unwrap_or(150)
                .max(0) as u32,
            harvester_too_far_distance: general.get_i32("HarvesterTooFarDistance").unwrap_or(5),
            chrono_harv_too_far_distance: general.get_i32("ChronoHarvTooFarDistance").unwrap_or(50),
            harvester_load_rate: general.get_i32("HarvesterLoadRate").unwrap_or(2),
            harvester_dump_rate: general.get_f32("HarvesterDumpRate").unwrap_or(0.016),
            chrono_delay: general.get_i32("ChronoDelay").unwrap_or(60),
            chrono_reinf_delay: general.get_i32("ChronoReinfDelay").unwrap_or(180),
            chrono_distance_factor: general.get_i32("ChronoDistanceFactor").unwrap_or(48),
            chrono_trigger: general.get_bool("ChronoTrigger").unwrap_or(true),
            chrono_minimum_delay: general.get_i32("ChronoMinimumDelay").unwrap_or(16),
            chrono_range_minimum: general.get_i32("ChronoRangeMinimum").unwrap_or(0),
            purifier_bonus: general.get_percent("PurifierBonus").unwrap_or(0.25),
            allied_survivor_divisor: general.get_i32("AlliedSurvivorDivisor").unwrap_or(500),
            soviet_survivor_divisor: general.get_i32("SovietSurvivorDivisor").unwrap_or(250),
            third_survivor_divisor: general.get_i32("ThirdSurvivorDivisor").unwrap_or(750),
            slope_climb: general
                .get_f32("SlopeClimb")
                .map(sim_from_f32)
                .unwrap_or(defaults.slope_climb),
            slope_descend: general
                .get_f32("SlopeDescend")
                .map(sim_from_f32)
                .unwrap_or(defaults.slope_descend),
            extra_unit_light: general.get_f32("ExtraUnitLight").unwrap_or(0.2),
            extra_infantry_light: general.get_f32("ExtraInfantryLight").unwrap_or(0.2),
            extra_aircraft_light: general.get_f32("ExtraAircraftLight").unwrap_or(0.2),
            close_enough: general
                .get_f32("CloseEnough")
                .map(|cells| sim_from_f32(cells * 256.0))
                .unwrap_or(defaults.close_enough),
            // URepairRate= is in minutes. Convert to ticks: minutes * 60 * 15 ticks/sec.
            unit_repair_rate_ticks: general
                .get_f32("URepairRate")
                .map(|minutes| (minutes * 60.0 * 15.0).round().max(1.0) as u32)
                .unwrap_or(defaults.unit_repair_rate_ticks),
            repair_step: general
                .get_i32("RepairStep")
                .unwrap_or(defaults.repair_step as i32)
                .max(1) as u16,
            repair_percent: general
                .get_percent("RepairPercent")
                .map(|frac| (frac * 100.0).round() as u16)
                .unwrap_or(defaults.repair_percent),
            reload_rate_ticks: general
                .get_f32("ReloadRate")
                .map(|minutes| (minutes * 60.0 * 15.0).round().max(1.0) as u32)
                .unwrap_or(defaults.reload_rate_ticks),
            // PathDelay= is in minutes. Convert to ticks: minutes * 60 * 15.
            path_delay_ticks: general
                .get_f32("PathDelay")
                .map(|minutes| (minutes * 60.0 * 15.0).round().max(1.0) as u16)
                .unwrap_or(defaults.path_delay_ticks),
            // BlockagePathDelay= is directly in frames (ticks).
            blockage_path_delay_ticks: general
                .get_i32("BlockagePathDelay")
                .map(|frames| frames.max(1) as u16)
                .unwrap_or(defaults.blockage_path_delay_ticks),
            concrete_walls: general
                .get_list("ConcreteWalls")
                .map(|list| {
                    list.into_iter()
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_ascii_uppercase())
                        .collect()
                })
                .unwrap_or_default(),
            cliff_back_impassability: general
                .get_i32("CliffBackImpassability")
                .unwrap_or(2)
                .clamp(0, 2) as u8,
            lightning_storm_duration: general.get_i32("LightningStormDuration").unwrap_or(180),
            lightning_damage: general.get_i32("LightningDamage").unwrap_or(250),
            lightning_deferment: general.get_i32("LightningDeferment").unwrap_or(250),
            lightning_hit_delay: general.get_i32("LightningHitDelay").unwrap_or(10).max(1),
            lightning_scatter_delay: general.get_i32("LightningScatterDelay").unwrap_or(5).max(1),
            lightning_cell_spread: general.get_i32("LightningCellSpread").unwrap_or(10),
            lightning_separation: general.get_i32("LightningSeparation").unwrap_or(3),
            lightning_warhead: general
                .get("LightningWarhead")
                .unwrap_or("IonWH")
                .to_string(),
        }
    }

    /// Resolve animation playback rates from art.ini sections.
    ///
    /// Called after both rules.ini and art.ini are loaded. Looks up each
    /// anim's own `[ANIM_NAME]` section for `Rate=` (ms per frame).
    pub fn resolve_art_rates(&mut self, art_ini: &IniFile) {
        fn rate_from_section(ini: &IniFile, name: &str, fallback: u32) -> u32 {
            ini.section(name)
                .and_then(|s| s.get_i32("Rate"))
                .map(|r| crate::rules::art_data::art_rate_to_delay_ms(r))
                .unwrap_or(fallback)
        }
        self.warp_in.rate_ms = rate_from_section(art_ini, &self.warp_in.name, DEFAULT_ANIM_RATE_MS);
        self.warp_out.rate_ms =
            rate_from_section(art_ini, &self.warp_out.name, DEFAULT_ANIM_RATE_MS);
        self.warp_away.rate_ms =
            rate_from_section(art_ini, &self.warp_away.name, DEFAULT_ANIM_RATE_MS);
        self.chrono_sparkle1.rate_ms =
            rate_from_section(art_ini, &self.chrono_sparkle1.name, DEFAULT_ANIM_RATE_MS);
        self.wake.rate_ms = rate_from_section(art_ini, &self.wake.name, DEFAULT_ANIM_RATE_MS);
        log::info!(
            "Warp anim rates: {}={}ms, {}={}ms, {}={}ms, wake: {}={}ms",
            self.warp_in.name,
            self.warp_in.rate_ms,
            self.warp_out.name,
            self.warp_out.rate_ms,
            self.warp_away.name,
            self.warp_away.rate_ms,
            self.wake.name,
            self.wake.rate_ms,
        );
        for fire in &mut self.damage_fire_types {
            fire.rate_ms = rate_from_section(art_ini, &fire.name, DEFAULT_ANIM_RATE_MS);
        }
        if !self.damage_fire_types.is_empty() {
            log::info!(
                "DamageFireTypes: {} types ({})",
                self.damage_fire_types.len(),
                self.damage_fire_types
                    .iter()
                    .map(|f| format!("{}={}ms", f.name, f.rate_ms))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
    }
}

impl ProductionRules {
    fn from_ini(ini: &IniFile) -> Self {
        let Some(general) = ini.section("General") else {
            return Self::default();
        };

        let bs = general.get_f32("BuildSpeed").unwrap_or(0.1);
        let mf = general.get_f32("MultipleFactory").unwrap_or(0.8);
        let lpp = general.get_f32("LowPowerPenaltyModifier").unwrap_or(1.0);
        let min_lp = general.get_f32("MinLowPowerProductionSpeed").unwrap_or(0.5);
        let max_lp = general.get_f32("MaxLowPowerProductionSpeed").unwrap_or(0.9);
        let wall_coeff = general.get_f32("WallBuildSpeedCoefficient").unwrap_or(1.0);
        let result = Self {
            build_speed: bs,
            multiple_factory: mf,
            low_power_penalty_modifier: lpp,
            min_low_power_production_speed: min_lp,
            max_low_power_production_speed: max_lp,
            multiple_factory_ppm: f32_to_ppm(mf, 0.01),
            low_power_penalty_modifier_ppm: f32_to_ppm(lpp, 0.0),
            min_low_power_production_speed_ppm: f32_to_ppm(min_lp, 0.0),
            max_low_power_production_speed_ppm: f32_to_ppm(max_lp.max(min_lp), 0.0),
            build_speed_x1000: (bs.max(0.01) as f64 * 1000.0) as u64,
            wall_build_speed_coefficient: wall_coeff,
        };
        log::info!(
            "ProductionRules: BuildSpeed={}, MultipleFactory={}, LowPowerPenalty={}",
            result.build_speed,
            result.multiple_factory,
            result.low_power_penalty_modifier,
        );
        result
    }
}

/// Master container for all game data parsed from rules.ini.
///
/// All lookups are by string ID (case-sensitive — IDs are already stored
/// in their original casing from rules.ini). The sim/ module uses RuleSet
/// to look up costs, speeds, weapons, and prerequisites for every game action.
#[derive(Debug)]
pub struct RuleSet {
    /// All game objects indexed by their ID (e.g., "MTNK" → ObjectType).
    objects: HashMap<String, ObjectType>,
    /// All weapons indexed by ID (e.g., "105mm" → WeaponType).
    weapons: HashMap<String, WeaponType>,
    /// All warheads indexed by ID (e.g., "AP" → WarheadType).
    warheads: HashMap<String, WarheadType>,
    /// All projectiles indexed by ID (e.g., "InvisibleLow" → ProjectileType).
    projectiles: HashMap<String, ProjectileType>,
    pub production: ProductionRules,
    /// Global gameplay constants (vision, gap generator, etc.).
    pub general: GeneralRules,
    /// Infantry IDs in registry order.
    pub infantry_ids: Vec<String>,
    /// Vehicle IDs in registry order.
    pub vehicle_ids: Vec<String>,
    /// Aircraft IDs in registry order.
    pub aircraft_ids: Vec<String>,
    /// Building IDs in registry order.
    pub building_ids: Vec<String>,
    /// Maps structure ID (uppercase) → FactoryType for quick lookup.
    /// Built once at load time from all ObjectType entries with Factory= set.
    /// Used by production_tech to determine what a building produces without
    /// hardcoding building names.
    pub factory_map: HashMap<String, FactoryType>,
    /// Maps prerequisite alias (uppercase, e.g. "POWER") → list of building IDs
    /// (uppercase) that satisfy it. Built from [General] PrerequisiteXxx keys.
    /// RA2 uses these so that Prerequisite=POWER means "any power plant" rather
    /// than a specific building ID.
    pub prerequisite_groups: HashMap<String, Vec<String>>,
    /// Rules-driven terrain land-type semantics keyed by TMP land byte.
    pub terrain_rules: TerrainRules,
    /// Rules-driven bridge destruction defaults.
    pub bridge_rules: BridgeRules,
    /// Garrison/bunker/open-topped combat multipliers from [CombatDamage].
    pub garrison_rules: GarrisonRules,
    /// Radar event visual parameters (ping rectangles on minimap).
    pub radar_event_config: RadarEventConfig,
    /// All superweapon types indexed by ID (e.g., "LightningStormSpecial" → SuperWeaponType).
    pub super_weapons: HashMap<String, SuperWeaponType>,
}

impl RuleSet {
    /// Parse a complete RuleSet from a rules.ini IniFile.
    ///
    /// Loads all type registries, individual object sections, and any
    /// weapons/warheads referenced by those objects. Missing sections
    /// are logged as warnings but don't cause errors — RA2's rules.ini
    /// sometimes references sections that don't exist.
    pub fn from_ini(ini: &IniFile) -> Result<Self, RulesError> {
        let mut objects: HashMap<String, ObjectType> = HashMap::new();
        let mut infantry_ids: Vec<String> = Vec::new();
        let mut vehicle_ids: Vec<String> = Vec::new();
        let mut aircraft_ids: Vec<String> = Vec::new();
        let mut building_ids: Vec<String> = Vec::new();
        let production: ProductionRules = ProductionRules::from_ini(ini);
        let general: GeneralRules = GeneralRules::from_ini(ini);
        let terrain_rules: TerrainRules = TerrainRules::from_ini(ini);
        let bridge_rules: BridgeRules = BridgeRules::from_ini(ini);
        let garrison_rules: GarrisonRules = GarrisonRules::from_ini(ini);
        let radar_event_config: RadarEventConfig = RadarEventConfig::from_ini(ini);

        // Step 1: Parse each type registry and load object sections.
        for &(registry_name, category) in TYPE_REGISTRIES {
            let ids: Vec<String> = parse_registry(ini, registry_name);
            log::info!("Registry [{}]: {} entries", registry_name, ids.len());

            for id in &ids {
                if let Some(section) = ini.section(id) {
                    let obj: ObjectType = ObjectType::from_ini_section(id, section, category);
                    objects.insert(id.clone(), obj);
                } else {
                    log::trace!(
                        "Object '{}' listed in [{}] but has no section",
                        id,
                        registry_name
                    );
                }
            }

            // Store ID lists per category.
            match category {
                ObjectCategory::Infantry => infantry_ids = ids,
                ObjectCategory::Vehicle => vehicle_ids = ids,
                ObjectCategory::Aircraft => aircraft_ids = ids,
                ObjectCategory::Building => building_ids = ids,
            }
        }

        // Step 2: Collect all weapon and warhead IDs referenced by objects.
        let (weapon_ids, warhead_refs) = collect_weapon_refs(&objects);

        // Step 3: Parse weapon sections.
        let mut weapons: HashMap<String, WeaponType> = HashMap::new();
        let mut warhead_ids: HashSet<String> = warhead_refs;

        for weapon_id in &weapon_ids {
            if let Some(section) = ini.section(weapon_id) {
                let weapon: WeaponType = WeaponType::from_ini_section(weapon_id, section);
                // Also collect warhead references from weapons themselves.
                if let Some(wh) = &weapon.warhead {
                    warhead_ids.insert(wh.clone());
                }
                weapons.insert(weapon_id.clone(), weapon);
            } else {
                log::trace!("Weapon '{}' referenced but has no section", weapon_id);
            }
        }

        // Step 4: Parse warhead sections.
        let mut warheads: HashMap<String, WarheadType> = HashMap::new();
        for warhead_id in &warhead_ids {
            if let Some(section) = ini.section(warhead_id) {
                let wh: WarheadType = WarheadType::from_ini_section(warhead_id, section);
                warheads.insert(warhead_id.clone(), wh);
            } else {
                log::trace!("Warhead '{}' referenced but has no section", warhead_id);
            }
        }

        // Step 5: Collect projectile IDs referenced by weapons and parse them.
        let mut projectiles: HashMap<String, ProjectileType> = HashMap::new();
        let mut projectile_ids: HashSet<String> = HashSet::new();
        for weapon in weapons.values() {
            if let Some(ref proj_id) = weapon.projectile {
                projectile_ids.insert(proj_id.clone());
            }
        }
        for proj_id in &projectile_ids {
            if let Some(section) = ini.section(proj_id) {
                let proj: ProjectileType = ProjectileType::from_ini_section(proj_id, section, None);
                projectiles.insert(proj_id.clone(), proj);
            } else {
                log::trace!("Projectile '{}' referenced but has no section", proj_id);
            }
        }

        // Step 6: Build factory lookup map from Factory= keys on all objects.
        let factory_map: HashMap<String, FactoryType> = objects
            .values()
            .filter_map(|obj| obj.factory.map(|ft| (obj.id.to_ascii_uppercase(), ft)))
            .collect();
        log::info!("Factory map: {} entries", factory_map.len());

        // Step 7: Parse prerequisite alias groups from [General].
        let prerequisite_groups: HashMap<String, Vec<String>> = parse_prerequisite_groups(ini);
        log::info!("Prerequisite groups: {} aliases", prerequisite_groups.len());

        // Step 8: Parse superweapon type registry.
        let mut super_weapons: HashMap<String, SuperWeaponType> = HashMap::new();
        let sw_ids: Vec<String> = parse_registry(ini, "SuperWeaponTypes");
        for sw_id in &sw_ids {
            if let Some(section) = ini.section(sw_id) {
                if let Some(sw) = SuperWeaponType::from_ini_section(sw_id, section) {
                    super_weapons.insert(sw_id.clone(), sw);
                } else {
                    log::warn!("SuperWeapon '{}' has unknown Type=, skipping", sw_id);
                }
            } else {
                log::trace!(
                    "SuperWeapon '{}' listed in [SuperWeaponTypes] but has no section",
                    sw_id
                );
            }
        }
        log::info!("SuperWeaponTypes: {} loaded", super_weapons.len());

        log::info!(
            "RuleSet loaded: {} objects ({} inf, {} veh, {} air, {} bld), \
             {} weapons, {} warheads, {} projectiles",
            objects.len(),
            infantry_ids.len(),
            vehicle_ids.len(),
            aircraft_ids.len(),
            building_ids.len(),
            weapons.len(),
            warheads.len(),
            projectiles.len()
        );

        Ok(RuleSet {
            objects,
            weapons,
            warheads,
            projectiles,
            production,
            general,
            infantry_ids,
            vehicle_ids,
            aircraft_ids,
            building_ids,
            factory_map,
            prerequisite_groups,
            terrain_rules,
            bridge_rules,
            garrison_rules,
            radar_event_config,
            super_weapons,
        })
    }

    /// Look up a game object by ID.
    /// Intern all known type IDs (infantry, vehicle, aircraft, building) into
    /// the given interner. Ensures that `interner.get(type_id)` succeeds for
    /// any type referenced by this ruleset.
    pub fn intern_all_ids(&self, interner: &mut crate::sim::intern::StringInterner) {
        for id in &self.infantry_ids {
            interner.intern(id);
        }
        for id in &self.vehicle_ids {
            interner.intern(id);
        }
        for id in &self.aircraft_ids {
            interner.intern(id);
        }
        for id in &self.building_ids {
            interner.intern(id);
        }
    }

    pub fn object(&self, id: &str) -> Option<&ObjectType> {
        self.objects.get(id)
    }

    /// Look up a game object by ID case-insensitively.
    pub fn object_case_insensitive(&self, id: &str) -> Option<&ObjectType> {
        self.objects.get(id).or_else(|| {
            self.objects
                .iter()
                .find_map(|(key, obj)| key.eq_ignore_ascii_case(id).then_some(obj))
        })
    }

    /// Look up a weapon by ID.
    pub fn weapon(&self, id: &str) -> Option<&WeaponType> {
        self.weapons.get(id)
    }

    /// Look up a warhead by ID.
    pub fn warhead(&self, id: &str) -> Option<&WarheadType> {
        self.warheads.get(id)
    }

    /// Look up a projectile by ID.
    pub fn projectile(&self, id: &str) -> Option<&ProjectileType> {
        self.projectiles.get(id)
    }

    /// Look up a superweapon type by ID.
    pub fn super_weapon(&self, id: &str) -> Option<&SuperWeaponType> {
        self.super_weapons.get(id)
    }

    /// Look up the factory type for a structure by ID (case-insensitive).
    /// Returns None if the structure has no Factory= key in rules.ini.
    pub fn factory_type(&self, structure_id: &str) -> Option<FactoryType> {
        self.factory_map
            .get(&structure_id.to_ascii_uppercase())
            .copied()
    }

    /// Look up which building IDs satisfy a prerequisite alias (case-insensitive).
    /// Returns None if the alias is not a known prerequisite group.
    pub fn prerequisite_group(&self, alias: &str) -> Option<&[String]> {
        self.prerequisite_groups
            .get(&alias.to_ascii_uppercase())
            .map(|v| v.as_slice())
    }

    /// Whether a structure type is marked as a refinery in rules.ini.
    pub fn is_refinery_type(&self, structure_id: &str) -> bool {
        self.object_case_insensitive(structure_id)
            .is_some_and(|obj| obj.refinery)
    }

    /// Whether a structure type is a repair depot (UnitRepair=yes in rules.ini).
    pub fn is_repair_depot(&self, structure_id: &str) -> bool {
        self.object_case_insensitive(structure_id)
            .is_some_and(|obj| obj.unit_repair)
    }

    /// Resolve a refinery's free starter unit if both the refinery and the unit exist.
    pub fn refinery_free_unit(&self, structure_id: &str) -> Option<&str> {
        let obj = self.object_case_insensitive(structure_id)?;
        if !obj.refinery {
            return None;
        }
        let free_unit = obj.free_unit.as_deref()?;
        let resolved = self.object_case_insensitive(free_unit)?;
        Some(resolved.id.as_str())
    }

    /// Whether a harvester type may dock at a specific structure according to Dock=.
    pub fn harvester_can_dock_at(&self, harvester_id: &str, structure_id: &str) -> bool {
        let Some(harvester) = self.object_case_insensitive(harvester_id) else {
            return false;
        };
        let Some(_structure) = self.object_case_insensitive(structure_id) else {
            return false;
        };
        harvester
            .dock
            .iter()
            .any(|dock| dock.eq_ignore_ascii_case(structure_id))
    }

    /// Merge art.ini data into object types (Foundation, QueueingCell, DockingOffset).
    ///
    /// In the original engine, `Foundation=` is an **art.ini-only** property — it does
    /// NOT exist in rules.ini. ObjectType defaults to "1x1" and this method overwrites
    /// it with the authoritative value from art.ini, resolved via the `Image=` key.
    /// Without this, all buildings would be 1x1 which breaks placement and rendering.
    pub fn merge_art_data(&mut self, art: &crate::rules::art_data::ArtRegistry) {
        let mut patched: u32 = 0;
        let mut dock_patched: u32 = 0;
        let mut buildings_checked: u32 = 0;
        for obj in self.objects.values_mut() {
            if obj.category != crate::rules::object_type::ObjectCategory::Building {
                continue;
            }
            buildings_checked += 1;
            // Resolve the art.ini section: use Image= override if present,
            // otherwise fall back to the object ID itself.
            let art_key: &str = &obj.image;
            let entry = art.get(art_key).or_else(|| art.get(&obj.id));
            if let Some(entry) = entry {
                if let Some(ref foundation) = entry.foundation {
                    if obj.foundation != *foundation {
                        log::trace!(
                            "Foundation patch: {} (image={}) {} → {}",
                            obj.id,
                            art_key,
                            obj.foundation,
                            foundation,
                        );
                    }
                    obj.foundation = foundation.clone();
                    patched += 1;
                }
                // Merge QueueingCell from art.ini (TibSun legacy dock system).
                if entry.queueing_cell.is_some() {
                    obj.queueing_cell = entry.queueing_cell;
                    dock_patched += 1;
                }
                // Merge DockingOffset0 from art.ini (TibSun legacy dock system).
                if entry.docking_offset.is_some() {
                    obj.docking_offset = entry.docking_offset;
                }
            }
        }
        log::info!(
            "Merged art.ini → RuleSet: {} foundations, {} dock cells ({} buildings checked)",
            patched,
            dock_patched,
            buildings_checked,
        );
    }

    /// Total number of game objects across all categories.
    pub fn object_count(&self) -> usize {
        self.objects.len()
    }

    /// Total number of weapons.
    pub fn weapon_count(&self) -> usize {
        self.weapons.len()
    }

    /// Total number of warheads.
    pub fn warhead_count(&self) -> usize {
        self.warheads.len()
    }

    /// Iterate all parsed warhead types.
    pub fn warheads_iter(&self) -> impl Iterator<Item = &WarheadType> {
        self.warheads.values()
    }

    /// Iterate all parsed weapon types.
    pub fn weapons_iter(&self) -> impl Iterator<Item = &WeaponType> {
        self.weapons.values()
    }

    /// Total number of projectiles.
    pub fn projectile_count(&self) -> usize {
        self.projectiles.len()
    }

    /// Iterate over all game objects in the registry.
    pub fn all_objects(&self) -> impl Iterator<Item = &ObjectType> {
        self.objects.values()
    }
}

/// Parse a type registry section (e.g., [InfantryTypes]) into a list of IDs.
///
/// Registry sections use numbered keys: `0=E1`, `1=E2`, ...
/// Returns empty Vec if the section doesn't exist.
fn parse_registry(ini: &IniFile, section_name: &str) -> Vec<String> {
    match ini.section(section_name) {
        Some(section) => {
            let raw: Vec<String> = section
                .get_values()
                .into_iter()
                .map(|s| s.to_string())
                .collect();
            // Deduplicate: rules.ini + rulesmd.ini merge can produce the same
            // type ID at different numbered keys (e.g., 42=GAAIRC in base and
            // 150=GAAIRC in YR patch). Keep first occurrence, preserve order.
            let mut seen: std::collections::HashSet<String> =
                std::collections::HashSet::with_capacity(raw.len());
            let before = raw.len();
            let deduped: Vec<String> = raw
                .into_iter()
                .filter(|id| seen.insert(id.to_ascii_uppercase()))
                .collect();
            let removed = before - deduped.len();
            if removed > 0 {
                log::info!(
                    "Registry [{}]: removed {} duplicate entries",
                    section_name,
                    removed,
                );
            }
            deduped
        }
        None => {
            log::warn!("Registry section [{}] not found in rules.ini", section_name);
            Vec::new()
        }
    }
}

/// Collect all weapon and warhead IDs referenced by objects.
///
/// Returns (weapon_ids, warhead_ids) as sets (deduplicated).
fn collect_weapon_refs(
    objects: &HashMap<String, ObjectType>,
) -> (HashSet<String>, HashSet<String>) {
    let mut weapon_ids: HashSet<String> = HashSet::new();
    let warhead_ids: HashSet<String> = HashSet::new();

    for obj in objects.values() {
        if let Some(ref w) = obj.primary {
            weapon_ids.insert(w.clone());
        }
        if let Some(ref w) = obj.secondary {
            weapon_ids.insert(w.clone());
        }
        if let Some(ref w) = obj.occupy_weapon {
            weapon_ids.insert(w.clone());
        }
        if let Some(ref w) = obj.elite_occupy_weapon {
            weapon_ids.insert(w.clone());
        }
    }

    (weapon_ids, warhead_ids)
}

/// Parse prerequisite alias groups from [General] PrerequisiteXxx keys.
///
/// RA2's rules.ini defines abstract prerequisite names (POWER, RADAR, etc.)
/// that map to lists of concrete building IDs. For example:
///   PrerequisitePower=GAPOWR,NAPOWR,NANRCT
/// means any unit with `Prerequisite=POWER` is satisfied by owning any of those.
///
/// Also registers secondary aliases used in RA2 prerequisites:
/// - FACTORY / WARFACTORY → same as PrerequisiteFactory list
/// - BARRACKS / TENT → same as PrerequisiteBarracks list
fn parse_prerequisite_groups(ini: &IniFile) -> HashMap<String, Vec<String>> {
    let mut groups: HashMap<String, Vec<String>> = HashMap::new();
    let Some(general) = ini.section("General") else {
        return groups;
    };

    /// Known [General] keys and the alias name they define.
    const PREREQ_KEYS: &[(&str, &str)] = &[
        ("PrerequisitePower", "POWER"),
        ("PrerequisiteProc", "PROC"),
        ("PrerequisiteRadar", "RADAR"),
        ("PrerequisiteTech", "TECH"),
        ("PrerequisiteBarracks", "BARRACKS"),
        ("PrerequisiteFactory", "FACTORY"),
    ];

    for &(ini_key, alias) in PREREQ_KEYS {
        if let Some(list) = general.get_list(ini_key) {
            let ids: Vec<String> = list
                .into_iter()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_ascii_uppercase())
                .collect();
            if !ids.is_empty() {
                groups.insert(alias.to_string(), ids);
            }
        }
    }

    // ProcAlternate entries merge into the PROC group.
    if let Some(list) = general.get_list("PrerequisiteProcAlternate") {
        let alt_ids: Vec<String> = list
            .into_iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_uppercase())
            .collect();
        if !alt_ids.is_empty() {
            groups
                .entry("PROC".to_string())
                .or_default()
                .extend(alt_ids);
        }
    }

    // Register secondary aliases that RA2 prerequisites use interchangeably.
    if let Some(factory_list) = groups.get("FACTORY").cloned() {
        groups.insert("WARFACTORY".to_string(), factory_list);
    }
    if let Some(barracks_list) = groups.get("BARRACKS").cloned() {
        groups.insert("TENT".to_string(), barracks_list);
    }

    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal rules.ini string for testing.
    fn make_test_rules() -> String {
        "\
[InfantryTypes]
0=E1
1=E2

[General]
BuildSpeed=0.75
MultipleFactory=0.7
LowPowerPenaltyModifier=1.25
MinLowPowerProductionSpeed=0.4
MaxLowPowerProductionSpeed=0.85

[VehicleTypes]
0=MTNK

[AircraftTypes]

[BuildingTypes]
0=GAPOWR

[E1]
Name=GI
Cost=200
Strength=125
Armor=flak
Speed=4
Primary=M60
BuildTimeMultiplier=1.15

[E2]
Name=Conscript
Cost=100
Strength=100
Armor=flak
Speed=4
Primary=INTL

[MTNK]
Name=Grizzly
Cost=700
Strength=300
Armor=heavy
Speed=6
Primary=105mm
Secondary=MachGun

[GAPOWR]
Name=Power Plant
Cost=800
Strength=750
Power=200
Foundation=2x2

[M60]
Damage=25
ROF=20
Range=5
Warhead=SA

[INTL]
Damage=20
ROF=20
Range=4.75
Warhead=SA

[105mm]
Damage=65
ROF=50
Range=5.75
Speed=40
Projectile=InvisibleLow
Warhead=AP
Burst=2

[MachGun]
Damage=20
ROF=15
Range=5
Projectile=InvisibleLow
Warhead=SA

[InvisibleLow]
AA=no
AG=yes

[SA]
Verses=100%,100%,100%,90%,70%,25%,100%,25%,25%,0%,0%
CellSpread=0

[AP]
Verses=100%,100%,90%,75%,75%,75%,60%,30%,20%,0%,0%
CellSpread=0
"
        .to_string()
    }

    #[test]
    fn test_load_ruleset() {
        let ini: IniFile = IniFile::from_str(&make_test_rules());
        let rules: RuleSet = RuleSet::from_ini(&ini).expect("Should parse");

        assert_eq!(rules.infantry_ids.len(), 2);
        assert_eq!(rules.vehicle_ids.len(), 1);
        assert_eq!(rules.aircraft_ids.len(), 0);
        assert_eq!(rules.building_ids.len(), 1);
        assert_eq!(rules.object_count(), 4); // E1, E2, MTNK, GAPOWR
        assert!((rules.production.build_speed - 0.75).abs() < 0.0001);
        assert!((rules.production.multiple_factory - 0.7).abs() < 0.0001);
        assert!((rules.production.low_power_penalty_modifier - 1.25).abs() < 0.0001);
        assert!((rules.production.min_low_power_production_speed - 0.4).abs() < 0.0001);
        assert!((rules.production.max_low_power_production_speed - 0.85).abs() < 0.0001);
        assert_eq!(rules.bridge_rules.strength, 250);
        assert!(rules.bridge_rules.destroyable_by_default);
    }

    #[test]
    fn test_object_lookup() {
        let ini: IniFile = IniFile::from_str(&make_test_rules());
        let rules: RuleSet = RuleSet::from_ini(&ini).expect("Should parse");

        let e1: &ObjectType = rules.object("E1").expect("E1 exists");
        assert_eq!(e1.cost, 200);
        assert_eq!(e1.strength, 125);
        assert_eq!(e1.category, ObjectCategory::Infantry);
        assert_eq!(e1.primary, Some("M60".to_string()));
        assert!((e1.build_time_multiplier - 1.15).abs() < 0.0001);

        let mtnk: &ObjectType = rules.object("MTNK").expect("MTNK exists");
        assert_eq!(mtnk.cost, 700);
        assert_eq!(mtnk.category, ObjectCategory::Vehicle);
        assert_eq!(mtnk.secondary, Some("MachGun".to_string()));

        let gapowr: &ObjectType = rules.object("GAPOWR").expect("GAPOWR exists");
        assert_eq!(gapowr.power, 200);
        assert_eq!(gapowr.foundation, "2x2");
    }

    #[test]
    fn test_weapon_and_warhead_loading() {
        let ini: IniFile = IniFile::from_str(&make_test_rules());
        let rules: RuleSet = RuleSet::from_ini(&ini).expect("Should parse");

        // Weapons referenced by objects should be loaded.
        let m60: &WeaponType = rules.weapon("M60").expect("M60 exists");
        assert_eq!(m60.damage, 25);
        assert_eq!(m60.warhead, Some("SA".to_string()));

        let cannon: &WeaponType = rules.weapon("105mm").expect("105mm exists");
        assert_eq!(cannon.damage, 65);
        assert_eq!(cannon.warhead, Some("AP".to_string()));
        assert_eq!(cannon.burst, 2);
        assert_eq!(cannon.projectile, Some("InvisibleLow".to_string()));

        // Burst defaults to 1 when not specified.
        assert_eq!(m60.burst, 1);

        // Projectiles referenced by weapons should be loaded.
        assert_eq!(rules.projectile_count(), 1);
        let proj = rules
            .projectile("InvisibleLow")
            .expect("InvisibleLow exists");
        assert!(!proj.aa);
        assert!(proj.ag);

        // Warheads referenced by weapons should be loaded.
        let sa: &WarheadType = rules.warhead("SA").expect("SA exists");
        assert_eq!(sa.verses.len(), 11);
        assert_eq!(sa.verses[0], 100); // none: 100%
        assert_eq!(sa.verses[5], 25); // heavy: 25%

        let ap: &WarheadType = rules.warhead("AP").expect("AP exists");
        assert_eq!(ap.verses[6], 60); // wood: 60%
    }

    #[test]
    fn refinery_helpers_are_data_driven_and_case_insensitive() {
        let ini = IniFile::from_str(
            "[InfantryTypes]\n\
             [VehicleTypes]\n\
             0=MODHARV\n\
             [AircraftTypes]\n\
             [BuildingTypes]\n\
             0=MODPROC\n\
             1=FAKEREF\n\
             [MODHARV]\n\
             Harvester=yes\n\
             Dock=modproc\n\
             [MODPROC]\n\
             Refinery=yes\n\
             FreeUnit=modharv\n\
             [FAKEREF]\n\
             Name=Fake Refinery\n",
        );
        let rules = RuleSet::from_ini(&ini).expect("Should parse");

        assert!(rules.is_refinery_type("modproc"));
        assert!(!rules.is_refinery_type("FAKEREF"));
        assert_eq!(rules.refinery_free_unit("MODPROC"), Some("MODHARV"));
        assert!(rules.harvester_can_dock_at("modharv", "MODPROC"));
        assert!(!rules.harvester_can_dock_at("modharv", "GAREFN"));
    }

    #[test]
    fn refinery_free_unit_ignores_missing_target() {
        let ini = IniFile::from_str(
            "[InfantryTypes]\n\
             [VehicleTypes]\n\
             [AircraftTypes]\n\
             [BuildingTypes]\n\
             0=MODPROC\n\
             [MODPROC]\n\
             Refinery=yes\n\
             FreeUnit=UNKNOWN\n",
        );
        let rules = RuleSet::from_ini(&ini).expect("Should parse");

        assert!(rules.is_refinery_type("MODPROC"));
        assert_eq!(rules.refinery_free_unit("MODPROC"), None);
    }

    #[test]
    fn harvester_scan_radii_parsed_from_general() {
        let ini = IniFile::from_str(
            "[InfantryTypes]\n\
             [VehicleTypes]\n\
             [AircraftTypes]\n\
             [BuildingTypes]\n\
             [General]\n\
             TiberiumShortScan=10\n\
             TiberiumLongScan=60\n\
             SlaveMinerShortScan=12\n\
             SlaveMinerSlaveScan=20\n\
             SlaveMinerLongScan=55\n\
             SlaveMinerScanCorrection=5\n\
             SlaveMinerKickFrameDelay=200\n\
             HarvesterTooFarDistance=8\n\
             ChronoHarvTooFarDistance=40\n\
             PurifierBonus=.30\n",
        );
        let rules = RuleSet::from_ini(&ini).expect("Should parse");
        assert_eq!(rules.general.tiberium_short_scan, 10);
        assert_eq!(rules.general.tiberium_long_scan, 60);
        assert_eq!(rules.general.slave_miner_short_scan, 12);
        assert_eq!(rules.general.slave_miner_slave_scan, 20);
        assert_eq!(rules.general.slave_miner_long_scan, 55);
        assert_eq!(rules.general.slave_miner_scan_correction, 5);
        assert_eq!(rules.general.slave_miner_kick_frame_delay, 200);
        assert_eq!(rules.general.harvester_too_far_distance, 8);
        assert_eq!(rules.general.chrono_harv_too_far_distance, 40);
        assert!((rules.general.purifier_bonus - 0.30).abs() < 0.001);
    }

    #[test]
    fn harvester_scan_radii_use_defaults_when_missing() {
        let ini = IniFile::from_str(
            "[InfantryTypes]\n\
             [VehicleTypes]\n\
             [AircraftTypes]\n\
             [BuildingTypes]\n\
             [General]\n",
        );
        let rules = RuleSet::from_ini(&ini).expect("Should parse");
        assert_eq!(rules.general.tiberium_short_scan, 6);
        assert_eq!(rules.general.tiberium_long_scan, 48);
        assert_eq!(rules.general.slave_miner_short_scan, 8);
        assert_eq!(rules.general.slave_miner_slave_scan, 14);
        assert_eq!(rules.general.slave_miner_long_scan, 48);
        assert_eq!(rules.general.slave_miner_scan_correction, 3);
        assert_eq!(rules.general.slave_miner_kick_frame_delay, 150);
        assert_eq!(rules.general.harvester_too_far_distance, 5);
        assert_eq!(rules.general.chrono_harv_too_far_distance, 50);
        assert!((rules.general.purifier_bonus - 0.25).abs() < 0.001);
    }

    #[test]
    fn bridge_rules_load_from_ini() {
        let ini = IniFile::from_str(
            "[InfantryTypes]\n\
             [VehicleTypes]\n\
             [AircraftTypes]\n\
             [BuildingTypes]\n\
             [CombatDamage]\n\
             BridgeStrength=900\n\
             [SpecialFlags]\n\
             DestroyableBridges=no\n",
        );
        let rules = RuleSet::from_ini(&ini).expect("Should parse");
        assert_eq!(rules.bridge_rules.strength, 900);
        assert!(!rules.bridge_rules.destroyable_by_default);
    }
}
