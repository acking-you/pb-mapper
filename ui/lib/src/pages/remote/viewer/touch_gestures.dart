import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/services.dart';

import '../../../state/desktop_input.dart';
import '../../../state/viewer_prefs.dart';
import 'remote_viewport.dart';

/// How long the second touch of a double tap is held before it drags.
const kHoldToDrag = Duration(milliseconds: 300);

/// A drag's press goes no sooner than this after the click before it, so
/// the host never merges the two into a double click. The host enforces its
/// own `GetDoubleClickTime() + 20 ms` on `drag_start` presses (R-UI-3);
/// this is the viewer's hint of it.
const kRemoteDoubleClickGuard = Duration(milliseconds: 520);

/// In direct touch, how long a moving first finger waits for a second one
/// before it presses: a pinch must never start as a drag.
const kPointerJoinGrace = Duration(milliseconds: 80);

/// Two fingers start navigating after this, or as soon as one moves.
const kNavigateStart = Duration(milliseconds: 80);

/// Mean finger travel per wheel notch of a three-finger scroll.
const kThreeFingerNotchPx = 24.0;

/// A long press's joystick: the radius of its ring, which is as far as the
/// ball strays from the press point.
const kJoystickRadius = 60.0;

/// Within this of the press point along an axis, the ball scrolls nothing
/// along it.
const kJoystickDeadZone = 10.0;

/// How often a deflected joystick sends the wheel.
const kJoystickTick = Duration(milliseconds: 16);

/// Wheel notches per second along an axis just past the dead zone, and with
/// the ball at the ring.
const kJoystickMinRate = 2.0;
const kJoystickMaxRate = 25.0;

enum HapticKind { press, select }

/// The viewers' haptics: a long press's joystick presses; a drag start and
/// a joystick's first scroll click.
void viewerHaptic(HapticKind kind) => unawaited(switch (kind) {
  HapticKind.press => HapticFeedback.mediumImpact(),
  HapticKind.select => HapticFeedback.selectionClick(),
});

/// Where the touch gestures send pointer intent: [RemoteInput], or a
/// recorder in tests. Points are normalised to the picture.
abstract interface class RemotePointerSink {
  /// The remote cursor as this viewer has it, which trackpad strokes move
  /// and taps click at: its own, never handed back to the host on a timer.
  Offset get cursor;
  set cursor(Offset normalized);

  void hover(Offset n);
  void move(Offset n);
  void press(RemoteButton b, Offset n, {bool dragStart = false});
  void release(RemoteButton b);
  void click(RemoteButton b, Offset n);
  void wheelStart();
  void wheel(int horizontal, int vertical);
  void wheelEnd();
}

/// A long press's joystick, in the stage's coordinates: a ring fixed at
/// the press point and the ball under the finger, kept inside it.
@immutable
class TouchJoystick {
  const TouchJoystick({
    required this.centre,
    required this.ball,
    this.active = false,
  });

  final Offset centre;
  final Offset ball;

  /// The ball has left the dead zone: a wheel burst is open.
  final bool active;

  @override
  bool operator ==(Object other) =>
      other is TouchJoystick &&
      other.centre == centre &&
      other.ball == ball &&
      other.active == active;

  @override
  int get hashCode => Object.hash(centre, ball, active);
}

enum _Phase {
  idle,

  /// The sequence started outside the visible picture in direct touch.
  ignored,

  /// Not interactive: one finger pans the zoomed picture locally.
  pan,
  firstDown,
  secondDown,
  stroke,
  dragging,

  /// After a long press: the finger steers the wheel.
  joystick,
  navigate,
  scroll,
}

class _Finger {
  _Finger(this.down) : last = down;
  final Offset down;
  Offset last;
  bool moved = false;
}

/// A tap that may become the first half of a double tap.
typedef _Tap = ({
  Duration upAt,
  Offset downAt,
  Duration clickAt,
  Offset cursorAt,
});

