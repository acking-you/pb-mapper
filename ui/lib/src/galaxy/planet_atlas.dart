/// The galaxy's glows, shadows and lit spheres as sprites. Each layer of a
/// sphere is rendered once per kind, colour and size into one atlas image,
/// and a frame draws them with `drawRawAtlas` in the order the vector painter
/// would, each layer at its own colour and opacity, so a frame needs no blur
/// or gradient of its own. A layer is painted as vectors in the one frame that
/// first asks for it; the end of that frame takes it into the atlas.
library;

import 'dart:math' as math;
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';

import 'planet_glyph.dart';

/// Paints a cell's layer at the origin, at its size of [unit] pixels and full
/// strength.
typedef _CellPaint = void Function(Canvas canvas, double unit);

/// One layer's image in the atlas.
class _Cell {
  _Cell(this.rect, this.anchor, this.unit, this.paint);

  /// Where it is in the atlas, in pixels.
  final Rect rect;

  /// The layer's centre, from [rect]'s top left.
  final Offset anchor;

  /// The layer's size (its radius) in pixels.
  final double unit;

  final _CellPaint paint;

  /// Whether the atlas image holds it yet.
  bool ready = false;
}

/// A stretch of the atlas's skyline: from [x], [width] wide, filled down to
/// [y].
class _Span {
  _Span(this.x, this.y, this.width);

  final double x;
  final double y;
  final double width;
}

/// Growable sprite arguments, reused from frame to frame.
class _Floats {
  Float32List _data = Float32List(256);
  int _length = 0;

  bool get isEmpty => _length == 0;

  Float32List get view => Float32List.sublistView(_data, 0, _length);

  void add(double a, double b, double c, double d) {
    if (_length + 4 > _data.length) {
      _data = Float32List(_data.length * 2)..setRange(0, _length, _data);
    }
    _data
      ..[_length] = a
      ..[_length + 1] = b
      ..[_length + 2] = c
      ..[_length + 3] = d;
    _length += 4;
  }

  void clear() => _length = 0;
}

class _Colors {
  Int32List _data = Int32List(64);
  int _length = 0;

  Int32List get view => Int32List.sublistView(_data, 0, _length);

  void add(int argb) {
    if (_length == _data.length) {
      _data = Int32List(_data.length * 2)..setRange(0, _length, _data);
    }
    _data[_length++] = argb;
  }

  void clear() => _length = 0;
}

/// The sprites of one galaxy canvas, owned with its painter state.
///
/// Sharp layers (bodies and rims) are rendered per size
/// bucket, four per octave of on-screen pixels, and drawn at most 16 % smaller
/// than rendered. Soft layers (blurred discs, contact shadows, the plane's
/// glow) blur over several pixels, so two buckets per octave serve them, and
/// above a small size they are scaled up, which a blur survives. Colours
/// other than a body's are applied when drawn (white images, modulated), so
/// one image serves every colour. Cells are packed bottom-left on a skyline in
/// one atlas of at most 512×2048 pixels (4 MB).
///
/// The images are created synchronously while painting, so none can arrive
/// after the canvas is hidden or its theme changes. A disabled atlas (a
/// background canvas) holds no images and creates none: it paints vectors.
class PlanetAtlas implements SphereLayers {
  final Map<int, _Cell> _cells = {};
  final List<_Cell> _fresh = [];
  ui.Image? _image;
  Canvas? _canvas;
  ColorScheme? _scheme;
  bool _enabled = false;
  double _ratio = 1;
  bool _full = false;
  int _frames = 0;
  final List<_Span> _skyline = [_Span(0, 0, _width.toDouble())];
  double _height = 0;

  ui.Image? _plane;
  (int, int)? _planeKey;
  Rect _planeSource = Rect.zero;

  final _transforms = _Floats();
  final _rects = _Floats();
  final _colors = _Colors();
  final Paint _paint = Paint()..filterQuality = FilterQuality.low;

  static int _liveBytes = 0;

  /// Bytes held by every canvas's sprite images: for diagnostics (the galaxy
  /// resource probe).
  static int get liveBytes => _liveBytes;

  static const int _width = 512;
  static const double _maxHeight = 2048;
  static const double _pad = 2;

  /// A full atlas starts afresh at most this often (in frames), so it drops
  /// sizes no longer drawn without rebuilding every frame; in between, what
  /// does not fit is painted as vectors.
  static const int _refill = 600;

  static const int _sharp = 4;
  static const int _smooth = 2;

