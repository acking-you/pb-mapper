# Encoder throughput cap: measurements

Release builds on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425)
and Rust 1.99.1. Other agents' builds ran on the machine meanwhile; the
load average before each run is in its raw output (2.1 to 4.5 for the
slow-encoder runs, 5.5 before the whole suite, up to 10.0 for one
`host_send_bench` run). Before is the
base, `26b91fc`, with only `slow_link.rs` from the branch, so that it
carries the new case; after is the branch at `1580eb5`. Before and after
took turns: before, after, before, after, before, after.

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

Each figure is read once a second from 8 s to 48 s after the viewer
connected, 40 seconds that take in the serial encoder's first probe and the
pipelined encoder's second cap: the viewer's received fps and the host's
`encoder_dwell_ms` (the median of the host's last second) and
`skipped_per_s` from its `Stats`.
[Run 1](lan_rtt_keeps_120-1-before.txt), [run 2](lan_rtt_keeps_120-2-after.txt),
[run 3](lan_rtt_keeps_120-3-before.txt), [run 4](lan_rtt_keeps_120-4-after.txt),
[run 5](lan_rtt_keeps_120-5-before.txt), [run 6](lan_rtt_keeps_120-6-after.txt).
The case fails on the base, as it should: its serial encoder's dwell is
39 ms.

### Serial, 11 ms a unit

| Build | Run | Received FPS (mean) | Dwell, median of the 40 s | Dwell, mean of the 40 s | Seconds with dwell over 25 ms | Skipped ticks a second: mean, most in a second | Pace |
| --- | ---: | ---: | ---: | ---: | ---: | --- | --- |
| before | 1 | 90.9 | 39 ms | 39.0 ms | 40 of 40 | 29.1, 30 | 120 |
| | 3 | 90.9 | 39 ms | 39.2 ms | 40 of 40 | 29.1, 30 | 120 |
| | 5 | 90.9 | 39 ms | 39.2 ms | 40 of 40 | 29.1, 30 | 120 |
| after | 2 | 85.7 | 11 ms | 12.1 ms | 1 of 40 | 0.1, 5 | 85, then 89 to 97, then 86 |
| | 4 | 85.8 | 11 ms | 11.7 ms | 1 of 40 | 0.2, 6 | 85, then 89 to 97, then 86 |
| | 6 | 85.7 | 11 ms | 12.0 ms | 1 of 40 | 0.1, 6 | 85, then 89 to 97, then 86 |

- Before, the encoder held four units: each waited behind three, 39 or
  40 ms in every second, and the 29 ticks a second the full encoder skipped
  spaced the frames unevenly.
- After, the first full second (27 full ticks, 90 units, a median dwell of
  39.3 or 39.4 ms) capped the pace at 85, 2 s after the viewer connected,
  and the guard kept it 2 s later, the dwell having fallen to 11.8 ms. The
  dwell was 11 or 12 ms, one service time, in every second outside the
  probe's two or three, at 85.7 to 85.8 FPS received, 5.1 to 5.2 below the
  90.9 of a full encoder.
- 30 s after the cap was measured, the probe raised it by a twentieth a
  second (89, 93, 97); at 97 the encoder filled again (5 or 6 full ticks),
  and the cap fell to 86. That cost one second at 38 ms, with the one before
  it at 21 or 23 ms in two runs and the one after at 12 or 17 ms in two, and
  5 or 6 skipped ticks. The next probe waits 60 s, past the end of the
  case.

Every `desktop_encoder_cap` the host traced is in the raw output, with the
time since the viewer connected, for example run 2:

```text
t_ms=1980 capped:85 asked=120 full_ticks=27 units=90 dwell_before_ms=39.3
t_ms=3981 kept:85 asked=120 full_ticks=0 units=85 dwell_before_ms=39.3 dwell_after_ms=11.8
t_ms=31992 raised:89 ... t_ms=32993 raised:93 ... t_ms=33995 raised:97
t_ms=34996 lowered:86 asked=120 full_ticks=5 units=91
```

### Pipelined, 45 ms a unit

| Build | Runs | Received FPS (mean) | Dwell, median | Skipped ticks a second (mean) | Pace |
| --- | --- | ---: | ---: | ---: | --- |
| before | 1, 3, 5 | 78.6 to 80.0 | 45 ms | 39.5 to 40.0 | 120 |
| after | 2, 4, 6 | 79.4 to 79.8 | 45 ms | 37.9 to 38.0 | 120, 75 for 2 s |

In every after run the first full second (40 full ticks, 78 or 79 units)
capped the pace at 74 or 75, and 2 s later the guard found the dwell where
it was (45.8 to 46.1 ms against 45.7 to 46.1 ms) and lifted the cap,
setting none for 30 s. At 34 s the same happened again (80 units, capped
at 75), and the next hold is 60 s. The two seconds of the second cap, in
the window, ran at 75.

### The rest of the slow-link suite

[Raw output](slow_link-after.txt) of the whole suite on the branch:
26 of 26 passed, in 727 s, at a load average of 5.5 to start. Only
`lan_rtt_keeps_120` capped (its serial case at 85.8 FPS received with one
second over 25 ms, its pipelined case at 79.7): the suite's other encoders
keep each unit 8 ms (at 60 and at 120 FPS) or 45 ms at 60 FPS, at most
three in flight, so none fills to four.

## Host allocations: `host_send_bench`

`RUST_LOG=off cargo run --release -p pb-mapper-desktop --example host_send_bench`,
after, before, after, before ([raw output](host_send_bench.txt)). The cap
adds two counters to thread C's one-second window, and work only at each
roll:

| Case | Before: allocations a unit | After | Before: bytes a unit | After |
| --- | --- | --- | --- | --- |
| `video` | 18.85, 18.89 | 18.83, 18.86 | 40 251, 40 283 | 40 255, 40 255 |
| `video+pointer` | 58.82, 58.53 | 58.66, 58.17 | 47 129, 47 061 | 47 126, 47 033 |
| `2viewers+pointer` | 164.68, 166.41 | 166.98, 160.47 | 100 106, 100 451 | 100 578, 99 427 |

The `2viewers+pointer` figures move with the inputs a run injected (482.5
to 499.8 a second).

## A first version

[`first-version/`](first-version/) has the runs of a first version, over
8 s to 18 s, whose cap rose by a twentieth a second after any three seconds
without a full tick, idle ones included, and remembered nothing. It probed
past the serial encoder every 6 s and filled it again for 2 to 4 seconds in
each 10, at 27 to 39 ms. A tenth of the pace in full ticks was needed to
cap, so an encoder within a tenth of the pace was never capped; a pace asked
at or below the cap cleared it; a slow encoder's cap was judged before its
queue drained. Review of it led to the rule measured above.

## Device runs still needed

- `video_bench` at 4K and 1440p, nominal 120, on AMD and Intel Windows hosts,
  in process and through the helper (`--backend`): the encoder's dwell and
  whether it fills to four.
- A live session on those hosts at 4K and 1440p and 120 FPS, before and
  after: `encoder_dwell_ms`, full ticks a second (`desktop_capture_pacing`'s
  `full_ticks_per_s` with `RUST_LOG=pb_mapper_desktop::feedback=debug`),
  received fps, and the `desktop_encoder_cap` changes.
- Whether a real encoder's throughput varies enough with the picture that
  the cap is lowered or lifted where it should not be, and how often a probe
  fills it.
