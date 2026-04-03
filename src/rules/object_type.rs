//! Game object type definitions parsed from rules.ini.
//!
//! Every unit, vehicle, aircraft, and building in RA2 is defined by a section in
//! rules.ini. This module provides the `ObjectType` struct which captures the
//! common properties shared by all game objects. Object-specific behavior (e.g.,
//! infantry prone stance, building power grid) is handled by category-specific
//! fields with sensible defaults.
//!
//! ## rules.ini format
//! ```ini
//! [MTNK]
//! Name=Grizzly Battle Tank
//! Cost=700
//! Strength=300
//! Armor=heavy
//! Speed=6
//! Sight=6
//! TechLevel=2
//! Owner=Americans,Alliance,British,French,Germans,Koreans
//! Prerequisite=GAWEAP
//! Primary=105mm
//! Image=MTNK
//! ```
//!
//! ## Dependency rules
//! - Part of rules/ — no dependencies on sim/, render/, ui/, etc.

use crate::rules::ini_parser::IniSection;
use crate::rules::jumpjet_params::JumpjetParams;
use crate::rules::locomotor_type::{LocomotorKind, MovementZone, SpeedType};
use crate::util::fixed_math::{SimFixed, sim_from_f32};

/// Which type registry an object belongs to.
///
/// Determines which `[XxxTypes]` section listed this object and affects
/// which game behaviors apply (e.g., only buildings have power, only
/// infantry can garrison).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ObjectCategory {
    Infantry,
    Vehicle,
    Aircraft,
    Building,
}

/// Sidebar/build-queue classification for buildable buildings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildCategory {
    Tech,
    Resource,
    Power,
    Infrastructure,
    Combat,
}

impl BuildCategory {
    fn from_ini(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "tech" => Some(Self::Tech),
            "resource" | "resoure" => Some(Self::Resource),
            "power" => Some(Self::Power),
            "infrastructure" => Some(Self::Infrastructure),
            "combat" => Some(Self::Combat),
            _ => None,
        }
    }
}

/// What pip display to show below a unit's health bar (PipScale= in rules.ini).
///
/// Controls the type of pip overlay rendered beneath selected units:
/// - `Tiberium`: cargo fill pips for harvesters (green=ore, colored=gem)
/// - `Passengers`: passenger count pips for transports
/// - `Ammo`: ammunition count pips
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PipScale {
    #[default]
    None,
    Tiberium,
    Passengers,
    Ammo,
}

impl PipScale {
    fn from_ini(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "tiberium" => Self::Tiberium,
            "passengers" => Self::Passengers,
            "ammo" => Self::Ammo,
            _ => Self::None,
        }
    }
}

/// What type of objects this building can produce (Factory= in rules.ini).
///
/// RA2 uses this key to determine which production queue a building serves:
/// a building with `Factory=InfantryType` acts as a barracks, one with
/// `Factory=UnitType` as a war factory, etc. This replaces hardcoded
/// building-name checks and lets modders add new factories without code changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FactoryType {
    /// Produces buildings (ConYards: GACNST, NACNST, YACNST).
    BuildingType,
    /// Produces infantry (Barracks: GAPILE, NAHAND, YABRCK).
    InfantryType,
    /// Produces vehicles (War Factories: GAWEAP, NAWEAP, YAWEAP).
    UnitType,
    /// Produces aircraft (Airfields: GAAIRC, AMRADR).
    AircraftType,
}

impl FactoryType {
    /// Parse the Factory= INI value (case-insensitive).
    pub fn from_ini(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "buildingtype" => Some(Self::BuildingType),
            "infantrytype" => Some(Self::InfantryType),
            "unittype" => Some(Self::UnitType),
            "aircrafttype" => Some(Self::AircraftType),
            _ => None,
        }
    }
}

