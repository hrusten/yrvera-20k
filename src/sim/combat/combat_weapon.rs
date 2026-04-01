//! Weapon selection logic for the combat system.
//!
//! Determines which weapon (Primary or Secondary) a unit should use against
//! a given target. Selection is based on:
//! 1. Projectile targeting flags (AA/AG) — can the projectile reach this target type?
//! 2. Warhead Verses value — is damage > 0% against this armor type?
//!
//! If the Primary weapon fails either check, the Secondary is tried. If both
//! fail, the unit cannot engage the target at all.
//!
//! ## Verses behavioral thresholds
//! - **0%**: Weapon completely blocked — cannot target even with force-fire.
//! - **1%**: No passive acquire, no retaliation. Force-fire still works at 1% damage.
//! - **>1%**: Normal engagement.
//!
//! ## Dependency rules
//! - Part of sim/ — depends on rules/ (RuleSet, ObjectType, WeaponType, etc.)
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use super::armor_index;
use crate::map::entities::EntityCategory;
use crate::rules::object_type::ObjectType;
use crate::rules::ruleset::RuleSet;
use crate::rules::warhead_type::WarheadType;
use crate::rules::weapon_type::WeaponType;

/// Which weapon slot the unit is using for this engagement.
///
/// Used to resolve the correct FLH (firing offset) from art.ini:
/// Primary → `PrimaryFireFLH`, Secondary → `SecondaryFireFLH`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WeaponSlot {
    Primary,
    Secondary,
}

/// Result of weapon selection: the chosen weapon, its warhead, and the
/// effective Verses percentage against the target's armor.
pub(crate) struct SelectedWeapon<'a> {
    pub weapon: &'a WeaponType,
    pub warhead: &'a WarheadType,
    /// Damage percentage for target armor (0–200). Already looked up from Verses.
    /// 100 = full damage, 0 = immune.
    pub verses_pct: u8,
    /// Which weapon slot (Primary or Secondary) was selected.
    pub slot: WeaponSlot,
}

/// Behavioral gate derived from the Verses damage percentage.
/// Controls whether a weapon can passively acquire or retaliate against a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VersesGate {
    /// 0% — weapon cannot target this armor type at all, even force-fire.
    Blocked,
    /// 1% — no passive acquire, no retaliation. Force-fire works at 1% damage.
    Suppressed,
    /// >1% — normal engagement allowed.
    Normal,
}

/// Classify a Verses percentage into its behavioral gate.
///
/// RA2 uses these thresholds to control targeting:
/// - 0 blocks the weapon entirely (falls back to Secondary).
/// - 1 (1%) suppresses auto-targeting but allows force-fire.
/// - >1 is normal combat.
pub(crate) fn verses_gate(verses_pct: u8) -> VersesGate {
    match verses_pct {
        0 => VersesGate::Blocked,
        1 => VersesGate::Suppressed,
        _ => VersesGate::Normal,
    }
}

/// Select the best weapon (Primary or Secondary) for an attacker against
/// a specific target. Returns None if no weapon can engage.
///
/// Selection logic:
/// 1. If the attacker has an IFV weapon override (`ifv_weapon_index`), use
///    the corresponding weapon from `weapon_list[]` instead of Primary/Secondary.
/// 2. Try Primary — check projectile AA/AG flags match target category,
///    AND warhead Verses > 0% for target armor.
/// 3. If Primary fails, try Secondary with same checks.
/// 4. If both fail, return None (unit cannot engage this target).
#[allow(dead_code)] // Used by tests; non-IFV callers use this simpler API.
pub(crate) fn select_weapon<'a>(
    rules: &'a RuleSet,
    attacker_obj: &ObjectType,
    target_category: EntityCategory,
    target_armor: &str,
) -> Option<SelectedWeapon<'a>> {
    select_weapon_with_ifv(rules, attacker_obj, target_category, target_armor, None)
}

