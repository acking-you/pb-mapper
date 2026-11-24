# Still refinement

The Windows host can refine a picture that stopped changing with a short
burst of repeats at the capture pace before the 250 ms liveness repeat
(`codec/refine.rs`; see "Host: capture and encoding" in
[remote-desktop.md](../../remote-desktop.md)). A session refines only on an
encoder that `REFINE_ENCODERS` lists, and none is listed: whether a Windows
encoder's repeats sharpen a still picture, and how fast, needs a Windows
GPU. This record has what ran here: the policy's tests, a type-check of
the Windows host, and a VideoToolbox measurement of why a Mac host has no
burst. It lists the device runs that decide each vendor.

Machine: Apple M6, 12 cores, 24 GiB, macOS 27.0 (26A425), Rust 1.99.1
(1.100.0 for the Windows type-check), with other builds running beside it
(load average 2.5 to 4).

## The policy, tested here

[`policy-tests.txt`](policy-tests.txt):
`cargo test -p pb-mapper-desktop --lib -- codec::refine codec::present_waiter
worker::tests::a_refinement worker::tests::an_armed`, on an injected clock:

- `codec::refine`: a burst starts only 200 ms after a fresh unit that
  filled three quarters of its frame's budget (none after a typed
  character's, none while disabled or at a pace of 4 or slower); small or
  flat-QP outputs before the third do not end it, two small ones in a row
  or two that did not lower the QP after it do, and so do a QP at the
  floor, 15 repeats or 1 s of them; a new picture or keyframe ends it, the
  outputs of an interrupted burst say nothing of the next, and liveness
  follows unchanged.
- `codec::present_waiter`: thread C's rule for when it needs the Windows
  encoder names a refinement repeat sooner than liveness, waits for the
  capture thread's look as any repeat does, and never asks to be called at
  once with nothing to do, burst or not.
- `worker::tests::a_refinement_burst_keeps_the_tick_armed_for_the_next_present`:
  thread C, with a fake armed capture thread whose encoder refines, sends
  the refinement repeats 200 ms after the picture a period apart, then the
  liveness repeat 250 ms after the last, and takes a present that lands
  1 ms after a refinement repeat at once. With the fake's refinement
  repeat spending the tick instead, the test fails at that last step.

## Windows type-check

As in the [capture conversion record](../2026-11-24-win-capture-convert/README.md),
the desktop crate's library, tests and examples (`video_bench` among them)
were linted for `x86_64-pc-windows-msvc` from this Mac with Rust 1.100.0's
Windows standard library and the stand-in C compiler and archiver kept
there ([`windows-check.txt`](windows-check.txt)). It passes with the one
lint Rust 1.100 adds that the code before this change trips as well
(`needless_range_loop` in `worker/cursor.rs`) allowed. CI's Rust 1.99.1 on
`windows-latest` is the gate.

## Why a Mac host has no burst: VideoToolbox

[`vt_still_refine.swift`](vt_still_refine.swift) sets VideoToolbox up as
the Mac host's encoder is (hardware, low-latency rate control, real time,
no reordering, an unbounded GOP, nominal 60 FPS, 4 Mbit/s with the 1.5 ×
over 1 s cap, wall-clock sample times), scrolls a 1080p page of glyph-like
text 4 rows a frame at 60 FPS for 1 s, stops, and submits the last picture
again for 2 s, as pb-mapper would schedule it: the liveness repeat every
250 ms from 250 ms, or a burst of 15 repeats 16.7 ms apart from 200 ms
(the cap, no stop rule) and then liveness. PSNR is VideoToolbox's own luma
mean squared error against its input. `swiftc -O vt_still_refine.swift -o
vt_still_refine && ./vt_still_refine 3`; raw output in
[`vt_still_refine.txt`](vt_still_refine.txt).

