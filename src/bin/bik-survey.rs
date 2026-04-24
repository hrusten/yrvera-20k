//! Headless Bink decoder survey.
//!
//! Iterates every .bik asset in the RA2 MIX archives (or a single file),
//! tries to decode every frame, and reports: frame count, keyframe layout,
//! first decode error (if any). Usage:
//!   bik-survey                      — iterate all assets, one-line summary
//!   bik-survey <substring>          — filter assets by substring
//!   bik-survey <name> --focus       — decode `name`, on failure dump packet
//!                                     hex + bitstream stats around the
//!                                     failing frame

use std::sync::Arc;

use vera20k::assets::asset_manager::AssetManager;
use vera20k::assets::bink_decode::BinkDecoder;
use vera20k::assets::bink_file::BinkFile;
use vera20k::assets::xcc_database::XccDatabase;
use vera20k::util::config::GameConfig;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let cfg = match GameConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config.toml load failed: {}", e);
            return;
        }
    };
    let mgr = match AssetManager::new(&cfg.paths.ra2_dir) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("AssetManager::new failed: {}", e);
            return;
        }
    };

    let xcc = match XccDatabase::load_from_disk() {
        Ok(x) => x,
        Err(e) => {
            eprintln!("XccDatabase::load_from_disk failed: {}", e);
            return;
        }
    };

    let mut names: Vec<String> = xcc
        .by_extension(".bik")
        .into_iter()
        .map(|e| e.filename.clone())
        .filter(|n| mgr.get_ref(n).is_some())
        .collect();
    names.sort_unstable();
    names.dedup();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let focus = args.iter().any(|a| a == "--focus");
    let avsync = args.iter().any(|a| a == "--avsync");
    let archives = args.iter().any(|a| a == "--archives");
    let filter: Option<&String> = args.iter().find(|a| !a.starts_with("--"));

    if archives {
        report_archives(&mgr, &xcc);
        return;
    }

    let mut failed = 0usize;
    let mut ok = 0usize;

    for name in &names {
        if let Some(f) = filter {
            if !name.to_ascii_lowercase().contains(&f.to_ascii_lowercase()) {
                continue;
            }
        }
        let bytes = match mgr.get_ref(name) {
            Some(b) => Arc::<[u8]>::from(b),
            None => continue,
        };
        let file = match BinkFile::parse(bytes) {
            Ok(f) => f,
            Err(e) => {
                println!("[PARSE-FAIL] {}: {}", name, e);
                failed += 1;
                continue;
            }
        };
        let mut decoder = match BinkDecoder::new(&file.header) {
            Ok(d) => d,
            Err(e) => {
                println!("[INIT-FAIL] {}: {}", name, e);
                failed += 1;
                continue;
            }
        };

        let n = file.frame_index.len();
        let kf_indices: Vec<usize> = file
            .frame_index
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_keyframe)
            .map(|(i, _)| i)
            .collect();
        let kf_summary = if kf_indices.len() <= 8 {
            format!("{:?}", kf_indices)
        } else {
            format!(
                "first 8: {:?} ... ({} total)",
                &kf_indices[..8],
                kf_indices.len()
            )
        };

        if avsync {
            avsync_report(name, &file);
            continue;
        }

        let mut first_fail: Option<(usize, usize, String)> = None;
        let mut max_packet = 0usize;
        for i in 0..n {
            let pkt = match file.video_packet(i) {
                Ok(p) => p,
                Err(e) => {
                    first_fail = Some((i, 0, format!("video_packet: {}", e)));
                    break;
                }
            };
            max_packet = max_packet.max(pkt.len());
            if let Err(e) = decoder.decode_frame(pkt) {
                first_fail = Some((i, pkt.len(), format!("decode_frame: {}", e)));
                break;
            }
        }

        match first_fail {
            Some((fi, pkt_len, why)) => {
                println!(
                    "[FAIL @ {:>4}/{:>4}] {}  {}x{}  kf={}  largest={}  maxpkt={}  reason: {}  failed_pkt_len={}",
                    fi,
                    n,
                    name,
                    file.header.width,
                    file.header.height,
                    kf_summary,
                    file.header.largest_frame,
                    max_packet,
                    why,
                    pkt_len,
                );
                failed += 1;

                if focus {
                    dump_failing_packet(&file, fi);
                }
            }
            None => {
                println!(
                    "[ OK      /{:>4}] {}  {}x{}  kf={}  largest={}  maxpkt={}",
                    n,
                    name,
                    file.header.width,
                    file.header.height,
                    kf_summary,
                    file.header.largest_frame,
                    max_packet,
                );
                ok += 1;
            }
        }
    }
    println!("\n== {} ok, {} failed ==", ok, failed);
}

