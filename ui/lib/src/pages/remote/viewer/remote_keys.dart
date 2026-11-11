import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

import '../../../state/desktop_input.dart';
import '../../../state/viewer_prefs.dart';
import 'modifier_latch.dart';

/// The chords of the Shortcuts menu. Each goes in one atomic sequence, and
/// none of them consumes or applies a latch.
enum RemoteShortcut {
  /// Win + D.
  showDesktop,

  /// Alt + Tab.
  switchWindow,

  /// Ctrl + Shift + Esc.
  taskManager,

  /// Win.
  startMenu,

  /// The host's next input method: Win + Space on Windows, Ctrl + Space on
  /// a Mac, where this computer's system would take that chord itself.
  switchInputMethod,

  /// Win + L, which is the host's own `lock` command (D17), not keys.
  lock,

  /// Never enabled: the secure attention sequence cannot be injected, and
  /// the menu says so.
  ctrlAltDel,

  /// Asks the host for a fresh picture.
  refresh,

  /// Releases every key and button this viewer holds on the host.
  releaseKeys;

  /// The keys a chord presses on the host, in order; released in reverse.
  /// Empty for the entries that are not chords. They name the host's keys,
  /// so no modifier swap applies.
  List<RemoteKey> chord({bool appleHost = false}) => switch (this) {
    showDesktop => const [RemoteKey.meta, RemoteKey.d],
    switchWindow => const [RemoteKey.alt, RemoteKey.tab],
    taskManager => const [RemoteKey.control, RemoteKey.shift, RemoteKey.escape],
    startMenu => const [RemoteKey.meta],
    switchInputMethod =>
      appleHost
          ? const [RemoteKey.control, RemoteKey.space]
          : const [RemoteKey.meta, RemoteKey.space],
    lock || ctrlAltDel || refresh || releaseKeys => const [],
  };
}

/// What one hardware key event does ([KeyRouting.route]).
@immutable
class KeyRoute {
  const KeyRoute({
    this.inputs = const [],
    this.consumesLatch = false,
    this.swallowUp = false,
    this.holds,
    this.defers,
    this.flush = false,
    this.handled = true,
  });

  /// Nothing is sent, and the event goes on to this device's input method
  /// and text input.
  const KeyRoute.ignored() : this(handled: false);

  /// [KeyEventResult.handled], else ignored.
  final bool handled;

  /// Nothing to send when empty; more than one go in one sequence.
  final List<DesktopInput> inputs;

  /// The caller takes the latch: armed modifiers turn off.
  final bool consumesLatch;

  /// The down was sent as a whole tap, so its key-up sends nothing.
  final bool swallowUp;

  /// The key a down holds on the host, or an up releases.
  final DesktopKeyInput? holds;

  /// A modifier's down that waits: it goes first with the next key, button
  /// or wheel sent while it is held, and alone it sends nothing.
  final DesktopKeyInput? defers;

  /// The modifiers waiting go before [inputs].
  final bool flush;
}

/// What [KeyRouting.route] knows besides the event.
@immutable
class KeyContext {
  const KeyContext({
    this.mode = KeyboardMode.local,
    this.textInput = false,
    this.composing = false,
    this.physicalKeys = false,
    this.swap = false,
    this.optionTypes = false,
    this.latched = const {},
    this.held = const {},
    this.othersDown = false,
  });

  final KeyboardMode mode;

  /// A text-input client is attached (local mode): what this device's
  /// input method commits reaches the host through it.
  final bool textInput;

  /// That input method is composing.
  final bool composing;

  /// The host listed `physical_keys`.
  final bool physicalKeys;

  /// Ctrl and Cmd (the Windows key) trade places on their way to the host.
  final bool swap;

  /// Option, with a printable key and no other modifier, types a character
  /// here (an Apple keyboard): it makes no chord.
  final bool optionTypes;

  /// The modifier bar's active set.
  final Set<RemoteKey> latched;

  /// The modifiers physically down here, as this keyboard names them.
  final Set<RemoteKey> held;

