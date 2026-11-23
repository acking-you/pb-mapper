import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_api.dart';
import 'package:pb_mapper_ui/src/pages/tunnels_page.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/shell/navigation.dart';
import 'package:pb_mapper_ui/src/widgets/status.dart';

import 'fake_desktop_api.dart';
import 'fake_pb_mapper_api.dart';
import 'support.dart';

/// A host whose enable can be held in flight: the store is busy with it.
class _BusyDesktop extends FakeDesktopApi {
  Completer<DesktopHostEnable>? enabling;

  @override
  Future<DesktopHostEnable> enableHost(
    String service,
    int displayId, {
    required bool allowControl,
  }) {
    final enabling = this.enabling;
    if (enabling == null) {
      return super.enableHost(service, displayId, allowControl: allowControl);
    }
    calls.add(('host_enable', {'service': service}));
    return enabling.future;
  }
}

class _Api extends FakePbMapperApi {
  final _busy = _BusyDesktop();

  @override
  _BusyDesktop get desktop => _busy;
}

void main() {
  testWidgets(
    'while viewed, Disconnect viewer and Stop sharing are reachable in the '
    'title bar, over a dialog and without a title bar, and work while busy',
    (tester) async {
      Future<_Api> viewed({bool needsSetup = false}) async {
        final api = _Api();
        api.desktop.hostSnapshot = viewedHost(controlling: true);
        await pumpApp(
          tester,
          api,
          page: AppPage.tunnels,
          needsSetup: needsSetup,
        );
        return api;
      }

      // The title bar: Stop never waits behind a host action in flight.
      var api = await viewed();
      expect(find.byKey(const Key('sharing-indicator')), findsOneWidget);
      expect(find.byKey(const Key('sharing-fallback')), findsNothing);
      final enabling = api.desktop.enabling = Completer<DesktopHostEnable>();
      final desktop = AppScope.of(
        tester.element(find.byType(TunnelsPage)),
      ).store.desktop;
      unawaited(desktop.enableHost(displayId: 1, allowControl: true));
      await tester.pump();
      expect(desktop.hostView.value.enabling, isTrue);
      await tester.tap(find.byKey(const Key('sharing-indicator-stop')));
      await tester.pump();
      expect(api.desktop.ops, contains('host_stop'));
      enabling.complete(const DesktopHostEnable());
      await finish(tester);

      // A dialog's barrier covers the title bar: the fallback above it takes
      // the tap.
      api = await viewed();
      unawaited(
        showDialog<void>(
          context: tester.element(find.byType(TunnelsPage)),
          builder: (_) => const AlertDialog(content: Text('busy elsewhere')),
        ),
      );
      await settle(tester);
      expect(find.byKey(const Key('sharing-fallback')), findsOneWidget);
      // One set of controls: the capsule under the barrier steps aside.
      expect(
        tester
            .widget<Visibility>(
              find
                  .ancestor(
                    of: find.byKey(const Key('sharing-indicator-stop')),
                    matching: find.byType(Visibility),
                  )
                  .first,
            )
            .visible,
        isFalse,
      );
      await tester.tap(find.byKey(const Key('sharing-fallback-disconnect')));
      await tester.pump();
      expect(api.desktop.ops, contains('host_end_session'));
      await finish(tester);

      // First-run setup has no title bar at all.
      api = await viewed(needsSetup: true);
      expect(find.byKey(const Key('setup-title')), findsOneWidget);
      expect(find.byKey(const Key('sharing-fallback')), findsOneWidget);
      await tester.tap(find.byKey(const Key('sharing-fallback-stop')));
      await tester.pump();
      expect(api.desktop.ops, contains('host_stop'));
      await finish(tester);
    },
  );

  testWidgets(
    'a viewer is announced by a few breaths of its dots, then the window '
    'draws nothing for as long as the session lasts',
    (tester) async {
      final api = _Api();
      api.desktop.hostSnapshot = viewedHost();
      // With motion on, settling waits for the rail's dot and the capsule's
      // to come to rest; a dot that kept breathing would never let it.
      await pumpApp(tester, api, page: AppPage.tunnels, reducedMotion: false);
      expect(find.byKey(const Key('sharing-indicator')), findsOneWidget);
      expect(find.byType(HostSharingDot), findsOneWidget);
      expect(find.byType(PulsingDot), findsNWidgets(2));

      // A minute of the session, the host polled all along: no frame.
      final polls = api.desktop.ops.where((op) => op == 'host_status').length;
      for (var second = 0; second < 60; second++) {
        await tester.pump(const Duration(seconds: 1));
        expect(tester.binding.hasScheduledFrame, isFalse);
      }
      expect(
        api.desktop.ops.where((op) => op == 'host_status').length,
        greaterThan(polls),
      );
      expect(find.byType(PulsingDot), findsNWidgets(2));
      await finish(tester);
    },
  );
}
