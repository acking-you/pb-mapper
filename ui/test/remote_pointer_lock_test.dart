import 'package:flutter/gestures.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_pointer.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_cursor_layer.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_pointer_lock.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_surface.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';
import 'package:pb_mapper_ui/src/state/viewer_prefs.dart';

import 'remote_viewer_harness.dart';

Duration _ms(int value) => Duration(milliseconds: value);

/// Rules for a viewer that can hold its pointer, in [mode].
PointerLockRules _rules(PointerMode mode) => PointerLockRules()
  ..setMode(mode)
  ..setAvailable(true);

/// The runner's pointer lock as a test sees it: each call, and the
/// callbacks of the lock it holds.
class _FakeLock implements DesktopPointerLock {
  final calls = <String>[];
  void Function(int dx, int dy)? motion;
  void Function()? lost;

  /// The runner cannot hold the pointer.
  bool fail = false;

  @override
  Future<void> lock({
    required void Function(int dx, int dy) onMotion,
    required void Function() onLost,
  }) async {
    calls.add('lock');
    if (fail) throw PlatformException(code: 'desktop_pointer');
    motion = onMotion;
    lost = onLost;
  }

  @override
  Future<void> unlock() async {
    calls.add('unlock');
    motion = null;
    lost = null;
  }
}

KeyEvent _esc(String kind) => switch (kind) {
  'down' => const KeyDownEvent(
    physicalKey: PhysicalKeyboardKey.escape,
    logicalKey: LogicalKeyboardKey.escape,
    timeStamp: Duration.zero,
  ),
  'repeat' => const KeyRepeatEvent(
    physicalKey: PhysicalKeyboardKey.escape,
    logicalKey: LogicalKeyboardKey.escape,
    timeStamp: Duration.zero,
  ),
  _ => const KeyUpEvent(
    physicalKey: PhysicalKeyboardKey.escape,
    logicalKey: LogicalKeyboardKey.escape,
    timeStamp: Duration.zero,
  ),
};

/// What a live host reports of its pointer: hidden or showing, in
/// generation 1, as report [seq] of the session.
DesktopView _view({required bool visible, required int seq}) => DesktopView(
  generation: 1,
  presentedFrameId: 1,
  displayWidth: 1920,
  displayHeight: 1080,
  videoWidth: 1920,
  videoHeight: 1080,
  state: 2,
  controlling: true,
  controlAllowed: true,
  fresh: true,
  cursorSeq: seq,
  cursorVisible: visible,
  cursorX: 100,
  cursorY: 200,
);

