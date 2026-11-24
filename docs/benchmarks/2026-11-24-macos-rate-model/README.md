# Mac host VideoToolbox rate control: measurements

A Mac host encodes with VideoToolbox's low-latency rate control. This record
measures five things about it on an M6:

- what it drops, with and without a QP bound of 51;
- how soon the last of a few new pictures after an IDR reaches the viewer,
  with and without sending a dropped last picture again;
- whether it budgets per second, so that a session could start at the
  `timestamp` rate model;
- what it and standard rate control sustain at 120 FPS at a Mac's own
  sizes;
- how it answers a bitrate cut.

The first two changed the host:

- A low-latency session sets `MaxAllowedFrameQP` to 51.
- A dropped newest picture is submitted again at the next tick.

The other three left it as it was:

- Sessions still start at `nominal`.
- H.265 stays on low latency at every size.
- A bitrate cut still sets the new cap at once.

See [VideoToolbox rate control on a Mac
host](../../remote-desktop.md#videotoolbox-rate-control-on-a-mac-host).

## How it was measured

Ignored tests drive a real VideoToolbox session as thread C drives it. They
are in `crates/pb-mapper-desktop/src/platform/macos/vt_measure.rs`, and the
rate-model sweeps are in `tests.rs`. They need no Screen Recording grant.
Each is run by name:

```
cargo test --release -p pb-mapper-desktop --lib -- --ignored --nocapture --test-threads=1 <name>
```

The pacing:

- Each tick is taken at its start, and two ticks are never closer than
  three quarters of a period (`pacer::Pacer`).
- A tick that finds two units in flight is skipped.
- A tick that finds the submit thread busy stays armed until that thread
  wakes the pacing thread.
- Presentation times come from the wall clock, as a host's do.
- VideoToolbox's own luma error (`kVTCompressionPropertyKey_CalculateMeanSquaredError`)
  gives each kept unit's PSNR.

The pictures are IOSurface NV12 pictures:

- **Text**: lines of glyphs on white, scrolling up 4 rows a picture.
- **Busy**: the text with image blocks, under fresh grain of ±6 over all of
  the luma.
- **Noise**: fresh luma noise from 64 to 191, the worst a full-motion video
  gives.

Each new picture is painted into a pool of eight surfaces as it is handed
over, as ScreenCaptureKit refills its own. The tests print how long that
took the pacing thread: at a case's median, 0.05 to 3.9 ms a picture (the
highest is noise at 2560x1440).

Each test of the first two changes runs both arms in the same build:

- **before**: no QP bound (`Options::max_qp`, a test knob), or no
  resubmit in the emulated `MacVideo` policy;
- **after**: the change.

The rate-model sweeps paint each picture on the pacing thread, onto three
surfaces, as the earlier rate-model test does.

### Environment

The runs were release builds on a 12-core Apple M6 with 24 GiB RAM, macOS
27.0 (26A425), Rust 1.99.1. Other agents' builds were running on the
machine, with load averages of 1.3 to 5.5 at the runs' starts and ends (each
file's first and last line). The Mac's screen was locked, which VideoToolbox
does not need.

### Raw output

The tables below come from these runs:

- [drops](drops.txt): `video_toolbox_drops`;
- [resubmit](resubmit.txt): `video_toolbox_resubmit`;
- [rate step](rate-step.txt): `video_toolbox_rate_step`;
- [follow-pace H.264](follow-h264.txt) and [H.265](follow-h265.txt):
  `video_toolbox_rate_model_follow_h264` and `_h265`;
- [120 FPS](120.txt): `video_toolbox_120`.

The resubmit and 120 FPS runs were made on this branch's final code. The
others were made before three review fixes that leave what they measure as
it was: a unit handed over after a drop cancels that drop's resubmit, a
unit is marked a duplicate as the encoder keeps it, and the 120 FPS test
prints its refusals. They print what the final code prints.

The resubmit and rate-step runs kept a document of 600 pictures. No case
in them painted that many new pictures.

Earlier runs are kept for the record. They cycled a fixed ring of 12
pictures painted once, and their before arm was the build before each
change (commits `bcee587`, `3e7f08c` and `24aaa1d`):

- the unchanged encoder: [drops](ring-before-drops.txt),
  [resubmit](ring-before-resubmit.txt), [rate step](ring-before-rate-step.txt),
  [follow H.264](ring-before-follow-h264.txt),
  [follow H.265](ring-before-follow-h265.txt), [120 FPS](ring-before-120.txt);
