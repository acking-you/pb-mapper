# Viewer pixel snap: measurements

How much contrast a 1-pixel stroke keeps through the desktop viewers' picture
transform, before and after a picture at one device pixel per remote pixel is
kept on the device-pixel grid, and a fit within 4% of its room is shown at
exactly 1:1.

## Environment

A 12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425), Flutter 3.50.1 (CI
pins 3.44.9). The baseline is `1515eda` (master); the measured head is
`e4f4627`. The runs are `flutter test` runs, so a debug build on the
rasteriser `flutter test` uses by default: what is measured is pixel values,
not time, and neither the build mode nor this machine's speed enters them.

## Method

[`stroke_contrast_probe_test.dart`](stroke_contrast_probe_test.dart), copied
into `ui/test` and run with `flutter test test/stroke_contrast_probe_test.dart`
from `ui/`:

- The picture is 1920×1080: black 1-pixel strokes on white, a vertical one
  every 8 pixels and a horizontal one every 64.
- It is drawn as the viewer draws a picture: a `RawImage` with
  `FilterQuality.low` (bilinear sampling, as the native `Texture` uses), in a
  `RemoteSurface` whose top left sits at (10.4, 36.6) logical pixels in the
  view: a fractional device pixel down at every ratio, and across at all but
  1.25.
- The viewport is set as `_ViewportSync` sets it: display and video
  1920×1080, at device pixel ratios 1, 1.25, 1.5 and 2.
- The whole view is rendered at that ratio with
  `RenderRepaintBoundary.toImage`, which is what the screen would show.
- A stroke's contrast is 255 minus its darkest pixel, measured inside the
  picture's visible part, 2 device pixels in from its edges. A stroke cut by
  that edge, and a line touched by a stroke across it, are left out. The
  source's own contrast is 255; a stroke half a device pixel off becomes two
  pixels of about half the darkness, a contrast of about 128.

Two cases:

- **Original size**: a 700.5×420.3 stage, smaller than the picture at every
  ratio, and a mouse hovered at 16 seeded positions in it; the view follows
  the pointer.
- **Near-1 fit**: Fit window in a stage of 1950×1100 device pixels (1950×1100,
  1560×880, 1300×733.4 and 975×550 logical), where fitting would enlarge the
  picture by 1.6%.

At `1515eda`, `configure()` sets the viewport through the API of the time:

```dart
v.aspectRatio = _w / _h;
v.setMode(mode, remotePixels: const Size(1920, 1080), dpr: dpr);
```

## Results

Original size, per position the worst stroke, over the 16 positions: the
lowest, the median and the highest.

| Device pixel ratio | Strokes | `1515eda` lowest / median / highest | `e4f4627` |
| ---: | --- | --- | --- |
| 1 | vertical | 138 / 196 / 243 | 255 at every position |
| 1 | horizontal | 130 / 212.5 / 250 | 255 at every position |
| 1.25 | vertical | 138 / 201.5 / 240 | 255 at every position |
| 1.25 | horizontal | 128 / 192.5 / 244 | 255 at every position |
| 1.5 | vertical | 135 / 173.5 / 244 | 255 at every position |
| 1.5 | horizontal | 137 / 210.5 / 254 | 255 at every position |
| 2 | vertical | 128 / 195 / 252 | 255 at every position |
| 2 | horizontal | 133 / 216 / 250 | 255 at every position |

Before, the same text was sharp at one pointer position and at half contrast
at the next; after, every stroke keeps the source's contrast wherever the
pointer is.

Near-1 fit: the picture's width on screen, and the worst stroke and the mean
over all strokes.

| Device pixel ratio | `1515eda` width | vertical worst / mean | horizontal worst | `e4f4627` width | strokes |
| ---: | ---: | --- | ---: | ---: | --- |
| 1 | 1950 | 141 / 192.5 | 193 | 1920 | 255, every stroke |
| 1.25 | 1950 | 143 / 192.2 | 155 | 1920 | 255, every stroke |
| 1.5 | 1950 | 137 / 192.4 | 154 | 1920 | 255, every stroke |
| 2 | 1950 | 135 / 192.3 | 217 | 1920 | 255, every stroke |

Before, a 1.6% enlargement left every stroke at its own phase, so each line
of text was blurred to a different degree; after, the picture is 1:1, with a
border of about 15 device pixels at the sides and 10 above and below.

Raw output: [before](before.txt), [after](after.txt). Each `PROBE` line gives
the picture's width on screen and its top left in device pixels (`edge`),
which is whole after the change and fractional before.

## Not measured here

- The on-screen check in the app: a 1920×1080 text clip replayed by
  `desktop_ui_host`, viewed at Original size in a smaller window, with
  Screenshot.app crops while the pointer moves. This Mac has no `ffmpeg` to
  make the clip, and the check moves the real pointer and captures the
  screen of an interactive session.
- Windows at 125% and 150% display scaling, and the Linux viewer, which need
  those devices.
