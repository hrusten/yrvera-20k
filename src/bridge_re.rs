//! Pure RE-backed bridge helpers.
//!
//! This file mirrors the closed bridge-related canon specs as small,
//! deterministic helpers with no dependency on the rest of the game code.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BridgeOverlayTriple {
    pub a: i32,
    pub center: i32,
    pub b: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LowBridgeOverlayDamageStepReason {
    NotBridgeOverlay,
    GateFailed,
    NoTransition,
    Changed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LowBridgeOverlayDamageStepResult {
    pub ok: bool,
    pub reason: LowBridgeOverlayDamageStepReason,
    pub changed: bool,
    pub triple_out: BridgeOverlayTriple,
}

fn in_range_inclusive(x: i32, lo: i32, hi: i32) -> bool {
    x >= lo && x <= hi
}

fn pattern_a_new_index_or_null(center_overlay_type_index: i32) -> Option<i32> {
    match center_overlay_type_index {
        0x60 => Some(0x61),
        0x62 => Some(0x63),
        x if x < 0x59 => Some(0x59),
        x if x < 0x5c => Some(0x65),
        _ => None,
    }
}

fn pattern_b_new_index_or_null(center_overlay_type_index: i32) -> Option<i32> {
    match center_overlay_type_index {
        0xe3 => Some(0xe4),
        0xe5 => Some(0xe6),
        x if x < 0xdc => Some(0xdc),
        x if x < 0xdf => Some(0xe8),
        _ => None,
    }
}

pub fn low_bridge_overlay_damage_step_ra2(
    triple: BridgeOverlayTriple,
    damage: i32,
    bridge_strength: i32,
    atom_damage: i32,
    random_ranged_1_bridge_strength: i32,
) -> LowBridgeOverlayDamageStepResult {
    let center = triple.center;
    let in_wood = in_range_inclusive(center, 0x4a, 0x63);
    let in_concrete = in_range_inclusive(center, 0xcd, 0xe6);

    if !in_wood && !in_concrete {
        return LowBridgeOverlayDamageStepResult {
            ok: true,
            reason: LowBridgeOverlayDamageStepReason::NotBridgeOverlay,
            changed: false,
            triple_out: triple,
        };
    }

    if damage != atom_damage {
        if bridge_strength <= 0 || random_ranged_1_bridge_strength >= damage {
            return LowBridgeOverlayDamageStepResult {
                ok: true,
                reason: LowBridgeOverlayDamageStepReason::GateFailed,
                changed: false,
                triple_out: triple,
            };
        }
    }

    let new_index = if in_wood {
        pattern_a_new_index_or_null(center)
    } else {
        pattern_b_new_index_or_null(center)
    };

    let Some(new_index) = new_index else {
        return LowBridgeOverlayDamageStepResult {
            ok: true,
            reason: LowBridgeOverlayDamageStepReason::NoTransition,
            changed: false,
            triple_out: triple,
        };
    };

    LowBridgeOverlayDamageStepResult {
        ok: true,
        reason: LowBridgeOverlayDamageStepReason::Changed,
        changed: true,
        triple_out: BridgeOverlayTriple {
            a: new_index,
            center: new_index,
            b: new_index,
        },
    }
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
pub struct LowBridgeConnectedSelectorResult {
    pub handled: bool,
    pub reason: LowBridgeConnectedSectionSelectorReason,
    pub pattern: Option<LowBridgePattern>,
    pub band: Option<LowBridgeConnectedBand>,
    pub anchor: Option<LowBridgeConnectedAnchor>,
    pub neighbor_range_lo: Option<i32>,
    pub neighbor_range_hi: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LowBridgeConnectedSectionSelectorReason {
    NotBridgeOverlay,
    Selected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LowBridgePattern {
    A,
    B,
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

fn low_bridge_neighbor_range_for_band(
    band: LowBridgeConnectedBand,
) -> (LowBridgePattern, i32, i32) {
    match band {
        LowBridgeConnectedBand::WoodBand1 | LowBridgeConnectedBand::WoodBand2 => {
            (LowBridgePattern::A, 0x4a, 0x65)
        }
        LowBridgeConnectedBand::ConcreteBand1 | LowBridgeConnectedBand::ConcreteBand2 => {
            (LowBridgePattern::B, 0xcd, 0xe8)
        }
    }
}

pub fn low_bridge_connected_section_selector_yr(
    center_overlay_type_index: i32,
    primary_probe_in_family_range: bool,
    secondary_probe_in_family_range: bool,
) -> LowBridgeConnectedSelectorResult {
    let Some(band) = classify_low_bridge_band(center_overlay_type_index) else {
        return LowBridgeConnectedSelectorResult {
            handled: false,
            reason: LowBridgeConnectedSectionSelectorReason::NotBridgeOverlay,
            pattern: None,
            band: None,
            anchor: None,
            neighbor_range_lo: None,
            neighbor_range_hi: None,
        };
    };

    let (pattern, neighbor_range_lo, neighbor_range_hi) = low_bridge_neighbor_range_for_band(band);

    let anchor = if !primary_probe_in_family_range {
        LowBridgeConnectedAnchor::OppositeAdjacent
    } else if !secondary_probe_in_family_range {
        LowBridgeConnectedAnchor::Center
    } else {
        match band {
            LowBridgeConnectedBand::WoodBand1 | LowBridgeConnectedBand::ConcreteBand1 => {
                LowBridgeConnectedAnchor::PrimaryAdjacent
            }
            LowBridgeConnectedBand::WoodBand2 | LowBridgeConnectedBand::ConcreteBand2 => {
                LowBridgeConnectedAnchor::ConnectedChainHelper
            }
        }
    };

    LowBridgeConnectedSelectorResult {
        handled: true,
        reason: LowBridgeConnectedSectionSelectorReason::Selected,
        pattern: Some(pattern),
        band: Some(band),
        anchor: Some(anchor),
        neighbor_range_lo: Some(neighbor_range_lo),
        neighbor_range_hi: Some(neighbor_range_hi),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZoneConnectionRecord {
    pub cell_a: Cell,
    pub cell_b: Cell,
    pub flags: u32,
    pub flags_byte8: u8,
    pub skip_if_nonzero: u32,
}

fn read_u8(bytes: &[u8], off: usize) -> u8 {
    bytes.get(off).copied().unwrap_or(0)
}

fn read_i16_le(bytes: &[u8], off: usize) -> i16 {
    i16::from_le_bytes([read_u8(bytes, off), read_u8(bytes, off + 1)])
}

fn read_u32_le(bytes: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([
        read_u8(bytes, off),
        read_u8(bytes, off + 1),
        read_u8(bytes, off + 2),
        read_u8(bytes, off + 3),
    ])
}

pub fn decode_zone_connection_record(record: &[u8]) -> Result<ZoneConnectionRecord, String> {
    if record.len() != 16 {
        return Err(format!("expected 16-byte record, got len={}", record.len()));
    }

    let cell_a = Cell {
        x: read_i16_le(record, 0x00) as i32,
        y: read_i16_le(record, 0x02) as i32,
    };
    let cell_b = Cell {
        x: read_i16_le(record, 0x04) as i32,
        y: read_i16_le(record, 0x06) as i32,
    };
    let flags = read_u32_le(record, 0x08);
    let skip_if_nonzero = read_u32_le(record, 0x0c);

    Ok(ZoneConnectionRecord {
        cell_a,
        cell_b,
        flags,
        flags_byte8: (flags & 0xff) as u8,
        skip_if_nonzero,
    })
}

pub fn zone_connection_matches_cell(record: &[u8], cell: Cell, dist: i32) -> bool {
    let Ok(decoded) = decode_zone_connection_record(record) else {
        return false;
    };
    if decoded.skip_if_nonzero != 0 {
        return false;
    }

    let d = dist.max(0);
    let a = decoded.cell_a;
    let b = decoded.cell_b;

    if a.x == b.x {
        let y_min = a.y.min(b.y);
        let y_max = a.y.max(b.y);
        return cell.y >= y_min && cell.y <= y_max && (cell.x - a.x).abs() <= d;
    }

    let x_min = a.x.min(b.x);
    let x_max = a.x.max(b.x);
    cell.x >= x_min && cell.x <= x_max && (cell.y - a.y).abs() <= d
}

#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeZonePolicyTarget {
    Ra2_1006,
    Yr_1001,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GetCellZoneIdBridgePolicyResult {
    pub use_bridge_path: bool,
    pub call_bridge_remap_fallback: bool,
    pub return_no_zone: bool,
}

const BRIDGE_GATE_BIT: u32 = 0x0100;
const NO_ZONE_CONNECTION: i32 = -1;

pub fn get_cell_zone_id_bridge_policy_decision(
    target: BridgeZonePolicyTarget,
    on_bridge: bool,
    cell_flags_dword: u32,
    zone_connection_index: i32,
) -> GetCellZoneIdBridgePolicyResult {
    let use_bridge_path = on_bridge && (cell_flags_dword & BRIDGE_GATE_BIT) != 0;
    if !use_bridge_path {
        return GetCellZoneIdBridgePolicyResult {
            use_bridge_path: false,
            call_bridge_remap_fallback: false,
            return_no_zone: false,
        };
    }

    if zone_connection_index != NO_ZONE_CONNECTION {
        return GetCellZoneIdBridgePolicyResult {
            use_bridge_path: true,
            call_bridge_remap_fallback: false,
            return_no_zone: false,
        };
    }

    if matches!(target, BridgeZonePolicyTarget::Yr_1001) {
        return GetCellZoneIdBridgePolicyResult {
            use_bridge_path: true,
            call_bridge_remap_fallback: true,
            return_no_zone: false,
        };
    }

    GetCellZoneIdBridgePolicyResult {
        use_bridge_path: true,
        call_bridge_remap_fallback: false,
        return_no_zone: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_bridge_overlay_damage_step_branches() {
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
        assert_eq!(
            out.reason,
            LowBridgeOverlayDamageStepReason::NotBridgeOverlay
        );
        assert!(!out.changed);
        assert_eq!(
            out.triple_out,
            BridgeOverlayTriple {
                a: 1,
                center: 1234,
                b: 2,
            }
        );

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
        assert_eq!(out.reason, LowBridgeOverlayDamageStepReason::GateFailed);
        assert!(!out.changed);

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
        assert_eq!(out.reason, LowBridgeOverlayDamageStepReason::Changed);
        assert!(out.changed);
        assert_eq!(
            out.triple_out,
            BridgeOverlayTriple {
                a: 97,
                center: 97,
                b: 97,
            }
        );

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
        assert!(out.changed);

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
        assert!(out.changed);

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
        assert!(out.changed);

        let out = low_bridge_overlay_damage_step_ra2(
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
        assert_eq!(out.reason, LowBridgeOverlayDamageStepReason::NoTransition);
        assert!(!out.changed);
    }

    #[test]
    fn low_bridge_connected_section_selector_branches() {
        let out = low_bridge_connected_section_selector_yr(1, false, false);
        assert!(!out.handled);
        assert_eq!(
            out.reason,
            LowBridgeConnectedSectionSelectorReason::NotBridgeOverlay
        );

        let out = low_bridge_connected_section_selector_yr(74, false, false);
        assert_eq!(
            out.reason,
            LowBridgeConnectedSectionSelectorReason::Selected
        );
        assert_eq!(out.pattern, Some(LowBridgePattern::A));
        assert_eq!(out.band, Some(LowBridgeConnectedBand::WoodBand1));
        assert_eq!(out.anchor, Some(LowBridgeConnectedAnchor::OppositeAdjacent));
        assert_eq!(out.neighbor_range_lo, Some(0x4a));
        assert_eq!(out.neighbor_range_hi, Some(0x65));

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
        assert_eq!(out.pattern, Some(LowBridgePattern::B));
        assert_eq!(out.band, Some(LowBridgeConnectedBand::ConcreteBand1));
        assert_eq!(out.anchor, Some(LowBridgeConnectedAnchor::OppositeAdjacent));
        assert_eq!(out.neighbor_range_lo, Some(0xcd));
        assert_eq!(out.neighbor_range_hi, Some(0xe8));

        let out = low_bridge_connected_section_selector_yr(214, true, true);
        assert_eq!(out.band, Some(LowBridgeConnectedBand::ConcreteBand2));
        assert_eq!(
            out.anchor,
            Some(LowBridgeConnectedAnchor::ConnectedChainHelper)
        );
    }

    #[test]
    fn zone_connection_record_layout_decode_basic() {
        let record = vec![10, 0, 254, 255, 10, 0, 5, 0, 1, 0, 0, 0, 0, 0, 0, 0];
        let decoded = decode_zone_connection_record(&record).expect("decode");
        assert_eq!(decoded.cell_a, Cell { x: 10, y: -2 });
        assert_eq!(decoded.cell_b, Cell { x: 10, y: 5 });
        assert_eq!(decoded.flags, 1);
        assert_eq!(decoded.flags_byte8, 1);
        assert_eq!(decoded.skip_if_nonzero, 0);
    }

    #[test]
    fn zone_connection_record_layout_match_vertical_segment() {
        let record = vec![10, 0, 254, 255, 10, 0, 5, 0, 1, 0, 0, 0, 0, 0, 0, 0];
        assert!(zone_connection_matches_cell(
            &record,
            Cell { x: 9, y: 0 },
            1
        ));
        assert!(!zone_connection_matches_cell(
            &record,
            Cell { x: 8, y: 0 },
            1
        ));
        assert!(!zone_connection_matches_cell(
            &record,
            Cell { x: 10, y: 6 },
            1
        ));
    }

    #[test]
    fn zone_connection_record_layout_match_skip_if_nonzero() {
        let record = vec![10, 0, 254, 255, 10, 0, 5, 0, 1, 0, 0, 0, 1, 0, 0, 0];
        assert!(!zone_connection_matches_cell(
            &record,
            Cell { x: 10, y: 0 },
            1
        ));
    }

    #[test]
    fn zone_id_bridge_policy_decisions() {
        let out = get_cell_zone_id_bridge_policy_decision(
            BridgeZonePolicyTarget::Yr_1001,
            false,
            256,
            -1,
        );
        assert_eq!(
            out,
            GetCellZoneIdBridgePolicyResult {
                use_bridge_path: false,
                call_bridge_remap_fallback: false,
                return_no_zone: false,
            }
        );

        let out =
            get_cell_zone_id_bridge_policy_decision(BridgeZonePolicyTarget::Ra2_1006, true, 0, -1);
        assert_eq!(
            out,
            GetCellZoneIdBridgePolicyResult {
                use_bridge_path: false,
                call_bridge_remap_fallback: false,
                return_no_zone: false,
            }
        );

        let out =
            get_cell_zone_id_bridge_policy_decision(BridgeZonePolicyTarget::Ra2_1006, true, 256, 3);
        assert_eq!(
            out,
            GetCellZoneIdBridgePolicyResult {
                use_bridge_path: true,
                call_bridge_remap_fallback: false,
                return_no_zone: false,
            }
        );

        let out = get_cell_zone_id_bridge_policy_decision(
            BridgeZonePolicyTarget::Ra2_1006,
            true,
            256,
            -1,
        );
        assert_eq!(
            out,
            GetCellZoneIdBridgePolicyResult {
                use_bridge_path: true,
                call_bridge_remap_fallback: false,
                return_no_zone: true,
            }
        );

        let out =
            get_cell_zone_id_bridge_policy_decision(BridgeZonePolicyTarget::Yr_1001, true, 256, -1);
        assert_eq!(
            out,
            GetCellZoneIdBridgePolicyResult {
                use_bridge_path: true,
                call_bridge_remap_fallback: true,
                return_no_zone: false,
            }
        );
    }
}
