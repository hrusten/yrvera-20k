//! Weapon type definitions parsed from rules.ini.
//!
//! Each weapon in RA2 has its own `[WeaponName]` section in rules.ini,
//! defining damage, range, rate of fire, and references to a projectile
//! type and warhead type. Units reference weapons via their `Primary=`
//! and `Secondary=` keys.
//!
//! ## rules.ini format
//! ```ini
//! [105mm]
//! Damage=65
//! ROF=50
//! Range=5.75
//! Speed=40
//! Projectile=InvisibleLow
//! Warhead=AP
//! ```
//!
//! ## Dependency rules
//! - Part of rules/ — no dependencies on sim/, render/, ui/, etc.

use crate::rules::ini_parser::IniSection;
use crate::util::fixed_math::{SIM_ZERO, SimFixed, sim_from_f32};

/// A weapon definition parsed from a rules.ini section.
///
/// Weapons are the bridge between units and damage. A unit fires a weapon,
/// which spawns a projectile carrying a warhead. The warhead determines
/// actual damage based on the target's armor type.
///
/// All 63 fields from the original WeaponTypeClass (verified from
/// decompilation of WeaponTypeClass::ReadINI at 0x772080).
#[derive(Debug, Clone)]
pub struct WeaponType {
    // ── Core fields ──────────────────────────────────────────────────
    /// Section name in rules.ini (e.g., "105mm", "Vulcan", "RedEye2").
    pub id: String,
    /// Base damage per hit.
    pub damage: i32,
    /// Maximum range in cells (fixed-point for deterministic range checks).
    pub range: SimFixed,
    /// Rate of fire: frames between consecutive shots (lower = faster).
    pub rof: i32,
    /// Projectile travel speed (0 = instant hit / hitscan).
    pub speed: i32,
    /// Projectile type ID (references a [ProjectileName] section).
    pub projectile: Option<String>,
    /// Warhead type ID (references a [WarheadName] section).
    pub warhead: Option<String>,
    /// Firing sound ID (references a [SoundName] section in sound.ini).
    /// Played each time this weapon fires.
    pub report: Option<String>,
    /// Number of rapid shots per attack cycle (default 1).
    /// After the full burst, the ROF cooldown begins.
    pub burst: i32,
    /// When true, firing this weapon clears shroud around the fire location.
    /// Used on some support powers and special weapons.
    pub reveal_on_fire: bool,

    // ── Int/fixed-point fields ───────────────────────────────────────
    /// Ambient damage dealt to units standing near the weapon's owner (+0x98).
    pub ambient_damage: i32,
    /// Minimum firing range in cells (+0xb8). Weapon won't fire if target
    /// is closer than this distance.
    pub minimum_range: SimFixed,
    /// Blink time for disguise fake when firing while disguised (+0x13c).
    pub disguise_fake_blink_time: i32,
    /// Duration of laser beam visual effect in frames (+0x14e).
    pub laser_duration: i32,
    /// Radiation level emitted on impact (+0x158).
    pub rad_level: i32,

    // ── String/reference fields ──────────────────────────────────────
    /// Sound played when weapon fires downward (e.g., from a building).
    pub down_report: Option<String>,
    /// Animation played during assault (garrison clearing).
    pub assault_anim: Option<String>,
    /// Animation played by occupants when firing from a building.
    pub occupant_anim: Option<String>,
    /// Animation played by units firing from an open-topped transport.
    pub open_topped_anim: Option<String>,
    /// Particle system spawned when this weapon fires.
    pub attached_particle_system: Option<String>,
    /// Comma-separated list of impact animations, indexed by damage magnitude.
    pub anim: Vec<String>,

    // ── Color fields (3 bytes each, "R,G,B" format) ──────────────────
    /// Inner color of laser beam (+0x120).
    pub laser_inner_color: [u8; 3],
    /// Outer color of laser beam (+0x123).
    pub laser_outer_color: [u8; 3],
    /// Outer spread/glow color of laser beam (+0x126).
    pub laser_outer_spread: [u8; 3],

