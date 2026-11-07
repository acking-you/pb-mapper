import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_api.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';

import 'fake_desktop_api.dart';

void main() {
  testWidgets('an invalid invitation ends where it stopped, without retries', (
    tester,
  ) async {
    final api = FakeDesktopApi();
    final store = DesktopStore(api);
    final viewer = store.demand(DesktopDemand.viewer);

    // Rust had verified the path and was checking the identity.
    api.failNext('connect', 'INVALID_INVITATION');
    final gate = Completer<void>();
    api.connectGate = gate;
    final connecting = store.connect(
      const DesktopTarget(invitation: 'malformed', name: 'Typo'),
    );
    api.viewerStatusValue = const DesktopViewerStatus(stage: 'identity');
    await tester.pump(const Duration(milliseconds: 500));
    gate.complete();
    await connecting;
    var session = store.session.value;
    expect(session.phase, DesktopPhase.ended);
    expect(session.end, DesktopEndReason.invalidInvitation);
    expect(session.setup, DesktopSetupStep.verifying);
    expect(session.reconnectAttempt, 0);
    await tester.pump(const Duration(seconds: 20));
    expect(api.argsOf('connect'), hasLength(1));

    await store.connect(
      const DesktopTarget(invitation: 'corrected', name: 'Typo'),
    );
    api.viewerStatusValue = liveStatus();
    await tester.pump(const Duration(milliseconds: 500));
    session = store.session.value;
    expect(session.phase, DesktopPhase.live);
    expect(session.setup, isNull);
    expect(api.argsOf('connect').last['invitation'], 'corrected');

    viewer.dispose();
    store.dispose();
  });

  testWidgets(
    'a lost stream retries the same target, restores only fresh control, and disconnect cancels the retry',
    (tester) async {
      const target = DesktopTarget(
        invitation: 'private invitation',
        name: 'Office',
        relayOnly: true,
      );
      final api = FakeDesktopApi();
      final store = DesktopStore(api);
      final viewer = store.demand(DesktopDemand.viewer);
      List<Object?> controls() => [
        for (final args in api.argsOf('control')) args['enabled'],
      ];

      // Control is wanted from the start, and asked for on a fresh picture.
      await store.connect(target);
      api.viewerStatusValue = liveStatus();
      await tester.pump(const Duration(milliseconds: 500));
      expect(controls(), [true]);
      await tester.pump(const Duration(milliseconds: 500));
      expect(store.session.value.controlling, isTrue);

      api.viewerStatusValue = stoppedStatus('transport');
      await tester.pump(const Duration(milliseconds: 500));
      expect(store.session.value.controlling, isFalse);
      expect(store.session.value.phase, DesktopPhase.reconnecting);
      await tester.pump(const Duration(seconds: 1));
      expect(api.argsOf('connect'), hasLength(2));
      for (final args in api.argsOf('connect')) {
        expect(args['invitation'], target.invitation);
        expect(args['relay_only'], isTrue);
      }

      // Not until the reconnected picture is fresh, and nothing is replayed.
      api.viewerStatusValue = liveStatus(fresh: false);
      await tester.pump(const Duration(seconds: 2));
      expect(controls(), [true]);
      api.viewerStatusValue = liveStatus();
      await tester.pump(const Duration(milliseconds: 500));
      expect(controls(), [true, true]);
      expect(api.inputs, isEmpty);

      // The host's input failed: control is not won back by itself.
      api.viewerStatusValue = DesktopViewerStatus(
        state: 'streaming',
        reason: 'input_failed',
        controlAllowed: true,
        fresh: true,
        generation: 1,
        display: const DesktopDisplay(1, 'Display 1', 1920, 1080),
        presentedFrameId: 1,
        lastVideoAgeMs: 16,
      );
      await tester.pump(const Duration(seconds: 3));
      expect(controls(), [true, true]);
      expect(store.session.value.controlWanted, isFalse);

      // A disconnect during the backoff cancels the retry.
      api.viewerStatusValue = stoppedStatus('transport');
      await tester.pump(const Duration(milliseconds: 500));
      expect(store.session.value.retryAt, isNotNull);
      await store.disconnect();
      await tester.pump(const Duration(seconds: 20));
      expect(api.argsOf('connect'), hasLength(2));
      expect(store.session.value.phase, DesktopPhase.idle);

      viewer.dispose();
      store.dispose();
    },
  );

  testWidgets(
    'a cancelled connection completion cannot replace a newer session',
    (tester) async {
      final api = FakeDesktopApi();
      final store = DesktopStore(api);
      final viewer = store.demand(DesktopDemand.viewer);
      final gate = Completer<void>();
      api.connectGate = gate;
      final first = store.connect(
        const DesktopTarget(invitation: 'old target', name: 'Old'),
      );
      await tester.pump();
      await store.disconnect();
      await store.connect(
        const DesktopTarget(invitation: 'new target', name: 'New'),
      );
      api.viewerStatusValue = liveStatus();
      await tester.pump(const Duration(milliseconds: 500));
      final geometry = store.geometry.value;
      expect(store.session.value.fresh, isTrue);

      gate.complete();
      await first;
      await tester.pump(const Duration(milliseconds: 500));
      expect(store.session.value.target?.invitation, 'new target');
      expect(store.session.value.fresh, isTrue);
      expect(store.geometry.value, geometry);
      expect(
        [for (final a in api.argsOf('connect')) a['invitation']],
        ['old target', 'new target'],
      );

      viewer.dispose();
      store.dispose();
    },
  );

  testWidgets(
    'retries end after two minutes of failures, counted from the last '
    'picture and not while hidden',
    (tester) async {
      final api = FakeDesktopApi();
      final store = DesktopStore(api, now: () => tester.binding.clock.now());
      var viewer = store.demand(DesktopDemand.viewer);
      DesktopSessionView session() => store.session.value;
      await store.connect(
        const DesktopTarget(invitation: 'pbd1.office', name: 'Office'),
      );
      api.viewerStatusValue = liveStatus();
      await tester.pump(const Duration(milliseconds: 500));
      expect(session().phase, DesktopPhase.live);

      // The host hands the session over (`restarting`), slowly. Shown again
      // after five minutes hidden, the viewer tries again, and the hand-off
      // completes.
      api
        ..failConnects = 'IO'
        ..viewerStatusValue = stoppedStatus('restarting');
      await tester.pump(const Duration(seconds: 100));
      viewer.dispose();
      await tester.pump(const Duration(minutes: 5));
      viewer = store.demand(DesktopDemand.viewer);
      await tester.pump(const Duration(seconds: 1));
      expect(session().phase, DesktopPhase.reconnecting);
      api.failConnects = null;
      await tester.pump(const Duration(seconds: 15));
      api.viewerStatusValue = liveStatus();
      await tester.pump(const Duration(milliseconds: 500));
      expect(session().phase, DesktopPhase.live);

      // The next one never completes: retried for two minutes from that
      // stop, then ended, and nothing retries by itself afterwards.
      api
        ..failConnects = 'IO'
        ..viewerStatusValue = stoppedStatus('restarting');
      await tester.pump(const Duration(seconds: 110));
      expect(session().phase, DesktopPhase.reconnecting);
      await tester.pump(const Duration(seconds: 15));
      expect(session().phase, DesktopPhase.ended);
      expect(session().end, DesktopEndReason.unreachable);
      expect(session().failure?.code, 'IO');
      final tried = api.argsOf('connect').length;
      await tester.pump(const Duration(minutes: 1));
      expect(api.argsOf('connect'), hasLength(tried));

      viewer.dispose();
      store.dispose();
    },
  );
}
