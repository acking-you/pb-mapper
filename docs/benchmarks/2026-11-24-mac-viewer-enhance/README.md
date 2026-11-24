# Mac viewer picture enhancement: measurements

The macOS viewer's **Sharpen enlarged picture** (the layer presenter's
`enhance`, `ui/darwin/DesktopVideo.swift`): a presenter thread upscales an
enlarged picture with a clamped Catmull-Rom and sharpens its luma with CAS
into a `CAMetalLayer`, after airhand's `client/src/lib/enhance.ts`. Release
builds on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425), Swift
6.4.2 in Swift 5 mode, Flutter 3.50.1's FlutterMacOS. One display, a G24H2
(4096x2304 at 144 Hz), **asleep throughout**, with the session locked: no
picture reached glass, so nothing here is a time to the screen, a look on a
screen or WindowServer's work. Other agents were building in parallel
worktrees; the load averages were 4.95 before the recorded run and 5.34
after it ([environment.txt](environment.txt)).

## The harness

[enhancecheck.swift](enhancecheck.swift), built and run by
[run.sh](run.sh): the script appends it to a copy of the runner's own
`DesktopVideo.swift`, as one file, so that the file-private shaders,
`EnhanceGpu`'s encoding, `EnhancedPresenter` and `LayerPresenter` it runs
are the ones the app ships (`swiftc -O`, linked against FlutterMacOS only
because the file's plugin needs it). Output of the recorded run:
[enhancecheck.txt](enhancecheck.txt). Review then changed the presenter
(the metal layer on whole pixels, the drawable taken before the picture,
the content layer kept live until the reveal, and the hand-off's
callbacks), not the shaders or how the passes are encoded, so the GPU time
and output below still hold. The presenter's sections were run again on
that code with `ENHANCECHECK_ONLY=presenter`, which skips the offscreen
ones: [presenter.txt](presenter.txt), load averages 4.46 before and 3.77
after ([presenter-environment.txt](presenter-environment.txt)), same Mac,
display asleep.

The test pictures are drawn with AppKit (window chrome, text from 9 to 14
points at one point a pixel, regular and monospaced, one line red, rules one
and two pixels wide) and converted to 420v as a pb-mapper host converts:
sRGB values through the BT.709 matrix, video range, chroma averaged over
2x2, no transfer conversion, tagged BT.709 matrix and primaries with an sRGB
transfer, as the layer presenter tags its pictures. The plain layer, the
baseline, is Core Animation itself: the 420v IOSurface as a layer's
contents, linear filter, rendered with `CARenderer` into an sRGB target at
the same size (its rows bottom up, which the harness flips).

## GPU time

Each case's picture, 30 pictures to warm up and then 300, one at a time
(the next encoded only once the last finished, as a picture at a time is
what the presenter thread draws): both passes in one command buffer as the
runner encodes them, `gpuStartTime` to `gpuEndTime`, and the CPU's time from
encoding to `waitUntilScheduled` returning; then each pass in a command
buffer of its own, pass 1 finished before pass 2 is committed. Medians, ms:

| Case | Picture to output | Both passes | Pass 1 (upscale) | Pass 2 (sharpen) | CPU, encode to scheduled |
| --- | --- | --- | --- | --- | --- |
| 1080p full screen on a 3024x1964 panel | 1920x1080 to 3024x1701 | 0.519 | 0.351 to 0.395 | 0.173 to 0.198 | 0.048 to 0.110 |
| 1440p full screen on a 3456x2234 panel | 2560x1440 to 3456x1944 | 0.673 | 0.454 | 0.224 to 0.225 | 0.050 to 0.062 |
| 1080p full screen on the 4096x2304 G24H2 | 1920x1080 to 4096x2304 | 0.940 to 0.941 | 0.634 | 0.313 to 0.315 | 0.057 to 0.132 |
| 1440p full screen on the G24H2 | 2560x1440 to 4096x2304 | 0.942 to 0.944 | 0.635 to 0.636 | 0.314 to 0.322 | 0.118 to 0.157 |
| 4K full screen on the G24H2 | 3840x2160 to 4096x2304 | 0.942 | 0.635 | 0.314 | 0.053 to 0.062 |
| 1080p at 1:1 | 1920x1080, sharpening only | 0.077 | – | 0.077 | 0.019 to 0.029 |

The ranges are over the three levels, which run the same shader with
another constant: the level changes nothing. The cost follows the output's
pixels, not the picture's: 1080p, 1440p and 4K into 4096x2304 cost the
same. In eight of the 18 level runs, while the other agents' builds loaded
the machine, the CPU column's maximum reached 4.1 to 9.4 ms and in four of
them its p95 1.4 to 3.1 ms; the GPU columns' p95 stayed within 0.05 ms of
their medians except in those four, where it rose up to 0.7 ms above.
The M6's GPU clock was whatever it chose for one picture at a time; a
stream at 60 or 120 pictures a second may run it faster.

The shaders compiled at run time in 1.0 ms in the recorded run, with
Metal's shader cache holding them; in the first runs on this Mac, before
it did, they took 110 ms ([an earlier run](enhancecheck-earlier.txt), on
the runner before two later edits that left the shaders and the GPU work
as they are: the platform thread's lock-free check of the route, and
comparing a handed-back picture by its unit rather than its buffer). The
presenter thread compiles them, never the platform thread.

## Output

- **Colour.** At 1:1 with sharpening off (peak 0, which only the harness
  can ask for), eight flat patches came out identical to the plain layer's,
  level for level (largest difference 0), each within 1 level of the sRGB
  source.
- **A flat field stays flat.** A uniform grey, enlarged to 4096x2304 and at
  1:1, changed by 0 levels at every level against peak 0.
- **Ringing.** A black-white edge and 3-pixel bars (luma 16 and 235),
  enlarged 1.6x: the upscale alone stayed within 16 to 235 (the clamp to
  the four nearest texels); the sharpening reached 13 to 237 at Low, 11 to
  239 at Medium and 9 to 241 at High. CAS lessens it where the 3x3 already
  has contrast; it does not stop it.
- **Sharpness.** The mean Sobel gradient of luma over the text region, on
  the same pixels: the upscale alone raised it 10 to 14 % over the plain
  layer, and Low, Medium and High 12 to 16 %, 13 to 17 % and 15 to 20 %:

| Case | Plain layer | Upscale only | Low | Medium | High |
| --- | --- | --- | --- | --- | --- |
| 1920x1080 to 3024x1701 | 80.08 | 90.38 (+13 %) | 92.16 (+15 %) | 93.59 (+17 %) | 95.30 (+19 %) |
| 2560x1440 to 3456x1944 | 59.29 | 66.19 (+12 %) | 67.44 (+14 %) | 68.44 (+15 %) | 69.60 (+17 %) |
| 1920x1080 to 4096x2304 | 65.87 | 75.02 (+14 %) | 76.30 (+16 %) | 77.38 (+17 %) | 78.73 (+20 %) |
| 2560x1440 to 4096x2304 | 54.03 | 61.03 (+13 %) | 62.21 (+15 %) | 63.16 (+17 %) | 64.30 (+19 %) |
| 3840x2160 to 4096x2304 | 28.79 | 31.70 (+10 %) | 32.23 (+12 %) | 32.63 (+13 %) | 33.09 (+15 %) |

  A gradient says how much edges stand out, not how they read. The crops
  in [crops/](crops) are the same 320x140 region of the 1080p picture at
  3024x1701, from the plain layer, the upscale alone and each level, for
  the eye; Screenshot.app crops of a lit screen are still to come.

## The presenter, in a window

From [presenter.txt](presenter.txt). `LayerPresenter` hosted in a
layer-backed view in an `NSWindow` on the asleep display (backing scale 2,
`occlusionState` not visible, which the viewer itself takes as a reason not
to enhance; the harness sets the target directly). The view sits as
Flutter hosts a platform view (`FlutterMutatorView.mm`): inside a flipped
mutator view at 100.25,37.75 points and a flipped container that carries
the zoom as its sublayer transform, at 640.3x360.17 points.

- **On whole pixels.** `pixelPlacement`, which places the metal layer, gave
  at rest, after the mutator moved by a fraction of a point and under a
  1.37x zoom a frame that lands in the root layer within 1.1e-13 pixels of
  whole pixels, at exactly the size in pixels the drawable takes, each edge
  at most 0.5 pixels from the view's (the view itself at 200.5,75.5 pixels
  and 1280.6x720.34 at rest). Once enhancing, the metal layer was at
  201,76 pixels, 1281x721, in the root layer.