/// A game object definition parsed from a rules.ini section.
///
/// Fields use sensible defaults when the INI key is absent, matching
/// the original game's behavior (RA2 uses hardcoded defaults for missing keys).
#[derive(Debug, Clone)]
pub struct ObjectType {
    /// Section name in rules.ini (e.g., "MTNK", "E1", "GAWEAP").
    /// This is the unique identifier used throughout the engine.
    pub id: String,
    /// Which type registry this object belongs to.
    pub category: ObjectCategory,
    /// Display name (CSF string table key or raw text). None if not specified.
    pub name: Option<String>,
    /// Credit cost to produce this object.
    pub cost: i32,
    /// Hit points (health). 0 = invincible or not applicable.
    pub strength: i32,
    /// Armor type name (e.g., "heavy", "light", "wood"). Determines damage
    /// multipliers from warhead Verses= values.
    pub armor: String,
    /// Movement speed (0 = immobile, e.g., buildings).
    pub speed: i32,
    /// Fraction of max speed gained per tick during acceleration (AccelerationFactor=).
    /// Default 0.03. At 15 fps, reaches max speed in ~2 seconds.
    pub accel_factor: SimFixed,
    /// Fraction of max speed lost per tick during braking (DeaccelerationFactor=).
    /// Default 0.02. Applied when within slowdown_distance of destination.
    pub decel_factor: SimFixed,
    /// Lepton distance from destination at which braking begins (SlowdownDistance=).
    /// Default 512 (~2 cells). Original engine default is 500.
    pub slowdown_distance: i32,
    /// Vision range in cells.
    pub sight: i32,
    /// Technology level required (-1 = unbuildable by player).
    pub tech_level: i32,
    /// Multiplier to the time it takes for this object to be built.
    pub build_time_multiplier: f32,
    /// `build_time_multiplier` pre-scaled ×1000 for deterministic build-time computation.
    pub build_time_multiplier_x1000: u64,
    /// Which houses/sides can build this (e.g., ["Americans", "Alliance"]).
    pub owner: Vec<String>,
    /// Specific countries that may build this object.
    pub required_houses: Vec<String>,
    /// Countries explicitly forbidden from building this (ForbiddenHouses= in rules.ini).
    /// Inverse of Owner — if the player's country is in this list, they cannot build.
    pub forbidden_houses: Vec<String>,
    /// Building prerequisites required before this can be built.
    pub prerequisite: Vec<String>,
    /// Alternative prerequisite path (PrerequisiteOverride= in rules.ini).
    /// If non-empty AND the owner has ANY building from this list, the normal
    /// Prerequisite check is skipped entirely (OR logic).
    pub prerequisite_override: Vec<String>,
    /// Maximum simultaneous copies allowed (BuildLimit= in rules.ini). Default 0 = unlimited.
    /// Positive: hard cap. Negative: abs value cap with rebuild-after-death semantics.
    pub build_limit: i32,
    /// Requires spy infiltration of an Allied Battle Lab to unlock.
    pub requires_stolen_allied_tech: bool,
    /// Requires spy infiltration of a Soviet Battle Lab to unlock.
    pub requires_stolen_soviet_tech: bool,
    /// Requires spy infiltration of a Yuri Battle Lab to unlock.
    pub requires_stolen_third_tech: bool,
    /// Primary weapon ID (references a [WeaponName] section).
    pub primary: Option<String>,
    /// Secondary weapon ID (e.g., anti-air for dual-purpose units).
    pub secondary: Option<String>,
    /// Art.ini image reference. Defaults to the object's ID if not specified.
    /// Used to look up sprite/voxel filenames in art.ini.
    pub image: String,
    /// Power generation (positive) or consumption (negative). Buildings only.
    pub power: i32,
    /// Building foundation footprint (e.g., "3x2", "1x1"). Buildings only.
    pub foundation: String,
    /// Pixel offset for health bar / selection bracket Y position.
    /// Negative values shift the bar UP (above taller sprites). Default 0.
    /// Parsed from `PixelSelectionBracketDelta` in rules.ini.
    pub pixel_selection_bracket_delta: i32,
    /// Sidebar/build tab grouping for structures.
    pub build_cat: Option<BuildCategory>,
    /// Human placement radius away from existing base-normal structures.
    pub adjacent: i32,
    /// Whether this structure expands the owner's build area.
    pub base_normal: bool,
    /// Whether selling/destruction can eject infantry crew from this structure.
    pub crewed: bool,
    /// Sound ID played when this unit is selected (references sound.ini section).
    pub voice_select: Option<String>,
    /// Sound ID played when this unit is ordered to move.
    pub voice_move: Option<String>,
    /// Sound ID played when this unit is ordered to attack.
    pub voice_attack: Option<String>,
    /// Sound ID played when this entity dies or is destroyed.
    pub die_sound: Option<String>,
    /// Sound ID played while this entity moves (looping engine/footstep).
    pub move_sound: Option<String>,
    /// Whether this unit has an independently rotating turret.
    /// Parsed from rules.ini `Turret=yes`. Only meaningful for vehicles/aircraft.
    pub has_turret: bool,
    /// Turret rotation speed in RA2 "ROT" units (degrees per game frame at 15fps).
    /// Higher = faster turret rotation. Only meaningful when `has_turret` is true.
    /// Typical values: 5 (War Miner), 7 (Grizzly/Rhino).
    pub turret_rot: i32,
    /// VXL turret model name for buildings (TurretAnim= in rules.ini, e.g., "SAM").
    /// The engine loads `{TurretAnim}.VXL` + `{TurretAnim}.HVA` as the turret model.
    pub turret_anim: Option<String>,
    /// Whether the turret anim is a VXL model (TurretAnimIsVoxel=, default false).
    /// When false, the TurretAnim is an SHP overlay handled by the building anim system.
    pub turret_anim_is_voxel: bool,
    /// Pixel X offset for building turret placement (TurretAnimX=).
    pub turret_anim_x: i32,
    /// Pixel Y offset for building turret placement (TurretAnimY=).
    pub turret_anim_y: i32,
    /// Depth adjustment for building turret (TurretAnimZAdjust=, negative = behind).
    pub turret_anim_z_adjust: i32,
    /// Scan radius in cells for auto-targeting idle enemies. If None, defaults
    /// to the primary weapon's range at runtime.
    pub guard_range: Option<SimFixed>,
    /// Whether this unit fires a warhead at its own position on death (e.g.,
    /// Apocalypse Tank explosion damages nearby units).
    pub explodes: bool,
    /// Specific weapon fired on death (overrides default explosion behavior).
    /// References a [WeaponName] section in rules.ini.
    pub death_weapon: Option<String>,
    /// Superweapon type ID granted when this building is completed (SuperWeapon= in rules.ini).
    /// References a section listed in [SuperWeaponTypes].
    pub super_weapon: Option<String>,
    /// Secondary superweapon type ID, typically from an upgrade (SuperWeapon2= in rules.ini).
    pub super_weapon2: Option<String>,
    /// When true, this building provides full map vision while powered.
    /// Used by the Allied Spy Satellite Uplink (GASPYSAT).
    pub spy_sat: bool,
    /// When true, this building/unit emits a gap field that hides enemy vision
    /// within GapRadius cells (parsed from [General]).
    pub gap_generator: bool,
    /// When true, this building activates the owner's radar display (minimap).
    /// Radar=yes in rules.ini. Used by GARADR (Allied), NARADR (Soviet), YARADR (Yuri).
    /// SpySat=yes buildings also implicitly provide radar.
    pub radar: bool,
    /// When true, this unit does NOT appear on enemy radar even when in line of sight.
    /// RadarInvisible= in rules.ini. Used by subs, Night Hawk, dolphins, giant squid.
    pub radar_invisible: bool,
    /// When true, this unit ALWAYS appears on radar even when under shroud.
    /// RadarVisible= in rules.ini. Used by certain special objects.
    pub radar_visible: bool,
    /// Whether this unit is a resource harvester (Harvester=yes in rules.ini).
    /// Data-driven replacement for hardcoded type ID string checks.
    pub harvester: bool,
    /// Whether this structure accepts ore/gem delivery (Refinery=yes in rules.ini).
    pub refinery: bool,
    /// Bonus credits storage for refineries (Storage= in rules.ini).
    /// Refineries typically have Storage=300 — added to owner credits on placement.
    pub storage: i32,
    /// Free unit spawned when this structure is placed (FreeUnit= in rules.ini).
    pub free_unit: Option<String>,
    /// Structures this unit may dock with (Dock= in rules.ini), normalized uppercase.
    pub dock: Vec<String>,
    /// Queueing cell offset from building origin (QueueingCell= in art.ini).
    /// Where miners wait outside the dock. Merged from art.ini during init.
    pub queueing_cell: Option<(u16, u16)>,
    /// First docking offset from art.ini (DockingOffset0=X,Y,Z in leptons).
    /// Where units sit during dock operations. Merged from art.ini during init.
    pub docking_offset: Option<(i32, i32, i32)>,
    /// Alternative VXL model displayed while unloading at a refinery (UnloadingClass= in rules.ini).
    /// e.g. HARV uses HORV (harvester without ore bin), CMIN uses CMON.
    pub unloading_class: Option<String>,
    /// Ammo count for aircraft. -1 = unlimited (default), 0+ = finite.
    /// Aircraft with finite ammo return to a helipad/airfield to reload after depleting.
    pub ammo: i32,

