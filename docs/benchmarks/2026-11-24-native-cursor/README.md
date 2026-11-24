# Native pointer shapes: measurements

A release build of `ui/tool/pointer_latency.dart` in the macOS runner, on a
12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425), Flutter 3.50.1 (CI
builds with 3.44.9), its display at 144 Hz and a device pixel ratio of 2.
Other agents' builds ran on the machine meanwhile: the one-minute load
average was 3.47 and 2.47 before and after run 1, 2.47 and 9.66 around run
2, and 9.66 and 5.75 around run 3. The tool runs in the real runner, so the
`pb_mapper/desktop_pointer` calls reach `ui/darwin/DesktopPointer.swift` and
the system cursor calls reach the engine; a synthetic mouse inside the window
stands for the pointer, so the real one never moved. Each change or event
waits 20 to 60 ms after the last, landing anywhere in a refresh, as a host's
report or a mouse move does. 200 per path per run, three runs. No run saw
the app's lifecycle state change, which macOS would report for a hidden
window. [Run 1](run1.json), [run 2](run2.json), [run 3](run3.json),
[output](output.txt).

```text
cd ui
FLUTTER_XCODE_ARCHS=arm64 FLUTTER_XCODE_ONLY_ACTIVE_ARCH=YES \
  bash ../scripts/build/flutter-macos.sh build macos --release -t tool/pointer_latency.dart
build/macos/Build/Products/Release/pb-mapper.app/Contents/MacOS/pb-mapper run1.json
```

## A painted pointer after a mouse move (before), in ms

What a controlling viewer did over a shape the system had no pointer for:
hide the pointer and paint the host's bitmap at the hover point. Here a hover
event moves a 32-pixel bitmap painted in its own layer, timed from the event
to the end of the raster of the frame that drew it (FrameTiming's
`rasterFinish`), on one clock: `Timeline.now` reads the clock the engine
stamps frame timings with.

| Part | Run | p50 | p90 | p99 | max |
| --- | ---: | ---: | ---: | ---: | ---: |
| Event to `rasterFinish` | 1 | 4.300 | 7.250 | 7.805 | 7.826 |
| | 2 | 4.071 | 7.038 | 7.788 | 7.938 |
| | 3 | 4.165 | 6.901 | 7.673 | 8.165 |
| Event to the frame's `buildStart` | 1 | 3.686 | 6.620 | 7.094 | 7.216 |
| | 2 | 3.698 | 6.595 | 7.221 | 7.257 |
| | 3 | 3.676 | 6.471 | 7.230 | 7.426 |
| `buildStart` to `rasterFinish` | 1 | 0.595 | 0.804 | 1.061 | 1.188 |
| | 2 | 0.455 | 0.728 | 1.034 | 1.081 |
| | 3 | 0.295 | 0.723 | 0.903 | 0.908 |

- Most of it is the wait for the next frame, up to about one refresh
  (6.9 ms at 144 Hz). The picture then reaches the screen at a later refresh, which
  nothing here times, and at a lower refresh rate both waits grow with the
  period; neither was measured.
- The events are synthetic, handed to Flutter's gesture binding from a
  timer. A real mouse's event first crosses macOS and the engine, which this
  leaves out, and which a native cursor does not wait for.
- After the change no Flutter frame is in the pointer's path: the system
  draws the host's bitmap as the cursor. How much sooner it shows needs a
  camera, and was not measured.

## A shape change to this computer's pointer, in ms

| Path | Run | p50 | p90 | p99 | max |
| --- | ---: | ---: | ---: | ---: | ---: |
| Before: rebuild, then the mouse tracker's post-frame pass, the change from a timer | 1 | 3.526 | 6.436 | 7.217 | 7.257 |
| | 2 | 3.503 | 6.574 | 7.231 | 7.406 |
| | 3 | 3.389 | 6.272 | 7.565 | 7.570 |
| Before: the same, the change made after a frame is drawn | 1 | 7.008 | 7.119 | 7.223 | 7.440 |
| | 2 | 7.039 | 7.178 | 14.174 | 34.844 |
| | 3 | 7.034 | 7.138 | 14.144 | 14.164 |
| After, a kind this OS has: `activateSystemCursor` | 1 | 0.233 | 0.315 | 0.595 | 2.074 |
| | 2 | 0.204 | 0.279 | 0.747 | 0.839 |
| | 3 | 0.225 | 0.370 | 41.603 | 53.199 |
| After, a bitmap the runner holds: `cursor_set` | 1 | 0.689 | 0.879 | 1.096 | 1.801 |
| | 2 | 0.583 | 0.795 | 4.060 | 46.994 |
| | 3 | 0.600 | 1.141 | 33.335 | 54.932 |
| After, a new bitmap: scale 32 px to 64 px, `cursor_create`, `cursor_set` | 1 | 1.388 | 1.711 | 3.738 | 4.256 |
| | 2 | 1.263 | 1.557 | 3.828 | 3.837 |
| | 3 | 1.263 | 4.958 | 129.333 | 152.769 |

- The two rows before are timed from the change to the activation of the
  new cursor's session, before its platform message leaves, so they leave
  out the `activateSystemCursor` hop that the rows after include.
- The store reads the host's shape both from a 16 ms timer and in every
  frame after it is drawn (`_onFrame`), whichever sees it first. From the
  timer, the rebuild waits up to about one refresh; after a frame, it waits
  for the whole next one, and longer where frames were missed (14 ms at p99
  in runs 2 and 3, 35 ms once). The old path's wait lay between those rows.
- The rows after are the platform calls `RemoteShapeCursor`'s session
  makes, made here directly rather than through `RemotePointer`, so the
  session's own few comparisons are not in them; the new bitmap's row
  includes the scaling. Each is timed to the reply, which comes after the
  engine or the runner has set the cursor. They wait for no frame.
- The three rows after take turns, sample by sample, so load from other
  work falls on each alike. In run 3, while the load average stood between
  9.66 and 5.75, all three had replies held up, the engine's own
  `activateSystemCursor` too: 4 of its 200 took over 5 ms (53 ms at most),
  against 9 for `cursor_set` (55 ms at most) and 19 for a new bitmap
  (153 ms at most), whose work on the main thread is the largest. Runs 1
  and 2 had one such reply between them, a 47 ms `cursor_set`.

## Not measured here

- A sweep of the real pointer over `desktop_ui_host`'s bands, with its
  `host_shape` lines against the runner's: this machine has no `ffmpeg` for
  the clip, and nothing here moves the real pointer.
- How far a painted pointer trails the hand on screen, before, and that a
  native one does not, after: a high-speed camera on a device, over a Mac
  host's open hand.
- Whether a Mac viewer over the busy band shows the host's hourglass (the
  widget tests check that it takes the bitmap path), and how a host bitmap
  looks as the cursor: size, hotspot and edges.
- The Windows and Linux runners, which build only in CI; on Windows, that
  `GetGuiResources(GR_USEROBJECTS)` stays flat across 200 shape changes.
- Any refresh rate but 144 Hz.