- **What re-measures it.** Views new in the window joined its layer tree
  only in the run loop's next pass, which called the view's `layout()`
  once; setting the view's frame unchanged, as Flutter does each frame it
  composites it, called its `setFrameSize:` once; KVO on a layer's
  `sublayerTransform` fired for a change but not when the same transform
  was set again. So a move of the picture alone reaches the viewer through
  `setFrameSize:`, a zoom through the KVO, and a view new in the window
  through `layout()`.
- Three plain commits, then the enhancement at 3840x2160, Medium: the next
  three pictures, before the reveal, were still committed to the content
  layer. The presenter thread took its drawable first and then the newest
  picture, so its first present was the third of those, not the picture
  the activation started from; the platform thread revealed the metal
  layer after it.
- A feeder thread handed 600 pictures at 120 a second to
  `LayerPresenter.show`, as the decoder thread does: each hand-off took a
  median 0.005 ms (p95 0.007, maximum 0.012). The presenter thread
  presented all 600, none superseded in the slot, a median 1.70 ms after
  the hand-off (p95 2.98, maximum 6.80), at 3840x2160. `nextDrawable` never
  held it up with the display asleep.
- A new size and level with no new picture redrew the last picture once.
- Back to the content layer: it took the newest picture the enhancement
  had (frame 606 of 606) in the transaction that showed it, and the next
  picture was committed there. A second activation revealed again.