- with the QP bound: [drops](ring-maxqp-drops.txt),
  [follow H.264](ring-maxqp-follow-h264.txt),
  [follow H.265](ring-maxqp-follow-h265.txt);
- with the resubmit: [resubmit](ring-after-resubmit.txt).

Those follow-pace runs forced their IDRs between steps, so an IDR's payback
fell in the next step. They agree with the runs above where both measured
the same thing.

## Drops and the QP bound

`video_toolbox_drops` runs each codec at 1920x1080 and 2560x1440:

- paced at 60 and at 120 FPS, at Balanced's starting bits per frame scaled
  by the size;
- for 3 s, starting 3 s after the session's IDR (steady state);
- then for 1.5 s after a forced IDR.

In steady state, pictures VideoToolbox dropped out of those handed over,
before → after:

| Content | Size | Codec | Pace 60 | Pace 120 |
| --- | --- | --- | --- | --- |
| Noise | 1920x1080 | H.264 | 169 → 90 of 180 | 338 → 180 of 360 |
| Noise | 1920x1080 | H.265 | 135 → 90 of 180 | 270 → 180 of 360 |
| Noise | 2560x1440 | H.264 | 162 → 90 of 180 | 330 of 352 → 177 of 353 |
| Noise | 2560x1440 | H.265 | 158 → 90 of 180 | 266 of 354 → 178 of 355 |
| Busy, Text | both | both | 0 → 0 | 0 → 0 |

With the bound, noise kept every other picture. Its units were more even:
the largest H.264 unit at 1920x1080 and 60 FPS was 0.24 Mbit instead of
2.74. Its PSNR fell from 18.5 to 16.8 dB, since the pictures now coded were
coded coarser.

On the busy pattern and on text the bound changed little: no drops either
way, and PSNR within 0.25 dB in all but two cases. The two were H.265 busy
at 1920x1080 and 60 FPS (35.27 → 35.88 dB) and at 2560x1440 and 60 FPS
(35.88 → 33.93 dB).

In the 1.5 s after an IDR, the busy pattern and text dropped about as many
either way, 15 to 37 pictures a window: for instance 22 of 90 H.264 busy
pictures at 1920x1080 and 60 FPS in both arms. Noise dropped fewer with the
bound: 45 of 90 at 60 FPS and 87 to 90 of about 180 at 120 FPS, against 65
to 67 and 130 to 134 without it.

No content here dropped a picture in steady state without the bound, except
noise. That includes grain over scrolling text and image blocks. A session
gains from the bound where its pictures are as unpredictable as noise.

## The last picture after an IDR

`video_toolbox_resubmit` runs each codec at 1920x1080 and 60 FPS, at 1, 2
and 4 Mbit/s. A trial goes:

1. The screen stands still for 2 s, repeated every 250 ms.
2. An IDR of the still picture is forced, as a control grant does.
3. k new pictures of text are handed over, one a tick, k = 1 to 12.
4. The screen stands still again.

It times the last new picture, from its submit to the first unit kept that
carries it. Trials where that took 250 ms or more, or never came within
600 ms, out of 12:

| Bitrate | H.264 before → after | H.265 before → after |
| --- | --- | --- |
| 1 Mbit/s | 10 → 5 | 10 → 5 |
| 2 Mbit/s | 1 → 1 | 1 → 1 |
| 4 Mbit/s | 0 → 0 | 0 → 0 |

At 1 Mbit/s, after the change, the picture came within 250 ms in 7 trials.
In 5 of those a resubmit carried it, 23 to 40 ms after its submit: one or
two ticks, plus the encode. In the other 5 trials it came only with a
liveness repeat (291 and 540 ms), or not within 600 ms.

At 2 Mbit/s the one long trial moved: before it was k = 3 (257 ms), after
it was k = 1 (291 ms), carried by a liveness repeat. At 4 Mbit/s every
trial in both arms came with the picture itself, within 8 ms.

An earlier run of the same test ([resubmit-earlier](resubmit-earlier.txt))
was made before a unit handed over after a drop cancelled that drop's
resubmit, and before a repeat of a picture every unit of which was dropped
counted as new. It gave the same counts. It differed at 4 Mbit/s, where one
trial a codec came by a resubmit, about 22 ms after its submit. One run of
each cannot say whether the change or the run made that difference.

A resubmit therefore helps only after the payback's first drops. The device
check still needed is a host behind a 2 Mbit/s link, taking control and
typing at once.

## Rate model

