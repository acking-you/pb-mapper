import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/painting.dart';
import 'package:flutter_test/flutter_test.dart';

/// A remote computer's screen for previews: the RemoteScreen artboard
/// (wallpaper, desktop icons, a document window, a terminal and the taskbar)
/// painted at the 1920×1080 display [liveStatus] reports and encoded as PNG.
///
/// It reaches the viewer through the fake JPEG path
/// (`FakeDesktopApi.emitJpeg`): the decoder takes PNG as readily, so a
/// preview needs no native texture. No pointer is painted — the viewer draws
/// the remote cursor itself.
abstract final class RemoteScreenFixture {
  static const Size size = Size(1920, 1080);

  static Uint8List? _png;

  /// The encoded picture, painted once per test file.
  static Future<Uint8List> png(WidgetTester tester) async =>
      _png ??= (await tester.runAsync(() async {
        final recorder = ui.PictureRecorder();
        _paint(Canvas(recorder));
        final image = await recorder.endRecording().toImage(
          size.width.toInt(),
          size.height.toInt(),
        );
        final bytes = await image.toByteData(format: ui.ImageByteFormat.png);
        image.dispose();
        return bytes!.buffer.asUint8List();
      }))!;

  /// The artboard's container units: 1 % of the width and of the height.
  static const double _w = 19.2, _h = 10.8;

  static void _paint(Canvas c) {
    c.drawColor(const Color(0xFF2E4A63), BlendMode.src);
    c.drawCircle(
      const Offset(-18 * _w + 40 * _w, 34 * _h + 40 * _w),
      40 * _w,
      Paint()..color = const Color(0xFF35587A),
    );
    c.drawCircle(
      const Offset(120 * _w - 30 * _w, -40 * _h + 30 * _w),
      30 * _w,
      Paint()..color = const Color(0xFF29435B),
    );
    _icons(c);
    _document(c);
    _terminal(c);
    _taskbar(c);
  }

  static void _icons(Canvas c) {
    var top = 3 * _h;
    for (final (label, color) in const [
      ('This PC', Color(0xFF8FB8E0)),
      ('Recycle Bin', Color(0xFFC9D3DC)),
      ('Projects', Color(0xFFE8C46A)),
    ]) {
      const side = 2.8 * _w, centre = 1.6 * _w + 2.5 * _w;
      c.drawRRect(
        RRect.fromRectAndRadius(
          Rect.fromLTWH(centre - side / 2, top, side, side),
          const Radius.circular(0.5 * _w),
        ),
        Paint()..color = color,
      );
      _text(
        c,
        label,
        Offset(centre, top + side + 0.6 * _h),
        size: 0.85 * _w,
        color: const Color(0xFFFFFFFF),
        align: TextAlign.center,
      );
      top += side + 0.6 * _h + 1.3 * _w + 3 * _h;
    }
  }

