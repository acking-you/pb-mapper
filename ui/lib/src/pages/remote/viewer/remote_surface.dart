import 'dart:async';
import 'dart:math' as math;
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import '../../../state/desktop_input.dart';
import '../../../state/viewer_prefs.dart';
import '../../../theme/motion.dart';
import '../../../theme/viewer_palette.dart';
import 'remote_input.dart';
import 'remote_scroll.dart';
import 'remote_viewport.dart';
import 'touch_gestures.dart';

/// How long the touch indicator takes to fade after the last finger.
const kTouchIndicatorFade = Duration(milliseconds: 150);

/// The remote picture and every pointer over it (ui §10.2-10.3).
///
/// One [Listener] takes all pointers: touch and stylus go to the
/// [TouchGestureMachine], the sole owner of touch; a mouse and a touchpad
/// are handled here. A wheel's notch is one notch on the host, whatever
/// distance this computer's embedder reports for it ([wheelPxPerNotch]); a
/// touchpad's travel goes in 1/120 of a notch to a host that takes it, and
/// its fling is carried on ([ScrollFling]). The picture is [child], laid
/// out at the viewport's scene and moved by one [Transform]: zoom, pan, the
/// soft keyboard and reconnects change the transform only, never the
/// child's element, so a native texture is never re-created (P1-4). The
/// surface tells the viewport where it is in the view, so a pixel-exact
/// picture lands on whole device pixels.
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

