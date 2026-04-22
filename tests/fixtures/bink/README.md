# Bink fixtures

`fixture.bik` is a ≤1 MB excerpt (4 frames) of a Red Alert 2 cutscene, used
as a decoder oracle for integration tests. Produced with:

    ffmpeg -i <source>.bik -frames:v 4 -c:v copy fixture.bik

`fixture_frame0.yuv` is the first frame's raw YUV420P planes (width*height Y
bytes, then width/2 * height/2 U bytes, then same for V), produced by:

    ffmpeg -i fixture.bik -f rawvideo -pix_fmt yuv420p frames.yuv

then sliced to frame 0 (first y_size + 2*uv_size bytes).

`fixture_frames.yuv` is the full 4-frame YUV420P output from the command
above (all four frames back-to-back, Y then U then V per frame).

The fixture is derived from copyrighted RA2 / Westwood assets. This excerpt
is included only for automated decoder correctness testing. It is not
distributed as a playable asset; anyone running these tests must already own
a legal copy of Red Alert 2.

## Why not committed

The fixture files are NOT checked into the repository. Each developer must
produce them locally from their own RA2 install. The integration tests in
`tests/bink_first_frame.rs` and `tests/bink_frame_diff.rs` print a SKIP
message and pass without assertion when the fixtures are absent, so the
default `cargo test -p vera20k` run stays green.

## Producing a fixture

1. Extract any short Bink cutscene from a MOVIES mix (for example,
   `ALLIEND1.BIK` from `MOVIES01.MIX`) using `mix-browser` or any other MIX
   tool. Save as `source.bik`.
2. Trim to 4 frames and copy the video stream as-is:

        ffmpeg -i source.bik -frames:v 4 -c:v copy fixture.bik

3. Produce the oracle YUV:

        ffmpeg -i fixture.bik -f rawvideo -pix_fmt yuv420p fixture_frames.yuv

4. Slice frame 0 (its byte size is `width*height + 2*(width/2)*(height/2)`):

        # Example for 640x480: frame size = 640*480 + 2*320*240 = 460800
        dd if=fixture_frames.yuv of=fixture_frame0.yuv bs=460800 count=1

5. Place the three files in this directory. Re-run:

        cargo test -p vera20k --test bink_first_frame
        cargo test -p vera20k --test bink_frame_diff
