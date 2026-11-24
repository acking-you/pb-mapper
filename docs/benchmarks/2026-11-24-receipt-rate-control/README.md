# Receipt-driven rate control: measurements

Release builds on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425) and
Rust 1.99.1. Other worktrees were building on the machine at the same time:
the load average at the start of each suite run was between 1.18 and 4.54,
and each run's figure is at the top of its raw output. Every case runs in one
process on loopback, through a real relay and TLS, with the synthetic encoder
of `slow_link.rs` (a per-frame budget of `bitrate / nominal` that follows a
new bitrate on the next frame unless stated) and a simulated decoder; the
viewer reaches the relay through the testkit's shaped hop and lists
`arrival_time`. These are pipeline observations: no GPU encoder's response to
a cut, and no real network, is in them.

"Before" is `1905d79`: `perf2/viewer-feedback` with the new slow-link cases
and trace fields and nothing on the host changed. "After" is `99d114d`, the
receipt-driven rate control. Each was built into one release test binary, and

```text
cargo test --release -p pb-mapper-desktop --lib -- --ignored --nocapture --test-threads=1 slow_link
```

ran five times on each, the before runs first, then the after runs
([before 1](before-1.txt), [2](before-2.txt), [3](before-3.txt),
[4](before-4.txt), [5](before-5.txt); [after 1](after-1.txt),
[2](after-2.txt), [3](after-3.txt), [4](after-4.txt), [5](after-5.txt)).
Figures below are the range over the five runs. Every one of the 24 cases
passes in all five after runs. The before runs fail
`moderate_limit_keeps_60`, `sudden_drop` and `recurring_stalls_cut` every
time, the cases this change is for, and `slow_presenter` once, at a mean pace
of 12.88 FPS against its 13.0 bound (the arrival-time record saw that case
at 12.86 once under load too).

The trace samples once a second, so every time below is to that resolution.
Bits per frame now rise 15% a second plus 100 kbit/s a second on a clear
link, so a case that waits 6 s before its limit meets it at Balanced's
maximum of 8 Mbit/s at 60 FPS after the change and at 4.15 Mbit/s before.

## Limits on the link

`bandwidth_limit`: 1 Mbit/s for 8 s after 6 s unlimited, then unlimited.
"Last 5 s" is the limit's last five seconds: the host's pace, and the
viewer's own received FPS sampled once a second. The ack queue is the host's
receipt queue toward the viewer, traced once a second over the limit.

