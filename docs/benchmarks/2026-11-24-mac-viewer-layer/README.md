# Mac viewer layer presenter: measurements

The macOS viewer's layer presenter (VideoToolbox decoding to 420v, each
picture committed from the decoder thread to the viewer's own Core
Animation layer) against Flutter's texture (VideoToolbox decoding to BGRA,
the newest picture taken by Flutter's compositor), in the real app and in
offscreen harnesses. Release builds on a 12-core Apple M6 with 24 GiB RAM,
macOS 27.0 (26A425), Rust 1.99.1 and Flutter 3.50.1. One display, a G24H2
at 144 Hz. Other agents were building in parallel worktrees throughout:
the load averages read before each run were 2.6 to 12.3
([runs-load.txt](runs-load.txt)).

The layer presenter is opt-in (`--dart-define=presenter=layer`); the app
keeps the texture until a run with the display on has done what the last
section lists. The scripts here pass the presenter explicitly.

**The console was locked, with the display asleep, during every run**
([displaystate.txt](displaystate.txt): `CGDisplayIsAsleep` true, and
loginwindow's shield over the screen). No picture reached glass, so this
record has no decode-to-glass latency, no on-screen colour, no
WindowServer GPU time and none of the functional checks; see the end.

## In the app

- Clips: `clipgen.swift` from
  [the decoder stats record](../2026-11-24-viewer-decoder-stats/clipgen.swift)
  (`swiftc -O`): 240 frames of a desktop-like picture, H.264 Main, one IDR,
  an access unit delimiter before every unit, at 1920x1080, 2560x1440 and
  3840x2160 (6,245, 11,814 and 20,850 bytes a unit on average).
- Host: `cargo build --release -p pb-mapper --features desktop --example desktop_ui_host`,
  then `desktop_ui_host --clip CLIP --size WxH --dir DIR --relay-only --view-only --fps 60|120 --seconds 40`,
  served by `node scripts/test/desktop-bootstrap.cjs DIR` ([run.sh](run.sh)).
- Viewer: the acceptance entry built four times
  ([build-app.sh](build-app.sh)) as in
  [the macOS viewer procedure](../../../scripts/test/desktop-ui-acceptance.md#macos-viewer),
  `--release --dart-define=seconds=40 --dart-define=resolution=native`,
  with `presenter=texture` or `presenter=layer`, and `maxFps=60` with the
  Balanced preset or `maxFps=120` with Smooth. Relay over TCP on loopback.
  Each run is a fresh app process; the tool reads `stats` once a second
  with timings on.
- Each case ran twice with each presenter, the order reversed in the second
  round ([matrix.sh](matrix.sh)). While the viewer ran, `run.sh` read its
  CPU time, WindowServer's and the GPU's utilisation about every 0.6 s.
- With the screen locked, about 31 s into every texture run Flutter's
  compositor fell to taking 4 to 8 pictures a second, the rest counted
  dropped, while every layer run went on committing every picture to its
  end ([late.py](late.py) prints each run's rates after 31 s and its
  phases). [analyze.py](analyze.py) therefore reads both presenters over
  the same window, from the end of the 5 s warm-up to 29 s (23 intervals):
  each timing's mean between two samples (the change in
  `samples * mean_ms` over the change in `samples`), the native counts a
  second, `decoderCpuMs` gained a second, and the CPU times from 8 s to
  28 s after the viewer started. [Per-run summary](summary.jsonl);
  [each run's samples and CPU readings](samples.json), which
  [extract.py](extract.py) kept from the run directories the scripts
  write and read (`work/runs`, not kept here); [table.py](table.py) prints
  the table from the summary.

Medians over the window, run a then run b. Decode is
`VTDecompressionSessionDecodeFrame`; present is the layer's span from the
end of a picture's decode to the end of its commit; release to consume is
the texture's span from a picture's publication to Flutter's compositor
taking it. The two end at different points: neither includes the time to
glass, and they are not one span shortened. CPU is a share of one core.

| Case | Presenter | Decode ms | Present ms | Release to consume ms | Decoder thread CPU ms/s | Viewer CPU % | WindowServer CPU % | Presented / decoded a second |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1080p 60 | texture | 2.55, 2.61 | – | 3.77, 3.86 | 4.9, 5.0 | 7.8, 8.2 | 2.3, 1.8 | 59.9 / 59.9, 59.9 / 59.8 |
| 1080p 60 | layer | 0.78, 0.88 | 0.24, 0.35 | – | 17.1, 24.5 | 4.4, 4.9 | 5.0, 3.6 | 59.9 / 59.9, 59.9 / 59.9 |
| 1440p 60 | texture | 2.60, 2.52 | – | 4.01, 3.71 | 3.1, 4.0 | 5.6, 5.5 | 1.5, 1.7 | 59.8 / 59.8, 59.9 / 59.9 |
| 1440p 60 | layer | 1.29, 1.04 | 0.52, 0.21 | – | 35.8, 16.1 | 6.5, 4.0 | 4.5, 3.0 | 59.9 / 59.9, 59.9 / 59.9 |
| 4K 60 | texture | 5.16, 4.84 | – | 3.87, 3.81 | 5.7, 5.3 | 8.5, 7.5 | 2.6, 1.7 | 59.9 / 59.9, 59.9 / 59.9 |
| 4K 60 | layer | 1.94, 1.91 | 0.42, 0.33 | – | 30.6, 23.8 | 5.4, 5.1 | 4.3, 3.2 | 59.9 / 59.9, 59.9 / 59.9 |
| 1080p 120 | texture | 2.35, 2.13 | – | 3.72, 3.69 | 7.4, 7.0 | 12.3, 10.5 | 2.9, 3.1 | 119.7 / 119.8, 119.7 / 119.8 |
| 1080p 120 | layer | 0.86, 0.84 | 0.39, 0.35 | – | 53.6, 47.9 | 8.5, 9.2 | 6.0, 5.9 | 119.8 / 119.8, 119.8 / 119.8 |
| 4K 120 | texture | 4.77, 5.05 | – | 3.52, 3.58 | 8.7, 9.5 | 11.0, 12.7 | 2.7, 2.9 | 117.8 / 119.7, 118.7 / 119.8 |
| 4K 120 | layer | 1.78, 1.70 | 0.28, 0.20 | – | 40.3, 29.5 | 7.8, 6.6 | 5.4, 5.0 | 119.8 / 119.8, 119.7 / 119.7 |

- Decoding into 420v took 0.8 to 0.9 ms a picture at 1080p instead of 2.1
  to 2.6 ms, 1.0 to 1.3 ms at 1440p instead of 2.5 to 2.6 ms, and 1.7 to
  1.9 ms at 4K instead of 4.8 to 5.2 ms.
- The commit took a median 0.2 to 0.5 ms a picture. It runs on the decoder
  thread, whose CPU rose by 12 to 46 ms a second.
- The viewer process used less CPU with the layer in 9 of the 10 pairs (4.0
  to 9.2% of a core against 5.5 to 12.7%); the exception, 1440p run a, used
  6.5% against 5.6%. WindowServer used more: 3.0 to 6.0% against 1.5 to
  3.1%. Added together, the layer's two were lower in 8 of the 10 pairs, by
  up to 4.0 points, and higher in two, by 1.5 and 3.9. With the display
  asleep WindowServer composited nothing to a screen, so its share here
  says nothing yet about a lit one.
- The texture runs counted up to 0.4 dropped pictures a second, and 1.3
  and 2.4 at 4K and 120 frames a second, where Flutter's compositor took
  1.1 and 1.9 fewer pictures a second than were decoded. The layer
  committed every picture. Its dropped count, the pictures committed within
  one refresh after another, needs its display link, which did not fire
  with the display asleep, so its zero here shows nothing.
- The GPU utilisation readings were 0 almost throughout, as expected with
  nothing composited to a screen; they are in `samples.json` but measure
  nothing here.
- Four runs, two of each presenter, passed through `recovering` after 31 s,
  outside the window, as stalls on this shared machine did in the decoder
  stats record.
- The matrix's four builds came from this change before its last two
  edits, a log line when the platform view is made and a CVDisplayLink
  stopped with its owner (before macOS 14 only). A last layer run, 1080p at
  60, built with the runner as committed (`final-layer` in
  [summary.jsonl](summary.jsonl)) logged its platform view hosting the
  layer (`PbDesktopVideo: Video view 0 for 1: its layer`) and read decode
  0.91 ms, present 0.37 ms and 59.8 pictures presented a second.

## Offscreen

[colourcheck.swift](colourcheck.swift) (`swiftc -O -target arm64-apple-macosx14.0`),
[output](colourcheck.txt). It encodes eight sRGB patches as a pb-mapper
host does (NV12 through the BT.709 matrix with no transfer conversion, VUI
tagged BT.709; the decoded luma matched the source exactly) and renders
the decoded pictures with `CARenderer` into an sRGB target:

- The layer path's 420v, tagged sRGB transfer, BT.709 primaries and matrix
  through the format description, came within 2 levels of VideoToolbox's
  BGRA values (the texture path's colours) on all eight patches. Left with
  the stream's BT.709 transfer it came out up to 12 levels lighter (mid-grey
  128 as 139).
- VideoToolbox carried the format description's tags onto the picture and
  its IOSurface (`IOSurfaceTransferFunction` IEC_sRGB with a matching
  colour space). Setting the transfer attachment on a picture after the
  decode changed `IOSurfaceTransferFunction` but left the BT.709
  `IOSurfaceColorSpace`, and Core Animation, which reads the colour space
  first, still showed it 12 levels lighter; with the colour space
  attachment removed, or replaced, it came within 2 levels. The runner's
  fallback removes it.
- An IOSurface as layer contents is drawn the same way up as a CGImage.
- The grey still: a `CIColorMatrix` with GreyStill's coefficients on the
  content layer came out up to 57 levels brighter than GreyStill's matrix
  applied to the BGRA values (mid-grey 128 as 92 instead of 64), as a
  matrix applied in linear light would. A layer with
  `compositingFilter = "colorBlendMode"` (or `saturationBlendMode`) over a
  grey fill, then black at half opacity, came within 9 levels (the blend
  modes weigh luminance 0.3, 0.59, 0.11 rather than BT.709's), mid-grey
  exactly.
- The snapshot's conversion (`VTCreateCGImageFromCVPixelBuffer` drawn into
  a device RGB context): the layer's 420v came within 2 levels of the sRGB
  source, the texture's BGRA, tagged BT.709, up to 11 levels lighter.

[viewcheck.swift](viewcheck.swift), [output](viewcheck.txt): the view
structure DesktopVideo.swift builds, in a window. A container added to a
layer-backed view's own layer before the view had a frame took the view's
size, and followed a resize with its content layer; 120 commits of
IOSurfaces from a thread without a run loop, each in its own explicit
transaction, left the contents set with no Core Animation warning; a second
view took the container, and removing the first view left it in the
second. The view's display link did not fire in a second: the display was
asleep.

## Points of interest

While the stats ask for timings, the runner marks each picture's `Decoded`
(as it leaves the decoder), the layer's `Commit` (an interval around the
commit) and the texture's `Consumed` (Flutter's compositor taking it) as
points of interest, with the picture's generation and frame.
[trace-run.sh](trace-run.sh) runs [run.sh](run.sh) and records the viewer
for 10 s, from 12 s after it started, with Animation Hitches and the Points
of Interest instrument added; [signposts.py](signposts.py) reads the marks
from the export. One run per presenter at 1080p and 60 frames a second,
built from this change after review (later edits only reworded comments),
with the display still asleep ([output](trace-runs.txt)):