/// The touch gestures of ui §10.2 (D18): the sole owner of touch, for
/// remote input and local navigation alike.
///
/// Pure and deterministic: every event carries its time; the owner arms one
/// timer for [nextTimeout] and calls [timeout] when it fires. A sequence
/// runs from the first finger down to the last finger up, and its class
/// only escalates — one finger, two (navigate), three or more (scroll) — so
/// lifting a pinch finger never becomes a stroke or a drag.
///
/// A long press turns the finger into a joystick (D-S1). The ball steers
/// the view as a browser's autoscroll does: pushed up, the remote page
/// scrolls up, towards its top; pushed right, it scrolls right. The host's
/// wheel is positive down and right, so each notch takes the sign of the
/// ball's offset: the opposite of the three-finger swipe, where the content
/// follows the fingers. Lifted without leaving the dead zone, it is a right
/// click.
class TouchGestureMachine {
  TouchGestureMachine({
    required RemotePointerSink sink,
    required RemoteViewport viewport,
    required TouchMode Function() mode,
    required bool Function() interactive,
    required void Function(HapticKind) haptic,
  }) : _sink = sink,
       _viewport = viewport,
       _mode = mode,
       _interactive = interactive,
       _haptic = haptic;

  final RemotePointerSink _sink;
  final RemoteViewport _viewport;
  final TouchMode Function() _mode;
  final bool Function() _interactive;
  final void Function(HapticKind) _haptic;

  final _fingers = <int, _Finger>{};
  final _indicator = ValueNotifier<Offset?>(null);
  final _joystick = ValueNotifier<TouchJoystick?>(null);

  _Phase _phase = _Phase.idle;
  bool _direct = false, _live = false;

  Duration? _graceAt, _longPressAt, _holdAt, _pressDueAt, _wheelAt;

  /// Trackpad: the last tap, consulted only by the next first finger.
  _Tap? _lastTap;

  /// Where a drag presses: the double tap's cursor (trackpad) or the first
  /// finger (direct), and whether that press was sent.
  Offset _pressAt = Offset.zero;
  bool _pressSent = false;

  /// Direct touch: the finger moved before the join grace ran out.
  bool _dragPending = false;

  Duration _secondDownAt = Duration.zero;
  bool _navigating = false;
  List<int> _navFingers = const [];
  List<Offset> _navStart = const [];

  double _scrollTravel = 0;

  /// A wheel burst is open: three fingers, or a deflected joystick.
  bool _wheelStarted = false;

  /// The joystick's wheel: accounted up to [_wheelFrom], with the notches'
  /// fractions carried per axis.
  Duration _wheelFrom = Duration.zero;
  double _wheelX = 0, _wheelY = 0;

  /// The finger in trackpad mode, for the touch indicator; null between
  /// sequences and while the joystick shows.
  ValueListenable<Offset?> get indicatorPoint => _indicator;

  /// A long press's joystick, in either mode; null otherwise.
  ValueListenable<TouchJoystick?> get joystick => _joystick;

  /// The earliest pending deadline.
  Duration? get nextTimeout {
    Duration? first;
    for (final d in [_graceAt, _longPressAt, _holdAt, _pressDueAt, _wheelAt]) {
      if (d != null && (first == null || d < first)) first = d;
    }
    return first;
  }

  void down(int pointer, Offset local, Duration t) {
    _fireDue(t);
    if (_fingers.containsKey(pointer)) return;
    _fingers[pointer] = _Finger(local);
    if (_fingers.length == 1) {
      _begin(local, t);
      return;
    }
    switch (_phase) {
      case _Phase.idle || _Phase.ignored:
        return;
      case _Phase.navigate:
        if (_fingers.length >= 3) {
          _toScroll();
        } else {
          _rebaseNavigation();
        }
      case _Phase.scroll:
        return;
      case _Phase.pan ||
          _Phase.firstDown ||
          _Phase.secondDown ||
          _Phase.stroke ||
          _Phase.dragging ||
          _Phase.joystick:
        _toNavigate(t);
        if (_fingers.length >= 3) _toScroll();
    }
  }