    // -- Slave Miner / economy fields --
    /// Infantry type enslaved/spawned by this unit (Enslaves= in rules.ini, YR only).
    /// Used by Slave Miner (SMIN) to spawn SLAV workers.
    pub enslaves: Option<String>,
    /// Number of slaves to spawn (SlavesNumber= in rules.ini). Default 0.
    pub slaves_number: i32,
    /// Frames before a dead slave is regenerated (SlaveRegenRate= in rules.ini). Default 0.
    pub slave_regen_rate: u32,
    /// Minimum frames between individual slave respawns (SlaveReloadRate= in rules.ini). Default 0.
    pub slave_reload_rate: u32,
    /// Whether this infantry is a slave unit (Slaved=yes in rules.ini).
    /// Slave units are bound to a master (Slave Miner) and have restricted AI.
    pub slaved: bool,
    /// Frames between bale pickups for slave harvesters (HarvestRate= in rules.ini). Default 0.
    pub harvest_rate: u32,
    /// AI flag: this unit earns money (ResourceGatherer=yes in rules.ini). Default false.
    pub resource_gatherer: bool,
    /// AI flag: this is a resource delivery point (ResourceDestination=yes in rules.ini). Default false.
    pub resource_destination: bool,
    /// Whether this building is an Ore Purifier (OrePurifier=yes in rules.ini).
    /// Owning one grants a PurifierBonus to all harvested ore.
    pub ore_purifier: bool,

    // -- Locomotor / movement fields --
    /// Which locomotor class controls this unit's movement (parsed from Locomotor= CLSID).
    pub locomotor: LocomotorKind,
    /// Which terrain cells are traversable (SpeedType= in rules.ini).
    pub speed_type: SpeedType,
    /// Pathfinder routing assumptions (MovementZone= in rules.ini).
    pub movement_zone: MovementZone,
    /// Whether this unit is treated as aircraft for game logic (ConsideredAircraft=).
    pub considered_aircraft: bool,
    /// Per-type render depth bias used when a unit is near or under a bridge.
    /// Original engine default is 7 when the key is absent.
    pub zfudge_bridge: i32,
    /// Prevents naval/large units from traversing under bridge structural cells.
    pub too_big_to_fit_under_bridge: bool,
    /// Whether this unit shows a visible crash animation on death (Crashable=).
    pub crashable: bool,
    /// Whether this unit can use chrono teleport movement (Teleporter=).
    pub teleporter: bool,
    /// Whether this unit can fire while hovering / in air (HoverAttack=).
    pub hover_attack: bool,
    /// Whether this unit stays airborne by default / doesn't land (BalloonHover=).
    pub balloon_hover: bool,
    /// AirportBound=yes — aircraft must dock at helipad; crashes if none available.
    pub airport_bound: bool,
    /// Fighter=yes — fighter aircraft classification (affects targeting).
    pub fighter: bool,
    /// FlyBy=yes — strafing fly-by attack pattern (continue forward after firing).
    pub fly_by: bool,
    /// FlyBack=yes — after fly-by, reverse course back over target.
    pub fly_back: bool,
    /// Landable=yes — aircraft can land on the ground.
    pub landable: bool,
    /// Whether this unit uses jumpjet controls (JumpJet= in rules.ini).
    pub jumpjet: bool,
    /// Jumpjet-specific tuning parameters. Only populated when `jumpjet` is true.
    pub jumpjet_params: Option<JumpjetParams>,

    // -- Deploy / undeploy fields --
    /// What this unit deploys into (e.g., AMCV DeploysInto=GACNST).
    /// Parsed from rules.ini `DeploysInto=`. Used for MCV→ConYard and similar transforms.
    pub deploys_into: Option<String>,
    /// What this building undeploys into (e.g., GACNST UndeploysInto=AMCV).
    /// Parsed from rules.ini `UndeploysInto=`. Used for ConYard→MCV sell-back.
    pub undeploys_into: Option<String>,

    /// Whether this unit can be crushed by vehicles with Crusher movement zones.
    /// Default: false for all types. Parsed from `Crushable=` in rules.ini.
    /// Only specific infantry (GI, GGI, SEAL, Rocketeer, Lunar) and some walls
    /// have this set to true.
    pub crushable: bool,
    /// Whether this unit can crush non-Crushable targets (only Battle Fortress).
    /// Default: false. Parsed from `OmniCrusher=` in rules.ini.
    pub omni_crusher: bool,
    /// Whether this unit is immune to ALL crush types including OmniCrusher.
    /// Default: false. Parsed from `OmniCrushResistant=` in rules.ini.
    pub omni_crush_resistant: bool,

    /// What type of objects this building can produce (Factory= in rules.ini).
    /// None for non-factory buildings/units. Data-driven replacement for
    /// hardcoded building-name checks in production queue logic.
    pub factory: Option<FactoryType>,

    /// Exit coordinate for produced units, in leptons relative to building origin.
    /// Parsed from `ExitCoord=X,Y,Z` in rules.ini. 256 leptons = 1 cell.
    /// Used by spawn logic to place newly built units near the correct factory exit.
    pub exit_coord: Option<(i32, i32, i32)>,

    // -- Cursor / interaction capability flags --
    // These drive which cursor is shown when hovering this unit/building.
    /// Whether this infantry type behaves as an engineer (captures buildings,
    /// repairs structures). Parsed from `Engineer=yes` in rules.ini.
    /// Triggers `EngineerRepair` cursor on damaged friendly buildings and
    /// `Enter` cursor on capturable enemy buildings when this unit is selected.
    pub engineer: bool,

    /// Whether this unit can self-deploy/undeploy via the Deploy command.
    /// Parsed from `Deployer=yes` in rules.ini. Triggers `Deploy`/`NoDeploy`
    /// cursor when the player hovers over this unit itself.
    pub deployer: bool,

    /// Whether this building can be infiltrated by a spy or captured by an engineer.
    /// Parsed from `Capturable=yes` in rules.ini. Enables the `Enter` cursor
    /// when an enemy Engineer or Spy is selected and hovering this building.
    pub capturable: bool,

    /// Whether this building can be repaired via the Repair command.
    /// Parsed from `Repairable=yes` in rules.ini. Defaults to true for buildings.
    pub repairable: bool,

    /// Whether infantry can garrison/occupy this building.
    /// Parsed from `CanBeOccupied=yes` in rules.ini. Enables `Enter` cursor
    /// for friendly infantry hovering this building.
    pub can_be_occupied: bool,

    /// Whether garrisoned infantry can fire from this building.
    /// Parsed from `CanOccupyFire=yes` in rules.ini. Building must also
    /// have `CanBeOccupied=yes` and at least one occupant for fire to occur.
    pub can_occupy_fire: bool,

    /// Whether to show pip indicators for each occupant inside the building.
    /// Parsed from `ShowOccupantPips=yes` in rules.ini.
    pub show_occupant_pips: bool,

    /// Maximum number of infantry passengers this vehicle can carry.
    /// Parsed from `Passengers=N` in rules.ini. >0 enables `Enter` cursor
    /// for friendly infantry hovering this transport.
    pub passengers: u32,

    /// Maximum Size= of individual passenger allowed (SizeLimit= in rules.ini).
    /// 0 means no size restriction. SizeLimit=2 means only Size<=2 can enter.
    pub size_limit: u32,

