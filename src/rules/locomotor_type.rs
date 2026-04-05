//! Locomotor, SpeedType, and MovementZone enums parsed from rules.ini.
//!
//! RA2/YR movement is a 4-layer system:
//! 1. **LocomotorKind** — runtime state machine class (Drive, Walk, Fly, etc.)
//! 2. **SpeedType** — which terrain cells are actually traversable
//! 3. **MovementZone** — pathfinder routing assumptions and special logic
//! 4. **Per-unit flags** — JumpJet, Teleporter, HoverAttack, etc. (on ObjectType)
//!
//! RA2 identifies locomotors by COM CLSIDs (e.g., `{4A582741-9839-11d1-B709-00A024DDAFD1}`
//! for Drive). We parse these into the `LocomotorKind` enum.
//!
//! ## Dependency rules
//! - Part of rules/ — no dependencies on sim/, render/, ui/, etc.

use crate::rules::object_type::ObjectCategory;

// ---------------------------------------------------------------------------
// LocomotorKind
// ---------------------------------------------------------------------------

/// Which locomotor class controls a unit's movement behavior.
///
/// Each variant is a distinct movement controller / state machine in the
/// original engine. Do NOT collapse these into one generic "ground mover" —
/// they have meaningfully different behavior (see locomotor report).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum LocomotorKind {
    /// Standard ground vehicle movement. Baseline for all ground movers.
    Drive,
    /// Hovering vehicle (Robot Tank, Hover MLRS). ~35% slower than Drive.
    Hover,
    /// Burrowing unit (Terror Drone underground). Two-mode: surface + underground.
    Tunnel,
    /// Infantry ground movement. Distinct arrival threshold from vehicles.
    Walk,
    /// Falling entry (drop pods). Temporary — restores previous locomotor on landing.
    DropPod,
    /// True aircraft (Harrier, Kirov). Dedicated altitude state machine.
    Fly,
    /// Chrono movement (instant relocation). Often a temporary override.
    Teleport,
    /// Walker vehicle (e.g., Mammoth Mk. II). Drive-like with wobble/gait quirks.
    Mech,
    /// Naval vessel. Drive-like but carries naval identity for AI recognition.
    Ship,
    /// Jumpjet hover-flight (Rocketeer). Altitude-holding state machine, NOT Fly.
    Jumpjet,
    /// Spawned missile (V3, Dreadnought). Scripted missile controller.
    Rocket,
}

/// Well-known RA2/YR COM CLSIDs for each locomotor class.
/// Format in rules.ini: `Locomotor={CLSID-GUID}`
const CLSID_DRIVE: &str = "4A582741-9839-11D1-B709-00A024DDAFD1";
const CLSID_HOVER: &str = "4A582742-9839-11D1-B709-00A024DDAFD1";
const CLSID_TUNNEL: &str = "4A582743-9839-11D1-B709-00A024DDAFD1";
const CLSID_WALK: &str = "4A582744-9839-11D1-B709-00A024DDAFD1";
const CLSID_DROPPOD: &str = "4A582745-9839-11D1-B709-00A024DDAFD1";
const CLSID_FLY: &str = "4A582746-9839-11D1-B709-00A024DDAFD1";
const CLSID_TELEPORT: &str = "4A582747-9839-11D1-B709-00A024DDAFD1";
const CLSID_MECH: &str = "55D141B8-DB94-11D1-AC98-006008055BB5";
const CLSID_SHIP: &str = "2BEA74E1-7CCA-11D3-BE14-00104B62A16C";
const CLSID_JUMPJET: &str = "92612C46-F71F-11D1-AC9F-006008055BB5";
const CLSID_ROCKET: &str = "B7B49766-E576-11D3-9BD9-00104B972FE8";