void main() {
  group('pointer lock rules', () {
    test('Auto holds the pointer once the remote one hides while a button '
        'is held, and lets go once it shows with every button up', () {
      final rules = _rules(PointerMode.auto);
      expect(rules.buttons(2), isFalse);
      expect(rules.locked || rules.relative, isFalse);
      // A game hides its pointer as the right button turns the camera.
      rules.hostCursor(hidden: true);
      expect(rules.locked && rules.relative, isTrue);
      // Shown for a moment mid-drag: still held, until every button is up.
      rules
        ..hostCursor(hidden: false)
        ..hostCursor(hidden: true)
        ..hostCursor(hidden: false);
      expect(rules.locked, isTrue);
      expect(rules.buttons(0), isFalse);
      expect(rules.locked || rules.relative, isFalse);
      // Hidden with no button held: moves go relative at once, and a
      // press holds the pointer.
      rules.hostCursor(hidden: true);
      expect(rules.relative, isTrue);
      expect(rules.locked, isFalse);
      rules.buttons(1);
      expect(rules.locked, isTrue);
    });

    test('Auto holds a pointer that stays hidden through 300 ms of '
        'movement, never one Windows hides while typing', () {
      final rules = _rules(PointerMode.auto);
      // Typing hid it; the first moves show it again within 300 ms.
      rules
        ..hostCursor(hidden: true)
        ..moved(_ms(0))
        ..moved(_ms(120))
        ..hostCursor(hidden: false)
        ..moved(_ms(310))
        ..moved(_ms(900));
      expect(rules.locked || rules.relative, isFalse);
      // A game's pointer stays hidden: held 300 ms after the first move.
      rules
        ..hostCursor(hidden: true)
        ..moved(_ms(1000))
        ..moved(_ms(1299));
      expect(rules.locked, isFalse);
      rules.moved(_ms(1300));
      expect(rules.locked, isTrue);
    });

    test('Desktop never holds the pointer; Game holds it from a click '
        'on the picture, none of which goes', () {
      final desktop = _rules(PointerMode.desktop)..hostCursor(hidden: true);
      desktop
        ..buttons(1)
        ..moved(_ms(0))
        ..moved(_ms(1000));
      expect(desktop.locked || desktop.relative || desktop.paused, isFalse);

      final game = _rules(PointerMode.game);
      expect(game.paused, isTrue);
      expect(game.relative, isFalse);
      // The click's press, a second button and its releases go nowhere.
      expect(game.buttons(1), isTrue);
      expect(game.locked && game.relative, isTrue);
      expect(game.buttons(3), isTrue);
      expect(game.buttons(1), isTrue);
      expect(game.buttons(0), isTrue);
      // The next click goes, and the remote pointer's state is no matter.
      game.hostCursor(hidden: false);
      expect(game.buttons(1), isFalse);
      expect(game.buttons(0), isFalse);
      expect(game.locked, isTrue);
    });

    test('Esc held a second lets go and a shorter one does not; Auto holds '
        'again only after a click', () {
      final rules = _rules(PointerMode.auto)
        ..hostCursor(hidden: true)
        ..buttons(1)
        ..buttons(0);
      expect(rules.locked, isTrue);
      rules.escDown(_ms(0));
      rules.tick(_ms(999));
      rules.escUp();
      rules.tick(_ms(1500));
      expect(rules.locked, isTrue);
      rules.escDown(_ms(2000));
      expect(rules.escDue, _ms(3000));
      rules.tick(_ms(3000));
      expect(rules.locked || rules.relative, isFalse);
      rules
        ..moved(_ms(3100))
        ..moved(_ms(4000));
      expect(rules.locked, isFalse);
      rules.buttons(1);
      expect(rules.locked, isTrue);
    });

    test('focus loss lets go and Game waits for a click again; a viewer '
        'that can no longer hold the pointer lets it go', () {
      final game = _rules(PointerMode.game)
        ..buttons(1)
        ..buttons(0);
      expect(game.locked, isTrue);
      game.letGo();
      expect(game.locked, isFalse);
      expect(game.paused, isTrue);
      expect(game.buttons(1), isTrue);
      expect(game.locked, isTrue);
      game.setAvailable(false);
      expect(game.locked || game.paused || game.relative, isFalse);
      // Choosing Desktop lets go too.
      final auto = _rules(PointerMode.auto)
        ..hostCursor(hidden: true)
        ..buttons(1);
      expect(auto.locked, isTrue);
      auto.setMode(PointerMode.desktop);
      expect(auto.locked || auto.relative, isFalse);
    });
  });

  testWidgets('relative moves add up behind the 4 ms gate, after an '
      'absolute move that replaces what waited, and NOT_FRESH drops them', (
    tester,
  ) async {
    final h = await ViewerHarness.live(
      tester,
      capabilities: ['relative_pointer'],
    );
    final store = h.store;
    final epoch = store.geometry.value!.epoch;
    h.sent();
    bool by(int dx, int dy) =>
        store.send(DesktopInput.moveBy(dx, dy), geometryEpoch: epoch);

    for (var i = 0; i < 5; i++) {
      expect(by(3, -2), isTrue);
    }
    expect(h.sent(), [
      ['by 3,-2'],
    ]);
    await tester.pump(_ms(4));
    expect(h.sent(), [
      ['by 12,-8'],
    ]);
    // The gate is open again for 4 ms: an absolute move replaces what
    // waits, a relative one after it follows it, and a click takes both
    // first.
    by(1, 1);
    store.send(DesktopInput.move(5, 5), geometryEpoch: epoch);
    by(2, 0);
    store.sendSequence([
      DesktopInput.button(RemoteButton.left, true),
      DesktopInput.button(RemoteButton.left, false),
    ], geometryEpoch: epoch);
    expect(h.sent(), [
      ['move 5,5', 'by 2,0', 'left↓', 'left↑'],
    ]);
    // Past a move's bound, what waits goes at once.
    await tester.pump(_ms(8));
    h.sent();
    by(1, 0);
    by(4000, 0);
    by(200, 0);
    expect(h.sent(), [
      ['by 1,0'],
      ['by 4000,0'],
    ]);
    await tester.pump(_ms(4));
    expect(h.sent(), [
      ['by 200,0'],
    ]);
    await tester.pump(_ms(4));

    // A stale picture drops relative motion, and the absolute move it
    // followed: neither goes with the next fresh picture, where a refused
    // absolute move alone would.
    h.sent();
    by(1, 1);
    store.send(DesktopInput.move(5, 5), geometryEpoch: epoch);
    by(9, 9);
    h.api.failNext('input', 'NOT_FRESH');
    expect(
      store.send(DesktopInput.key(RemoteKey.a, true), geometryEpoch: epoch),
      isFalse,
    );
    expect(store.session.value.controlling, isTrue);
    expect(h.sent(), [
      ['by 1,1'],
      ['move 5,5', 'by 9,9', 'a↓'],
    ]);
    h.api.viewValue = DesktopView(
      generation: 1,
      presentedFrameId: 2,
      displayWidth: 1920,
      displayHeight: 1080,
      state: 2,
      controlling: true,
      controlAllowed: true,
      fresh: true,
    );
    await tester.pump(_ms(16));
    expect(h.sent(), isEmpty);
    h.dispose();
  });

  testWidgets('Game holds the pointer from a click, sends its raw motion, '
      'paints the host pointer, and lets go at a second of Esc, at focus '
      'loss and when the runner loses it', (tester) async {
    final h = await ViewerHarness.live(
      tester,
      capabilities: ['relative_pointer'],
    );
    final prefs = ViewerPrefs(persist: false);
    await prefs.setPointerMode(PointerMode.game);
    final runner = _FakeLock();
    final lock = RemotePointerLock(
      desktop: h.store,
      input: h.input,
      prefs: prefs,
      runner: runner,
    );
    h.sent();
    expect(lock.paused, isTrue);
    expect(lock.buttons(1), isTrue);
    expect(lock.buttons(0), isTrue);
    await tester.pump();
    expect(runner.calls, ['lock']);
    expect(h.input.relative && h.store.cursor.relative, isTrue);
    expect(lock.relativeListenable.value, isTrue);
    expect(
      RemotePointerLook.of(
        mouse: true,
        controlling: true,
        atLocalPointer: true,
        kind: null,
        relative: true,
      ),
      const RemotePointerLook(
        SystemMouseCursors.none,
        RemotePointerPaint.arrow,
      ),
    );
    runner.motion!(5, -3);
    runner.motion!(2, 1);
    await tester.pump(_ms(4));
    expect(h.sent(), [
      ['by 5,-3'],
      ['by 2,1'],
    ]);
    // A click lands where the host's pointer is, with no move.
    h.input
      ..press(RemoteButton.left, const Offset(0.1, 0.1))
      ..release(RemoteButton.left);
    expect(h.sent(), [
      ['left↓'],
      ['left↑'],
    ]);
    // Every report is followed: the host's pointer is painted where it is.
    h.api.viewValue = _view(visible: true, seq: 1);
    await tester.pump(_ms(16));
    expect(h.store.cursor.shown, Offset(100 / 1920, 200 / 1080));

    // Esc goes to the host; its repeats do not; a second of it lets go.
    expect(lock.key(_esc('down')), isFalse);
    expect(lock.key(_esc('repeat')), isTrue);
    await tester.pump(_ms(500));
    expect(runner.calls, ['lock']);
    await tester.pump(_ms(500));
    expect(runner.calls, ['lock', 'unlock']);
    expect(lock.key(_esc('up')), isFalse);
    expect(lock.paused, isTrue);
    expect(h.input.relative, isFalse);
    // Nothing moves the host until the next click holds the pointer again.
    expect(lock.buttons(1), isTrue);
    expect(lock.buttons(0), isTrue);
    await tester.pump();
    expect(runner.calls.last, 'lock');
    lock.focusLost();
    await tester.pump();
    expect(runner.calls.last, 'unlock');
    lock
      ..buttons(1)
      ..buttons(0);
    await tester.pump();
    runner.lost!();
    expect(lock.paused, isTrue);
    expect(runner.calls, ['lock', 'unlock', 'lock', 'unlock', 'lock']);

    // A runner that cannot hold the pointer leaves it free.
    runner.fail = true;
    lock
      ..buttons(1)
      ..buttons(0);
    await tester.pump();
    expect(lock.locked, isFalse);
    expect(h.input.relative, isFalse);
    lock.dispose();
    h.dispose();
  });

  testWidgets('over the picture, Game sends nothing from the mouse until a '
      'click, which goes nowhere and holds the pointer, and then a click '
      'lands where the host pointer is; Auto sends a hidden host pointer the '
      "mouse's motion in this computer's pixels", (tester) async {
    tester.view.devicePixelRatio = 2;
    addTearDown(tester.view.resetDevicePixelRatio);
    for (final mode in [PointerMode.game, PointerMode.auto]) {
      final h = await ViewerHarness.live(
        tester,
        capabilities: ['relative_pointer'],
      );
      final prefs = ViewerPrefs(persist: false);
      await prefs.setPointerMode(mode);
      final runner = _FakeLock();
      final lock = RemotePointerLock(
        desktop: h.store,
        input: h.input,
        prefs: prefs,
        runner: runner,
      );
      await tester.pumpWidget(
        Directionality(
          textDirection: TextDirection.ltr,
          child: Align(
            alignment: Alignment.topLeft,
            child: SizedBox(
              width: 400,
              height: 300,
              child: RemoteSurface(
                viewport: h.viewport,
                input: h.input,
                machine: h.machine,
                touchMode: h.touchMode,
                pointerLock: lock,
                child: const Texture(textureId: 17),
              ),
            ),
          ),
        ),
      );
      await tester.pump();
      h.sent();
      const at = Offset(200, 150);
      final mouse = await tester.createGesture(kind: PointerDeviceKind.mouse);
      await mouse.addPointer(location: at);
      if (mode == PointerMode.game) {
        await mouse.moveTo(at + const Offset(10, 0));
        await mouse.down(at);
        await mouse.up();
        await tester.pump();
        expect(h.sent(), isEmpty);
        expect(runner.calls, ['lock']);
        // Held: the runner's motion moves the host, not the local pointer.
        await mouse.moveTo(at + const Offset(30, 5));
        await mouse.down(at);
        await mouse.up();
        expect(h.sent(), [
          ['left↓'],
          ['left↑'],
        ]);
      } else {
        h.api.viewValue = _view(visible: false, seq: 1);
        await tester.pump(_ms(16));
        await mouse.moveTo(at + const Offset(10, 0));
        await mouse.moveTo(at + const Offset(10, 2.25));
        await tester.pump(_ms(4));
        expect(h.sent(), [
          ['by 20,0'],
          ['by 0,4'],
        ]);
        expect(runner.calls, isEmpty);
      }
      await mouse.removePointer();
      await tester.pumpWidget(const SizedBox());
      lock.dispose();
      h.dispose();
    }
  });

  testWidgets('Auto holds the pointer while the host reports its own hidden '
      'and a button is held, and lets go once it shows with every button '
      'up; a host that does not list relative_pointer is never held', (
    tester,
  ) async {
    for (final listed in [true, false]) {
      final h = await ViewerHarness.live(
        tester,
        capabilities: [if (listed) 'relative_pointer'],
      );
      final prefs = ViewerPrefs(persist: false);
      final runner = _FakeLock();
      final lock = RemotePointerLock(
        desktop: h.store,
        input: h.input,
        prefs: prefs,
        runner: runner,
      );
      h.api.viewValue = _view(visible: false, seq: 1);
      await tester.pump(_ms(16));
      expect(h.store.cursor.hostHidden, isTrue);
      expect(lock.buttons(2), isFalse);
      await tester.pump();
      expect(runner.calls, listed ? ['lock'] : isEmpty);
      h.api.viewValue = _view(visible: true, seq: 2);
      await tester.pump(_ms(16));
      expect(lock.locked, listed);
      lock.buttons(0);
      await tester.pump();
      expect(lock.locked, isFalse);
      expect(runner.calls, listed ? ['lock', 'unlock'] : isEmpty);
      lock.dispose();
      h.dispose();
    }
  });
}
