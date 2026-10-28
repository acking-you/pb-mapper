import 'dart:async';

/// One pull at a time, with native backpressure while paused and cancellation
/// even when an idle tunnel has no new events to yield.
Stream<Map<String, dynamic>> diagnosticStream(
  Future<Map<String, dynamic>> Function(Map<String, dynamic>) call, {
  required String key,
  required bool share,
}) {
  late StreamController<Map<String, dynamic>> controller;
  Future<void>? pumping;
  Completer<void>? resumed;
  bool cancelled = false;
  Object? id;

  Future<Map<String, dynamic>> request(Map<String, dynamic> request) async {
    final result = await call(request);
    if (result['success'] != true) {
      throw StateError(result['message'].toString());
    }
    return Map<String, dynamic>.from(result['data'] as Map);
  }

  Future<void> closeNative() async {
    final closing = id;
    id = null;
    if (closing != null) await request({'op': 'close', 'id': closing});
  }

  Future<void> pump() async {
    try {
      final initial = await request({
        'op': 'subscribe',
        'key': key,
        'share': share,
      });
      id = initial['id'];
      if (!cancelled) controller.add(initial);
      while (!cancelled) {
        await resumed?.future;
        if (cancelled) break;
        final data = await request({
          'op': 'next',
          'id': id,
          'timeout_ms': 30_000,
        });
        if (cancelled) break;
        if (data['timeout'] != true) controller.add(data);
        if (data['closed'] == true) break;
      }
    } catch (error, stack) {
      if (!cancelled) controller.addError(error, stack);
    } finally {
      try {
        await closeNative();
      } catch (error, stack) {
        if (!cancelled) controller.addError(error, stack);
      }
      unawaited(controller.close());
    }
  }

  controller = StreamController(
    onListen: () {
      pumping = pump();
    },
    onPause: () {
      resumed ??= Completer<void>();
    },
    onResume: () {
      resumed?.complete();
      resumed = null;
    },
    onCancel: () async {
      cancelled = true;
      resumed?.complete();
      resumed = null;
      try {
        await closeNative();
      } finally {
        await pumping;
      }
    },
  );
  return controller.stream;
}