| Measure | Before | After |
| --- | --- | --- |
| Encoder bitrate when the limit began (Mbit/s) | 4.15 | 7.43 to 8.00 |
| Lowest pace (FPS) | 10 to 11 | 44 to 46 |
| Pace over the last 5 s (FPS, mean) | 15.0 to 21.0 | 49.6 to 51.0 |
| Viewer received over the last 5 s (FPS, mean) | 13.7 to 14.5 | 50.2 to 51.1 |
| Lowest bits per frame | 58 050 to 64 679 | 16 667 (Balanced's floor) |
| Ack queue, 95th percentile over the limit (ms) | 211 to 260 | 720 to 724 |
| Ack queue over the last 5 s (ms, mean) | 128 to 184 | 5 to 12 |
| Sent over the link's rate | 0.90 to 0.98 | 0.86 to 0.88 |
| Receipt cuts | 0 | 4 |
| Back to 80% of the start after the lift (s) | 1 (2 runs), 2 (3 runs) | 4 (all 5) |

The frame rate holds: bits per frame are at the floor in the first interval
traced in the limit, and the pace follows what the link delivers at that
floor, 44 to 60 FPS. The 95th percentile of the ack queue is that first
second: the drop from 8 Mbit/s to 1 traced an ack queue of 720 to 724 ms and a
standing queue of 586 to 593 ms there, what was in flight before the first
receipts showed the limit; over the last 5 s the queue averages 5 to 12 ms.
Recovery is slower: before, bits per
frame barely moved and only the pace came back, within 1 to 2 s; now bits per
frame climb back from the floor, at 4% plus 100 kbit/s a second until 1.2 ×
the limit's capacity and then at 50% a second, and reach 80% of the start
(3.2 Mbit/s) 4 s after the lift in every run. No run took 5 s.

`moderate_limit_keeps_60`: 3 Mbit/s for 10 s after 6 s unlimited.

| Measure | Before | After |
| --- | --- | --- |
| Encoder bitrate when the limit began (Mbit/s) | 4.15 | 8.00 |
| Lowest pace (FPS) | 60 | 60 |
| Viewer received over the last 5 s (FPS, mean) | 38.3 to 38.6 | 59.9 to 60.1 |
| Lowest encoder bitrate (Mbit/s) | 4.15 to 4.30 | 2.41 to 2.46 |
| Ack queue, 95th percentile (ms) | 120 to 126 | 55 to 75 |
| Ack queue over the last 5 s (ms, mean) | 118 to 120 | 14 to 17 |
| Sent over the link's rate | 0.97 to 0.98 | 0.89 to 0.92 |
| Receipt cuts | 0 | 3 |
| Back to 80% of the start after the lift (s) | 0 to 1 | 1 to 2 |

Before, the probe's delay stayed under 150 ms (98 to 129 ms in the trace),
so the pace stayed at 60, but the window, clamped to the delivery rate, held
units back: the viewer received 38 FPS. After, bits per frame take the limit
and the viewer receives 60.

`sudden_drop`: unlimited until bits per frame reach Balanced's maximum (at
most 40 s), then 5 Mbit/s for 10 s.

| Measure | Before | After |
| --- | --- | --- |
| Bits per frame at the maximum after (s) | not in 40 s | 6.0 |
| Encoder bitrate at the drop (Mbit/s) | 5.80 to 5.95 | 8.00 |
| Ack queue, peak (ms) | 121 to 130 | 52 to 66 |
| Ack queue over the last 5 s (ms, mean) | 119 to 122 | 14 to 20 |
| Viewer received over the last 5 s (FPS, mean) | 44.6 to 45.9 | 60.0 to 60.1 |
| Sent over the link's rate | 0.93 to 0.95 | 0.86 to 0.91 |
| Receipt cuts | 0 | 3 |

Before, the 3 s windows had raised bits per frame only to 5.8 to 5.95 Mbit/s
in 40 s, so its drop was smaller; the queue still stood at 120 ms throughout.

`per_second_encoder_on_a_bandwidth_limit`: the 1 Mbit/s limit on an encoder
that budgets per second at the nominal model, which spends the nominal
bitrate at any pace. A cut of bits per frame lowers that bitrate; the pace
cap below the floor does not.

| Measure | Before | After |
| --- | --- | --- |
| Lowest pace (FPS) | 2 | 10 to 45 |
| Viewer received over the last 5 s (FPS, mean) | 0.2 | 6.3 to 13.1 |
| Sent over the link's rate | 1.94 to 2.22 | 0.93 to 1.01 |
| Ack queue over the last 5 s (ms, mean) | 259 to 264 | 104 to 183 |
| Back to 80% of the start after the lift (s) | 1 | 4 (2 runs), 5 (2 runs), 6 (1 run) |

The pictures still fill the link, as the encoder spends the floor's nominal
bitrate whatever the pace; a Mac host sheds the rest only once its sessions
start at the timestamp model (a separate change).

## Delay that is no congestion, and stalls that are

`added_route_delay`: 18 ms each way, then 60 ms more each way (120 ms on the
round trip) for 20 s, with no path change reported.

| Measure | Before | After |
| --- | --- | --- |
| Encoder bitrate before (Mbit/s) | 4.30 | 8.00 |
| Receipt cuts | 0 | 2, then a rebase |
| Lowest pace (FPS) | 60 | 60 |
| Lowest encoder bitrate (Mbit/s) | 4.30 | 5.73 to 6.20 |
| Back at the bitrate before (s) | never below it | 6 (4 runs), 7 (1 run) |