    // ── Bool fields ──────────────────────────────────────────────────
    /// Weapon uses fire particle effects (+0x129).
    pub use_fire_particles: bool,
    /// Weapon uses spark particle effects (+0x12a).
    pub use_spark_particles: bool,
    /// Fires in all directions regardless of facing (+0x12b).
    pub omni_fire: bool,
    /// Distributes weapon fire across multiple targets (+0x12c).
    pub distributed_weapon_fire: bool,
    /// Weapon is a railgun (special projectile visual) (+0x12d).
    pub is_railgun: bool,
    /// Projectile follows a lobbed arc trajectory (+0x12e).
    pub lobber: bool,
    /// Weapon flash is extra bright (+0x12f).
    pub bright: bool,
    /// Weapon deals sonic/disruptor damage (+0x130).
    pub is_sonic: bool,
    /// Weapon spawns aircraft (like aircraft carriers) (+0x131).
    pub spawner: bool,
    /// Weapon can fire from limbo (e.g., paradrop weapons) (+0x132).
    pub limbo_launch: bool,
    /// Unit must decloak before firing this weapon (+0x133).
    pub decloak_to_fire: bool,
    /// Uses cell-center rangefinding instead of edge-to-edge (+0x134).
    pub cell_rangefinding: bool,
    /// Weapon fires only once then is consumed (+0x135).
    pub fire_once: bool,
    /// Weapon is never automatically selected for use (+0x136).
    pub never_use: bool,
    /// Weapon can fire at terrain/ground (+0x138).
    pub terrain_fire: bool,
    /// Uses the sabotage cursor when this weapon is active (+0x139).
    pub sabotage_cursor: bool,
    /// Uses the MiG attack cursor (+0x13a).
    pub mig_attack_cursor: bool,
    /// Can only fire while the unit is disguised (+0x13b).
    pub disguise_fire_only: bool,
    /// Mind control effect has no unit limit (+0x140).
    pub infinite_mind_control: bool,
    /// Unit can fire this weapon while moving (+0x141).
    pub fire_while_moving: bool,
    /// Weapon drains target's health to heal the firer (+0x142).
    pub drain_weapon: bool,
    /// Weapon can fire from inside a transport (+0x143).
    pub fire_in_transport: bool,
    /// Firing this weapon kills the attacker (+0x144).
    pub suicide: bool,
    /// Grants speed boost on hit (+0x145).
    pub turbo_boost: bool,
    /// Suppresses target (Westwood's misspelling preserved) (+0x146).
    pub supress: bool,
    /// Weapon spawns a camera/reveal at impact point (+0x147).
    pub camera: bool,
    /// Weapon has limited charges (+0x148).
    pub charges: bool,
    /// Weapon fires a laser beam visual (+0x149).
    pub is_laser: bool,
    /// Weapon fires a disk-shaped laser (Vortex) (+0x14a).
    pub disk_laser: bool,
    /// Weapon draws a visible line to target (+0x14b).
    pub is_line: bool,
    /// Weapon fires a thicker laser beam (+0x14c).
    pub is_big_laser: bool,
    /// Laser color matches house/player color (+0x14d).
    pub is_house_color: bool,
    /// Weapon is affected by ion storms (+0x14f).
    pub ion_sensitive: bool,
    /// Weapon fires at area/ground instead of specific target (+0x150).
    pub area_fire: bool,
    /// Weapon fires an electric bolt visual (Tesla) (+0x151).
    pub is_electric_bolt: bool,
    /// Draw electric bolt using laser rendering (+0x152).
    pub draw_bolt_as_laser: bool,
    /// Laser uses alternate (darker) color scheme (+0x153).
    pub is_alternate_color: bool,
    /// Weapon fires a radiation beam visual (+0x154).
    pub is_rad_beam: bool,
    /// Weapon triggers a radiation eruption effect (+0x155).
    pub is_rad_eruption: bool,
    /// Weapon fires a magnetron beam (+0x15c).
    pub is_mag_beam: bool,
}

