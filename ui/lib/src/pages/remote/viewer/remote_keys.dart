import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

import '../../../state/desktop_input.dart';
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

  /// Win + L, which is the host's own `lock` command (D17), not keys.
  lock,

  /// Never enabled: the secure attention sequence cannot be injected, and
  /// the menu says so.
  ctrlAltDel,

  /// Asks the host for a fresh picture.
  refresh,

  /// Releases every key and button this viewer holds on the host.
  releaseKeys;

  /// The keys a chord presses, in order; released in reverse. Empty for the
  /// entries that are not chords.
  List<RemoteKey> get chord => switch (this) {
    showDesktop => const [RemoteKey.meta, RemoteKey.d],
    switchWindow => const [RemoteKey.alt, RemoteKey.tab],
    taskManager => const [RemoteKey.control, RemoteKey.shift, RemoteKey.escape],
    startMenu => const [RemoteKey.meta],
    lock || ctrlAltDel || refresh || releaseKeys => const [],
  };
}

/// What one hardware key event sends ([KeyRouting.route]).
@immutable
class KeyRoute {
  const KeyRoute({
    this.inputs = const [],
    this.consumesLatch = false,
    this.swallowUp = false,
    this.holds,
  }) : handled = true;

  const KeyRoute.ignored()
    : handled = false,
      inputs = const [],
      consumesLatch = false,
      swallowUp = false,
      holds = null;

  /// [KeyEventResult.handled], else ignored.
  final bool handled;

  /// Nothing to send when empty; more than one go in one sequence.
  final List<DesktopInput> inputs;

  /// The caller takes the latch: armed modifiers turn off.
  final bool consumesLatch;

  /// The down was sent as a whole tap, so its key-up sends nothing.
  final bool swallowUp;

  /// The key a down holds on the host, or an up releases.
  final RemoteKey? holds;
}

/// How hardware keys reach the host (ui §10.5.1). Pure: the caller supplies
/// what is latched, which modifiers are physically down, and what the
/// event's physical key did on its way down.
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
  static List<DesktopInput> tap(RemoteKey key, Iterable<RemoteKey> mods) {
    final ordered = [
      for (final m in ModifierLatch.order)
        if (mods.contains(m)) m,
    ];
    return [
      for (final m in ordered) DesktopInput.key(m, true),
      DesktopInput.key(key, true),
      DesktopInput.key(key, false),
      for (final m in ordered.reversed) DesktopInput.key(m, false),
    ];
  }

  /// The inputs one hardware event sends.
  ///
  /// [latched] is the latch's active set and [held] the modifiers physically
  /// down. A physically held modifier went to the host as a real down, so it
  /// is never part of a wrap: the host does not count downs, and a wrapped
  /// up would release the key the user still holds (`D/windows.rs`).
  /// [holding] is the key this event's physical key holds on the host, and
  /// [upIsSwallowed] whether its down went as a whole tap.
  static KeyRoute route(
    KeyEvent e, {
    required Set<RemoteKey> latched,
    required Set<RemoteKey> held,
    RemoteKey? holding,
    bool upIsSwallowed = false,
  }) {
    final up = e is KeyUpEvent;
    final wrap = latched.difference(held);
    final logical = e.logicalKey;

    // An up releases what its down holds, whatever is held around it now.
    if (up && holding != null) {
      return KeyRoute(
        inputs: [DesktopInput.key(holding, false)],
        holds: holding,
      );
    }

    // 1. A modifier is a real down and up; repeats add nothing.
    final m = modifier(logical);
    if (m != null) {
      if (e is KeyRepeatEvent) return const KeyRoute();
      return KeyRoute(inputs: [DesktopInput.key(m, !up)], holds: m);
    }

    // Its down went as a whole tap.
    if (up && upIsSwallowed) return const KeyRoute();

    // 2. A special key: wrapped while latched, else a real down and up.
    final s = special(logical);
    if (s != null) {
      if (!up && wrap.isNotEmpty) {
        return KeyRoute(
          inputs: tap(s, wrap),
          consumesLatch: true,
          swallowUp: true,
        );
      }
      return KeyRoute(inputs: [DesktopInput.key(s, !up)], holds: s);
    }

    // 3. A letter, digit or space under a held Ctrl, Alt or Meta is a
    // shortcut, unless it typed something else: AltGr arrives as Control +
    // Alt and types a character of its own.
    final b = _bases[logical];
    final shortcut = held.any(
      (m) =>
          m == RemoteKey.control || m == RemoteKey.alt || m == RemoteKey.meta,
    );
    if (b != null && shortcut) {
      final c = _printable(e.character);
      if (c == null || c.toLowerCase() == b.$2) {
        if (!up && wrap.isNotEmpty) {
          return KeyRoute(
            inputs: tap(b.$1, wrap),
            consumesLatch: true,
            swallowUp: true,
          );
        }
        return KeyRoute(inputs: [DesktopInput.key(b.$1, !up)], holds: b.$1);
      }
    }

    // 4. A printable character: typed as text, or keyed while latched.
    final c = _printable(e.character);
    if (!up && c != null) {
      if (wrap.isEmpty) {
        return KeyRoute(inputs: [DesktopInput.text(c)], swallowUp: true);
      }
      final key = forChar(c);
      if (key == null) {
        // The latch cannot apply to it, and must not linger.
        return KeyRoute(
          inputs: [DesktopInput.text(c)],
          consumesLatch: true,
          swallowUp: true,
        );
      }
      return KeyRoute(
        inputs: tap(
          key.$1,
          {...wrap, if (key.$2) RemoteKey.shift}.difference(held),
        ),
        consumesLatch: true,
        swallowUp: true,
      );
    }

    // 5. Anything else, including keys outside the host's vocabulary.
    return const KeyRoute.ignored();
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
