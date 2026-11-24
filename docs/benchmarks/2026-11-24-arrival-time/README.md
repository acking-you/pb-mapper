# Arrival times and keyframe seqs: measurements

Release builds on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425) and
Rust 1.99.1. Other builds ran on the machine at the same time, so throughput
and latency figures carry their noise. Every case runs in one process on
loopback, through a real relay and TLS, with a synthetic encoder and a
simulated decoder; the viewer reaches the relay through the testkit's shaped
hop, which now shapes each direction apart (`shaped_link_each_way`).

"Before" is `perf2/wave1` (`bdbcd76`) with the two new slow-link cases added
and nothing else changed (`e25330b`). "After" is the change as committed: the
viewer stamps a receipt once it has read its unit whole and a probe echo once
it has read the probe, and asks with the last seq read for an IDR it drops
predicted frames for. It includes the review's fixes: the two directions of a
stamp are judged apart, a probe's queue toward the viewer counts even when its
round trip is over 3 s, and a skip of predicted units ends only at an IDR,
asking for it again every 2 s. No case below queues 3 s either way, and the
requested-IDR runs made no more host IDRs than before.
The build first submitted for review (`d299970`) was measured the same way
([1](submitted-1.txt), [2](submitted-2.txt), [3](submitted-3.txt)) and agrees
with the after runs. Variants measured on the way are reported where they
explain a decision.

```text
cargo test --release -p pb-mapper-desktop --lib -- --ignored --nocapture --test-threads=1 slow_link
```

ran three times on each side ([before 1](before-1.txt), [2](before-2.txt),
[3](before-3.txt); [after 1](after-1.txt), [2](after-2.txt),
[3](after-3.txt)). Every one of the 20 cases passes in all three after runs.
The before runs fail `reverse_path_queue_is_not_congestion`, the case this
change is for.

## A queue on the viewer's uplink

`reverse_path_queue_is_not_congestion`: 18 ms each way for 6 s, then 300 ms
more on the viewer-to-relay direction only for 15 s, then lifted. The host's
interval figures are from the 14 intervals that cover only time under the
queue; received FPS is the viewer's own figure, sampled once a second from
3 s into the queue (an earlier sample can still cover its first second).

| Measure | Before (3 runs) | After (3 runs) |
| --- | --- | --- |
| Host pace (FPS) | 4 in every interval | 60 in every interval |
| Encoder bitrate (Mbit/s) | 4.15 before the queue, down to 2.24 | 4.15 before, 4.30 rising to 4.90 |
| Window clamped to the delivery rate | in every interval | in none |
| Credit window | 3 frames, 32 KiB | 27 frames, 471 to 538 KB |
| Sent (Mbit/s, mean) | 0.21, 0.21, 0.21 | 4.62, 4.62, 4.62 |
| Viewer received (FPS) | 3.9 to 5.0 | 59.5 to 60.8 |

The host's trace after shows the receipts' reverse queue at 300 to 302 ms and
their forward queue at 1 ms, and the window's time grows by the reverse
queue: 18 ms + 100 ms + 300 ms at 60 FPS is 27 frames. The second after the
queue begins still costs frames: for one round trip no receipt comes back and
the window has yet to learn why, so the sample that covers it read 49.6, 48.7
and 49.7 FPS. Before, both the probes and the receipts read the uplink's queue
as congestion: the pace fell to 4, bits per frame fell 15% every 3 s, and the
byte window sat at its 32 KiB floor until the queue was lifted.

`reverse_queue_with_forward_limit_still_cuts` adds a 3 Mbit/s limit toward the
viewer under the same uplink queue. After, the forward queue reached 223 to
288 ms, the pace fell to 7 or 8 and averaged 37 to 44 FPS over the queue, the
window clamped, the encoder bitrate fell to 3.14 to 3.26 Mbit/s, and the host
sent 2.05 to 2.30 Mbit/s on average: a limit toward the viewer still cuts.
Before, the pace sat at 4 here too, for the uplink's queue.

