//! Integration test: decode all frames in the Bink fixture and compare
//! against FFmpeg's multi-frame YUV oracle. Covers INTER / MOTION / RESIDUE
//! paths once the first frame has been decoded.
//!
//! The fixture files are not committed — see `tests/fixtures/bink/README.md`
//! for the production recipe. When the fixture is absent the test prints a
//! SKIP message and passes, so default `cargo test` stays green.

use vera20k::assets::bink_decode::BinkDecoder;
use vera20k::assets::bink_file::BinkFile;

const BIK_PATH: &str = "tests/fixtures/bink/fixture.bik";
const YUV_PATH: &str = "tests/fixtures/bink/fixture_frames.yuv";

#[test]
fn decodes_all_fixture_frames_bit_exact() {
    let bik_bytes = match std::fs::read(BIK_PATH) {
        Ok(b) => b,
        Err(_) => {
            eprintln!(
                "SKIP: {} missing — see tests/fixtures/bink/README.md",
                BIK_PATH
            );
            return;
        }
    };
    let oracle = match std::fs::read(YUV_PATH) {
        Ok(b) => b,
        Err(_) => {
            eprintln!(
                "SKIP: {} missing — see tests/fixtures/bink/README.md",
                YUV_PATH
            );
            return;
        }
    };

    let file = BinkFile::parse_from_slice(&bik_bytes).unwrap();
    let mut decoder = BinkDecoder::new(&file.header).unwrap();

    let w = decoder.width() as usize;
    let h = decoder.height() as usize;
    let y_size = w * h;
    let uv_size = (w / 2) * (h / 2);
    let frame_bytes = y_size + 2 * uv_size;
    let expected_frames = file.frame_index.len().min(oracle.len() / frame_bytes);

    for i in 0..expected_frames {
        let pkt = file.video_packet(i).unwrap();
        decoder.decode_frame(pkt).unwrap();

        let base = i * frame_bytes;
        let oracle_y = &oracle[base..base + y_size];
        let oracle_u = &oracle[base + y_size..base + y_size + uv_size];
        let oracle_v = &oracle[base + y_size + uv_size..base + frame_bytes];

        for row in 0..h {
            let got =
                &decoder.cur.y[row * decoder.cur.stride_y..row * decoder.cur.stride_y + w];
            let want = &oracle_y[row * w..row * w + w];
            if got != want {
                let col = (0..w).find(|&c| got[c] != want[c]).unwrap();
                panic!(
                    "Y mismatch at frame {} row {} col {}: got {} want {}",
                    i, row, col, got[col], want[col]
                );
            }
        }
        for row in 0..h / 2 {
            let got_u = &decoder.cur.u
                [row * decoder.cur.stride_uv..row * decoder.cur.stride_uv + w / 2];
            let want_u = &oracle_u[row * w / 2..row * w / 2 + w / 2];
            assert_eq!(got_u, want_u, "U mismatch at frame {} row {}", i, row);
            let got_v = &decoder.cur.v
                [row * decoder.cur.stride_uv..row * decoder.cur.stride_uv + w / 2];
            let want_v = &oracle_v[row * w / 2..row * w / 2 + w / 2];
            assert_eq!(got_v, want_v, "V mismatch at frame {} row {}", i, row);
        }
    }
}
