# Windows capture conversion: checks here and the device runs still due

A Windows host now converts each new picture straight from the Desktop
Duplication surface into the encoder's NV12 sample, repeats a still
picture by copying the last sample, shares a rotated display upright,
crops a desktop one pixel over the even encoded size instead of scaling
it, and can ask the video processor for quality
([Host: capture and encoding](../../remote-desktop.md#host-capture-and-encoding)).
The Windows code runs only on a Windows device, which this record's
machine is not: nothing here measures a picture or a time on Windows.
What ran here, on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425)
and Rust 1.99.1, is below.

## The geometry, tested on every platform

`scale.rs` holds what the converter is set up from: `crop_only`,
`downscales`, the quarter turn's `source_rect`, and `scanned_out`, the
size a turned display's pictures are captured at given its display mode
and its desktop rectangle.
`cargo test -p pb-mapper-desktop --all-features --lib scale::`
([`scale-tests.txt`](scale-tests.txt)) passes. The size test reads a
quarter-turned display's mode upright, the desktop rectangle's size, as
WebRTC's Desktop Duplication capturer requires, so that a 1080×1920
portrait mode is captured at 1920×1080, and reads it as scanned out only
where that alone matches the desktop. With `scanned_out` returning the
mode as given, as the first version of this change read every mode, it
fails (`left: (1080, 1920)`, `right: (1920, 1080)`). The region test walks
every captured pixel of small odd and even pictures under each turn,
keeping the whole upright picture or one column, row or both less, and
checks that a pixel is inside the source rectangle exactly when the turn
lands it in the kept top-left corner. With `Cw90`'s rectangle replaced by
the one that keeps the captured picture's top-left corner instead, which
leaves out the upright picture's left column rather than its right, the
test fails (`Cw90 (5, 3) keeping (2, 5): captured (0, 0) lands at (2, 0)`).

## Windows type-check

As in the [encoder settings record](../2026-11-24-windows-encoder-settings/README.md),
the desktop crate's library, tests (the converter's GPU test among them)
and examples were linted for
`x86_64-pc-windows-msvc` from this Mac with Rust 1.100.0's Windows
standard library: `cargo +1.100.0 clippy --target x86_64-pc-windows-msvc
-p pb-mapper-desktop --all-features --all-targets -- -D warnings`, with
`RUSTC_WRAPPER` empty and ring's C handed to the stand-in compiler and
archiver kept here ([`fakecc`](fakecc), [`fakear`](fakear)), which write
empty objects and archives that a type-check never links
([`windows-check.txt`](windows-check.txt)). It reports only the two lints
Rust 1.100 adds that the code before this change trips as well
(`fetch_update` deprecated in `video/codec.rs`, `needless_range_loop` in
`worker/cursor.rs`); with those two allowed it passes. `cargo +1.100.0 doc`
for the same target with `RUSTDOCFLAGS="-D warnings"` passes too. CI's
Rust 1.99.1 on `windows-latest` is the gate.

## Not measured here

Each needs a Windows device, in release mode
(`cargo run --release -p pb-mapper-desktop --example video_bench -- ...`),
and each result goes in a run of its own under `docs/benchmarks/`:

- **The video processor's geometry**, on any Windows machine with a GPU,
  no rotated display needed: `cargo test -p pb-mapper-desktop
  --all-features --lib converter -- --ignored --nocapture`. On each
  hardware adapter, at speed and at quality, it converts a synthetic
  picture of four grey quadrants under each turn, cropped one column and
  one row (every pixel checked, the left-out column and row a fifth grey)
  and halved (each quadrant's middle checked); it prints a line per case
  and the processors that lack the rotation cap. It type-checks here; it
  has not run.
- **Direct conversion.** At 4K captured and 1080p encoded
  (`--max-width 1920 --max-height 1080`) and at 1440p native, on a
  discrete GPU and an Intel iGPU: `30 60 --animate --gpu-time` against the
  same with `--copy-capture`, the copy every picture took before, in the
  same build. Compare `gpu_time`'s `convert_p50_us` and `convert_p95_us`,
  `case=encoder`'s `submit_p50_us`, and `dwell_p50_ms`/`dwell_p95_ms`; the
  `capture` line must say `direct` for the first arm, and
  `desktop_capture_copied` must not appear in its log
  (`RUST_LOG=pb_mapper_desktop::session=info`). Where the video processor
  runs on the video engine, check the span against GPUView or PIX. Then a
  still desktop (`10 60 --text --gpu-time`): `repeats`, `repeat_p50_us` and
  `gpu_time`'s `repeat_p50_us` against the converted pictures', and that
  the `--output` stream's repeats decode to the same picture
  (`--dump-source` shows what each was given).
- **Rotation.** With the display turned to 90°, 180° and 270° in turn:
  `10 60 --animate --dump-source src.nv12` and a real session. The first
  picture of `src.nv12`
  (`ffmpeg -f rawvideo -pix_fmt nv12 -s WxH -i src.nv12 -frames:v 1 f.png`,
  W×H from `first_access_unit`'s `size`) must be upright, matching what
  the display shows, with `desktop_video_encoder_opened`'s `rotation` the
  angle. Record that line's `mode` and `captured`: at 90° and 270° a
  portrait display's mode is expected upright (1080x1920) and its capture
  landscape (1920x1080); a first frame that fails with "the desktop image
  is ..., not the ... its display mode and rotation give" says the reading
  is wrong on that driver. In the session a click on a corner and on the
  pattern window must land where aimed. Compare `gpu_time`'s
  `convert_p50_us` with an unrotated run. A GPU whose video processor
  lacks the rotation cap must fail with the error that says so, with no
  `desktop_video_encoder_opened` before it.
- **Odd size.** A VM or a custom resolution of 1365×767:
  `10 60 --text --dump-source src.nv12` under the default bounds, before
  this change and after (`desktop_video_encoder_opened` says `cropped`).
  PSNR of the dumped source against a screenshot of the same text cropped
  to 1364×766 and converted to NV12 with the same matrix (BT.709, studio
  range): after, the text must match up to the conversion; before, the
  0.999 scaling blurs it. In a session, a click a pixel inside the right
  edge lands one pixel further right in the right half of the picture,
  as the docs say.
- **Quality scaling.** On NVIDIA, Intel and AMD: `10 60 --text
  --dump-source src.nv12 --gpu-time` at 4K→1080p and 1440p→1080p, Fit and
  `--exact`, with `--vp-usage quality` against `--vp-usage speed`. PSNR
  and SSIM of each dump against an area (or Lanczos) downscale of a
  screenshot of the same text, with `gpu_time`'s `convert_p50_us`. A
  vendor joins `QUALITY_DOWNSCALE_VENDORS` only where quality is sharper
  and its conversion stays under 1 ms.
