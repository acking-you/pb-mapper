import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/modifier_latch.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_keys.dart';
import 'package:pb_mapper_ui/src/state/desktop_input.dart';

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
