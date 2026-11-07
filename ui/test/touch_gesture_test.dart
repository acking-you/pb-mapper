import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_viewport.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/touch_gestures.dart';
import 'package:pb_mapper_ui/src/state/desktop_input.dart';
import 'package:pb_mapper_ui/src/state/viewer_prefs.dart';

import 'remote_viewer_harness.dart';

/// Records what the gestures send, in order.
class _Sink implements RemotePointerSink {
  final calls = <String>[];

  @override
  Offset cursor = const Offset(0.5, 0.5);

  @override
  set strokeActive(bool active) {}

  @override
  void hover(Offset n) => calls.add('hover');
  @override
  void move(Offset n) => calls.add('move');
  @override
  void press(RemoteButton b, Offset n, {bool dragStart = false}) =>
      calls.add('press ${b.name}${dragStart ? ' drag' : ''}');
  @override
  void release(RemoteButton b) => calls.add('release ${b.name}');
  @override
  void click(RemoteButton b, Offset n) => calls.add('click ${b.name}');
  @override
  void wheelStart() => calls.add('wheelStart');
  @override
  void wheel(int horizontal, int vertical) =>
      calls.add('wheel $horizontal,$vertical');
  @override
  void wheelEnd() => calls.add('wheelEnd');
}

Duration ms(int value) => Duration(milliseconds: value);

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

  testWidgets(
    'a trackpad drag presses at the double tap and takes the host to where the finger slid while the press waited',
    (tester) async {
      final h = await ViewerHarness.live(tester);
      // The picture is 1920 px wide: a finger's pixel is a display pixel.
      h.viewport.layout(const Size(1920, 1200));
      final machine = h.machine;
      const at = Offset(200, 150);
      h.sent();

      // The second touch slides 120 px before the press is due at 580 ms.
      machine.down(1, at, ms(0));
      machine.up(1, at, ms(60));
      machine.down(2, at, ms(160));
      machine.move(2, at + const Offset(30, 0), ms(200));
      machine.move(2, at + const Offset(120, 0), ms(300));
      expect(h.sent(), [
        ['move 960,540', 'left↓', 'left↑'],
      ]);
      machine.timeout(ms(580));
      machine.up(2, at + const Offset(120, 0), ms(700));
      expect(h.sent(), [
        ['move 960,540', 'left↓ drag'],
        ['move 1080,540'],
        ['left↑'],
      ]);

      // Lifted before the press is due, the drag still goes whole. (The
      // pump reopens the store's move gate, so the move goes on its own.)
      await tester.pump(const Duration(milliseconds: 100));
      machine.down(3, at, ms(2000));
      machine.up(3, at, ms(2060));
      machine.down(4, at, ms(2160));
      machine.move(4, at + const Offset(30, 0), ms(2200));
      machine.move(4, at + const Offset(120, 0), ms(2300));
      machine.up(4, at + const Offset(120, 0), ms(2400));
      expect(h.sent(), [
        ['move 1080,540', 'left↓', 'left↑'],
        ['move 1080,540', 'left↓ drag'],
        ['move 1200,540'],
        ['left↑'],
      ]);

      h.dispose();
    },
  );

  test(
    'long press right-clicks, a two-finger tap does nothing, three fingers scroll without navigating, and a pinch never presses in direct touch',
    () {
      final (:machine, :sink, :viewport, :haptics) = _machine(
        TouchMode.trackpad,
      );

      machine.down(1, const Offset(200, 150), ms(0));
      machine.timeout(ms(500));
      machine.up(1, const Offset(200, 150), ms(700));
      expect(sink.calls, ['click right']);
      expect(haptics, [HapticKind.press]);

      // Two-finger tap: dropped (D18).
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
      final direct = _machine(TouchMode.direct);
      direct.machine.down(1, const Offset(180, 150), ms(0));
      direct.machine.move(1, const Offset(150, 150), ms(20));
      direct.machine.down(2, const Offset(220, 150), ms(60));
      direct.machine.timeout(ms(600));
      direct.machine.move(2, const Offset(300, 150), ms(120));
      direct.machine.up(1, const Offset(150, 150), ms(200));
      direct.machine.up(2, const Offset(300, 150), ms(210));
      expect(direct.sink.calls, isEmpty);
      expect(direct.viewport.scale, greaterThan(1.5));
    },
  );
}
