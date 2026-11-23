# Native pointer shapes: measurements

A release build of `ui/tool/pointer_latency.dart` in the macOS runner, on a
12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425), Flutter 3.50.1 (CI
builds with 3.44.9), its display at 144 Hz and a device pixel ratio of 2. Other
builds ran on the machine meanwhile (load average 4.6 to 6.7). The tool runs
in the real runner, so the `pb_mapper/desktop_pointer` calls reach
`ui/darwin/DesktopPointer.swift` and the system cursor calls reach the engine;
a synthetic mouse inside the window stands for the pointer, so the real one
never moved. Each change waits 20 to 60 ms after the last, landing anywhere in
a refresh, as a host's report does. 200 changes per path per run, three runs.
[Per-run samples](run1.json), [run 2](run2.json), [run 3](run3.json),
[output](output.txt).

```text
cd ui
FLUTTER_XCODE_ARCHS=arm64 FLUTTER_XCODE_ONLY_ACTIVE_ARCH=YES \
  bash ../scripts/build/flutter-macos.sh build macos --release -t tool/pointer_latency.dart
build/macos/Build/Products/Release/pb-mapper.app/Contents/MacOS/pb-mapper run1.json
```

## Shape change to this computer's pointer, in ms

| Path | Run | p50 | p90 | p99 | max |
| --- | ---: | ---: | ---: | ---: | ---: |
| Rebuild, then the mouse tracker's post-frame pass (before) | 1 | 3.530 | 6.508 | 7.219 | 7.252 |
| | 2 | 3.478 | 6.401 | 7.274 | 7.324 |
| | 3 | 3.602 | 6.611 | 7.407 | 7.633 |
| `activateSystemCursor` from the session (after, a kind this OS has) | 1 | 0.208 | 0.320 | 0.399 | 0.457 |
| | 2 | 0.216 | 0.273 | 0.317 | 0.322 |
| | 3 | 0.083 | 0.101 | 0.114 | 0.116 |
| `cursor_set` of a held bitmap (after, any other kind) | 1 | 0.542 | 0.854 | 1.326 | 1.570 |
| | 2 | 0.545 | 0.735 | 1.037 | 1.745 |
| | 3 | 0.492 | 0.671 | 0.842 | 1.048 |
| Scale a 32-px shape to 64 px, `cursor_create`, `cursor_set` (after, a new bitmap) | 1 | 0.667 | 1.366 | 89.203 | 114.130 |
| | 2 | 0.658 | 0.926 | 3.015 | 14.422 |
| | 3 | 1.169 | 1.542 | 2.177 | 2.423 |

- The rebuild path is timed from the change to the activation of the new
  cursor's session, before its platform message leaves, so it leaves out the
  `activateSystemCursor` hop the other rows include. At 144 Hz it waits up to
  one refresh (6.9 ms) for the frame, as its maximum shows; at a lower
  refresh rate the wait grows with the period. That was not measured here.
- The paths after the change wait for no frame. Each is timed to the reply,
  which comes after the engine or the runner has set the cursor.
- Run 1's twelve bitmap creations between samples 132 and 160 took 12 to
  114 ms, while other builds loaded the machine; runs 2 and 3 show no such
  cluster.

## Not measured here

- A sweep of the real pointer over `desktop_ui_host`'s bands, with its
  `host_shape` lines against the runner's: this machine has no `ffmpeg` for
  the clip, and nothing here moves the real pointer.
- How far a painted pointer trails the hand, before, and that a native one
  does not, after: a 240 fps camera on a device, over a Mac host's open hand.
- Whether a Mac viewer over the busy band shows the host's hourglass (the
  widget tests check that it takes the bitmap path).
- The Windows and Linux runners, which build only in CI; on Windows, that
  `GetGuiResources(GR_USEROBJECTS)` stays flat across 200 shape changes.
