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

/// Where the finger is in trackpad mode: a 44 px ring that fades when the
/// last finger lifts.
class TouchIndicator extends StatelessWidget {
  const TouchIndicator({super.key, required this.machine});

  final TouchGestureMachine machine;

  static const _size = 44.0;

  @override
  Widget build(BuildContext context) => IgnorePointer(
    child: _LastPoint(
      point: machine.indicatorPoint,
      builder: (context, at, visible) => Stack(
        children: [
          Positioned(
            left: at.dx - _size / 2,
            top: at.dy - _size / 2,
            width: _size,
            height: _size,
            child: AnimatedOpacity(
              opacity: visible ? 1 : 0,
              duration: visible
                  ? Duration.zero
                  : Motion.of(context, kTouchIndicatorFade),
              child: const DecoratedBox(
                decoration: BoxDecoration(
                  shape: BoxShape.circle,
                  color: Color.fromRGBO(255, 255, 255, 0.22),
                  border: Border.fromBorderSide(
                    BorderSide(
                      color: Color.fromRGBO(255, 255, 255, 0.8),
                      width: 1.5,
                    ),
                  ),
                ),
              ),
            ),
          ),
        ],
      ),
    ),
  );
}

/// Keeps the last non-null point, so the indicator fades where it was.
class _LastPoint extends StatefulWidget {
  const _LastPoint({required this.point, required this.builder});

  final ValueListenable<Offset?> point;
  final Widget Function(BuildContext context, Offset at, bool visible) builder;

  @override
  State<_LastPoint> createState() => _LastPointState();
}

class _LastPointState extends State<_LastPoint> {
  Offset? _last;

  @override
  Widget build(BuildContext context) => ValueListenableBuilder(
    valueListenable: widget.point,
    builder: (context, point, _) {
      if (point != null) _last = point;
      final at = _last;
      if (at == null) return const SizedBox.shrink();
      return widget.builder(context, at, point != null);
    },
  );
}
