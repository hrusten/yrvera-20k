//! Debug test to dump ra2.mix entry IDs and compare against computed CRC-32 hashes.
//! Run with: cargo test --test debug_mix_hashes -- --nocapture

// We'll inline the hash computation and MIX parsing logic here
// to avoid issues with module visibility.

fn crc32(data: &[u8]) -> u32 {
    const CRC32_POLYNOMIAL: u32 = 0xEDB88320;
    let mut table: [u32; 256] = [0u32; 256];
    for i in 0..256 {
        let mut crc: u32 = i as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ CRC32_POLYNOMIAL;
            } else {
                crc >>= 1;
            }
        }
        table[i] = crc;
    }
    let mut crc: u32 = 0xFFFFFFFF;
    for &byte in data {
        let index: usize = ((crc ^ byte as u32) & 0xFF) as usize;
        crc = (crc >> 8) ^ table[index];
    }
    crc ^ 0xFFFFFFFF
}

fn mix_hash_crc32(name: &str) -> i32 {
    let upper: Vec<u8> = name.bytes().map(|b| b.to_ascii_uppercase()).collect();
    crc32(&upper) as i32
}

fn westwood_hash(name: &str) -> i32 {
    let upper: Vec<u8> = name.bytes().map(|b| b.to_ascii_uppercase()).collect();
    let len: usize = upper.len();
    let mut a: u32 = 0;
    let mut i: usize = 0;
    while i < len {
        let mut buffer: u32 = 0;
        for _j in 0..4 {
            buffer >>= 8;
            if i < len {
                buffer = buffer.wrapping_add((upper[i] as u32) << 24);
                i += 1;
            }
        }
        a = a.rotate_left(1).wrapping_add(buffer);
    }
    a as i32
}

fn read_u16_le(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn read_i32_le(data: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn ra2_dir() -> String {
    std::env::var("RA2_DIR")
        .unwrap_or_else(|_| panic!("Set RA2_DIR env var to your RA2/YR install directory"))
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn debug_dump_ra2_mix_ids() {
    let ra2_dir_path = std::path::PathBuf::from(ra2_dir());
    let ra2_mix_path = ra2_dir_path.join("ra2.mix");

    if !ra2_mix_path.exists() {
        println!("SKIP: ra2.mix not found at {:?}", ra2_mix_path);
        return;
    }

    let data = std::fs::read(&ra2_mix_path).expect("read ra2.mix");
    println!(
        "ra2.mix size: {} bytes ({:.1} MB)",
        data.len(),
        data.len() as f64 / 1048576.0
    );
    println!("First 32 bytes: {:02X?}", &data[0..32]);

    let first_word = read_u16_le(&data, 0);
    let flags = read_u16_le(&data, 2);
    println!("first_word=0x{:04X} flags=0x{:04X}", first_word, flags);
    println!(
        "Encrypted={} Checksum={}",
        (flags & 0x0002) != 0,
        (flags & 0x0001) != 0
    );

    // Load via the project's MixArchive to use the actual decryption
    // We can't easily call into the crate from an integration test without
    // the crate being a library. Let's just try parsing it.
    // Actually, the crate likely builds as a binary. Let me try importing.
}

#[test]
#[ignore] // Requires RA2_DIR (retail game files)
fn debug_compare_hash_algorithms() {
    // Known filenames expected inside ra2.mix
    let names = [
        "local.mix",
        "conquer.mix",
        "cache.mix",
        "isosnow.mix",
        "isotem.mix",
        "isourb.mix",
        "snow.mix",
        "temperat.mix",
        "urban.mix",
        "generic.mix",
        "isogen.mix",
        "cameo.mix",
        "conqmd.mix",
        "genermd.mix",
        "isogenmd.mix",
        "isosnowmd.mix",
        "isourmd.mix",
        "isotemmd.mix",
        "mousemd.mix",
        "langmd.mix",
        "rules.ini",
        "art.ini",
        "ai.ini",
        "sound.ini",
        "eva.ini",
        "theme.ini",
        "battle.ini",
    ];

    println!("\n{:>20} {:>12} {:>12}", "FILENAME", "CRC-32", "WESTWOOD");
    println!("{}", "-".repeat(50));
    for name in &names {
        let crc = mix_hash_crc32(name);
        let ww = westwood_hash(name);
        println!("{:>20} 0x{:08X} 0x{:08X}", name, crc as u32, ww as u32,);
    }
}
