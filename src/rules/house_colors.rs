//! House color definitions — palette ramps for player team colors.
//!
//! RA2 reserves palette indices 16–31 for "house colors" — 16 shades that get
//! swapped per player to distinguish units visually. Allied units appear blue,
//! Soviet units appear red, etc.
//!
//! Each color scheme is a 16-entry gradient from lightest (index 0) to darkest
//! (index 15). When rendering a unit, the base palette's indices 16–31 are
//! replaced with the owning player's color scheme before pixel conversion.
//!
//! ## Standard RA2 Schemes
//! Gold (default/neutral), DarkBlue (Allied), DarkRed (Soviet), Green,
//! Orange, Purple, LightBlue, Brown.
//!
//! ## Dependency rules
//! - Part of rules/ — depends only on assets/pal_file (Color type).

use crate::assets::pal_file::Color;

/// Index into the standard color scheme table.
///
/// Stored as u8 for cheap hashing in atlas keys (used in HashMap lookups every frame).
/// Default (0) = Gold, the neutral/unassigned color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct HouseColorIndex(pub u8);

/// Sentinel value meaning "do not apply house color remap — use raw palette."
/// Used for Neutral, Special, Civilian buildings that have no player color.
pub const NO_REMAP: HouseColorIndex = HouseColorIndex(255);

/// Returns true if the given owner is a non-player house that should NOT receive
/// player color remapping. These buildings render with their original palette.
pub fn is_non_player_house(owner: &str) -> bool {
    let up = owner.to_ascii_uppercase();
    matches!(
        up.as_str(),
        "NEUTRAL" | "SPECIAL" | "CIVILIAN" | "GOODGUY" | "BADGUY"
    )
}

/// Number of standard house color schemes.
const SCHEME_COUNT: usize = 9;

/// Number of shades per house color ramp (matches palette indices 16–31).
const RAMP_SIZE: usize = 16;

/// Standard scheme names for lookup. Order matches SCHEMES array indices.
const SCHEME_NAMES: [&str; SCHEME_COUNT] = [
    "gold",
    "darkblue",
    "darkred",
    "green",
    "orange",
    "purple",
    "lightblue",
    "brown",
    "grey",
];

/// Base RGB values for each scheme. Ramps are generated from these.
const SCHEME_BASES: [(u8, u8, u8); SCHEME_COUNT] = [
    (200, 180, 60),  // Gold
    (40, 60, 200),   // DarkBlue
    (200, 40, 40),   // DarkRed
    (40, 180, 40),   // Green
    (220, 140, 20),  // Orange
    (160, 40, 180),  // Purple
    (80, 160, 220),  // LightBlue
    (140, 90, 40),   // Brown
    (140, 140, 130), // Grey — civilian/neutral buildings
];

/// Pre-computed color ramps for all standard schemes.
/// Each scheme has 16 shades from brightest (index 0) to darkest (index 15).
static SCHEMES: [[Color; RAMP_SIZE]; SCHEME_COUNT] = {
    let mut result: [[Color; RAMP_SIZE]; SCHEME_COUNT] = [[Color {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    }; RAMP_SIZE]; SCHEME_COUNT];

    let mut scheme_idx: usize = 0;
    while scheme_idx < SCHEME_COUNT {
        let (base_r, base_g, base_b) = SCHEME_BASES[scheme_idx];
        result[scheme_idx] = generate_ramp(base_r, base_g, base_b);
        scheme_idx += 1;
    }
    result
};

/// Get the 16-color ramp for a house color index.
///
/// Returns the Gold ramp for out-of-range indices (defensive fallback).
pub fn house_color_ramp(index: HouseColorIndex) -> &'static [Color; RAMP_SIZE] {
    let idx: usize = index.0 as usize;
    if idx < SCHEME_COUNT {
        &SCHEMES[idx]
    } else {
        &SCHEMES[0] // Gold fallback
    }
}

/// Map a color scheme name to its index.
///
/// Case-insensitive lookup. Returns Gold (index 0) for unknown names.
/// Accepts both RA2 names ("DarkBlue") and bare color words ("Blue").
pub fn color_index_for_name(name: &str) -> HouseColorIndex {
    let lower: String = name.to_lowercase();

    // Exact match against standard names.
    for (i, &scheme_name) in SCHEME_NAMES.iter().enumerate() {
        if lower == scheme_name {
            return HouseColorIndex(i as u8);
        }
    }

    // Partial match for common aliases.
    if lower.contains("blue") && lower.contains("light") {
        return HouseColorIndex(6); // LightBlue
    }
    if lower.contains("blue") {
        return HouseColorIndex(1); // DarkBlue
    }
    if lower.contains("red") {
        return HouseColorIndex(2); // DarkRed
    }
    if lower.contains("green") {
        return HouseColorIndex(3); // Green
    }
    if lower.contains("orange") {
        return HouseColorIndex(4); // Orange
    }
    if lower.contains("purple") || lower.contains("magenta") {
        return HouseColorIndex(5); // Purple
    }
    if lower.contains("brown") {
        return HouseColorIndex(7); // Brown
    }
    if lower.contains("grey") || lower.contains("gray") {
        return HouseColorIndex(8); // Grey (civilian/neutral)
    }
    if lower.contains("gold") || lower.contains("yellow") {
        return HouseColorIndex(0); // Gold
    }

    // Default fallback for unknown color names.
    HouseColorIndex(0)
}

