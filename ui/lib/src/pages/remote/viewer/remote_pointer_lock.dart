import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

import '../../../ffi/desktop_pointer.dart';
import '../../../state/desktop_store.dart';
import '../../../state/viewer_prefs.dart';
import 'remote_input.dart';

/// How long the remote pointer stays hidden while the mouse moves over the
/// picture before Auto holds this computer's pointer (airhand's
/// `AUTO_LOCK_AFTER_MS`). Windows hides its pointer while someone types and
/// shows it at the next move, well within this, so typing never locks.
const kAutoLockAfter = Duration(milliseconds: 300);

/// How long Esc is held, while the pointer is held, to let it go (airhand's
/// `ESC_HOLD_MS`). A shorter press is the remote computer's Esc.
const kEscHold = Duration(seconds: 1);

/// The pointer-lock rules of airhand's viewer (client/src/lib/input.ts),
/// with no runner, timer or widget: given what happened, whether this
/// computer's pointer is held ([locked]), whether moves go as relative
/// moves ([relative]), and whether Game mode waits for a click ([paused]).
///
/// - Auto holds the pointer when the remote one is hidden while a button is
///   held, or while the mouse moves for [kAutoLockAfter]; until then moves
///   already go relative. It lets go when the remote pointer shows again
///   with every button up: one that shows while a button is held counts
///   once all are up, so a game that shows its pointer for a moment
///   mid-drag is not let go and held again.
/// - Desktop never holds it; Game holds it from a click on the picture,
///   which goes nowhere, and sends nothing from the mouse until then.
/// - An Esc held for [kEscHold], the window losing the focus, or a runner
///   that cannot hold the pointer lets go ([letGo]): Auto then holds it
///   again only after the next click, Game at the next click.
class PointerLockRules {
  PointerMode _mode = PointerMode.auto;
  bool _available = false;

  /// The remote pointer is hidden, as the rules take it, and as the newest
  /// report says.
  bool _hidden = false, _hiddenNow = false;

  /// The mouse buttons held over the picture (`PointerEvent.buttons`).
  int _buttons = 0;

  /// Let go in Auto with the remote pointer hidden: no hold before a click.
  bool _userUnlocked = false;
  Duration? _hiddenMoveSince;
  bool _locked = false;

  /// The click that held the pointer in Game mode, from its press until
  /// every button is up again.
  bool _swallowing = false;
  Duration? _escSince;

  PointerMode get mode => _mode;

  /// This viewer can hold its pointer for the host it controls now.
  bool get available => _available;

  /// This computer's pointer is held, its motion going as relative moves.
  bool get locked => _locked;

  /// Moves go as relative moves, the pointer held or not.
  bool get relative =>
      _available &&
      switch (_mode) {
        PointerMode.game => _locked,
        PointerMode.desktop => false,
        PointerMode.auto => _locked || (_hidden && !_userUnlocked),
      };

  /// Game mode with the pointer not held: the mouse sends nothing, and the
  /// next click on the picture holds it.
  bool get paused => _available && _mode == PointerMode.game && !_locked;

  /// The click that held the pointer is not over yet: none of it goes.
  bool get swallowing => _swallowing;

  /// When an Esc held now lets the pointer go.
  Duration? get escDue {
    final since = _escSince;
    return since == null ? null : since + kEscHold;
  }

  void setMode(PointerMode mode) {
    if (mode == _mode) return;
    _mode = mode;
    _userUnlocked = false;
    _hiddenMoveSince = null;
    // Game keeps a pointer held already; Auto, while the remote one hides.
    if (mode == PointerMode.desktop || (mode == PointerMode.auto && !_hidden)) {
      _release();
    }
  }

  /// A mouse viewer whose runner holds the pointer, in control of a host
  /// that moves its pointer by counts. Becoming unavailable lets go and
  /// forgets the buttons; the remote pointer's state stays.
  void setAvailable(bool available) {
    if (available == _available) return;
    _available = available;
    if (available) return;
    _release();
    _buttons = 0;
    _hiddenMoveSince = null;
  }

  /// The remote computer's newest report: its pointer hidden or showing.
  void hostCursor({required bool hidden}) {
    _hiddenNow = hidden;
    if (!hidden && _hidden && _buttons != 0) return;
    _applyHidden(hidden);
  }

