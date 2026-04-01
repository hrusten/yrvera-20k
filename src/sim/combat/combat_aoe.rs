//! Area-of-effect (AoE) damage logic for warheads with CellSpread > 0.
//!
//! When a warhead detonates with CellSpread > 0, it damages all entities
//! within the blast radius. Damage falls off linearly from 100% at the
//! epicenter to `PercentAtMax` at the edge of the radius.
//!
//! ## Damage formula
//! ```text
//! damage_at_distance(d) = base_damage * verses[armor] * lerp(1.0, percent_at_max, d / cell_spread)
//! ```
//!
//! ## Dependency rules
//! - Part of sim/ — depends on rules/ (RuleSet, WarheadType) and sim/components.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use super::{armor_index, lepton_distance_sq_raw};
use crate::rules::ruleset::RuleSet;
use crate::rules::warhead_type::WarheadType;
use crate::sim::entity_store::EntityStore;
use crate::sim::intern::StringInterner;
use crate::util::fixed_math::{SIM_ZERO, SimFixed, isqrt_i64};
use crate::util::lepton::CELL_CENTER_LEPTON;

/// Apply area-of-effect damage from a warhead detonation at a specific cell.
///
/// Returns a list of (stable_id, damage) pairs for all entities within the blast
/// radius. Friendly fire IS applied — CellSpread does not discriminate by owner,
/// matching RA2 behavior (e.g., V3 rockets can damage your own units).
///
/// `base_damage` is the weapon's raw damage value (before Verses scaling).
pub(crate) fn apply_aoe_damage(
    entities: &EntityStore,
    impact_rx: u16,
    impact_ry: u16,
    base_damage: i32,
    warhead: &WarheadType,
    rules: &RuleSet,
    interner: &StringInterner,
    _attacker_owner: &str,
) -> Vec<(u64, u16)> {
    let cell_spread: SimFixed = warhead.cell_spread;
    if cell_spread <= SIM_ZERO {
        return Vec::new();
    }

    // Pre-compute squared radius in lepton space (i64) for quick rejection.
    let spread_leptons: i64 = cell_spread.to_num::<i64>() * 256;
    let spread_sq: i64 = spread_leptons * spread_leptons;
    let mut damage_list: Vec<(u64, u16)> = Vec::new();

    for entity in entities.values() {
        if entity.health.current == 0 {
            continue;
        }

        // Impact point detonates at cell center (sub = 128,128).
        let dist_sq_leptons: i64 = lepton_distance_sq_raw(
            impact_rx,
            impact_ry,
            CELL_CENTER_LEPTON,
            CELL_CENTER_LEPTON,
            entity.position.rx,
            entity.position.ry,
            entity.position.sub_x,
            entity.position.sub_y,
        );

        // Quick reject in lepton space.
        if dist_sq_leptons > spread_sq {
            continue;
        }

        // Convert lepton distance to cell distance for falloff formula.
        // sqrt(dist_sq_leptons) / 256 = distance in cells.
        // Uses integer sqrt to avoid platform-dependent f64 rounding.
        let dist_leptons: i64 = isqrt_i64(dist_sq_leptons);
        let distance: SimFixed = SimFixed::from_num(dist_leptons / 256);
        if distance > cell_spread {
            continue;
        }

        // Look up target armor for Verses scaling.
        let armor_str: &str = rules
            .object(interner.resolve(entity.type_ref))
            .map(|o| o.armor.as_str())
            .unwrap_or("none");
        let idx: usize = armor_index(armor_str);
        let verses_pct: u8 = warhead.verses.get(idx).copied().unwrap_or(100);

        let dmg: u16 = aoe_damage_at_distance(
            base_damage,
            distance,
            cell_spread,
            warhead.percent_at_max,
            verses_pct,
        );

        if dmg > 0 {
            damage_list.push((entity.stable_id, dmg));
        }
    }

    damage_list
}

/// Compute distance-scaled AoE damage using integer/fixed-point math.
///
/// At distance 0 (epicenter): full `base_damage * verses_pct / 100`.
/// At distance == cell_spread (edge): `base_damage * verses_pct * percent_at_max_pct / 10000`.
/// Linear interpolation between those extremes.
fn aoe_damage_at_distance(
    base_damage: i32,
    distance: SimFixed,
    cell_spread: SimFixed,
    percent_at_max_pct: u8,
    verses_pct: u8,
) -> u16 {
    // t = distance / cell_spread, clamped [0, 1] — how far from center (SimFixed).
    let t: SimFixed = if cell_spread > SIM_ZERO {
        (distance / cell_spread).clamp(SIM_ZERO, SimFixed::from_num(1))
    } else {
        SIM_ZERO
    };
    // falloff_pct = lerp(100, percent_at_max_pct, t) in integer.
    // = 100 + (percent_at_max_pct - 100) * t
    let pam: i32 = percent_at_max_pct as i32;
    let falloff_fixed: SimFixed = SimFixed::from_num(100) + SimFixed::from_num(pam - 100) * t;
    let falloff_pct: i32 = falloff_fixed.to_num::<i32>();

    // raw = base_damage * verses_pct * falloff_pct / 10000
    // Compute in i64 and clamp to i32 range to prevent silent narrowing overflow.
    let wide = base_damage as i64 * verses_pct as i64 * falloff_pct as i64 / 10000;
    let raw: i32 = wide.clamp(0, i32::MAX as i64) as i32;
    raw as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::fixed_math::sim_from_f32;

    #[test]
    fn test_aoe_damage_at_center() {
        // At distance 0, full damage: 100 * 100 * 100 / 10000 = 100.
        let dmg = aoe_damage_at_distance(100, SIM_ZERO, sim_from_f32(3.0), 25, 100);
        assert_eq!(dmg, 100);
    }

    #[test]
    fn test_aoe_damage_at_edge() {
        // At distance == cell_spread, damage = base * percent_at_max / 100 = 100 * 25 / 100 = 25.
        let dmg = aoe_damage_at_distance(100, sim_from_f32(3.0), sim_from_f32(3.0), 25, 100);
        assert_eq!(dmg, 25);
    }

    #[test]
    fn test_aoe_damage_at_midpoint() {
        // At half distance, falloff_pct = lerp(100, 25, 0.5) = 62.
        // damage = 100 * 100 * 62 / 10000 = 62.
        let dmg = aoe_damage_at_distance(100, sim_from_f32(1.5), sim_from_f32(3.0), 25, 100);
        assert_eq!(dmg, 62);
    }

    #[test]
    fn test_aoe_damage_with_verses() {
        // 50% verses at center: 100 * 50 * 100 / 10000 = 50.
        let dmg = aoe_damage_at_distance(100, SIM_ZERO, sim_from_f32(3.0), 25, 50);
        assert_eq!(dmg, 50);
    }

    #[test]
    fn test_aoe_damage_zero_verses() {
        let dmg = aoe_damage_at_distance(100, SIM_ZERO, sim_from_f32(3.0), 25, 0);
        assert_eq!(dmg, 0);
    }

    #[test]
    fn test_aoe_beyond_radius() {
        // Beyond radius clamped to t=1 → percent_at_max.
        let dmg = aoe_damage_at_distance(100, sim_from_f32(5.0), sim_from_f32(3.0), 25, 100);
        assert_eq!(dmg, 25);
    }
}
