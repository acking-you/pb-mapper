import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import '../../../state/desktop_store.dart';
import '../../../state/viewer_prefs.dart';
import 'modifier_latch.dart';
import 'remote_keys.dart';
import 'remote_text_input.dart';
import 'touch_gestures.dart';

/// Wheel notches per event, each way.
const kWheelClamp = DesktopWheel.maxNotches;

/// A held key of the modifier bar repeats after this, at this interval.
const kKeyRepeatDelay = Duration(milliseconds: 400);
const kKeyRepeatInterval = Duration(milliseconds: 60);

/// UTF-8 bytes per `text` event.
const kTextChunkBytes = DesktopText.maxBytes;

/// While a latch is active, [RemoteInput.typeText] keys at most this many
/// characters, each a wrapped tap of at most 10 events; the rest of the
/// call goes as `text`. The host's input queue is sized on this bound
/// (perf §6.4).
const kLatchedTextKeys = 16;

/// UTF-8 bytes per [RemoteInput.typeText] call: at most 64 `text` events.
const kTextMaxBytes = 64 * 1024;

/// Code points per [RemoteInput.backspace] call: 128 events in 8 batches.
const _backspaceMax = 64;

/// Whole backspace taps per sequence.
const _backspacesPerSequence = kSequenceMax ~/ 2;

/// The viewer's only door to the store's input (ui §10.4). Every method is
/// a no-op unless [ready], except [releaseAll] and the `refresh` and
/// `releaseKeys` shortcuts.
///
/// Whatever carries modifiers goes as one atomic sequence wrapped in their
/// downs and ups, and a sequence the store refuses is released whole, so
/// nothing can stay held on the host (D16). What is held across sequences
/// — a pressed button, a three-finger scroll's modifiers, a repeating key,
/// a physical key — is tracked, and [releaseAll] gives it back.
///
/// The cursor it moves is the session's ([DesktopStore.cursor], D12), not
/// this viewer's: strokes, taps and the mouse move it, and a viewer opened
/// again resumes it.
final class RemoteInput implements RemotePointerSink {
  /// [onLocalCancel] cancels the local gesture in progress (the touch
  /// machine's `cancel`). [clock] is the viewer's one timeline for events
  /// and timeouts.
  RemoteInput({
    required DesktopStore desktop,
    required ModifierLatch latch,
    required void Function() onLocalCancel,
    Duration Function()? clock,
  }) : _desktop = desktop,
       _latch = latch,
       _onLocalCancel = onLocalCancel,
       clock = clock ?? _stopwatch() {
    _desktop.session.addListener(_onState);
    _desktop.geometry.addListener(_onState);
    _epoch = _desktop.geometry.value?.epoch;
    _ready.value = _live != null;
  }

  static Duration Function() _stopwatch() {
    final watch = Stopwatch()..start();
    return () => watch.elapsed;
  }

  final DesktopStore _desktop;
  final ModifierLatch _latch;
  final void Function() _onLocalCancel;
  final Duration Function() clock;

  final _ready = ValueNotifier(false);
  int? _epoch;

  /// Physical keys whose down is held on the host, and those whose down
  /// went as a whole tap, so their up sends nothing.
  final _heldKeys = <PhysicalKeyboardKey, DesktopKeyInput>{};
  final _swallowUps = <PhysicalKeyboardKey>{};

  /// Modifiers down here whose down waits for the next key, button or
  /// wheel, in the order they went down.
  final _waiting = <PhysicalKeyboardKey, DesktopKeyInput>{};

  /// How keys reach this computer; the viewer keeps it current.
  KeyboardSettings keyboard = const KeyboardSettings();

  /// The text-input client this viewer attaches, if any: while it is
  /// attached in local mode, printable keys go to this device's input
  /// method, and its text comes back through the client.
  RemoteTextInput? textInput;

  /// Whether this device's keyboard is an Apple one: Cmd, and Option that
  /// types characters.
  static bool get appleViewer => const {
    TargetPlatform.macOS,
    TargetPlatform.iOS,
  }.contains(defaultTargetPlatform);

  /// The host is a Mac.
  bool get appleHost =>
      _desktop.session.value.capabilities.contains('os_macos');

