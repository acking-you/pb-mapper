import 'dart:async';
import 'dart:math' as math;
import 'dart:ui' as ui;

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

/// One surface owns local navigation and remote pointer input. The native
/// texture remains the same child while only its transform changes.
class DesktopGestureSurface extends StatefulWidget {
  const DesktopGestureSurface({
    super.key,
    required this.aspectRatio,
    required this.enabled,
    required this.trackpad,
    required this.inputEpoch,
    required this.onMove,
    required this.onButton,
    required this.onWheel,
    required this.onRelease,
    required this.onFocus,
    required this.child,
    this.cursor,
  });

  final double aspectRatio;
  final bool enabled, trackpad;
  final Object inputEpoch;
  final Offset? cursor;
  final Future<void> Function(Offset normalized) onMove;
  final Future<void> Function(String button, bool down) onButton;
  final Future<void> Function(Offset delta) onWheel;
  final Future<void> Function() onRelease;
  final VoidCallback onFocus;
  final Widget child;

  @override
  State<DesktopGestureSurface> createState() => DesktopGestureSurfaceState();
}

class DesktopGestureSurfaceState extends State<DesktopGestureSurface> {
  final _touches = <int, (Offset, Offset)>{};
  final _mouseButtons = <int, String>{};
  Size _viewport = Size.zero, _scene = Size.zero;
  Offset _offset = Offset.zero, _scaleAnchor = Offset.zero;
  Offset? _remoteCursor;
  final _cursorPaint = ValueNotifier<Offset?>(null);
  DateTime? _lastLocalMove;
  double _scale = 1, _startScale = 1, _gestureScale = 1;
  int? _scalePointers;
  int _maxTouches = 0;
  Duration _touchStarted = Duration.zero;
  Offset _tapPoint = Offset.zero;
  bool _moved = false, _touchEligible = false, _dragging = false;
  Timer? _hold;

  @override
  void initState() {
    super.initState();
    _cursorPaint.value = widget.cursor;
  }

