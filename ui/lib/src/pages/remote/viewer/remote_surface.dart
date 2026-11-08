import 'dart:async';
import 'dart:math' as math;
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import '../../../state/desktop_input.dart';
import '../../../state/viewer_prefs.dart';
import '../../../theme/motion.dart';
import '../../../theme/viewer_palette.dart';
import 'remote_input.dart';
import 'remote_viewport.dart';
import 'touch_gestures.dart';

/// Scroll distance per wheel notch; fractions accumulate.
const kWheelPxPerNotch = 30.0;

/// How long the touch indicator takes to fade after the last finger.
const kTouchIndicatorFade = Duration(milliseconds: 150);

/// The remote picture and every pointer over it (ui §10.2-10.3).
///
/// One [Listener] takes all pointers: touch and stylus go to the
/// [TouchGestureMachine], the sole owner of touch; a mouse and a touchpad
/// are handled here. The picture is [child], laid out at the viewport's
/// scene and moved by one [Transform]: zoom, pan, the soft keyboard and
/// reconnects change the transform only, never the child's element, so a
/// native texture is never re-created (P1-4).
class RemoteSurface extends StatefulWidget {
  const RemoteSurface({
    super.key,
    required this.viewport,
    required this.input,
    required this.machine,
    required this.touchMode,
    required this.child,
  });

  final RemoteViewport viewport;
  final RemoteInput input;
  final TouchGestureMachine machine;
  final ValueListenable<TouchMode> touchMode;
  final Widget child;

  @override
  State<RemoteSurface> createState() => _RemoteSurfaceState();
}

class _RemoteSurfaceState extends State<RemoteSurface> {
  /// Fires the machine's next deadline.
  Timer? _timer;

  /// The mouse buttons this surface pressed on the host, per pointer.
  final _buttons = <int, Set<RemoteButton>>{};

  double _wheelX = 0, _wheelY = 0;
  double _panZoomScale = 1;

  RemoteViewport get _viewport => widget.viewport;
  RemoteInput get _input => widget.input;
  TouchGestureMachine get _machine => widget.machine;

  @override
  void initState() {
    super.initState();
    widget.touchMode.addListener(_modeChanged);
  }

