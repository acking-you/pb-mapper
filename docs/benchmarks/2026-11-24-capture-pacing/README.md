# Present-anchored capture pacing: measurements

Release builds on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425) and
Rust 1.99.1. Other builds ran on the machine at the same time, so throughput
and latency figures carry their noise; allocation counts per unit do not
depend on timing. The baseline is master at `1515eda`; the measured code is
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

Thread C's new per-capture work (the pacer's dated branch and the pacing
window) allocates nothing. The two-viewer figures follow the input rate each
run reached, which the concurrent builds lowered in some runs of both builds:
at an equal rate they match (master run 2, 499.3 inputs/s: 166.96; branch
run 1, 499.9 inputs/s: 167.04). Every run of both builds produced 60 units a
second.

## Left for a device

None of the effect shows here: the pacing tests run on a synthetic dated
display, and a Mac host needs a Screen Recording grant this session does not
have. Release builds, master against the branch, recording GPU, driver,
display mode and OS build:

- Windows host in process, on a 60 Hz mode, a 59.94 Hz mode and 120 Hz, at
  pace 60 and 120, scrolling and with a 120 fps animation:
  `cargo run --release -p pb-mapper-desktop --example video_bench -- 30 60 --animate`
  for `present_to_submit_p50_ms`/`p95_ms` and `coalesced_presents` per 30 s
  (master's `video_bench` prints neither, so its side is a live session's
  trace), and a live session's `desktop_capture_pacing`; the capture rate must
  not fall.
- Windows host on a still desktop, clock and pointer hidden, before and after
  a UAC switch through the helper: the `windows_capture_first_acquire` line
  and the time to the first unit.
- Mac host with Screen Recording granted:
  `target/release/examples/deployed APP_CONFIG_PATH OUT/<arm>-<n> selftest 40 --tcp --feedback-log --fps 60`
  and `--fps 120 --quality smooth --fps-override`, in the order A B B A:
  `desktop_capture_pacing` wait p50/p95 and presents per capture,
  `macos_capture_timing`; presented FPS must not fall and typing echo must
  not regress.
