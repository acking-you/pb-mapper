import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/l10n/app_localizations_en.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/desktop_keyboard.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/modifier_latch.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_cursor_layer.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_keys.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_text_input.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/viewer_menus.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';
import 'package:pb_mapper_ui/src/state/viewer_prefs.dart';

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
        RemoteCursorLayer(viewport: h.viewport, input: h.input),
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

  testWidgets(
    "the viewer's cursor is its own: no wait and no reopened viewer hands it "
    'back to an old report, a new report or a hidden cursor is followed, and '
    'every report while viewing only',
    (tester) async {
      final h = await ViewerHarness.live(tester);
      // The picture is 1920 px wide: a finger's pixel is a display pixel.
      h.viewport.layout(const Size(1920, 1080));
      var machine = h.machine, input = h.input;
      Future<void> report(int x, int y, {bool visible = true}) async {
        h.api.viewValue = DesktopView(
          generation: 1,
          presentedFrameId: 1,
          displayWidth: 1920,
          displayHeight: 1080,
          videoWidth: 1920,
          videoHeight: 1080,
          cursorX: x,
          cursorY: y,
          state: 2,
          cursorVisible: visible,
          controlling: true,
          controlAllowed: true,
          fresh: true,
        );
        await tester.pump(const Duration(milliseconds: 16));
      }

      // The first report of a connection is followed.
      await report(480, 270);
      expect(input.shownCursor, const Offset(0.25, 0.25));

      // A stroke takes it 300 px right; the host reports none of what this
      // viewer injected. However long after, it stays, and a tap clicks
      // there.
      machine.down(1, const Offset(100, 100), _ms(0));
      machine.move(1, const Offset(400, 100), _ms(50));
      machine.up(1, const Offset(400, 100), _ms(100));
      await tester.pump(const Duration(seconds: 2));
      expect(input.shownCursor, input.cursor);
      expect(input.cursor.dx * 1920, closeTo(780, 0.01));
      h.sent();
      machine.down(2, const Offset(400, 100), _ms(3000));
      machine.up(2, const Offset(400, 100), _ms(3050));
      expect(h.sent(), [
        ['move 780,270', 'left↓', 'left↑'],
      ]);

      // Back, and the viewer opened again over the session, whose last
      // report is still the one from before the stroke: it resumes there.
      h.reopen();
      machine = h.machine;
      input = h.input;
      await tester.pump(const Duration(milliseconds: 16));
      expect(input.cursor.dx * 1920, closeTo(780, 0.01));
      machine.down(2, const Offset(400, 100), _ms(4000));
      machine.up(2, const Offset(400, 100), _ms(4050));
      expect(h.sent(), [
        ['move 780,270', 'left↓', 'left↑'],
      ]);

      // A report within a pixel is the host's whole pixels: no move.
      final local = input.cursor;
      await report(781, 270);
      expect(input.cursor, local);

      // The remote user moved it: followed, and the next tap clicks there.
      await report(1440, 810);
      expect(input.cursor, const Offset(0.75, 0.75));
      machine.down(3, const Offset(400, 100), _ms(5000));
      machine.up(3, const Offset(400, 100), _ms(5050));
      expect(h.sent().last, ['move 1440,810', 'left↓', 'left↑']);

      // Hidden, there is no arrow, until this viewer moves it.
      await report(1440, 810, visible: false);
      expect(input.shownCursor, isNull);
      machine.down(4, const Offset(400, 100), _ms(7000));
      machine.move(4, const Offset(420, 100), _ms(7050));
      machine.up(4, const Offset(420, 100), _ms(7100));
      expect(input.shownCursor, isNotNull);

      // Viewing only, every report is followed, within a pixel too.
      h.store.control(false);
      await tester.pump(const Duration(milliseconds: 500));
      await report(1000, 500);
      await report(1001, 500);
      expect(input.cursor, const Offset(1001 / 1920, 500 / 1080));

      h.dispose();
    },
  );

  /// The picture as a viewer has it: focused, keys to [input].
  Future<FocusNode> picture(WidgetTester tester, ViewerHarness h) async {
    final node = FocusNode();
    addTearDown(node.dispose);
    await tester.pumpWidget(
      Focus(
        key: ObjectKey(h),
        focusNode: node,
        autofocus: true,
        onKeyEvent: h.input.handleHardwareKey,
        child: const SizedBox.expand(),
      ),
    );
    await tester.pump();
    return node;
  }

  /// Pressed and released through the platform's key path, as the picture
  /// gets them: whether the picture kept the down.
  Future<bool> press(
    WidgetTester tester,
    LogicalKeyboardKey key, {
    String? character,
    String platform = 'windows',
  }) => tester.sendKeyDownEvent(key, character: character, platform: platform);
  Future<bool> lift(
    WidgetTester tester,
    LogicalKeyboardKey key, {
    String platform = 'windows',
  }) => tester.sendKeyUpEvent(key, platform: platform);

  testWidgets(
    'local mode: modifiers wait, chords and special keys take them along, '
    'and printable keys and composing belong to the input method',
    (tester) async {
      final h = await ViewerHarness.live(tester);
      final text = RemoteTextInput(input: h.input)..show();
      h.input.textInput = text;
      await picture(tester, h);
      h.sent();

      // A lone Shift, and Ctrl+Shift, switch this computer's input method:
      // nothing reaches the host, and the input method sees them.
      expect(await press(tester, LogicalKeyboardKey.shiftLeft), isFalse);
      await lift(tester, LogicalKeyboardKey.shiftLeft);
      await press(tester, LogicalKeyboardKey.controlLeft);
      await press(tester, LogicalKeyboardKey.shiftLeft);
      await lift(tester, LogicalKeyboardKey.shiftLeft);
      await lift(tester, LogicalKeyboardKey.controlLeft);
      expect(h.sent(), isEmpty);

      // A capital goes to the input method with Shift, and its text comes
      // back through the client; Shift never reached the host.
      await press(tester, LogicalKeyboardKey.shiftLeft);
      expect(
        await press(tester, LogicalKeyboardKey.keyA, character: 'A'),
        isFalse,
      );
      text.updateEditingValue(
        TextEditingValue(
          text: '${'1' * 64}A',
          selection: const TextSelection.collapsed(offset: 65),
        ),
      );
      await lift(tester, LogicalKeyboardKey.keyA);
      await lift(tester, LogicalKeyboardKey.shiftLeft);
      expect(h.sent(), [
        ['text "A"'],
      ]);

      // Ctrl+C: Ctrl goes first, with the C, and both come up.
      await press(tester, LogicalKeyboardKey.controlLeft);
      expect(await press(tester, LogicalKeyboardKey.keyC), isTrue);
      await lift(tester, LogicalKeyboardKey.keyC);
      await lift(tester, LogicalKeyboardKey.controlLeft);
      expect(h.sent(), [
        ['control↓', 'c↓'],
        ['c↑'],
        ['control↑'],
      ]);

      // Ctrl+/ is not in an older host's vocabulary: it stays here.
      await press(tester, LogicalKeyboardKey.controlLeft);
      await press(tester, LogicalKeyboardKey.slash);
      await lift(tester, LogicalKeyboardKey.slash);
      await lift(tester, LogicalKeyboardKey.controlLeft);
      expect(h.sent(), isEmpty);

      // Shift and a click: Shift goes first, and comes up on its own.
      await press(tester, LogicalKeyboardKey.shiftLeft);
      h.input.click(RemoteButton.left, const Offset(0.5, 0.5));
      await lift(tester, LogicalKeyboardKey.shiftLeft);
      expect(h.sent(), [
        ['move 960,540', 'shift↓', 'left↓', 'left↑'],
        ['shift↑'],
      ]);

      // Shift+Enter: a special key takes the modifiers along.
      await press(tester, LogicalKeyboardKey.shiftLeft);
      expect(await press(tester, LogicalKeyboardKey.enter), isTrue);
      await lift(tester, LogicalKeyboardKey.enter);
      await lift(tester, LogicalKeyboardKey.shiftLeft);
      expect(h.sent(), [
        ['shift↓', 'enter↓'],
        ['enter↑'],
        ['shift↑'],
      ]);

      // While the input method composes, Enter, Backspace, the arrows and
      // Escape are its own.
      text.updateEditingValue(
        TextEditingValue(
          text: '${'1' * 64}Ani',
          selection: const TextSelection.collapsed(offset: 67),
          composing: const TextRange(start: 65, end: 67),
        ),
      );
      for (final key in [
        LogicalKeyboardKey.enter,
        LogicalKeyboardKey.backspace,
        LogicalKeyboardKey.arrowLeft,
        LogicalKeyboardKey.escape,
      ]) {
        expect(await press(tester, key), isFalse);
        await lift(tester, key);
      }
      expect(h.sent(), isEmpty);

      text.dispose();
      h.dispose();
    },
  );

  testWidgets(
    'a modifier waiting needs no release when focus goes; one sent does, '
    'and one pressed while a button is held goes at once',
    (tester) async {
      final h = await ViewerHarness.live(tester);
      await picture(tester, h);
      h.sent();

      await press(tester, LogicalKeyboardKey.shiftLeft);
      h.input.releaseAll();
      await lift(tester, LogicalKeyboardKey.shiftLeft);
      expect(h.sent(), isEmpty);
      expect(h.releases, 0);

      h.input.press(RemoteButton.left, const Offset(0.5, 0.5));
      await press(tester, LogicalKeyboardKey.shiftLeft);
      expect(h.sent(), [
        ['move 960,540', 'left↓'],
        ['shift↓'],
      ]);
      h.input.releaseAll();
      expect(h.releases, 1);
      await lift(tester, LogicalKeyboardKey.shiftLeft);
      expect(h.sent(), isEmpty);

      h.dispose();
    },
  );

  testWidgets(
    'remote mode: keys go by place to a host that takes them, Shift at once '
    'and Win with what it modifies; an older host gets keys and text',
    (tester) async {
      var h = await ViewerHarness.live(
        tester,
        capabilities: const ['os_windows', 'physical_keys'],
      );
      h.input.keyboard = const KeyboardSettings(mode: KeyboardMode.remote);
      await picture(tester, h);
      h.sent();

      // Shift at once, for the host's input method; the A by its place.
      expect(await press(tester, LogicalKeyboardKey.shiftLeft), isTrue);
      await press(tester, LogicalKeyboardKey.keyA, character: 'A');
      await lift(tester, LogicalKeyboardKey.keyA);
      await lift(tester, LogicalKeyboardKey.shiftLeft);
      expect(h.sent(), [
        ['#e1↓'],
        ['#4↓'],
        ['#4↑'],
        ['#e1↑'],
      ]);

      // A lone Win stays here; with a key it goes first.
      await press(tester, LogicalKeyboardKey.metaLeft);
      await lift(tester, LogicalKeyboardKey.metaLeft);
      expect(h.sent(), isEmpty);
      await press(tester, LogicalKeyboardKey.metaLeft);
      await press(tester, LogicalKeyboardKey.keyE);
      await lift(tester, LogicalKeyboardKey.keyE);
      await lift(tester, LogicalKeyboardKey.metaLeft);
      expect(h.sent(), [
        ['#e3↓', '#8↓'],
        ['#8↑'],
        ['#e3↑'],
      ]);
      h.dispose();

      // A host without physical keys: its vocabulary as keys, the rest as
      // text, which no host input method composes.
      h = await ViewerHarness.live(tester, capabilities: const ['os_windows']);
      h.input.keyboard = const KeyboardSettings(mode: KeyboardMode.remote);
      await picture(tester, h);
      h.sent();
      await press(tester, LogicalKeyboardKey.shiftLeft);
      await press(tester, LogicalKeyboardKey.keyN, character: 'N');
      await lift(tester, LogicalKeyboardKey.keyN);
      await press(tester, LogicalKeyboardKey.digit1, character: '!');
      await lift(tester, LogicalKeyboardKey.digit1);
      await press(tester, LogicalKeyboardKey.slash, character: '?');
      await lift(tester, LogicalKeyboardKey.slash);
      await lift(tester, LogicalKeyboardKey.shiftLeft);
      expect(h.sent(), [
        ['shift↓'],
        ['n↓'],
        ['n↑'],
        ['digit1↓'],
        ['digit1↑'],
        ['text "?"'],
        ['shift↑'],
      ]);
      h.dispose();
    },
  );

  testWidgets(
    'Cmd on a Mac is the host’s Ctrl, and a menu chord names the host’s keys',
    (tester) async {
      debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
      try {
        final h = await ViewerHarness.live(
          tester,
          capabilities: const ['os_windows'],
        );
        await picture(tester, h);
        h.sent();
        expect(h.input.swapsModifiers, isTrue);
        await press(tester, LogicalKeyboardKey.metaLeft, platform: 'macos');
        await press(
          tester,
          LogicalKeyboardKey.keyC,
          character: 'c',
          platform: 'macos',
        );
        await lift(tester, LogicalKeyboardKey.keyC, platform: 'macos');
        await lift(tester, LogicalKeyboardKey.metaLeft, platform: 'macos');
        h.input.shortcut(RemoteShortcut.switchInputMethod);
        expect(h.sent(), [
          ['control↓', 'c↓'],
          ['c↑'],
          ['control↑'],
          ['meta↓', 'space↓', 'space↑', 'meta↑'],
        ]);

        // Turned off for this computer, Cmd is the Windows key again.
        h.input.keyboard = const KeyboardSettings(swap: false);
        expect(h.input.swapsModifiers, isFalse);
        h.dispose();
      } finally {
        debugDefaultTargetPlatformOverride = null;
      }
    },
  );

  testWidgets(
    'the Shortcuts menu gives a Mac host its own chords and leaves out the '
    'ones it has none for',
    (tester) async {
      final l10n = AppLocalizationsEn();
      final prefs = ViewerPrefs(persist: false);
      Map<String, ViewerMenuItem> menu(ViewerHarness h) => {
        for (final item in shortcutsMenu(
          l10n,
          ViewerActions(
            desktop: h.store,
            input: h.input,
            prefs: prefs,
            viewport: h.viewport,
            onDisconnect: () async {},
            onQuality: (_) {},
            onResolution: (_) {},
            onMaxFps: (_) {},
            onTypeText: () {},
            onDisplayMode: (_) {},
          ),
        ).whereType<ViewerMenuItem>())
          (item.key as ValueKey<String>).value: item,
      };
      const chords = [
        'shortcut-show-desktop',
        'shortcut-switch-window',
        'shortcut-task-manager',
        'shortcut-start',
        'shortcut-input-method',
      ];

      // A host that names no system keeps the Windows chords.
      final windows = await ViewerHarness.live(tester);
      var items = menu(windows);
      for (final key in chords) {
        items[key]!.onSelected();
      }
      expect(windows.sent(), [
        ['meta↓', 'd↓', 'd↑', 'meta↑'],
        ['alt↓', 'tab↓', 'tab↑', 'alt↑'],
        ['control↓', 'shift↓', 'escape↓', 'escape↑', 'shift↑', 'control↑'],
        ['meta↓', 'meta↑'],
        ['meta↓', 'space↓', 'space↑', 'meta↑'],
      ]);
      expect(
        [for (final key in chords) items[key]!.hint],
        ['Win + D', 'Alt + Tab', 'Ctrl + Shift + Esc', 'Win', 'Win + Space'],
      );
      expect(items['shortcut-cad']!.label, 'Ctrl + Alt + Del');
      windows.dispose();

      final mac = await ViewerHarness.live(
        tester,
        capabilities: const ['os_macos'],
      );
      items = menu(mac);
      expect(items.keys, isNot(contains('shortcut-show-desktop')));
      expect(items.keys, isNot(contains('shortcut-cad')));
      for (final key in chords.skip(1)) {
        items[key]!.onSelected();
      }
      expect(mac.sent(), [
        ['meta↓', 'tab↓', 'tab↑', 'meta↑'],
        ['alt↓', 'meta↓', 'escape↓', 'escape↑', 'meta↑', 'alt↑'],
        ['meta↓', 'space↓', 'space↑', 'meta↑'],
        ['control↓', 'space↓', 'space↑', 'control↑'],
      ]);
      expect(
        [
          for (final key in [...chords.skip(1), 'shortcut-lock'])
            '${items[key]!.label} ${items[key]!.hint}',
        ],
        [
          'Switch app ⌘Tab',
          'Force Quit ⌥⌘Esc',
          'Spotlight ⌘Space',
          'Switch input method ⌃Space',
          'Lock remote computer ⌃⌘Q',
        ],
      );
      mac.dispose();
    },
  );

  test('AltGr arrives as Control + Alt and types a character of its own', () {
    final altGr = KeyDownEvent(
      physicalKey: PhysicalKeyboardKey.keyQ,
      logicalKey: LogicalKeyboardKey.keyQ,
      character: '@',
      timeStamp: Duration.zero,
    );
    const held = {RemoteKey.control, RemoteKey.alt};
    // To the input method while a client is attached, else as text.
    expect(
      KeyRouting.route(
        altGr,
        const KeyContext(textInput: true, heldHere: held),
      ).handled,
      isFalse,
    );
    final typed = KeyRouting.route(altGr, const KeyContext(heldHere: held));
    expect(typed.inputs, [DesktopInput.text('@')]);
    expect(typed.flush, isFalse);
  });

  test('with Ctrl and Cmd swapped, the host’s Win waits, and a latch is '
      'compared with what the host holds', () {
    // From a Mac in remote mode: its Ctrl is the host's Win, which waits;
    // its Cmd the host's Ctrl, which goes at once.
    const remote = KeyContext(
      mode: KeyboardMode.remote,
      physicalKeys: true,
      swap: true,
    );
    final ctrl = KeyRouting.route(
      _down(LogicalKeyboardKey.controlLeft, PhysicalKeyboardKey.controlLeft),
      remote,
    );
    expect(ctrl.inputs, isEmpty);
    expect(ctrl.defers, const DesktopPhysicalKey(0xE3, true));
    final cmd = KeyRouting.route(
      _down(LogicalKeyboardKey.metaLeft, PhysicalKeyboardKey.metaLeft),
      remote,
    );
    expect(cmd.inputs, [const DesktopPhysicalKey(0xE0, true)]);
    expect(cmd.defers, isNull);

    // Cmd held here is the host's Ctrl: a latched Ctrl is not wrapped
    // around the key again, and a latched Win is.
    final tab = _down(LogicalKeyboardKey.tab, PhysicalKeyboardKey.tab);
    List<DesktopInput> withLatched(RemoteKey latched) => KeyRouting.route(
      tab,
      KeyContext(
        swap: true,
        latched: {latched},
        heldHere: const {RemoteKey.meta},
      ),
    ).inputs;
    expect(withLatched(RemoteKey.control), [
      const DesktopKey(RemoteKey.tab, true),
    ]);
    expect(
      withLatched(RemoteKey.meta),
      KeyRouting.tap(RemoteKey.tab, const {RemoteKey.meta}),
    );
  });

  testWidgets(
    'a modifier that goes at once takes the one waiting ahead of it, and an '
    'on-screen key takes the modifiers held here',
    (tester) async {
      final h = await ViewerHarness.live(
        tester,
        capabilities: const ['os_windows', 'physical_keys'],
      );
      h.input.keyboard = const KeyboardSettings(mode: KeyboardMode.remote);
      await picture(tester, h);
      h.sent();

      // Win+Shift+S: Win waits, then goes ahead of the Shift that goes at
      // once.
      await press(tester, LogicalKeyboardKey.metaLeft);
      await press(tester, LogicalKeyboardKey.shiftLeft);
      await press(tester, LogicalKeyboardKey.keyS, character: 'S');
      await lift(tester, LogicalKeyboardKey.keyS);
      await lift(tester, LogicalKeyboardKey.shiftLeft);
      await lift(tester, LogicalKeyboardKey.metaLeft);
      expect(h.sent(), [
        ['#e3↓', '#e1↓'],
        ['#16↓'],
        ['#16↑'],
        ['#e1↑'],
        ['#e3↑'],
      ]);

      // Shift held here and Tab from the modifier bar: Shift+Tab.
      h.input.keyboard = const KeyboardSettings();
      await press(tester, LogicalKeyboardKey.shiftLeft);
      h.input.tapKey(RemoteKey.tab);
      await lift(tester, LogicalKeyboardKey.shiftLeft);
      expect(h.sent(), [
        ['shift↓', 'tab↓', 'tab↑'],
        ['shift↑'],
      ]);
      h.dispose();
    },
  );

  testWidgets(
    "a desktop viewer's input method composes in local mode, and in remote "
    'mode is off only while the picture has the keyboard',
    (tester) async {
      final h = await ViewerHarness.live(tester);
      final node = await picture(tester, h);
      final ime = LocalInputMethod();
      final keyboard = DesktopKeyboard(input: h.input, picture: node, ime: ime)
        ..sync();
      expect((keyboard.text.attached.value, ime.enabled), (true, true));
      // The client names its view: the Windows embedder refuses one that
      // does not ("view ID is null"), and typed text is then lost.
      expect(tester.testTextInput.setClientArgs?['viewId'], tester.view.viewId);

      h.input.keyboard = const KeyboardSettings(mode: KeyboardMode.remote);
      keyboard.sync();
      expect((keyboard.text.attached.value, ime.enabled), (false, false));
      // A menu or a dialog takes the keyboard: the input method is back.
      node.unfocus();
      await tester.pump();
      expect(ime.enabled, isTrue);
      node.requestFocus();
      await tester.pump();
      expect(ime.enabled, isFalse);
      // The viewer closes.
      keyboard.dispose();
      expect(ime.enabled, isTrue);
      h.dispose();
    },
  );
}