- Both traces held the marks, each with its picture: 634 `Decoded` and 634
  `Commit` intervals with the layer, 634 `Decoded` and 631 `Consumed` with
  the texture.
- From `Decoded` to the end of `Commit`, the layer's median was 0.24 ms
  (p95 1.04 ms); from `Decoded` to `Consumed`, the texture's was 3.81 ms
  (p95 6.90 ms). These end at the hand-off to the WindowServer and to
  Flutter's compositor respectively, not on the glass, which the asleep
  display never showed.

## The cursor over the picture

Over the picture the layer's view, not FlutterView, receives AppKit's
cursor updates, and FlutterView sets its last cursor again only where it is
the view under the pointer. The layer's view sets that cursor itself, read
from FlutterView's `_lastCursor`, which the engine's cursor plugin and the
pointer plugin both tell through `didUpdateMouseCursor:`.
[cursorcheck.swift](cursorcheck.swift) ([output](cursorcheck.txt)) loaded
Flutter 3.50.1's debug and release FlutterMacOS: FlutterView answers the
hook, `_lastCursor` is an `NSCursor`, and reading it back after the hook
gives the cursor the hook was told. Flutter 3.44.9's engine, which CI
builds with, was not checked; without the ivar the view leaves the cursor
as it is.

## Left for a run with the display on, or a device

- Decode-to-glass latency for both presenters at 60 Hz and 144 Hz: the
  marks above against the display's frames in the same trace
  ([trace-run.sh](trace-run.sh)), and WindowServer's GPU time for each
  (`Metal System Trace`). The layer becomes the default only with a median
  lower by at least half a refresh at both rates, a p95 no worse, and
  WindowServer's GPU time no higher than the texture path's.
- WindowServer CPU with the display lit.
- On screen: the layer's colours against the texture's, the grey still,
  the local pointer over the picture (hidden or the host's shape, also
  after switching to another app and back and after closing a menu over
  the picture), the first click into an inactive window, pinch, pan and
  zoom, the compact frame, the overlays above the picture, the picture kept
  across a reconnect and moving during a live resize.
- A ProMotion MacBook: 120 Hz cadence with and without the display link's
  frame-rate request, and the dropped count it keeps.
- CI's Flutter 3.44.9 macOS build with the platform view.