/// Walk every loaded MIX archive. For each, list .bik files detected by
/// magic bytes (BIKi/BIKk) and report whether the current picker logic can
/// identify them by name via the XCC database.
fn report_archives(mgr: &AssetManager, xcc: &XccDatabase) {
    use std::collections::HashSet;
    use vera20k::assets::mix_hash::{mix_hash, westwood_hash};
    let known_bik_crc: HashSet<i32> = xcc
        .by_extension(".bik")
        .into_iter()
        .map(|e| mix_hash(&e.filename))
        .collect();
    let known_bik_ww: HashSet<i32> = xcc
        .by_extension(".bik")
        .into_iter()
        .map(|e| westwood_hash(&e.filename))
        .collect();
    let mut total_bik_by_magic = 0usize;
    let mut total_unresolved_bik = 0usize;
    mgr.visit_archives(|name, archive| {
        let mut n_entries = 0usize;
        let mut n_bik_known = 0usize;
        let mut n_bik_magic = 0usize;
        let mut unresolved_hashes: Vec<i32> = Vec::new();
        for entry in archive.entries() {
            n_entries += 1;
            let data = match archive.get_by_id(entry.id) {
                Some(d) => d,
                None => continue,
            };
            if data.len() >= 3 && (&data[..3] == b"BIK" || &data[..3] == b"KB2") {
                n_bik_magic += 1;
                if known_bik_crc.contains(&entry.id) || known_bik_ww.contains(&entry.id) {
                    n_bik_known += 1;
                } else {
                    unresolved_hashes.push(entry.id);
                }
            }
        }
        if n_bik_magic == 0 {
            return;
        }
        total_bik_by_magic += n_bik_magic;
        total_unresolved_bik += unresolved_hashes.len();
        println!(
            "archive {:<32} entries={:<6} .bik(magic)={:<4} .bik(XCC-known)={:<4} unresolved={}",
            name,
            n_entries,
            n_bik_magic,
            n_bik_known,
            unresolved_hashes.len(),
        );
        for h in unresolved_hashes.iter().take(16) {
            println!("  unresolved hash 0x{:08X}", *h as u32);
        }
        if unresolved_hashes.len() > 16 {
            println!("  ... {} more", unresolved_hashes.len() - 16);
        }
    });
    println!(
        "\n== total .bik in archives: {} ({} unresolved by XCC) ==",
        total_bik_by_magic, total_unresolved_bik,
    );

    // Cross-check: what XCC thinks exists for .bik, and what mgr can resolve.
    let xcc_biks: Vec<&str> = xcc
        .by_extension(".bik")
        .into_iter()
        .map(|e| e.filename.as_str())
        .collect();
    let mut resolvable: Vec<&str> = xcc_biks
        .iter()
        .copied()
        .filter(|n| mgr.get_ref(n).is_some())
        .collect();
    resolvable.sort_unstable();
    resolvable.dedup();
    println!(
        "XCC .bik entries: {}   resolvable via AssetManager: {} (unique names)",
        xcc_biks.len(),
        resolvable.len(),
    );
    // List XCC .bik names that the archive walk found but the picker's
    // resolve path misses — these are candidates for naming-convention bugs
    // (e.g., XCC entry in uppercase, archive keyed in lowercase, or vice versa).
    let resolvable_ci: HashSet<String> =
        resolvable.iter().map(|s| s.to_ascii_lowercase()).collect();
    let mut xcc_unresolvable: Vec<&str> = xcc_biks
        .iter()
        .copied()
        .filter(|n| !resolvable_ci.contains(&n.to_ascii_lowercase()))
        .collect();
    xcc_unresolvable.sort_unstable();
    xcc_unresolvable.dedup();
    println!(
        "XCC .bik entries NOT resolved by mgr.get_ref(): {}",
        xcc_unresolvable.len(),
    );
    for n in xcc_unresolvable.iter().take(40) {
        println!("  {}", n);
    }
    if xcc_unresolvable.len() > 40 {
        println!("  ... {} more", xcc_unresolvable.len() - 40);
    }

    // Where does each picker entry actually resolve? This exposes whether
    // RA2 MOVIES*.MIX is shadowing YR's movmd03.mix due to search order.
    use std::collections::BTreeMap;
    let mut by_source: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in &resolvable {
        if let Some((_, src)) = mgr.get_with_source_ref(name) {
            by_source
                .entry(src.to_string())
                .or_default()
                .push((*name).to_string());
        }
    }
    println!("\nResolved-from breakdown (where mgr.get_ref serves each picker name):");
    for (src, names) in &by_source {
        println!("  {:<32} {} files", src, names.len());
    }
}