    /// How much transport space this unit occupies (Size= in rules.ini).
    /// Infantry default 1, vehicles default 3.
    pub size: u32,

    /// Whether this transport is open-topped — passengers can fire from inside.
    /// Parsed from `OpenTopped=yes` in rules.ini.
    pub open_topped: bool,

    /// Whether this transport uses the Gunner system (IFV weapon swap).
    /// Parsed from `Gunner=yes` in rules.ini. When a passenger enters, the
    /// transport's active weapon changes based on the passenger's IFVMode.
    pub gunner: bool,

    /// Which IFV weapon turret index this infantry type selects when inside
    /// a Gunner=yes transport. Parsed from `IFVMode=N` in rules.ini. Default 0.
    pub ifv_mode: u32,

    /// Maximum number of garrison occupants for CanBeOccupied buildings.
    /// Parsed from `MaxNumberOccupants=N` in rules.ini. Default 0.
    pub max_number_occupants: u32,

    /// Whether this infantry can garrison `CanBeOccupied` buildings.
    /// Parsed from `Occupier=yes` in rules.ini. Only GI and Conscript in base RA2.
    pub occupier: bool,

    /// Whether this infantry can assault enemy buildings (hostile garrison entry).
    /// Parsed from `Assaulter=yes` in rules.ini.
    pub assaulter: bool,

    /// Weapon used when this infantry fires from inside a garrisoned building.
    /// Parsed from `OccupyWeapon=WeaponName` in rules.ini. Falls back to
    /// primary weapon if not specified.
    pub occupy_weapon: Option<String>,

    /// Elite-level weapon used when garrisoned. Falls back to `OccupyWeapon`
    /// or primary weapon if not specified.
    /// Parsed from `EliteOccupyWeapon=WeaponName` in rules.ini.
    pub elite_occupy_weapon: Option<String>,

    /// pips.shp frame index for this infantry when garrisoned in a building.
    /// Parsed from `OccupyPip=PersonGreen` in rules.ini.  Default 7 (PersonGreen).
    /// Values: 7=PersonGreen, 8=PersonYellow, 9=PersonWhite, 10=PersonRed,
    /// 11=PersonBlue, 12=PersonPurple.  Empty slots use frame 6.
    pub occupy_pip: u32,

    /// What pip display to render below the health bar (PipScale= in rules.ini).
    /// `Tiberium` shows per-bale cargo pips for harvesters using pips2.shp.
    pub pip_scale: PipScale,

    /// Whether this building absorbs infantry (Yuri Bio Reactor).
    /// Parsed from `InfantryAbsorb=yes` in rules.ini.
    pub infantry_absorb: bool,

    /// Whether this building absorbs vehicles.
    /// Parsed from `UnitAbsorb=yes` in rules.ini.
    pub unit_absorb: bool,

    /// Indexed weapon list for IFV (Weapon1..Weapon17 in rules.ini).
    /// Only populated when Gunner=yes. Index 0 = Weapon1.
    pub weapon_list: Vec<String>,

    /// Whether this unit shows an `Attack` cursor even on friendly targets.
    /// Parsed from `AttackCursorOnFriendlies=yes` in rules.ini.
    /// Used by Desolator and Boris whose weapons affect friendlies.
    pub attack_cursor_on_friendlies: bool,

    /// Whether this infantry uses the `Enter`/sabotage cursor instead of
    /// the normal `Attack` cursor on enemy structures.
    /// Parsed from `SabotageCursor=yes` in rules.ini. Used by Tanya and Navy SEAL.
    pub sabotage_cursor: bool,

    /// Whether this building repairs docked ground units (UnitRepair=yes in rules.ini).
    /// Used by Service Depots (GADEPT, NADEPT, YADEPT).
    pub unit_repair: bool,
    /// Whether this building reloads ammo for docked aircraft (UnitReload=yes in rules.ini).
    /// Used by Airfields (GAAIRC, NAAIRC).
    pub unit_reload: bool,
    /// Whether this building is a helipad (Helipad=yes in rules.ini).
    pub helipad: bool,
    /// How many units may dock at this building simultaneously (NumberOfDocks= in rules.ini).
    /// Default 1. Airfields typically have 4.
    pub number_of_docks: u8,

    /// Whether this building can be toggled on/off by the player.
    /// Parsed from `TogglePower=yes` in rules.ini.
    /// Defaults to true for buildings (most can be powered down).
    /// Triggers `TogglePower` cursor when hovering this building in power-toggle mode.
    pub toggle_power: bool,

    /// Whether this building is affected by low-power situations.
    /// Parsed from `Powered=yes` in rules.ini. Defaults to true for buildings.
    /// When true and the owner is in low power, the building deactivates:
    /// defenses stop firing, radar goes offline, gap/spysat/superweapons pause.
    /// Power plants (positive Power=) are never deactivated regardless of this flag.
    pub powered: bool,

    /// Whether this unit can use the disguise ability (Spy).
    /// Parsed from `CanDisguise=yes` in rules.ini. Enables `Disguise` cursor
    /// when the selected Spy hovers over an eligible enemy infantry target.
    pub can_disguise: bool,

    /// Whether this building is a wall segment (Wall=yes in rules.ini).
    /// Wall buildings render as overlays (auto-tiled connectivity frames),
    /// not as normal SHP building sprites. GAWALL, NAWALL, GAFWLL etc.
    pub wall: bool,

    // -- Naval flags --
    /// Building requires water placement (WaterBound=yes in INI).
    /// When set, the placement validator checks the water speed column instead
    /// of Buildable. Default: true if SpeedType is Float, false otherwise.
    pub water_bound: bool,
    /// Unit or building is classified as naval (Naval=yes in INI).
    /// Controls AI targeting priority, factory classification, and UI filtering.
    pub naval: bool,
    /// Number of foundation rows (from the top, Y-axis) that are impassable.
    /// Default -1 = all rows impassable. Parsed from NumberImpassableRows= in rules.ini.
    /// Controls which foundation cells units can path through (e.g., war factory
    /// exit lanes, naval yard docks).
    pub number_impassable_rows: i32,

    // -- Point light source fields (from rules.ini, primarily buildings) --
    /// Light emission range in leptons (LightVisibility= in rules.ini). Default 0 (no light).
    /// 256 leptons = 1 cell. Used by lamp posts (GALITE=5000) and other light-emitting buildings.
    pub light_visibility: i32,
    /// Light emission brightness (LightIntensity= in rules.ini). Default 0.0.
    /// Negative values darken the area. Typical range: 0.0–1.0.
    pub light_intensity: f32,
    /// Red channel tint for emitted light (LightRedTint= in rules.ini). Default 1.0.
    pub light_red_tint: f32,
    /// Green channel tint for emitted light (LightGreenTint= in rules.ini). Default 1.0.
    pub light_green_tint: f32,
    /// Blue channel tint for emitted light (LightBlueTint= in rules.ini). Default 1.0.
    pub light_blue_tint: f32,
}

