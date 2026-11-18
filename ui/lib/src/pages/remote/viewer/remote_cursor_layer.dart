import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';

import '../../../state/desktop_store.dart';
import 'remote_input.dart';
import 'remote_viewport.dart';

/// What is painted at the host's pointer.
enum RemotePointerPaint {
  /// Nothing: the local pointer is there, in the host's shape.
  none,

  /// The generic arrow, as for a host that sends no shape.
  arrow,

  /// The host's shape, its bitmap.
  shape,
}

/// How the host's pointer shows (D11, D12): the local OS pointer over the
/// picture, and what the cursor layer paints where the host's pointer is.
///
/// A host that sends no shape (before 0.12) keeps the generic arrow, painted,
/// and the local pointer as it is. A touch viewer paints the host's shape.
/// A desktop viewer in control gives the local pointer the host's shape
/// while the host's pointer is under it, so one pointer shows, not two: a
/// standard shape as the system's own pointer, any other as its bitmap
/// painted under a hidden pointer. Viewing only, or once the host's pointer
/// is elsewhere (someone there moved it, or the local pointer left the
/// picture), the host's shape is painted where the host's pointer is.
@immutable
class RemotePointerLook {
  const RemotePointerLook(this.local, this.paint);

  /// What a host that sends no shape gets: today's arrow.
  static const generic = RemotePointerLook(
    MouseCursor.defer,
    RemotePointerPaint.arrow,
  );

  /// [mouse]: a desktop viewer, whose local pointer hovers the picture.
  /// [atLocalPointer]: the host's pointer is where the local one is.
  /// [kind]: the host's shape, null from a host that sends none.
  factory RemotePointerLook.of({
    required bool mouse,
    required bool controlling,
    required bool atLocalPointer,
    required DesktopCursorKind? kind,
  }) {
    if (kind == null) return generic;
    if (!mouse || !controlling) {
      return const RemotePointerLook(
        MouseCursor.defer,
        RemotePointerPaint.shape,
      );
    }
    final system = systemCursorOf(kind);
    if (!atLocalPointer) {
      return RemotePointerLook(
        system ?? SystemMouseCursors.basic,
        RemotePointerPaint.shape,
      );
    }
    return system == null
        ? const RemotePointerLook(
            SystemMouseCursors.none,
            RemotePointerPaint.shape,
          )
        : RemotePointerLook(system, RemotePointerPaint.none);
  }

  /// The local pointer over the picture.
  final MouseCursor local;
  final RemotePointerPaint paint;

  @override
  bool operator ==(Object other) =>
      other is RemotePointerLook &&
      other.local == local &&
      other.paint == paint;

  @override
  int get hashCode => Object.hash(local, paint);
}

/// The system pointer of [kind]; null where the viewer's OS has none, so
/// that only its bitmap shows it.
MouseCursor? systemCursorOf(DesktopCursorKind kind) => switch (kind) {
  DesktopCursorKind.arrow => SystemMouseCursors.basic,
  DesktopCursorKind.ibeam => SystemMouseCursors.text,
  DesktopCursorKind.hand => SystemMouseCursors.click,
  DesktopCursorKind.wait => SystemMouseCursors.wait,
  DesktopCursorKind.progress => SystemMouseCursors.progress,
  DesktopCursorKind.crosshair => SystemMouseCursors.precise,
  DesktopCursorKind.help => SystemMouseCursors.help,
  DesktopCursorKind.notAllowed => SystemMouseCursors.forbidden,
  DesktopCursorKind.move => SystemMouseCursors.move,
  DesktopCursorKind.resizeNs => SystemMouseCursors.resizeUpDown,
  DesktopCursorKind.resizeEw => SystemMouseCursors.resizeLeftRight,
  DesktopCursorKind.resizeNwse => SystemMouseCursors.resizeUpLeftDownRight,
  DesktopCursorKind.resizeNesw => SystemMouseCursors.resizeUpRightDownLeft,
  DesktopCursorKind.upArrow || DesktopCursorKind.custom => null,
};

/// The host's pointer as one viewer shows it: its [RemotePointerLook],
/// kept current as control, the host's shape and the local pointer's hover
/// change, and what the painter needs of the picture's scale.
final class RemotePointer extends ValueNotifier<RemotePointerLook> {
  RemotePointer({
    required this.mouse,
    required this.cursor,
    required this.controlling,
    required this.geometry,
  }) : super(RemotePointerLook.generic) {
    cursor.addListener(_update);
    controlling.addListener(_update);
    _update();
  }

  /// A desktop viewer: see [RemotePointerLook.of].
  final bool mouse;
  final DesktopCursor cursor;

  /// Control is live: the local pointer moves the host's.
  final ValueListenable<bool> controlling;
  final ValueListenable<DesktopGeometry?> geometry;

  bool _over = false;

  /// The local pointer hovers the picture.
  set over(bool over) {
    if (over == _over) return;
    _over = over;
    _update();
  }

  void _update() {
    value = RemotePointerLook.of(
      mouse: mouse,
      controlling: controlling.value,
      atLocalPointer: _over && cursor.followsLocal,
      kind: cursor.shape?.definition.kind,
    );
  }

  @override
  void dispose() {
    cursor.removeListener(_update);
    controlling.removeListener(_update);
    super.dispose();
  }
}

