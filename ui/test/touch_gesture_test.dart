import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_viewport.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/touch_gestures.dart';
import 'package:pb_mapper_ui/src/state/desktop_input.dart';
import 'package:pb_mapper_ui/src/state/viewer_prefs.dart';

/// Records what the gestures send, in order, and where clicks and hovers
/// went.
class _Sink implements RemotePointerSink {
  final calls = <String>[];
  final points = <Offset>[];

  @override
  Offset cursor = const Offset(0.5, 0.5);

  @override
  set strokeActive(bool active) {}

  @override
  void hover(Offset n) {
    calls.add('hover');
    points.add(n);
  }

  @override
  void move(Offset n) => calls.add('move');
  @override
  void press(RemoteButton b, Offset n, {bool dragStart = false}) =>
      calls.add('press ${b.name}${dragStart ? ' drag' : ''}');
  @override
  void release(RemoteButton b) => calls.add('release ${b.name}');
  @override
  void click(RemoteButton b, Offset n) {
    calls.add('click ${b.name}');
    points.add(n);
  }

  /// The wheel's notches so far, summed per axis.
  (int, int) get wheeled {
    var h = 0, v = 0;
    for (final c in calls) {
      if (!c.startsWith('wheel ')) continue;
      final [x, y] = c.substring(6).split(',').map(int.parse).toList();
      h += x;
      v += y;
    }
    return (h, v);
  }

  @override
  void wheelStart() => calls.add('wheelStart');
  @override
  void wheel(int horizontal, int vertical) =>
      calls.add('wheel $horizontal,$vertical');
  @override
  void wheelEnd() => calls.add('wheelEnd');
}

Duration ms(int value) => Duration(milliseconds: value);

/// Fires the machine's deadlines one by one up to [end], as the surface's
/// timer does.
void _runUntil(TouchGestureMachine machine, Duration end) {
  for (var due = machine.nextTimeout; due != null && due <= end;) {
    machine.timeout(due);
    due = machine.nextTimeout;
  }
}