  void _applyHidden(bool hidden) {
    if (_hidden == hidden) return;
    _hidden = hidden;
    _hiddenMoveSince = null;
    if (_mode != PointerMode.auto) return;
    if (hidden) {
      if (_available && _buttons != 0 && !_userUnlocked) _locked = true;
    } else {
      _userUnlocked = false;
      _release();
    }
  }

  /// The mouse buttons held over the picture now. True while they belong
  /// to the click that held the pointer in Game mode, which goes nowhere.
  bool buttons(int held) {
    final pressed = held & ~_buttons;
    _buttons = held;
    if (!_available) return false;
    if (_swallowing) {
      if (held == 0) _swallowing = false;
      return true;
    }
    if (pressed != 0) {
      if (paused) {
        _locked = true;
        _swallowing = true;
        return true;
      }
      _userUnlocked = false;
      if (_mode == PointerMode.auto && _hidden) _locked = true;
    }
    // A remote pointer that showed while a button was held counts now.
    if (held == 0 && _hidden && !_hiddenNow) _applyHidden(false);
    return false;
  }

  /// The mouse moved over the picture at [now], the pointer not held.
  void moved(Duration now) {
    if (!_available ||
        _locked ||
        _mode != PointerMode.auto ||
        !_hidden ||
        _userUnlocked) {
      return;
    }
    final since = _hiddenMoveSince ??= now;
    if (now - since >= kAutoLockAfter) _locked = true;
  }

  /// Esc went down at [now] (a repeat is no press): held for [kEscHold]
  /// while the pointer is held, it lets go.
  void escDown(Duration now) {
    if (_locked) _escSince ??= now;
  }

  void escUp() => _escSince = null;

  /// The time is [now]: an Esc held long enough lets go.
  void tick(Duration now) {
    final due = escDue;
    if (due == null || now < due) return;
    _escSince = null;
    if (_locked) letGo();
  }

  /// The user let go (Esc held), the window lost the focus, or the runner
  /// could not hold or keep the pointer.
  void letGo() {
    if (_mode == PointerMode.auto && _hidden) _userUnlocked = true;
    _hiddenMoveSince = null;
    _release();
  }

  void _release() {
    _locked = false;
    _swallowing = false;
    _escSince = null;
  }
}

/// One desktop viewer's pointer lock: [PointerLockRules] over the session,
/// the device's [PointerMode] and this computer's runner. It holds the
/// runner's pointer as the rules say, sends its motion, and tells
/// [RemoteInput] whether moves go relative, which also makes the session's
/// cursor follow every report of the host's pointer, painted in place of a
/// local pointer.
///
/// The surface hands it the mouse's buttons, moves and the keys; the viewer
/// its focus. Without a runner ([DesktopPointerLock]) or a host that lists
/// `relative_pointer`, nothing is ever held, and moves stay absolute.
final class RemotePointerLock {
  RemotePointerLock({
    required DesktopStore desktop,
    required RemoteInput input,
    required ViewerPrefs prefs,
    DesktopPointerLock? runner,
  }) : _desktop = desktop,
       _input = input,
       _prefs = prefs,
       _runner = runner ?? (nativePointerCursors ? desktopPointerLock : null) {
    _desktop.session.addListener(_sync);
    _desktop.cursor.addListener(_sync);
    _input.ready.addListener(_sync);
    _prefs.addListener(_sync);
    _sync();
  }

  final DesktopStore _desktop;
  final RemoteInput _input;
  final ViewerPrefs _prefs;
  final DesktopPointerLock? _runner;
  final _rules = PointerLockRules();
  final _relative = ValueNotifier(false);

  /// The runner holds the pointer, or was asked to.
  bool _held = false;
  bool _disposed = false;
  Timer? _escTimer;
  Duration? _escDue;

  /// What the unheld relative moves fell short of a whole count.
  double _carryX = 0, _carryY = 0;

  /// Moves go as relative moves: the cursor layer paints the host's
  /// pointer and hides this computer's over the picture.
  ValueListenable<bool> get relativeListenable => _relative;
  bool get relative => _rules.relative;

  /// The runner holds this computer's pointer.
  bool get locked => _rules.locked;