  /// A button or a key other than a modifier is held on the host: a
  /// modifier pressed now goes at once.
  final bool othersDown;
}

/// How hardware keys reach the host (ui §10.5.1). Pure: the caller supplies
/// the keyboard's context and what the event's physical key did on its way
/// down.
///
/// In local mode this device's input method composes. Modifiers wait: one
/// goes to the host only with the next key, button or wheel while it is
/// held, so a lone Shift, Ctrl+Shift, Alt+Shift or Win stays here to switch
/// this device's input method or layout. A chord (Ctrl, Alt or Win held)
/// and the special keys go as keys; while the input method composes every
/// key is its own; anything printable goes to it, and its text reaches the
/// host as text through the text-input client.
///
/// In remote mode keys go by their place to a host that listed
/// `physical_keys`, so its layout and input method type them; Shift, Ctrl
/// and Alt go at once, since the host's input method needs their taps, and
/// only Win waits. An older host gets its vocabulary as keys and the rest
/// as text.
abstract final class KeyRouting {
  static final _modifiers = <LogicalKeyboardKey, RemoteKey>{
    LogicalKeyboardKey.shift: RemoteKey.shift,
    LogicalKeyboardKey.shiftLeft: RemoteKey.shift,
    LogicalKeyboardKey.shiftRight: RemoteKey.shift,
    LogicalKeyboardKey.control: RemoteKey.control,
    LogicalKeyboardKey.controlLeft: RemoteKey.control,
    LogicalKeyboardKey.controlRight: RemoteKey.control,
    LogicalKeyboardKey.alt: RemoteKey.alt,
    LogicalKeyboardKey.altLeft: RemoteKey.alt,
    LogicalKeyboardKey.altRight: RemoteKey.alt,
    LogicalKeyboardKey.meta: RemoteKey.meta,
    LogicalKeyboardKey.metaLeft: RemoteKey.meta,
    LogicalKeyboardKey.metaRight: RemoteKey.meta,
  };

  /// The physical modifiers, left and right.
  static final modifierKeys = <PhysicalKeyboardKey>{
    PhysicalKeyboardKey.shiftLeft,
    PhysicalKeyboardKey.shiftRight,
    PhysicalKeyboardKey.controlLeft,
    PhysicalKeyboardKey.controlRight,
    PhysicalKeyboardKey.altLeft,
    PhysicalKeyboardKey.altRight,
    PhysicalKeyboardKey.metaLeft,
    PhysicalKeyboardKey.metaRight,
  };

  static final _specials = <LogicalKeyboardKey, RemoteKey>{
    LogicalKeyboardKey.f1: RemoteKey.f1,
    LogicalKeyboardKey.f2: RemoteKey.f2,
    LogicalKeyboardKey.f3: RemoteKey.f3,
    LogicalKeyboardKey.f4: RemoteKey.f4,
    LogicalKeyboardKey.f5: RemoteKey.f5,
    LogicalKeyboardKey.f6: RemoteKey.f6,
    LogicalKeyboardKey.f7: RemoteKey.f7,
    LogicalKeyboardKey.f8: RemoteKey.f8,
    LogicalKeyboardKey.f9: RemoteKey.f9,
    LogicalKeyboardKey.f10: RemoteKey.f10,
    LogicalKeyboardKey.f11: RemoteKey.f11,
    LogicalKeyboardKey.f12: RemoteKey.f12,
    LogicalKeyboardKey.enter: RemoteKey.enter,
    LogicalKeyboardKey.numpadEnter: RemoteKey.enter,
    LogicalKeyboardKey.escape: RemoteKey.escape,
    LogicalKeyboardKey.tab: RemoteKey.tab,
    LogicalKeyboardKey.backspace: RemoteKey.backspace,
    LogicalKeyboardKey.delete: RemoteKey.delete,
    LogicalKeyboardKey.insert: RemoteKey.insert,
    LogicalKeyboardKey.home: RemoteKey.home,
    LogicalKeyboardKey.end: RemoteKey.end,
    LogicalKeyboardKey.pageUp: RemoteKey.pageUp,
    LogicalKeyboardKey.pageDown: RemoteKey.pageDown,
    LogicalKeyboardKey.arrowLeft: RemoteKey.left,
    LogicalKeyboardKey.arrowRight: RemoteKey.right,
    LogicalKeyboardKey.arrowUp: RemoteKey.up,
    LogicalKeyboardKey.arrowDown: RemoteKey.down,
  };