impl LocomotorKind {
    /// Parse a LocomotorKind from an RA2/YR COM CLSID string.
    ///
    /// The input may include curly braces (e.g., `{4A582741-...}`).
    /// Unrecognized CLSIDs default to `Teleport`, matching the original engine's
    /// fallback behavior where an invalid locomotor produces Teleport movement.
    pub fn from_clsid(clsid: &str) -> Self {
        // Strip curly braces and whitespace, uppercase for comparison.
        let normalized: String = clsid
            .trim()
            .trim_start_matches('{')
            .trim_end_matches('}')
            .to_ascii_uppercase();

        match normalized.as_str() {
            CLSID_DRIVE => Self::Drive,
            CLSID_HOVER => Self::Hover,
            CLSID_TUNNEL => Self::Tunnel,
            CLSID_WALK => Self::Walk,
            CLSID_DROPPOD => Self::DropPod,
            CLSID_FLY => Self::Fly,
            CLSID_TELEPORT => Self::Teleport,
            CLSID_MECH => Self::Mech,
            CLSID_SHIP => Self::Ship,
            CLSID_JUMPJET => Self::Jumpjet,
            CLSID_ROCKET => Self::Rocket,
            _ => {
                log::warn!(
                    "Unknown locomotor CLSID '{}', defaulting to Teleport",
                    clsid
                );
                Self::Teleport
            }
        }
    }

    /// Default locomotor for a given object category when Locomotor= is absent.
    pub fn default_for_category(category: ObjectCategory) -> Self {
        match category {
            ObjectCategory::Infantry => Self::Walk,
            ObjectCategory::Vehicle => Self::Drive,
            ObjectCategory::Aircraft => Self::Fly,
            ObjectCategory::Building => Self::Drive, // immobile, but safe default
        }
    }
}

// ---------------------------------------------------------------------------
// SpeedType
// ---------------------------------------------------------------------------

/// Determines which terrain cells are actually traversable for a unit.
///
/// Parsed from rules.ini `SpeedType=` key. Controls terrain legality in the
/// pathfinder — a cell is only enterable if the SpeedType allows it.
///
/// Variant order matches the binary enum table at 0x81DA58 in gamemd.exe.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum SpeedType {
    /// Infantry default. Can traverse most land terrain.
    Foot,
    /// Most vehicles. Cannot cross water, limited on rough terrain.
    Track,
    /// Wheeled vehicles. Slower on rough terrain than Track.
    Wheel,
    /// Jumpjet hover movement type.
    Hover,
    /// Aircraft. Ignores terrain entirely.
    Winged,
    /// Hover units. Can cross water and land.
    Float,
    /// Amphibious units. Can traverse both land and water.
    Amphibious,
    /// Hover that can go on beaches (specific to certain hover units).
    FloatBeach,
}

impl Default for SpeedType {
    fn default() -> Self {
        Self::Track
    }
}

impl SpeedType {
    /// All SpeedTypes that have terrain cost grids (excludes Winged which ignores terrain).
    /// Order matches the binary enum table.
    pub const ALL_WITH_COSTS: &[SpeedType] = &[
        SpeedType::Foot,
        SpeedType::Track,
        SpeedType::Wheel,
        SpeedType::Hover,
        SpeedType::Float,
        SpeedType::Amphibious,
        SpeedType::FloatBeach,
    ];