  /// Largest buckets: 16 px for blurred discs, 32 px for shadows, 128 px for
  /// sharp layers (larger ones are painted as vectors).
  static const int _softMax = 4 * _smooth;
  static const int _shadowMax = 5 * _smooth;
  static const int _sharpMax = 7 * _sharp;

  static const int _soft = 1;
  static const int _shadow = 2;
  static const int _body = 3;
  static const int _rim = 4;

  set enabled(bool enabled) {
    if (_enabled == enabled) return;
    _enabled = enabled;
    if (!enabled) dispose();
  }

  /// A new theme starts afresh.
  void palette(ColorScheme scheme) {
    if (_scheme == scheme) return;
    dispose();
    _scheme = scheme;
  }

  /// The atlas image, for tests.
  @visibleForTesting
  ui.Image? get debugImage => _image;

  /// Start a frame on [canvas], drawn at device pixel [ratio].
  void begin(Canvas canvas, {required double ratio}) {
    _canvas = canvas;
    _ratio = ratio;
    _frames++;
    // Nothing carries over from a frame that did not finish.
    _transforms.clear();
    _rects.clear();
    _colors.clear();
    if (_full && _frames > _refill) _clear();
  }

  /// Draw what is queued. Anything painted on the canvas directly must come
  /// after, so sprites stay under what is drawn after them.
  void flush() {
    final image = _image;
    final canvas = _canvas;
    if (_transforms.isEmpty || image == null || canvas == null) return;
    canvas.drawRawAtlas(
      image,
      _transforms.view,
      _rects.view,
      _colors.view,
      BlendMode.modulate,
      null,
      _paint,
    );
    _transforms.clear();
    _rects.clear();
    _colors.clear();
  }

  /// End the frame: draw what is queued, and render the layers it asked for
  /// into the atlas the next frame draws from.
  void end() {
    flush();
    _canvas = null;
    if (_fresh.isNotEmpty) _render();
  }

  /// Release the images.
  void dispose() {
    _clear();
    _setPlane(null);
    _planeKey = null;
  }

  @override
  void softDisc(Offset center, double radius, double sigma, Color color) {
    if (radius <= 0 || color.a <= 0) return;
    if (sigma <= 0) {
      _vector.softDisc(center, radius, sigma, color);
      return;
    }
    // The blur relative to the disc, in sixteenths of an octave (±2 %).
    final blur = _relative(sigma / radius, 16);
    final bucket = math.min(_bucket(radius, _smooth), _softMax);
    final key = _key(_soft, blur.$1 + 1024, bucket);
    if (_draw(key, center, radius, color)) return;
    if (!_cells.containsKey(key)) {
      final relative = blur.$2;
      final e = 1 + 3 * relative;
      _add(
        key,
        _unit(bucket, _smooth),
        Rect.fromLTRB(-e, -e, e, e),
        (canvas, unit) => CanvasSphereLayers(
          canvas,
        ).softDisc(Offset.zero, unit, unit * relative, Colors.white),
      );
    }
    _vector.softDisc(center, radius, sigma, color);
  }

  @override
  void contactShadow(Offset center, double radius, Color color) {
    if (radius <= 0 || color.a <= 0) return;
    final bucket = math.min(_bucket(radius, _smooth), _shadowMax);
    final key = _key(_shadow, 0, bucket);
    if (_draw(key, center, radius, color)) return;
    if (!_cells.containsKey(key)) {
      final shape = contactShadowShape(Offset.zero, 1);
      _add(
        key,
        _unit(bucket, _smooth),
        shape.oval.inflate(shape.sigma * 3),
        (canvas, unit) => CanvasSphereLayers(
          canvas,
        ).contactShadow(Offset.zero, unit, Colors.white),
      );
    }
    _vector.contactShadow(center, radius, color);
  }

  @override
  void body(
    Offset center,
    double radius,
    Color color, {
    required bool dark,
    required double alpha,
  }) {
    if (radius <= 0 || alpha <= 0) return;
    final bucket = _bucket(radius, _sharp);
    if (bucket <= _sharpMax) {
      // The alpha is applied when drawn: only the colour picks the image.
      final rgb = color.toARGB32() & 0xFFFFFF;
      final key = _key(_body, rgb * 2 + (dark ? 1 : 0), bucket);
      if (_draw(key, center, radius, _white(alpha))) return;
      if (!_cells.containsKey(key)) {
        final opaque = color.withValues(alpha: 1);
        _add(
          key,
          _unit(bucket, _sharp),
          _unitBox,
          (canvas, unit) => CanvasSphereLayers(
            canvas,
          ).body(Offset.zero, unit, opaque, dark: dark, alpha: 1),
        );
      }
    }
    _vector.body(center, radius, color, dark: dark, alpha: alpha);
  }

