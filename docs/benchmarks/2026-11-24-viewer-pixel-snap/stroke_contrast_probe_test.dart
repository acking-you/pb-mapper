// The probe behind this directory's README: renders a picture of 1-pixel
// strokes through RemoteSurface at a fractional place in the view and
// prints the contrast left across each stroke. Copy it into ui/test and
// run `flutter test test/stroke_contrast_probe_test.dart` from ui/; at
// 1515eda, configure() is the two lines the README gives.
import 'dart:math' as math;
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/gestures.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_surface.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_viewport.dart';
import 'package:pb_mapper_ui/src/state/viewer_prefs.dart';

import 'remote_viewer_harness.dart';

const _w = 1920, _h = 1080;

/// The viewport's pixels and mode, as _ViewportSync sets them.
void configure(RemoteViewport v, ViewerScaleMode mode, double dpr) {
  v.aspectRatio = _w / _h;
  v.setPixels(
    display: const Size(1920, 1080),
    video: const Size(1920, 1080),
    dpr: dpr,
  );
  v.setMode(mode);
}

Future<ui.Image> _strokes() async {
  final rgba = Uint8List(_w * _h * 4);
  for (var y = 0; y < _h; y++) {
    for (var x = 0; x < _w; x++) {
      final dark = x % 8 == 3 || y % 64 == 5;
      final i = (y * _w + x) * 4;
      final v = dark ? 0 : 255;
      rgba[i] = rgba[i + 1] = rgba[i + 2] = v;
      rgba[i + 3] = 255;
    }
  }
  final buffer = await ui.ImmutableBuffer.fromUint8List(rgba);
  final descriptor = ui.ImageDescriptor.raw(
    buffer,
    width: _w,
    height: _h,
    pixelFormat: ui.PixelFormat.rgba8888,
  );
  final codec = await descriptor.instantiateCodec();
  return (await codec.getNextFrame()).image;
}

/// Per stroke: 255 minus its darkest pixel's darkness deficit, i.e. the
/// contrast against the white ground. Lines touched by a stroke across
/// them are skipped (less than 60% white ground).
(int, double, int) _contrast(ByteData px, int width, Rect r, bool rows) {
  final strokes = <int>[];
  int lum(int x, int y) => px.getUint8((y * width + x) * 4 + 1);
  final outer = rows
      ? [for (var y = r.top.ceil(); y < r.bottom.floor(); y++) y]
      : [for (var x = r.left.ceil(); x < r.right.floor(); x++) x];
  final inner = rows
      ? [for (var x = r.left.ceil(); x < r.right.floor(); x++) x]
      : [for (var y = r.top.ceil(); y < r.bottom.floor(); y++) y];
  for (final o in outer) {
    var white = 0;
    for (final i in inner) {
      if ((rows ? lum(i, o) : lum(o, i)) >= 250) white++;
    }
    // A line touched by a stroke across it has no white ground.
    if (white < inner.length * 0.6) continue;
    // A stroke cut by the measured rect's edge, at either end, is not
    // counted: part of it lies outside.
    var run = 256;
    var cut = true;
    for (final i in inner) {
      final v = rows ? lum(i, o) : lum(o, i);
      if (v < 250) {
        run = math.min(run, v);
      } else {
        if (run < 256 && !cut) strokes.add(255 - run);
        run = 256;
        cut = false;
      }
    }
  }
  if (strokes.isEmpty) return (-1, -1, -1);
  final mean = strokes.reduce((a, b) => a + b) / strokes.length;
  return (strokes.reduce(math.min), mean, strokes.length);
}

