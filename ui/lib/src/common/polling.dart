import 'dart:async';

import 'package:pb_mapper_ui/src/common/state_change.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_service.dart';

/// Waits for something on the Rust side to stop changing.
///
/// Registering or connecting reports success as soon as the request is
/// accepted; whether the tunnel actually came up shows up a moment later, and
/// the caller wants to report the state it settled on rather than the state it
/// was in the instant the request was taken.
///
/// This used to be a one-second timer loop. It now waits on the change stream —
/// the same events that keep the lists fresh — so it reacts the moment Rust
/// notices a transition instead of up to a second later, and nothing is asking
/// the native side questions in the meantime. The slow tick that remains is a
/// backstop, not the mechanism: if an event is ever missed, the wait still
/// finishes rather than hanging until the timeout.
///
/// [attempt] returns true when there is nothing left to wait for — settled,
/// failed, or the caller has gone away. The result says whether it settled
/// rather than timed out, so a caller can tell the two apart.
Future<bool> waitUntilSettled({
  required Future<bool> Function() attempt,
  Duration timeout = const Duration(seconds: 30),
  Duration backstop = const Duration(seconds: 5),
  Stream<StateChange>? changes,
}) async {
  final deadline = DateTime.now().add(timeout);
  final stream = changes ?? PbMapperService.changeStream;
  final wakeups = StreamController<void>();

  final sub = stream.listen((_) {
    if (!wakeups.isClosed) wakeups.add(null);
  });
  final ticker = Timer.periodic(backstop, (_) {
    if (!wakeups.isClosed) wakeups.add(null);
  });

  try {
    // One immediate check: the change may already have landed while the caller
    // was awaiting the request itself.
    if (await attempt()) return true;

    await for (final _ in wakeups.stream) {
      if (DateTime.now().isAfter(deadline)) return false;
      if (await attempt()) return true;
    }
    return false;
  } finally {
    ticker.cancel();
    await sub.cancel();
    await wakeups.close();
  }
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
