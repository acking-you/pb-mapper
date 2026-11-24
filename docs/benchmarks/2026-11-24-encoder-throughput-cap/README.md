# Encoder throughput cap: measurements

Release builds on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425)
and Rust 1.99.1. Other agents' builds ran on the machine meanwhile; the
load average before each of the last three runs is in its raw output (2.6
to 4.6). Before is the base, `26b91fc`, with only `slow_link.rs` from the
branch, so that it carries the new case; after is the branch (`71937ce`).
Before and after took turns: before, after, after (inside a run of the whole
slow-link suite), before, before, after.

Everything here is synthetic: a relay, a host and a viewer in one process on
loopback, with a synthetic encoder and decoder. No real encoder was measured.
Whether an AMD or Intel Media Foundation encoder at 4K or 1440p and 120 FPS
fills to four units, and so caps the pace, needs a Windows device
([Device runs still needed](#device-runs-still-needed)).

## Slow encoders at 120 FPS: `lan_rtt_keeps_120`

`cargo test --release -p pb-mapper-desktop --lib -- --ignored --nocapture --test-threads=1 slow_link::lan_rtt_keeps_120`.
A Balanced session at the viewer's own 120 FPS, so the ceiling is 120, over
the suite's short path (18 ms each way), with a decoder that takes no time.
The case runs two synthetic H.264 encoders at 1080p one after the other:

- `serial_11ms` serves a picture each 11 ms, one at a time, while it takes up
  to thread C's four, as a Media Foundation encoder slower than the pace and
  serial at its bottleneck does: it delivers at most 1000 / 11, about 91 a
  second;
- `pipelined_45ms` keeps each picture 45 ms but takes many at once: at four
  in flight it delivers about 80 a second, its latency, not its throughput,
  being what fills it.

Each figure is read once a second from 8 s to 18 s after the viewer
connected: the viewer's received fps and the host's `encoder_dwell_ms` (the
median of the host's last second) and `skipped_per_s` from its `Stats`.
[Run 1](lan_rtt_keeps_120-1-before.txt), [run 2](lan_rtt_keeps_120-2-after.txt),
[run 3](slow_link-after.txt), [run 4](lan_rtt_keeps_120-4-before.txt),
[run 5](lan_rtt_keeps_120-5-before.txt), [run 6](lan_rtt_keeps_120-6-after.txt).

### Serial, 11 ms a unit

| Build | Run | Received FPS (mean) | Dwell, median of the 10 s | Dwell, mean of the 10 s | Seconds with dwell over 22 ms | Skipped ticks a second (mean) | Pace |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| before | 1 | 90.8 | 39 ms | 39.2 ms | 10 of 10 | 29.1 | 120 |
| | 4 | 90.9 | 39 ms | 39.0 ms | 10 of 10 | 29.1 | 120 |
| | 5 | 91.0 | 39 ms | 39.0 ms | 10 of 10 | 29.1 | 120 |
| after | 2 | 88.8 | 11 ms | 20.5 ms | 4 of 10 | 1.0 | 86 to 97 |
| | 3 | 88.3 | 11 ms | 15.2 ms | 2 of 10 | 0.4 | 86 to 94 |
| | 6 | 88.3 | 11 ms | 18.3 ms | 3 of 10 | 1.0 | 86 to 94 |

- Before, the encoder held four units: each waited behind three, 39 ms at
  every second's median, and the 29 ticks a second the full encoder skipped
  spaced the frames unevenly.
- After, the first full second (26 or 27 full ticks, 90 units) capped the
  pace at 85, and the guard kept it 2 s later: the dwell had fallen from 39.2
  to 39.5 ms (by run) to 11.8 ms. The median dwell is one service time,
  11 ms, and the received rate 2 to 3 FPS lower, 88.3 to 88.8.
- The cap probes upward: after three quiet seconds it rises by a twentieth
  each second (85, 89, 93 and 97 the first time, then 86, 90 and 94), and a
  second or two at 93 or more fill the encoder again (3 to 6 full ticks),
  which lowers the cap to 86. That cycle repeats every 6 s, and its filling
  seconds are the 2 to 4 a run with a dwell of 27 to 39 ms. The rule as
  briefed has no memory of the throughput it found; one that stopped raising
  short of the last cap that filled the encoder would avoid them.

Every `desktop_encoder_cap` the host traced is in the raw output, with the
time since the viewer connected, for example run 6:

```text
t_ms=1980 capped:85 asked=120 full_ticks=27 units=90 dwell_before_ms=39.3
t_ms=3981 kept:85 asked=120 full_ticks=0 units=85 dwell_before_ms=39.3 dwell_after_ms=11.8
t_ms=4981 raised:89 ... t_ms=5981 raised:93 ... t_ms=6981 raised:97
t_ms=7982 lowered:86 asked=120 full_ticks=6 units=91
t_ms=10982 raised:90 ... t_ms=11982 raised:94
t_ms=13983 lowered:86 asked=120 full_ticks=3 units=91
```

### Pipelined, 45 ms a unit

| Build | Runs | Received FPS (mean) | Dwell, median | Skipped ticks a second (mean) | Pace |
| --- | --- | ---: | ---: | ---: | --- |
| before | 1, 4, 5 | 79.9 to 80.0 | 45 ms | 39.9 to 40.0 | 120 |
| after | 2, 3, 6 | 80.0 | 45 ms | 40.0 | 120 |

In every after run the first full second (40 full ticks, 79 units) capped
the pace at 75, and 2 s later the guard found the dwell where it was (46.1
against 46.0 or 46.1 ms) and lifted the cap, setting none for 30 s: from 8 s
to 18 s the session ran as before. The cap cost the two seconds between, at
75.

### The rest of the slow-link suite

The whole suite passed on the branch, 26 of 26
([raw output](slow_link-after.txt)). Its other encoders keep each unit 8 ms
(at 60 and at 120 FPS) or 45 ms at 60 FPS, at most three units in flight, so
none fills to four and none is capped.

## Host allocations: `host_send_bench`

`RUST_LOG=off cargo run --release -p pb-mapper-desktop --example host_send_bench`,
after then before ([raw output](host_send_bench.txt)). The cap adds two
counters to thread C's one-second window, and work only at each roll:

| Case | Before: allocations a unit | After | Before: bytes a unit | After |
| --- | ---: | ---: | ---: | ---: |
| `video` | 18.85 | 18.76 | 40 254 | 40 121 |
| `video+pointer` | 58.40 | 58.23 | 47 018 | 47 056 |
| `2viewers+pointer` | 167.08 | 167.22 | 100 568 | 100 588 |

## Device runs still needed

- `video_bench` at 4K and 1440p, nominal 120, on AMD and Intel Windows hosts,
  in process and through the helper (`--backend`): the encoder's dwell and
  whether it fills to four.
- A live session on those hosts at 4K and 1440p and 120 FPS, before and
  after: `encoder_dwell_ms`, full ticks a second (`desktop_capture_pacing`'s
  `full_ticks_per_s` with `RUST_LOG=pb_mapper_desktop::feedback=debug`),
  received fps, and the `desktop_encoder_cap` changes.
- Whether a real encoder's throughput varies enough with the picture that
  the cap is lowered or lifted where it should not be.