`video_toolbox_rate_model_follow_h264` and `_h265` run each codec at 1080p,
1440p and 4K, with low latency and without. Each case is at a nominal 60 FPS,
with Balanced's bits per frame scaled by the size (`bpf`).

After a step at 60, two sweeps run at 30, 15, 10 and 5 FPS:

- **nominal**: the bitrate stays at bpf × 60, which a session starting at
  `nominal` gives VideoToolbox;
- **timestamp**: the bitrate is set to bpf × pace, live.

Each step runs 2 s, then is measured over 4 s. After the sweeps, each model
forces an IDR at 10 and at 5 FPS.

What a step produced a second, over bpf × pace (the spend QoS intends), with
low latency, at paces 30 / 15 / 10 / 5:

| Codec | Size | nominal | timestamp |
| --- | --- | --- | --- |
| H.264 | 1920x1080 | 2.00 / 3.95 / 5.95 / 11.80 | 1.01 / 0.99 / 0.98 / 0.77 |
| H.264 | 2560x1440 | 1.98 / 4.02 / 5.98 / 11.94 | 0.98 / 1.01 / 0.86 / 1.05 |
| H.264 | 3840x2160 | 1.99 / 3.93 / 5.90 / 11.64 | 1.00 / 0.99 / 1.04 / 1.04 |
| H.265 | 1920x1080 | 2.00 / 3.83 / 5.73 / 11.75 | 0.81 / 0.80 / 1.19 / 2.38 |
| H.265 | 2560x1440 | 1.83 / 3.61 / 5.33 / 10.74 | 0.92 / 0.92 / 1.36 / 0.02 |
| H.265 | 3840x2160 | 1.83 / 3.34 / 4.99 / 12.20 | 0.58 / 1.13 / 0.04 / 0.02 |

Low-latency H.264 budgets per second: under `timestamp` it spent what QoS
meant at every pace and size. Low-latency H.265 did not, in three ways:

- At 1440p at pace 5, and at 4K at paces 10 and 5, it put out every picture
  but at 0.02–0.04 of the budget. Those are units that change next to
  nothing, so the picture stood still.
- At 1080p it overspent 2.4× at pace 5.
- At 4K it spent 0.58 at pace 30.

The plan's gate was this: Timestamp's error smaller than Nominal's in every
case, and within ±25% in all but tiny samples. H.265 failed it, at 1440p and
4K at pace 5 and at several larger samples.

An IDR is also costly under `timestamp` at a low pace:

- At 10 FPS, low latency dropped 16 to 18 of the 20 pictures in the 2 s
  after it, for both codecs at every size.
- At 5 FPS it dropped 9 of 10.
- Under `nominal` it dropped 0 to 3 and 0 to 1.

The IDRs were of a like size and PSNR under both models: 11 to 28 bpf, at
25.7 to 28.5 dB. What differs is the payback, which a per-second budget of
bpf × pace spreads over seconds.

Without low latency, `timestamp` stayed between 0.56 and 1.03 of the budget
for H.264, and between 0.19 and 1.08 for H.265.

So a Mac host still starts at `nominal`, and the rate-model latch still
moves a session whose encoder overspends.

## 120 FPS

`video_toolbox_120` runs each codec at 2560x1440, 2560x1664, 3024x1964 and
3840x2160, at a nominal 120 FPS. The bitrate is Smooth's starting bits per
frame scaled by the size. The arms:

- low latency with the QP bound, and without;
- standard rate control with the data-rate cap;
- standard rate control without it, at the target and, for H.265, at 0.85
  and 0.75 of it.

Each case runs 1 s. It is then measured for 2 s (early) and for the next 3 s
(late). Besides units a second, it prints refusals a second
(`refused_per_s`): ticks skipped for two units in flight, and submits that
found the submit thread busy, which `macos_encoder_load` counts as
`gate_ticks` and `armed_refusals`.

H.265 paced at 120, units a second (late window):

| Size | Content | Low latency | Standard, capped | Standard, uncapped |
| --- | --- | ---: | ---: | ---: |
| 2560x1440 | Text | 120.3 | 118.7 | 119.7 |
| 2560x1440 | Busy | 119.3 | 118.3 | 119.7 |
| 2560x1664 | Text | 113.3 | 106.0 | 119.7 |
| 2560x1664 | Busy | 106.0 | 104.0 | 119.7 |
| 3024x1964 | Text | 92.7 | 78.0 | 120.0 |
| 3024x1964 | Busy | 90.7 | 76.3 | 120.0 |
| 3840x2160 | Text | 63.3 | 71.0 | 119.3 |
| 3840x2160 | Busy | 60.0 | 71.0 | 103.3 |