  /// Ctrl and Cmd trade places on their way to the host.
  bool get swapsModifiers =>
      keyboard.swaps(appleViewer: appleViewer, appleHost: appleHost);

  final _heldButtons = <RemoteButton>{};

  /// Modifiers held down around pressed buttons, and around a wheel burst.
  final _buttonMods = <RemoteKey>{};
  final _wheelMods = <RemoteKey>{};

  RemoteKey? _repeatKey;
  Set<RemoteKey> _repeatMods = const {};
  Timer? _repeatTimer;

  /// Control is live and the picture's geometry known.
  ValueListenable<bool> get ready => _ready;

  /// Fires when the cursor moves, shows or hides.
  Listenable get cursorChanges => _desktop.cursor;

  /// Where taps click and strokes start, normalised to the display.
  @override
  Offset get cursor => _desktop.cursor.position;

  /// A local move: the arrow shows here.
  @override
  set cursor(Offset normalized) => _desktop.cursor.moveTo(normalized);

  /// Where the cursor layer draws the arrow; null while the host hides its
  /// cursor and this viewer has not moved it since.
  Offset? get shownCursor => _desktop.cursor.shown;

  // -------------------------------------------------------------- pointer

  @override
  void hover(Offset n) => _moveTo(n);

  @override
  void move(Offset n) => _moveTo(n);

  @override
  void press(RemoteButton b, Offset n, {bool dragStart = false}) {
    final g = _live;
    if (g == null) return;
    cursor = n;
    final mods = _latchedToWrap().difference(_buttonMods);
    final sent = _sequence(g, [
      _moveInput(g, n),
      ..._waitingDowns,
      ..._downs(mods),
      DesktopInput.button(b, true, dragStart: dragStart),
    ]);
    if (!sent) return;
    _sentWaiting();
    _latch.take();
    _heldButtons.add(b);
    _buttonMods.addAll(mods);
  }

  @override
  void release(RemoteButton b) {
    final g = _live;
    if (g == null || !_heldButtons.remove(b)) return;
    final mods = _heldButtons.isEmpty ? {..._buttonMods} : const <RemoteKey>{};
    if (_heldButtons.isEmpty) _buttonMods.clear();
    _sequence(g, [DesktopInput.button(b, false), ..._ups(mods)]);
  }

  @override
  void click(RemoteButton b, Offset n) {
    final g = _live;
    if (g == null) return;
    cursor = n;
    final waiting = _waitingDowns;
    _wrapped(
      g,
      const {},
      (mods) => [
        _moveInput(g, n),
        ...waiting,
        ..._downs(mods),
        DesktopInput.button(b, true),
        DesktopInput.button(b, false),
        ..._ups(mods),
      ],
      sent: _sentWaiting,
    );
  }

  @override
  void wheelStart() {
    final g = _live;
    final mods = _latchedToWrap();
    if (g == null || mods.isEmpty) return;
    if (_sequence(g, [..._waitingDowns, ..._downs(mods)])) {
      _sentWaiting();
      _latch.take();
      _wheelMods.addAll(mods);
    }
  }

  @override
  void wheel(int horizontal, int vertical) {
    final g = _live;
    if (g == null || (horizontal == 0 && vertical == 0)) return;
    final wheel = DesktopInput.wheel(
      horizontal.clamp(-kWheelClamp, kWheelClamp),
      vertical.clamp(-kWheelClamp, kWheelClamp),
    );
    if (_waiting.isEmpty) {
      _desktop.send(wheel, geometryEpoch: g.epoch);
    } else if (_sequence(g, [..._waitingDowns, wheel])) {
      _sentWaiting();
    }
  }

  @override
  void wheelEnd() {
    if (_wheelMods.isEmpty) return;
    final mods = {..._wheelMods};
    _wheelMods.clear();
    final g = _live;
    if (g != null) _sequence(g, _ups(mods));
  }

  // ----------------------------------------------------------------- keys

  /// The keyboard as [KeyRouting.route] sees it now.
  KeyContext get _keyContext {
    final text = textInput;
    final caps = _desktop.session.value.capabilities;
    return KeyContext(
      mode: keyboard.mode,
      textInput: text != null && text.attached.value,
      composing: text != null && text.composing.value.isNotEmpty,
      physicalKeys: caps.contains('physical_keys'),
      swap: swapsModifiers,
      optionTypes: appleViewer,
      latched: _latch.active,
      heldHere: _physicalModifiers(),
      othersDown:
          _heldButtons.isNotEmpty ||
          _heldKeys.keys.any((k) => !KeyRouting.modifierKeys.contains(k)),
    );
  }

