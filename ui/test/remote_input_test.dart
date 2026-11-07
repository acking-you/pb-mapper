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
