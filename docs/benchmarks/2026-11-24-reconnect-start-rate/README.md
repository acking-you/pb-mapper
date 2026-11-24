# Reconnect start rate: measurements

Release builds on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425) and
Rust 1.99.1. Other worktrees may have been building on the machine at the same
time: the load average at the start of each run was between 1.57 and 3.46
before and between 1.67 and 2.47 after, and each run's figure is at the top of
its raw output. The case runs in one process on loopback, through a real relay
and TLS, with the synthetic encoder of `slow_link.rs` (a per-frame budget of
`bitrate / nominal`, an IDR four times that, following a new bitrate on the
next frame) and a simulated decoder that presents each unit at once; the
viewer reaches the relay through the testkit's shaped hop and lists
`arrival_time`. These are pipeline observations: no GPU encoder's IDR at a low
bitrate, and no real network, is in them.

"Before" is `773c9d7`: `perf2/receipt-rate-control` with the new case and
nothing on the host changed. "After" is `fdcd5b6`, where a session of the same
preset accepted within 2 min of one its transport ended starts at the lower of
its preset's start and the bits per frame that one ended at. Each was built
into one release test binary, and

```text
cargo test --release -p pb-mapper-desktop --lib -- --ignored --nocapture --test-threads=1 slow_link::reconnect
```

ran five times on each ([before 1](before-1.txt), [2](before-2.txt),
[3](before-3.txt), [4](before-4.txt), [5](before-5.txt); [after 1](after-1.txt),
[2](after-2.txt), [3](after-3.txt), [4](after-4.txt), [5](after-5.txt)). Every
run passed. Figures below are the range over the five runs. One run of the
whole suite (`slow_link` as the filter) on the after build passed all 25
cases ([suite-after.txt](suite-after.txt)); each case starts its own host, so
none but this one meets a record.

`reconnect_on_a_slow_link` shapes the viewer's link to 1 Mbit/s each way. A
viewer watches for 8 s and stops; 3 s later another connects over the same
link and watches for 8 s; 3 s after that one stops, a third connects on the
relay's unlimited link. The host's status showed no viewer by the time each
`Viewer::stop` returned, in every run. For each session the case reports the
time from `Viewer::connect` to its first presented picture, that picture's
bytes, and the host's first three once-a-second trace intervals; for the
third, when its encoder bitrate reached 80% of Balanced's start (3.2 Mbit/s).
The trace samples once a second, so its pace and queue figures are samples at
those instants.

## The reconnect on the same link

| Measure | Before | After |
| --- | --- | --- |
| `Viewer::connect` to the first presented picture (ms) | 302.6 to 310.8 | 98.1 to 108.9 |
| First picture, an IDR (bytes) | 33 332 | 8 332 |
| Ack queue, peak over the first three intervals (ms) | 263 to 266 | 0 |
| Bits per frame in the first interval | 66 667 | 19 653 to 19 673 |
| Pace in the first three intervals (FPS) | 60, 48 to 49, 48 to 49 | 60, 36, 43 |
| Sent over the first three intervals (Mbit) | 2.82 to 2.83 | 2.03 |
| Pace, mean of intervals 2 to 7 (FPS) | 47.0 to 48.2 | 44.5 to 47.3 |

The first session ended at Balanced's floor, 16 667 bits per frame, in every
run of both builds, so after the change the reconnect started there: its IDR
was a quarter of the start's, and its first picture came 201 ms sooner at
the median of the runs (105.7 ms against 306.6), about what 25 000 fewer bytes
take on 1 Mbit/s. Before,
the startup second at the startup cap's 30 FPS and Balanced's start, with the
IDR, was more than the link carries: the first interval found 263 to 266 ms
in the receipts' queue, and the cut it brought took bits per frame to the
floor and the pace to 48 or 49. After, nothing queued in the first second, and
the receipts raised bits per frame a little (to 19 653 to 19 673); once the
startup cap lifted, 60 FPS at that rate was more than the link carries, and
the receipts' first cut, with bits per frame at the floor, took the pace to 36
in the second interval, from where it climbed (43, then 45 to 60). Over
intervals 2 to 7 the sampled pace averaged 44.5 to 47.3 FPS after against 47.0
to 48.2 before; the first session, unchanged by the record, averaged 47.3 to
48.5 on both builds.

## The first session, and another viewer on a fast link

The first session has no record to start from, and its figures did not
change: 298.7 to 306.4 ms to the first picture, an IDR of 33 332 bytes, and an
ack queue peak of 264 to 266 ms on both builds.

The third viewer, on the unlimited link, connected 3 s after the reconnect
ended at Balanced's floor, so after the change it started there too, and
climbed as the receipts raise on a clear link: 15% a second plus 100 kbit/s a
second.

| Measure | Before | After |
| --- | --- | --- |
| Encoder bitrate at 80% of Balanced's start after connecting (ms) | 1012 to 1014 (the first interval) | 7011 to 7015 |
| Bits per frame in the first three intervals | 70 015 to 73 524, 76 128 to 79 886, 89 805 to 94 150 | 19 669 to 19 675, 22 036 to 22 053, 27 331 to 27 361 |
| First picture, an IDR (bytes) | 33 332 | 8 332 |
| `Viewer::connect` to the first presented picture (ms) | 21.4 to 24.0 | 18.9 to 23.2 |

That is the cost of a record that knows no viewer: any viewer of the preset
within 2 min of a transport end starts no higher than where that session
ended. On this link it reached the start's 80% 6 s later than without the
record.

## Not measured here

How an IDR from a real encoder (a Windows MFT, VideoToolbox) at the floor's
bitrate compares with one at the start's, which the synthetic encoder fixes at
four times the per-frame budget; real links; and the app's own reconnects,
whose backoff begins at 1 s. A viewer that reconnects before the host saw its
old session end (up to 3 s of silence when the link died) joins the room that
session is still in and gets no record from it.