  @override
  void didUpdateWidget(covariant RemoteSurface oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.touchMode != widget.touchMode) {
      oldWidget.touchMode.removeListener(_modeChanged);
      widget.touchMode.addListener(_modeChanged);
    }
    if (oldWidget.machine != widget.machine) _timer?.cancel();
  }

  @override
  void dispose() {
    widget.touchMode.removeListener(_modeChanged);
    _timer?.cancel();
    super.dispose();
  }

  /// Switching between trackpad and direct touch ends the gesture, but
  /// keeps the latches (§10.8).
  void _modeChanged() {
    _machine.cancel(_input.clock());
    _arm();
  }

  void _arm() {
    _timer?.cancel();
    _timer = null;
    final due = _machine.nextTimeout;
    if (due == null) return;
    final wait = due - _input.clock();
    _timer = Timer(wait < Duration.zero ? Duration.zero : wait, () {
      _timer = null;
      final now = _input.clock();
      // The timer fired because the deadline is due.
      _machine.timeout(now < due ? due : now);
      _arm();
    });
  }

  static bool _isTouch(PointerEvent e) =>
      e.kind == ui.PointerDeviceKind.touch ||
      e.kind == ui.PointerDeviceKind.stylus ||
      e.kind == ui.PointerDeviceKind.invertedStylus;

  void _focus() =>
      Focus.maybeOf(context, createDependency: false)?.requestFocus();

  // ---------------------------------------------------------------- touch

  void _down(PointerDownEvent e) {
    _focus();
    if (_isTouch(e)) {
      _machine.down(e.pointer, e.localPosition, _input.clock());
      _arm();
      return;
    }
    final n = _viewport.toNormalized(e.localPosition);
    if (n == null || !_input.ready.value) return;
    _syncButtons(e.pointer, e.buttons, n);
  }

  void _move(PointerMoveEvent e) {
    if (_isTouch(e)) {
      _machine.move(e.pointer, e.localPosition, _input.clock());
      _arm();
      return;
    }
    final held = _buttons[e.pointer];
    if (held == null || !_input.ready.value) return;
    _viewport.autoScrollTo(e.localPosition);
    final n = _viewport.toNormalized(e.localPosition, clamp: true);
    if (n == null) return;
    _input.move(n);
    _syncButtons(e.pointer, e.buttons, n);
  }

  void _up(PointerUpEvent e) {
    if (_isTouch(e)) {
      _machine.up(e.pointer, e.localPosition, _input.clock());
      _arm();
      return;
    }
    for (final b in _buttons.remove(e.pointer) ?? const <RemoteButton>{}) {
      _input.release(b);
    }
  }

  void _cancel(PointerCancelEvent e) {
    if (_isTouch(e)) {
      _machine.cancel(_input.clock());
      _arm();
      return;
    }
    for (final b in _buttons.remove(e.pointer) ?? const <RemoteButton>{}) {
      _input.release(b);
    }
  }

  // ---------------------------------------------------------------- mouse

  /// Presses and releases what changed in [buttons]: a second button
  /// arrives as a move, not a down.
  void _syncButtons(int pointer, int buttons, Offset n) {
    final held = _buttons.putIfAbsent(pointer, () => {});
    for (final (mask, button) in const [
      (kPrimaryMouseButton, RemoteButton.left),
      (kSecondaryMouseButton, RemoteButton.right),
      (kMiddleMouseButton, RemoteButton.middle),
    ]) {
      final down = buttons & mask != 0;
      if (down && held.add(button)) {
        _input.press(button, n);
      } else if (!down && held.remove(button)) {
        _input.release(button);
      }
    }
  }

  /// In original-size mode the view follows the pointer first, so the
  /// remote pointer goes to what is under the local one afterwards.
  void _hover(PointerHoverEvent e) {
    if (_isTouch(e)) return;
    _viewport.autoScrollTo(e.localPosition);
    final n = _viewport.toNormalized(e.localPosition);
    if (n != null && _input.ready.value) _input.hover(n);
  }

  void _signal(PointerSignalEvent e) {
    if (e is! PointerScrollEvent) return;
    final dy = e.scrollDelta.dy, dx = e.scrollDelta.dx;
    if (HardwareKeyboard.instance.isControlPressed) {
      // Zoom stays local (U20).
      _viewport.zoomAt(e.localPosition, math.exp(-dy / 250));
      return;
    }
    final n = _viewport.toNormalized(e.localPosition);
    if (n == null || !_input.ready.value) return;
    _input.hover(n);
    _wheelBy(dx, dy);
  }

  void _panZoomStart(PointerPanZoomStartEvent e) => _panZoomScale = 1;

  void _panZoomUpdate(PointerPanZoomUpdateEvent e) {
    if (e.scale != 1) {
      _viewport.zoomAt(e.localPosition, e.scale / _panZoomScale);
      _panZoomScale = e.scale;
      return;
    }
    if (_input.ready.value) {
      // Natural scrolling: the content follows the fingers (U10).
      _wheelBy(-e.panDelta.dx, -e.panDelta.dy);
    } else {
      _viewport.panBy(e.panDelta);
    }
  }

  /// Whole notches go to the host; fractions wait for the next event.
  void _wheelBy(double dx, double dy) {
    _wheelX += dx / kWheelPxPerNotch;
    _wheelY += dy / kWheelPxPerNotch;
    final nx = _wheelX.truncate(), ny = _wheelY.truncate();
    if (nx == 0 && ny == 0) return;
    _wheelX -= nx;
    _wheelY -= ny;
    _input.wheel(nx, ny);
  }

  @override
  Widget build(BuildContext context) => LayoutBuilder(
    builder: (context, constraints) {
      _viewport.layout(constraints.biggest);
      return Listener(
        behavior: HitTestBehavior.opaque,
        onPointerDown: _down,
        onPointerMove: _move,
        onPointerUp: _up,
        onPointerCancel: _cancel,
        onPointerHover: _hover,
        onPointerSignal: _signal,
        onPointerPanZoomStart: _panZoomStart,
        onPointerPanZoomUpdate: _panZoomUpdate,
        child: ClipRect(
          child: ListenableBuilder(
            listenable: _viewport,
            builder: (context, picture) {
              final scene = _viewport.scene;
              final offset = _viewport.offset;
              final scale = _viewport.scale;
              return Stack(
                fit: StackFit.expand,
                children: [
                  Positioned(
                    left: 0,
                    top: 0,
                    width: scene.width,
                    height: scene.height,
                    child: Transform(
                      key: const Key('remote-scene-transform'),
                      transform: Matrix4.diagonal3Values(scale, scale, 1)
                        ..setTranslationRaw(offset.dx, offset.dy, 0),
                      child: picture,
                    ),
                  ),
                ],
              );
            },
            child: RepaintBoundary(child: widget.child),
          ),
        ),
      );
    },
  );
}

/// Where the finger is in trackpad mode, a 44 px ring, and a long press's
/// joystick in either mode; each fades when the last finger lifts. Its own
/// layer, like the cursor's: it moves with every stroke and repaints nothing
/// around it. Both carry a light and a dark edge, so they show over a white
/// page, a black one and anything between.
class TouchIndicator extends StatelessWidget {
  const TouchIndicator({super.key, required this.machine});

  final TouchGestureMachine machine;