/// Like `select_weapon` but also considers IFV weapon override index.
pub(crate) fn select_weapon_with_ifv<'a>(
    rules: &'a RuleSet,
    attacker_obj: &ObjectType,
    target_category: EntityCategory,
    target_armor: &str,
    ifv_weapon_index: Option<u32>,
) -> Option<SelectedWeapon<'a>> {
    // IFV weapon override: Gunner=yes transport with a passenger selects
    // a specific weapon from weapon_list[] based on the passenger's IFVMode.
    if let Some(idx) = ifv_weapon_index {
        if let Some(weapon_id) = attacker_obj.weapon_list.get(idx as usize) {
            if let Some(result) = try_weapon(
                rules,
                weapon_id,
                target_category,
                target_armor,
                WeaponSlot::Primary,
            ) {
                return Some(result);
            }
        }
        // Fallback to default Primary/Secondary if IFV weapon fails.
    }

    // Try Primary weapon first.
    if let Some(ref weapon_id) = attacker_obj.primary {
        if let Some(result) = try_weapon(
            rules,
            weapon_id,
            target_category,
            target_armor,
            WeaponSlot::Primary,
        ) {
            return Some(result);
        }
    }
    // Primary failed or doesn't exist — try Secondary.
    if let Some(ref weapon_id) = attacker_obj.secondary {
        if let Some(result) = try_weapon(
            rules,
            weapon_id,
            target_category,
            target_armor,
            WeaponSlot::Secondary,
        ) {
            return Some(result);
        }
    }
    None
}

/// Select the weapon used by a garrisoned occupant firing from a building.
///
/// Priority chain (matching gamemd `BuildingClass::GetWeapon` 0x004526F0):
/// 1. Elite occupant → `EliteOccupyWeapon` (fall back to `OccupyWeapon`)
/// 2. Normal occupant → `OccupyWeapon`
/// 3. Fallback → occupant's Primary weapon
///
/// Returns None if no weapon can engage the target type.
pub(crate) fn select_garrison_weapon<'a>(
    rules: &'a RuleSet,
    occupant_type_ref: &str,
    occupant_veterancy: u16,
    target_category: EntityCategory,
    target_armor: &str,
) -> Option<SelectedWeapon<'a>> {
    let occupant_obj = rules.object(occupant_type_ref)?;
    let is_elite = occupant_veterancy >= 200;

    // Try elite/normal OccupyWeapon.
    let occupy_weapon_id = if is_elite {
        occupant_obj
            .elite_occupy_weapon
            .as_deref()
            .or(occupant_obj.occupy_weapon.as_deref())
    } else {
        occupant_obj.occupy_weapon.as_deref()
    };

    if let Some(wid) = occupy_weapon_id {
        if let Some(sw) = try_weapon(
            rules,
            wid,
            target_category,
            target_armor,
            WeaponSlot::Primary,
        ) {
            return Some(sw);
        }
    }

    // Fallback: occupant's primary weapon.
    if let Some(ref primary) = occupant_obj.primary {
        return try_weapon(
            rules,
            primary,
            target_category,
            target_armor,
            WeaponSlot::Primary,
        );
    }
    None
}

/// Try a single weapon against a target. Returns Some if the weapon can engage.
pub(crate) fn try_weapon<'a>(
    rules: &'a RuleSet,
    weapon_id: &str,
    target_category: EntityCategory,
    target_armor: &str,
    slot: WeaponSlot,
) -> Option<SelectedWeapon<'a>> {
    let weapon: &WeaponType = rules.weapon(weapon_id)?;

    // Check projectile targeting flags (AA/AG) against target category.
    if !can_projectile_engage(rules, weapon, target_category) {
        return None;
    }

    // Check warhead Verses against target armor — 0% blocks entirely.
    let warhead: &WarheadType = weapon.warhead.as_ref().and_then(|id| rules.warhead(id))?;
    let idx: usize = armor_index(target_armor);
    let verses_pct: u8 = warhead.verses.get(idx).copied().unwrap_or(100);

    if verses_gate(verses_pct) == VersesGate::Blocked {
        return None;
    }

    Some(SelectedWeapon {
        weapon,
        warhead,
        verses_pct,
        slot,
    })
}

