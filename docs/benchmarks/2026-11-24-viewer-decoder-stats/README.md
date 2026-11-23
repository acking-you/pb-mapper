# Mac viewer decoder stats: measurements

What the macOS viewer's new `stats` figures read in the real app, against a
harness that times the same decode outside it. Release builds on a 12-core
Apple M6 with 24 GiB RAM, macOS 27.0 (26A425), Rust 1.99.1 and Flutter
3.50.1, one display at 144 Hz. Other agents were building in parallel
worktrees on the machine throughout: the load averages read between runs
were 4.3 to 10.7.

This change measures; it does not change decoding, so there is no speed to
compare, only whether the new figures read true. The cost it adds runs only
while timings are on (the quality panel is open, or the acceptance tool
reads them): per decoded unit, two monotonic clock reads around
`VTDecompressionSessionDecodeFrame`, one thread-CPU clock read and one
uncontended lock. It was not measured separately.

## Setup

- Clips: `clipgen.swift` (`swiftc -O`) encodes 240 frames of a desktop-like
  picture (flat background, text-like rows, a moving window) with
  VideoToolbox's real-time H.264 encoder, Main profile, no reordering, one
  IDR at the start, an access unit delimiter before every unit:
  `clipgen 1920 1080 240 clip-1080p.h264` (6,245 bytes a unit on average) and
  `clipgen 3840 2160 240 clip-4k.h264` (20,850).
- Host: `cargo build --release -p pb-mapper --features desktop --example desktop_ui_host`,
  then `desktop_ui_host --clip CLIP --size WxH --dir DIR --relay-only --view-only --fps 60 --seconds 40`,
  served by `node scripts/test/desktop-bootstrap.cjs DIR`.
- Viewer: the acceptance entry built as in
  [the macOS viewer procedure](../../../scripts/test/desktop-ui-acceptance.md#macos-viewer),
  `--release` with `--dart-define=maxFps=60 --dart-define=seconds=40 --dart-define=resolution=native`
  (a 4K host needs `native`: at the default 1080p the session ended `protocol`).
  Balanced preset, relay over TCP on loopback. Each run is a fresh app
  process; the tool reads `stats` once a second with timings on.
- `analyze.py` turns each report's samples after the 5 s warm-up into
  per-second figures: each timing's mean between two samples (the change in
  `samples * mean_ms` over the change in `samples`) and `decoderCpuMs`
  gained per second. [Per-run summary](summary.jsonl);
  [the samples' `native` figures, phases and rates](samples.json).
- Harness: `paced.swift` (`swiftc -O`) decodes the same clip three times
  over, paced at 60 fps, with the session `ui/darwin/DesktopVideo.swift`
  builds (hardware, real time, synchronous, IOSurface-backed BGRA, two
  pictures held), timing each `VTDecompressionSessionDecodeFrame`.
  [Output](paced.txt)
- Baseline: the same acceptance build with `ui/darwin/DesktopVideo.swift`
  from `1515eda`, which reports no decode time, run twice on the 1080p clip
  (`base-1080p-a`, `base-1080p-b` in the raw files).

## Results

Every run of this change reported `decoder` "VideoToolbox H.264", `hardware`
true, `lowLatency` true and `refreshHz` 144. Per-second figures over 35
intervals a run (the run IDs are `h264-1080p-a` and so on in the raw files):

| Run | Decode ms, median (min–max) | Decode max in run, ms | Decoder CPU ms/s, median (min–max) | Release to display ms, median | Decoded/s, median (min) |
| --- | --- | ---: | --- | ---: | --- |
| 1080p a | 2.87 (1.60–49.73) | 723.1 | 5.45 (0.49–8.31) | 3.90 | 59.9 (7.2) |
| 1080p b | 1.82 (1.63–21.13) | 397.5 | 5.08 (2.90–7.68) | 3.73 | 59.8 (29.9) |
| 1080p c | 2.95 (1.67–3.39) | 9.0 | 6.83 (3.28–7.97) | 4.14 | 59.8 (59.8) |
| 1080p d | 2.15 (1.63–7.66) | 228.8 | 4.54 (2.74–7.90) | 3.97 | 59.9 (53.1) |
| 4K a | 5.60 (4.44–6.06) | 22.6 | 8.36 (2.98–10.11) | 3.89 | 59.9 (59.8) |
| 4K b | 4.99 (4.49–6.41) | 43.7 | 4.96 (2.54–10.07) | 4.06 | 59.8 (59.8) |

The harness, run twice on each clip (720 decodes a run):

| Clip | Mean ms | Median ms | p95 ms | Max ms |
| --- | ---: | ---: | ---: | ---: |
| 1080p | 3.050, 2.596 | 3.217, 2.774 | 3.912, 3.616 | 15.4, 15.7 |
| 4K | 5.632, 5.099 | 5.662, 4.736 | 6.198, 6.582 | 31.5, 31.8 |

- The app's per-second decode means sit in the harness's range for the same
  clip: run medians of 1.8 to 3.0 ms against a harness mean of 2.6 to
  3.1 ms at 1080p, and 5.0 to 5.6 ms against 5.1 to 5.6 ms at 4K.
- The decoder thread spent a median 4.5 to 8.4 ms of CPU a second, under 1%
  of a core and far below its decode time, as a thread that waits for the
  hardware decoder during a synchronous decode would.
- Three 1080p runs had seconds where single decodes took 0.2 to 0.7 s, all
  after 32 s; in two of them the session went `recovering` and back to
  `live`. The 4K runs and 1080p c had none. The baseline had the same kind
  of late stall in one of its two runs: decoded pictures fell to 41 and then
  31 a second at 39 and 40 s, and the session went `recovering`. So the
  stalls predate this change; with the machine shared, this record does not
  say where they came from. The new decode timing is what places them in
  the decoder, where before only the frame rate fell.
- The baseline's release-to-display medians (3.74 and 3.95 ms) and
  decoded rates (59.9 and 59.8 a second, medians) match the new runs'.

## Left for a device

- `refreshHz` following the viewer window to a display with another refresh
  rate: this Mac has one display.
- The Linux runner's `timings.surface_consume` and `refreshHz`, and the
  Windows panel's Convert and copy row, which only CI builds here.
