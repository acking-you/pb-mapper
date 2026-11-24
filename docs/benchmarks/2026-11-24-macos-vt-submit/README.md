# Mac host VideoToolbox submit thread: measurements

A Mac host's thread C made every VideoToolbox call itself. A session whose
size is a multiple of 16 encodes inside `VTCompressionSessionEncodeFrame`, so
thread C sat in that call for the whole encode while input queued on its lane.
Each encoder now has its own submit thread that makes every call on the
session; thread C hands it a picture or a bitrate through a one-slot mailbox.
A submit that finds the thread busy leaves its tick armed until the thread
wakes thread C. A Mac host also holds VideoToolbox to two units in flight
(`VT_IN_FLIGHT`) instead of thread C's four (`ENCODER_DEPTH`). See
[Host: capture and encoding](../../remote-desktop.md#host-capture-and-encoding).

## How it was measured

Two ignored tests in `crates/pb-mapper-desktop/src/platform/macos/tests.rs`
drive a real VideoToolbox session. They need no Screen Recording grant:

```
cargo test --release -p pb-mapper-desktop --lib -- --ignored --nocapture --test-threads=1 video_toolbox_submit_blocking
cargo test --release -p pb-mapper-desktop --lib -- --ignored --nocapture --test-threads=1 video_toolbox_in_flight
```

Each case paces one low-latency encoder the way thread C paces it:

- The pictures are eight IOSurface NV12 pictures, painted before the run, so
  the pacing thread does nothing else.
- Each tick is taken at its start, and two ticks are never closer than three
  quarters of a period (`pacer::Pacer`).
- The encoder runs at the case's pace as its nominal rate.
- The bitrate is Balanced's starting 66,667 bits per frame at that pace,
  scaled by `(pixels / 1080p)^0.75` as a session's QoS scales it.
- A reader thread queues an input every millisecond and wakes the pacing
  thread. The pacing thread takes the queued inputs before and after each
  submit, as thread C drains its lane, and records how long each one waited.

Each case warms up for 1 s and is then measured for 5 s. It reports:

- pictures submitted and units out, per second;
- ticks spent on a full gate (`skipped_per_s`);
- due ticks that found the submit thread busy and stayed armed (`armed_per_s`;
  after only);
- the submit call on the pacing thread (p50, p99, max);
- the queued inputs' wait (p50, p99, max);
- the units' dwell, from submit to output (p50, p95).

**Before** is commit `3aeef96`: the measurement tests on the base,
`perf2/mac-wake-display` (`5491036`). There thread C makes the VideoToolbox call,
and the gate is `pending() >= depth` with the depth at `ENCODER_DEPTH` (four).
**After** is this branch: the submit thread, `Encoder::admit` at
`VT_IN_FLIGHT` (two), and a submit that found the thread busy waits for its
wake, as `MacVideo::idle_until` makes thread C wait. In `video_toolbox_in_flight`
both arms run each case with the gate at four and at two.

`video_toolbox_submit_blocking` ran three times per arm. Run 1 ran through
cargo, before then after. Runs 2 and 3 ran the two saved release test binaries
in turn: before 2, after 2, before 3, after 3. `video_toolbox_in_flight` ran
once per arm. Both arms of `video_toolbox_rate_model_h264` and `_h265` ran
once; the H.265 one then ran once more per arm.

The runs were release builds on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0
(26A425), Rust 1.99.1. Other agents' builds were running on the machine, with
load averages of 1.6 to 8.1 at the runs' starts (the first line of each file).
The Mac had its screen locked, which VideoToolbox does not need.

Raw output:

- submit blocking: [before 1](before-submit-1.txt), [after 1](after-submit-1.txt),
  [before 2](before-submit-2.txt), [after 2](after-submit-2.txt),
  [before 3](before-submit-3.txt), [after 3](after-submit-3.txt);
- in flight: [before](before-in-flight-1.txt), [after](after-in-flight-1.txt);
- rate model: [before](before-rate-model-1.txt), [after](after-rate-model-1.txt);
  H.265 again: [before](before-rate-model-h265-2.txt),
  [after](after-rate-model-h265-2.txt).

## Submit call and input wait

`video_toolbox_submit_blocking`: the median of three runs, with the range in
brackets. Times are in microseconds. The gate is four units before and two
after.

| Size | Codec | Pace | Submit call p99 | Input wait p99 | Input wait max |
| --- | --- | ---: | ---: | ---: | ---: |
| 1920x1080 | H.264 | 60 | 78 (77–87) → 9 (9–14) | 45 (45–46) → 49 (46–52) | 160 (130–321) → 401 (302–515) |
| 1920x1080 | H.264 | 120 | 62 (61–73) → 11 (10–12) | 47 (46–48) → 47 (47–48) | 288 (180–751) → 481 (412–1289) |
| 1920x1080 | H.265 | 60 | 66 (62–76) → 14 (12–15) | 46 (45–47) → 45 (45–53) | 242 (80–263) → 385 (305–518) |
| 1920x1080 | H.265 | 120 | 63 (63–71) → 9 (8–10) | 45 (44–45) → 45 (44–47) | 155 (125–370) → 478 (333–540) |
| 1920x1200 | H.264 | 60 | 5995 (5942–6054) → 12 (11–15) | 5236 (5174–5336) → 45 (45–47) | 6160 (5750–6282) → 171 (166–1848) |
| 1920x1200 | H.264 | 120 | 6160 (5557–6175) → 7 (7–9) | 5252 (4582–5373) → 47 (44–47) | 6800 (6430–7220) → 671 (253–1001) |
| 1920x1200 | H.265 | 60 | 7184 (7038–14807) → 8 (8–11) | 5176 (4934–12356) → 46 (46–4416) | 7177 (7148–16564) → 266 (161–8223) |
| 1920x1200 | H.265 | 120 | 6843 (6724–6986) → 6 (5–7) | 5008 (4989–5078) → 45 (43–48) | 6773 (6761–6887) → 385 (334–916) |
| 2560x1440 | H.264 | 60 | 9019 (8951–9037) → 10 (8–15) | 7724 (7470–7770) → 46 (45–51) | 8919 (8701–8989) → 483 (346–6397) |
| 2560x1440 | H.264 | 120 | 8796 (8723–8938) → 8 (5–10) | 7610 (7555–7819) → 45 (45–49) | 8821 (8450–9444) → 304 (260–525) |
| 2560x1440 | H.265 | 60 | 10423 (10378–10688) → 8 (7–8) | 7335 (7270–7384) → 46 (46–3326) | 10587 (10030–12113) → 1222 (221–5526) |
| 2560x1440 | H.265 | 120 | 10051 (10051–10059) → 6 (5–7) | 7479 (7342–7549) → 44 (40–45) | 10059 (9812–12042) → 369 (253–966) |
| 3328x2160 | H.264 | 60 | 15640 (15207–15871) → 7 (6–11) | 13445 (13417–13472) → 46 (45–46) | 15635 (15460–15659) → 438 (352–3339) |
| 3328x2160 | H.264 | 120 | 15934 (15243–15959) → 5 (5–6) | 13795 (13786–13866) → 45 (44–46) | 15985 (15816–16093) → 207 (143–341) |
| 3328x2160 | H.265 | 60 | 18474 (18392–18728) → 8 (7–10) | 12786 (12611–12800) → 45 (44–46) | 18515 (18208–18708) → 143 (121–1310) |
| 3328x2160 | H.265 | 120 | 18293 (18185–18309) → 6 (6–7) | 14943 (14879–14967) → 46 (44–47) | 18370 (18242–18635) → 942 (238–1005) |
| 3840x2160 | H.264 | 60 | 17949 (17848–17979) → 9 (6–14) | 15312 (15277–15575) → 45 (44–45) | 17995 (17876–18384) → 250 (134–627) |
| 3840x2160 | H.264 | 120 | 17955 (17875–18103) → 6 (6–6) | 15780 (15704–15891) → 45 (44–46) | 18209 (17551–18575) → 243 (162–715) |
| 3840x2160 | H.265 | 60 | 21364 (21274–21550) → 10 (7–12) | 14939 (14496–15043) → 45 (44–46) | 21344 (21241–21982) → 179 (107–715) |
| 3840x2160 | H.265 | 120 | 20853 (20849–21058) → 6 (5–6) | 17404 (17304–17457) → 46 (45–46) | 20815 (20673–21703) → 181 (125–678) |

At a median, the call took 5.0–5.2 ms at 1920x1200, 7.2–7.5 ms at 2560x1440,
12.6–12.9 ms at 3328x2160 and 14.3–15.0 ms at 3840x2160 before. After, it took
3–5 µs at every size. 1920x1080 is not a multiple of 16, so VideoToolbox
encodes it asynchronously and its call took 31–36 µs even before.

Before, an input queued during a synchronous encode waited until the encode
returned, and the p99 wait was close to the call. After, the p99 wait was
40–53 µs in every run but two. Run 1 of H.265 at pace 60 waited 4.4 ms at
1920x1200 and 3.3 ms at 2560x1440. The other two runs of those cases waited
46 µs. In that run the pacing thread was not inside an encode call: its
longest call was 14 µs at 1920x1200 and 45 µs at 2560x1440. Something else
held it, on a machine whose load average was 5.3 at that run's start. These
runs do not show what.

The maximum wait is a single input of about 5,000. At 1920x1080, which
VideoToolbox already encoded asynchronously, the median maximum rose from
0.16–0.29 ms to 0.39–0.48 ms. The p99 there did not move.

## Frame rate and dwell

From the same runs, the median of three. Every case but four submitted and
put out its pace (60 or 120 a second, within 0.2) before and after. The dwell
p50 stayed within 0.05 ms, and the dwell p95 within 0.2 ms, of before.

The other four are 3328x2160 and 3840x2160 paced at 120, where the encoder
is slower than the pace. The range is in brackets:

| Size | Codec | Units per s | Dwell p50 (ms) | Dwell p95 (ms) | Armed per s (after) |
| --- | --- | ---: | ---: | ---: | ---: |
| 3328x2160 | H.264 | 75.2 (75.0–75.8) → 74.8 (74.6–74.8) | 12.83 → 12.88 | 14.19 → 15.51 | 74.8 (74.6–74.8) |
| 3328x2160 | H.265 | 78.0 (77.8–78.2) → 77.8 (77.6–77.8) | 12.52 → 12.51 | 14.70 → 13.38 | 77.8 (77.4–78.0) |
| 3840x2160 | H.264 | 66.0 (65.4–66.2) → 65.4 (65.2–65.4) | 14.99 → 14.91 | 16.17 → 16.20 | 65.2 (65.0–65.4) |
| 3840x2160 | H.265 | 68.4 (67.8–68.6) → 68.2 (67.8–68.2) | 14.27 → 14.26 | 19.00 → 19.12 | 68.0 (67.8–68.2) |

In those four cases the session kept the encoder's own rate, within 1% of
before. Nearly every due tick found the submit thread still encoding and
stayed armed. The thread's wake then let the pacing thread take the next
picture, as the blocking call's return did before. No tick was spent on the
in-flight gate in any `video_toolbox_submit_blocking` case, before or after.

## Two units in flight against four

`video_toolbox_in_flight`, one run per arm. Before is thread C's call with the
`pending()` gate. After is the submit thread with `Encoder::admit`.

| Size | Codec | Pace | Gate | Units per s | Skipped per s | Dwell p50 (ms) | Dwell p95 (ms) | Submit call p99 (µs) |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3024x1964 | H.264 | 120 | 4 | 88.8 → 89.4 | 31.2 → 30.6 | 40.83 → 40.64 | 44.94 → 45.11 | 1623 → 8 |
| 3024x1964 | H.264 | 120 | 2 | 89.4 → 89.6 | 30.6 → 30.4 | 17.97 → 18.11 | 21.92 → 22.01 | 87 → 12 |
| 3024x1964 | H.265 | 120 | 4 | 92.4 → 91.8 | 27.6 → 28.0 | 38.88 → 39.13 | 45.31 → 45.80 | 1549 → 11 |
| 3024x1964 | H.265 | 120 | 2 | 92.6 → 92.2 | 27.2 → 27.6 | 17.44 → 17.33 | 21.36 → 21.46 | 95 → 8 |
| 3340x2160 | H.264 | 120 | 4 | 74.4 → 74.6 | 45.4 → 45.2 | 49.39 → 49.40 | 53.56 → 53.62 | 1954 → 8 |
| 3340x2160 | H.264 | 120 | 2 | 75.0 → 75.2 | 44.8 → 45.0 | 22.60 → 22.61 | 26.38 → 26.51 | 78 → 15 |
| 3340x2160 | H.265 | 120 | 4 | 80.2 → 77.8 | 40.0 → 42.2 | 45.54 → 46.79 | 53.59 → 54.53 | 1745 → 7 |
| 3340x2160 | H.265 | 120 | 2 | 81.0 → 78.6 | 38.8 → 41.6 | 20.39 → 21.22 | 24.74 → 26.37 | 78 → 7 |

At pace 120, where VideoToolbox could not keep up, a gate of two halved the
dwell at the same frame rate as four. On this branch the dwell p50 fell by
22.5 ms (3024x1964 H.264), 21.8 ms (3024x1964 H.265), 26.8 ms (3340x2160
H.264) and 25.6 ms (3340x2160 H.265). The frame rate at two was 0.2–1.0%
higher than at four. With four units in flight, VideoToolbox also made the
caller wait in its call: the p99 was 1.5–2.0 ms before.

At pace 60 at both sizes, and at 1920x1080 at either pace, every case
delivered its pace, with no tick skipped, at either gate. The dwell p50 was
11.4–15.4 ms at the large sizes and 4.5–5.9 ms at 1920x1080, at both gates
and in both arms (see the raw files).

## Rate model

`video_toolbox_rate_model_h264` and `_h265` pace their own loop and paint each
picture on it. After, they follow a Mac host's gate (two units in flight,
before: three in the encoder or undrained, the size of their ring).