  /// Keys the vocabulary cannot name that command rather than type: sent
  /// by place in local mode too, to a host that takes physical keys. Locks
  /// and input-method keys are not among them: in local mode they belong
  /// to this device.
  static final _commands = <PhysicalKeyboardKey>{
    PhysicalKeyboardKey.printScreen,
    PhysicalKeyboardKey.pause,
    PhysicalKeyboardKey.contextMenu,
    PhysicalKeyboardKey.f13,
    PhysicalKeyboardKey.f14,
    PhysicalKeyboardKey.f15,
    PhysicalKeyboardKey.f16,
    PhysicalKeyboardKey.f17,
    PhysicalKeyboardKey.f18,
    PhysicalKeyboardKey.f19,
    PhysicalKeyboardKey.f20,
    PhysicalKeyboardKey.f21,
    PhysicalKeyboardKey.f22,
    PhysicalKeyboardKey.f23,
    PhysicalKeyboardKey.f24,
  };

  /// Letters, digits and space, with the character each types unshifted.
  static final _bases = <LogicalKeyboardKey, (RemoteKey, String)>{
    for (var i = 0; i < 26; i++)
      LogicalKeyboardKey(LogicalKeyboardKey.keyA.keyId + i): (
        RemoteKey.values[RemoteKey.a.index + i],
        String.fromCharCode(0x61 + i),
      ),
    for (var i = 0; i < 10; i++)
      LogicalKeyboardKey(LogicalKeyboardKey.digit0.keyId + i): (
        RemoteKey.values[RemoteKey.digit0.index + i],
        String.fromCharCode(0x30 + i),
      ),
    LogicalKeyboardKey.space: (RemoteKey.space, ' '),
  };

  /// Shift, Control, Alt or Meta, left or right.
  static RemoteKey? modifier(LogicalKeyboardKey k) => _modifiers[k];

  /// F1–F12, Enter, Escape, Tab, Backspace, Delete, Insert, Home, End,
  /// PageUp, PageDown and the arrows.
  static RemoteKey? special(LogicalKeyboardKey k) => _specials[k];

  /// a–z, digit0–9 and space.
  static RemoteKey? base(LogicalKeyboardKey k) => _bases[k]?.$1;

  /// The key's USB HID usage on the keyboard page, where it has one.
  static int? usage(PhysicalKeyboardKey k) {
    final id = k.usbHidUsage;
    final usage = id & 0xFFFF;
    return id >> 16 == 0x07 &&
            usage >= DesktopPhysicalKey.minUsage &&
            usage <= DesktopPhysicalKey.maxUsage
        ? usage
        : null;
  }

  /// The key that types [c] on a US layout, and whether it needs Shift:
  /// a–z, A–Z, 0–9, space and newline.
  static (RemoteKey, bool)? forChar(String c) {
    if (c.length != 1) return null;
    final code = c.codeUnitAt(0);
    if (code >= 0x61 && code <= 0x7A) {
      return (RemoteKey.values[RemoteKey.a.index + code - 0x61], false);
    }
    if (code >= 0x41 && code <= 0x5A) {
      return (RemoteKey.values[RemoteKey.a.index + code - 0x41], true);
    }
    if (code >= 0x30 && code <= 0x39) {
      return (RemoteKey.values[RemoteKey.digit0.index + code - 0x30], false);
    }
    return switch (c) {
      ' ' => (RemoteKey.space, false),
      '\n' => (RemoteKey.enter, false),
      _ => null,
    };
  }