  /// Hardware keys, bound to the picture's `Focus.onKeyEvent`; the rules
  /// are [KeyRouting.route]'s.
  KeyEventResult handleHardwareKey(FocusNode node, KeyEvent event) {
    final g = _live;
    if (g == null) return KeyEventResult.ignored;
    final physical = event.physicalKey;
    final route = KeyRouting.route(
      event,
      _keyContext,
      holding: _heldKeys[physical],
      upIsSwallowed: _swallowUps.contains(physical),
    );
    final result = route.handled
        ? KeyEventResult.handled
        : KeyEventResult.ignored;
    final up = event is KeyUpEvent;
    if (up) {
      _swallowUps.remove(physical);
      _heldKeys.remove(physical);
      _waiting.remove(physical);
    } else if (route.swallowUp) {
      _swallowUps.add(physical);
    } else {
      // A key that sends its own up must never have it swallowed.
      _swallowUps.remove(physical);
    }
    if (route.defers case final defers?) _waiting[physical] = defers;
    final waiting = route.flush ? _waitingDowns : const <DesktopInput>[];
    final inputs = [...waiting, ...route.inputs];
    final sent = switch (inputs.length) {
      0 => true,
      1 => _desktop.send(inputs.single, geometryEpoch: g.epoch),
      _ => _sequence(g, inputs),
    };
    if (!sent) return result;
    if (waiting.isNotEmpty) _sentWaiting();
    if (route.consumesLatch) _latch.take();
    final holds = route.holds;
    if (!up && holds != null) _heldKeys[physical] = holds;
    return result;
  }

  /// One tap of [key], after the modifiers waiting here and wrapped in the
  /// latched modifiers and [extra].
  void tapKey(RemoteKey key, {Set<RemoteKey> extra = const {}}) {
    final g = _live;
    if (g == null) return;
    final waiting = _waitingDowns;
    _wrapped(
      g,
      extra,
      (mods) => [...waiting, ...KeyRouting.tap(key, mods)],
      sent: _sentWaiting,
    );
  }

  /// Holds [key] down, after the modifiers waiting here and wrapped in the
  /// latched modifiers, repeating its down after [delay] every
  /// [kKeyRepeatInterval] until [releaseKey]. A key held past
  /// [kKeyRepeatDelay] before it was pressed repeats at once (the modifier
  /// bar, which sends a tap on release).
  void pressKey(RemoteKey key, {Duration delay = kKeyRepeatDelay}) {
    final g = _live;
    if (g == null) return;
    releaseKey();
    final mods = _latchedToWrap();
    final sent = _sequence(g, [
      ..._waitingDowns,
      ..._downs(mods),
      DesktopInput.key(key, true),
    ]);
    if (!sent) return;
    _sentWaiting();
    _latch.take();
    _repeatKey = key;
    _repeatMods = mods;
    _repeatTimer = Timer(delay, () {
      _repeatTimer = Timer.periodic(kKeyRepeatInterval, (_) {
        final g = _live;
        if (g != null) {
          _desktop.send(DesktopInput.key(key, true), geometryEpoch: g.epoch);
        }
      });
    });
  }

  void releaseKey() {
    final key = _repeatKey;
    if (key == null) return;
    final mods = _repeatMods;
    _stopRepeat();
    final g = _live;
    if (g != null) _sequence(g, [DesktopInput.key(key, false), ..._ups(mods)]);
  }

  /// A menu's chord: one sequence, and latches are neither applied nor
  /// consumed.
  void shortcut(RemoteShortcut s) {
    switch (s) {
      case RemoteShortcut.refresh:
        _desktop.requestKeyframe();
      case RemoteShortcut.releaseKeys:
        releaseAll();
      case RemoteShortcut.ctrlAltDel:
        break;
      case RemoteShortcut.lock:
        if (_live != null) _desktop.lockRemote();
      case RemoteShortcut.showDesktop ||
          RemoteShortcut.switchWindow ||
          RemoteShortcut.taskManager ||
          RemoteShortcut.startMenu ||
          RemoteShortcut.switchInputMethod:
        final g = _live;
        if (g == null) return;
        final chord = s.chord(appleHost: appleHost);
        _sequence(g, [
          for (final k in chord) DesktopInput.key(k, true),
          for (final k in chord.reversed) DesktopInput.key(k, false),
        ]);
    }
  }