  @override
  void didUpdateWidget(covariant DesktopGestureSurface oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.enabled != widget.enabled ||
        oldWidget.trackpad != widget.trackpad ||
        oldWidget.inputEpoch != widget.inputEpoch) {
      cancelInput();
      _remoteCursor = null;
    }
    updateRemoteCursor(widget.cursor);
  }

  void updateRemoteCursor(Offset? cursor) {
    if (_touches.isEmpty &&
        (_lastLocalMove == null ||
            DateTime.now().difference(_lastLocalMove!) >
                const Duration(milliseconds: 500))) {
      _cursorPaint.value = cursor;
      _remoteCursor = cursor;
    }
  }

  /// Focus/session changes release actual held input, not harmless pinch taps.
  /// Navigation survives reconnects; only a new viewer starts at fit-to-screen.
  void cancelInput() {
    _hold?.cancel();
    _hold = null;
    final held = _dragging || _mouseButtons.isNotEmpty;
    _dragging = false;
    _mouseButtons.clear();
    _touches.clear();
    _maxTouches = 0;
    _scalePointers = null;
    if (held) unawaited(widget.onRelease());
  }

  @override
  void dispose() {
    cancelInput();
    _cursorPaint.dispose();
    super.dispose();
  }

  Offset _clampCursor(Offset p) =>
      Offset(p.dx.clamp(0.0, 1.0), p.dy.clamp(0.0, 1.0));

  Offset? _point(Offset local, {bool clamp = false}) {
    if (_scene.isEmpty) return null;
    final point = Offset(
      (local.dx - _offset.dx) / (_scene.width * _scale),
      (local.dy - _offset.dy) / (_scene.height * _scale),
    );
    if (!clamp &&
        (point.dx < 0 || point.dy < 0 || point.dx > 1 || point.dy > 1)) {
      return null;
    }
    return _clampCursor(point);
  }

  void _move(Offset point) {
    if (!widget.enabled) return;
    _remoteCursor = _clampCursor(point);
    _lastLocalMove = DateTime.now();
    _cursorPaint.value = _remoteCursor;
    unawaited(widget.onMove(_remoteCursor!));
  }

  void _click(String button, Offset local) {
    if (!widget.enabled) return;
    final point = widget.trackpad
        ? _remoteCursor ?? widget.cursor ?? const Offset(0.5, 0.5)
        : _point(local);
    if (point == null) return;
    // Enqueue the entire click synchronously. The store orders these operations
    // and invalidates them together on a grant/session change.
    _move(point);
    unawaited(widget.onButton(button, true));
    unawaited(widget.onButton(button, false));
  }

  bool _isTouch(PointerEvent event) =>
      event.kind == ui.PointerDeviceKind.touch ||
      event.kind == ui.PointerDeviceKind.stylus ||
      event.kind == ui.PointerDeviceKind.invertedStylus;

  void _down(PointerDownEvent event) {
    if (!_isTouch(event)) {
      final point = _point(event.localPosition);
      if (!widget.enabled || point == null) return;
      widget.onFocus();
      final button = event.buttons & kSecondaryMouseButton != 0
          ? 'right'
          : event.buttons & kMiddleMouseButton != 0
          ? 'middle'
          : 'left';
      _mouseButtons[event.pointer] = button;
      _move(point);
      unawaited(widget.onButton(button, true));
      return;
    }
    if (_touches.isEmpty) {
      _maxTouches = 0;
      _moved = false;
      _touchStarted = event.timeStamp;
      _tapPoint = event.localPosition;
      _touchEligible =
          widget.enabled &&
          (widget.trackpad || _point(event.localPosition) != null);
      // Keep consecutive trackpad strokes continuous. The displayed cursor may
      // still belong to an earlier frame on a slow link; rebasing every touch
      // on it makes the pointer jump backwards before that frame catches up.
      _remoteCursor ??= widget.cursor;
      if (_touchEligible) widget.onFocus();
    }
    _touches[event.pointer] = (event.localPosition, event.localPosition);
    _maxTouches = math.max(_maxTouches, _touches.length);
    if (_touches.length >= 2) {
      _hold?.cancel();
      if (_dragging) {
        _dragging = false;
        unawaited(widget.onRelease());
      }
      if (_touches.length == 2) {
        _tapPoint =
            _touches.values.map((p) => p.$1).reduce((a, b) => a + b) / 2;
      }
    } else if (_touchEligible) {
      _hold = Timer(kLongPressTimeout, () {
        if (!mounted || !widget.enabled || _touches.length != 1 || _moved) {
          return;
        }
        _dragging = true;
        _move(
          widget.trackpad
              ? _remoteCursor ?? const Offset(0.5, 0.5)
              : _point(_tapPoint, clamp: true)!,
        );
        unawaited(widget.onButton('left', true));
      });
    }
  }

  void _movePointer(PointerMoveEvent event) {
    if (!_isTouch(event)) {
      if (_mouseButtons.containsKey(event.pointer)) {
        final point = _point(event.localPosition, clamp: true);
        if (point != null) _move(point);
      }
      return;
    }
    final previous = _touches[event.pointer];
    if (previous == null) return;
    _touches[event.pointer] = (previous.$1, event.localPosition);
    if ((event.localPosition - previous.$1).distance > kTouchSlop) {
      _moved = true;
      _hold?.cancel();
    }
    if (_maxTouches != 1 || !_touchEligible || !_moved || !widget.enabled) {
      return;
    }
    if (widget.trackpad) {
      final cursor = _remoteCursor ?? const Offset(0.5, 0.5);
      final delta = event.localPosition - previous.$2;
      _move(
        cursor +
            Offset(
              delta.dx / (_scene.width * _scale),
              delta.dy / (_scene.height * _scale),
            ),
      );
    } else {
      // A stroke moves the pointer; holding first is the explicit drag gesture.
      // This also leaves a second finger time to join without clicking remotely.
      _move(_point(event.localPosition, clamp: true)!);
    }
  }

  void _up(PointerUpEvent event) {
    if (!_isTouch(event)) {
      final button = _mouseButtons.remove(event.pointer);
      if (button != null) unawaited(widget.onButton(button, false));
      return;
    }
    final touch = _touches.remove(event.pointer);
    if (touch == null) return;
    if ((event.localPosition - touch.$1).distance > kTouchSlop) _moved = true;
    if (_touches.isNotEmpty) return;
    _hold?.cancel();
    _hold = null;
    if (_dragging) {
      _dragging = false;
      unawaited(widget.onButton('left', false));
    } else if (_touchEligible &&
        !_moved &&
        event.timeStamp - _touchStarted <= kLongPressTimeout) {
      if (_maxTouches == 1) _click('left', event.localPosition);
      if (_maxTouches == 2) _click('right', _tapPoint);
    }
  }

  Offset _boundedOffset(Offset desired, double scale) {
    double axis(double offset, double viewport, double scene) =>
        scene * scale <= viewport
        ? (viewport - scene * scale) / 2
        : offset.clamp(viewport - scene * scale, 0.0);
    return Offset(
      axis(desired.dx, _viewport.width, _scene.width),
      axis(desired.dy, _viewport.height, _scene.height),
    );
  }

  void _layout(Size viewport) {
    final width = math.min(
      viewport.width,
      viewport.height * widget.aspectRatio,
    );
    final scene = Size(width, width / widget.aspectRatio);
    if (viewport == _viewport && scene == _scene) return;
    final center =
        _point(_viewport.center(Offset.zero), clamp: true) ??
        const Offset(0.5, 0.5);
    _viewport = viewport;
    _scene = scene;
    _offset = _boundedOffset(
      viewport.center(Offset.zero) -
          Offset(scene.width * center.dx, scene.height * center.dy) * _scale,
      _scale,
    );
    _scalePointers = null;
  }

  void _scaleStart(ScaleStartDetails details) {
    _scalePointers = null;
  }

  void _scaleUpdate(ScaleUpdateDetails details) {
    // A lifted pinch finger must not turn the remainder into a remote drag.
    // PointerPanZoom events from a physical trackpad have pointerCount == 0.
    final navigate = details.pointerCount != 1 || !widget.enabled;
    if (!navigate || _scene.isEmpty) return;
    if (_scalePointers != details.pointerCount) {
      _scalePointers = details.pointerCount;
      _startScale = _scale;
      _gestureScale = details.scale;
      _scaleAnchor = (details.localFocalPoint - _offset) / _scale;
      return;
    }
    if (details.pointerCount >= 2) _moved = true;
    final next = (_startScale * details.scale / _gestureScale).clamp(1.0, 8.0);
    setState(() {
      _scale = next;
      _offset = _boundedOffset(
        details.localFocalPoint - _scaleAnchor * next,
        next,
      );
    });
  }

  void _signal(PointerSignalEvent event) {
    if (event is! PointerScrollEvent) return;
    if (widget.enabled && !HardwareKeyboard.instance.isControlPressed) {
      final point = _point(event.localPosition);
      if (point != null) {
        _move(point);
        unawaited(widget.onWheel(event.scrollDelta));
      }
    } else {
      final anchor = (event.localPosition - _offset) / _scale;
      final next = (_scale * math.exp(-event.scrollDelta.dy / 250)).clamp(
        1.0,
        8.0,
      );
      setState(() {
        _scale = next;
        _offset = _boundedOffset(event.localPosition - anchor * next, next);
      });
    }
  }

  @override
  Widget build(BuildContext context) => LayoutBuilder(
    builder: (context, constraints) {
      _layout(constraints.biggest);
      return Listener(
        behavior: HitTestBehavior.opaque,
        onPointerDown: _down,
        onPointerMove: _movePointer,
        onPointerUp: _up,
        onPointerCancel: (_) => cancelInput(),
        onPointerHover: (event) {
          final point = _point(event.localPosition);
          if (point != null) _move(point);
        },
        onPointerSignal: _signal,
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          supportedDevices: const {
            ui.PointerDeviceKind.touch,
            ui.PointerDeviceKind.stylus,
            ui.PointerDeviceKind.invertedStylus,
            ui.PointerDeviceKind.trackpad,
          },
          onScaleStart: _scaleStart,
          onScaleUpdate: _scaleUpdate,
          onScaleEnd: (_) => _scalePointers = null,
          child: ClipRect(
            child: Stack(
              fit: StackFit.expand,
              children: [
                Positioned(
                  left: 0,
                  top: 0,
                  width: _scene.width,
                  height: _scene.height,
                  child: Transform(
                    key: const Key('desktop-scene-transform'),
                    transform: Matrix4.diagonal3Values(_scale, _scale, 1)
                      ..setTranslationRaw(_offset.dx, _offset.dy, 0),
                    child: RepaintBoundary(child: widget.child),
                  ),
                ),
                ValueListenableBuilder<Offset?>(
                  valueListenable: _cursorPaint,
                  builder: (_, cursor, _) => cursor == null
                      ? const SizedBox.shrink()
                      : Positioned(
                          left: _offset.dx + cursor.dx * _scene.width * _scale,
                          top: _offset.dy + cursor.dy * _scene.height * _scale,
                          child: const IgnorePointer(
                            child: CustomPaint(
                              size: Size(18, 27),
                              painter: DesktopCursorPainter(),
                            ),
                          ),
                        ),
                ),
              ],
            ),
          ),
        ),
      );
    },
  );
}

/// A visible pointer remains the same screen size at every zoom level.
class DesktopCursorPainter extends CustomPainter {
  const DesktopCursorPainter();
  @override
  void paint(Canvas canvas, Size size) {
    final pointer = Path()
      ..moveTo(1, 1)
      ..lineTo(1, 20)
      ..lineTo(6, 16)
      ..lineTo(10, 25)
      ..lineTo(14, 23)
      ..lineTo(10, 15)
      ..lineTo(17, 15)
      ..close();
    canvas.drawPath(pointer, Paint()..color = Colors.black);
    canvas.drawPath(
      pointer,
      Paint()
        ..color = Colors.white
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1.5,
    );
  }

  @override
  bool shouldRepaint(covariant DesktopCursorPainter oldDelegate) => false;
}