- Released, then a new enhancement: told that the released one presented
  its epoch 1, which the new one's first activation also is, the content
  layer stayed; the new one's own first present revealed it. The end
  released the thread with no failure.
- `CALayer.convert(_:to:)` gave a 960x540 layer under its container's 2x
  sublayer transform as 1920x1080 in the root layer.

The first full run ([enhancecheck.txt](enhancecheck.txt)), on the code
before review, had the same hand-off (median 0.005 ms) and presents
(median 1.70 ms after the hand-off, p95 2.99, maximum 4.69).

Not run here: a drawable that never comes (the content layer takes the
picture back after two seconds without one) and a present that finishes
after the content layer took a newer picture back (no receipt, no
snapshot change), which need a viewer with a decoder; they rest on review.

## In the app, with the display asleep

One acceptance replay of the release app, built as
[the layer presenter's record](../2026-11-24-mac-viewer-layer/README.md)
builds it (`build-app.sh`'s command) with `--dart-define=presenter=layer`
and `--dart-define=enhance=medium`, maximum 60 frames a second, Balanced,
native resolution, run by that record's `run.sh` against
`desktop_ui_host` replaying a 1920x1080 clip from
[clipgen.swift](../2026-11-24-viewer-decoder-stats/clipgen.swift) (240
frames, one IDR) at 60 a second for 40 s, relay over TCP on loopback.
[replay.py](replay.py) reads the report: [summary](replay-medium-60.txt),
[the report's native samples](replay-medium-60.json),
[the runner's log lines](replay-medium-60.log).

- The view hosted the layer and VideoToolbox configured for 1920x1080 in
  hardware. Over the 35 one-second intervals after the warm-up it decoded
  and presented a median 60 pictures a second with none dropped;
  `timings.present` (decode end to commit) had a median 0.60 ms and
  `timings.decode` 1.07 ms.
- No sample carried `enhanced`: with the display asleep the window was not
  visible, so the viewer left the picture on the content layer, as it does
  for an occluded or minimised window. This run shows that the layer
  presenter's commits and receipts still work through the reworked
  hand-off, and that asking for the enhancement where it cannot run costs
  the picture nothing; it says nothing about the enhancement in the app.

The same replay again, built the same way from the code after review,
whose receipts now take only a picture newer than the last one
acknowledged (built before a last rename of a local in the presenter
thread, which this run never reaches), with Flutter 3.50.1 and a host
description without "display asleep": the harness had found the display
asleep five minutes before. [Summary](replay-review-medium-60.txt),
[samples](replay-review-medium-60.json),
[log lines](replay-review-medium-60.log); load averages 6.52 before and
15.76 after ([environment](replay-review-environment.txt)), other agents
building. Over the 35 intervals it decoded and presented a median 60
pictures a second with none dropped; `timings.present` had a median
0.27 ms and `timings.decode` 0.89 ms. In the last 11 intervals, as that
load rose, the decoder took up to a mean of 30 ms a picture and the rate
fell as low as 15 a second before it came back to 60; all 496 pictures
decoded in them were presented, none dropped. No sample carried
`enhanced`.

## Left for a run with the display on

On this Mac's G24H2 and on a Retina panel, with the acceptance tool built
with `--dart-define=presenter=layer` and `--dart-define=enhance=...`
([the macOS viewer procedure](../../../scripts/test/desktop-ui-acceptance.md#macos-viewer)):
the passes' GPU time and WindowServer's in `Metal System Trace` at each
level, `timings.present` against the plain layer, `timings.decode` and the
decoded rate unchanged at 120 frames a second into 60 Hz, Screenshot.app
crops of the same text at each level, crops of a letterboxed full-screen
viewer at window heights that put the picture's edge on a half pixel (the
metal layer must stay as sharp as at a whole one), and a pinch zoom, a
window resize and a pan while the desktop is still, which must redraw and
keep the picture on whole pixels.