/// Check whether a weapon's projectile can target the given entity category.
///
/// Aircraft require AA=yes on the projectile. Ground units, infantry, and
/// buildings require AG=yes (which defaults to true for most projectiles).
/// If the weapon has no projectile defined, we assume it can hit ground only.
fn can_projectile_engage(
    rules: &RuleSet,
    weapon: &WeaponType,
    target_category: EntityCategory,
) -> bool {
    let proj = weapon
        .projectile
        .as_ref()
        .and_then(|id| rules.projectile(id));

    match target_category {
        EntityCategory::Aircraft => proj.is_some_and(|p| p.aa),
        // Ground units, infantry, buildings all need AG.
        EntityCategory::Unit | EntityCategory::Infantry | EntityCategory::Structure => {
            proj.is_none_or(|p| p.ag)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::ini_parser::IniFile;

    /// Build a test RuleSet with a dual-weapon unit (AG primary, AA secondary).
    fn make_dual_weapon_rules() -> RuleSet {
        let ini_str: &str = "\
[InfantryTypes]
[VehicleTypes]
0=IFV
[AircraftTypes]
[BuildingTypes]

[IFV]
Name=IFV
Cost=600
Strength=200
Armor=light
Speed=8
Primary=Missiles
Secondary=FlakGun

[Missiles]
Damage=50
ROF=40
Range=6
Projectile=MissileGround
Warhead=HE

[FlakGun]
Damage=30
ROF=20
Range=8
Projectile=FlakProj
Warhead=Flak

[MissileGround]
AG=yes
AA=no

[FlakProj]
AG=no
AA=yes

[HE]
Verses=100%,100%,100%,80%,60%,40%,100%,40%,20%,0%,0%

[Flak]
Verses=100%,100%,100%,80%,60%,40%,100%,40%,20%,0%,0%
";
        let ini: IniFile = IniFile::from_str(ini_str);
        RuleSet::from_ini(&ini).expect("Should parse test rules")
    }

    #[test]
    fn test_primary_selected_for_ground() {
        let rules: RuleSet = make_dual_weapon_rules();
        let ifv = rules.object("IFV").unwrap();

        let result = select_weapon(&rules, ifv, EntityCategory::Unit, "light");
        assert!(result.is_some());
        let selected = result.unwrap();
        assert_eq!(selected.weapon.id, "Missiles");
        assert_eq!(selected.slot, WeaponSlot::Primary);
    }

    #[test]
    fn test_secondary_selected_for_aircraft() {
        let rules: RuleSet = make_dual_weapon_rules();
        let ifv = rules.object("IFV").unwrap();

        let result = select_weapon(&rules, ifv, EntityCategory::Aircraft, "light");
        assert!(result.is_some());
        let selected = result.unwrap();
        assert_eq!(selected.weapon.id, "FlakGun");
        assert_eq!(selected.slot, WeaponSlot::Secondary);
    }

    #[test]
    fn test_zero_verses_blocks_and_falls_back() {
        // special_1 armor (index 9) has 0% in both HE and Flak warheads.
        let rules: RuleSet = make_dual_weapon_rules();
        let ifv = rules.object("IFV").unwrap();

        let result = select_weapon(&rules, ifv, EntityCategory::Unit, "special_1");
        assert!(result.is_none(), "Both weapons have 0% vs special_1");
    }

    #[test]
    fn test_verses_gate_thresholds() {
        assert_eq!(verses_gate(0), VersesGate::Blocked);
        assert_eq!(verses_gate(1), VersesGate::Suppressed);
        assert_eq!(verses_gate(2), VersesGate::Normal);
        assert_eq!(verses_gate(100), VersesGate::Normal);
        assert_eq!(verses_gate(200), VersesGate::Normal);
    }

    #[test]
    fn test_no_weapons_returns_none() {
        let ini_str: &str = "\
[InfantryTypes]
[VehicleTypes]
0=CIV
[AircraftTypes]
[BuildingTypes]

[CIV]
Name=Civilian
Cost=0
Strength=50
Armor=none
";
        let ini: IniFile = IniFile::from_str(ini_str);
        let rules: RuleSet = RuleSet::from_ini(&ini).expect("parse");
        let civ = rules.object("CIV").unwrap();

        let result = select_weapon(&rules, civ, EntityCategory::Unit, "none");
        assert!(result.is_none());
    }
}
