import 'dart:async';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/diagnostics_stream.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'fake_pb_mapper_api.dart';
import 'support.dart';

void main() {
  test(
    'an idle subscription cancels its pending native read without waiting for an event',
    () async {
      final reading = Completer<void>();
      final pending = Completer<Map<String, dynamic>>();
      var closes = 0;
      final stream = diagnosticStream(
        (request) async {
          switch (request['op']) {
            case 'subscribe':
              return {
                'success': true,
                'data': {'id': 1, 'snapshot': {}},
              };
            case 'next':
              reading.complete();
              return pending.future;
            case 'close':
              closes++;
              pending.complete({
                'success': true,
                'data': {'closed': true},
              });
              return {
                'success': true,
                'data': {'closed': true},
              };
            default:
              throw StateError('unexpected operation');
          }
        },
        key: 'web',
        share: true,
      );
      final subscription = stream.listen((_) {});
      await reading.future;
      await subscription.cancel().timeout(const Duration(seconds: 1));
      expect(closes, 1);
    },
  );

  testWidgets(
    'diagnostics reconcile replay, show live mapping events and release the stream',
    (tester) async {
      final updates = StreamController<DiagnosticUpdate>();
      var cancelled = false;
      updates.onCancel = () {
        cancelled = true;
      };
      final api = FakePbMapperApi(
        services: [fakeService(serviceKey: 'web', portMapping: true)],
      )..diagnosticUpdates = updates.stream;
      await pumpApp(
        tester,
        api,
        page: AppPage.tunnels,
        size: const Size(390, 844),
      );
      await tester.tap(find.byKey(const Key('menu-share:web')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('menu-diagnostics')));
      await settle(tester);
      Map<String, dynamic> event(int seq, String stage) => {
        'seq': seq,
        'kind': {
          'kind': 'port_mapping',
          'mapping': {
            'stage': stage,
            'active': stage == 'active',
            'protocol': 'pcp',
            'local_port': 12345,
            'external_addr': '203.0.113.7:40000',
          },
        },
      };
      updates.add(
        DiagnosticUpdate(
          snapshot: {
            'events': [event(1, 'requesting')],
          },
        ),
      );
      updates.add(
        DiagnosticUpdate(event: TunnelDiagnosticEvent(event(1, 'requesting'))),
      );
      updates.add(
        DiagnosticUpdate(event: TunnelDiagnosticEvent(event(2, 'active'))),
      );
      await settle(tester);
      expect(find.byKey(const Key('diagnostic-1')), findsOneWidget);
      expect(find.byKey(const Key('diagnostic-2')), findsOneWidget);
      expect(find.textContaining('203.0.113.7:40000'), findsOneWidget);
      expect(tester.takeException(), isNull);
      await tester.tap(find.byTooltip('Close').last);
      await settle(tester);
      expect(cancelled, isTrue);
      // Let the widget binding pump the stream's close microtask during cleanup.
      unawaited(updates.close());
      await finish(tester);
    },
  );
}
