# Rate-model latch: measurements

Release builds on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425) and
Rust 1.99.1. Other worktrees were building on the machine at the same time:
the load average at the start of each run was between 1.05 and 5.17, and each
run's figure is in its raw output. Everything here runs in one process on
loopback, over a real relay and TLS, with the synthetic encoders of
`slow_link.rs`: pipeline observations, not a GPU's encoder.

"Old latch" is `qos.rs` as of `1515eda` (two consecutive 3 s windows);
"per-tick latch" is this branch's (the last 2 s of 250 ms QoS ticks, the
expected bits integrated at each tick's highest per-frame budget, and an
overshoot above `max(1.3, min(0.6 × nominal / mean pace, 2))`). Each was built
with this branch's final `slow_link.rs` into one release test binary, and the
whole suite ran three times on each, the two binaries taking turns:

`cargo test --release -p pb-mapper-desktop --lib -- --ignored --nocapture
--test-threads=1 slow_link`. [Old latch](slow_link_before.txt),
[per-tick latch](slow_link_after.txt).

The suite adds an encoder that budgets per second (`Sizes::PerSecond`: the
bitrate over the time since the previous capture, at most 1 s) and reports,
for each case, when the host traced `desktop_rate_model_latched`. Every run of
the old latch fails `per_second_encoder_latches_under_an_fps_cap`, which
asserts a latch within 2.5 s of the pace falling: that is the change measured.

## A per-second encoder under a viewer's fps cap

`per_second_encoder_latches_under_an_fps_cap`: a decoder taking 67 ms a frame,
so the viewer's fps cap holds the pace at 10 to 20 FPS (a mean of 11.9 to 13.0
over the last 10 s) against a nominal 60. The startup cap and then the fps cap
kept the pace at 45 FPS or below from the first capture on, so the time to the
latch counts from that capture. Sent over expected is the trace's sent bitrate
over `bits per frame × pace`, averaged over its 1 s intervals at 45 FPS or
below, before and after the latch.

| Latch | Run | Latched after (ms) | Sent / expected before | Sent / expected after |
| --- | --- | ---: | ---: | ---: |
| Old | 1 | 6049 | 4.94 | 1.03 |
| Old | 2 | 6060 | 4.98 | 1.10 |
| Old | 3 | 6049 | 5.08 | 1.00 |
| Per-tick | 1 | 2015 | 3.53 | 1.03 |
| Per-tick | 2 | 2017 | 3.47 | 1.01 |
| Per-tick | 3 | 2018 | 3.47 | 1.03 |

Until the latch the encoder sent several times what the host expected; after
it, about what it expected. The per-tick latch came 2.0 s after the pace fell
instead of 6.0 s, so its average before the latch covers only the interval
ending in the second second, near the startup cap's 30 FPS, where a per-second
budget overspends by less than at the fps cap's 10 to 20.

## A per-second encoder on a bandwidth limit

`per_second_encoder_on_a_bandwidth_limit`: 6 s unlimited, then 1 Mbit/s for
8 s, then unlimited until the expected bitrate is back at 3.2 Mbit/s; the time
to the latch counts from the limit.

| Latch | Run | Latched | Lowest pace in the limit | Lowest encoder bitrate in the limit | Recovered after the lift |
| --- | --- | --- | ---: | ---: | ---: |
| Old | 1 | 9103 ms after the limit began | 2 | 4.15 Mbit/s | 1019 ms |
| Old | 2 | 9102 ms after the limit began | 2 | 4.30 Mbit/s | 1012 ms |
| Old | 3 | not before the case ended | 2 | 4.30 Mbit/s | 1004 ms |
| Per-tick | 1 | not before the case ended | 2 | 4.15 Mbit/s | 1020 ms |
| Per-tick | 2 | not before the case ended | 2 | 4.30 Mbit/s | 1004 ms |
| Per-tick | 3 | not before the case ended | 2 | 4.30 Mbit/s | 1007 ms |

Neither latch finds a per-second encoder during the limit. Its pictures carry
the nominal 4 Mbit/s whatever the pace, so they fill the link; capture then
waits for credit, each picture after a wait carries the wait's bits, and the
pace falls to 2 FPS. No 2 s of ticks, and no 3 s window, then holds the six
unique frames either latch needs, and the encoder bitrate stayed at the
nominal model's throughout. The old latch fired in two runs about 1.1 s after
the lift, from a 3 s window that counts frames whatever the pace. The per-tick
latch needs 2 s of ticks at 45 FPS or below with six frames in them, and each
case ended, once the expected bitrate was back, about a second after the lift.
In these runs the sent bitrate is no measure of the overspend: the per-second
pictures exceed the send window, so the intervals at a low pace sent nothing
(sent over expected 0.00).

## A fixed-size encoder

`sustained_slow_tls_reader` sends 64 KiB (524,288-bit) pictures whatever the
bitrate on a 256 KiB/s link, which carries four of them a second, against
Balanced's 66,667 bits per frame at its start and 16,667 at its floor. No rate
model fits such an encoder, so the case reports the latch without asserting
it. The old latch fired 6036, 6038 and 6040 ms after the case's start. The
per-tick latch fired 9061, 8301 and 8306 ms after it, 1.8 to 2.1 s into the
stretch at 45 FPS or below that it came in. The pace in this case moved
between 2 and 60 FPS (the once-a-ten-seconds lines in the raw output), and the
link carries at most four of these pictures a second, so a span of six unique
frames in 2 s comes only while the pace is 3 FPS or more.

## Per-frame encoders never latch

Every case on a per-frame budget (`TYPICAL`, and the slow IDR case's IDRs of
20 budgets) passed and asserted that nothing latched, in all three runs of
each latch. The per-frame slow presenter sent 1.12 to 1.18 times what was
expected at the fps cap. The LAN case reached the ceiling 978 to 980 ms after
connecting, the pace held 60 through three 600 ms stalls and at a 350 ms RTT,
the 45 ms encoder kept 60 FPS, and the per-frame bandwidth case fell to 10 or
11 FPS and recovered 1.0 to 5.0 s after the lift, as in the
[desktop pipeline record](../2026-11-24-desktop-pipeline/README.md).

## Unit tests

`cargo test -p pb-mapper-desktop --lib qos::` passes on this branch, and each
of these changes to the per-tick latch fails at least one of its latch tests: the expected bits taken at the
budget at a tick's end, or from bits per frame instead of the encoder's
budget, or without every change of bits per frame raising the tick's highest;
a tick counted as paced low by its start or its end alone; the six unique
frames dropped; an early tick not folded into the next; the mean pace taken
from the published pace; the pace term uncapped, dropped, or capped at 3;
`LATCH_PACE_SHARE` at 0.3 or 0.9; and a 3 s span.

## What needs a device

Whether any encoder pb-mapper ships budgets per second. `video_bench --pace
60,30,15,5` on a Windows host with an AMD or an Intel encoder, with `--codec
h264` and `--codec h265`, classifies its MFTs; on a Mac host with Screen
Recording granted, `rate_model` and `sent_bitrate` against `expected_bps` in
the first seconds of a Quality session show when VideoToolbox latches.
