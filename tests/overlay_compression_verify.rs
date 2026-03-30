//! Verify which compression algorithm RA2 actually uses for OverlayPack.
//!
//! Strategy: extract the raw base64-decoded (but not yet decompressed) bytes
//! from [OverlayPack], then try BOTH LCW and LZO on those bytes and check:
//! 1. Which one produces exactly 262144 bytes without error
//! 2. Whether the ore cells found match between the two codecs
//! 3. Cross-check a known ore position from a real map against both outputs
//!
//! Run with: cargo test --test overlay_compression_verify -- --nocapture

use vera20k::util::base64;
use vera20k::util::lcw;
use vera20k::util::lzo;
use std::path::Path;

fn ra2_dir() -> String {
    std::env::var("RA2_DIR")
        .unwrap_or_else(|_| panic!("Set RA2_DIR env var to your RA2/YR install directory"))
}
const OVERLAY_GRID: usize = 512;
const EXPECTED_SIZE: usize = 262_144;

// Ore overlay IDs per the doc §4.2 (TIB01-TIB20 = 102-121, GEM01-GEM12 = 27-38)
fn is_ore(id: u8) -> bool {
    matches!(id, 27..=38 | 102..=121 | 127..=166)
}

fn load_raw_overlay_pack_from_map(map: &vera20k::map::map_file::MapFile) -> Vec<u8> {
    let section = map.ini.section("OverlayPack").expect("OverlayPack section");
    let mut b64: String = String::new();
    for key in section.keys() {
        if let Some(val) = section.get(key) {
            b64.push_str(val);
        }
    }
    assert!(!b64.is_empty(), "no base64 data in OverlayPack");
    base64::base64_decode(&b64).expect("base64 decode")
}

fn count_ore_cells(data: &[u8]) -> usize {
    data.iter()
        .take(EXPECTED_SIZE)
        .filter(|&&b| is_ore(b))
        .count()
}

