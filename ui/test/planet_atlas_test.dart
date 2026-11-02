import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/galaxy/planet_atlas.dart';
import 'package:pb_mapper_ui/src/galaxy/planet_glyph.dart';

const _ratio = 2.0;
const _size = Size(460, 240);

void _scene(SphereLayers layers, bool dark, void Function(Offset) between) {
  const colors = [Color(0xFF3FB950), Color(0xFFE5534B), Color(0xFF8E8E8A)];
  const radii = [2.4, 4.4, 7.3, 14.0, 26.0];
  var x = 30.0;
  for (final radius in radii) {
    var y = 30.0;
    for (final (i, opacity) in [1.0, 0.62, 0.3].indexed) {
      final center = Offset(x, y);
      paintSphereLayers(
        layers,
        center,
        radius,
        colors[i],
        dark: dark,
        glow: i == 2 ? 0 : 0.9,
        opacity: opacity,
      );
      between(center);
      paintSphereLayers(
        layers,
        center + Offset(radius * 0.8, radius * 0.5),
        radius * 0.7,
        colors[(i + 1) % 3],
        dark: dark,
        opacity: opacity,
      );
      y += 70;
    }
    x += radius * 2 + 44;
  }
  layers.softDisc(
    const Offset(380, 210),
    38,
    19,
    colors[0].withValues(alpha: 0.16),
  );
}

Future<Uint8List> _pixels(void Function(Canvas) paint) async {
  final recorder = ui.PictureRecorder();
  final canvas = Canvas(recorder)..scale(_ratio);
  canvas.drawColor(const Color(0xFF808080), BlendMode.src);
  paint(canvas);
  final picture = recorder.endRecording();
  final image = await picture.toImage(
    (_size.width * _ratio).round(),
    (_size.height * _ratio).round(),
  );
  final data = await image.toByteData();
  image.dispose();
  picture.dispose();
  return data!.buffer.asUint8List();
}

double _different(Uint8List a, Uint8List b) {
  var count = 0;
  for (var i = 0; i < a.length; i++) {
    if ((a[i] - b[i]).abs() > 8) count++;
  }
  return count / a.length;
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  for (final dark in [false, true]) {
    test(
      'cached layers preserve vector colour, opacity and ordering: $dark',
      () async {
        final atlas = PlanetAtlas()
          ..palette(dark ? const ColorScheme.dark() : const ColorScheme.light())
          ..enabled = true;
        addTearDown(atlas.dispose);
        void ring(Canvas canvas, Offset center) => canvas.drawCircle(
          center,
          5,
          Paint()
            ..style = PaintingStyle.stroke
            ..strokeWidth = 1.4
            ..color = Colors.white,
        );
        void frame(Canvas canvas) {
          atlas.begin(canvas, ratio: _ratio);
          _scene(atlas, dark, (c) {
            atlas.flush();
            ring(canvas, c);
          });
          atlas.end();
        }

        final vector = await _pixels(
          (canvas) =>
              _scene(CanvasSphereLayers(canvas), dark, (c) => ring(canvas, c)),
        );
        expect(
          await _pixels(frame),
          vector,
          reason: 'the first frame uses the original vector layers',
        );
        final cached = await _pixels(frame);
        expect(
          cached,
          isNot(vector),
          reason: 'the second frame uses the atlas',
        );
        expect(
          _different(vector, cached),
          lessThan(0.0025),
          reason: 'only resampled anti-aliased edges should change',
        );
      },
    );
  }

  test('images are reused and released on hide and theme change', () {
    final atlas = PlanetAtlas()..palette(const ColorScheme.dark());
    addTearDown(atlas.dispose);
    void frame() {
      final recorder = ui.PictureRecorder();
      atlas.begin(Canvas(recorder), ratio: 2);
      paintSphereLayers(
        atlas,
        const Offset(20, 20),
        8,
        Colors.green,
        dark: true,
      );
      atlas.end();
      recorder.endRecording().dispose();
    }

    frame();
    expect(atlas.debugImage, isNull);
    atlas.enabled = true;
    frame();
    final image = atlas.debugImage!;
    frame();
    expect(atlas.debugImage, same(image));
    atlas.enabled = false;
    expect(image.debugDisposed, isTrue);
    atlas.enabled = true;
    frame();
    final themed = atlas.debugImage!;
    atlas.palette(const ColorScheme.light());
    expect(themed.debugDisposed, isTrue);
    expect(PlanetAtlas.liveBytes, 0);
  });

  test('the atlas remains bounded with many sizes and colours', () {
    final atlas = PlanetAtlas()
      ..palette(const ColorScheme.dark())
      ..enabled = true;
    addTearDown(atlas.dispose);
    ui.Image? image;
    var images = 0;
    for (var frame = 0; frame < 60; frame++) {
      final recorder = ui.PictureRecorder();
      atlas.begin(Canvas(recorder), ratio: _ratio);
      for (var i = 0; i < 40; i++) {
        atlas.body(
          const Offset(50, 50),
          60,
          Color(0xFF000000 | (frame * 40 + i) * 997),
          dark: true,
          alpha: 1,
        );
      }
      atlas.end();
      recorder.endRecording().dispose();
      if (!identical(atlas.debugImage, image)) {
        image = atlas.debugImage;
        images++;
      }
    }
    expect(image!.width, 512);
    expect(image.height, lessThanOrEqualTo(2048));
    expect(images, lessThan(10));
  });

  test('extreme or nonfinite plane bounds do not allocate a huge image', () {
    final atlas = PlanetAtlas()
      ..palette(const ColorScheme.dark())
      ..enabled = true;
    addTearDown(atlas.dispose);
    final baseline = PlanetAtlas.liveBytes;
    final recorder = ui.PictureRecorder();
    atlas.begin(Canvas(recorder), ratio: 1);
    atlas.softOval(const Rect.fromLTWH(-1e7, -1e7, 2e7, 2e7), 1, Colors.white);
    atlas.softOval(
      const Rect.fromLTRB(0, 0, double.infinity, 1),
      1,
      Colors.white,
    );
    atlas.end();
    recorder.endRecording().dispose();
    expect(PlanetAtlas.liveBytes, baseline);
  });
}
