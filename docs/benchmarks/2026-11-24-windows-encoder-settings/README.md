# Windows encoder settings: samples, checks and the device runs still due

A Windows host now asks for H.264 in a High profile with CABAC, with
Baseline as the fallback; sends an H.264 SPS that says nothing of picture
reordering rewritten to say none; and can set Media Foundation's quality
against speed, its display-remoting scenario and a higher GPU priority,
which a session leaves off
([H.265 and H.264](../../remote-desktop.md#h265-and-h264),
[Host: capture and encoding](../../remote-desktop.md#host-capture-and-encoding)).
The Windows encoder runs only on a Windows device, which this record's
machine is not: nothing here measures a picture, a bitrate or a time on
Windows. What ran here is the SPS rewriter's evidence, on a 12-core Apple
M6 with 24 GiB RAM, macOS 27.0 (26A425) and Rust 1.99.1.

## SPS samples

`codec/sps.rs` reads an H.264 SPS through its VUI and writes it back; its
tests need SPS that real encoders wrote and SPS in the shapes they did not.

- [`vt_sps_samples.swift`](vt_sps_samples.swift) encodes one picture with
  VideoToolbox's H.264 encoder in eight configurations (Constrained High
  with low-latency rate control at 1080p and 2160p, with and without BT.709
  tags; High, Main and Baseline without it, with frame reordering allowed or
  not) and prints the SPS and PPS of each:
  [`vt_sps_samples.txt`](vt_sps_samples.txt), built with
  `swiftc -O vt_sps_samples.swift` and run once. With low-latency rate
  control VideoToolbox states `max_num_reorder_frames` 0 (and
  `max_dec_frame_buffering` its reference frames); without it, its SPS has
  no VUI, or a VUI without `bitstream_restriction`. The "Constrained High"
  it was asked for comes out with no constraint flags set.
- [`sps_build.py`](sps_build.py) writes three SPS bit by bit, without the
  Rust writer: timing (whose 32-bit `num_units_in_tick` of 1 needs emulation
  prevention) and a NAL HRD in the VUI with a restriction allowing 2
  reordered frames; scaling matrices, picture order type 1 and field coding
  with MBAFF; 4:4:4 Predictive with a separate colour plane, 10-bit, and a
  VUI without a restriction: [`built.txt`](built.txt).
- [`sps_dump.py`](sps_dump.py) is an independent reader, and
  [`sps_expect.py`](sps_expect.py) builds each expected rewrite from the bit
  positions it finds: the bits up to the VUI flag or the restriction flag,
  then a restriction with `max_num_reorder_frames` 0 and
  `max_dec_frame_buffering` the reference frames (and, where there was
  none, a VUI with no other field and the restriction's other fields as
  VideoToolbox writes its own: `motion_vectors_over_pic_boundaries` 1, the
  motion-vector lengths 15, which an absent restriction infers too, and
  both size denominators 0, which states no bound where an absent
  restriction infers 2 and 1): [`expected.txt`](expected.txt). An SPS that
  already states 0 comes out as it went in, which `restrict_reorder` sends
  unchanged.

The tests take these bytes as they are. `cargo test -p pb-mapper-desktop
--lib codec::`: every sample and every rewrite reads whole and writes back
byte for byte; the fields read match `sps_dump.py`'s; and each rewrite
equals `expected.txt`'s. On a Mac, `core_media_takes_every_sps_rewrite`
(`platform/macos/tests.rs`) has Core Media build a format description
from each of the seven rewrites with its PPS
(`CMVideoFormatDescriptionCreateFromH264ParameterSets`) at the original's
dimensions. The 4:4:4 sample is left out of that last check, since its PPS
is VideoToolbox's 4:2:0 one. The PPS the scaling sample is paired with is
VideoToolbox's High PPS rewritten to name SPS 3.

## The Core Media check refuses a damaged SPS

[`cm_probe.swift`](cm_probe.swift) shows that check has teeth: Core Media
builds the 720p rewrite at 1280×720 and refuses the same SPS cut by one or
three bytes, a VUI flag with no VUI after it, and an SPS without a body
([`cm_probe.txt`](cm_probe.txt), status -12710 and -12714).

## Windows type-check

The Windows code compiles only in CI. As an extra check before it, the
desktop crate's library, tests and examples were linted for
`x86_64-pc-windows-msvc` from this Mac with Rust 1.100.0's Windows standard
library (`cargo +1.100.0 clippy --target x86_64-pc-windows-msvc -p
pb-mapper-desktop --all-features --all-targets -- -D warnings`, ring's C
compiled against stub headers and archived empty, which a type-check does
not link). It passed except for two lints Rust 1.100 adds that the code
before this change trips as well (`fetch_update` deprecated, and a
`needless_range_loop` in `worker/cursor.rs`). CI's Rust 1.99.1 on
`windows-latest` is the gate.

## Not measured here

Each needs a Windows device, per GPU vendor (NVIDIA, Intel, AMD where one
drives a display), in release mode, and each result goes in
[Encoder rate model per GPU](../../remote-desktop.md#encoder-rate-model-per-gpu):

- **Profile.** `cargo run --release -p pb-mapper-desktop --example video_bench
  -- 10 60 --codec h264 --h264-profile base --text --bitrate 4000000 --output
  h264.base --dump-source src.nv12`, the same with `--h264-profile high
  --output h264.high`, then `ffmpeg -f h264 -i h264.high -f rawvideo
  -pix_fmt nv12 -s WxH -i src.nv12 -lavfi "[0][1]psnr;[0][1]ssim" -f null -`
  for each (W×H from the first unit's `size`), with the IDR sizes, mean
  encode ms and dwell p50/p95; and `--pace 60,30,15,5` on the moving
  pattern with each profile, which must stay `nominal` within ±25%.
  `ffprobe` must show profile High and `has_b_frames=0`, and
  `ffmpeg -bsf:v trace_headers` the SPS VUI, before and after the rewrite:
  after in `--output`, which holds what the host sends, and before in
  `encoder_opened`'s `encoder_sps`, the encoder's own SPS in hex, written
  out behind a start code. `encoder_opened` gives the profile, CABAC,
  B-frame count and `sps_reorder` the encoder took; those SPS are each
  vendor's samples for `codec/sps.rs`'s tests.
- **Decoders.** The `--output` dumps decoded by a Windows viewer's Media
  Foundation decoder, VideoToolbox on a Mac, Android's MediaCodec and
  gst-libav, and a live session's `picture_latency_ms` and decoder timings
  on each viewer, Baseline against High.
- **Fallback.** On any GPU whose encoder refuses High, fails in it or
  reorders in it: `desktop_h264_high_failed` says what failed, the next
  encoder opens in Baseline, and the session keeps its picture, in the app
  and through the helper.
- **Quality against speed and scenario.** `--qvs 0`, `33`, `50`, `66` and
  `100`, each with and without `--scenario display-remoting`, for `--codec
  h264` and `--codec h265`, at the display's 1080p and at `--exact
  --max-width 3840 --max-height 2160`. A value becomes a vendor's default at
  a size only where PSNR at equal bitrate rises by 0.3 dB or more (or bytes
  at equal QP fall by 5% or more) while the median encode rises by 1 ms or
  less at 4K60, dwell p95 stays within a quarter of the frame period at the
  session's largest size and pace (4.2 ms at 4K60), and the rate model
  stays `nominal`.
- **GPU priority.** A GPU-saturating 3D load beside `video_bench 30 60
  --animate`, with and without `--gpu-priority`, from an elevated prompt
  and an unelevated one, hardware-accelerated GPU scheduling on and off,
  with `RUST_LOG=pb_mapper_desktop::session=info` for
  `desktop_gpu_priority`, which says whether the privilege was enabled,
  the device took the priority and the class was raised: dwell p50/p95,
  present-to-output, unique FPS and the load's own frame rate. A live
  session, in the app and through the helper, takes the session's
  `EncoderOptions::default()` (`NativeBackend`, `platform/windows/mod.rs`),
  so those arms need a build with `gpu_priority` set there; through the
  helper the capture worker's restricted token holds no privileges, so
  that arm tests the scheduling class alone. It goes on by default only if
  it lowers dwell p95 or present-to-output without lowering the load's
  frame rate by more than 5%.
