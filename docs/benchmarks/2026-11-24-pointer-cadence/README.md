# Pointer cadence: measurements

Release builds on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425),
Rust 1.99.1 and Flutter 3.50.1 (CI builds with 3.44.9), the display at 144 Hz
and a device pixel ratio of 2. Other agents' builds ran on the machine
meanwhile; each run's one-minute load average is in the raw output. Before and
after took turns, in the order before, after, after, before, before, after.

## Host: `host_send_bench`

`RUST_LOG=off cargo run --release -p pb-mapper-desktop --example host_send_bench`.
Before is `dd1291d` (the base's host, with the bench's two new cases); after
is `ab1d64c` (the 2 ms shape look and the pointer clock at the pace). Everything
runs in one process on loopback with a synthetic encoder: the shape and the
host pointer are the bench backend's own, and the times run from the move (or
the host's pointer moving) to the viewer's `view()` showing it, polled every
100 µs. [Raw output](host_send_bench.txt)

### Shape and host pointer, in ms

128 samples a case, each 20 to 60 ms after the last, so they land anywhere in
the host's pointer clock.

| Case | Build | Run | p50 | p90 | p99 | max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `shape`: the controller's move into the other half, until its view names the new shape (60 FPS) | before | 1 | 7.71 | 14.48 | 16.47 | 16.49 |
| | | 2 | 7.78 | 14.65 | 16.19 | 16.25 |
| | | 3 | 8.31 | 14.27 | 16.30 | 16.32 |
| | after | 1 | 2.46 | 2.63 | 2.89 | 2.95 |
| | | 2 | 2.46 | 2.63 | 2.67 | 2.76 |
| | | 3 | 2.32 | 2.48 | 2.54 | 2.55 |
| `host_pointer`: the host's pointer moved, until a watching viewer sees it, 60 FPS | before | 1 | 7.98 | 14.72 | 16.14 | 16.20 |
| | | 2 | 7.45 | 14.95 | 16.19 | 16.21 |
| | | 3 | 7.63 | 14.54 | 16.06 | 16.25 |
| | after | 1 | 7.84 | 14.93 | 16.18 | 16.29 |
| | | 2 | 7.29 | 14.02 | 15.96 | 16.04 |
| | | 3 | 7.24 | 13.76 | 16.15 | 16.29 |
| `host_pointer`, 120 FPS | before | 1 | 7.76 | 13.72 | 15.38 | 15.92 |
| | | 2 | 7.75 | 13.55 | 15.49 | 15.72 |
| | | 3 | 7.21 | 14.15 | 17.08 | 17.71 |
| | after | 1 | 4.24 | 7.69 | 8.51 | 8.61 |
| | | 2 | 4.23 | 7.82 | 8.51 | 9.43 |
| | | 3 | 3.76 | 7.49 | 8.29 | 8.46 |

Runs are numbered in each build's order. The 120 FPS rooms produced 119.7 to
119.9 units a second, the 60 FPS ones 59.9 to 60.9.

- A shape the move brings waited for the next 16 ms pointer reading, 7.7 to
  8.3 ms at the median and up to 16.5 ms; it now leaves 2 ms after the move,
  2.3 to 2.5 ms at the median and at most 2.95 ms, transport and polling
  included.
- At 60 FPS the host's pointer clock stays at 16 ms, and so do the times. At
  120 FPS it runs once a period (8.3 ms): the median halves, from 7.2 to
  7.8 ms to 3.8 to 4.2 ms, and the slowest sample falls from 15.7 to 17.7 ms
  to 8.5 to 9.4 ms.
- `peer_cursor` (in the raw output) is a controller's move as another viewer
  sees it, which thread C publishes when it injects the move: neither change
  touches it, and its medians stayed at 156 to 158 µs but for one after run at
  272 µs.

### Zero cost on the data path

Allocations per unit, medians of three runs. The bench's backend now reads
shapes, so in the moving-pointer cases the after build schedules a shape look
after each move; it shows no pointer there, as before, so each look reads the
pointer only.

