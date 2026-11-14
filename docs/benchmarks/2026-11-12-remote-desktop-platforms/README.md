# Remote desktop on every platform: the data paths before and after, 2026-11-12

The change that brings native video to the macOS, iOS and Linux viewers,
hosting to macOS, pointer shapes and the two keyboard modes touches the
remote desktop's data paths:

- thread C's 16 ms pointer tick, beside video, now asks the backend for the
  pointer's shape when the session negotiated `cursor_shape`;
- `HostMessage` gains the `CursorShape` record and `Cursor.shape`, and B-send
  keeps the slot table;
- the viewer's read loop reads the shape's payload;
- the input path gains `Input::PhysicalKey`, and text is cut into runs with
  one Enter or Tab per line break or tab;
- the Windows encoder's late, stall and liveness rules moved to
  `codec/pipeline.rs`, shared with macOS.

This record measures those paths before and after, on the benches the
2026-11-11 records use. Only measured figures are below. The raw runs are
the `platforms-*.txt` files next to this one. In each, every run's result
lines follow a header that names its side and round, in run order.

## Machine

| | |
|---|---|
| CPU | AMD Ryzen 9 9950X, 16 cores / 32 threads |
| RAM | 61.6 GiB |
| OS | Windows 11 Enterprise 10.0.26300 |
| Power plan | Balanced (`381b4222-f694-41f0-9685-ff5bb260df2e`) |
| GPU | NVIDIA GeForce RTX 5070 Ti (driver 32.0.16.1088), which drives the display; the CPU's AMD Radeon graphics beside it |
| Display | 2560 × 1440 at 165 Hz |
| Encoders | NVIDIA H.264 Encoder MFT, NVIDIA HEVC Encoder MFT, the ones `video_bench` picks |
| Rust | rustc 1.99.1 (58cf8f9c3 2026-10-13), LLVM 23.1.1, `x86_64-pc-windows-msvc` |
| Profile | the workspace's `release` profile, unchanged |

Other agents worked on the machine meanwhile. Every run waited while any
`rustc`, `cargo` or `link` process existed on Windows, or `rustc`, `cargo`,
`ld` or `cc1plus` in WSL. Four runs of `host_send_bench` waited 15 to 60 s,
and four of the second H.265 session 15 to 45 s. The two builds of a
comparison alternated, so what load remained fell on both.

## Method