  /// Committed text. With no latch active it goes as `text` events of at
  /// most [kTextChunkBytes]. While one is, each of the first
  /// [kLatchedTextKeys] characters that has a key goes as a wrapped tap
  /// (Shift added for capitals); a character without a key, and everything
  /// after them, goes as `text` and consumes the armed latches. At most
  /// [kTextMaxBytes] per call, cut at a code point.
  void typeText(String text) {
    final g = _live;
    if (g == null || text.isEmpty) return;
    final pending = StringBuffer();
    var bytes = 0, index = 0;
    for (final rune in text.runes) {
      bytes += _utf8Length(rune);
      if (bytes > kTextMaxBytes) break;
      final c = String.fromCharCode(rune);
      final key = index < kLatchedTextKeys && !_latch.isEmpty
          ? KeyRouting.forChar(c)
          : null;
      index++;
      if (key != null) {
        _sendText(g, pending);
        _wrapped(g, {
          if (key.$2) RemoteKey.shift,
        }, (mods) => KeyRouting.tap(key.$1, mods));
      } else {
        pending.write(c);
        // A latch cannot apply to text, and must not linger.
        if (!_latch.isEmpty) _latch.take();
      }
    }
    _sendText(g, pending);
  }

  /// Deletes [count] characters before the host's caret, at most 64 per
  /// call. A single one is a Backspace key and takes the latch; more are
  /// the IME's own editing, which goes as bare taps, eight to a sequence,
  /// so the burst stays within the bound the host's queue is sized on
  /// (perf §6.4: 128 events).
  void backspace(int count) {
    final g = _live;
    if (g == null || count <= 0) return;
    if (count == 1) {
      tapKey(RemoteKey.backspace);
      return;
    }
    _latch.take();
    final total = math.min(count, _backspaceMax);
    for (var sent = 0; sent < total; sent += _backspacesPerSequence) {
      final taps = math.min(_backspacesPerSequence, total - sent);
      final batch = [
        for (var i = 0; i < taps; i++) ...[
          const DesktopInput.key(RemoteKey.backspace, true),
          const DesktopInput.key(RemoteKey.backspace, false),
        ],
      ];
      if (!_sequence(g, batch)) return;
    }
  }

  /// Focus loss, the app going inactive or hidden, leaving the picture:
  /// the local gesture is cancelled, latches clear, and the host releases
  /// whatever this viewer holds there, if anything.
  void releaseAll() {
    final held =
        _heldKeys.isNotEmpty ||
        _heldButtons.isNotEmpty ||
        _buttonMods.isNotEmpty ||
        _wheelMods.isNotEmpty ||
        _repeatKey != null;
    _clearHeld();
    // A modifier still waiting was never sent: nothing to release.
    _waiting.clear();
    _onLocalCancel();
    _latch.clear();
    if (held) _desktop.release();
  }

  /// Dispose before the touch machine: this releases through it.
  void dispose() {
    releaseAll();
    _desktop.session.removeListener(_onState);
    _desktop.geometry.removeListener(_onState);
    _ready.dispose();
  }

  // ---------------------------------------------------------------- inner

  /// The geometry input is stamped with, while input is live.
  DesktopGeometry? get _live {
    final g = _desktop.geometry.value;
    return g != null && _desktop.session.value.inputLive ? g : null;
  }

  /// Losing control or the picture, or a new geometry epoch: the host has
  /// released everything already, so the local state only clears.
  void _onState() {
    final g = _desktop.geometry.value;
    final live = _live != null;
    final newEpoch = g != null && _epoch != null && g.epoch != _epoch;
    if (g != null) _epoch = g.epoch;
    final lost = _ready.value && !live;
    _ready.value = live;
    if (lost || newEpoch) {
      _clearHeld();
      _waiting.clear();
      _onLocalCancel();
      _latch.clear();
    }
  }

