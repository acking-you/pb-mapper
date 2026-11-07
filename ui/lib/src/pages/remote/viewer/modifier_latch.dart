import 'package:flutter/foundation.dart';

import '../../../state/desktop_input.dart';

/// A modifier on the modifier bar: off, armed for the next action only, or
/// locked until tapped again (D16).
enum LatchState { off, armed, locked }

/// The latched modifiers of one viewer. A latch never holds a key on the
/// host: whatever it applies to is sent wrapped in the modifiers' downs and
/// ups ([RemoteInput]).
final class ModifierLatch extends ChangeNotifier {
  /// The order modifiers are pressed in; they are released in reverse.
  static const order = [
    RemoteKey.control,
    RemoteKey.alt,
    RemoteKey.shift,
    RemoteKey.meta,
  ];

  final _states = <RemoteKey, LatchState>{};

  LatchState stateOf(RemoteKey modifier) => _states[modifier] ?? LatchState.off;

  /// The armed and locked modifiers, in [order].
  Set<RemoteKey> get active => {
    for (final m in order)
      if (stateOf(m) != LatchState.off) m,
  };

  bool get isEmpty => _states.isEmpty;

  /// off → armed → locked → off.
  void tap(RemoteKey modifier) {
    assert(order.contains(modifier));
    switch (stateOf(modifier)) {
      case LatchState.off:
        _states[modifier] = LatchState.armed;
      case LatchState.armed:
        _states[modifier] = LatchState.locked;
      case LatchState.locked:
        _states.remove(modifier);
    }
    notifyListeners();
  }

  /// An action is sent: returns what applies to it, and the armed ones
  /// turn off. Locked ones stay.
  Set<RemoteKey> take() {
    final taken = active;
    final before = _states.length;
    _states.removeWhere((_, state) => state == LatchState.armed);
    if (_states.length != before) notifyListeners();
    return taken;
  }

  void clear() {
    if (_states.isEmpty) return;
    _states.clear();
    notifyListeners();
  }
}
