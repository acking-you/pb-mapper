import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/ffi/tunnel_event_stream.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';

import 'fake_pb_mapper_api.dart';
import 'support.dart';

/// A tunnel's diagnostics, shaped as `pb_mapper_tunnel_events_call_json`
/// answers with them.
Map<String, dynamic> tunnelDiagnostics({
  Map<String, dynamic>? mapping,
  bool? lanFirst,
}) => {
  'relay_transport': 'tcp',
  'lan_first': ?lanFirst,
  'last_setup_latency_ms': 42,
  'consecutive_failures': 0,
  'direct': {'direct_sessions': 2, 'relay_sessions': 0, 'fallbacks': 0},
  if (mapping != null)
    'port_mapping': {
      'current': mapping,
      'attempts': 1,
      'renewals': 0,
      'failures': 0,
    },
  'events': const [],
};

Map<String, dynamic> mappingStatus(String stage) => {
  'stage': stage,
  'protocol': 'pcp',
  'local_port': 52183,
  if (stage == 'active') 'external_addr': '203.0.113.7:52183',
  'active': stage == 'active',
  'lease_lifetime_ms': 7200000,
};

TunnelEventsUpdate tunnelEvent(
  int seq,
  Map<String, dynamic> kind, {
  Map<String, dynamic>? diagnostics,
}) => TunnelEventsUpdate.fromMap({
  'type': 'event',
  'event': {'seq': seq, 'age_ms': 0, 'repeats': 1, 'kind': kind},
  'diagnostics': diagnostics ?? tunnelDiagnostics(),
});

void main() {
  test(
    'cancelling a reader closes it in Rust, waking a pending read',
    () async {
      final reading = Completer<void>();
      final pending = Completer<Map<String, dynamic>>();
      final requests = <String>[];
      Map<String, dynamic> ok(Map<String, dynamic> data) => {
        'success': true,
        'data': data,
      };
      final stream = tunnelEventStream(
        (request) async {
          requests.add(request['op'] as String);
          switch (request['op']) {
            case 'subscribe':
              return ok({'id': 1, 'diagnostics': tunnelDiagnostics()});
            case 'next':
              reading.complete();
              return pending.future;
            case 'close':
              pending.complete(ok({'type': 'closed'}));
              return ok({'type': 'closed'});
          }
          throw StateError('unexpected ${request['op']}');
        },
        serviceKey: 'web',
        share: true,
      );
      final subscription = stream.listen((_) {});
      await reading.future;
      await subscription.cancel().timeout(const Duration(seconds: 1));
      expect(requests, ['subscribe', 'next', 'close']);
    },
  );

  testWidgets(
    'diagnostics follow a running tunnel live and are not offered for a stopped one',
    (tester) async {
      final feed = StreamController<TunnelEventsUpdate>();
      final api = FakePbMapperApi(
        services: [
          fakeService(serviceKey: 'web', portMapping: PortMappingMode.always),
          fakeService(serviceKey: 'old', status: 'stopped'),
        ],
      )..tunnelEventFeed = feed.stream;
      await pumpApp(
        tester,
        api,
        page: AppPage.tunnels,
        size: const Size(390, 844),
      );

      await tester.tap(find.byKey(const Key('menu-share:old')));
      await settle(tester);
      expect(
        tester
            .widget<PopupMenuItem<Object>>(
              find.byKey(const Key('menu-diagnostics')),
            )
            .enabled,
        isFalse,
      );
      await tester.tapAt(Offset.zero);
      await settle(tester);

      feed
        ..add(
          TunnelEventsUpdate.fromMap({
            'diagnostics': tunnelDiagnostics(
              mapping: mappingStatus('requesting'),
            ),
          }),
        )
        ..add(tunnelEvent(1, {'kind': 'relay_ready', 'latency_ms': 42}))
        ..add(tunnelEvent(2, {'kind': 'session_opened', 'path': 'direct'}))
        ..add(
          tunnelEvent(
            3,
            {
              'kind': 'port_mapping_changed',
              'mapping': mappingStatus('active'),
            },
            diagnostics: tunnelDiagnostics(
              mapping: mappingStatus('active'),
              lanFirst: false,
            ),
          ),
        );
      await tester.tap(find.byKey(const Key('menu-share:web')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('menu-diagnostics')));
      await settle(tester);
      expect(find.byKey(const Key('tunnel-event-1')), findsOneWidget);
      expect(find.byKey(const Key('tunnel-event-3')), findsOneWidget);
      // Sessions are counted, not listed one by one.
      expect(find.byKey(const Key('tunnel-event-2')), findsNothing);
      expect(
        tester.widget<Text>(find.byKey(const Key('port-mapping-stage'))).data,
        'Mapped',
      );
      expect(find.text(':52183 → 203.0.113.7:52183'), findsOneWidget);
      expect(find.byKey(const Key('traffic-relay')), findsNothing);
      // A tunnel started with LAN-first off says so.
      expect(find.byKey(const Key('lan-first-off')), findsOneWidget);

      // A sample between events keeps the figures live, and alone does
      // not lose what the events said.
      feed.add(
        TunnelEventsUpdate.fromMap({
          'type': 'metrics',
          'metrics': {
            'sessions': {'direct': 2, 'relay': 1, 'lan': 0},
            'traffic': {
              'relay': {'sent': 2048, 'received': 4096},
              'direct': {'sent': 10, 'received': 20},
              'lan': {'sent': 0, 'received': 0},
            },
            'relay': {'rtt_ms': 12, 'consecutive_failures': 0},
          },
        }),
      );
      await settle(tester);
      expect(find.byKey(const Key('traffic-relay')), findsOneWidget);
      expect(find.byKey(const Key('traffic-direct')), findsOneWidget);
      expect(
        tester.widget<Text>(find.byKey(const Key('port-mapping-stage'))).data,
        'Mapped',
      );
      expect(tester.takeException(), isNull);

      await tester.tap(find.byTooltip('Close').last);
      await settle(tester);
      expect(feed.hasListener, isFalse);
      unawaited(feed.close());
      await finish(tester);
    },
  );
}