impl ObjectType {
    /// Parse an ObjectType from a rules.ini section.
    ///
    /// Missing keys get sensible defaults matching RA2's behavior.
    /// The `id` is the section name, and `category` comes from which
    /// type registry listed this object.
    pub fn from_ini_section(id: &str, section: &IniSection, category: ObjectCategory) -> Self {
        let owner: Vec<String> = section
            .get_list("Owner")
            .unwrap_or_default()
            .into_iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();

        let prerequisite: Vec<String> = section
            .get_list("Prerequisite")
            .unwrap_or_default()
            .into_iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();

        let required_houses: Vec<String> = section
            .get_list("RequiredHouses")
            .unwrap_or_default()
            .into_iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();

        let forbidden_houses: Vec<String> = section
            .get_list("ForbiddenHouses")
            .unwrap_or_default()
            .into_iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();

        let prerequisite_override: Vec<String> = section
            .get_list("PrerequisiteOverride")
            .unwrap_or_default()
            .into_iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();

        let btm_f32: f32 = section.get_f32("BuildTimeMultiplier").unwrap_or(1.0);

        Self {
            id: id.to_string(),
            category,
            name: section.get("Name").map(|s| s.to_string()),
            cost: section.get_i32("Cost").unwrap_or(0),
            strength: section.get_i32("Strength").unwrap_or(0),
            armor: section.get("Armor").unwrap_or("none").to_string(),
            speed: section.get_i32("Speed").unwrap_or(0),
            accel_factor: section
                .get_f32("AccelerationFactor")
                .map(sim_from_f32)
                .unwrap_or(SimFixed::lit("0.03")),
            decel_factor: section
                .get_f32("DeaccelerationFactor")
                .map(sim_from_f32)
                .unwrap_or(SimFixed::lit("0.002")),
            slowdown_distance: section.get_i32("SlowdownDistance").unwrap_or(500),
            sight: section.get_i32("Sight").unwrap_or(0),
            tech_level: section.get_i32("TechLevel").unwrap_or(-1),
            build_time_multiplier: btm_f32,
            build_time_multiplier_x1000: (btm_f32.max(0.01) as f64 * 1000.0) as u64,
            owner,
            required_houses,
            forbidden_houses,
            prerequisite,
            prerequisite_override,
            build_limit: section.get_i32("BuildLimit").unwrap_or(0),
            requires_stolen_allied_tech: section
                .get_bool("RequiresStolenAlliedTech")
                .unwrap_or(false),
            requires_stolen_soviet_tech: section
                .get_bool("RequiresStolenSovietTech")
                .unwrap_or(false),
            requires_stolen_third_tech: section
                .get_bool("RequiresStolenThirdTech")
                .unwrap_or(false),
            primary: section.get("Primary").map(|s| s.to_string()),
            secondary: section.get("Secondary").map(|s| s.to_string()),
            image: section.get("Image").unwrap_or(id).to_string(),
            power: section.get_i32("Power").unwrap_or(0),
            // In the original engine, Foundation= lives in art.ini, not rules.ini.
            // Art.ini is authoritative — merge_art_data() overwrites this value.
            // We still parse from rules here as a fallback for tests and edge cases.
            foundation: section.get("Foundation").unwrap_or("1x1").to_string(),
            pixel_selection_bracket_delta: section
                .get_i32("PixelSelectionBracketDelta")
                .unwrap_or(0),
            build_cat: section.get("BuildCat").and_then(BuildCategory::from_ini),
            adjacent: section.get_i32("Adjacent").unwrap_or(6),
            base_normal: section.get_bool("BaseNormal").unwrap_or(true),
            crewed: section.get_bool("Crewed").unwrap_or(false),
            voice_select: section.get("VoiceSelect").map(|s| s.to_string()),
            voice_move: section.get("VoiceMove").map(|s| s.to_string()),
            voice_attack: section.get("VoiceAttack").map(|s| s.to_string()),
            die_sound: section.get("DieSound").map(|s| s.to_string()),
            move_sound: section.get("MoveSound").map(|s| s.to_string()),
            has_turret: section.get_bool("Turret").unwrap_or(false),
            turret_rot: section.get_i32("ROT").unwrap_or(0),
            turret_anim: section
                .get("TurretAnim")
                .filter(|s| !s.is_empty())
                .map(|s| s.to_uppercase()),
            turret_anim_is_voxel: section.get_bool("TurretAnimIsVoxel").unwrap_or(false),
            turret_anim_x: section.get_i32("TurretAnimX").unwrap_or(0),
            turret_anim_y: section.get_i32("TurretAnimY").unwrap_or(0),
            turret_anim_z_adjust: section.get_i32("TurretAnimZAdjust").unwrap_or(0),
            guard_range: section.get_f32("GuardRange").map(sim_from_f32),
            explodes: section.get_bool("Explodes").unwrap_or(false),
            death_weapon: section.get("DeathWeapon").map(|s| s.to_string()),
            super_weapon: section.get("SuperWeapon").map(|s| s.to_string()),
            super_weapon2: section.get("SuperWeapon2").map(|s| s.to_string()),
            spy_sat: section.get_bool("SpySat").unwrap_or(false),
            gap_generator: section.get_bool("GapGenerator").unwrap_or(false),
            radar: section.get_bool("Radar").unwrap_or(false),
            radar_invisible: section.get_bool("RadarInvisible").unwrap_or(false),
            radar_visible: section.get_bool("RadarVisible").unwrap_or(false),
            harvester: section.get_bool("Harvester").unwrap_or(false),
            refinery: section.get_bool("Refinery").unwrap_or(false),
            storage: section.get_i32("Storage").unwrap_or(0),
            free_unit: section.get("FreeUnit").map(|s| s.to_string()),
            dock: section
                .get_list("Dock")
                .unwrap_or_default()
                .into_iter()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_ascii_uppercase())
                .collect(),
            queueing_cell: None,  // merged from art.ini later
            docking_offset: None, // merged from art.ini later
            unloading_class: section.get("UnloadingClass").map(|s| s.to_string()),
            ammo: section.get_i32("Ammo").unwrap_or(-1),

            // Slave Miner / economy fields
            enslaves: section.get("Enslaves").map(|s| s.to_string()),
            slaves_number: section.get_i32("SlavesNumber").unwrap_or(0),
            slave_regen_rate: section.get_i32("SlaveRegenRate").unwrap_or(0).max(0) as u32,
            slave_reload_rate: section.get_i32("SlaveReloadRate").unwrap_or(0).max(0) as u32,
            slaved: section.get_bool("Slaved").unwrap_or(false),
            harvest_rate: section.get_i32("HarvestRate").unwrap_or(0).max(0) as u32,
            resource_gatherer: section.get_bool("ResourceGatherer").unwrap_or(false),
            resource_destination: section.get_bool("ResourceDestination").unwrap_or(false),
            ore_purifier: section.get_bool("OrePurifier").unwrap_or(false),

            // Locomotor / movement fields
            locomotor: section
                .get("Locomotor")
                .map(LocomotorKind::from_clsid)
                .unwrap_or_else(|| LocomotorKind::default_for_category(category)),
            speed_type: section
                .get("SpeedType")
                .map(SpeedType::from_ini)
                .unwrap_or_default(),
            movement_zone: section
                .get("MovementZone")
                .map(MovementZone::from_ini)
                .unwrap_or_default(),
            considered_aircraft: section.get_bool("ConsideredAircraft").unwrap_or(false),
            zfudge_bridge: section.get_i32("ZFudgeBridge").unwrap_or(7),
            too_big_to_fit_under_bridge: section
                .get_bool("TooBigToFitUnderBridge")
                .unwrap_or(false),
            crashable: section.get_bool("Crashable").unwrap_or(false),
            teleporter: section.get_bool("Teleporter").unwrap_or(false),
            hover_attack: section.get_bool("HoverAttack").unwrap_or(false),
            balloon_hover: section.get_bool("BalloonHover").unwrap_or(false),
            airport_bound: section.get_bool("AirportBound").unwrap_or(false),
            fighter: section.get_bool("Fighter").unwrap_or(false),
            fly_by: section.get_bool("FlyBy").unwrap_or(false),
            fly_back: section.get_bool("FlyBack").unwrap_or(false),
            landable: section.get_bool("Landable").unwrap_or(false),
            jumpjet: section.get_bool("JumpJet").unwrap_or(false),
            jumpjet_params: if section.get_bool("JumpJet").unwrap_or(false) {
                Some(JumpjetParams::from_ini_section(section))
            } else {
                None
            },

            // Crush properties -- default false for all types.
            crushable: section.get_bool("Crushable").unwrap_or(false),
            omni_crusher: section.get_bool("OmniCrusher").unwrap_or(false),
            omni_crush_resistant: section.get_bool("OmniCrushResistant").unwrap_or(false),

            deploys_into: section.get("DeploysInto").map(|s| s.to_string()),
            undeploys_into: section.get("UndeploysInto").map(|s| s.to_string()),
            factory: section.get("Factory").and_then(FactoryType::from_ini),
            exit_coord: parse_exit_coord(section.get("ExitCoord")),

            // Cursor / interaction capability flags
            engineer: section.get_bool("Engineer").unwrap_or(false),
            deployer: section.get_bool("Deployer").unwrap_or(false),
            capturable: section.get_bool("Capturable").unwrap_or(false),
            // Repairable defaults to true — most buildings can be repaired in RA2.
            repairable: section.get_bool("Repairable").unwrap_or(true),
            can_be_occupied: section.get_bool("CanBeOccupied").unwrap_or(false),
            can_occupy_fire: section.get_bool("CanOccupyFire").unwrap_or(false),
            show_occupant_pips: section.get_bool("ShowOccupantPips").unwrap_or(true),
            passengers: section.get_i32("Passengers").unwrap_or(0).max(0) as u32,
            size_limit: section.get_i32("SizeLimit").unwrap_or(0).max(0) as u32,
            size: section
                .get_i32("Size")
                .unwrap_or(if category == ObjectCategory::Infantry {
                    1
                } else {
                    3
                })
                .max(0) as u32,
            open_topped: section.get_bool("OpenTopped").unwrap_or(false),
            gunner: section.get_bool("Gunner").unwrap_or(false),
            ifv_mode: section.get_i32("IFVMode").unwrap_or(0).max(0) as u32,
            max_number_occupants: section.get_i32("MaxNumberOccupants").unwrap_or(0).max(0) as u32,
            occupier: section.get_bool("Occupier").unwrap_or(false),
            assaulter: section.get_bool("Assaulter").unwrap_or(false),
            occupy_weapon: section.get("OccupyWeapon").map(|s| s.to_string()),
            elite_occupy_weapon: section.get("EliteOccupyWeapon").map(|s| s.to_string()),
            occupy_pip: section
                .get("OccupyPip")
                .map(|s| match s.to_ascii_lowercase().as_str() {
                    "persongreen" => 7,
                    "personyellow" => 8,
                    "personwhite" => 9,
                    "personred" => 10,
                    "personblue" => 11,
                    "personpurple" => 12,
                    _ => 7,
                })
                .unwrap_or(7),
            pip_scale: section
                .get("PipScale")
                .map(|s| PipScale::from_ini(s))
                .unwrap_or_default(),
            infantry_absorb: section.get_bool("InfantryAbsorb").unwrap_or(false),
            unit_absorb: section.get_bool("UnitAbsorb").unwrap_or(false),
            weapon_list: if section.get_bool("Gunner").unwrap_or(false) {
                (1..=17)
                    .filter_map(|i| section.get(&format!("Weapon{}", i)).map(|s| s.to_string()))
                    .collect()
            } else {
                Vec::new()
            },
            attack_cursor_on_friendlies: section
                .get_bool("AttackCursorOnFriendlies")
                .unwrap_or(false),
            sabotage_cursor: section.get_bool("SabotageCursor").unwrap_or(false),
            unit_repair: section.get_bool("UnitRepair").unwrap_or(false),
            unit_reload: section.get_bool("UnitReload").unwrap_or(false),
            helipad: section.get_bool("Helipad").unwrap_or(false),
            number_of_docks: section.get_i32("NumberOfDocks").unwrap_or(1).max(1) as u8,
            // TogglePower defaults to true for buildings, false for units.
            toggle_power: section
                .get_bool("TogglePower")
                .unwrap_or(category == ObjectCategory::Building),
            // Powered defaults to true for buildings — most deactivate during low power.
            powered: section
                .get_bool("Powered")
                .unwrap_or(category == ObjectCategory::Building),
            can_disguise: section.get_bool("CanDisguise").unwrap_or(false),
            wall: section.get_bool("Wall").unwrap_or(false),

            // Naval flags
            water_bound: {
                // WaterBound defaults to true if SpeedType is already Float.
                let default = section
                    .get("SpeedType")
                    .map_or(false, |s| s.eq_ignore_ascii_case("Float"));
                section.get_bool("WaterBound").unwrap_or(default)
            },
            naval: section.get_bool("Naval").unwrap_or(false),
            number_impassable_rows: section.get_i32("NumberImpassableRows").unwrap_or(-1),

            // Point light source fields
            light_visibility: section.get_i32("LightVisibility").unwrap_or(0),
            light_intensity: section.get_f32("LightIntensity").unwrap_or(0.0),
            light_red_tint: section.get_f32("LightRedTint").unwrap_or(1.0),
            light_green_tint: section.get_f32("LightGreenTint").unwrap_or(1.0),
            light_blue_tint: section.get_f32("LightBlueTint").unwrap_or(1.0),
        }
    }
}

