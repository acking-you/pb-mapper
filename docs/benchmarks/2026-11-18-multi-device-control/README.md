# Multi-device remote desktop control and peer cursors, 2026-11-18

Environment: 12-core Apple M3 Pro, 36 GiB RAM (`38654705664` bytes), macOS 27.0.1 (`26A434`), `rustc 1.99.1 (58cf8f9c3 2026-10-13)`, arm64 `release` profile (`RUST_LOG=off cargo run --release -p pb-mapper-desktop --example host_send_bench`).

`host_send_bench` runs a relay, a synthetic H.264 host (4 KiB per encoded unit at 60 FPS) and in-process viewers with global allocation counting across the whole process:

1. `video`: one controlling viewer, video only for 5 s.
2. `video+pointer`: one controlling viewer moving its pointer every 4 ms (~250 Hz) for 5 s.
3. `2viewers+pointer`: two concurrent controlling viewers both moving their pointers every 4 ms (~500 inputs/s total), exercising zero-copy `Bytes` video fan-out across two relay sessions and bidirectional `PeerCursor` updates outside the video queue (`units_per_s` stays measured per host encoded unit).

| Case | Build | Units/s | Inputs/s | Allocs/s | Allocs/unit | Allocated bytes/unit |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `video` | Baseline (`b0e77c6`) | 60.0 | 0.0 | 1,156 | 19.28 | 40,218 |
| `video` | After (3-run median) | 60.0 | 0.0 | 1,145 | 19.10 | 40,228 |
| `video+pointer` | Baseline (`b0e77c6`) | 60.0 | 246.6 | 3,579 | 59.65 | 47,046 |
| `video+pointer` | After (3-run median) | 60.2 | 249.8 | 3,596 | 59.75 | 47,070 |
| `2viewers+pointer` | After (3-run median) | 60.0 | 499.2 | 10,104 | 168.42 | 100,459 |

Single-viewer sessions pay zero extra allocations or heap copies: `FrameBytes::Owned(Vec<u8>)` avoids `Bytes` promotion with one active viewer, `RoomShared.epoch` avoids locking room membership on the capture thread unless membership or codec changes, and `Engine` touches `PeerCursorSnapshot` only while another `peer_cursors`-capable session observes it. Raw output from the baseline run and the three post-change runs is in [host_send_bench.txt](host_send_bench.txt).
