//! RE-backed bridge helper algorithms not yet fully wired into the live runtime.
//!
//! These helpers mirror closed behavior from the RE repo:
//! - low-bridge overlay damage step (RA2)
//! - low-bridge connected-section selector (YR)
//! - ZoneConnection record decode + proximity matching
//! - bridge-layer zone-id policy gate (RA2/YR)
//!
//! They are kept as pure functions so the runtime can adopt them incrementally
//! once mutable overlay state and ZoneConnection records are available.

const BRIDGE_GATE_BIT: u32 = 0x0100;
const NO_ZONE_CONNECTION: i16 = -1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BridgeOverlayTriple {
    pub a: i32,
    pub center: i32,
    pub b: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LowBridgeOverlayDamageReason {
    NotBridgeOverlay,
    GateFailed,
    NoTransition,
    Changed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LowBridgeOverlayDamageStepResult {
    pub ok: bool,
    pub reason: LowBridgeOverlayDamageReason,
    pub changed: bool,
    pub triple_out: BridgeOverlayTriple,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LowBridgeConnectedBand {
    WoodBand1,
    WoodBand2,
    ConcreteBand1,
    ConcreteBand2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LowBridgeConnectedAnchor {
    OppositeAdjacent,
    Center,
    PrimaryAdjacent,
    ConnectedChainHelper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LowBridgeConnectedPattern {
    A,
    B,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LowBridgeConnectedSectionSelectorResult {
    pub handled: bool,
    pub reason_not_bridge_overlay: bool,
    pub pattern: Option<LowBridgeConnectedPattern>,
    pub band: Option<LowBridgeConnectedBand>,
    pub anchor: Option<LowBridgeConnectedAnchor>,
    pub neighbor_range_lo: Option<i32>,
    pub neighbor_range_hi: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZoneConnectionRecord {
    pub cell_a: (i16, i16),
    pub cell_b: (i16, i16),
    pub flags: u32,
    pub flags_byte8: u8,
    pub skip_if_nonzero: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeZoneIdPolicyTarget {
    Ra21006,
    Yr1001,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BridgeZoneIdPolicyDecision {
    pub use_bridge_path: bool,
    pub call_bridge_remap_fallback: bool,
    pub return_no_zone: bool,
}

pub fn low_bridge_overlay_damage_step_ra2(
    triple: BridgeOverlayTriple,
    damage: i32,
    bridge_strength: i32,
    atom_damage: i32,
    random_ranged_1_bridge_strength: i32,
) -> LowBridgeOverlayDamageStepResult {
    let center = triple.center;
    let in_a = in_range_inclusive(center, 0x4a, 0x63);
    let in_b = in_range_inclusive(center, 0xcd, 0xe6);

    if !in_a && !in_b {
        return LowBridgeOverlayDamageStepResult {
            ok: true,
            reason: LowBridgeOverlayDamageReason::NotBridgeOverlay,
            changed: false,
            triple_out: triple,
        };
    }

    if damage != atom_damage {
        if bridge_strength <= 0 || random_ranged_1_bridge_strength >= damage {
            return LowBridgeOverlayDamageStepResult {
                ok: true,
                reason: LowBridgeOverlayDamageReason::GateFailed,
                changed: false,
                triple_out: triple,
            };
        }
    }

    let new_index = if in_a {
        pattern_a_new_index(center)
    } else {
        pattern_b_new_index(center)
    };

    match new_index {
        Some(new_index) => LowBridgeOverlayDamageStepResult {
            ok: true,
            reason: LowBridgeOverlayDamageReason::Changed,
            changed: true,
            triple_out: BridgeOverlayTriple {
                a: new_index,
                center: new_index,
                b: new_index,
            },
        },
        None => LowBridgeOverlayDamageStepResult {
            ok: true,
            reason: LowBridgeOverlayDamageReason::NoTransition,
            changed: false,
            triple_out: triple,
        },
    }
}

pub fn low_bridge_connected_section_selector_yr(
    center_overlay_type_index: i32,
    primary_probe_in_family_range: bool,
    secondary_probe_in_family_range: bool,
) -> LowBridgeConnectedSectionSelectorResult {
    let Some(band) = classify_low_bridge_band(center_overlay_type_index) else {
        return LowBridgeConnectedSectionSelectorResult {
            handled: false,
            reason_not_bridge_overlay: true,
            pattern: None,
            band: None,
            anchor: None,
            neighbor_range_lo: None,
            neighbor_range_hi: None,
        };
    };

    let (pattern, neighbor_range_lo, neighbor_range_hi) = match band {
        LowBridgeConnectedBand::WoodBand1 | LowBridgeConnectedBand::WoodBand2 => {
            (LowBridgeConnectedPattern::A, 0x4a, 0x65)
        }
        LowBridgeConnectedBand::ConcreteBand1 | LowBridgeConnectedBand::ConcreteBand2 => {
            (LowBridgeConnectedPattern::B, 0xcd, 0xe8)
        }
    };

    let anchor = if !primary_probe_in_family_range {
        LowBridgeConnectedAnchor::OppositeAdjacent
    } else if !secondary_probe_in_family_range {
        LowBridgeConnectedAnchor::Center
    } else if matches!(
        band,
        LowBridgeConnectedBand::WoodBand1 | LowBridgeConnectedBand::ConcreteBand1
    ) {
        LowBridgeConnectedAnchor::PrimaryAdjacent
    } else {
        LowBridgeConnectedAnchor::ConnectedChainHelper
    };

    LowBridgeConnectedSectionSelectorResult {
        handled: true,
        reason_not_bridge_overlay: false,
        pattern: Some(pattern),
        band: Some(band),
        anchor: Some(anchor),
        neighbor_range_lo: Some(neighbor_range_lo),
        neighbor_range_hi: Some(neighbor_range_hi),
    }
}

pub fn decode_zone_connection_record(record: &[u8]) -> ZoneConnectionRecord {
    assert_eq!(record.len(), 16, "expected 16-byte ZoneConnection record");

    let flags = read_u32_le(record, 0x08);
    ZoneConnectionRecord {
        cell_a: (read_i16_le(record, 0x00), read_i16_le(record, 0x02)),
        cell_b: (read_i16_le(record, 0x04), read_i16_le(record, 0x06)),
        flags,
        flags_byte8: (flags & 0xff) as u8,
        skip_if_nonzero: read_u32_le(record, 0x0c),
    }
}

pub fn zone_connection_matches_cell(record: &[u8], cell: (i16, i16), dist: i16) -> bool {
    let decoded = decode_zone_connection_record(record);
    if decoded.skip_if_nonzero != 0 {
        return false;
    }

    let dist = dist.max(0);
    let ((ax, ay), (bx, by)) = (decoded.cell_a, decoded.cell_b);

    if ax == bx {
        let y_min = ay.min(by);
        let y_max = ay.max(by);
        cell.1 >= y_min && cell.1 <= y_max && (cell.0 - ax).abs() <= dist
    } else {
        let x_min = ax.min(bx);
        let x_max = ax.max(bx);
        cell.0 >= x_min && cell.0 <= x_max && (cell.1 - ay).abs() <= dist
    }
}

pub fn get_cell_zone_id_bridge_policy_decision(
    target: BridgeZoneIdPolicyTarget,
    on_bridge: bool,
    cell_flags_dword: u32,
    zone_connection_index: i16,
) -> BridgeZoneIdPolicyDecision {
    let use_bridge_path = on_bridge && (cell_flags_dword & BRIDGE_GATE_BIT) != 0;
    if !use_bridge_path {
        return BridgeZoneIdPolicyDecision {
            use_bridge_path: false,
            call_bridge_remap_fallback: false,
            return_no_zone: false,
        };
    }

    if zone_connection_index != NO_ZONE_CONNECTION {
        return BridgeZoneIdPolicyDecision {
            use_bridge_path: true,
            call_bridge_remap_fallback: false,
            return_no_zone: false,
        };
    }

    match target {
        BridgeZoneIdPolicyTarget::Yr1001 => BridgeZoneIdPolicyDecision {
            use_bridge_path: true,
            call_bridge_remap_fallback: true,
            return_no_zone: false,
        },
        BridgeZoneIdPolicyTarget::Ra21006 => BridgeZoneIdPolicyDecision {
            use_bridge_path: true,
            call_bridge_remap_fallback: false,
            return_no_zone: true,
        },
    }
}

fn in_range_inclusive(x: i32, lo: i32, hi: i32) -> bool {
    x >= lo && x <= hi
}

fn pattern_a_new_index(center_overlay_type_index: i32) -> Option<i32> {
    match center_overlay_type_index {
        0x60 => Some(0x61),
        0x62 => Some(0x63),
        x if x < 0x59 => Some(0x59),
        x if x < 0x5c => Some(0x65),
        _ => None,
    }
}

fn pattern_b_new_index(center_overlay_type_index: i32) -> Option<i32> {
    match center_overlay_type_index {
        0xe3 => Some(0xe4),
        0xe5 => Some(0xe6),
        x if x < 0xdc => Some(0xdc),
        x if x < 0xdf => Some(0xe8),
        _ => None,
    }
}

fn classify_low_bridge_band(center_overlay_type_index: i32) -> Option<LowBridgeConnectedBand> {
    let x = center_overlay_type_index;

    if in_range_inclusive(x, 0x4a, 0x52) || in_range_inclusive(x, 0x5c, 0x5f) || x == 0x64 {
        return Some(LowBridgeConnectedBand::WoodBand1);
    }
    if in_range_inclusive(x, 0x53, 0x5b) || in_range_inclusive(x, 0x60, 0x63) || x == 0x65 {
        return Some(LowBridgeConnectedBand::WoodBand2);
    }
    if in_range_inclusive(x, 0xcd, 0xd5) || in_range_inclusive(x, 0xdf, 0xe2) || x == 0xe7 {
        return Some(LowBridgeConnectedBand::ConcreteBand1);
    }
    if in_range_inclusive(x, 0xd6, 0xde) || in_range_inclusive(x, 0xe3, 0xe6) || x == 0xe8 {
        return Some(LowBridgeConnectedBand::ConcreteBand2);
    }

    None
}

fn read_u16_le(bytes: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([bytes[off], bytes[off + 1]])
}

fn read_i16_le(bytes: &[u8], off: usize) -> i16 {
    read_u16_le(bytes, off) as i16
}

fn read_u32_le(bytes: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_bridge_damage_step_ignores_non_bridge_overlay() {
        let out = low_bridge_overlay_damage_step_ra2(
            BridgeOverlayTriple {
                a: 1,
                center: 1234,
                b: 2,
            },
            50,
            150,
            999,
            1,
        );
        assert_eq!(out.reason, LowBridgeOverlayDamageReason::NotBridgeOverlay);
        assert!(!out.changed);
        assert_eq!(out.triple_out.center, 1234);
    }

    #[test]
    fn low_bridge_damage_step_applies_rng_gate() {
        let out = low_bridge_overlay_damage_step_ra2(
            BridgeOverlayTriple {
                a: 96,
                center: 96,
                b: 96,
            },
            10,
            150,
            999,
            10,
        );
        assert_eq!(out.reason, LowBridgeOverlayDamageReason::GateFailed);
        assert!(!out.changed);
    }

    #[test]
    fn low_bridge_damage_step_atom_damage_bypasses_gate() {
        let out = low_bridge_overlay_damage_step_ra2(
            BridgeOverlayTriple {
                a: 96,
                center: 96,
                b: 96,
            },
            999,
            150,
            999,
            150,
        );
        assert_eq!(out.reason, LowBridgeOverlayDamageReason::Changed);
        assert!(out.changed);
        assert_eq!(
            out.triple_out,
            BridgeOverlayTriple {
                a: 97,
                center: 97,
                b: 97,
            }
        );
    }

    #[test]
    fn low_bridge_damage_step_maps_wood_family() {
        let out = low_bridge_overlay_damage_step_ra2(
            BridgeOverlayTriple {
                a: 74,
                center: 74,
                b: 74,
            },
            2,
            150,
            999,
            1,
        );
        assert_eq!(out.triple_out.center, 89);

        let out = low_bridge_overlay_damage_step_ra2(
            BridgeOverlayTriple {
                a: 89,
                center: 89,
                b: 90,
            },
            2,
            150,
            999,
            1,
        );
        assert_eq!(out.triple_out.center, 101);
    }

    #[test]
    fn low_bridge_damage_step_maps_concrete_family_and_no_transition() {
        let out = low_bridge_overlay_damage_step_ra2(
            BridgeOverlayTriple {
                a: 227,
                center: 227,
                b: 227,
            },
            2,
            150,
            999,
            1,
        );
        assert_eq!(out.triple_out.center, 228);

        let no_change = low_bridge_overlay_damage_step_ra2(
            BridgeOverlayTriple {
                a: 223,
                center: 223,
                b: 223,
            },
            2,
            150,
            999,
            1,
        );
        assert_eq!(no_change.reason, LowBridgeOverlayDamageReason::NoTransition);
        assert!(!no_change.changed);
    }

    #[test]
    fn low_bridge_selector_rejects_non_bridge_overlay() {
        let out = low_bridge_connected_section_selector_yr(1, false, false);
        assert!(!out.handled);
        assert!(out.reason_not_bridge_overlay);
    }

    #[test]
    fn low_bridge_selector_uses_exact_anchor_policy() {
        let out = low_bridge_connected_section_selector_yr(74, false, false);
        assert_eq!(out.pattern, Some(LowBridgeConnectedPattern::A));
        assert_eq!(out.band, Some(LowBridgeConnectedBand::WoodBand1));
        assert_eq!(out.anchor, Some(LowBridgeConnectedAnchor::OppositeAdjacent));
        assert_eq!(out.neighbor_range_lo, Some(74));
        assert_eq!(out.neighbor_range_hi, Some(101));

        let out = low_bridge_connected_section_selector_yr(74, true, false);
        assert_eq!(out.anchor, Some(LowBridgeConnectedAnchor::Center));

        let out = low_bridge_connected_section_selector_yr(74, true, true);
        assert_eq!(out.anchor, Some(LowBridgeConnectedAnchor::PrimaryAdjacent));

        let out = low_bridge_connected_section_selector_yr(83, true, true);
        assert_eq!(out.band, Some(LowBridgeConnectedBand::WoodBand2));
        assert_eq!(
            out.anchor,
            Some(LowBridgeConnectedAnchor::ConnectedChainHelper)
        );

        let out = low_bridge_connected_section_selector_yr(205, false, false);
        assert_eq!(out.pattern, Some(LowBridgeConnectedPattern::B));
        assert_eq!(out.band, Some(LowBridgeConnectedBand::ConcreteBand1));
        assert_eq!(out.anchor, Some(LowBridgeConnectedAnchor::OppositeAdjacent));
        assert_eq!(out.neighbor_range_lo, Some(205));
        assert_eq!(out.neighbor_range_hi, Some(232));

        let out = low_bridge_connected_section_selector_yr(214, true, true);
        assert_eq!(out.band, Some(LowBridgeConnectedBand::ConcreteBand2));
        assert_eq!(
            out.anchor,
            Some(LowBridgeConnectedAnchor::ConnectedChainHelper)
        );
    }

    #[test]
    fn zone_connection_record_decodes_layout() {
        let record = [10, 0, 254, 255, 10, 0, 5, 0, 1, 0, 0, 0, 0, 0, 0, 0];
        let decoded = decode_zone_connection_record(&record);
        assert_eq!(decoded.cell_a, (10, -2));
        assert_eq!(decoded.cell_b, (10, 5));
        assert_eq!(decoded.flags, 1);
        assert_eq!(decoded.flags_byte8, 1);
        assert_eq!(decoded.skip_if_nonzero, 0);
    }

    #[test]
    fn zone_connection_match_uses_axis_aligned_segment_proximity() {
        let record = [10, 0, 254, 255, 10, 0, 5, 0, 1, 0, 0, 0, 0, 0, 0, 0];
        assert!(zone_connection_matches_cell(&record, (9, 0), 1));
        assert!(!zone_connection_matches_cell(&record, (8, 0), 1));
        assert!(!zone_connection_matches_cell(&record, (10, 6), 1));
    }

    #[test]
    fn zone_connection_match_respects_skip_flag() {
        let record = [10, 0, 254, 255, 10, 0, 5, 0, 1, 0, 0, 0, 1, 0, 0, 0];
        assert!(!zone_connection_matches_cell(&record, (10, 0), 1));
    }

    #[test]
    fn bridge_zone_policy_turns_off_when_on_bridge_false() {
        let out = get_cell_zone_id_bridge_policy_decision(
            BridgeZoneIdPolicyTarget::Yr1001,
            false,
            0x0100,
            -1,
        );
        assert_eq!(
            out,
            BridgeZoneIdPolicyDecision {
                use_bridge_path: false,
                call_bridge_remap_fallback: false,
                return_no_zone: false,
            }
        );
    }

    #[test]
    fn bridge_zone_policy_turns_off_when_bridge_bit_clear() {
        let out =
            get_cell_zone_id_bridge_policy_decision(BridgeZoneIdPolicyTarget::Ra21006, true, 0, -1);
        assert!(!out.use_bridge_path);
        assert!(!out.call_bridge_remap_fallback);
        assert!(!out.return_no_zone);
    }

    #[test]
    fn bridge_zone_policy_matches_ra2_and_yr_fallback_split() {
        let hit = get_cell_zone_id_bridge_policy_decision(
            BridgeZoneIdPolicyTarget::Ra21006,
            true,
            0x0100,
            3,
        );
        assert_eq!(
            hit,
            BridgeZoneIdPolicyDecision {
                use_bridge_path: true,
                call_bridge_remap_fallback: false,
                return_no_zone: false,
            }
        );

        let ra2_miss = get_cell_zone_id_bridge_policy_decision(
            BridgeZoneIdPolicyTarget::Ra21006,
            true,
            0x0100,
            -1,
        );
        assert_eq!(
            ra2_miss,
            BridgeZoneIdPolicyDecision {
                use_bridge_path: true,
                call_bridge_remap_fallback: false,
                return_no_zone: true,
            }
        );

        let yr_miss = get_cell_zone_id_bridge_policy_decision(
            BridgeZoneIdPolicyTarget::Yr1001,
            true,
            0x0100,
            -1,
        );
        assert_eq!(
            yr_miss,
            BridgeZoneIdPolicyDecision {
                use_bridge_path: true,
                call_bridge_remap_fallback: true,
                return_no_zone: false,
            }
        );
    }
}
