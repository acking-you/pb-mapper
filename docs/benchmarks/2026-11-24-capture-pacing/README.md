# Present-anchored capture pacing: measurements

Release builds on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425) and
Rust 1.99.1. Other builds ran on the machine at the same time, so throughput
and latency figures carry their noise; allocation counts per unit depend on
timing only through the input rate a run reached (see the two-viewer case).
The baseline is master at `1515eda`; the measured code is
`c24e7c0`, which present-anchored pacing, the new `Backend` seams and both
hosts' picture dating end at. Everything here runs in one process on loopback
with a synthetic encoder, whose pictures carry no present time: it measures
what the change costs on the data path, not what it gains, which needs a
display (see the last section).

## Zero cost on the data path

`RUST_LOG=off cargo run --release -p pb-mapper-desktop --example host_send_bench`,
three runs each, alternating master and the branch; per-metric medians.
[Per-run output](host_send_bench.txt)

| Case | Build | Units/s | Inputs/s | Allocations/unit | Allocated bytes/unit |
| --- | --- | ---: | ---: | ---: | ---: |
| One viewer, video | `1515eda` | 60.0 | 0 | 18.82 | 40,197 |
| One viewer, video | `c24e7c0` | 60.0 | 0 | 18.83 | 40,183 |
| One viewer, moving pointer | `1515eda` | 60.0 | 249.1 | 58.60 | 46,994 |
| One viewer, moving pointer | `c24e7c0` | 60.0 | 249.7 | 58.71 | 47,059 |
| Two viewers, both moving pointers | `1515eda` | 60.0 | 498.0 | 166.46 | 100,315 |
| Two viewers, both moving pointers | `c24e7c0` | 60.0 | 489.6 | 161.58 | 99,486 |

The synthetic encoder dates no picture, so these runs take only the pacer's
undated path; its dated path and the pacing statistics allocate nothing by
construction (a few scalars, and fixed arrays of 256 samples), which no run
here exercised. The two-viewer figures follow the input rate each
run reached, which the concurrent builds lowered in some runs of both builds:
at an equal rate they match (master run 2, 499.3 inputs/s: 166.96; branch
run 1, 499.9 inputs/s: 167.04). Every run of both builds produced 60 units a
second.

After review, the code at `eeff0ec` (a picture of several presents no longer
moves the timeline earlier, a recovery always rebuilds the source, and
`video_bench` gained its grid arm; the undated path is unchanged) ran three
more times, back to back, with no master runs between them. Medians, as
allocations and bytes per unit: one viewer, video 18.83 / 40,184; moving
pointer 58.83 / 47,060 at 249.9 inputs/s; two viewers 167.18 / 100,438 at
499.8 inputs/s. Units per second ranged from 59.8 to 60.2; the 59.8 was the
moving-pointer case of run 2, at 247.0 inputs/s.

## Left for a device

None of the effect shows here: the pacing tests run on a synthetic dated
display, and a Mac host needs a Screen Recording grant this session does not
have. Release builds, recording GPU, driver, display mode and OS build. Master
reports neither the wait from a present nor coalesced presents, so each
comparison's baseline arm is the branch on the even grid, which
`undated_consumes_keep_the_grid` proves lays master's ticks exactly:

- Windows host in process, on a 60 Hz mode, a 59.94 Hz mode and 120 Hz, at
  pace 60 and 120, scrolling and with a 120 fps animation:
  `cargo run --release -p pb-mapper-desktop --example video_bench -- 30 60 --animate`
  (anchored) against the same with `--grid` (the grid), alternating. Compare
  `present_to_submit_p50_ms`/`p95_ms`, `coalesced_presents_per_s`,
  `polls_per_s` and `calls_kcycles_per_unit` (anchoring polls DXGI from three
  quarters of a period after each picture, so its extra capture calls show
  there), and `unique_capture_fps`, which must not fall. A live session in
  the app with `RUST_LOG=info,pb_mapper_desktop::feedback=debug` adds
  `desktop_capture_pacing`.
- Windows host on a still desktop, clock and pointer hidden: `video_bench`
  without `--animate`, master against the branch. If the first frame
  reports no present, master's first unit waits for a change on the screen,
  or the run fails as capture stuck after 3 s; the branch prints
  `first_access_unit_ms` and what the first frame reported
  (`first_acquire_*`). Behind the background service, before and after a UAC
  switch: whether the viewer shows the still desktop at once, since neither
  the service log nor the capture helper records debug events.
- Mac host with Screen Recording granted:
  `target/release/examples/deployed APP_CONFIG_PATH OUT/<arm>-<n> selftest 40 --tcp --feedback-log --fps 60`
  and `--fps 120 --quality smooth --fps-override`, in the order A B B A. B is
  the branch; A is the branch with thread C's submit path passing no date to
  the pacer (in `worker/capture.rs`, `self.pacer.consume(now, None)` where it
  passes the picture), so that `desktop_capture_pacing` reports the grid's
  wait too. Compare its `wait_p50_us`/`wait_p95_us` and
  `presents_per_s`/`captures_per_s`, and `macos_capture_timing`; presented
  FPS must not fall and typing echo must not regress, against master as well.