  static void _document(Canvas c) {
    const window = Rect.fromLTWH(12 * _w, 6 * _h, 58 * _w, 78 * _h);
    _window(c, window, const Color(0xFFFFFFFF), 0.35);
    c.save();
    c.clipRRect(
      RRect.fromRectAndRadius(window, const Radius.circular(0.6 * _w)),
    );
    // Title bar and its caption glyphs.
    final title = Rect.fromLTWH(
      window.left,
      window.top,
      window.width,
      4.4 * _h,
    );
    c.drawRect(title, Paint()..color = const Color(0xFFF7F7F7));
    _text(
      c,
      'Q4 plan.docx — Document',
      Offset(title.left + _w, title.center.dy - 0.6 * _w),
      size: 0.95 * _w,
      color: const Color(0xFF1F1F1F),
    );
    final glyph = Paint()
      ..color = const Color(0xFF555555)
      ..style = PaintingStyle.stroke
      ..strokeWidth = 1.5;
    final y = title.center.dy;
    var x = title.right - _w - 0.5 * _w;
    c
      ..drawLine(Offset(x, y - 7), Offset(x + 0.5 * _w, y + 7), glyph)
      ..drawLine(Offset(x + 0.5 * _w, y - 7), Offset(x, y + 7), glyph);
    x -= 2.2 * _w;
    c.drawRect(Rect.fromLTWH(x, y - 6, 12, 12), glyph);
    x -= 2.2 * _w;
    c.drawLine(Offset(x, y), Offset(x + 12, y), glyph);
    // Ribbon.
    final ribbon = Rect.fromLTWH(
      window.left,
      title.bottom,
      window.width,
      4.2 * _h,
    );
    c.drawRect(ribbon, Paint()..color = const Color(0xFFF2F2F2));
    final hairline = Paint()..color = const Color(0xFFE5E5E5);
    c
      ..drawRect(
        Rect.fromLTWH(window.left, title.bottom - 1, window.width, 1),
        hairline,
      )
      ..drawRect(
        Rect.fromLTWH(window.left, ribbon.bottom - 1, window.width, 1),
        hairline,
      );
    var pill = ribbon.left + _w;
    for (final (width, color) in const [
      (4.0, Color(0xFFD9D9D9)),
      (3.0, Color(0xFFD9D9D9)),
      (1.6, Color(0xFFBDBDBD)),
      (1.6, Color(0xFFBDBDBD)),
      (1.6, Color(0xFFBDBDBD)),
      (6.0, Color(0xFFD9D9D9)),
    ]) {
      _bar(
        c,
        Rect.fromLTWH(pill, ribbon.center.dy - 0.8 * _h, width * _w, 1.6 * _h),
        color,
      );
      pill += (width + 0.8) * _w;
    }
    // The page on its grey desk.
    c.drawRect(
      Rect.fromLTRB(window.left, ribbon.bottom, window.right, window.bottom),
      Paint()..color = const Color(0xFFE9E9E9),
    );
    final page = Rect.fromLTWH(
      window.center.dx - window.width * 0.35,
      ribbon.bottom + 3 * _h,
      window.width * 0.7,
      window.bottom - ribbon.bottom,
    );
    _window(c, page, const Color(0xFFFFFFFF), 0.08, radius: 0);
    final left = page.left + 4 * _w, width = page.width - 8 * _w;
    var top = page.top + 4 * _h;
    const ink = Color(0xFF1F1F1F), line = Color(0xFFDADADA);
    top += _text(
      c,
      'Q4 plan',
      Offset(left, top),
      size: 1.7 * _w,
      color: ink,
      weight: FontWeight.w600,
    );
    for (final share in const [0.92, 0.86, 0.74]) {
      top += 1.6 * _h;
      _bar(c, Rect.fromLTWH(left, top, width * share, 1.1 * _h), line);
      top += 1.1 * _h;
    }
    top += 2.6 * _h;
    top += _text(
      c,
      'Milestones',
      Offset(left, top),
      size: 1.15 * _w,
      color: ink,
      weight: FontWeight.w600,
    );
    top += 1.6 * _h;
    const gap = 0.4 * _w, rowHeight = 0.85 * _w * 1.4 + 1.6 * _h;
    final cell = (width - 2 * gap) / 3;
    for (final (i, row) in const [
      ['Stage', 'Owner', 'Date'],
      ['Design review', 'Platform', 'Nov 18'],
      ['Device acceptance', 'Client', 'Dec 2'],
    ].indexed) {
      for (final (j, text) in row.indexed) {
        final box = Rect.fromLTWH(
          left + j * (cell + gap),
          top,
          cell,
          rowHeight,
        );
        if (i == 0) {
          c.drawRect(box, Paint()..color = const Color(0xFFEEF3F8));
        } else {
          c.drawRect(
            Rect.fromLTWH(box.left, box.bottom - 1, box.width, 1),
            Paint()..color = const Color(0xFFEEEEEE),
          );
        }
        _text(
          c,
          text,
          Offset(box.left + 0.6 * _w, box.top + 0.8 * _h),
          size: 0.85 * _w,
          color: ink,
        );
      }
      top += rowHeight + gap;
    }
    for (final share in const [0.88, 0.64]) {
      top += 1.6 * _h;
      _bar(c, Rect.fromLTWH(left, top, width * share, 1.1 * _h), line);
      top += 1.1 * _h;
    }
    c.restore();
  }

