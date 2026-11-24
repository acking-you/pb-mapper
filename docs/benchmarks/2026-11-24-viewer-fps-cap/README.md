# Viewer fps cap and backlog drain: measurements

Release builds on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425) and
Rust 1.99.1, with other builds running on the machine at the same time. Each
side ran three times:

`cargo test --release -p pb-mapper-desktop --lib -- --ignored --nocapture --test-threads=1 slow_link`

"Before" is `1515eda` with this change's slow-link cases added and nothing
else changed. "After" is this change. Every case runs in one process on
loopback, through a real relay and TLS, with a synthetic encoder and a
simulated decoder: a thread that takes a unit, sleeps for its decode time and
presents it. None of this is a real decoder; see the last section.

Raw output: [before 1](before-1.txt), [2](before-2.txt), [3](before-3.txt);
[after 1](after-1.txt), [2](after-2.txt), [3](after-3.txt). In the before
build the new cases only asserted that they reached their end state; the
bounds they assert now were set from these runs. The before runs fail two
cases (`slow_presenter_is_capped_by_fps_cap_not_host_backpressure` and
`pipelined_slow_consumer`) on the assertions this change adds. All 16 cases
pass in every after run.

## The fps cap

Values are the three runs in order.

| Case | Measure | Before | After |
| --- | --- | --- | --- |
| Decoder taking 67 ms a frame, 10 to 60 s | backlog episodes | 6, 5, 4 | 0, 0, 0 |
| | keyframe requests | 6, 5, 4 | 0, 0, 0 |
| | mean host pace (FPS) | 12.2, 12.3, 12.3 | 13.1, 13.0, 13.0 |
| 67 ms a frame for 20 s, then 5 ms | cap removed after (s) | 12.0, 9.0, 12.0 | 8.0, 7.0, 7.0 |
| Four units held inside, 67 ms each, 10 to 30 s | backlog episodes | 4, 3, 5 | 0, 0, 0 |
| | keyframe requests | 4, 3, 5 | 0, 0, 0 |
| | receipt to presentation, p95 (ms) | 461, 473, 486 | 262, 358, 354 |
| | presentations | 254, 244, 241 | 252, 247, 247 |
| | mean host pace (FPS) | 13.7, 13.2, 13.1 | 12.6, 12.3, 12.3 |
| 10 ms a frame at 120 FPS, 10 to 30 s | sink wait (receipt to hand-off), p95 (ms) | 51.7, 45.1, 58.1 | 50.9, 47.6, 35.7 |
| | receipt to presentation, p95 (ms) | 65.1, 59.2, 72.8 | 64.5, 61.9, 49.2 |
| | presentations | 1317, 1312, 1303 | 1370, 1414, 1392 |
| | mean host pace (FPS) | 65.2, 65.7, 64.7 | 68.0, 70.2, 69.1 |
| | backlog episodes, units discarded | 0, 0 in each | 0, 0 in each |
| 120 FPS, 18 ms one way, 1 ms a frame but 20 ms for one second | 115 FPS received again after (s) | 15.3, 15.3, 15.3 | 7.3, 7.3, 7.3 |
| | backlog episodes, keyframe requests (whole session) | 2, 2 in each | 3, 2 in each |

In the host's once-a-second trace of the 67 ms decoder, the cap before went
from 10 or 11 to 15 or 16 every few seconds, and each overshoot ended in a
dropped queue and a keyframe request. After, it is 13, 95% of the rate the cap
measured, rounded down, for the whole 50 s (12 to 14 in the first 7 or 8 s
of two runs), with single one-second samples at 15 where a probe was taken
back. The 10 ms decoder at 120 FPS mostly holds at 68 to 71 with probes to 76
to 79, lowered to 58 to 60 once or twice in 20 s; before, the cap climbed by 5
a second to 75 to 85 and fell back to 57 to 60 every 6 to 8 s.

The pipelined decoder models the Linux runner, which keeps up to four units
inside its GStreamer pipeline. Its host pace is lower after (12.3 to 12.6
against 13.1 to 13.7), but it presents as many frames: before, the extra
frames reached the sink and were dropped with the queue. Its latency is
lower, and it no longer asks for keyframes. Its cap cycles between 10 or 11
and 13 to 15, with probes to 15 or 16, every 8 to 10 s; before, it cycled
between 10 or 11 and 20 or 21.

A variant in which a turnaround that raised the capacity also reset the probe
backoff gave the same pipelined pace (12.8, 12.2, 12.4) and
receipt-to-presentation p95 (285, 367, 332 ms) as the version kept. It was
dropped because only a probe that held should reset the backoff: its output
is [variant 1](variant-reset-1.txt), [2](variant-reset-2.txt),
[3](variant-reset-3.txt).

## The backlog drain

A 2 ms decoder stalls once for 400 ms, 10 s into the session. The fresh
picture is the first presentation of a unit received after the stall began,
counted from the stall's end.

| Link | Measure | Before | After |
| --- | --- | --- | --- |
| Unlimited | fresh picture after the stall (ms) | 5.8, 6.0, 6.1 | 8.8, 6.0, 5.3 |
| | longest interval between presentations (ms) | 419, 419, 425 | 421, 423, 419 |
| 1 Mbit/s | fresh picture after the stall (ms) | 344, 400, 447 | 6.0, 6.1, 5.6 |
| | longest interval between presentations (ms) | 462, 483, 465 | 458, 462, 452 |
| Both | keyframe requests, backlog episodes | 1, 1 in each | 1, 1 in each |

On the unlimited link the requested IDR lands before the stall ends, so both
builds show it at once. On the 1 Mbit/s link it lands after: before, the
picture stayed frozen until it did; after, the decoder drains what queued
during the stall and shows it at once. The IDR is still requested at the
same moment, and supersedes the queue when it lands.

## Every other case

Unchanged within run-to-run spread, in all six runs: pace at the ceiling
0.98 s after connecting on the 18 ms path, then 60.0 to 60.1 received FPS;
pace 60 through each 600 ms stall; 59.9 to 60.1 received FPS with a 45 ms
encoder and no skipped tick; pace 60 at a 350 ms RTT; back to 3.2 Mbit/s 2.0 s
after the 1 Mbit/s limit lifts; no keyframe request when IDRs take over a
second to cross; every requested and every other viewer's IDR answered, with
median waits of 371 to 469 ms before and 379 to 432 ms after (requested) and
376 to 426 ms before and 269 to 431 ms after (another viewer's).

## Left for a device

- Linux software decoding (GStreamer, for example 1080p120 on a weak CPU),
  Windows software Media Foundation in a VM, and 4K at 120 FPS on an
  integrated GPU: the measured capacity, the probes and the turnaround rule on
  real decoders, and the pace and latency against a build without them.
- Real decoder stalls on Windows, macOS and Linux viewers: the fresh picture
  after the stall and the present-interval maximum.
- The turnaround's floor on Darwin and Windows, where a presentation waits for
  the compositor: it should stay within 40 ms of the median.