    /// Parse from a rules.ini SpeedType= value string (case-insensitive).
    pub fn from_ini(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "foot" => Self::Foot,
            "track" => Self::Track,
            "wheel" => Self::Wheel,
            "float" => Self::Float,
            "amphibious" => Self::Amphibious,
            "winged" => Self::Winged,
            "floatbeach" => Self::FloatBeach,
            "hover" => Self::Hover,
            _ => {
                log::warn!("Unknown SpeedType '{}', defaulting to Track", value);
                Self::Track
            }
        }
    }

    /// Human-readable name for debug display.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Foot => "Foot",
            Self::Track => "Track",
            Self::Wheel => "Wheel",
            Self::Float => "Float",
            Self::Amphibious => "Amphibious",
            Self::Winged => "Winged",
            Self::FloatBeach => "FloatBeach",
            Self::Hover => "Hover",
        }
    }

    /// Next SpeedType in `ALL_WITH_COSTS`, wrapping around.
    pub fn cycle_next(&self) -> SpeedType {
        let list = Self::ALL_WITH_COSTS;
        let idx = list.iter().position(|s| s == self).unwrap_or(0);
        list[(idx + 1) % list.len()]
    }

    /// Previous SpeedType in `ALL_WITH_COSTS`, wrapping around.
    pub fn cycle_prev(&self) -> SpeedType {
        let list = Self::ALL_WITH_COSTS;
        let idx = list.iter().position(|s| s == self).unwrap_or(0);
        list[(idx + list.len() - 1) % list.len()]
    }
}

// ---------------------------------------------------------------------------
// MovementZone
// ---------------------------------------------------------------------------

/// Determines path search behavior and special routing logic.
///
/// Parsed from rules.ini `MovementZone=` key. Controls what kind of route
/// the pathfinder plans — distinct from SpeedType which controls terrain legality.
///
/// The numeric value IS the passability-matrix row index used by the original
/// pathfinding code. Recent RE shows these rows are keyed by derived
/// `MovementClass8`, not directly by our terrain `LandType` buckets.
///
/// Example: `MovementZone=Subterranean` enables dig-in/dig-out cell search
/// logic that plain Drive does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum MovementZone {
    /// Row 0: only movement class 0 is passable.
    Normal = 0,
    /// Row 1: classes 0 and 1 are passable.
    Crusher = 1,
    /// Row 2: classes 0, 1, and 2 are passable.
    Destroyer = 2,
    /// Row 3: classes 0, 1, 2, 3, 4, and 5 are passable.
    AmphibiousDestroyer = 3,
    /// Row 4: classes 0, 1, 3, and 4 are passable.
    AmphibiousCrusher = 4,
    /// Row 5: classes 0, 3, and 4 are passable.
    Amphibious = 5,
    /// Row 6: classes 0, 1, 2, and 6 are passable.
    Subterranean = 6,
    /// Row 7: classes 0 and 5 are passable.
    Infantry = 7,
    /// Row 8: classes 0, 1, 2, and 5 are passable.
    InfantryDestroyer = 8,
    /// Row 9: classes 0 through 6 are passable.
    Fly = 9,
    /// Row 10: only class 4 is passable.
    Water = 10,
    /// Row 11: classes 3 and 4 are passable.
    WaterBeach = 11,
    /// Row 12: classes 0, 1, and 2 are passable.
    CrusherAll = 12,
}

impl Default for MovementZone {
    fn default() -> Self {
        Self::Normal
    }
}