impl WeaponType {
    /// Parse a WeaponType from a rules.ini section.
    pub fn from_ini_section(id: &str, section: &IniSection) -> Self {
        Self {
            id: id.to_string(),
            damage: section.get_i32("Damage").unwrap_or(0),
            range: section
                .get_f32("Range")
                .map(sim_from_f32)
                .unwrap_or(SIM_ZERO),
            rof: section.get_i32("ROF").unwrap_or(0),
            speed: section.get_i32("Speed").unwrap_or(0),
            projectile: section.get("Projectile").map(|s| s.to_string()),
            warhead: section.get("Warhead").map(|s| s.to_string()),
            report: section.get("Report").map(|s| s.to_string()),
            burst: section.get_i32("Burst").unwrap_or(1),
            reveal_on_fire: section.get_bool("RevealOnFire").unwrap_or(false),

            // Int/fixed-point fields
            ambient_damage: section.get_i32("AmbientDamage").unwrap_or(0),
            minimum_range: section
                .get_f32("MinimumRange")
                .map(sim_from_f32)
                .unwrap_or(SIM_ZERO),
            disguise_fake_blink_time: section.get_i32("DisguiseFakeBlinkTime").unwrap_or(0),
            laser_duration: section.get_i32("LaserDuration").unwrap_or(0),
            rad_level: section.get_i32("RadLevel").unwrap_or(0),

            // String/reference fields
            down_report: section.get("DownReport").map(|s| s.to_string()),
            assault_anim: section.get("AssaultAnim").map(|s| s.to_string()),
            occupant_anim: section.get("OccupantAnim").map(|s| s.to_string()),
            open_topped_anim: section.get("OpenToppedAnim").map(|s| s.to_string()),
            attached_particle_system: section.get("AttachedParticleSystem").map(|s| s.to_string()),
            anim: section
                .get_list("Anim")
                .unwrap_or_default()
                .into_iter()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect(),

            // Color fields
            laser_inner_color: section
                .get("LaserInnerColor")
                .map(parse_rgb_color)
                .unwrap_or([0, 0, 0]),
            laser_outer_color: section
                .get("LaserOuterColor")
                .map(parse_rgb_color)
                .unwrap_or([0, 0, 0]),
            laser_outer_spread: section
                .get("LaserOuterSpread")
                .map(parse_rgb_color)
                .unwrap_or([0, 0, 0]),

            // Bool fields
            use_fire_particles: section.get_bool("UseFireParticles").unwrap_or(false),
            use_spark_particles: section.get_bool("UseSparkParticles").unwrap_or(false),
            omni_fire: section.get_bool("OmniFire").unwrap_or(false),
            distributed_weapon_fire: section.get_bool("DistributedWeaponFire").unwrap_or(false),
            is_railgun: section.get_bool("IsRailgun").unwrap_or(false),
            lobber: section.get_bool("Lobber").unwrap_or(false),
            bright: section.get_bool("Bright").unwrap_or(false),
            is_sonic: section.get_bool("IsSonic").unwrap_or(false),
            spawner: section.get_bool("Spawner").unwrap_or(false),
            limbo_launch: section.get_bool("LimboLaunch").unwrap_or(false),
            decloak_to_fire: section.get_bool("DecloakToFire").unwrap_or(false),
            cell_rangefinding: section.get_bool("CellRangefinding").unwrap_or(false),
            fire_once: section.get_bool("FireOnce").unwrap_or(false),
            never_use: section.get_bool("NeverUse").unwrap_or(false),
            terrain_fire: section.get_bool("TerrainFire").unwrap_or(false),
            sabotage_cursor: section.get_bool("SabotageCursor").unwrap_or(false),
            mig_attack_cursor: section.get_bool("MigAttackCursor").unwrap_or(false),
            disguise_fire_only: section.get_bool("DisguiseFireOnly").unwrap_or(false),
            infinite_mind_control: section.get_bool("InfiniteMindControl").unwrap_or(false),
            fire_while_moving: section.get_bool("FireWhileMoving").unwrap_or(false),
            drain_weapon: section.get_bool("DrainWeapon").unwrap_or(false),
            fire_in_transport: section.get_bool("FireInTransport").unwrap_or(false),
            suicide: section.get_bool("Suicide").unwrap_or(false),
            turbo_boost: section.get_bool("TurboBoost").unwrap_or(false),
            supress: section.get_bool("Supress").unwrap_or(false),
            camera: section.get_bool("Camera").unwrap_or(false),
            charges: section.get_bool("Charges").unwrap_or(false),
            is_laser: section.get_bool("IsLaser").unwrap_or(false),
            disk_laser: section.get_bool("DiskLaser").unwrap_or(false),
            is_line: section.get_bool("IsLine").unwrap_or(false),
            is_big_laser: section.get_bool("IsBigLaser").unwrap_or(false),
            is_house_color: section.get_bool("IsHouseColor").unwrap_or(false),
            ion_sensitive: section.get_bool("IonSensitive").unwrap_or(false),
            area_fire: section.get_bool("AreaFire").unwrap_or(false),
            is_electric_bolt: section.get_bool("IsElectricBolt").unwrap_or(false),
            draw_bolt_as_laser: section.get_bool("DrawBoltAsLaser").unwrap_or(false),
            is_alternate_color: section.get_bool("IsAlternateColor").unwrap_or(false),
            is_rad_beam: section.get_bool("IsRadBeam").unwrap_or(false),
            is_rad_eruption: section.get_bool("IsRadEruption").unwrap_or(false),
            is_mag_beam: section.get_bool("IsMagBeam").unwrap_or(false),
        }
    }
}

