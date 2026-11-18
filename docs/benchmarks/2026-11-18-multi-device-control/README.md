# Multi-device desktop control and peer cursor measurements

The arm64 release build ran on a 12-core Apple M3 Pro with 36 GiB RAM, macOS 27.0.1 (26A434) and Rust 1.99.1. Reproduce with `RUST_LOG=off cargo run --release -p pb-mapper-desktop --example host_send_bench`.

The relay, synthetic H.264 host and viewers run in one process. The host produces 4 KiB units at 60 FPS. Each throughput window lasts five seconds. Frames/s counts pictures presented by the first viewer. Inputs/s counts moves applied by the host across all viewers. Allocation counts cover the whole process.

| Case | Build | Frames/s | Inputs/s | Allocations/frame | Allocated bytes/frame |
| --- | --- | ---: | ---: | ---: | ---: |
| One viewer, video | Initial base `b0e77c6` | 60.0 | 0 | 19.28 | 40,218 |
| One viewer, video | Integrated `f2784df` | 60.0 | 0 | 19.06 | 40,213 |
| One viewer, moving pointer | Initial base `b0e77c6` | 60.0 | 246.6 | 59.65 | 47,046 |
| One viewer, moving pointer | Integrated `f2784df` | 60.0 | 250.0 | 59.06 | 47,103 |
| Two viewers, both moving pointers | Original PR `68177f4` | 60.0 | 499.1 | 167.91 | 100,450 |
| Two viewers, both moving pointers | Integrated `f2784df` | 60.0 | 499.1 | 167.20 | 100,417 |

The integrated values are per-metric medians across three runs. The initial base and original PR rows each use one run. A single viewer receives the encoder's owned buffer. Multiple viewers share the encoded bytes.

## Cursor latency

After the throughput windows, one controller sent 256 distinct moves, waiting for each position to appear in the other viewer's public Rust snapshot before sending the next. The observer sampled at 100 µs sleep intervals. The medians of the three runs' percentiles were **262 µs at p50, 543 µs at p95 and 681 µs at p99**. This measures the local relay and both SDKs through snapshot observation. Network transit on a physical link, Flutter vsync, compositing and display scanout are outside the measurement.

The raw output includes the original measurements, the review-fix comparison and all three integrated runs. [Per-run output](host_send_bench.txt)