## Forward congestion as before

The 1 Mbit/s bandwidth-limit case reports when the expected bitrate is back
to 80% of the start after the limit lifts, which the trace samples once a
second. [Per-run summaries](bandwidth_limit.txt)

| Build | Runs | Recovered in about 1 s | about 2 s | 5 s |
| --- | ---: | ---: | ---: | ---: |
| Before | 16 | 2 | 14 | 0 |
| After | 35 | 18 | 16 | 1 |
| After, viewer listing neither capability (round trips) | 32 | 12 | 15 | 5 |
| Submitted build (`d299970`) | 16 | 5 | 11 | 0 |
| Variant: header stamps, split TLS record | 16 | 7 | 5 | 4 |
| Variant: whole-unit stamps, no floor gap | 16 | 4 | 9 | 3 |
| Variant: floor gap on the probe delay only | 16 | 10 | 5 | 1 |
| Variant: floor gap on probes and ack queue | 16 | 11 | 4 | 1 |

The after runs are the three suite runs and 32 alone, one after another. The
round-trip row is the same build, measured right after them, with the viewer
listing neither `arrival_time` nor `keyframe_seq`, so the host times round
trips exactly as before this change. It recovered in 5 s in 5 of 32 runs,
against 1 of 35 with arrival times: the 5 s recoveries belong to the round
trip controller on this machine, not to the one-way delay, and the before
build's 0 of 16, measured earlier, came from a quieter spell. In the after
build's one 5 s run the pace was back at 60 two seconds after the lift, as in
the others. Its bits per frame had been cut by 10% in three 3 s windows, the
last one spanning the lift while the limit's queue drained, where a 2 s run's
were cut by 10%, 5% and 10%, so at 60 FPS the expected bitrate stood at
3.13 Mbit/s, under the case's 3.2 Mbit/s line, until the next window raised
it. The
[rate-model latch record](../2026-11-24-rate-model-latch/README.md) saw
recoveries of 1.0 to 5.0 s on this base too. The plan asked for none at 5 s in
16 runs; neither the after build nor the same build timing round trips met
that here.

The first build stamped a receipt when the reader finished its unit's header,
as the plan said, and the host wrote each video header as its own TLS record
so that the stamp marked the unit's first bytes. It recovered in 5 s in 4 of
16 runs. Two things differ from a round trip there. A header stamp leaves the
unit's own transfer out of the forward queue, which the 100 ms ack-queue
limit was tuned on with the transfer in, and puts it in the reverse queue,
which the window adds to its time. And a round trip under 10 ms counts as
10 ms, which hides the first (10 ms − its least round trip) of any queue; on
loopback that is about 8 ms, which the one-way delay did not hide. Traced side
by side in ten alternating runs of whole-unit stamps without the gap
([trace](traced_bandwidth_limit.txt)), the first cut read a delay of 197 to
212 ms on the round trip, under the 200 ms band edge in 5 runs, where the cut
stops at 40 to 46 FPS; one-way it read 205 to 218 ms, never under 200, and
cut to 10 FPS every time. With the gap hidden it read 195 to 203 ms, under
200 in 11 of 16 runs. The committed build stamps the whole unit (so no split
record is needed) and hides the floor's gap from the probe delay and the ack
queue.

The other cases match before: the LAN case reached the ceiling 978 to 979 ms
after connecting (978 to 980 before); three 600 ms stalls never moved the
pace from 60; the 45 ms encoder and the 350 ms round trip kept 60 FPS; the
slow IDR was followed, not asked for again; the slow presenter held a mean
pace of 13.2 to 14.1 FPS (13.0 to 13.2 before) and the per-second presenter
13.1 to 13.2 (13.1 to 13.2); the 120 FPS consumer received 115 FPS again
6.3 to 7.3 s after its slowdown (6.0 to 7.3 s). One run of the header variant
failed the per-second presenter's bound at a mean pace of 12.86, with the
viewer's own fps cap at 12 for 12 s and the host's controller at 60
throughout: the cap is the viewer's measure of its simulated decoder, which
this change does not touch, and no other run came below 13.0.