Capped standard rate control held 120 a second at the large sizes for its
first 2 s, then fell to 71 to 78. Uncapped, it held the pace except on the
busy pattern at 4K.

The same cases, quality and spend (late window):

| Size | Content | Low latency: PSNR (dB), produced / target | Uncapped at the target | Uncapped at 0.75 |
| --- | --- | --- | --- | --- |
| 3024x1964 | Text | 52.2, 0.24 | 37.1, 0.91 | 28.6, 0.64 |
| 3024x1964 | Busy | 35.8, 0.99 | 27.0, 1.34 | 27.0, 1.35 |
| 3840x2160 | Text | 52.1, 0.12 | 40.9, 0.85 | 32.1, 0.70 |
| 3840x2160 | Busy | 35.5, 1.04 | 27.2, 1.11 | 27.2, 1.11 |

Uncapped, standard rate control coded scrolling text 11 to 24 dB below low
latency at these sizes, while spending 2.7 to 7.1 times its bits a second.
Its quality was still rising over the 5 s: at 3840x2160 at the target it
was 33.6 dB early and 40.9 late.

On the busy pattern it spent the same whatever mean it was set to: 1.34 to
1.35 of the target at 3024x1964, 1.11 to 1.12 at 3840x2160 and 1.49 at the
two smaller sizes.

H.264 standard rate control, uncapped, held 120 units a second early at
3024x1964 and 90 to 95.5 at 3840x2160, and fell to 67 to 81 late at both.

The plan would switch a low-latency H.265 session to uncapped standard rate
control when, over a 1 s window, the session was paced at 0.9 of the
nominal rate or more, encoded under 0.95 of the pace, and was refused at
least a tenth of the nominal rate's ticks. Its acceptance asked for four
things at 3840x2160 and 3024x1964, and one at the smaller sizes:

- at least 115 units a second: met except busy at 4K (103);
- dwell p95 of 18 ms or less: met (14.3 to 15.4 ms at the target);
- produced at most 1.15 × target on grain: not met at 3024x1964 (1.34);
- scroll PSNR within 1.5 dB of low latency: not met (11 to 15 dB below at
  the target);
- no switch at 2560x1440 and 2560x1664: not met. The rule is not in the
  code, but its three conditions can be read from the columns, over the
  2 s and 3 s windows rather than 1 s ones. Paced at 120, in three of its
  four windows, low latency at 2560x1664 encoded 105 to 113 units a
  second, under 114, with 101 to 106 refusals a second. At 2560x1440 it
  kept 119 to 120 units a second, and the rule held in no window.

So H.265 stays on low latency. A Mac host at 4K or at a MacBook Pro's
native size, with a 120 Hz viewer, still gets low latency's 60 to 93
pictures a second, at its quality.

The QP bound changed low latency's units by at most 8.5 a second and its
PSNR by at most 3.1 dB. The largest change in units was H.264 text at
3024x1964 in the early window at 120, with no picture dropped in either
arm. The largest in PSNR was in the early window paced at 60 under
`timestamp`, where the bound kept pictures that the payback of the
session's IDR dropped without it: H.265 text at 2560x1440 dropped 8 of 120
without it and none with it.

The run made on the build the switch was evaluated on is not kept, since
that build is in no commit; this run, on the final code, replaces it.

## Bitrate cut

`video_toolbox_rate_step` runs each codec at 1920x1080 on the busy pattern,
low latency, 60 FPS. The bitrate goes from 8 to 2 Mbit/s at picture 60, and
back to 8 at picture 180, to picture 300.

In every arm:

- no picture was dropped after either change;
- after the cut, the first unit encoded at the new rate was already within
  1.2 times the new budget;
- over the 120 pictures after the cut, the mean unit was 0.58 to 0.73 of
  the new budget, with the smallest at 0.03.

The pictures before the cut were as small (0.01 to 0.02 at their smallest).
The 1 s window of the data-rate cap did not hold the previous second's
bytes against the new cap.

So `set_bitrate` still sets the new cap at once.

## Still to measure on a device

- A Mac host playing video at Balanced over a shaped 4 Mbit/s link: the
  viewer's `received_fps` and the host's `macos_encoder_dropped`.
- The same host over a 2 Mbit/s link, taking control and typing at once:
  how long the echo takes.
- A 4K or MacBook Pro 14 host with a 120 Hz viewer on Smooth: capture fps,
  `encoder_dwell_ms`, and bitrate against target, with H.265 on low
  latency.