  @override
  Widget build(BuildContext context) => RepaintBoundary(
    child: IgnorePointer(
      child: Stack(
        fit: StackFit.expand,
        children: [
          _Fading(
            value: machine.indicatorPoint,
            painter: (at) => _FingerPainter(at),
          ),
          _Fading(
            value: machine.joystick,
            painter: (stick) => _JoystickPainter(stick),
          ),
        ],
      ),
    ),
  );
}

/// Keeps the last non-null value, so what it draws fades where it was.
class _Fading<T extends Object> extends StatefulWidget {
  const _Fading({required this.value, required this.painter});

  final ValueListenable<T?> value;
  final CustomPainter Function(T value) painter;

  @override
  State<_Fading<T>> createState() => _FadingState<T>();
}

class _FadingState<T extends Object> extends State<_Fading<T>> {
  T? _last;

  @override
  Widget build(BuildContext context) => ValueListenableBuilder(
    valueListenable: widget.value,
    builder: (context, value, _) {
      if (value != null) _last = value;
      final last = _last;
      if (last == null) return const SizedBox.shrink();
      return AnimatedOpacity(
        opacity: value != null ? 1 : 0,
        duration: value != null
            ? Duration.zero
            : Motion.of(context, kTouchIndicatorFade),
        child: CustomPaint(painter: widget.painter(last)),
      );
    },
  );
}

const _light = Color(0xFFFFFFFF);
const _dark = Color(0xFF000000);

/// A soft shadow under a circle.
void _shadow(Canvas canvas, Offset at, double radius, double sigma) =>
    canvas.drawCircle(
      at,
      radius,
      Paint()
        ..color = _dark.withValues(alpha: 0.32)
        ..maskFilter = MaskFilter.blur(BlurStyle.normal, sigma),
    );

void _edge(Canvas canvas, Offset at, double radius, Color color, double w) =>
    canvas.drawCircle(
      at,
      radius,
      Paint()
        ..style = PaintingStyle.stroke
        ..strokeWidth = w
        ..color = color,
    );

/// The trackpad finger: a translucent 44 px ring over the cursor, its white
/// edge ringed in black.
class _FingerPainter extends CustomPainter {
  const _FingerPainter(this.at);

  final Offset at;

  static const _radius = 22.0;

  @override
  void paint(Canvas canvas, Size size) {
    canvas.drawCircle(
      at,
      _radius,
      Paint()..color = _light.withValues(alpha: 0.22),
    );
    _edge(canvas, at, _radius - 0.75, _light.withValues(alpha: 0.85), 1.5);
    _edge(canvas, at, _radius + 0.5, _dark.withValues(alpha: 0.45), 1);
  }

  @override
  bool shouldRepaint(_FingerPainter old) => old.at != at;
}

/// The joystick: a dark translucent ring with a light edge and four
/// chevrons at the press point, and a light ball with a dark edge under
/// the finger. The edge takes the signal blue while it scrolls.
class _JoystickPainter extends CustomPainter {
  const _JoystickPainter(this.stick);

  final TouchJoystick stick;

  static const _ball = 22.0;

  @override
  void paint(Canvas canvas, Size size) {
    final c = stick.centre;
    const r = kJoystickRadius;
    _shadow(canvas, c, r, 8);
    canvas.drawCircle(c, r, Paint()..color = _dark.withValues(alpha: 0.38));
    _edge(
      canvas,
      c,
      r - 0.75,
      stick.active ? ViewerPalette.signal : _light.withValues(alpha: 0.75),
      1.5,
    );
    final chevron = Paint()
      ..style = PaintingStyle.stroke
      ..strokeWidth = 2
      ..strokeCap = StrokeCap.round
      ..strokeJoin = StrokeJoin.round
      ..color = _light.withValues(alpha: 0.7);
    for (var quarter = 0; quarter < 4; quarter++) {
      final a = quarter * math.pi / 2;
      final dir = Offset(math.cos(a), math.sin(a));
      final side = Offset(-dir.dy, dir.dx);
      final tip = c + dir * (r - 9);
      final left = tip - dir * 5 + side * 5, right = tip - dir * 5 - side * 5;
      canvas.drawPath(
        Path()
          ..moveTo(left.dx, left.dy)
          ..lineTo(tip.dx, tip.dy)
          ..lineTo(right.dx, right.dy),
        chevron,
      );
    }
    final b = stick.ball;
    _shadow(canvas, b, _ball, 4);
    canvas.drawCircle(
      b,
      _ball,
      Paint()..color = _light.withValues(alpha: 0.92),
    );
    _edge(canvas, b, _ball - 0.75, _dark.withValues(alpha: 0.5), 1.5);
  }

  @override
  bool shouldRepaint(_JoystickPainter old) => old.stick != stick;
}
