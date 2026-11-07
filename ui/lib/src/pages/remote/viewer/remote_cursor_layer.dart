import 'package:flutter/foundation.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/widgets.dart';

import '../../../state/desktop_store.dart';
import 'remote_input.dart';
import 'remote_viewport.dart';

/// The remote cursor over the picture (D11): the host's latest cursor,
/// read from the view leaf after every rendered frame, or the local
/// prediction while this viewer moves it, since the host does not echo a
/// mover's cursor back for [kCursorEchoWindow]. The local OS pointer stays
/// visible on desktop.
class RemoteCursorLayer extends StatefulWidget {
  const RemoteCursorLayer({
    super.key,
    required this.readView,
    required this.geometry,
    required this.viewport,
    required this.input,
  });

  final DesktopView? Function() readView;
  final ValueListenable<DesktopGeometry?> geometry;
  final RemoteViewport viewport;
  final RemoteInput input;

  @override
  State<RemoteCursorLayer> createState() => _RemoteCursorLayerState();
}

class _RemoteCursorLayerState extends State<RemoteCursorLayer> {
  final _repaint = _Repaint();

  /// The host's cursor, normalised; null while hidden or unknown.
  Offset? _remote;
  DesktopView? _sample;

  @override
  void initState() {
    super.initState();
    SchedulerBinding.instance.addPostFrameCallback(_onFrame);
  }

  @override
  void dispose() {
    _repaint.dispose();
    super.dispose();
  }

  /// Rides frames rendered anyway: a post-frame callback never schedules
  /// one. When the cursor moves while nothing else renders, the store's
  /// view timer asks for a frame.
  void _onFrame(Duration _) {
    if (!mounted) return;
    _accept(widget.readView(), widget.geometry.value);
    SchedulerBinding.instance.addPostFrameCallback(_onFrame);
  }

  void _accept(DesktopView? view, DesktopGeometry? g) {
    if (view == null || g == null) {
      if (_sample != null || _remote != null) {
        _sample = null;
        _remote = null;
        _repaint.notify();
      }
      return;
    }
    // A sample from another generation belongs to another display size.
    if (view.generation != g.generation) return;
    final last = _sample;
    if (last != null &&
        last.cursorSeq == view.cursorSeq &&
        last.cursorX == view.cursorX &&
        last.cursorY == view.cursorY &&
        last.cursorVisible == view.cursorVisible) {
      return;
    }
    _sample = view;
    _remote = !view.cursorVisible || g.displayWidth <= 0 || g.displayHeight <= 0
        ? null
        : Offset(
            (view.cursorX / g.displayWidth).clamp(0.0, 1.0),
            (view.cursorY / g.displayHeight).clamp(0.0, 1.0),
          );
    _repaint.notify();
  }

  /// Where to draw, in the stage's coordinates.
  Offset? _at() {
    final input = widget.input;
    final n = input.predicting ? input.cursor : _remote;
    return n == null ? null : widget.viewport.toLocal(n);
  }

  @override
  Widget build(BuildContext context) => IgnorePointer(
    child: ClipRect(
      child: CustomPaint(
        size: Size.infinite,
        painter: RemoteCursorPainter(
          at: _at,
          repaint: Listenable.merge([
            _repaint,
            widget.viewport,
            widget.input.cursorChanges,
          ]),
        ),
      ),
    ),
  );
}

/// An arrow, 18×27 whatever the zoom: black with a white edge, its tip at
/// the point.
class RemoteCursorPainter extends CustomPainter {
  RemoteCursorPainter({required this.at, required Listenable repaint})
    : super(repaint: repaint);

  final Offset? Function() at;

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

  @override
  void paint(Canvas canvas, Size _) {
    final point = at();
    if (point == null) return;
    // The arrow's tip is at (1, 1).
    final path = _arrow.shift(point - const Offset(1, 1));
    canvas
      ..drawPath(path, _fill)
      ..drawPath(path, _edge);
  }

  @override
  bool shouldRepaint(covariant RemoteCursorPainter oldDelegate) =>
      oldDelegate.at != at;
}

class _Repaint extends ChangeNotifier {
  void notify() => notifyListeners();
}