  void move(int pointer, Offset local, Duration t) {
    _fireDue(t);
    final finger = _fingers[pointer];
    if (finger == null) return;
    final delta = local - finger.last;
    finger.last = local;
    if (!finger.moved && (local - finger.down).distance > kTouchSlop) {
      finger.moved = true;
    }
    final moved = finger.moved;
    switch (_phase) {
      case _Phase.idle || _Phase.ignored:
        return;
      case _Phase.pan:
        if (moved) _viewport.panBy(delta);
      case _Phase.firstDown when _direct:
        if (!moved) return;
        _longPressAt = null;
        final grace = _graceAt;
        if (grace == null || t >= grace) {
          _graceAt = null;
          _startDirectDrag(local);
        } else {
          _dragPending = true;
        }
      case _Phase.firstDown:
        _indicator.value = local;
        if (!moved) return;
        _longPressAt = null;
        _phase = _Phase.stroke;
        _stroke(delta);
      case _Phase.secondDown:
        _indicator.value = local;
        if (!moved) return;
        _holdAt = null;
        _toDrag(t);
        _drag(delta);
      case _Phase.stroke:
        _indicator.value = local;
        _stroke(delta);
      case _Phase.joystick:
        _steer(local, t);
      case _Phase.dragging when _direct:
        _sink.move(_pointAt(local));
      case _Phase.dragging:
        _indicator.value = local;
        _drag(delta);
      case _Phase.navigate:
        if (!_navigating &&
            (t - _secondDownAt >= kNavigateStart ||
                _fingers.values.any((f) => f.moved))) {
          _navigating = true;
        }
        if (_navigating) {
          _viewport.pinchPan(_navStart, [
            for (final id in _navFingers) _fingers[id]!.last,
          ]);
        }
      case _Phase.scroll:
        if (!_wheelStarted) return;
        // Fingers down pull the content down: the wheel goes up.
        _scrollTravel += delta.dy / _fingers.length;
        final notches = (_scrollTravel / kThreeFingerNotchPx).truncate();
        if (notches != 0) {
          _scrollTravel -= notches * kThreeFingerNotchPx;
          _sink.wheel(0, -notches);
        }
    }
  }

  void up(int pointer, Offset local, Duration t) {
    _fireDue(t);
    final finger = _fingers.remove(pointer);
    if (finger == null) return;
    if (_fingers.isNotEmpty) {
      // No de-escalation: the remaining fingers go on as they were.
      if (_phase == _Phase.navigate) _rebaseNavigation();
      return;
    }
    switch (_phase) {
      case _Phase.firstDown when _direct:
        if (_dragPending) {
          _sink.press(RemoteButton.left, _pressAt);
          _sink.move(_pointAt(local));
          _sink.release(RemoteButton.left);
        } else {
          _sink.click(RemoteButton.left, _pressAt);
        }
      case _Phase.firstDown:
        // At once (D18): a single tap is never delayed to wait for a second.
        final cursor = _sink.cursor;
        _sink.click(RemoteButton.left, cursor);
        _lastTap = (upAt: t, downAt: finger.down, clickAt: t, cursorAt: cursor);
      case _Phase.secondDown:
        // A quick second tap: the host sees a double click.
        _sink.click(RemoteButton.left, _sink.cursor);
        _lastTap = null;
      case _Phase.dragging:
        // The host holds this press past its double-click time (R-UI-3).
        if (!_pressSent) _sendDragPress();
        _sink.release(RemoteButton.left);
      case _Phase.joystick when !_wheelStarted:
        // Never out of the dead zone: the long press was a right click.
        _sink.click(RemoteButton.right, _direct ? _pressAt : _sink.cursor);
      case _Phase.scroll || _Phase.joystick:
        if (_wheelStarted) _sink.wheelEnd();
      case _Phase.idle ||
          _Phase.ignored ||
          _Phase.pan ||
          _Phase.stroke ||
          _Phase.navigate:
        break;
    }
    _end();
  }

  /// A pointer cancel, an epoch, mode or readiness change, or focus loss:
  /// a sent press is released, a pending one dropped, a wheel burst ended.
  void cancel(Duration t) {
    if (_phase == _Phase.dragging && _pressSent) {
      _sink.release(RemoteButton.left);
    }
    if (_wheelStarted) _sink.wheelEnd();
    _fingers.clear();
    _lastTap = null;
    _end();
  }

  /// Fires every deadline up to [t], in order.
  void timeout(Duration t) => _fireDue(t);

  void dispose() {
    _indicator.dispose();
    _joystick.dispose();
  }

  // ------------------------------------------------------------- phases