  /// One whole tap of [key] wrapped in [mods]: nothing stays held.
  static List<DesktopInput> tap(RemoteKey key, Iterable<RemoteKey> mods) =>
      _wrap(DesktopKey(key, true), mods);

  static List<DesktopInput> _wrap(
    DesktopKeyInput key,
    Iterable<RemoteKey> mods,
  ) {
    final ordered = [
      for (final m in ModifierLatch.order)
        if (mods.contains(m)) m,
    ];
    return [
      for (final m in ordered) DesktopInput.key(m, true),
      key,
      key.up,
      for (final m in ordered.reversed) DesktopInput.key(m, false),
    ];
  }

  /// The inputs one hardware event sends, and what it holds or defers.
  ///
  /// A physically held modifier is never part of a wrap: the host does not
  /// count downs, and a wrapped up would release the key the user still
  /// holds (`D/windows.rs`). [holding] is the key this event's physical
  /// key holds on the host, and [upIsSwallowed] whether its down went as a
  /// whole tap.
  static KeyRoute route(
    KeyEvent e,
    KeyContext c, {
    DesktopKeyInput? holding,
    bool upIsSwallowed = false,
  }) {
    final up = e is KeyUpEvent;
    final remote = c.mode == KeyboardMode.remote;
    // This device's input method sees what the picture leaves alone.
    final ime = !remote && c.textInput;
    final m = modifier(e.logicalKey);

    // An up releases what its down holds, whatever is held around it now.
    if (up && holding != null) {
      return KeyRoute(
        inputs: [holding.up],
        holds: holding,
        handled: !(ime && m != null),
      );
    }

    // 1. A modifier waits for what it modifies, Shift, Ctrl and Alt in
    // remote mode excepted; repeats add nothing.
    if (m != null) {
      if (up || e is KeyRepeatEvent) return KeyRoute(handled: !ime);
      final key = _hostModifier(e.physicalKey, m, c);
      final waits = !remote || m == RemoteKey.meta;
      if (waits && !c.othersDown) return KeyRoute(defers: key, handled: !ime);
      return KeyRoute(inputs: [key], holds: key, handled: !ime);
    }

    // Its down went as a whole tap, or the host never had it.
    if (up) {
      return ime && !upIsSwallowed
          ? const KeyRoute.ignored()
          : const KeyRoute();
    }
    if (remote) return _remote(e, c);

    // 2. A chord: a letter, digit or space under a held Ctrl, Alt or Meta
    // is a shortcut, and so is any other key the host takes by place,
    // unless it typed something else: AltGr arrives as Control + Alt, and
    // Option on a Mac, and each types a character of its own.
    final wrap = c.latched.difference(c.held);
    final logical = e.logicalKey;
    final s = special(logical);
    final chord =
        c.held.contains(RemoteKey.control) ||
        c.held.contains(RemoteKey.meta) ||
        (c.held.contains(RemoteKey.alt) && !c.optionTypes);
    if (chord && s == null && !_typesOwn(e)) {
      final b = _bases[logical];
      final usage = KeyRouting.usage(e.physicalKey);
      final DesktopKeyInput? key = b != null
          ? DesktopKey(b.$1, true)
          : c.physicalKeys && usage != null
          ? DesktopPhysicalKey(usage, true)
          : null;
      if (key == null) return KeyRoute(handled: !ime);
      return _press(key, wrap);
    }

    // 3. The input method's own keys while it composes: Enter, Backspace,
    // the arrows and Escape act on what it composes.
    if (ime && c.composing) return const KeyRoute.ignored();

    // 4. A special key: a real down and up, wrapped while latched; a
    // command key the vocabulary cannot name goes by place.
    if (s != null) return _press(DesktopKey(s, true), wrap);
    final usage = KeyRouting.usage(e.physicalKey);
    if (_commands.contains(e.physicalKey) && c.physicalKeys && usage != null) {
      return _press(DesktopPhysicalKey(usage, true), wrap);
    }

    // 5. Anything printable: this device's input method takes it and
    // commits text through the client; without one, it is typed as text,
    // or keyed while latched.
    if (ime) return const KeyRoute.ignored();
    final ch = _printable(e.character);
    if (ch == null) return const KeyRoute.ignored();
    if (wrap.isEmpty) {
      return KeyRoute(inputs: [DesktopInput.text(ch)], swallowUp: true);
    }
    final key = forChar(ch);
    if (key == null) {
      // The latch cannot apply to it, and must not linger.
      return KeyRoute(
        inputs: [DesktopInput.text(ch)],
        consumesLatch: true,
        swallowUp: true,
      );
    }
    // A Shift held here goes as a real key first, never inside the tap.
    return KeyRoute(
      inputs: tap(
        key.$1,
        {...wrap, if (key.$2) RemoteKey.shift}.difference(c.held),
      ),
      consumesLatch: true,
      swallowUp: true,
      flush: true,
    );
  }

