# Desktop pipeline changes: measurements

Release builds on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425) and
Rust 1.99.1, with nothing else running. The baseline is `65ca9e7` (master with
#61); the measured head is `688bfa0`, the code this release ships. Everything
here runs in one process on loopback with synthetic encoders. None of it shows
the Windows changes at work (armed capture ticks, helper protocol 10, the
viewer's GPU event wait), which need a Windows device; see the last section.

## Zero cost on the data path

`RUST_LOG=off cargo run --release -p pb-mapper-desktop --example host_send_bench`,
three runs each; per-metric medians. [Per-run output](host_send_bench.txt)

| Case | Build | Frames/s | Inputs/s | Allocations/frame | Allocated bytes/frame |
| --- | --- | ---: | ---: | ---: | ---: |
| One viewer, video | `65ca9e7` | 60.0 | 0 | 19.15 | 40,239 |
| One viewer, video | `688bfa0` | 60.0 | 0 | 19.12 | 40,233 |
| One viewer, moving pointer | `65ca9e7` | 60.0 | 250.0 | 59.16 | 47,140 |
| One viewer, moving pointer | `688bfa0` | 60.0 | 249.9 | 59.13 | 47,154 |
| Two viewers, both moving pointers | `65ca9e7` | 60.0 | 499.5 | 167.39 | 100,522 |
| Two viewers, both moving pointers | `688bfa0` | 60.0 | 499.5 | 167.47 | 100,512 |

The receipt-rate estimator, which now runs on every receipt, adds no allocation
per frame. Peer-cursor latency medians were 256/393/423 µs (p50/p95/p99) before
and 253/415/440 µs after, within run-to-run spread.

## Slow links

`cargo test --release -p pb-mapper-desktop --lib -- --ignored --nocapture
--test-threads=1 slow_link` at `688bfa0`: all ten cases pass, and the eight
that predate this release match [the receipt-credit record](../../remote-desktop.md#this-revision-receipt-credit)
(pace at the ceiling 0.98 s after connecting on the LAN case; 60 FPS through
three 600 ms stalls; 60 FPS with a 45 ms encoder and no skipped tick; pace 60
at a 350 ms RTT). [Output](slow_link.txt)

The bandwidth-limit case's recovery (back to at least 3.2 Mbit/s after the
1 Mbit/s limit lifts) is quantised by the QoS tick and varies from run to run:
over nine baseline runs it took 1.0 s twice and about 2.0 s seven times.

## Withdrawn: a keyframe superseding the hand-off

Letting an H.264/H.265 keyframe replace the units still waiting for send
credit was implemented and measured, then withdrawn:

| Case (1 Mbit/s, 45 ms encoder, 8 rounds a run) | Without (3 runs) | With (3 runs) |
| --- | --- | --- |
| Keyframe another viewer caused, wait median (ms) | 507, 480, 487 | 397, 330, 275 |
| Keyframe this viewer asked for, wait median (ms) | 405, 337, 450 | 381, 367, 413 |
| Bandwidth-limit recovery taking 5 s | 0 of 21 runs | 4 of 16 runs |

It helped only a room's other viewers, and made the common single-viewer
recovery after congestion slower in a quarter of the runs. The two slow-link
cases that time requested keyframes stay, for a later attempt.

## Withdrawn: thread C priority

Raising thread C to the user-interactive QoS class while it streams H.264 or
H.265 was to ship only if it measured a gain under an all-core load. With 24
`yes > /dev/null` processes on the 12 cores, `host_send_bench` whose backend
declared off-thread encoding (raised) and the same without (normal) both held
60 frames/s and 250 or 500 inputs/s in every run; peer-cursor p99 was 279, 422
and 2,214 µs raised against 282, 802 and 627 µs normal, which is noise, not a
gain. [Per-run output](priority_ab.txt) A Windows host, where a timer wake can
wait out a busy thread's quantum, was not measured.

## Left for a device

- Windows host, in process and through the helper (protocol 10): the wait from
  a present to its capture while active (typing echo), capture calls per
  second on a still and on an active desktop, and the helper's pipe round
  trips while polling every 2 ms. `video_bench` now mirrors thread C's armed
  tick and prints `capture_calls_per_s` and `polls_per_s`.
- Windows viewer: decoder-thread CPU before and after the GPU event wait, and
  the new `timings.surface_consume` and `timings.convert` figures at 1080p60
  and 4K (see the Windows section of `scripts/test/desktop-ui-acceptance.md`).
- Direct UDP receive buffers: `netstat -s -p udp` receive errors on a Windows
  viewer over LAN QUIC at Quality or 4K. On this Mac the default is already
  786,896 bytes and an 8 MiB request is granted.