class _RemoteSurfaceState extends State<RemoteSurface>
    with SingleTickerProviderStateMixin {
  /// Fires the machine's next deadline.
  Timer? _timer;

  /// The mouse buttons this surface pressed on the host, per pointer.
  final _buttons = <int, Set<RemoteButton>>{};

  /// Scrolling not yet a whole step (a notch, or 1/120 of one toward a host
  /// that takes that): a wheel's apart from a touchpad's travel and its
  /// fling, so what one left never shortens the other's next step.
  final _wheel = ScrollCarry(), _travel = ScrollCarry();
  double _panZoomScale = 1;

  /// A touchpad's pan so far, whose velocity starts a fling as it ends:
  /// from its first move that scrolls the host, unless [_panFlings] went
  /// false, as a pinch or a pan of the picture here makes it.
  VelocityTracker? _pan;
  bool _panFlings = false;

  /// The fling carrying a touchpad's pan on, and how far it has gone.
  ScrollFling? _fling;
  Offset _flung = Offset.zero;
  late final Ticker _flingTicker;

  /// A read of the surface's place in the view waits for the frame's end.
  bool _originDue = false;

  RemoteViewport get _viewport => widget.viewport;
  RemoteInput get _input => widget.input;
  TouchGestureMachine get _machine => widget.machine;

  @override
  void initState() {
    super.initState();
    _flingTicker = createTicker(_flingTick);
    widget.touchMode.addListener(_modeChanged);
    widget.input.localCancels.addListener(_scrollCancelled);
    WheelLines.load();
  }

  @override
  void didUpdateWidget(covariant RemoteSurface oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.touchMode != widget.touchMode) {
      oldWidget.touchMode.removeListener(_modeChanged);
      widget.touchMode.addListener(_modeChanged);
    }
    if (oldWidget.input != widget.input) {
      oldWidget.input.localCancels.removeListener(_scrollCancelled);
      widget.input.localCancels.addListener(_scrollCancelled);
      _scrollCancelled();
    }
    if (oldWidget.machine != widget.machine) _timer?.cancel();
  }

  @override
  void dispose() {
    widget.touchMode.removeListener(_modeChanged);
    widget.input.localCancels.removeListener(_scrollCancelled);
    _flingTicker.dispose();
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
    _stopFling();
    if (_isTouch(e)) {
      _machine.down(e.pointer, e.localPosition, _input.clock());
      _arm();
      return;
    }
    _viewport.setOrigin(_originOf(e));
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
    // A drag's moves keep its down's transform, stale if the stage moved
    // since: the origin came with the down, or with a layout since.
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
    _viewport
      ..setOrigin(_originOf(e))
      ..autoScrollTo(e.localPosition);
    final n = _viewport.toNormalized(e.localPosition);
    if (n != null && _input.ready.value) _input.hover(n);
  }

  void _signal(PointerSignalEvent e) {
    if (e is PointerScrollInertiaCancelEvent) {
      // Fingers back on the touchpad stop a fling, as they stop the
      // system's.
      _stopFling();
      return;
    }
    if (e is! PointerScrollEvent) return;
    final trackpad = e.kind == ui.PointerDeviceKind.trackpad;
    // A wheel turned stops a fling, as it stops a scroll view's.
    if (!trackpad) _stopFling();
    final dy = e.scrollDelta.dy, dx = e.scrollDelta.dx;
    if (HardwareKeyboard.instance.isControlPressed) {
      // Zoom stays local (U20).
      _viewport.zoomAt(e.localPosition, math.exp(-dy / 250));
      return;
    }
    final n = _viewport.toNormalized(e.localPosition);
    if (n == null || !_input.ready.value) return;
    _input.hover(n);
    _scrollBy(dx, dy, trackpad: trackpad);
  }

  void _panZoomStart(PointerPanZoomStartEvent e) {
    _stopFling();
    _panZoomScale = 1;
    _pan = null;
    _panFlings = true;
  }

  void _panZoomUpdate(PointerPanZoomUpdateEvent e) {
    if (e.scale != 1) {
      // A pinch flings nothing.
      _pan = null;
      _panFlings = false;
      _viewport.zoomAt(e.localPosition, e.scale / _panZoomScale);
      _panZoomScale = e.scale;
      return;
    }
    if (_input.ready.value) {
      if (_panFlings) {
        (_pan ??= ScrollFling.tracker(
          defaultTargetPlatform,
        )).addPosition(e.timeStamp, e.pan);
      }
      // Natural scrolling: the content follows the fingers (U10).
      _scrollBy(-e.panDelta.dx, -e.panDelta.dy, trackpad: true);
    } else {
      _pan = null;
      _panFlings = false;
      _viewport.panBy(e.panDelta);
    }
  }

  /// Fingers that lift while the pan still moves fling it on.
  void _panZoomEnd(PointerPanZoomEndEvent e) {
    final pan = _pan;
    _pan = null;
    _panFlings = false;
    if (pan == null || !_input.ready.value) return;
    final fling = ScrollFling.start(
      pan.getVelocity(),
      platform: defaultTargetPlatform,
      devicePixelRatio: _devicePixelRatio,
    );
    if (fling == null) return;
    _stopFling();
    _fling = fling;
    _flung = Offset.zero;
    _flingTicker.start();
  }

  /// Each frame of a fling scrolls the host by what it travelled since the
  /// last, as the pan it carries on did.
  void _flingTick(Duration elapsed) {
    final fling = _fling;
    if (fling == null || !_input.ready.value) {
      _stopFling();
      return;
    }
    final at = fling.offsetAt(elapsed);
    final step = at - _flung;
    _flung = at;
    _scrollBy(-step.dx, -step.dy, trackpad: true);
    if (fling.isDoneAt(elapsed)) _stopFling();
  }

  void _stopFling() {
    _fling = null;
    if (_flingTicker.isActive) _flingTicker.stop();
  }

  /// Focus or control lost, or a new picture generation, perhaps of a host
  /// that scrolls in other steps: the fling and any pan under way stop, and
  /// what fell short of a step is dropped with them.
  void _scrollCancelled() {
    _stopFling();
    _pan = null;
    _panFlings = false;
    _wheel.clear();
    _travel.clear();
  }

  double get _devicePixelRatio =>
      View.maybeOf(context)?.devicePixelRatio ?? 1.0;

  /// A touchpad's travel ([trackpad]) or a wheel's distance, in logical
  /// pixels as Flutter reports them, to the host: to one that takes it, in
  /// 1/120 of a notch, 100 px a notch of travel and a wheel's notch as
  /// this computer's embedder reports one; to any other, in whole notches,
  /// 30 px of travel each. What falls short of a step waits for the next
  /// of its kind.
  void _scrollBy(double dx, double dy, {required bool trackpad}) {
    final notch = trackpad
        ? null
        : wheelPxPerNotch(
            defaultTargetPlatform,
            devicePixelRatio: _devicePixelRatio,
            windowsLines: WheelLines.value,
          );
    final carry = trackpad ? _travel : _wheel;
    if (_input.hiresWheel) {
      final (h, v) = notch == null
          ? carry.take(dx * kScrollUnitsPerPixel, dy * kScrollUnitsPerPixel)
          : carry.take(
              dx / notch * DesktopScroll.unitsPerNotch,
              dy / notch * DesktopScroll.unitsPerNotch,
            );
      if (h != 0 || v != 0) _input.scroll(h, v, continuous: trackpad);
    } else {
      final per = notch ?? kTrackpadPxPerNotch;
      final (h, v) = carry.take(dx / per, dy / per);
      if (h != 0 || v != 0) _input.wheel(h, v);
    }
  }

  // ---------------------------------------------------------------- origin

  /// The surface's top left in the view, from the transform of a hover or
  /// a down, each hit-tested where the stage is now: current even when the
  /// stage moved without a new layout. Null unless it only translates.
  static Offset? _originOf(PointerEvent e) {
    final transform = e.transform;
    if (transform == null) return Offset.zero;
    final toLocal = MatrixUtils.getAsTranslation(transform);
    return toLocal == null ? null : -toLocal;
  }

  /// Once the frame that laid the surface out is done, where that put it
  /// in the view: the viewport snaps a pixel-exact picture relative to it.
  void _readOrigin() {
    if (_originDue) return;
    _originDue = true;
    SchedulerBinding.instance.addPostFrameCallback((_) {
      _originDue = false;
      final box = mounted ? context.findRenderObject() : null;
      if (box is! RenderBox || !box.attached) return;
      _viewport.setOrigin(
        MatrixUtils.getAsTranslation(box.getTransformTo(null)),
      );
    });
  }

  @override
  Widget build(BuildContext context) => LayoutBuilder(
    builder: (context, constraints) {
      _viewport.layout(constraints.biggest);
      _readOrigin();
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
        onPointerPanZoomEnd: _panZoomEnd,
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