  void _begin(Offset p, Duration t) {
    _direct = _mode() == TouchMode.direct;
    _live = _interactive();
    _pressSent = false;
    _dragPending = false;
    if (!_live) {
      _lastTap = null;
      _phase = _Phase.pan;
      return;
    }
    if (_direct) {
      // What lies under a gutter, or the keyboard, is not touched.
      final n = _viewport.visibleRect.contains(p)
          ? _viewport.toNormalized(p)
          : null;
      if (n == null) {
        _phase = _Phase.ignored;
        return;
      }
      _pressAt = n;
      _longPressAt = t + kLongPressTimeout;
      _graceAt = t + kPointerJoinGrace;
      _phase = _Phase.firstDown;
      return;
    }
    _indicator.value = p;
    final tap = _lastTap;
    if (tap != null &&
        t - tap.upAt >= kDoubleTapMinTime &&
        t - tap.upAt <= kDoubleTapTimeout &&
        (p - tap.downAt).distance <= kDoubleTapSlop) {
      _holdAt = t + kHoldToDrag;
      _phase = _Phase.secondDown;
    } else {
      _lastTap = null;
      _longPressAt = t + kLongPressTimeout;
      _phase = _Phase.firstDown;
    }
  }

  /// A double tap becomes a drag. Its press waits out the remote double
  /// click after the first tap's click.
  void _toDrag(Duration t) {
    final tap = _lastTap!;
    _lastTap = null;
    _pressAt = tap.cursorAt;
    final due = tap.clickAt + kRemoteDoubleClickGuard;
    _pressDueAt = due > t ? due : t;
    _phase = _Phase.dragging;
    _fireDue(t);
  }

  void _startDirectDrag(Offset last) {
    _longPressAt = null;
    _sink.press(RemoteButton.left, _pressAt);
    _sink.move(_pointAt(last));
    _pressSent = true;
    _phase = _Phase.dragging;
  }

  void _toNavigate(Duration t) {
    if (_phase == _Phase.dragging && _pressSent) {
      _sink.release(RemoteButton.left);
    }
    if (_wheelStarted) _sink.wheelEnd();
    _wheelStarted = false;
    _graceAt = _longPressAt = _holdAt = _pressDueAt = _wheelAt = null;
    _pressSent = false;
    _dragPending = false;
    _lastTap = null;
    _indicator.value = null;
    _joystick.value = null;
    _phase = _Phase.navigate;
    _secondDownAt = t;
    _navigating = false;
    _rebaseNavigation();
  }

  /// The fingers changed: navigation continues from where they are now.
  void _rebaseNavigation() {
    _navFingers = _fingers.keys.toList();
    _navStart = [for (final f in _fingers.values) f.last];
  }

  /// A third finger freezes navigation and scrolls.
  void _toScroll() {
    _phase = _Phase.scroll;
    _scrollTravel = 0;
    _wheelStarted = _live;
    if (_wheelStarted) _sink.wheelStart();
  }

  void _end() {
    _graceAt = _longPressAt = _holdAt = _pressDueAt = _wheelAt = null;
    _pressSent = false;
    _dragPending = false;
    _wheelStarted = false;
    _navigating = false;
    _phase = _Phase.idle;
    _indicator.value = null;
    _joystick.value = null;
  }

  /// The finger held still: it becomes a joystick where it rests, so the
  /// jitter of the hold opens no burst.
  void _toJoystick() {
    _graceAt = null;
    final at = _fingers.values.single.last;
    _indicator.value = null;
    _phase = _Phase.joystick;
    _wheelX = _wheelY = 0;
    _joystick.value = TouchJoystick(centre: at, ball: at);
  }

  /// The ball follows the finger inside the ring. Out of the dead zone it
  /// opens the wheel burst once — in direct touch over the press point, so
  /// the wheel reaches the window under the finger; in trackpad mode at the
  /// cursor — and keeps the wheel ticking while it stays out.
  void _steer(Offset local, Duration t) {
    final stick = _joystick.value;
    if (stick == null) return;
    final offset = _clampToRing(local - stick.centre);
    final out = _outOfDeadZone(offset);
    if (out && !_wheelStarted) {
      if (_direct) _sink.hover(_pressAt);
      _sink.wheelStart();
      _wheelStarted = true;
      _haptic(HapticKind.select);
    }
    if (!out) {
      _wheelAt = null;
      _wheelX = _wheelY = 0;
    } else if (_wheelAt == null) {
      _wheelFrom = t;
      _wheelAt = t + kJoystickTick;
    }
    _joystick.value = TouchJoystick(
      centre: stick.centre,
      ball: stick.centre + offset,
      active: _wheelStarted,
    );
  }

