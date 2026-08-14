import 'dart:async';

/// Waits for something on the Rust side to stop changing.
///
/// Registering or connecting reports success as soon as the request is
/// accepted; whether the tunnel actually came up shows up a moment later in the
/// status cache. Both workspaces had their own copy of this loop, which is how
/// they drifted to different attempt counts for the same wait.
///
/// [attempt] returns true when there is nothing left to wait for — settled,
/// failed, or the caller has gone away. Returning false asks for another round.
/// The result says whether it settled rather than ran out of attempts, so a
/// caller can tell a timeout from a success.
Future<bool> pollUntilSettled({
  required Future<bool> Function() attempt,
  int attempts = 10,
  Duration interval = const Duration(seconds: 1),
}) async {
  for (var i = 0; i < attempts; i++) {
    await Future<void>.delayed(interval);
    if (await attempt()) return true;
  }
  return false;
}

/// A lookup that says "not found" instead of handing back a stand-in.
///
/// `firstWhere` has no nullable form, so both views used to build an empty
/// config as its `orElse` and then test `serviceKey.isNotEmpty` to discover
/// whether the lookup had matched at all. A blank record reads as a value
/// rather than a miss, and it is one forgotten check away from being operated
/// on as if it were real.
extension FirstWhereOrNull<T> on Iterable<T> {
  T? firstWhereOrNull(bool Function(T element) test) {
    for (final element in this) {
      if (test(element)) return element;
    }
    return null;
  }
}