/// The picture under the local pointer: it takes the host's shape there
/// ([RemotePointerLook.local]).
class RemotePointerRegion extends StatelessWidget {
  const RemotePointerRegion({
    super.key,
    required this.pointer,
    required this.child,
  });

  final RemotePointer pointer;
  final Widget child;

  @override
  Widget build(BuildContext context) =>
      ValueListenableBuilder<RemotePointerLook>(
        valueListenable: pointer,
        child: child,
        builder: (context, look, child) => MouseRegion(
          cursor: look.local,
          opaque: false,
          onEnter: (_) => pointer.over = true,
          onExit: (_) => pointer.over = false,
          child: child,
        ),
      );
}

/// The remote cursor over the picture (D11, D12): this viewer's own, which
/// follows the host's reports only as [RemoteInput.shownCursor] says, so it
/// never jumps back to where a lagging report left it, drawn as [pointer]
/// says; without one, the generic arrow.
///
/// Its own layer: the arrow moves with every stroke, hover and report, and
/// repaints nothing around it.
class RemoteCursorLayer extends StatelessWidget {
  const RemoteCursorLayer({
    super.key,
    required this.viewport,
    required this.input,
    this.pointer,
  });

  final RemoteViewport viewport;
  final RemoteInput input;
  final RemotePointer? pointer;

  /// Where to draw, in the stage's coordinates, and what.
  RemoteCursorPaint? _paint() {
    final n = input.shownCursor;
    if (n == null) return null;
    final at = viewport.toLocal(n);
    final pointer = this.pointer;
    if (pointer == null) return (at: at, shape: null, scale: 1);
    final shape = pointer.cursor.shape;
    final image = shape?.image;
    switch (pointer.value.paint) {
      case RemotePointerPaint.none:
        return null;
      case RemotePointerPaint.shape when shape != null && image != null:
        return (at: at, shape: shape, scale: _scale(pointer, shape));
      case RemotePointerPaint.arrow || RemotePointerPaint.shape:
        return (at: at, shape: null, scale: 1);
    }
  }

  /// The host's pixels as the picture shows them now, zoom included, but
  /// never so small that the shape is under [kMinPointerSide].
  double _scale(RemotePointer pointer, DesktopPointerShape shape) {
    final display = pointer.geometry.value?.displayWidth ?? 0;
    final shown =
        viewport.toLocal(const Offset(1, 0)).dx -
        viewport.toLocal(Offset.zero).dx;
    final picture = display > 0 && shown > 0 ? shown / display : 1.0;
    final d = shape.definition;
    return math.max(picture, kMinPointerSide / math.max(d.width, d.height));
  }

  @override
  Widget build(BuildContext context) => RepaintBoundary(
    child: IgnorePointer(
      child: ClipRect(
        child: CustomPaint(
          size: Size.infinite,
          painter: RemoteCursorPainter(
            cursor: _paint,
            repaint: Listenable.merge([
              viewport,
              input.cursorChanges,
              ?pointer,
            ]),
          ),
        ),
      ),
    ),
  );
}

/// The smallest a painted shape's longer side gets, in logical pixels: a
/// phone shows a whole desktop small, and its pointer must stay legible.
const kMinPointerSide = 32.0;

/// Where the cursor is drawn, and as what: [shape]'s bitmap at [scale],
/// its hotspot at [at], or with no shape the generic arrow.
typedef RemoteCursorPaint = ({
  Offset at,
  DesktopPointerShape? shape,
  double scale,
});

/// The host's shape, or an arrow 18×27 whatever the zoom: black with a
/// white edge, its tip at the point.
class RemoteCursorPainter extends CustomPainter {
  RemoteCursorPainter({required this.cursor, required Listenable repaint})
    : super(repaint: repaint);

  final RemoteCursorPaint? Function() cursor;

  static const size = Size(18, 27);

  static final _arrow = Path()
    ..moveTo(1, 1)
    ..lineTo(1, 20)
    ..lineTo(6, 16)
    ..lineTo(10, 25)
    ..lineTo(14, 23)
    ..lineTo(10, 15)
    ..lineTo(17, 15)
    ..close();

  static final _fill = Paint()..color = const Color(0xFF000000);
  static final _edge = Paint()
    ..color = const Color(0xFFFFFFFF)
    ..style = PaintingStyle.stroke
    ..strokeWidth = 1.5;
  static final _bitmap = Paint()..filterQuality = FilterQuality.medium;

  @override
  void paint(Canvas canvas, Size _) {
    final what = cursor();
    if (what == null) return;
    final shape = what.shape, image = shape?.image;
    if (shape != null && image != null) {
      final d = shape.definition;
      final topLeft =
          what.at - Offset(d.hotX.toDouble(), d.hotY.toDouble()) * what.scale;
      canvas.drawImageRect(
        image,
        Rect.fromLTWH(0, 0, d.width.toDouble(), d.height.toDouble()),
        topLeft & Size(d.width * what.scale, d.height * what.scale),
        _bitmap,
      );
      return;
    }
    // The arrow's tip is at (1, 1).
    final path = _arrow.shift(what.at - const Offset(1, 1));
    canvas
      ..drawPath(path, _fill)
      ..drawPath(path, _edge);
  }

  @override
  bool shouldRepaint(covariant RemoteCursorPainter oldDelegate) =>
      oldDelegate.cursor != cursor;
}