| Case | Build | Inputs/s | Allocations/unit | Allocated bytes/unit |
| --- | --- | ---: | ---: | ---: |
| One viewer, video | before | 0 | 18.84 | 40,185 |
| | after | 0 | 18.85 | 40,184 |
| One viewer, moving pointer | before | 249.9 | 58.63 | 47,067 |
| | after | 249.9 | 58.77 | 47,060 |
| Two viewers, both moving pointers | before | 499.8 | 167.22 | 100,442 |
| | after | 499.9 | 167.27 | 100,443 |

Every run produced 59.8 to 60.0 units a second. The moving-pointer runs ranged
from 57.59 (before run 1, at 242.8 inputs a second) to 58.81 allocations a
unit; at the same input rate the builds match.

## Viewer: `ui/tool/hover_frames.dart`

The app's own `DesktopStore` over the tests' `FakeDesktopApi`, the real
`RemoteInput` with its move gate, `RemotePointer`, `RemotePointerRegion` and
`RemoteCursorLayer` over a still picture, in the macOS runner, its window in
front. The pointer moves every 8 ms along a line across the picture through
`RemoteInput.hover`, what `RemoteSurface` calls for each hover event, as a
mouse whose events the system coalesces to about 125 a second would; the
host's shape is the text beam, which the local pointer shows, so the cursor
layer paints nothing. 5 s windows: at rest, sweeping, at rest, sweeping.
Before is the base's `desktop_store.dart` and `remote_cursor_layer.dart` with
the tool (`9346060`); after is `118fc57`.

```text
cd ui
FLUTTER_XCODE_ARCHS=arm64 FLUTTER_XCODE_ONLY_ACTIVE_ARCH=YES \
  bash ../scripts/build/flutter-macos.sh build macos --release -t tool/hover_frames.dart
open -W -n -a build/macos/Build/Products/Release/pb-mapper.app --args "$PWD/hover_frames.json"
```

[Run 1](hover_1-before.json), [2](hover_2-after.json),
[3](hover_3-after.json), [4](hover_4-before.json), [5](hover_5-before.json),
[6](hover_6-after.json), [load averages](hover_runs.txt).

| Window | Build | Frames drawn/s | FrameTiming reports/s | View leaf reads/s | Moves sent/s |
| --- | --- | ---: | ---: | ---: | ---: |
| Sweep | before | 123.3 to 124.4 | 123.3 to 124.4 | 185.9 to 187.0 | 124.7 to 124.8 |
| | after | 0 | 0 | 187.1 to 187.3 | 124.7 to 124.8 |
| Rest | before | 0 | 0 | 62.4 to 62.6 | 0 |
| | after | 0 | 0 | 62.4 to 62.6 | 0 |

- Before, every hover drew a frame: about 124 a second, one per hover, under
  the display's 144. After, none: the layer had nothing new to paint.
- The view leaf is read as often as before while the pointer moves: before,
  in each hover's frame; after, as each 4 ms move gate closes, once per move
  sent at this rate. Both add the 16 ms timer's 62.5 reads a second. A shape
  the host sends for the move is therefore picked up no later than before.
- None of the six runs saw the app's lifecycle state change.

## Not measured here

- The GPU power the missing frames save (`sudo powermetrics --samplers
  gpu_power`): this session has no `sudo`.
- A real mouse over a real viewer against `desktop_ui_host`, whose `host_shape`
  lines would time a band crossing from `host_input`: this machine has no
  `ffmpeg` for the clip, and nothing here moves the real pointer. The bench's
  `shape` case times the same host path in process.
- A Windows host, in process and through the helper, hovering links: the
  time from the move's send to the viewer's `cursor_shape` change. Through the
  helper, thread C sees a new shape only once a capture call has observed it:
  every 2 ms after an input while the picture is still, at the frame rate while
  it changes.
- Two viewers on devices, a 120 Hz one watching the other's pointer: peer
  updates a second on its screen.
- The Windows and Linux viewers, which build only in CI.