/// A 400×300 stage showing a 2:1 picture: the scene is 400×200 at y 50.
({
  TouchGestureMachine machine,
  _Sink sink,
  RemoteViewport viewport,
  List<HapticKind> haptics,
})
_machine(TouchMode mode) {
  final sink = _Sink();
  final viewport = RemoteViewport()
    ..aspectRatio = 2
    ..layout(const Size(400, 300));
  final haptics = <HapticKind>[];
  final machine = TouchGestureMachine(
    sink: sink,
    viewport: viewport,
    mode: () => mode,
    interactive: () => true,
    haptic: haptics.add,
  );
  return (machine: machine, sink: sink, viewport: viewport, haptics: haptics);
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test(
    'trackpad taps click at once and a held second tap drags without a remote double-click',
    () {
      final (:machine, :sink, viewport: _, :haptics) = _machine(
        TouchMode.trackpad,
      );
      const at = Offset(200, 150);

      machine.down(1, at, ms(0));
      machine.up(1, at, ms(60));
      // Sent on the first up, never held back for a second tap (D18).
      expect(sink.calls, ['click left']);

      // A second touch within the double-tap time, held: it drags, but
      // its press waits until 520 ms after the first click.
      machine.down(2, at + const Offset(4, 0), ms(160));
      expect(machine.nextTimeout, ms(460));
      machine.timeout(ms(460));
      expect(haptics, [HapticKind.select]);
      expect(machine.nextTimeout, ms(580));
      machine.move(2, at + const Offset(30, 0), ms(500));
      expect(sink.calls, ['click left'], reason: 'nothing before 580 ms');
      machine.timeout(ms(580));
      machine.move(2, at + const Offset(60, 0), ms(620));
      machine.up(2, at + const Offset(60, 0), ms(700));
      expect(sink.calls, [
        'click left',
        'press left drag',
        'move',
        'move',
        'release left',
      ]);

      // A quick second tap is a second immediate click.
      sink.calls.clear();
      machine.down(3, at, ms(2000));
      machine.up(3, at, ms(2060));
      machine.down(4, at, ms(2160));
      machine.up(4, at, ms(2200));
      expect(sink.calls, ['click left', 'click left']);
      expect(machine.nextTimeout, isNull);
    },
  );

  test(
    'a long press lifted in place right-clicks on release, a two-finger tap does nothing, three fingers scroll without navigating, and a pinch never presses in direct touch',
    () {
      final (:machine, :sink, :viewport, :haptics) = _machine(
        TouchMode.trackpad,
      );

      // The hold shows the joystick and sends nothing; a lift that never
      // left the dead zone is the right click, at the cursor.
      machine.down(1, const Offset(200, 150), ms(0));
      machine.timeout(ms(500));
      expect(haptics, [HapticKind.press]);
      expect(machine.joystick.value?.centre, const Offset(200, 150));
      machine.move(1, const Offset(206, 145), ms(600));
      expect(sink.calls, isEmpty);
      machine.up(1, const Offset(206, 145), ms(700));
      expect(sink.calls, ['click right']);
      expect(sink.points.single, sink.cursor);
      expect(machine.joystick.value, isNull);

      // Direct touch: where it was pressed. The scene is 400×200 at y 50.
      final direct = _machine(TouchMode.direct);
      direct.machine.down(1, const Offset(100, 100), ms(0));
      direct.machine.timeout(ms(500));
      direct.machine.up(1, const Offset(100, 100), ms(800));
      expect(direct.sink.calls, ['click right']);
      expect(direct.sink.points.single, const Offset(0.25, 0.25));

      // Two-finger tap: dropped (D18).
      sink.calls.clear();
      sink.calls.clear();
      machine.down(1, const Offset(150, 150), ms(1000));
      machine.down(2, const Offset(250, 150), ms(1030));
      machine.up(1, const Offset(150, 150), ms(1090));
      machine.up(2, const Offset(250, 150), ms(1100));
      expect(sink.calls, isEmpty);

      // Three fingers 72 px down: three notches of wheel up, no zoom.
      final scale = viewport.scale, offset = viewport.offset;
      const starts = [Offset(150, 100), Offset(200, 100), Offset(250, 100)];
      for (var i = 0; i < 3; i++) {
        machine.down(i, starts[i], ms(2000 + i * 10));
      }
      for (var step = 1; step <= 6; step++) {
        for (var i = 0; i < 3; i++) {
          machine.move(
            i,
            starts[i] + Offset(0, step * 12.0),
            ms(2100 + step * 16 + i),
          );
        }
      }
      for (var i = 0; i < 3; i++) {
        machine.up(i, starts[i] + const Offset(0, 72), ms(2300 + i));
      }
      final wheels = [
        for (final c in sink.calls)
          if (c.startsWith('wheel ')) int.parse(c.split(',').last),
      ];
      expect(wheels.fold<int>(0, (a, b) => a + b), -3);
      expect(sink.calls.first, 'wheelStart');
      expect(sink.calls.last, 'wheelEnd');
      expect((viewport.scale, viewport.offset), (scale, offset));

      // Direct touch: the first finger moves before a second one joins
      // within the grace; the pinch zooms and nothing is pressed.
      final pinch = _machine(TouchMode.direct);
      pinch.machine.down(1, const Offset(180, 150), ms(0));
      pinch.machine.move(1, const Offset(150, 150), ms(20));
      pinch.machine.down(2, const Offset(220, 150), ms(60));
      pinch.machine.timeout(ms(600));
      pinch.machine.move(2, const Offset(300, 150), ms(120));
      pinch.machine.up(1, const Offset(150, 150), ms(200));
      pinch.machine.up(2, const Offset(300, 150), ms(210));
      expect(pinch.sink.calls, isEmpty);
      expect(pinch.viewport.scale, greaterThan(1.5));
    },
  );

  test(
    'a pushed joystick scrolls the way it points, on both axes, until it is lifted',
    () {
      final (:machine, :sink, viewport: _, :haptics) = _machine(
        TouchMode.trackpad,
      );
      const at = Offset(200, 150);
      machine.down(1, at, ms(0));
      machine.timeout(ms(500));

      // Pushed up past the dead zone: the burst opens at the cursor, and
      // notches flow on the machine's own ticks, up (negative) only.
      machine.move(1, at + const Offset(4, -40), ms(520));
      expect(sink.calls, ['wheelStart']);
      expect(machine.joystick.value?.active, isTrue);
      expect(haptics, [HapticKind.press, HapticKind.select]);
      _runUntil(machine, ms(1520));
      final (h1, v1) = sink.wheeled;
      expect(h1, 0);
      expect(v1, inInclusiveRange(-11, -9), reason: 'about 10 a second');

      // Up and to the right: both axes, right being positive.
      machine.move(1, at + const Offset(30, -40), ms(1520));
      _runUntil(machine, ms(2520));
      final (h2, v2) = sink.wheeled;
      expect(h2, greaterThan(0));
      expect(v2, lessThan(v1));

      // Back in the dead zone: no ticks, but the burst stays open.
      machine.move(1, at + const Offset(3, 2), ms(2530));
      expect(machine.nextTimeout, isNull);
      expect(sink.wheeled, (h2, v2));

      // The lift ends the burst and clicks nothing.
      machine.up(1, at, ms(2600));
      expect(sink.calls.first, 'wheelStart');
      expect(sink.calls.last, 'wheelEnd');
      expect(sink.calls.where((c) => !c.startsWith('wheel')), isEmpty);
      expect(sink.calls.where((c) => c == 'wheelEnd'), hasLength(1));
      expect(machine.joystick.value, isNull);
    },
  );

  test(
    'in direct touch the joystick wheels over the press point, and a second finger or a cancel ends the burst',
    () {
      final (:machine, :sink, :viewport, haptics: _) = _machine(
        TouchMode.direct,
      );
      machine.down(1, const Offset(100, 100), ms(0));
      machine.timeout(ms(500));
      machine.move(1, const Offset(100, 140), ms(520));
      // Hovered first, so the wheel reaches the window under the finger.
      expect(sink.calls, ['hover', 'wheelStart']);
      expect(sink.points.single, const Offset(0.25, 0.25));
      _runUntil(machine, ms(1000));
      expect(sink.wheeled.$2, isPositive, reason: 'pushed down');

      // A second finger: the burst ends, and two fingers only navigate.
      sink.calls.clear();
      machine.down(2, const Offset(200, 140), ms(1000));
      expect(sink.calls, ['wheelEnd']);
      expect(machine.nextTimeout, isNull);
      machine.move(2, const Offset(260, 140), ms(1100));
      machine.up(1, const Offset(100, 140), ms(1200));
      machine.up(2, const Offset(260, 140), ms(1210));
      expect(sink.calls, ['wheelEnd']);
      expect(viewport.scale, greaterThan(1.2));

      // A cancel ends a burst as well.
      sink.calls.clear();
      machine.down(3, const Offset(100, 120), ms(3000));
      machine.timeout(ms(3500));
      machine.move(3, const Offset(60, 120), ms(3520));
      machine.cancel(ms(3600));
      expect(sink.calls.first, 'hover');
      expect(sink.calls.last, 'wheelEnd');
      expect(machine.nextTimeout, isNull);
      expect(machine.joystick.value, isNull);
    },
  );
}