Two cuts left the receipts' queue where it was, so the receipts rebased onto
the longer route and the probes' baseline moved up with them; the recovery
climbs back toward 90% of the rate before the cuts at 50% a second.

`isolated_stall_does_not_collapse_fps`: three 600 ms stalls, 6 s apart. No
receipt cut in any run either side; the pace stayed at 60 through each, and
after the change the encoder bitrate never fell (the case now asserts both).

`recurring_stalls_cut`: 300 ms stalls every second for 10 s. Before, the
encoder bitrate never fell (the probes' median of three hides one stall a
second, and the 3 s windows raised bits per frame throughout), so the case
fails. After, two cuts within the first 2 s (8.00 to 5.03 to 5.09 Mbit/s);
the stalls went on as before, so they were excused, and the rate was back at
8 Mbit/s 4 to 5 s into them; the pace stayed at 60.

`reverse_path_queue_is_not_congestion` (300 ms queued on the viewer's uplink
only) passes either side, with the viewer receiving 59.9 to 60.1 FPS on
average from 3 s into the queue. `reverse_queue_with_forward_limit_still_cuts`
(the same with 3 Mbit/s toward the viewer) passes either side; from 3 s into
the queue the viewer received a mean of 30.8 to 40.0 FPS (lowest second 8.9 to
12.9) before and 59.5 to 59.9 (lowest 53.7 to 57.6) after.

## Clear paths

| Case | Measure | Before | After |
| --- | --- | --- | --- |
| `lan_rtt_keeps_60` (18 ms each way) | Pace at the ceiling after connecting (ms) | 978 to 979 | 978 |
| | Bits per frame at Balanced's maximum after (ms) | not in 30 s (89 167 at 30 s) | 5979 to 5983 |
| | Received FPS, 10 s mean | 59.9 to 60.1 | 59.9 to 60.1 |
| `fixed_wan_keeps_full_rate_at_fixed_resolution` (350 ms RTT) | Pace at the ceiling after connecting (ms) | 5824 to 5827 | 823 |
| | Expected bitrate after 20 s (Mbit/s) | 4.91 | 8.00 |

Balanced now starts at its ceiling at the first probe whatever its round
trip; before, a first probe over 50 ms left it at 45 and the probes climbed.

## Keyframes on a slow link

| Case | Measure | Before | After |
| --- | --- | --- | --- |
| `idr_slower_than_a_second_is_followed_not_replaced` | Host keyframes, requests | 1, 0 | 1, 0 |
| | Lowest encoder bitrate (Mbit/s) | 2.62 to 2.92 | 1.00 |
| | Viewer received FPS at the end | 18.8 to 20.8 | 47.6 to 54.6 |
| `requested_idr_on_a_slow_link` | Wait for the IDR, median (ms) | 286 to 361 | 125 to 135 |
| | Wait for the IDR, max (ms) | 323 to 418 | 130 to 164 |
| | Predicted frames that arrived first, 8 rounds | 20 to 27 | 7 to 17 |
| `idr_another_viewer_asked_for_on_a_slow_link` | Wait for the IDR, median (ms) | 238 to 602 | 128 to 138 |
| | Host keyframes over 8 rounds | 17 to 24 | 7 |

With a short queue in front of it, an IDR crosses the 1 Mbit/s link sooner,
and in the room the host made 7 IDRs over the 8 rounds instead of 17 to 24.

## Unchanged