  /// Remote mode, past the modifiers.
  static KeyRoute _remote(KeyEvent e, KeyContext c) {
    final wrap = c.latched.difference(c.held);
    if (c.physicalKeys) {
      final usage = KeyRouting.usage(e.physicalKey);
      if (usage == null) return const KeyRoute.ignored();
      return _press(DesktopPhysicalKey(usage, true), wrap);
    }
    // An older host: its vocabulary as keys, the rest as text.
    final k = special(e.logicalKey) ?? base(e.logicalKey);
    if (k != null) return _press(DesktopKey(k, true), wrap);
    final ch = _printable(e.character);
    if (ch == null) return const KeyRoute.ignored();
    return KeyRoute(inputs: [DesktopInput.text(ch)], swallowUp: true);
  }

  /// A key's down after the modifiers waiting: a real down, or a whole tap
  /// wrapped in the latched ones.
  static KeyRoute _press(DesktopKeyInput key, Set<RemoteKey> wrap) =>
      wrap.isEmpty
      ? KeyRoute(inputs: [key], holds: key, flush: true)
      : KeyRoute(
          inputs: _wrap(key, wrap),
          consumesLatch: true,
          swallowUp: true,
          flush: true,
        );

  /// The host's name for modifier [m]: by place to a host that takes
  /// physical keys in remote mode, else in its vocabulary; Control and Meta
  /// traded when [KeyContext.swap].
  static DesktopKeyInput _hostModifier(
    PhysicalKeyboardKey physical,
    RemoteKey m,
    KeyContext c,
  ) {
    final usage = KeyRouting.usage(physical);
    if (c.mode == KeyboardMode.remote && c.physicalKeys && usage != null) {
      // ControlLeft 0xE0 ↔ MetaLeft 0xE3, ControlRight 0xE4 ↔ MetaRight 0xE7.
      final swapped = !c.swap
          ? usage
          : switch (usage) {
              0xE0 => 0xE3,
              0xE3 => 0xE0,
              0xE4 => 0xE7,
              0xE7 => 0xE4,
              _ => usage,
            };
      return DesktopPhysicalKey(swapped, true);
    }
    final swapped = !c.swap
        ? m
        : switch (m) {
            RemoteKey.control => RemoteKey.meta,
            RemoteKey.meta => RemoteKey.control,
            _ => m,
          };
    return DesktopKey(swapped, true);
  }

  /// The key typed a character of its own under its modifiers (AltGr,
  /// Option): not its unshifted one.
  static bool _typesOwn(KeyEvent e) {
    final ch = _printable(e.character);
    if (ch == null) return false;
    final label = e.logicalKey.keyLabel;
    return label.isEmpty || ch.toLowerCase() != label.toLowerCase();
  }

  /// [c] when it is a character to type: not empty, and no C0 or C1
  /// control character, which is what Ctrl+letter reports on some platforms.
  static String? _printable(String? c) {
    if (c == null || c.isEmpty) return null;
    for (final code in c.runes) {
      if (code < 0x20 || (code >= 0x7F && code <= 0x9F)) return null;
    }
    return c;
  }
}