  @override
  void rim(Offset center, double radius, double width, double alpha) {
    if (radius <= 0 || alpha <= 0) return;
    final bucket = _bucket(radius, _sharp);
    if (bucket <= _sharpMax) {
      // The width relative to the radius, in eighths of an octave (±4 %).
      final relative = _relative(width / radius, 8);
      final key = _key(_rim, relative.$1 + 1024, bucket);
      if (_draw(key, center, radius, _white(alpha))) return;
      if (!_cells.containsKey(key)) {
        final w = relative.$2;
        final e = 0.94 + w / 2;
        _add(
          key,
          _unit(bucket, _sharp),
          Rect.fromLTRB(-e, -e, e, e),
          (canvas, unit) =>
              CanvasSphereLayers(canvas).rim(Offset.zero, unit, unit * w, 1),
        );
      }
    }
    _vector.rim(center, radius, width, alpha);
  }

  /// [oval] blurred by [sigma] in [color] (the plane's glow), from an image
  /// of its shape rendered small and stretched over it.
  void softOval(Rect oval, double sigma, Color color) {
    if (oval.isEmpty ||
        !oval.isFinite ||
        !sigma.isFinite ||
        sigma <= 0 ||
        color.a <= 0) {
      return;
    }
    final canvas = _canvas!;
    flush();
    // A nearly edge-on or zoomed plane can span an enormous region. Never
    // allocate an image proportional to it; the viewport clips the fallback.
    final cacheable = oval.width / sigma < 240 && oval.height / sigma < 240;
    if (_enabled && cacheable) {
      // The shape is its size relative to its blur, in 1/32 octaves.
      int log(double v) => (math.log(v) / math.ln2 * 32).round();
      final key = (log(oval.width / sigma), log(oval.height / sigma));
      if (key != _planeKey) {
        _planeKey = key;
        // A blur of 8 px, which scales up smoothly.
        const blur = 8.0;
        final size = Size(
          math.pow(2, key.$1 / 32) * blur,
          math.pow(2, key.$2 / 32) * blur,
        );
        _planeSource = (Offset.zero & size)
            .inflate(blur * 3)
            .shift(const Offset(blur * 3, blur * 3));
        final recorder = ui.PictureRecorder();
        Canvas(recorder).drawOval(
          const Offset(blur * 3, blur * 3) & size,
          Paint()
            ..color = Colors.white
            ..maskFilter = const MaskFilter.blur(BlurStyle.normal, blur),
        );
        final picture = recorder.endRecording();
        try {
          _setPlane(
            picture.toImageSync(
              _round(_planeSource.width, 16),
              _round(_planeSource.height, 16),
            ),
          );
        } catch (_) {
          // No rasteriser: the glow is painted as a vector.
          _setPlane(null);
        } finally {
          picture.dispose();
        }
      }
    }
    final image = _enabled && cacheable ? _plane : null;
    if (image == null) {
      canvas.drawOval(
        oval,
        Paint()
          ..color = color
          ..maskFilter = MaskFilter.blur(BlurStyle.normal, sigma),
      );
      return;
    }
    canvas.drawImageRect(
      image,
      _planeSource,
      oval.inflate(sigma * 3),
      Paint()
        ..filterQuality = FilterQuality.low
        ..colorFilter = ColorFilter.mode(color, BlendMode.modulate),
    );
  }

  static final Rect _unitBox = Rect.fromLTRB(-1, -1, 1, 1);

  static int _white(double alpha) =>
      (((alpha.clamp(0.0, 1.0) * 255).round()) << 24) | 0xFFFFFF;

  /// Vector layers, after what is queued.
  CanvasSphereLayers get _vector {
    flush();
    return CanvasSphereLayers(_canvas!);
  }

  /// The size bucket of [size] (in logical pixels) on screen, [perOctave]
  /// buckets per octave.
  int _bucket(double size, int perOctave) => math.max(
    -2 * perOctave,
    (math.log(size * _ratio) / math.ln2 * perOctave).ceil(),
  );

  static double _unit(int bucket, int perOctave) =>
      math.pow(2, bucket / perOctave).toDouble();

  /// [ratio] rounded to a [perOctave] fraction of an octave: the step, and
  /// the ratio it stands for.
  static (int, double) _relative(double ratio, int perOctave) {
    final step = (math.log(ratio) / math.ln2 * perOctave).round();
    return (step, math.pow(2, step / perOctave).toDouble());
  }