/// Parse ExitCoord=X,Y,Z from rules.ini. Values are in leptons (256 = 1 cell).
fn parse_exit_coord(value: Option<&str>) -> Option<(i32, i32, i32)> {
    let val = value?;
    let parts: Vec<&str> = val.split(',').collect();
    if parts.len() >= 2 {
        let x: i32 = parts[0].trim().parse().ok()?;
        let y: i32 = parts[1].trim().parse().ok()?;
        let z: i32 = parts
            .get(2)
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        Some((x, y, z))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::ini_parser::IniFile;

    #[test]
    fn test_parse_vehicle() {
        let ini: IniFile = IniFile::from_str(
            "[MTNK]\nName=Grizzly Battle Tank\nCost=700\nStrength=300\n\
             Armor=heavy\nSpeed=6\nSight=6\nTechLevel=2\n\
             Owner=Americans,Alliance\nRequiredHouses=Americans\n\
             Prerequisite=GAWEAP\nPrimary=105mm\n",
        );
        let section: &IniSection = ini.section("MTNK").unwrap();
        let obj: ObjectType =
            ObjectType::from_ini_section("MTNK", section, ObjectCategory::Vehicle);

        assert_eq!(obj.id, "MTNK");
        assert_eq!(obj.category, ObjectCategory::Vehicle);
        assert_eq!(obj.name, Some("Grizzly Battle Tank".to_string()));
        assert_eq!(obj.cost, 700);
        assert_eq!(obj.strength, 300);
        assert_eq!(obj.armor, "heavy");
        assert_eq!(obj.speed, 6);
        assert_eq!(obj.tech_level, 2);
        assert!((obj.build_time_multiplier - 1.0).abs() < f32::EPSILON);
        assert_eq!(obj.owner, vec!["Americans", "Alliance"]);
        assert_eq!(obj.required_houses, vec!["Americans"]);
        assert_eq!(obj.prerequisite, vec!["GAWEAP"]);
        assert_eq!(obj.primary, Some("105mm".to_string()));
        assert_eq!(obj.secondary, None);
        assert_eq!(obj.image, "MTNK"); // Defaults to ID when Image= absent.
        assert_eq!(obj.build_cat, None);
        assert_eq!(obj.adjacent, 6);
        assert!(obj.base_normal);
        assert!(!obj.crewed);
    }

    #[test]
    fn test_parse_building() {
        let ini: IniFile = IniFile::from_str(
            "[GAPOWR]\nName=Power Plant\nCost=800\nStrength=750\n\
             Power=200\nFoundation=2x2\nArmor=wood\nBuildCat=Power\nCrewed=yes\n",
        );
        let section: &IniSection = ini.section("GAPOWR").unwrap();
        let obj: ObjectType =
            ObjectType::from_ini_section("GAPOWR", section, ObjectCategory::Building);

        assert_eq!(obj.power, 200);
        assert_eq!(obj.foundation, "2x2");
        assert_eq!(obj.armor, "wood");
        assert_eq!(obj.build_cat, Some(BuildCategory::Power));
        assert_eq!(obj.adjacent, 6);
        assert!(obj.base_normal);
        assert!(obj.crewed);
    }

    #[test]
    fn test_defaults_for_missing_keys() {
        let ini: IniFile = IniFile::from_str("[BARE]\n");
        let section: &IniSection = ini.section("BARE").unwrap();
        let obj: ObjectType =
            ObjectType::from_ini_section("BARE", section, ObjectCategory::Infantry);

        assert_eq!(obj.cost, 0);
        assert_eq!(obj.strength, 0);
        assert_eq!(obj.armor, "none");
        assert_eq!(obj.speed, 0);
        assert_eq!(obj.sight, 0);
        assert_eq!(obj.tech_level, -1);
        assert!((obj.build_time_multiplier - 1.0).abs() < f32::EPSILON);
        assert!(obj.owner.is_empty());
        assert!(obj.required_houses.is_empty());
        assert!(obj.prerequisite.is_empty());
        assert_eq!(obj.primary, None);
        assert_eq!(obj.image, "BARE");
        assert_eq!(obj.power, 0);
        assert_eq!(obj.foundation, "1x1");
        assert_eq!(obj.build_cat, None);
        assert_eq!(obj.adjacent, 6);
        assert!(obj.base_normal);
        assert!(!obj.crewed);
    }

    #[test]
    fn test_parse_build_time_multiplier() {
        let ini: IniFile = IniFile::from_str("[HTNK]\nBuildTimeMultiplier=1.3\n");
        let section: &IniSection = ini.section("HTNK").unwrap();
        let obj: ObjectType =
            ObjectType::from_ini_section("HTNK", section, ObjectCategory::Vehicle);

        assert!((obj.build_time_multiplier - 1.3).abs() < 0.0001);
    }

    #[test]
    fn test_parse_building_placement_flags() {
        let ini: IniFile =
            IniFile::from_str("[GAGAP]\nFoundation=2x2\nAdjacent=0\nBaseNormal=no\n");
        let section: &IniSection = ini.section("GAGAP").unwrap();
        let obj: ObjectType =
            ObjectType::from_ini_section("GAGAP", section, ObjectCategory::Building);

        assert_eq!(obj.adjacent, 0);
        assert!(!obj.base_normal);
    }

    #[test]
    fn test_parse_bridge_render_flags() {
        let ini: IniFile =
            IniFile::from_str("[DEST]\nZFudgeBridge=11\nTooBigToFitUnderBridge=yes\n");
        let section: &IniSection = ini.section("DEST").unwrap();
        let obj: ObjectType =
            ObjectType::from_ini_section("DEST", section, ObjectCategory::Vehicle);
        assert_eq!(obj.zfudge_bridge, 11);
        assert!(obj.too_big_to_fit_under_bridge);
    }

    #[test]
    fn test_bridge_render_flags_default() {
        let ini: IniFile = IniFile::from_str("[BOAT]\n");
        let section: &IniSection = ini.section("BOAT").unwrap();
        let obj: ObjectType =
            ObjectType::from_ini_section("BOAT", section, ObjectCategory::Vehicle);
        assert_eq!(obj.zfudge_bridge, 7);
        assert!(!obj.too_big_to_fit_under_bridge);
    }

    #[test]
    fn test_parse_deploys_into() {
        let ini: IniFile = IniFile::from_str("[AMCV]\nDeploysInto=GACNST\nSpeed=4\n");
        let section: &IniSection = ini.section("AMCV").unwrap();
        let obj: ObjectType =
            ObjectType::from_ini_section("AMCV", section, ObjectCategory::Vehicle);
        assert_eq!(obj.deploys_into, Some("GACNST".to_string()));
        assert_eq!(obj.undeploys_into, None);
    }

    #[test]
    fn test_parse_undeploys_into() {
        let ini: IniFile = IniFile::from_str("[GACNST]\nUndeploysInto=AMCV\nFoundation=4x4\n");
        let section: &IniSection = ini.section("GACNST").unwrap();
        let obj: ObjectType =
            ObjectType::from_ini_section("GACNST", section, ObjectCategory::Building);
        assert_eq!(obj.undeploys_into, Some("AMCV".to_string()));
        assert_eq!(obj.deploys_into, None);
    }

    #[test]
    fn test_parse_slave_miner_fields() {
        let ini: IniFile = IniFile::from_str(
            "[SMIN]\nEnslaves=SLAV\nSlavesNumber=5\nSlaveRegenRate=500\n\
             SlaveReloadRate=25\nResourceGatherer=yes\nResourceDestination=yes\n\
             DeploysInto=YAREFN\nStorage=20\nSpeed=3\n",
        );
        let section: &IniSection = ini.section("SMIN").unwrap();
        let obj: ObjectType =
            ObjectType::from_ini_section("SMIN", section, ObjectCategory::Vehicle);
        assert_eq!(obj.enslaves, Some("SLAV".to_string()));
        assert_eq!(obj.slaves_number, 5);
        assert_eq!(obj.slave_regen_rate, 500);
        assert_eq!(obj.slave_reload_rate, 25);
        assert!(obj.resource_gatherer);
        assert!(obj.resource_destination);
        assert_eq!(obj.deploys_into, Some("YAREFN".to_string()));
        assert!(!obj.slaved); // SMIN is master, not slave
    }

    #[test]
    fn test_parse_slave_infantry_fields() {
        let ini: IniFile = IniFile::from_str("[SLAV]\nSlaved=yes\nStorage=4\nHarvestRate=150\n");
        let section: &IniSection = ini.section("SLAV").unwrap();
        let obj: ObjectType =
            ObjectType::from_ini_section("SLAV", section, ObjectCategory::Infantry);
        assert!(obj.slaved);
        assert_eq!(obj.storage, 4);
        assert_eq!(obj.harvest_rate, 150);
        assert_eq!(obj.enslaves, None);
    }

    #[test]
    fn test_parse_ore_purifier() {
        let ini: IniFile = IniFile::from_str("[GAPROC]\nOrePurifier=yes\n");
        let section: &IniSection = ini.section("GAPROC").unwrap();
        let obj: ObjectType =
            ObjectType::from_ini_section("GAPROC", section, ObjectCategory::Building);
        assert!(obj.ore_purifier);
    }

    #[test]
    fn test_parse_refinery_free_unit_and_dock() {
        let ini: IniFile = IniFile::from_str(
            "[MODPROC]\nRefinery=yes\nFreeUnit=MODHARV\n\
             [MODHARV]\nHarvester=yes\nDock=modproc,NAREFN\n",
        );

        let refinery = ObjectType::from_ini_section(
            "MODPROC",
            ini.section("MODPROC").expect("MODPROC section"),
            ObjectCategory::Building,
        );
        assert!(refinery.refinery);
        assert_eq!(refinery.free_unit, Some("MODHARV".to_string()));

        let harvester = ObjectType::from_ini_section(
            "MODHARV",
            ini.section("MODHARV").expect("MODHARV section"),
            ObjectCategory::Vehicle,
        );
        assert!(harvester.harvester);
        assert_eq!(
            harvester.dock,
            vec!["MODPROC".to_string(), "NAREFN".to_string()]
        );
    }

    #[test]
    fn test_parse_transport_fields() {
        let ini: IniFile = IniFile::from_str(
            "[HTK]\nPassengers=5\nSizeLimit=2\nOpenTopped=no\nGunner=no\nSize=3\n",
        );
        let section: &IniSection = ini.section("HTK").unwrap();
        let obj = ObjectType::from_ini_section("HTK", section, ObjectCategory::Vehicle);
        assert_eq!(obj.passengers, 5);
        assert_eq!(obj.size_limit, 2);
        assert_eq!(obj.size, 3);
        assert!(!obj.open_topped);
        assert!(!obj.gunner);
        assert!(obj.weapon_list.is_empty());
    }

    #[test]
    fn test_parse_ifv_gunner_fields() {
        let ini: IniFile = IniFile::from_str(
            "[FV]\nPassengers=1\nSizeLimit=1\nGunner=yes\nSize=3\n\
             Weapon1=Missiles\nWeapon2=FlakGun\nWeapon3=RepairArm\n",
        );
        let section: &IniSection = ini.section("FV").unwrap();
        let obj = ObjectType::from_ini_section("FV", section, ObjectCategory::Vehicle);
        assert_eq!(obj.passengers, 1);
        assert_eq!(obj.size_limit, 1);
        assert!(obj.gunner);
        assert_eq!(obj.weapon_list, vec!["Missiles", "FlakGun", "RepairArm"]);
    }

    #[test]
    fn test_parse_infantry_ifv_mode() {
        let ini: IniFile = IniFile::from_str("[E1]\nIFVMode=0\nSize=1\n");
        let section: &IniSection = ini.section("E1").unwrap();
        let obj = ObjectType::from_ini_section("E1", section, ObjectCategory::Infantry);
        assert_eq!(obj.ifv_mode, 0);
        assert_eq!(obj.size, 1);
    }

    #[test]
    fn test_parse_garrison_building() {
        let ini: IniFile =
            IniFile::from_str("[GAPOST]\nCanBeOccupied=yes\nMaxNumberOccupants=10\n");
        let section: &IniSection = ini.section("GAPOST").unwrap();
        let obj = ObjectType::from_ini_section("GAPOST", section, ObjectCategory::Building);
        assert!(obj.can_be_occupied);
        assert_eq!(obj.max_number_occupants, 10);
    }

    #[test]
    fn test_parse_absorb_fields() {
        let ini: IniFile = IniFile::from_str("[YABRCK]\nInfantryAbsorb=yes\nUnitAbsorb=no\n");
        let section: &IniSection = ini.section("YABRCK").unwrap();
        let obj = ObjectType::from_ini_section("YABRCK", section, ObjectCategory::Building);
        assert!(obj.infantry_absorb);
        assert!(!obj.unit_absorb);
    }

    #[test]
    fn test_size_defaults_by_category() {
        // Infantry defaults to Size=1
        let ini: IniFile = IniFile::from_str("[INF]\n");
        let section: &IniSection = ini.section("INF").unwrap();
        let obj = ObjectType::from_ini_section("INF", section, ObjectCategory::Infantry);
        assert_eq!(obj.size, 1);

        // Vehicle defaults to Size=3
        let ini2: IniFile = IniFile::from_str("[VEH]\n");
        let section2: &IniSection = ini2.section("VEH").unwrap();
        let obj2 = ObjectType::from_ini_section("VEH", section2, ObjectCategory::Vehicle);
        assert_eq!(obj2.size, 3);
    }
}
