# Mac capture cadence: measurements

A Mac host now sets ScreenCaptureKit's minimum interval between pictures from
the display's refresh and the session's pace, follows the pace live, slows to
two pictures a second while every viewer is paused, finds its display among
the displays and on-screen windows only, fills the frame from macOS 14, and
keeps its stream and encoder for a change of the frame-rate limit alone (see
[Host: capture and encoding](../../remote-desktop.md#host-capture-and-encoding)).
This records what could be measured on this Mac, and what still needs a Mac
whose screen is awake and unlocked.

## Environment

- Apple M6, 12 cores, 24 GiB; macOS 27.0 (26A425); Rust 1.99.1, release
  builds (`environment.txt`).
- Main display: an external 4096x2304 panel, its mode at 144 Hz, its display
  link's nominal period 166667 / 24000000 s (143.9997 Hz).
- Screen Recording was granted to the shell the tests ran from
  (`CGPreflightScreenCaptureAccess` answered true).
- **The display was asleep and the screen locked** throughout. ScreenCaptureKit
  listed no display, and nothing on the screen moved, so no stream could be
  measured.

## What ran here

### Finding a display, and reading its refresh

`sck_display_lookup`, an ignored test in `platform/macos/tests.rs`, asks
ScreenCaptureKit for its shareable content 20 times each way after one of
each to warm up, alternating which goes first: every window
(`getShareableContentWithCompletionHandler`, what a start or retry used
before), and the on-screen windows without the desktop's
(`getShareableContentExcludingDesktopWindows:true onScreenWindowsOnly:true`,
what it uses now). It then reads the main display's refresh 1000 times back
to back, three rounds, makes two display links, and reads a link's period
1000 times. Three runs (`sck_display_lookup.txt`):

`cargo test --release -p pb-mapper-desktop --lib -- --ignored --nocapture
--test-threads=1 sck_display_lookup`

| Run | Every window: p50 / max (ms) | On screen: p50 / max (ms) | Windows listed (every / on screen) |
| --- | --- | --- | --- |
| 1 | 33.08 / 34.08 | 13.71 / 14.58 | 331 / 16 |
| 2 | 33.10 / 33.86 | 13.64 / 13.94 | 331 / 16 |
| 3 | 33.11 / 34.47 | 13.62 / 14.00 | 331 / 16 |

With the screen locked, 16 windows counted as on screen. On a Mac in use
more do, so the on-screen lookup will list more and take longer than here;
what it saves at a start, a rebuild and each 200 ms retry there is still to
be measured (below). Neither lookup listed a display, as the display slept:
a start would have failed with `DisplayLost` and lit the display, as before.

| Read, back to back | Cost |
| --- | --- |
| Display mode's refresh (`CGDisplayCopyDisplayMode`, `CGDisplayModeGetRefreshRate`), rounds of 1000 | 23.0–24.0 µs, then 9.6–11.9 µs, then 5.2–5.9 µs a read |
| The first display link the process makes (`CVDisplayLinkCreateWithCGDisplay`) | 2.42–2.67 ms |
| Another display link | 184–195 µs |
| A link's nominal period | 11 ns a read |

A stream reads the mode's refresh at most once a second while it runs, and
makes one display link when it opens, which reads the link's period for
`macos_capture_opened` and as the refresh where the mode reports none.

### ScreenCaptureKit's gate (Step 0)

`sck_cadence` runs a stream of the main display at 1920x1080 or less for
5 s at each of 0.9/60 (15 ms, the interval before), the floor rule (10.4 ms
on 144 Hz), the cadence at 60 (3.5 ms on 144 Hz), 0.9/120 (7.5 ms) and zero,
then back, and prints complete pictures a second, WindowServer's CPU, and how
many refresh periods apart consecutive pictures' display times were. Here it
skipped:

```
test platform::macos::tests::sck_cadence ... skipped sck_cadence: the main display sleeps
```

Until it runs, nothing here says whether ScreenCaptureKit holds each picture
to the interval strictly (then 15 ms passes every third refresh of a 144 Hz
display) or otherwise, so no frame-rate gain is claimed.

## Still to measure on a Mac whose screen is awake and unlocked

1. `sck_cadence` on this 144 Hz display, with an animation that changes the
   whole screen every refresh. If 15 ms already gives 60 or more pictures a
   second, the frame-rate claim goes and only following the pace's GPU
   saving stands.
2. Order A B B A, before against after:
   `target/release/examples/deployed APP_CONFIG_PATH OUT/<arm>-<n> selftest 40
   --tcp --feedback-log --fps 60`, then `--fps 120 --quality smooth
   --fps-override`, `--quality quality` on a 60 Hz display, and `--fps 120` at
   Balanced on a 120 Hz host: presented FPS and unique FPS medians (must not
   fall), `desktop_capture_pacing`'s wait p50/p95 and presents a second,
   WindowServer's CPU (`top -l 30 -pid $(pgrep -x WindowServer)`) and
   `sudo powermetrics --samplers gpu_power -i 1000 -n 30`; `drift_ppm` from
   `macos_capture_opened` on each display.
3. The gate: scroll, then stop, at a pace of 30 on a 60 Hz display; the last
   picture must reach the viewer within one interval.
4. The pause: hide the viewer over a moving source; `macos_capture_timing`'s
   `complete_per_s`, GPU power and WindowServer's CPU before and after; change
   the content during the pause and resume within 2 s: the last content must
   arrive within one interval.
5. The start: 30 or more windows open, minimized ones among them, 10 connects
   an arm: `macos_capture_started`'s `lookup_us` and `first_picture_us`, and
   the selftest's time to its first presented picture.
6. The kept capture: two viewers, one `--fps 120` and one at 60; pause and
   resume the 60 FPS one 10 times: the largest gap between presented pictures
   on the other, and how many `macos_capture_opened` lines there are.
7. The fill: on a 3456x2234 or 3024x1964 display at Native or 1080p,
   `SCStreamFrameInfoContentRect` before (short of the frame) and after (the
   whole frame), and the first and last rows of a thumbnail.

Record the display, its refresh and the macOS version with each.