As in [the 2026-11-11 record](../2026-11-11-refactor/README.md#method):

- **The builds.** Two release builds, each with its own target directory:
  - before = `151ed8d7` (`master`), from a `git archive` of that commit with
    the same submodules;
  - after = `60205324`, the head of the remote-desktop branch.
- **The benches' sources** are identical in both trees.
- **Run order.** The sides alternate: before then after on even rounds,
  after then before on odd ones.
- **The tables.** Each table gives:
  - the median of each side, with its smallest and largest run in brackets;
  - the change of the median;
  - a two-sided Mann-Whitney U p-value (normal approximation, computed as in
    the earlier records).

The benches:

- **`host_send_bench`** runs a relay, a host and a viewer in one process at
  60 fps. The host's "encoder" returns one 4 KiB H.264 unit per capture, and
  the viewer presents every unit. It runs 5 s of video, then 5 s of video
  while the viewer moves the pointer every 4 ms under control. It counts
  every heap allocation and the bytes each asks for, per video unit.
- **`video_bench 10 60 --animate`** drives this machine's hardware encoder on
  its display at 60 fps for 10 s, with its pattern window animating. It
  prints what the encoder's own `submit` and `drain` calls allocate per unit,
  and the time and thread cycles they take. It changes nothing on the
  machine: no display mode, no input.
- **`video_bench 10 60 --animate --codec h265`** is the same on the HEVC
  encoder.

## Results

### `host_send_bench`, 10 rounds each

| metric | before (n=10) | after (n=10) | change | p |
|---|---|---|---|---|
| video units_per_s | 60.00 [60.00..60.00] | 60.00 [60.00..60.00] | 0.0% | 1.000 |
| video allocs_per_s | 1148.0 [1147.0..1150.0] | 1148.0 [1147.0..1150.0] | 0.0% | 0.496 |
| video allocs_per_unit | 19.14 [19.12..19.17] | 19.14 [19.13..19.18] | 0.0% | 0.940 |
| video alloc_bytes_per_unit | 40192.0 [40192.0..40197.0] | 40192.0 [40192.0..40200.0] | 0.0% | 0.473 |
| video+pointer units_per_s | 60.00 [60.00..60.00] | 60.00 [60.00..60.00] | 0.0% | 1.000 |
| video+pointer inputs_per_s | 249.9 [249.7..250.0] | 249.9 [249.9..250.0] | -0.0% | 0.597 |
| video+pointer allocs_per_s | 3653.5 [3647.0..3656.0] | 3655.0 [3652.0..3657.0] | 0.0% | 0.174 |
| video+pointer allocs_per_unit | 60.91 [60.80..60.94] | 60.93 [60.87..60.98] | 0.0% | 0.140 |
| video+pointer alloc_bytes_per_unit | 47121.5 [47115.0..47123.0] | 47122.5 [47121.0..47137.0] | 0.0% | 0.021 |

The allocation counts did not move, with video alone and with the pointer
moving. The one difference p calls significant is 1 byte per unit at the
median, about 0.002 %.

### `video_bench 10 60 --animate`, H.264, 10 rounds each

| metric | before (n=10) | after (n=10) | change | p |
|---|---|---|---|---|
| encoder units | 596.5 [594.0..599.0] | 598.0 [595.0..600.0] | 0.3% | 0.450 |
| encoder allocs_per_unit | 1.314 [1.176..1.530] | 1.285 [1.205..1.465] | -2.2% | 0.734 |
| encoder alloc_bytes_per_unit | 14336.0 [14240.0..15072.0] | 14317.0 [14206.0..14410.0] | -0.1% | 0.762 |
| encoder calls_us_per_unit | 481.0 [419.1..675.8] | 472.6 [399.4..697.3] | -1.7% | 0.650 |
| encoder calls_kcycles_per_unit | 1397.3 [1231.6..1944.0] | 1367.8 [1188.1..1941.7] | -2.1% | 0.496 |
| encoder submit_p50_us | 412.6 [353.6..582.4] | 397.5 [333.6..580.4] | -3.7% | 0.597 |
| encoder submit_mean_us | 435.0 [376.7..607.8] | 429.5 [359.4..621.7] | -1.3% | 0.650 |
| encoder drain_p50_us | 22.85 [19.60..31.30] | 22.10 [17.40..32.20] | -3.3% | 0.678 |
| encoder drain_mean_us | 25.45 [23.10..34.50] | 24.55 [21.20..38.20] | -3.5% | 0.597 |
| encoder submit_p50_kcycles | 1168.3 [1008.4..1641.7] | 1119.5 [984.7..1623.7] | -4.2% | 0.545 |
| encoder submit_mean_kcycles | 1222.2 [1071.7..1686.1] | 1202.0 [1036.8..1682.7] | -1.7% | 0.496 |
| encoder drain_p50_kcycles | 96.35 [83.00..132.0] | 93.50 [73.50..136.3] | -3.0% | 0.705 |
| encoder drain_mean_kcycles | 106.1 [95.50..140.6] | 101.7 [87.70..145.5] | -4.2% | 0.705 |
| unique_capture_fps | 59.65 [59.40..59.90] | 59.80 [59.50..60.00] | 0.3% | 0.450 |
| mean_encode_ms | 3.096 [2.949..3.280] | 3.043 [2.946..3.427] | -1.7% | 0.821 |
| bitrate_mbps | 6.835 [6.780..7.220] | 6.850 [6.750..6.880] | 0.2% | 0.473 |
| keyframes | 2.000 [2.000..2.000] | 2.000 [2.000..2.000] | 0.0% | 1.000 |

No metric moved beyond its spread. Every p is above 0.4, and each side's
range covers the other's median.

### `video_bench 10 60 --animate --codec h265`

First session, 6 rounds each, run right after the H.264 comparison
(`platforms-h265-video_bench.txt`):

| metric | before (n=6) | after (n=6) | change | p |
|---|---|---|---|---|
| encoder units | 598.5 [593.0..600.0] | 597.0 [594.0..599.0] | -0.3% | 0.298 |
| encoder allocs_per_unit | 1.268 [1.224..1.410] | 1.312 [1.121..1.437] | 3.5% | 0.749 |
| encoder alloc_bytes_per_unit | 8675.5 [8611.0..8736.0] | 8737.0 [8596.0..9158.0] | 0.7% | 0.423 |
| encoder calls_us_per_unit | 452.5 [436.9..519.3] | 480.3 [442.7..573.5] | 6.1% | 0.109 |
| encoder calls_kcycles_per_unit | 1330.8 [1301.2..1508.1] | 1415.3 [1299.3..1627.5] | 6.4% | 0.200 |
| encoder submit_p50_us | 394.1 [369.4..453.3] | 415.2 [381.7..458.8] | 5.4% | 0.150 |
| encoder submit_mean_us | 411.4 [395.0..471.0] | 436.5 [403.3..520.8] | 6.1% | 0.150 |
| encoder drain_p50_us | 19.05 [17.60..22.60] | 20.30 [19.30..23.60] | 6.6% | 0.055 |
| encoder drain_mean_us | 21.30 [20.60..25.00] | 23.05 [21.10..26.90] | 8.2% | 0.109 |
| encoder submit_p50_kcycles | 1140.0 [1081.5..1277.1] | 1192.6 [1094.8..1301.2] | 4.6% | 0.298 |
| encoder submit_mean_kcycles | 1175.8 [1143.8..1324.7] | 1251.2 [1148.1..1429.1] | 6.4% | 0.173 |
| encoder drain_p50_kcycles | 80.65 [74.40..95.50] | 85.60 [82.10..98.50] | 6.1% | 0.045 |
| encoder drain_mean_kcycles | 86.95 [86.50..104.7] | 94.30 [88.10..112.0] | 8.5% | 0.055 |
| unique_capture_fps | 59.85 [59.30..60.00] | 59.70 [59.40..59.90] | -0.3% | 0.298 |
| mean_encode_ms | 2.913 [2.850..3.049] | 2.867 [2.809..2.955] | -1.6% | 0.150 |
| bitrate_mbps | 4.145 [4.070..4.170] | 4.170 [4.100..4.340] | 0.6% | 0.298 |
| keyframes | 2.000 [2.000..2.000] | 2.000 [2.000..2.000] | 0.0% | 1.000 |

The after build's call times sat higher at the median, though none at a p
I would claim, so I ran a second session with 10 rounds each, after the app
gates (`platforms-h265b-video_bench.txt`):

| metric | before (n=10) | after (n=10) | change | p |
|---|---|---|---|---|
| encoder units | 596.0 [592.0..599.0] | 596.5 [591.0..600.0] | 0.1% | 0.571 |
| encoder allocs_per_unit | 1.357 [1.158..1.517] | 1.336 [1.125..1.566] | -1.5% | 0.940 |
| encoder alloc_bytes_per_unit | 16677.5 [14587.0..17170.0] | 16666.0 [14592.0..17063.0] | -0.1% | 0.880 |
| encoder calls_us_per_unit | 450.0 [424.5..541.6] | 460.0 [445.1..528.8] | 2.2% | 0.226 |
| encoder calls_kcycles_per_unit | 1352.1 [1283.7..1579.7] | 1386.4 [1344.2..1546.6] | 2.5% | 0.226 |
| encoder submit_p50_us | 385.5 [356.3..466.6] | 400.6 [375.8..449.6] | 3.9% | 0.326 |
| encoder submit_mean_us | 403.7 [378.2..488.9] | 412.8 [397.8..474.4] | 2.3% | 0.307 |
| encoder drain_p50_us | 23.75 [21.30..26.10] | 23.95 [21.90..26.40] | 0.8% | 0.571 |
| encoder drain_mean_us | 25.75 [22.80..28.10] | 26.15 [24.60..29.10] | 1.6% | 0.131 |
| encoder submit_p50_kcycles | 1121.3 [1054.8..1320.1] | 1168.5 [1103.0..1280.5] | 4.2% | 0.174 |
| encoder submit_mean_kcycles | 1178.1 [1107.8..1380.3] | 1206.5 [1163.0..1341.3] | 2.4% | 0.307 |
| encoder drain_p50_kcycles | 100.2 [89.90..110.5] | 101.5 [92.50..111.4] | 1.3% | 0.521 |
| encoder drain_mean_kcycles | 107.8 [95.60..117.4] | 109.3 [101.9..120.3] | 1.4% | 0.112 |
| unique_capture_fps | 59.60 [59.20..59.90] | 59.64 [59.10..60.00] | 0.1% | 0.678 |
| mean_encode_ms | 2.883 [2.819..2.975] | 2.875 [2.835..2.927] | -0.3% | 0.762 |
| bitrate_mbps | 7.915 [6.940..8.160] | 7.940 [6.950..8.070] | 0.3% | 0.940 |
| keyframes | 2.000 [2.000..2.000] | 2.000 [2.000..2.000] | 0.0% | 1.000 |

Verdict: no change is claimed. In both sessions the after build's H.265
calls took a little longer at the median: 6 % in the first, 2 to 4 % in the
second. Every p was above 0.1, except one drain row at 0.045 in the first
session, and each side's range covers the other's median. The H.264 runs
went the other way, by 2 %.

The calls' code did not change: `late` and `codec_error` moved to
`codec/pipeline.rs` unchanged. Code placement between builds moved
`framing_bench` by a few percent in the 2026-11-11 record, and that is the
likeliest cause here. A longer run would settle it.

## What the numbers show, and what they do not

- **No measured path changed.** Allocations per unit did not change, with
  video alone or with pointer input. The Windows encoder's calls did not
  change for H.264. H.265 shows a small median offset (above) that is not
  significant and is not claimed. This is what moving the encoder rules to
  `codec/pipeline.rs` should give: the same code, called from another
  module.
- **The shape path is not measured.** `host_send_bench`'s backend reports no
  pointer and no shapes, so `cursor_shape` is never negotiated, even though
  the after build's viewer lists it. The tables therefore show only that the
  feature costs nothing when it is not negotiated: no shape read, nothing
  added to `Cursor`, B-send as before. The negotiated cost is not measured.
  That cost is a shape read on thread C when the OS pointer changes, a
  definition of `w × h × 4` bytes plus its header once per new shape, and
  about 10 bytes per cursor report. A 32 × 32 pointer is a 4096-byte
  payload. These figures are computed, not measured.
- **Keys are not measured.** The bench's input is pointer moves. Physical
  keys and the text runs are not measured.
- **The Windows host's shape read was not timed.** Its cost per new
  `HCURSOR` (`GetIconInfo`, `GetDIBits`) was not measured.
- **These runs cannot be compared with the 2026-11-11 ones.** The encoder's
  bytes per unit are about half of that record's, because the picture under
  the pattern window differs between sessions. Compare only within one table.

## The viewers

- **Linux.** The Linux viewer was measured by its lane, not in this pass:
  - Environment: WSL2 on this machine, Ubuntu 24.04, glibc 2.39, kernel
    6.6.87.2, the RTX 5070 Ti reached through `/dev/dxg` (driver 610.88),
    GStreamer 1.24.2, WSLg.
  - The decoder's own pacing: in the clip harness, picture interval p50
    16.6–16.8 ms and p95 17.1–17.5 ms. This held for NVDEC and gst-libav,
    H.264 and H.265, with clips offered at 60 and 120 fps.
  - 4K throughput: 3840 × 2160 offered at 120 fps was decoded at 112.6–119.7
    pictures/s by NVDEC and at 120.0 by gst-libav. The libav figure is
    optimistic: x264's zero-latency clips are sliced, which libav decodes in
    parallel.
  - The real app (the acceptance entry, Wayland under WSLg, 720p60 for 20 s)
    ran against a host that replayed recorded clips:
    - H.264 on NVDEC: presented FPS p50 58–60 and picture latency p50
      30–38 ms, with no gap over 250 ms and one decoder configuration;
    - H.264 on gst-libav: presented p50 59, latency p50 17–18 ms;
    - H.265 on NVDEC: presented p50 30–38, latency p50 10–11 ms;
    - X11 through XWayland: presented p50 45, latency p50 33 ms.
  - These app figures say little about a Linux desktop. Presented FPS varied
    between 27 and 60 from run to run while the decoder held 60, and Mesa
    reported no hardware EGL screen, so Flutter may have drawn in software.
  - Not measured on a real Linux desktop: presentation rate, latency and CPU
    cost.
- **macOS and iOS: not measured.** Nothing ran on a Mac or an iPhone; there
  is none here. Encode time, decode time, latency, frame rate and the shape
  read are all not measured.

## Commands

From the repository root of each tree (`CARGO_TARGET_DIR` for the after
build was a directory of its own):

```text
cargo build --release --locked -p pb-mapper-desktop --example host_send_bench --example video_bench
host_send_bench.exe
video_bench.exe 10 60 --animate
video_bench.exe 10 60 --animate --codec h265
```

Every run went through one script that alternated the two builds, waited for
a quiet machine and kept each run's result lines. The tables came from a
second script, with the median and Mann-Whitney U computed as in the
2026-11-11 records; it reproduces that record's p-values from its raw files.
Neither script is kept.

## Files

- `platforms-host_send_bench.txt`: the `host_send_bench` comparison.
- `platforms-video_bench.txt`: the H.264 comparison.
- `platforms-h265-video_bench.txt`, `platforms-h265b-video_bench.txt`: the
  two H.265 sessions.