| Case | Measure | Before | After |
| --- | --- | --- | --- |
| `encoder_latency_of_45ms_keeps_60` | Received FPS, 10 s mean; skipped ticks | 60.0 to 60.1; 0 | 59.9 to 60.1; 0 |
| `slow_presenter_is_capped_by_fps_cap_not_host_backpressure` | Mean pace from 10 s to 60 s | 12.9 to 13.2 | 13.1 to 13.2 |
| `per_second_encoder_latches_under_an_fps_cap` | Latched into its low stretch (ms); sent over expected after | 2010 to 2016; 1.00 to 1.01 | 2014 to 2019; 0.99 to 1.00 |
| `pipelined_slow_consumer` | Mean pace; receipt to presentation, p95 (ms) | 12.2 to 12.8; 290 to 406 | 12.2 to 12.6; 299 to 384 |
| `fast_consumer_at_120` | Mean pace | 69.9 to 71.8 | 67.7 to 71.8 |
| `injected_backlog_at_120_recovers` | 115 FPS again after (ms) | 6281 to 7291 | 6285 to 7293 |
| `slow_decoder_speeds_up` | Cap removed after (ms) | 5002 to 8003 | 7003 to 8004 |
| `consumer_stall_drains` and its 1 Mbit/s twin | Fresh picture after the stall (ms) | 5.7 to 6.1 | 5.1 to 6.1 |

`sustained_slow_tls_reader`, whose 64 KiB pictures ignore the bitrate, skips
1592 to 1593 capture ticks in 30 s after against 547 to 986 before: its pace
stays at 60 (it was 2 to 60 at the 10 s and 20 s samples before), so more
ticks find the window full; it captured 122 to 123 pictures against 116 to
122, and control, release and revoke took as long as before.

## Zero cost on the data path

`RUST_LOG=off cargo run --release -p pb-mapper-desktop --example host_send_bench`,
three runs each, the two builds taking turns. [Per-run output](host_send_bench.txt)

| Case | Build | Units/s | Allocations/unit | Allocated bytes/unit |
| --- | --- | ---: | ---: | ---: |
| One viewer, video | Before | 60.0 to 60.2 | 18.83 to 18.88 | 40 242 to 40 259 |
| One viewer, video | After | 60.0 | 18.84 to 18.89 | 40 268 to 40 298 |
| One viewer, moving pointer | Before | 60.0 | 58.72 to 58.77 | 47 127 to 47 145 |
| One viewer, moving pointer | After | 60.2 | 58.57 to 58.59 | 47 050 to 47 063 |
| Two viewers, both moving pointers | Before | 60.0 | 166.39 to 167.31 | 100 467 to 100 591 |
| Two viewers, both moving pointers | After | 60.0 | 167.07 to 167.34 | 100 552 to 100 635 |

A receipt allocates nothing: the history is a ring reserved once, the busy
median a stack array, and the stall ring fixed.

## The closed loop

`cargo test -p pb-mapper-desktop --lib qos::` runs a deterministic closed loop
of the controller, the credit ledger and a FIFO bottleneck in virtual time
(`qos/sim.rs`): a clear path, drops to 3 and 1 Mbit/s and the lift, an
encoder whose rate trails its bitrate by 0.7 s, a per-second encoder, an
isolated 600 ms stall, 300 ms stalls every second, a route 120 ms longer, a
queue on the way back only, 4× IDRs every 2 s at 10% above Balanced's start,
a still picture, and Quality's order of cuts. Its figures are a model's, not
measurements, and are in the test's assertions only.

## Left for a device

- How each encoder follows a cut: the NVIDIA, AMD and Intel MFTs on Windows
  (`video_bench --rate-steps 8,2,8`) and VideoToolbox on a Mac, which now get
  a new bitrate up to about four times a second while the rate climbs, and a
  cut at most once per hold. The synthetic encoder here follows at once.
- Real Wi-Fi, cellular and relay paths: how often reverse-path jitter, Wi-Fi
  aggregation and power save trip `standing_ms`, `busy_bps` and `cuts` against
  `ack_queue_ms`, with and without arrival times.
- Whether 50 FPS at Balanced's floor looks better than 10 FPS at a higher
  bits per frame on a constrained link: the owner's call, per preset.