  /// Game mode waits for a click on the picture to hold the pointer: the
  /// mouse sends nothing until then.
  bool get paused => _rules.paused;

  @visibleForTesting
  PointerLockRules get rules => _rules;

  /// The mouse buttons held over the picture now. True while they belong
  /// to the click that holds the pointer in Game mode: send none of it.
  bool buttons(int held) {
    final swallowed = _rules.buttons(held);
    _apply();
    return swallowed;
  }

  /// The mouse moved over the picture by [delta] logical pixels, at
  /// [devicePixelRatio], the pointer not held: Auto counts it towards
  /// holding the pointer, and a relative move goes in this computer's
  /// pixels, as airhand's unheld moves do.
  void moved(Offset delta, double devicePixelRatio) {
    _rules.moved(_input.clock());
    if (_rules.relative && !_rules.locked) {
      _carryX += delta.dx * devicePixelRatio;
      _carryY += delta.dy * devicePixelRatio;
      final dx = _carryX.truncate(), dy = _carryY.truncate();
      _carryX -= dx;
      _carryY -= dy;
      if (dx != 0 || dy != 0) _input.moveBy(dx, dy);
    }
    _apply();
  }

  /// A key on its way to the host. True when it must not go: an Esc
  /// repeat while the pointer is held, which only times the hold.
  bool key(KeyEvent event) {
    if (event.logicalKey != LogicalKeyboardKey.escape) return false;
    switch (event) {
      case KeyDownEvent():
        _rules.escDown(_input.clock());
      case KeyUpEvent():
        _rules.escUp();
      case KeyRepeatEvent():
        return _rules.locked || _rules.escDue != null;
    }
    _apply();
    return false;
  }

  /// The picture lost the keyboard, or the app went inactive or hidden.
  void focusLost() {
    _rules.letGo();
    _apply();
  }

  void _sync() {
    if (_disposed) return;
    final caps = _desktop.session.value.capabilities;
    _rules
      ..setMode(_prefs.pointerMode)
      ..setAvailable(
        _runner != null &&
            _input.ready.value &&
            caps.contains('relative_pointer'),
      )
      ..hostCursor(hidden: _desktop.cursor.hostHidden);
    _apply();
  }

  void _apply() {
    if (_disposed) return;
    final relative = _rules.relative;
    if (relative != _input.relative) {
      _input.relative = relative;
      _carryX = 0;
      _carryY = 0;
    }
    _relative.value = relative;
    final hold = _rules.locked;
    if (hold != _held) {
      _held = hold;
      final runner = _runner;
      if (runner != null) {
        if (hold) {
          runner
              .lock(onMotion: _motion, onLost: _lost)
              .then<void>((_) {}, onError: _lockFailed);
        } else {
          runner.unlock().ignore();
        }
      }
    }
    _armEsc();
  }

  /// A timer for the Esc held now, once per hold.
  void _armEsc() {
    final due = _rules.escDue;
    if (due == _escDue) return;
    _escDue = due;
    _escTimer?.cancel();
    _escTimer = null;
    if (due == null) return;
    final wait = due - _input.clock();
    _escTimer = Timer(wait < Duration.zero ? Duration.zero : wait, () {
      _escTimer = null;
      // The hold's time has come: the timer says so, whatever the clock
      // read a moment ago.
      _rules.tick(due);
      _apply();
    });
  }

  /// The runner's motion while it holds the pointer.
  void _motion(int dx, int dy) {
    if (_disposed || !_rules.locked || _rules.swallowing) return;
    _input.moveBy(dx, dy);
  }

  /// The runner let go by itself: this window lost the focus.
  void _lost() {
    if (_disposed) return;
    _held = false;
    _rules.letGo();
    _apply();
  }

  void _lockFailed(Object error) {
    if (_disposed || !_held) return;
    debugPrint('Desktop pointer: cannot hold the pointer ($error)');
    _held = false;
    _rules.letGo();
    _apply();
  }

  void dispose() {
    if (_disposed) return;
    _disposed = true;
    _escTimer?.cancel();
    _desktop.session.removeListener(_sync);
    _desktop.cursor.removeListener(_sync);
    _input.ready.removeListener(_sync);
    _prefs.removeListener(_sync);
    if (_held) _runner?.unlock().ignore();
    _input.relative = false;
    _relative.dispose();
  }
}