  static void _terminal(Canvas c) {
    const window = Rect.fromLTWH(56 * _w, 40 * _h, 36 * _w, 44 * _h);
    _window(c, window, const Color(0xFF1E1F22), 0.4);
    c.save();
    c.clipRRect(
      RRect.fromRectAndRadius(window, const Radius.circular(0.6 * _w)),
    );
    final title = Rect.fromLTWH(
      window.left,
      window.top,
      window.width,
      3.8 * _h,
    );
    c.drawRect(title, Paint()..color = const Color(0xFF2B2D31));
    _text(
      c,
      'Terminal',
      Offset(title.left + _w, title.center.dy - 0.6 * _w),
      size: 0.9 * _w,
      color: const Color(0xFFE6E6E6),
    );
    const text = Color(0xFFD4D4D4), size = 0.95 * _w, step = size * 1.7;
    final left = window.left + 1.2 * _w;
    var top = title.bottom + 1.6 * _h;
    const prompt = r'PS C:\work\pb-mapper> ';
    _text(
      c,
      '${prompt}cargo build --release',
      Offset(left, top),
      size: size,
      color: text,
      mono: true,
    );
    top += step;
    _text(
      c,
      '   Compiling pb-mapper-desktop v0.11.0',
      Offset(left, top),
      size: size,
      color: text,
      mono: true,
    );
    top += step;
    _text(
      c,
      '    Finished',
      Offset(left, top),
      size: size,
      color: const Color(0xFF6CCB8B),
      mono: true,
    );
    _text(
      c,
      '             release profile in 41.2s',
      Offset(left, top),
      size: size,
      color: text,
      mono: true,
    );
    top += step;
    final end = _text(
      c,
      prompt,
      Offset(left, top),
      size: size,
      color: text,
      mono: true,
      measureWidth: true,
    );
    c.drawRect(
      Rect.fromLTWH(left + end, top, size * 0.6, size * 1.25),
      Paint()..color = text,
    );
    c.restore();
  }

  static void _taskbar(Canvas c) {
    final bar = Rect.fromLTWH(0, size.height - 5.6 * _h, size.width, 5.6 * _h);
    c.drawRect(bar, Paint()..color = const Color(0xE616181C));
    const side = 3.2 * _h, gap = 0.9 * _w;
    const colors = [
      Color(0xFF4D8FD6),
      Color(0xFF6E7781),
      Color(0xFFE8C46A),
      Color(0xFF5B9BD5),
      Color(0xFF3C3F44),
    ];
    var x =
        bar.center.dx - (colors.length * side + (colors.length - 1) * gap) / 2;
    for (final (i, color) in colors.indexed) {
      final icon = Rect.fromLTWH(x, bar.center.dy - side / 2, side, side);
      c.drawRRect(
        RRect.fromRectAndRadius(icon, const Radius.circular(0.4 * _w)),
        Paint()..color = color,
      );
      // The running apps: the document and the terminal.
      if (i >= 3) {
        _bar(
          c,
          Rect.fromLTWH(icon.center.dx - 6, icon.bottom + 3, 12, 3),
          const Color(0xFFFFFFFF),
        );
      }
      x += side + gap;
    }
    final right = bar.right - 1.2 * _w;
    const clock = Color(0xFFFFFFFF), small = 0.85 * _w;
    _text(
      c,
      '14:32',
      Offset(right, bar.top + 0.8 * _h),
      size: small,
      color: clock,
      align: TextAlign.right,
    );
    _text(
      c,
      '2026/11/7',
      Offset(right, bar.top + 0.8 * _h + small * 1.3),
      size: small,
      color: clock,
      align: TextAlign.right,
    );
  }

  /// A window's body and its soft drop shadow.
  static void _window(
    Canvas c,
    Rect rect,
    Color color,
    double shadow, {
    double radius = 0.6 * _w,
  }) {
    final shape = RRect.fromRectAndRadius(rect, Radius.circular(radius));
    c
      ..drawRRect(
        shape.shift(const Offset(0, 1 * _h)),
        Paint()
          ..color = Color.fromRGBO(0, 0, 0, shadow)
          ..maskFilter = const MaskFilter.blur(BlurStyle.normal, 1.5 * _h),
      )
      ..drawRRect(shape, Paint()..color = color);
  }

  static void _bar(Canvas c, Rect rect, Color color) => c.drawRRect(
    RRect.fromRectAndRadius(rect, const Radius.circular(0.3 * _w)),
    Paint()..color = color,
  );

  /// Lays [text] out at [at] (its top, at the [align] edge) and returns its
  /// height, or its width when [measureWidth].
  static double _text(
    Canvas c,
    String text,
    Offset at, {
    required double size,
    required Color color,
    FontWeight weight = FontWeight.w400,
    TextAlign align = TextAlign.left,
    bool mono = false,
    bool measureWidth = false,
  }) {
    final painter = TextPainter(
      text: TextSpan(
        text: text,
        style: TextStyle(
          fontFamily: mono ? 'GeistMono' : 'GoogleSansFlex',
          fontSize: size,
          fontWeight: weight,
          color: color,
        ),
      ),
      textDirection: TextDirection.ltr,
    )..layout();
    final dx = switch (align) {
      TextAlign.center => at.dx - painter.width / 2,
      TextAlign.right => at.dx - painter.width,
      _ => at.dx,
    };
    painter.paint(c, Offset(dx, at.dy));
    final measured = measureWidth ? painter.width : painter.height;
    painter.dispose();
    return measured;
  }
}