impl MovementZone {
    /// Parse from a rules.ini MovementZone= value string (case-insensitive).
    pub fn from_ini(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "normal" => Self::Normal,
            "crusher" => Self::Crusher,
            "destroyer" => Self::Destroyer,
            "amphibiousdestroyer" => Self::AmphibiousDestroyer,
            "amphibiouscrusher" => Self::AmphibiousCrusher,
            "amphibious" => Self::Amphibious,
            "subterranean" | "subterrannean" => Self::Subterranean,
            "infantry" => Self::Infantry,
            "infantrydestroyer" => Self::InfantryDestroyer,
            "fly" => Self::Fly,
            "water" => Self::Water,
            "waterbeach" => Self::WaterBeach,
            "crusherall" => Self::CrusherAll,
            _ => {
                log::warn!("Unknown MovementZone '{}', defaulting to Normal", value);
                Self::Normal
            }
        }
    }

    /// Water movers bypass the land PathGrid and use the passability matrix
    /// directly. Single source of truth for pathfinding, movement stepping,
    /// target redirect, and wake effects.
    pub fn is_water_mover(&self) -> bool {
        matches!(self, Self::Water | Self::WaterBeach)
    }

    /// All MovementZone variants that need computed zone grids.
    /// Fly is excluded — airborne units trivially reach everywhere.
    pub fn all_ground() -> &'static [MovementZone] {
        &[
            MovementZone::Normal,
            MovementZone::Crusher,
            MovementZone::Destroyer,
            MovementZone::AmphibiousDestroyer,
            MovementZone::AmphibiousCrusher,
            MovementZone::Amphibious,
            MovementZone::Subterranean,
            MovementZone::Infantry,
            MovementZone::InfantryDestroyer,
            MovementZone::Water,
            MovementZone::WaterBeach,
            MovementZone::CrusherAll,
        ]
    }

    /// Which SpeedType governs terrain cost for this movement zone.
    /// Controls how fast a unit moves on passable cells (not which cells are passable).
    pub fn speed_type(&self) -> SpeedType {
        match self {
            MovementZone::Normal
            | MovementZone::Crusher
            | MovementZone::Destroyer
            | MovementZone::CrusherAll
            | MovementZone::Subterranean => SpeedType::Track,
            MovementZone::AmphibiousCrusher
            | MovementZone::AmphibiousDestroyer
            | MovementZone::Amphibious => SpeedType::Amphibious,
            MovementZone::Infantry
            | MovementZone::InfantryDestroyer => SpeedType::Foot,
            MovementZone::Water => SpeedType::Float,
            MovementZone::WaterBeach => SpeedType::FloatBeach,
            MovementZone::Fly => SpeedType::Winged,
        }
    }

    /// Whether this MovementZone can traverse bridges (ground-capable).
    pub fn can_use_bridges(&self) -> bool {
        !matches!(self, MovementZone::Water | MovementZone::WaterBeach | MovementZone::Fly)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clsid_drive() {
        let kind = LocomotorKind::from_clsid("{4A582741-9839-11d1-B709-00A024DDAFD1}");
        assert_eq!(kind, LocomotorKind::Drive);
    }

    #[test]
    fn test_clsid_all_known() {
        // All 11 standard RA2/YR locomotor CLSIDs.
        let cases: Vec<(&str, LocomotorKind)> = vec![
            (
                "{4A582741-9839-11d1-B709-00A024DDAFD1}",
                LocomotorKind::Drive,
            ),
            (
                "{4A582742-9839-11d1-B709-00A024DDAFD1}",
                LocomotorKind::Hover,
            ),
            (
                "{4A582743-9839-11d1-B709-00A024DDAFD1}",
                LocomotorKind::Tunnel,
            ),
            (
                "{4A582744-9839-11d1-B709-00A024DDAFD1}",
                LocomotorKind::Walk,
            ),
            (
                "{4A582745-9839-11d1-B709-00A024DDAFD1}",
                LocomotorKind::DropPod,
            ),
            ("{4A582746-9839-11d1-B709-00A024DDAFD1}", LocomotorKind::Fly),
            (
                "{4A582747-9839-11d1-B709-00A024DDAFD1}",
                LocomotorKind::Teleport,
            ),
            (
                "{55D141B8-DB94-11d1-AC98-006008055BB5}",
                LocomotorKind::Mech,
            ),
            (
                "{2BEA74E1-7CCA-11d3-BE14-00104B62A16C}",
                LocomotorKind::Ship,
            ),
            (
                "{92612C46-F71F-11d1-AC9F-006008055BB5}",
                LocomotorKind::Jumpjet,
            ),
            (
                "{B7B49766-E576-11d3-9BD9-00104B972FE8}",
                LocomotorKind::Rocket,
            ),
        ];
        for (clsid, expected) in cases {
            assert_eq!(
                LocomotorKind::from_clsid(clsid),
                expected,
                "CLSID: {}",
                clsid
            );
        }
    }

    #[test]
    fn test_clsid_unknown_defaults_to_teleport() {
        let kind = LocomotorKind::from_clsid("{00000000-0000-0000-0000-000000000000}");
        assert_eq!(kind, LocomotorKind::Teleport);
    }

    #[test]
    fn test_clsid_no_braces() {
        let kind = LocomotorKind::from_clsid("4A582744-9839-11D1-B709-00A024DDAFD1");
        assert_eq!(kind, LocomotorKind::Walk);
    }

    #[test]
    fn test_default_for_category() {
        assert_eq!(
            LocomotorKind::default_for_category(ObjectCategory::Infantry),
            LocomotorKind::Walk
        );
        assert_eq!(
            LocomotorKind::default_for_category(ObjectCategory::Vehicle),
            LocomotorKind::Drive
        );
        assert_eq!(
            LocomotorKind::default_for_category(ObjectCategory::Aircraft),
            LocomotorKind::Fly
        );
        assert_eq!(
            LocomotorKind::default_for_category(ObjectCategory::Building),
            LocomotorKind::Drive
        );
    }

    #[test]
    fn test_speed_type_from_ini() {
        assert_eq!(SpeedType::from_ini("Foot"), SpeedType::Foot);
        assert_eq!(SpeedType::from_ini("Track"), SpeedType::Track);
        assert_eq!(SpeedType::from_ini("wheel"), SpeedType::Wheel);
        assert_eq!(SpeedType::from_ini("FLOAT"), SpeedType::Float);
        assert_eq!(SpeedType::from_ini("Amphibious"), SpeedType::Amphibious);
        assert_eq!(SpeedType::from_ini("Winged"), SpeedType::Winged);
        assert_eq!(SpeedType::from_ini("FloatBeach"), SpeedType::FloatBeach);
        assert_eq!(SpeedType::from_ini("Hover"), SpeedType::Hover);
    }

    #[test]
    fn test_speed_type_unknown_defaults_to_track() {
        assert_eq!(SpeedType::from_ini("bogus"), SpeedType::Track);
    }

    #[test]
    fn test_movement_zone_from_ini() {
        assert_eq!(MovementZone::from_ini("Normal"), MovementZone::Normal);
        assert_eq!(MovementZone::from_ini("crusher"), MovementZone::Crusher);
        assert_eq!(MovementZone::from_ini("DESTROYER"), MovementZone::Destroyer);
        assert_eq!(
            MovementZone::from_ini("AmphibiousCrusher"),
            MovementZone::AmphibiousCrusher
        );
        assert_eq!(
            MovementZone::from_ini("AmphibiousDestroyer"),
            MovementZone::AmphibiousDestroyer
        );
        assert_eq!(MovementZone::from_ini("Infantry"), MovementZone::Infantry);
        assert_eq!(
            MovementZone::from_ini("InfantryDestroyer"),
            MovementZone::InfantryDestroyer
        );
        assert_eq!(MovementZone::from_ini("Fly"), MovementZone::Fly);
        assert_eq!(
            MovementZone::from_ini("Subterranean"),
            MovementZone::Subterranean
        );
        // Legacy misspelling still works
        assert_eq!(
            MovementZone::from_ini("Subterrannean"),
            MovementZone::Subterranean
        );
        assert_eq!(
            MovementZone::from_ini("Amphibious"),
            MovementZone::Amphibious
        );
        assert_eq!(MovementZone::from_ini("Water"), MovementZone::Water);
        assert_eq!(
            MovementZone::from_ini("WaterBeach"),
            MovementZone::WaterBeach
        );
        assert_eq!(
            MovementZone::from_ini("CrusherAll"),
            MovementZone::CrusherAll
        );
    }

    #[test]
    fn test_movement_zone_unknown_defaults_to_normal() {
        assert_eq!(MovementZone::from_ini("invalid"), MovementZone::Normal);
    }
}