void main() {
  for (final (label, mode, dpr, stage) in [
    for (final dpr in [1.0, 1.25, 1.5, 2.0])
      ('original', ViewerScaleMode.original, dpr, const Size(700.5, 420.3)),
    ('near-1 fit', ViewerScaleMode.fit, 1.0, const Size(1950, 1100)),
    ('near-1 fit', ViewerScaleMode.fit, 1.25, const Size(1560, 880)),
    ('near-1 fit', ViewerScaleMode.fit, 1.5, const Size(1300, 733.4)),
    ('near-1 fit', ViewerScaleMode.fit, 2.0, const Size(975, 550)),
  ]) {
    testWidgets('$label at dpr $dpr', (tester) async {
      const origin = Offset(10.4, 36.6);
      final h = await ViewerHarness.live(tester);
      final image = (await tester.runAsync(_strokes))!;
      tester.view
        ..devicePixelRatio = dpr
        ..physicalSize =
            Size(origin.dx + stage.width + 20, origin.dy + stage.height + 20) *
            dpr;
      addTearDown(tester.view.reset);
      final root = GlobalKey();
      await tester.pumpWidget(
        RepaintBoundary(
          key: root,
          child: Directionality(
            textDirection: TextDirection.ltr,
            child: ColoredBox(
              color: const Color(0xFF808080),
              child: Align(
                alignment: Alignment.topLeft,
                child: Padding(
                  padding: EdgeInsets.only(left: origin.dx, top: origin.dy),
                  child: SizedBox.fromSize(
                    size: stage,
                    child: RemoteSurface(
                      viewport: h.viewport,
                      input: h.input,
                      machine: h.machine,
                      touchMode: h.touchMode,
                      child: RawImage(
                        image: image,
                        fit: BoxFit.fill,
                        filterQuality: FilterQuality.low,
                      ),
                    ),
                  ),
                ),
              ),
            ),
          ),
        ),
      );
      configure(h.viewport, mode, dpr);
      await tester.pump();
      await tester.pump();
      final mouse = await tester.createGesture(kind: PointerDeviceKind.mouse);
      await mouse.addPointer(location: origin + const Offset(1, 1));
      final random = math.Random(7);
      final xs = <int>[], ys = <int>[];
      final positions = mode == ViewerScaleMode.original ? 16 : 1;
      for (var i = 0; i < positions; i++) {
        await mouse.moveTo(
          origin +
              Offset(
                random.nextDouble() * stage.width,
                random.nextDouble() * stage.height,
              ),
        );
        await tester.pump();
        await tester.pump();
        final v = h.viewport;
        final picture = (origin + v.offset) & v.scene * v.scale;
        final stageRect = origin & stage;
        final shown = picture.intersect(stageRect);
        final device = Rect.fromLTRB(
          shown.left * dpr + 2,
          shown.top * dpr + 2,
          shown.right * dpr - 2,
          shown.bottom * dpr - 2,
        );
        final boundary =
            root.currentContext!.findRenderObject()! as RenderRepaintBoundary;
        final px = (await tester.runAsync(() async {
          final img = await boundary.toImage(pixelRatio: dpr);
          final data = await img.toByteData();
          final w = img.width;
          img.dispose();
          return (data!, w);
        }))!;
        final x = _contrast(px.$1, px.$2, device, true);
        final y = _contrast(px.$1, px.$2, device, false);
        xs.add(x.$1);
        ys.add(y.$1);
        final edge = (origin + v.offset) * dpr;
        // ignore: avoid_print
        print(
          'PROBE $label dpr=$dpr pos=$i '
          'scale_px=${(v.scene.width * v.scale * dpr).toStringAsFixed(3)} '
          'edge=(${edge.dx.toStringAsFixed(3)},${edge.dy.toStringAsFixed(3)}) '
          'vertical_strokes(min=${x.$1} mean=${x.$2.toStringAsFixed(1)} n=${x.$3}) '
          'horizontal_strokes(min=${y.$1} mean=${y.$2.toStringAsFixed(1)} n=${y.$3})',
        );
      }
      // ignore: avoid_print
      print(
        'SUMMARY $label dpr=$dpr positions=$positions '
        'vertical_min=${xs.reduce(math.min)} vertical_max=${xs.reduce(math.max)} '
        'horizontal_min=${ys.reduce(math.min)} horizontal_max=${ys.reduce(math.max)}',
      );
      await mouse.removePointer();
      await tester.pumpWidget(const SizedBox.shrink());
      image.dispose();
      h.dispose();
    });
  }
}