fn ore_positions(data: &[u8]) -> Vec<(u16, u16)> {
    data.iter()
        .take(EXPECTED_SIZE)
        .enumerate()
        .filter(|&(_, &b)| is_ore(b))
        .map(|(i, _)| ((i % OVERLAY_GRID) as u16, (i / OVERLAY_GRID) as u16))
        .collect()
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn lcw_vs_lzo_on_real_overlay_pack() {
    let _ = env_logger::try_init();
    let ra2_dir_str = ra2_dir();
    let ra2_dir = Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        println!("SKIP: RA2 dir not found");
        return;
    }

    let map_path = ra2_dir.join("Dustbowl.mmx");
    let map = vera20k::map::map_file::load_mmx(&map_path).expect("load map");
    let raw = load_raw_overlay_pack_from_map(&map);
    println!("Raw base64-decoded OverlayPack: {} bytes", raw.len());
    println!("First 16 raw bytes: {:02X?}", &raw[..16.min(raw.len())]);

    // Try LCW
    let lcw_result = lcw::decompress_chunks(&raw);
    match &lcw_result {
        Ok(data) => println!(
            "LCW: OK → {} bytes, {} ore cells",
            data.len(),
            count_ore_cells(data)
        ),
        Err(e) => println!("LCW: FAILED — {}", e),
    }

    // Try LZO
    let lzo_result = lzo::decompress_chunks(&raw);
    match &lzo_result {
        Ok(data) => println!(
            "LZO: OK → {} bytes, {} ore cells",
            data.len(),
            count_ore_cells(data)
        ),
        Err(e) => println!("LZO: FAILED — {}", e),
    }

    // The correct decompressor must produce exactly EXPECTED_SIZE bytes
    let lcw_ok = lcw_result
        .as_ref()
        .map_or(false, |d| d.len() == EXPECTED_SIZE);
    let lzo_ok = lzo_result
        .as_ref()
        .map_or(false, |d| d.len() == EXPECTED_SIZE);
    println!("\nLCW produces 262144 bytes: {}", lcw_ok);
    println!("LZO produces 262144 bytes: {}", lzo_ok);

    // If both succeed, compare ore positions
    if let (Ok(lcw_data), Ok(lzo_data)) = (&lcw_result, &lzo_result) {
        let lcw_ore = ore_positions(lcw_data);
        let lzo_ore = ore_positions(lzo_data);
        println!("\nOre cells: LCW={} LZO={}", lcw_ore.len(), lzo_ore.len());
        if lcw_ore == lzo_ore {
            println!("Both codecs produce IDENTICAL ore positions");
        } else {
            println!("Codecs produce DIFFERENT ore positions — outputs differ");
            println!("LCW first 5: {:?}", &lcw_ore[..5.min(lcw_ore.len())]);
            println!("LZO first 5: {:?}", &lzo_ore[..5.min(lzo_ore.len())]);
        }

        // Check if raw output bytes differ
        let differ = lcw_data
            .iter()
            .zip(lzo_data.iter())
            .take(EXPECTED_SIZE)
            .filter(|(a, b)| a != b)
            .count();
        println!("Bytes that differ between LCW and LZO outputs: {}", differ);
    }

    // Sanity: the result used by the map loader (LCW) should have ore at known Dustbowl cells.
    // We know from overlay_ids test that Dustbowl has 316 TIB01 cells (id=102).
    if let Ok(data) = &lcw_result {
        let tib01_count = data
            .iter()
            .take(EXPECTED_SIZE)
            .filter(|&&b| b == 102)
            .count();
        println!("\nLCW TIB01 (id=102) cells on Dustbowl: {}", tib01_count);
        assert!(
            tib01_count > 0,
            "LCW should find TIB01 ore cells on Dustbowl"
        );
    }
    if let Ok(data) = &lzo_result {
        let tib01_count = data
            .iter()
            .take(EXPECTED_SIZE)
            .filter(|&&b| b == 102)
            .count();
        println!("LZO TIB01 (id=102) cells on Dustbowl: {}", tib01_count);
    }
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn overlay_pack_first_bytes_identify_codec() {
    // LCW streams start with a command byte.
    // LZO1X streams: first chunk header is [u16 src_len][u16 dst_len], then
    // the LZO payload starts with a byte > 17 for a literal run, or 0x11 for EOS.
    // This test prints the first bytes of the chunk payload so we can identify
    // the codec from the stream structure alone.
    let ra2_dir_str = ra2_dir();
    let ra2_dir = Path::new(&ra2_dir_str);
    if !ra2_dir.exists() {
        println!("SKIP: RA2 dir not found");
        return;
    }

    let map_path = ra2_dir.join("Dustbowl.mmx");
    let map = vera20k::map::map_file::load_mmx(&map_path).expect("load map");
    let raw = load_raw_overlay_pack_from_map(&map);

    if raw.len() < 8 {
        println!("Too short");
        return;
    }

    // Both LCW and LZO use the same chunk framing: [u16 src][u16 dst][payload]
    let src_len = u16::from_le_bytes([raw[0], raw[1]]) as usize;
    let dst_len = u16::from_le_bytes([raw[2], raw[3]]) as usize;
    println!("Chunk 0: src_len={} dst_len={}", src_len, dst_len);

    if raw.len() >= 4 + src_len {
        let payload = &raw[4..4 + src_len];
        println!(
            "Payload first 32 bytes: {:02X?}",
            &payload[..32.min(payload.len())]
        );

        // LZO first byte interpretation:
        // If first byte > 17: literal run of (byte-17) bytes follows
        // If first byte == 17 (0x11): check next bytes for EOS (0x11 0x00 0x00)
        // If first byte <= 17: it's a match instruction on first byte (unusual)
        let first = payload[0];
        println!("\nFirst payload byte: 0x{:02X} ({})", first, first);
        if first > 17 {
            println!("LZO interpretation: literal run of {} bytes", first - 17);
        } else if first == 0x11 {
            println!("LZO interpretation: possible EOS marker");
        } else {
            println!("LZO interpretation: match instruction (unusual for first byte)");
        }

        // LCW first byte interpretation:
        // 0x80 = end marker (empty stream)
        // 0x81-0xBF = literal copy of (byte & 0x3F) bytes
        // 0xC0-0xFD = short absolute copy
        // 0xFE = RLE fill
        // 0xFF = large absolute copy
        // 0x00-0x7F = relative back-reference (needs 2nd byte)
        if first == 0x80 {
            println!("LCW interpretation: end-of-stream marker");
        } else if first & 0x80 != 0 && first & 0x40 == 0 {
            println!("LCW interpretation: literal copy of {} bytes", first & 0x3F);
        } else if first >= 0xC0 {
            println!("LCW interpretation: absolute copy command");
        } else {
            println!("LCW interpretation: relative back-reference (2-byte command)");
        }
    }
}
