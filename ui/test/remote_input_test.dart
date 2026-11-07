import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/modifier_latch.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_cursor_layer.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_keys.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';

import 'fake_desktop_api.dart';
import 'remote_viewer_harness.dart';

KeyEvent _down(
  LogicalKeyboardKey logical,
  PhysicalKeyboardKey physical, [
  String? character,
]) => KeyDownEvent(
  physicalKey: physical,
  logicalKey: logical,
  character: character,
  timeStamp: Duration.zero,
);

KeyEvent _up(LogicalKeyboardKey logical, PhysicalKeyboardKey physical) =>
    KeyUpEvent(
      physicalKey: physical,
      logicalKey: logical,
      timeStamp: Duration.zero,
    );

Duration _ms(int value) => Duration(milliseconds: value);

void main() {
  testWidgets('latched modifiers wrap one action and never stay held', (
    tester,
  ) async {
    final h = await ViewerHarness.live(tester);
    final input = h.input, latch = h.latch;
    h.sent();

    // Armed: one action carries it, then it is off.
    latch.tap(RemoteKey.control);
    input.tapKey(RemoteKey.c);
    expect(h.sent(), [
      ['control↓', 'c↓', 'c↑', 'control↑'],
    ]);
    expect(latch.isEmpty, isTrue);

    // Locked: every action carries it, and it stays.
    latch
      ..tap(RemoteKey.control)
      ..tap(RemoteKey.control);
    input
      ..tapKey(RemoteKey.c)
      ..tapKey(RemoteKey.v)
      ..click(RemoteButton.left, const Offset(0.5, 0.5));
    expect(h.sent(), [
      ['control↓', 'c↓', 'c↑', 'control↑'],
      ['control↓', 'v↓', 'v↑', 'control↑'],
      ['move 960,540', 'control↓', 'left↓', 'left↑', 'control↑'],
    ]);
    expect(latch.stateOf(RemoteKey.control), LatchState.locked);

    // A hardware key is wrapped too, and its up sends nothing more.
    final picture = FocusNode();
    expect(
      input.handleHardwareKey(
        picture,
        _down(LogicalKeyboardKey.f5, PhysicalKeyboardKey.f5),
      ),
      KeyEventResult.handled,
    );
    input.handleHardwareKey(
      picture,
      _up(LogicalKeyboardKey.f5, PhysicalKeyboardKey.f5),
    );
    expect(h.sent(), [
      ['control↓', 'f5↓', 'f5↑', 'control↑'],
    ]);
    picture.dispose();

    // A dropped press is released whole by the store, so nothing is left
    // to release afterwards.
    latch.clear();
    h.api.failNext('input', 'NOT_FRESH');
    input.press(RemoteButton.left, const Offset(0.5, 0.5));
    expect(h.releases, 1);
    input.releaseAll();
    expect(h.releases, 1);

    // A held button is released once, and only once.
    input.press(RemoteButton.left, const Offset(0.5, 0.5));
    expect(h.sent().last, ['move 960,540', 'left↓']);
    input.releaseAll();
    expect(h.releases, 2);
    input.releaseAll();
    expect(h.releases, 2);

    h.dispose();
  });

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
      machine.down(1, at, _ms(0));
      machine.up(1, at, _ms(60));
      machine.down(2, at, _ms(160));
      machine.move(2, at + const Offset(30, 0), _ms(200));
      machine.move(2, at + const Offset(120, 0), _ms(300));
      expect(h.sent(), [
        ['move 960,540', 'left↓', 'left↑'],
      ]);
      machine.timeout(_ms(580));
      machine.up(2, at + const Offset(120, 0), _ms(700));
      expect(h.sent(), [
        ['move 960,540', 'left↓ drag'],
        ['move 1080,540'],
        ['left↑'],
      ]);

      // Lifted before the press is due, the drag still goes whole. (The
      // pump reopens the store's move gate, so the move goes on its own.)
      await tester.pump(const Duration(milliseconds: 100));
      machine.down(3, at, _ms(2000));
      machine.up(3, at, _ms(2060));
      machine.down(4, at, _ms(2160));
      machine.move(4, at + const Offset(30, 0), _ms(2200));
      machine.move(4, at + const Offset(120, 0), _ms(2300));
      machine.up(4, at + const Offset(120, 0), _ms(2400));
      expect(h.sent(), [
        ['move 1080,540', 'left↓', 'left↑'],
        ['move 1080,540', 'left↓ drag'],
        ['move 1200,540'],
        ['left↑'],
      ]);

      h.dispose();
    },
  );

  testWidgets(
    'a host cursor sample is drawn in one frame, none left scheduled',
    (tester) async {
      final h = await ViewerHarness.live(tester);
      h.viewport.layout(const Size(800, 450));
      DesktopView view(int seq, int x, int y, {int generation = 1}) =>
          DesktopView(
            generation: generation,
            presentedFrameId: 1,
            cursorSeq: seq,
            displayWidth: 1920,
            displayHeight: 1080,
            videoWidth: 1920,
            videoHeight: 1080,
            cursorX: x,
            cursorY: y,
            state: 2,
            cursorVisible: true,
            controlling: true,
            controlAllowed: true,
            fresh: true,
          );
      // A point inside the arrow, whose tip is at the cursor.
      PaintPattern arrowAt(Offset n) =>
          paints..path(includes: [h.viewport.toLocal(n) + const Offset(2, 6)]);
      final layer = find.byType(RemoteCursorLayer);

      h.api.viewValue = view(1, 480, 270);
      await tester.pumpWidget(
        RemoteCursorLayer(
          hostCursor: h.store.hostCursor,
          viewport: h.viewport,
          input: h.input,
        ),
      );
      await tester.pump();
      expect(layer, arrowAt(const Offset(0.25, 0.25)));
      expect(tester.binding.hasScheduledFrame, isFalse);

      // Read by the view timer, at the host's 16 ms: the repaint asks for one
      // frame, which draws it.
      h.api.viewValue = view(2, 960, 540);
      await tester.binding.delayed(const Duration(milliseconds: 16));
      expect(tester.binding.hasScheduledFrame, isTrue);
      await tester.pump();
      expect(layer, arrowAt(const Offset(0.5, 0.5)));
      expect(tester.binding.hasScheduledFrame, isFalse);

      // Read in a frame, after its paint: the next frame draws it.
      h.api.viewValue = view(3, 1440, 810);
      tester.binding.scheduleFrame();
      await tester.pump();
      expect(tester.binding.hasScheduledFrame, isTrue);
      await tester.pump();
      expect(layer, arrowAt(const Offset(0.75, 0.75)));
      expect(tester.binding.hasScheduledFrame, isFalse);

      // A new generation's cursor, read before its geometry, is drawn once
      // the geometry is.
      h.api.viewValue = view(1, 480, 270, generation: 2);
      h.api.viewerStatusValue = liveStatus(generation: 2, controlling: true);
      tester.binding.scheduleFrame();
      await tester.pump();
      await tester.pump();
      expect(layer, arrowAt(const Offset(0.25, 0.25)));
      expect(tester.binding.hasScheduledFrame, isFalse);

      h.dispose();
    },
  );

  test('held Ctrl sends bare letter keys and AltGr characters stay text', () {
    // Ctrl+B: the bare key, never a chord that would release the Ctrl the
    // user still holds; its up follows even after Ctrl was let go.
    final b = KeyRouting.route(
      _down(LogicalKeyboardKey.keyB, PhysicalKeyboardKey.keyB),
      latched: const {},
      held: const {RemoteKey.control},
    );
    expect(b.inputs, [const DesktopInput.key(RemoteKey.b, true)]);
    expect(b.holds, RemoteKey.b);
    final bUp = KeyRouting.route(
      _up(LogicalKeyboardKey.keyB, PhysicalKeyboardKey.keyB),
      latched: const {},
      held: const {},
      holding: b.holds,
    );
    expect(bUp.inputs, [const DesktopInput.key(RemoteKey.b, false)]);

    // AltGr arrives as Control + Alt and types its own character.
    final at = KeyRouting.route(
      _down(LogicalKeyboardKey.keyQ, PhysicalKeyboardKey.keyQ, '@'),
      latched: const {},
      held: const {RemoteKey.control, RemoteKey.alt},
    );
    expect(at.inputs, [DesktopInput.text('@')]);
    expect(at.swallowUp, isTrue);

    // Shift went as a real key; the character carries the case.
    final capital = KeyRouting.route(
      _down(LogicalKeyboardKey.keyA, PhysicalKeyboardKey.keyA, 'A'),
      latched: const {},
      held: const {RemoteKey.shift},
    );
    expect(capital.inputs, [DesktopInput.text('A')]);
  });
}