  /// The downs of the modifiers waiting, in the order they went down.
  List<DesktopInput> get _waitingDowns => [..._waiting.values];

  /// The modifiers waiting went to the host: held there now, until their
  /// keys go up.
  void _sentWaiting() {
    _heldKeys.addAll(_waiting);
    _waiting.clear();
  }

  void _clearHeld() {
    _heldKeys.clear();
    _swallowUps.clear();
    _heldButtons.clear();
    _buttonMods.clear();
    _wheelMods.clear();
    _stopRepeat();
  }

  void _stopRepeat() {
    _repeatTimer?.cancel();
    _repeatTimer = null;
    _repeatKey = null;
    _repeatMods = const {};
  }

  void _moveTo(Offset n) {
    final g = _live;
    if (g == null) return;
    cursor = n;
    _desktop.send(_moveInput(g, n), geometryEpoch: g.epoch);
  }

  /// A refused sequence was released whole on the host, together with
  /// everything else held there, so nothing is held here either.
  bool _sequence(DesktopGeometry g, List<DesktopInput> inputs) {
    if (_desktop.sendSequence(inputs, geometryEpoch: g.epoch)) return true;
    _clearHeld();
    return false;
  }

  /// Sends what [build] makes of the latched modifiers and [extra]; the
  /// latch is taken, and [sent] runs, only once it was sent.
  void _wrapped(
    DesktopGeometry g,
    Set<RemoteKey> extra,
    List<DesktopInput> Function(Set<RemoteKey> mods) build, {
    VoidCallback? sent,
  }) {
    if (_sequence(g, build(_latchedToWrap(extra)))) {
      sent?.call();
      _latch.take();
    }
  }

  /// The latched modifiers and [extra] that wrap an action, as the host
  /// names them: not those a key held here holds on the host, whose wrapped
  /// up would release the key still held (KeyRouting.route).
  Set<RemoteKey> _latchedToWrap([Set<RemoteKey> extra = const {}]) =>
      {..._latch.active, ...extra}.difference(
        KeyRouting.hostModifiers(_physicalModifiers(), swap: swapsModifiers),
      );

  void _sendText(DesktopGeometry g, StringBuffer pending) {
    if (pending.isEmpty) return;
    final text = pending.toString();
    pending.clear();
    final chunk = StringBuffer();
    var bytes = 0;
    for (final rune in text.runes) {
      final length = _utf8Length(rune);
      if (bytes + length > kTextChunkBytes) {
        _desktop.send(
          DesktopInput.text(chunk.toString()),
          geometryEpoch: g.epoch,
        );
        chunk.clear();
        bytes = 0;
      }
      chunk.writeCharCode(rune);
      bytes += length;
    }
    if (chunk.isNotEmpty) {
      _desktop.send(
        DesktopInput.text(chunk.toString()),
        geometryEpoch: g.epoch,
      );
    }
  }

  /// Normalised to display pixels of the current geometry.
  static DesktopInput _moveInput(DesktopGeometry g, Offset n) {
    final w = g.displayWidth, h = g.displayHeight;
    return DesktopInput.move(
      (n.dx * w).floor().clamp(0, math.max(0, w - 1)),
      (n.dy * h).floor().clamp(0, math.max(0, h - 1)),
    );
  }

  static List<DesktopInput> _downs(Set<RemoteKey> mods) => [
    for (final m in ModifierLatch.order)
      if (mods.contains(m)) DesktopInput.key(m, true),
  ];

  static List<DesktopInput> _ups(Set<RemoteKey> mods) => [
    for (final m in ModifierLatch.order.reversed)
      if (mods.contains(m)) DesktopInput.key(m, false),
  ];

  static Set<RemoteKey> _physicalModifiers() {
    final keyboard = HardwareKeyboard.instance;
    return {
      if (keyboard.isControlPressed) RemoteKey.control,
      if (keyboard.isAltPressed) RemoteKey.alt,
      if (keyboard.isShiftPressed) RemoteKey.shift,
      if (keyboard.isMetaPressed) RemoteKey.meta,
    };
  }

  static int _utf8Length(int rune) => rune < 0x80
      ? 1
      : rune < 0x800
      ? 2
      : rune < 0x10000
      ? 3
      : 4;
}