H.264, three trials an arm (PSNR the viewer shows at each time: the last
unit submitted by then; the scroll's last unit before the first repeat):

| Arm | Trial | Last scroll unit | 250 ms | 500 ms | 1000 ms | Best | Within 0.5 dB of best | Repeat bytes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| liveness | 1 | 35.4 dB | 35.4 | 37.3 | 44.8 | 49.1 | 1003 ms | 323 304 |
| liveness | 2 | 35.9 dB | 35.9 | 39.0 | 46.6 | 49.1 | 1001 ms | 313 506 |
| liveness | 3 | 35.9 dB | 35.9 | 36.1 | 42.9 | 49.0 | 1260 ms | 318 457 |
| burst | 1 | 35.8 dB | 41.1 | 43.2 | 48.6 | 49.0 | 943 ms | 328 462 |
| burst | 2 | 34.9 dB | 39.2 | 41.2 | 47.6 | 48.9 | 1193 ms | 355 384 |
| burst | 3 | 37.2 dB | 41.9 | 42.5 | 45.8 | 49.0 | 1189 ms | 293 241 |

VideoToolbox gives a repeat the bits of the time since the unit before.
The burst's first three repeats, 200 to 240 ms after the stop, took 108 to
125 KB together, what had accrued since the stop, and raised the picture
4.3 to 5.3 dB. Of the twelve after them most came out at 84 to 550 bytes,
with a few of 2.8 to 45 KB as bits accrued again, and they added 0.6 to
2.1 dB in all. So the burst sharpened the picture sooner (39.2 to 41.9 dB
at 250 ms, where the liveness arm still showed the scroll's last unit,
and 41.2 to 43.2 dB at 500 ms against 36.1 to 39.0), but the picture came
within 0.5 dB of the 48.9 to 49.1 dB it converged to no sooner: 0.94 to
1.19 s against 1.00 to 1.26 s, for about the same bytes. Applied to these
sizes, the policy's stop (two outputs in a row under 512 bytes, after the
third) would have ended each burst at its seventh repeat.

H.265 coded this page at 51.8 to 52.2 dB in the scroll's last unit in
every trial; neither arm's repeats (118 to 133 bytes each) moved it by
more than 0.1 dB. VideoToolbox's low-latency rate control dropped 18 of
the 60 scroll pictures in every H.264 trial and 16 in every H.265 one.

A Mac host therefore keeps the liveness repeat alone: on this content a
burst moved the first refinement step earlier, by about one liveness
interval, but not the convergence. Whether that earlier step is worth a
burst on the Mac, and its low-latency drops and a final picture it drops,
are the macOS lane's (`mac-vt-rate-control`).

## Not measured here

Each needs a Windows device, in release mode, and goes in a run of its
own under `docs/benchmarks/`. NVIDIA (whose MFTs are measured `nominal`, a
budget per frame) first, then AMD and Intel:

- **Does a burst sharpen sooner?** At 1080p and 4 Mbit/s, for H.264 and
  then H.265 (`--codec h265`): `cargo run --release -p pb-mapper-desktop
  --example video_bench -- 6 60 --text --freeze 2000 --bitrate 4000000
  --max-width 1920 --max-height 1080 --refine off --output off.h264
  --dump-source off.nv12`, and the same with `--refine on` (`on.h264`,
  `on.nv12`). Decode each output, and take ffmpeg's PSNR of each unit
  after the freeze (`still_unit` lines: the unit's index in the output,
  ms since the freeze, size, QP) against the source picture of the last
  non-duplicate unit; then the time from the freeze to within 0.5 dB (and
  1 dB) of the picture the run converges to, each way, the units and
  bytes the burst spent (`still`, `refined`), and the QPs. An encoder joins
  `REFINE_ENCODERS` only where the burst brings that time down, its
  repeats fall under 512 bytes once the picture converged (an encoder that
  pads its constant bitrate never does), and its repeats raise PSNR at
  all; the same runs set `REFINE_QP_FLOOR` (and show whether the encoder
  sets `MFSampleExtension_VideoEncodeQP` at all, and in its low 16 bits),
  `REFINE_MAX`, `REFINE_MAX_TIME` and `REFINE_AFTER`.
- **The rate model stays `nominal`.** `video_bench --pace 60,30,15,5
  --refine on`, and a Smooth session's `rate_model` with refinement on:
  no `desktop_rate_model_latched`.
- **Typing pays nothing.** In a session with refinement on (a build with
  the encoder listed), characters typed every 250 to 400 ms on a still
  page: no burst after any of them (`refined` stays 0 in the encoder's
  statistics), and the present-to-submit p95 of their echoes as without
  refinement. If a typed echo waits behind a refinement repeat for more
  than 1 ms on average, a soft tick (a fresh picture may follow a
  refinement repeat a quarter period later instead of three quarters) is
  a follow-up of the next pacer change.
- **Behind the helper**, the background service's capture worker, which
  takes a session's settings (so `video_bench --backend` cannot force a
  burst): with a build that lists the encoder, a viewer of the service
  host watching a scrolled page stop sees it sharpen as in process, and
  the quality panel's `presented_fps` rise for a moment after the stop.