  /// The wheel for the time since the last tick, at the rate the ball's
  /// offset gives each axis; whole notches go, fractions wait.
  void _wheelTick(Duration t) {
    final stick = _joystick.value;
    if (_phase != _Phase.joystick || stick == null) return;
    final seconds = (t - _wheelFrom).inMicroseconds / 1e6;
    _wheelFrom = t;
    final offset = stick.ball - stick.centre;
    double axis(double carried, double along) {
      final rate = _rate(along);
      return rate == 0 ? 0 : carried + rate * seconds;
    }

    _wheelX = axis(_wheelX, offset.dx);
    _wheelY = axis(_wheelY, offset.dy);
    final h = _wheelX.truncate(), v = _wheelY.truncate();
    _wheelX -= h;
    _wheelY -= v;
    if (h != 0 || v != 0) _sink.wheel(h, v);
    _wheelAt = t + kJoystickTick;
  }

  static Offset _clampToRing(Offset offset) {
    final d = offset.distance;
    return d <= kJoystickRadius ? offset : offset * (kJoystickRadius / d);
  }

  static bool _outOfDeadZone(Offset offset) =>
      offset.dx.abs() > kJoystickDeadZone ||
      offset.dy.abs() > kJoystickDeadZone;

  /// Signed wheel notches per second for a ball [offset] along one axis:
  /// none within the dead zone, then from [kJoystickMinRate] just past it
  /// up to [kJoystickMaxRate] at the ring, on a quadratic so a small push
  /// reads a page and a full one flies.
  static double _rate(double offset) {
    final past = offset.abs() - kJoystickDeadZone;
    if (past <= 0) return 0;
    final u = math.min(past / (kJoystickRadius - kJoystickDeadZone), 1.0);
    final rate =
        kJoystickMinRate + (kJoystickMaxRate - kJoystickMinRate) * u * u;
    return offset.isNegative ? -rate : rate;
  }

  // ------------------------------------------------------------ actions

  /// Direct touch: the picture point under the finger, pulled to the edge
  /// when it slides off (or where the drag began, before any layout).
  Offset _pointAt(Offset local) =>
      _viewport.toNormalized(local, clamp: true) ?? _pressAt;

  Offset _cursorBy(Offset delta) {
    final scene = _viewport.scene * _viewport.scale;
    if (scene.isEmpty) return _sink.cursor;
    final c = _sink.cursor;
    final next = Offset(
      (c.dx + delta.dx / scene.width).clamp(0.0, 1.0),
      (c.dy + delta.dy / scene.height).clamp(0.0, 1.0),
    );
    _sink.cursor = next;
    _viewport.ensureVisible(next);
    return next;
  }

  void _stroke(Offset delta) => _sink.hover(_cursorBy(delta));

  /// The local cursor always moves; the remote one only after the press.
  void _drag(Offset delta) {
    final cursor = _cursorBy(delta);
    if (_pressSent) _sink.move(cursor);
  }

  /// The trackpad drag's delayed press, at the double tap, then a move to
  /// where the finger took the cursor meanwhile. That point is read first:
  /// the press puts the cursor back at the double tap.
  void _sendDragPress() {
    final to = _sink.cursor;
    _sink.press(RemoteButton.left, _pressAt, dragStart: true);
    _sink.move(to);
    _pressSent = true;
  }

  void _fireDue(Duration t) {
    while (true) {
      final due = nextTimeout;
      if (due == null || due > t) return;
      if (due == _graceAt) {
        _graceAt = null;
        if (_phase == _Phase.firstDown &&
            _dragPending &&
            _fingers.length == 1) {
          _startDirectDrag(_fingers.values.single.last);
        }
      } else if (due == _longPressAt) {
        _longPressAt = null;
        if (_phase == _Phase.firstDown) {
          // Nothing is sent yet: a lift is a right click, a push scrolls.
          _haptic(HapticKind.press);
          _toJoystick();
        }
      } else if (due == _wheelAt) {
        _wheelAt = null;
        _wheelTick(due);
      } else if (due == _holdAt) {
        _holdAt = null;
        if (_phase == _Phase.secondDown) {
          _haptic(HapticKind.select);
          _toDrag(due);
        }
      } else if (due == _pressDueAt) {
        _pressDueAt = null;
        if (_phase == _Phase.dragging && !_pressSent) _sendDragPress();
      }
    }
  }
}