## Keyframe requests that name the last seq read

`requested_idr_on_a_slow_link`: a 1 Mbit/s link and a 45 ms encoder; every
2 s the decoder asks for an IDR, as one missing a reference does, and drops
the predicted frames that come first. Eight rounds a run.

| Measure | Before (3 runs) | After (3 runs) |
| --- | --- | --- |
| Wait for the IDR, median (ms) | 364, 365, 404 | 334, 302, 286 |
| Wait for the IDR, max (ms) | 390, 491, 463 | 409, 425, 377 |
| Predicted frames that arrived first, 8 rounds | 33, 39, 35 | 23, 30, 18 |
| Predicted frames the host dropped for the request | 0, 0, 0 | 13, 13, 10 |
| Keyframe requests, host IDRs | 8, 7 in each | 8, 7 in each |

The frames that still arrive first were booked before the request reached
the host, on their way over the 1 Mbit/s link; dropping cannot recall them,
so the count stays near three a round rather than falling to none. The
submitted build read medians of 313, 305 and 316 ms and 23, 24 and 23 frames
first. `idr_another_viewer_asked_for_on_a_slow_link`, where another viewer's
user request brings the IDR and this viewer asks for nothing, is not on this
path: its medians were 421, 320 and 315 ms before and 494, 293 and 401 ms
after (473, 337 and 375 submitted), with the viewer receiving 12.9 to 15.9
FPS before and 17.9 to 19.9 after; its spread is wide.

## Zero cost on the data path

`RUST_LOG=off cargo run --release -p pb-mapper-desktop --example host_send_bench`,
three runs each; medians. The bench's viewer lists both new capabilities, so
every receipt and probe echo it writes carries an arrival time.
[Per-run output](host_send_bench.txt)

| Case | Build | Units/s | Allocations/unit | Allocated bytes/unit |
| --- | --- | ---: | ---: | ---: |
| One viewer, video | Before | 60.0 | 18.86 | 40,198 |
| One viewer, video | Header variant | 60.0 | 20.12 | 40,314 |
| One viewer, video | After | 60.0 | 18.87 | 40,256 |
| One viewer, moving pointer | Before | 60.2 | 58.49 | 46,973 |
| One viewer, moving pointer | After | 60.0 | 58.76 | 47,160 |
| Two viewers, both moving pointers | Before | 60.0 | 165.19 | 100,104 |
| Two viewers, both moving pointers | After | 60.0 | 167.18 | 100,587 |

The split TLS record of the header variant cost 1.26 allocations a unit at
the median (19.92 to 20.29 in its runs, against 18.85 to 18.88), about one
encrypted record more each; stamping the whole unit needs no split, and the
committed build allocates as before (18.85 to 18.90). The bytes allocated a
unit grow by 58, 187 and 483 at the median in the three cases: every receipt
and probe echo the viewer writes now carries an arrival time. The two-viewer
figures also follow the input rate each run reached (484.5 to 499.8 inputs/s
before, 499.5 to 499.9 after). Every run of every build produced 59.9 to 60.2
units a second.

## Left for a device

A real uplink queue: a viewer on a home router running a saturating upload
(`iperf3 -R` from the viewer, or a cloud backup), over a direct QUIC path and
over the relay's TCP, compared with and without this change: the pace,
received FPS and encoder bitrate while it runs, and how fast the transport
grows its own window behind the larger credit window. The in-process hop
delays bytes in user space, so neither QUIC's nor TCP's congestion control
sees the uplink's queue here. An uplink queue over 3 s, where the review's
fixes act, is covered by unit tests only. Windows and Linux viewers run the
same Rust core as this Mac's. A one-direction delay of real loopback with
`dnctl` and `pfctl` needs root, which this session did not have.