  /// [size] rounded up to a multiple of [step]: images of few sizes, which
  /// the GPU can reuse as they are replaced, instead of one size each.
  static int _round(double size, int step) => (size / step).ceil() * step;

  static int _key(int kind, int variant, int bucket) =>
      kind * (1 << 44) + variant * (1 << 12) + bucket + 2048;

  /// Queue the cell of [key] at [center] and [size] in [color] (as ARGB
  /// colour or an int), if the atlas holds it.
  bool _draw(int key, Offset center, double size, Object color) {
    final cell = _cells[key];
    if (cell == null || !cell.ready) return false;
    final scale = size / cell.unit;
    final anchor = cell.anchor;
    _transforms.add(
      scale,
      0,
      center.dx - scale * anchor.dx,
      center.dy - scale * anchor.dy,
    );
    final rect = cell.rect;
    _rects.add(rect.left, rect.top, rect.right, rect.bottom);
    _colors.add(color is Color ? color.toARGB32() : color as int);
    return true;
  }

  /// Place a cell of [unit] pixels spanning [box] (in units around its
  /// centre) for the next frame's atlas.
  void _add(int key, double unit, Rect box, _CellPaint paint) {
    if (!_enabled || _full) return;
    final width = (box.width * unit).ceilToDouble() + _pad * 2;
    final height = (box.height * unit).ceilToDouble() + _pad * 2;
    final rect = _place(width, height);
    if (rect == null) {
      _full = true;
      return;
    }
    final cell = _Cell(
      rect,
      Offset(_pad - box.left * unit, _pad - box.top * unit),
      unit,
      paint,
    );
    _cells[key] = cell;
    _fresh.add(cell);
  }

  /// The lowest room for a cell on the skyline, leftmost first, or null when
  /// the atlas is full.
  Rect? _place(double width, double height) {
    var best = -1;
    var bestY = double.infinity;
    for (var i = 0; i < _skyline.length; i++) {
      final x = _skyline[i].x;
      if (x + width > _width) break;
      // The cell rests on the highest span it covers.
      var y = 0.0;
      for (var j = i; j < _skyline.length && _skyline[j].x < x + width; j++) {
        y = math.max(y, _skyline[j].y);
      }
      if (y < bestY) {
        best = i;
        bestY = y;
      }
    }
    if (best < 0 || bestY + height > _maxHeight) return null;
    final x = _skyline[best].x;
    // Replace the spans the cell covers by its top, keeping what sticks out.
    final right = x + width;
    var end = best;
    while (end < _skyline.length && _skyline[end].x < right) {
      end++;
    }
    final last = _skyline[end - 1];
    final rest = last.x + last.width - right;
    _skyline.replaceRange(best, end, [
      _Span(x, bestY + height, width),
      if (rest > 0) _Span(right, last.y, rest),
    ]);
    _height = math.max(_height, bestY + height);
    return Rect.fromLTWH(x, bestY, width, height);
  }

  /// Render the atlas again: the last image as it was, the new cells below
  /// and beside it.
  void _render() {
    final recorder = ui.PictureRecorder();
    final canvas = Canvas(recorder);
    final old = _image;
    if (old != null) canvas.drawImage(old, Offset.zero, Paint());
    for (final cell in _fresh) {
      final rect = cell.rect;
      canvas
        ..save()
        ..clipRect(rect)
        ..translate(rect.left + cell.anchor.dx, rect.top + cell.anchor.dy);
      cell.paint(canvas, cell.unit);
      canvas.restore();
    }
    final picture = recorder.endRecording();
    try {
      _setImage(picture.toImageSync(_width, _round(_height, 64)));
      for (final cell in _fresh) {
        cell.ready = true;
      }
    } catch (_) {
      // No rasteriser: these layers stay vectors.
    } finally {
      picture.dispose();
      _fresh.clear();
    }
  }

  void _setImage(ui.Image? image) {
    final old = _image;
    if (old != null) {
      _liveBytes -= old.width * old.height * 4;
      old.dispose();
    }
    _image = image;
    if (image != null) _liveBytes += image.width * image.height * 4;
  }

  void _setPlane(ui.Image? image) {
    final old = _plane;
    if (old != null) {
      _liveBytes -= old.width * old.height * 4;
      old.dispose();
    }
    _plane = image;
    if (image != null) _liveBytes += image.width * image.height * 4;
  }

  void _clear() {
    _transforms.clear();
    _rects.clear();
    _colors.clear();
    _setImage(null);
    _cells.clear();
    _fresh.clear();
    _full = false;
    _frames = 0;
    _skyline
      ..clear()
      ..add(_Span(0, 0, _width.toDouble()));
    _height = 0;
  }
}
