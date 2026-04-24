//! Integration test: decode frame 0 of a real Bink fixture and compare
//! against FFmpeg's YUV oracle byte-for-byte.
//!
//! The fixture files are not committed — see `tests/fixtures/bink/README.md`
//! for the production recipe. When the fixture is absent the test prints a
//! SKIP message and passes, so default `cargo test` stays green.

use vera20k::assets::bink_decode::BinkDecoder;
use vera20k::assets::bink_file::BinkFile;

const BIK_PATH: &str = "tests/fixtures/bink/fixture.bik";
const YUV_PATH: &str = "tests/fixtures/bink/fixture_frame0.yuv";

#[test]
fn decodes_fixture_frame_0_bit_exact() {
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

    let file = BinkFile::parse_from_slice(&bik_bytes).expect("parse fixture");
    let mut decoder = BinkDecoder::new(&file.header).expect("decoder init");

    let pkt = file.video_packet(0).expect("frame 0");
    decoder.decode_frame(pkt).expect("decode frame 0");

    let w = decoder.width() as usize;
    let h = decoder.height() as usize;
    let y_size = w * h;
    let uv_size = (w / 2) * (h / 2);
    assert_eq!(oracle.len(), y_size + 2 * uv_size, "oracle YUV size mismatch");

    let oracle_y = &oracle[..y_size];
    let oracle_u = &oracle[y_size..y_size + uv_size];
    let oracle_v = &oracle[y_size + uv_size..];

    for row in 0..h {
        assert_eq!(
            &decoder.cur.y[row * decoder.cur.stride_y..row * decoder.cur.stride_y + w],
            &oracle_y[row * w..row * w + w],
            "Y plane mismatch at row {}",
            row
        );
    }
    for row in 0..h / 2 {
        assert_eq!(
            &decoder.cur.u
                [row * decoder.cur.stride_uv..row * decoder.cur.stride_uv + w / 2],
            &oracle_u[row * w / 2..row * w / 2 + w / 2],
            "U plane mismatch at row {}",
            row
        );
        assert_eq!(
            &decoder.cur.v
                [row * decoder.cur.stride_uv..row * decoder.cur.stride_uv + w / 2],
            &oracle_v[row * w / 2..row * w / 2 + w / 2],
            "V plane mismatch at row {}",
            row
        );
    }
}