/// Generate a 16-shade gradient ramp from a base color.
///
/// Shade 0 is the brightest (base color tinted toward white).
/// Shade 15 is the darkest (base color shaded toward black).
/// This produces a smooth gradient suitable for house color remapping.
const fn generate_ramp(base_r: u8, base_g: u8, base_b: u8) -> [Color; RAMP_SIZE] {
    let mut ramp: [Color; RAMP_SIZE] = [Color {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    }; RAMP_SIZE];
    let mut i: usize = 0;
    while i < RAMP_SIZE {
        // t ranges from 0.0 (brightest) to 1.0 (darkest).
        // We use integer math to stay const-compatible.
        // Brightness range: 1.4 (lightest) down to 0.3 (darkest).
        // Formula: brightness = 1.4 - (i * 1.1 / 15)
        // In fixed point (x100): brightness_100 = 140 - (i * 110 / 15)
        let brightness_100: u32 = 140 - (i as u32 * 110 / 15);

        let r_raw: u32 = base_r as u32 * brightness_100 / 100;
        let g_raw: u32 = base_g as u32 * brightness_100 / 100;
        let b_raw: u32 = base_b as u32 * brightness_100 / 100;
        let r: u32 = if r_raw > 255 { 255 } else { r_raw };
        let g: u32 = if g_raw > 255 { 255 } else { g_raw };
        let b: u32 = if b_raw > 255 { 255 } else { b_raw };

        ramp[i] = Color {
            r: r as u8,
            g: g as u8,
            b: b as u8,
            a: 255,
        };
        i += 1;
    }
    ramp
}

/// Public wrapper for generate_ramp — creates a 16-shade gradient from an RGB base.
/// Used for tiberium color remapping (same algorithm as house colors).
pub fn generate_ramp_from_base(r: u8, g: u8, b: u8) -> [Color; RAMP_SIZE] {
    generate_ramp(r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_schemes_have_correct_size() {
        for (i, scheme) in SCHEMES.iter().enumerate() {
            assert_eq!(scheme.len(), 16, "Scheme {} has wrong size", i);
            // All colors should be fully opaque.
            for color in scheme {
                assert_eq!(color.a, 255, "Scheme {} has non-opaque color", i);
            }
        }
    }

    #[test]
    fn test_ramp_brightest_to_darkest() {
        // For each scheme, shade 0 should be brighter than shade 15.
        for (i, scheme) in SCHEMES.iter().enumerate() {
            let bright: u32 = scheme[0].r as u32 + scheme[0].g as u32 + scheme[0].b as u32;
            let dark: u32 = scheme[15].r as u32 + scheme[15].g as u32 + scheme[15].b as u32;
            assert!(
                bright > dark,
                "Scheme {} not bright→dark: {} vs {}",
                i,
                bright,
                dark
            );
        }
    }

    #[test]
    fn test_color_index_exact_match() {
        assert_eq!(color_index_for_name("DarkBlue"), HouseColorIndex(1));
        assert_eq!(color_index_for_name("darkred"), HouseColorIndex(2));
        assert_eq!(color_index_for_name("GOLD"), HouseColorIndex(0));
        assert_eq!(color_index_for_name("Green"), HouseColorIndex(3));
    }

    #[test]
    fn test_color_index_partial_match() {
        assert_eq!(color_index_for_name("Blue"), HouseColorIndex(1));
        assert_eq!(color_index_for_name("Red"), HouseColorIndex(2));
        assert_eq!(color_index_for_name("LightBlue"), HouseColorIndex(6));
    }

    #[test]
    fn test_unknown_color_returns_gold() {
        assert_eq!(color_index_for_name("PinkPolkaDot"), HouseColorIndex(0));
        assert_eq!(color_index_for_name(""), HouseColorIndex(0));
    }

    #[test]
    fn test_house_color_ramp_valid() {
        let ramp: &[Color; 16] = house_color_ramp(HouseColorIndex(1)); // DarkBlue
        // Blue should dominate in the DarkBlue scheme.
        assert!(
            ramp[0].b > ramp[0].r,
            "DarkBlue shade 0 should have more blue than red"
        );
    }

    #[test]
    fn test_out_of_range_returns_gold() {
        let gold: &[Color; 16] = house_color_ramp(HouseColorIndex(0));
        let oob: &[Color; 16] = house_color_ramp(HouseColorIndex(255));
        assert_eq!(gold[0], oob[0], "Out-of-range should return Gold ramp");
    }
}