/// For A/V drift investigation: decode one track's audio packet-by-packet,
/// sum the sample counts, and compare the audio timeline against the video
/// timeline implied by fps. Reports what each packet's payload says about
/// preload/drift.
fn avsync_report(name: &str, file: &BinkFile) {
    use vera20k::assets::bink_audio::BinkAudioDecoder;

    let fps = file.header.fps();
    let frame_dt = 1.0 / fps;
    let track = match file.header.audio_tracks.first().copied() {
        Some(t) => t,
        None => {
            println!("[AV] {}  no audio track", name);
            return;
        }
    };
    let mut adec = match BinkAudioDecoder::new(track) {
        Ok(a) => a,
        Err(e) => {
            println!("[AV] {}  audio init failed: {}", name, e);
            return;
        }
    };
    let sample_rate = adec.sample_rate() as f64;
    let channels = adec.channels() as f64;

    let n = file.frame_index.len().min(200);
    let mut cum_samples: u64 = 0;
    let mut declared_samples: u64 = 0;
    println!(
        "[AV] {}  {}x{} @ {:.3} fps  track: {}Hz {}ch  dct={}  {} audio frames inspected",
        name,
        file.header.width,
        file.header.height,
        fps,
        track.sample_rate,
        if track.is_stereo() { "stereo" } else { "mono" },
        track.uses_dct(),
        n,
    );
    for i in 0..n {
        let pkts = match file.audio_packets(i) {
            Ok(p) => p,
            Err(e) => {
                println!("  frame {}: audio_packets err: {}", i, e);
                break;
            }
        };
        for ap in pkts {
            if ap.track_index != 0 {
                continue;
            }
            declared_samples += ap.sample_count as u64;
            let before = cum_samples;
            match adec.decode_packet(ap.bytes) {
                Ok(samples) => {
                    cum_samples += (samples.len() as u64) / channels as u64;
                }
                Err(e) => {
                    println!("  frame {}: decode err: {}", i, e);
                    break;
                }
            }
            let decoded_ms = (cum_samples as f64 * 1000.0) / sample_rate;
            let video_ms = (i + 1) as f64 * frame_dt * 1000.0;
            let declared_ms = (declared_samples as f64 * 1000.0) / sample_rate;
            if i < 10 || i % 20 == 0 || i == n - 1 {
                println!(
                    "  frame {:>3}: pkt_bytes={:>6}  declared_samples={:>6}  decoded_delta={:>5}  cum_decoded_ms={:>7.1}  cum_declared_ms={:>7.1}  video_ms={:>7.1}  decoded_lead={:>+6.1}ms  declared_lead={:>+6.1}ms",
                    i,
                    ap.bytes.len(),
                    ap.sample_count,
                    (cum_samples - before),
                    decoded_ms,
                    declared_ms,
                    video_ms,
                    decoded_ms - video_ms,
                    declared_ms - video_ms,
                );
            }
        }
    }
}

/// Dump the bytes of the failing packet + the few around it. For finding
/// whether the failure is content-dependent on the actual packet bytes, or
/// inherited from the decoder state established by frames 0..fi-1.
fn dump_failing_packet(file: &BinkFile, fi: usize) {
    println!("\n  --- failing frame {} ---", fi);
    for delta in 0..=0 {
        if fi + delta >= file.frame_index.len() {
            break;
        }
        let idx = fi + delta;
        let pkt = match file.video_packet(idx) {
            Ok(p) => p,
            Err(e) => {
                println!("  frame {}: packet unavailable: {}", idx, e);
                continue;
            }
        };
        println!(
            "  frame {}: pkt_len={} first 32 bytes: {}",
            idx,
            pkt.len(),
            pkt.iter()
                .take(32)
                .map(|b| format!("{:02X}", b))
                .collect::<Vec<_>>()
                .join(" "),
        );
        println!(
            "  frame {}: last 32 bytes:               {}",
            idx,
            pkt.iter()
                .rev()
                .take(32)
                .rev()
                .map(|b| format!("{:02X}", b))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
}
