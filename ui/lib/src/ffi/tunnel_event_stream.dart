import 'dart:async';

/// Read a running tunnel's events through [call], which runs one operation of
/// `pb_mapper_tunnel_events_call_json` and answers with its envelope.
///
/// The reader asks for the next event only once the last has been taken, and
/// not while the subscription is paused, so nothing queues on either side of
/// the boundary. Cancelling closes the reader in Rust, which also wakes a read
/// that is waiting on a quiet tunnel. The stream ends when the tunnel stops.
Stream<Map<String, dynamic>> tunnelEventStream(
  Future<Map<String, dynamic>> Function(Map<String, dynamic> request) call, {
  required String serviceKey,
  required bool share,
}) {
  late final StreamController<Map<String, dynamic>> controller;
  Future<void>? reading;
  Completer<void>? resumed;
  var cancelled = false;
  Object? id;

  Future<Map<String, dynamic>> request(Map<String, dynamic> request) async {
    final answer = await call(request);
    if (answer['success'] != true) throw StateError('${answer['message']}');
    return Map<String, dynamic>.from(answer['data'] as Map);
  }

  Future<void> close() async {
    final open = id;
    id = null;
    if (open != null) await request({'op': 'close', 'id': open});
  }

  Future<void> read() async {
    try {
      final opened = await request({
        'op': 'subscribe',
        'kind': share ? 'share' : 'connect',
        'key': serviceKey,
      });
      id = opened['id'];
      if (!cancelled) controller.add(opened);
      while (!cancelled) {
        await resumed?.future;
        if (cancelled) break;
        final next = await request({
          'op': 'next',
          'id': id,
          'timeout_ms': 30000,
        });
        if (cancelled || next['type'] == 'closed') break;
        if (next['type'] != 'idle') controller.add(next);
      }
    } catch (error, stack) {
      if (!cancelled) controller.addError(error, stack);
    } finally {
      try {
        await close();
      } catch (error, stack) {
        if (!cancelled) controller.addError(error, stack);
      }
      unawaited(controller.close());
    }
  }

  controller = StreamController(
    onListen: () => reading = read(),
    onPause: () => resumed ??= Completer<void>(),
    onResume: () {
      resumed?.complete();
      resumed = null;
    },
    onCancel: () async {
      cancelled = true;
      resumed?.complete();
      resumed = null;
      try {
        await close();
      } finally {
        await reading;
      }
    },
  );
  return controller.stream;
}