- **Fitted model.** The model was always `timestamp`. Its `within_25_percent`
  verdict matched case for case, with one exception: H.265 at 1920x1080 with
  low-latency rate control. In the first pair of runs it fit within 25%
  before and not after. In the second pair it fit after and not before. Its
  pace-5 step and the largest unit after the pause vary that much between
  runs of the same build.
- **Submits.** Every step submitted the same pictures before and after, with
  two exceptions:
  - 4K with low-latency rate control at pace 60, where the loop's own painting
    falls behind. H.264 submitted 197 before and 200 after; H.265 submitted
    197 and 196 before, and 192 and 194 after.
  - H.265 at 4K with standard rate control at pace 30. There VideoToolbox held
    each unit about 40 ms, longer than the 33 ms period, and the gate of two
    spent a few more ticks: it submitted 115 and 116 after, against 120 and
    119 before.
- **Units out.** They followed the submits within one.
- **Dwell.** The dwell p50 per step moved by under 1 ms in all but four steps:
  - H.264 1080p with low-latency rate control at pace 15: 7.13 → 5.39 ms;
  - H.264 4K with standard rate control at pace 60: 13.73 → 16.23 ms;
  - H.265 1080p with low-latency rate control at pace 5, second pair:
    7.20 → 8.21 ms;
  - H.265 4K with standard rate control at pace 30, second pair:
    43.62 → 38.16 ms, with fewer units in flight.

## Not measured here

These need a Mac with Screen Recording granted and a viewer:

- in a live session on a 4K or 1440p display at Native, with moving content,
  typing and dragging: the host trace's `input_wait_p99_us` before and after,
  and `macos_encoder_load` (`call_p50_us`, `armed_refusals`) after;
- what `macos_capture_layout` reports for ScreenCaptureKit's pictures: whether
  their planes or surfaces are padded to a multiple of 16, which would put
  more sizes on VideoToolbox's synchronous path;
- the frame rate and dwell of a real ScreenCaptureKit stream with the gate at
  two, on a MacBook's built-in display at its default resolution and 120 FPS.