/// Parse an "R,G,B" color string into a `[u8; 3]` array.
///
/// Each component is clamped to 0–255. Returns `[0, 0, 0]` if parsing fails.
/// Used for LaserInnerColor, LaserOuterColor, LaserOuterSpread.
fn parse_rgb_color(raw: &str) -> [u8; 3] {
    let parts: Vec<&str> = raw.split(',').map(|s| s.trim()).collect();
    if parts.len() >= 3 {
        let r = parts[0].parse::<u8>().unwrap_or(0);
        let g = parts[1].parse::<u8>().unwrap_or(0);
        let b = parts[2].parse::<u8>().unwrap_or(0);
        [r, g, b]
    } else {
        [0, 0, 0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::ini_parser::IniFile;

    #[test]
    fn test_parse_weapon() {
        let ini: IniFile = IniFile::from_str(
            "[105mm]\nDamage=65\nROF=50\nRange=5.75\nSpeed=40\n\
             Projectile=InvisibleLow\nWarhead=AP\n",
        );
        let section: &IniSection = ini.section("105mm").unwrap();
        let weapon: WeaponType = WeaponType::from_ini_section("105mm", section);

        assert_eq!(weapon.id, "105mm");
        assert_eq!(weapon.damage, 65);
        assert_eq!(weapon.range, sim_from_f32(5.75));
        assert_eq!(weapon.rof, 50);
        assert_eq!(weapon.speed, 40);
        assert_eq!(weapon.projectile, Some("InvisibleLow".to_string()));
        assert_eq!(weapon.warhead, Some("AP".to_string()));
    }

    #[test]
    fn test_weapon_defaults() {
        let ini: IniFile = IniFile::from_str("[Empty]\n");
        let section: &IniSection = ini.section("Empty").unwrap();
        let weapon: WeaponType = WeaponType::from_ini_section("Empty", section);

        assert_eq!(weapon.damage, 0);
        assert_eq!(weapon.range, SIM_ZERO);
        assert_eq!(weapon.rof, 0);
        assert_eq!(weapon.projectile, None);
        assert_eq!(weapon.warhead, None);
        // New fields should all default correctly
        assert_eq!(weapon.ambient_damage, 0);
        assert_eq!(weapon.minimum_range, SIM_ZERO);
        assert_eq!(weapon.laser_duration, 0);
        assert_eq!(weapon.rad_level, 0);
        assert!(!weapon.is_sonic);
        assert!(!weapon.is_laser);
        assert!(!weapon.is_electric_bolt);
        assert!(weapon.anim.is_empty());
        assert_eq!(weapon.laser_inner_color, [0, 0, 0]);
        assert_eq!(weapon.down_report, None);
    }

    #[test]
    fn test_parse_bool_fields() {
        let ini: IniFile = IniFile::from_str(
            "[TestWeapon]\nDamage=100\nIsSonic=yes\nSpawner=yes\n\
             FireOnce=yes\nIsLaser=yes\nIsElectricBolt=no\n\
             Lobber=true\nSuicide=yes\nAreaFire=yes\n",
        );
        let section: &IniSection = ini.section("TestWeapon").unwrap();
        let weapon: WeaponType = WeaponType::from_ini_section("TestWeapon", section);

        assert!(weapon.is_sonic);
        assert!(weapon.spawner);
        assert!(weapon.fire_once);
        assert!(weapon.is_laser);
        assert!(!weapon.is_electric_bolt);
        assert!(weapon.lobber);
        assert!(weapon.suicide);
        assert!(weapon.area_fire);
        // Unset bools remain false
        assert!(!weapon.bright);
        assert!(!weapon.camera);
    }

    #[test]
    fn test_parse_laser_colors() {
        let ini: IniFile = IniFile::from_str(
            "[PrismWeapon]\nDamage=100\nIsLaser=yes\n\
             LaserInnerColor=255,0,0\nLaserOuterColor=128,64,32\n\
             LaserOuterSpread=200,200,200\n",
        );
        let section: &IniSection = ini.section("PrismWeapon").unwrap();
        let weapon: WeaponType = WeaponType::from_ini_section("PrismWeapon", section);

        assert_eq!(weapon.laser_inner_color, [255, 0, 0]);
        assert_eq!(weapon.laser_outer_color, [128, 64, 32]);
        assert_eq!(weapon.laser_outer_spread, [200, 200, 200]);
    }

    #[test]
    fn test_parse_rgb_color_helper() {
        assert_eq!(parse_rgb_color("255,128,0"), [255, 128, 0]);
        assert_eq!(parse_rgb_color("0, 0, 0"), [0, 0, 0]);
        assert_eq!(parse_rgb_color("invalid"), [0, 0, 0]);
        assert_eq!(parse_rgb_color(""), [0, 0, 0]);
    }

    #[test]
    fn test_parse_anim_list() {
        let ini: IniFile =
            IniFile::from_str("[TestWeapon]\nDamage=50\nAnim=YOURFIRE,YOUREXPL,YOURBOOM\n");
        let section: &IniSection = ini.section("TestWeapon").unwrap();
        let weapon: WeaponType = WeaponType::from_ini_section("TestWeapon", section);

        assert_eq!(weapon.anim.len(), 3);
        assert_eq!(weapon.anim[0], "YOURFIRE");
        assert_eq!(weapon.anim[1], "YOUREXPL");
        assert_eq!(weapon.anim[2], "YOURBOOM");
    }

    #[test]
    fn test_parse_minimum_range() {
        let ini: IniFile = IniFile::from_str("[TestWeapon]\nDamage=50\nMinimumRange=3.5\n");
        let section: &IniSection = ini.section("TestWeapon").unwrap();
        let weapon: WeaponType = WeaponType::from_ini_section("TestWeapon", section);

        assert_eq!(weapon.minimum_range, sim_from_f32(3.5));
    }
}
