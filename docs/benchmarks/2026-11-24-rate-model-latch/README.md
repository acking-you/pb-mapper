# Rate-model latch: measurements

Release builds on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425) and
Rust 1.99.1. Other worktrees may have been building on the machine at the same
time. Everything here runs in one process on loopback, over a real relay and
TLS, with the synthetic encoders of `slow_link.rs`: pipeline observations, not
a GPU's encoder.

"Old latch" is `qos.rs` as of `1515eda` (two consecutive 3 s windows); "per-tick
latch" is `823e0af`'s (the last 2 s of 250 ms QoS ticks, bits per frame
integrated tick by tick, overshoot above `max(1.3, 0.6 × nominal / mean pace)`).
Both ran the slow-link cases of `4ecbcac`, which add an encoder that budgets
per second (`Sizes::PerSecond`: the bitrate over the time since the previous
capture, at most 1 s) and report, for each case, when the host traced
`desktop_rate_model_latched`.

`cargo test --release -p pb-mapper-desktop --lib -- --ignored --nocapture
--test-threads=1 slow_link`: the whole suite once on the old latch and three
times on the per-tick latch, and the per-second and fixed-size cases
(`slow_link::per_second slow_link::sustained_slow_tls_reader`) three more times
on the old latch. [Old latch](slow_link_before.txt),
[per-tick latch](slow_link_after.txt).

## A per-second encoder under a viewer's fps cap

`per_second_encoder_latches_under_an_fps_cap`: a decoder taking 67 ms a frame,
so the viewer's fps cap holds the pace at 10 to 20 FPS (a mean of 12.0 to 13.3
over the last 10 s) against a nominal 60.
The startup cap and then the fps cap kept the pace at 45 FPS or below from the
first capture on, so the time to the latch counts from that capture. Sent over
expected is the trace's sent bitrate over `bits per frame × pace`, averaged over
its 1 s intervals at 45 FPS or below, before and after the latch.

| Latch | Run | Latched after (ms) | Sent / expected before | Sent / expected after |
| --- | --- | ---: | ---: | ---: |
| Old | 1 (whole suite) | 6049 | 5.34 | 0.99 |
| Old | 2 | 6051 | 4.95 | 1.14 |
| Old | 3 | 6050 | 5.52 | 1.06 |
| Old | 4 | 6062 | 4.88 | 0.96 |
| Per-tick | 1 | 2017 | 3.52 | 1.02 |
| Per-tick | 2 | 2018 | 3.53 | 1.11 |
| Per-tick | 3 | 2018 | 3.47 | 1.03 |

Until the latch, the encoder sent several times what the host expected; after
it, about what it expected. The per-tick latch came 2.0 s after the pace fell
instead of 6.0 s, so its average before the latch covers only the interval
ending in the second second, near the startup cap's 30 FPS, where a per-second
budget overspends by less than at the fps cap's 10 to 20.

## A per-second encoder on a bandwidth limit

`per_second_encoder_on_a_bandwidth_limit`: 6 s unlimited, then 1 Mbit/s for
8 s, then unlimited until the expected bitrate is back at 3.2 Mbit/s; the time
to the latch counts from the limit.

| Latch | Runs | Latched | Lowest pace in the limit | Lowest encoder bitrate in the limit |
| --- | --- | --- | ---: | ---: |
| Old | 4 | 9111, 9100 and 9099 ms after the limit began, 1.1 s after it lifted; once not before the case ended | 2 | 4.15 to 4.30 Mbit/s |
| Per-tick | 6 (three in the whole suite, three alone) | never before the case ended | 2 | 4.15 to 4.30 Mbit/s |

Neither latch finds a per-second encoder during the limit. Its pictures carry
the nominal 4 Mbit/s whatever the pace, so they fill the link; capture then
waits for credit, each picture after a wait carries the wait's bits, and the
pace falls to 2 FPS. No 2 s of ticks, and no 3 s window, then holds the six
unique frames either latch needs, and the encoder bitrate stayed at the
nominal model's throughout. The old latch fired once pictures flowed again
after the lift, from a 3 s window that counts frames whatever the pace. The
per-tick latch needs 2 s of ticks at 45 FPS or below with six frames in them,
and each case ended, once the expected bitrate was back, 0.02 to 1.0 s after
the lift. In these runs the sent bitrate is no measure of the overspend: the
per-second pictures exceed the send window, so the intervals at a low pace
sent nothing (sent over expected 0.00).

## A fixed-size encoder

`sustained_slow_tls_reader` sends 64 KiB (524,288-bit) pictures whatever the
bitrate on a 256 KiB/s link, which carries four of them a second, against
Balanced's 66,667 bits per frame at its start and 16,667 at its floor. No rate
model fits such an encoder, so the case reports the latch without asserting
it: the old latch fired 6030, 6042, 6035 and 6028 ms after the case's start,
the per-tick latch 10813, 11577 and 11580 ms after it, its content-floor term
asking for more at so low a pace.

## Per-frame encoders never latch

Every case on a per-frame budget (`TYPICAL`, and the slow IDR case's IDRs of
20 budgets) passed and asserted that nothing latched: once on the old latch and
in each of the three runs on the per-tick latch. As in the
[desktop pipeline record](../2026-11-24-desktop-pipeline/README.md), the LAN
case reached the ceiling 0.98 s after connecting, the pace held 60 through
three 600 ms stalls and at a 350 ms RTT, and the 45 ms encoder kept 60 FPS.

The per-tick runs' whole-suite output shows `per_second_encoder_on_a_bandwidth_limit`
failing an assertion that it latched, which these runs showed it cannot meet
and which was then dropped; the three runs of that case alone carry the final
assertions.

## Unit tests

`cargo test -p pb-mapper-desktop --lib qos::` passes on this branch. Its four
latch tests, put on `1515eda`'s `qos.rs`, fail there: a per-second encoder
capped to 15 FPS latched 5 s after the cap instead of within 2.5 s; at 30 FPS
it did not latch within 3 s; bits per frame halved in the middle of each window
latched a per-frame encoder; and frames of 1.79 times their budget at 5 FPS
(NVIDIA's content floor at a nominal 120) latched it too.

## What needs a device

Whether any encoder pb-mapper ships budgets per second. `video_bench --pace
60,30,15,5` on a Windows host with an AMD or an Intel encoder, with `--codec
h264` and `--codec h265`, classifies its MFTs; on a Mac host with Screen
Recording granted, `rate_model` and `sent_bitrate` against `expected_bps` in
the first seconds of a Quality session show when VideoToolbox latches.
